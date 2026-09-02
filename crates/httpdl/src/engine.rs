//! HttpEngine（§14，impl DownloadEngine）：M4a 骨架 + M4b 多连接并行下载/镜像/换源/校验。
//! add = 探测 → 规划 → 登记 → 后台下载循环；段失败 → 镜像轮换；校验失败 → 重下 1 次 → 降级接受。

use crate::download::download_dynamic;
use crate::range::probe_range;
use crate::rate::RateLimiter;
use crate::resume;
use crate::segment_manager::DEFAULT_MIN_SPLIT;
use crate::verify::{verify_file, verify_file_md5};
use parking_lot::Mutex;
use smart_dl_core::identity::ContentIdentity;
use smart_dl_core::session::output::OutputManager;
use smart_dl_core::task::DownloadTask;
use smart_dl_core::types::{
    Capability, DownloadEngine, DownloadSource, EngineError, EngineKind, EngineState, EngineStatus,
    EngineTaskId, PeerInfo,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// 换源竞态窗口：段失败后等待 update_sources 到达（mirrors 变化则重试）。
const SOURCE_WINDOW: Duration = Duration::from_millis(500);

/// 引擎内任务。
struct HttpTask {
    headers: Vec<(String, String)>,
    /// 候选源列表（add 初始单 URL；update_sources 替换）。
    mirrors: Vec<String>,
    /// 当前源 ETag（换源对比）。
    etag: Option<String>,
    dest: PathBuf,
    /// 续传起点（0 = 全新下载；>0 = 跳过 [0, offset) 动态领取剩余段）。
    offset: u64,
    state: EngineState,
    done: u64,
    total: u64,
    error: Option<String>,
    sha256: Option<String>,
    /// 备用源内容 MD5 校验目标（切备用源后生效；主源阶段 None）。
    md5: Option<String>,
    verify_attempts: u32,
    /// 备用源 URL（主源两次校验失败后切换）。
    backup_url: Option<String>,
    /// 备用源内容 MD5（夸克 backup_md5 机制）。
    backup_md5: Option<String>,
    /// 备用源是否已使用（避免无限切换）。
    backup_used: bool,
    /// 换源代次：etag 变化 → gen+1 → 旧下载循环退出、新循环启动。
    /// .part 路径随 gen 隔离（`dest.<gen>.part`），避免新旧循环并发写同一文件。
    gen: u64,
    /// 任务级下载限速（KiB/s 配置回显；None = 走全局）。实际生效速在
    /// limiters 表的 RateLimiter 上（set_limits 运行中即时改率）。
    limit_kb_s: Option<u32>,
    /// 顺序下载（边下边播）：true = download_loop 每轮传给 download_dynamic，
    /// 在飞段窗口收紧。set_sequential 运行中改写 → 下一次重下轮拾取。
    sequential: bool,
}

struct EngineInner {
    tasks: Mutex<HashMap<EngineTaskId, HttpTask>>,
    /// Mirror 加权评分（URL → 分数）：跨任务持久，成功 +1 / 失败 -2，clamp [-4, +4]。
    mirror_scores: Arc<Mutex<HashMap<String, i64>>>,
    /// 任务级限速器（tid → limiter）；未登记的任务走引擎全局 limiter。
    /// 与全局不同：任务条目内的 RateLimiter 速率可运行中热调（set_rate_kb_s）。
    limiters: Mutex<HashMap<EngineTaskId, Arc<RateLimiter>>>,
}

/// HTTP 引擎：reqwest 传输 + 自研调度层（D29）。
#[derive(Clone)]
pub struct HttpEngine {
    client: reqwest::Client,
    /// 跨段共享限速器（0 = 不限）。
    limiter: Arc<RateLimiter>,
    inner: Arc<EngineInner>,
}

impl HttpEngine {
    /// 不限速引擎（默认路径，兼容既有测试）。
    pub fn new(client: reqwest::Client) -> Self {
        Self::new_limited(client, 0)
    }

    /// `download_kb_s` = 全局下载限速 KiB/s（0 = 不限）。
    pub fn new_limited(client: reqwest::Client, download_kb_s: u32) -> Self {
        HttpEngine {
            client,
            limiter: Arc::new(RateLimiter::new(download_kb_s)),
            inner: Arc::new(EngineInner {
                tasks: Mutex::new(HashMap::new()),
                mirror_scores: Arc::new(Mutex::new(HashMap::new())),
                limiters: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// 启动下载循环（代次 gen）。
    /// 可靠性修复（V11，报告第二轮）：不再丢弃 JoinHandle——监控任务捕获
    /// 下载循环 panic，把任务标 Error（修复前 panic 任务静默变僵尸：状态
    /// 永停 Downloading、无 Failed 事件、无收尸路径）。
    fn spawn_download(&self, tid: EngineTaskId, gen: u64) {
        let client = self.client.clone();
        // 任务级限速优先，未登记回退全局（跨段共享口径不变）。
        let limiter = self
            .inner
            .limiters
            .lock()
            .get(&tid)
            .cloned()
            .unwrap_or_else(|| self.limiter.clone());
        let inner = self.inner.clone();
        let inner_mon = self.inner.clone();
        let tid_mon = tid.clone();
        let handle = tokio::spawn(async move {
            download_loop(&client, limiter, inner, tid, gen).await;
        });
        // 收尸监控：panic → 任务标 Error（V11 锁治理后 parking_lot 无中毒，
        // 无条件锁不再有级联引爆风险，收尸保证执行；锁在子线程 unwind 时已随 RAII 释放）
        tokio::spawn(async move {
            if let Err(join_err) = handle.await {
                if join_err.is_panic() {
                    let msg = format!("下载循环 panic（V11 收尸）: {join_err}");
                    tracing::error!("[V11] tid={tid_mon}: {msg}");
                    let mut tasks = inner_mon.tasks.lock();
                    if let Some(t) = tasks.get_mut(&tid_mon) {
                        if t.gen == gen {
                            t.state = EngineState::Error;
                            t.error = Some(msg);
                        }
                    }
                }
            }
        });
    }
}

async fn download_loop(
    client: &reqwest::Client,
    limiter: Arc<RateLimiter>,
    inner: Arc<EngineInner>,
    tid: EngineTaskId,
    gen: u64,
) {
    loop {
        // 快照任务参数（不跨 await 持锁）
        let (part, offset, mirrors_raw, total, sha256, md5, sequential) = {
            let tasks = inner.tasks.lock();
            let t = match tasks.get(&tid) {
                Some(t) if t.gen == gen => t,
                _ => return, // 换源代次已推进 → 本循环作废
            };
            (
                part_path_of(&t.dest, gen),
                t.offset,
                t.mirrors.clone(),
                t.total,
                t.sha256.clone(),
                t.md5.clone(),
                t.sequential,
            )
        };

        // Mirror 加权评分：按历史分数降序稳定排序（同分保持原序），优先健康源。
        let mut mirrors = mirrors_raw.clone();
        {
            let scores = inner.mirror_scores.lock();
            mirrors.sort_by_key(|u| -scores.get(u).copied().unwrap_or(0));
        }

        match download_dynamic(
            client,
            &part,
            total,
            offset,
            DEFAULT_MIN_SPLIT,
            &mirrors,
            limiter.clone(),
            Some(inner.mirror_scores.clone()),
            sequential,
        )
        .await
        {
            Ok(()) => {
                // finalize 前检查代次：换源已发生 → 本循环结果作废（gen1 会重下）
                let still_current = inner
                    .tasks
                    .lock()
                    .get(&tid)
                    .map(|t| t.gen == gen)
                    .unwrap_or(false);
                if !still_current {
                    return;
                }
                // 段全部落位 → 校验（sha256 或备用源 md5；均未提供 → 不校验直接落位）
                let dest = dest_of(&inner, &tid);
                let verify_result = match (&sha256, &md5) {
                    (Some(expected), _) => verify_file(&part, expected),
                    (None, Some(expected)) => verify_file_md5(&part, expected),
                    (None, None) => Ok(true),
                };
                match verify_result {
                    Ok(true) => {
                        // 校验通过 → 落位
                        match finalize_part(&part, &dest, total) {
                            Ok(()) => {
                                cleanup_old_parts(&dest, gen);
                                // 落位完成 → 清理续传凭据（.part 已移动，etag 副文件删除）
                                let _ = std::fs::remove_file(resume::part_etag_path(&part));
                                finish(&inner, &tid, EngineState::Completed, None);
                            }
                            Err(e) => finish(&inner, &tid, EngineState::Error, Some(e)),
                        }
                        return;
                    }
                    Ok(false) => {
                        let attempts = {
                            let mut tasks = inner.tasks.lock();
                            let t = tasks.get_mut(&tid).unwrap();
                            t.verify_attempts += 1;
                            t.verify_attempts
                        };
                        if attempts <= 1 {
                            // 重下 1 次：作废 .part（含 etag 副文件）
                            remove_part(&part);
                            continue;
                        }
                        // 主源两次校验失败 → 切备用源（夸克 backup_url/backup_md5 机制）
                        let (backup_url, backup_md5, backup_used) = {
                            let tasks = inner.tasks.lock();
                            let t = tasks.get(&tid).unwrap();
                            (t.backup_url.clone(), t.backup_md5.clone(), t.backup_used)
                        };
                        if let (Some(bu), false) = (&backup_url, backup_used) {
                            let mut tasks = inner.tasks.lock();
                            let t = tasks.get_mut(&tid).unwrap();
                            t.backup_used = true;
                            t.mirrors = vec![bu.clone()];
                            t.offset = 0; // 备用源可能是不同文件 → 全量重下
                            t.sha256 = None;
                            t.md5 = backup_md5.clone();
                            t.verify_attempts = 0;
                            t.error = None;
                            t.state = EngineState::Downloading;
                            drop(tasks);
                            println!("[httpdl] {}: 主源校验失败，切换备用源", tid);
                            remove_part(&part);
                            continue;
                        }
                        // 备用源也失败（或未配置）→ 降级接受 + 告警（Q-B5）
                        let warn = if md5.is_some() {
                            "md5 mismatch, accepted downgrade".to_string()
                        } else {
                            "sha256 mismatch, accepted downgrade".to_string()
                        };
                        match finalize_part(&part, &dest, total) {
                            Ok(()) => {
                                cleanup_old_parts(&dest, gen);
                                // 落位完成 → 清理续传凭据（.part 已移动，etag 副文件删除）
                                let _ = std::fs::remove_file(resume::part_etag_path(&part));
                                finish(&inner, &tid, EngineState::Completed, Some(warn));
                                return;
                            }
                            Err(e) => {
                                finish(&inner, &tid, EngineState::Error, Some(e));
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        finish(
                            &inner,
                            &tid,
                            EngineState::Error,
                            Some(format!("verify io: {e}")),
                        );
                        return;
                    }
                }
            }
            Err(e) => {
                // 段全 mirror 失败：给 update_sources 一个竞态窗口
                tokio::time::sleep(SOURCE_WINDOW).await;
                let now_mirrors = {
                    let tasks = inner.tasks.lock();
                    tasks.get(&tid).map(|t| t.mirrors.clone())
                };
                if now_mirrors.as_deref() != Some(mirrors_raw.as_slice()) {
                    continue; // 换源已生效 → 用新列表重试
                }
                finish(&inner, &tid, EngineState::Error, Some(e));
                return;
            }
        }
    }
}

fn dest_of(inner: &Arc<EngineInner>, tid: &str) -> PathBuf {
    inner
        .tasks
        .lock()
        .get(tid)
        .map(|t| t.dest.clone())
        .unwrap_or_default()
}

fn finish(inner: &Arc<EngineInner>, tid: &str, state: EngineState, error: Option<String>) {
    let mut tasks = inner.tasks.lock();
    if let Some(t) = tasks.get_mut(tid) {
        t.state = state;
        t.done = t.total;
        t.error = error;
    }
}

/// .part → 目标落位。换源/重下场景 dest 可能已有旧内容且大小相同
/// （OutputManager::finalize_to 的幂等短路会直接 Ok 不覆盖）→ 先删 dest 强制落位。
fn finalize_part(part: &Path, dest: &Path, total: u64) -> Result<(), String> {
    if let Err(e) = std::fs::remove_file(dest) {
        tracing::warn!("finalize_part: 删除目标文件失败 {dest:?}: {e}");
    }
    let om = OutputManager::new(PathBuf::from("."));
    om.finalize_to(part, dest, total).map_err(|e| e.to_string())
}

/// .part 路径：gen0 → `<dest>.part`（续传语义）；gen≥1 → `<dest>.<gen>.part`
/// （换源重下与新循环隔离，避免并发写同一文件）。
fn part_path_of(dest: &Path, gen: u64) -> PathBuf {
    if gen == 0 {
        let mut s = dest.as_os_str().to_os_string();
        s.push(".part");
        PathBuf::from(s)
    } else {
        let mut s = dest.as_os_str().to_os_string();
        s.push(format!(".{gen}.part"));
        PathBuf::from(s)
    }
}

/// 删除 .part 及其 ETag 副文件（作废重下/清理共用）。
fn remove_part(part: &Path) {
    let _ = std::fs::remove_file(part);
    let _ = std::fs::remove_file(resume::part_etag_path(part));
}

/// finalize 成功后清理旧代次的 .part（gen0 的 `<dest>.part` 或上一 gen）。
fn cleanup_old_parts(dest: &Path, gen: u64) {
    if gen > 0 {
        let _ = std::fs::remove_file(part_path_of(dest, gen - 1));
    }
}

#[async_trait::async_trait]
impl DownloadEngine for HttpEngine {
    fn id(&self) -> &str {
        "http"
    }

    fn kind(&self) -> EngineKind {
        EngineKind::Http
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Http,
            Capability::Https,
            Capability::Range,
            Capability::MultiConnection,
            Capability::Mirror,
            Capability::UrlRefresh,
            Capability::Sequential,
        ]
    }

    async fn add(&self, task: &DownloadTask) -> Result<EngineTaskId, EngineError> {
        let (url, headers, backup_url) = match &task.source {
            DownloadSource::Http {
                url,
                headers,
                backup_url,
                ..
            } => (url.clone(), headers.clone(), backup_url.clone()),
            _ => return Err(EngineError::Other("source is not http".to_string())),
        };
        let probe = probe_range(&self.client, &url, &headers).await?;
        let total = probe.total.unwrap_or(0);

        let rel = task
            .metadata
            .name
            .clone()
            .unwrap_or_else(|| "download.bin".to_string());
        // 安全修复（V3）：任务名可能来自恶意 torrent/远端，join 前必须净化
        // （拒 `..` / 绝对路径 / 盘符前缀），非法即拒任务。
        let rel_pb = smart_dl_core::session::output::sanitize_rel(&rel)
            .map_err(|e| EngineError::Other(e.to_string()))?;
        let dest = task.dest_root.join(&rel_pb);
        // 断点续传（#4）：.part 存在 → 探测+决策 → 从偏移续传或作废重下；
        // 探测到的 ETag 持久化到 `<part>.etag` 供下次决策。
        // P0 动态分段：只记录续传起点 offset，段由 SegmentManager 动态领取。
        let part0 = part_path_of(&dest, 0);
        let offset = if part0.exists() {
            let part_len = std::fs::metadata(&part0).map(|m| m.len()).unwrap_or(0);
            let part_etag = resume::read_part_etag(&part0);
            match resume::decide_resume(part_len, part_etag.as_deref(), &probe) {
                resume::ResumeDecision::ContinueFrom(off) => {
                    println!(
                        "[httpdl] 断点续传 {}: 从偏移 {off} 继续 (part_len={part_len})",
                        task.id
                    );
                    off
                }
                resume::ResumeDecision::Restart => {
                    println!("[httpdl] 断点续传 {}: .part 不可信，作废重下", task.id);
                    remove_part(&part0);
                    0
                }
            }
        } else {
            0
        };
        resume::write_part_etag(&part0, probe.etag.as_deref());
        let (sha256, backup_md5) = match &task.identity {
            ContentIdentity::SingleFile {
                sha256, backup_md5, ..
            } => (sha256.clone(), backup_md5.clone()),
            _ => (None, None),
        };

        let tid = task.id.clone();
        {
            let mut tasks = self.inner.tasks.lock();
            tasks.insert(
                tid.clone(),
                HttpTask {
                    headers,
                    mirrors: vec![url.clone()],
                    etag: probe.etag.clone(),
                    dest,
                    offset,
                    state: EngineState::Downloading,
                    done: 0,
                    total,
                    error: None,
                    sha256,
                    md5: None,
                    verify_attempts: 0,
                    backup_url,
                    backup_md5,
                    backup_used: false,
                    gen: 0,
                    limit_kb_s: None,
                    sequential: task.sequential,
                },
            );
        }
        self.spawn_download(tid.clone(), 0);
        Ok(tid)
    }

    async fn pause(&self, id: &EngineTaskId) -> Result<(), EngineError> {
        let mut tasks = self.inner.tasks.lock();
        let t = tasks.get_mut(id).ok_or(EngineError::NotFound)?;
        t.state = EngineState::Paused;
        Ok(())
    }

    async fn resume(&self, id: &EngineTaskId) -> Result<(), EngineError> {
        let mut tasks = self.inner.tasks.lock();
        let t = tasks.get_mut(id).ok_or(EngineError::NotFound)?;
        t.state = EngineState::Downloading;
        Ok(())
    }

    async fn status(&self, id: &EngineTaskId) -> Result<EngineStatus, EngineError> {
        let tasks = self.inner.tasks.lock();
        let t = tasks.get(id).ok_or(EngineError::NotFound)?;
        Ok(EngineStatus {
            state: t.state,
            metadata_received: true,
            files: vec![],
            total_done: t.done,
            total: t.total,
            down_rate: 0,
            up_rate: 0,
            num_peers: 0,
            num_seeds: 0,
            error: t.error.clone(),
        })
    }

    async fn remove(&self, id: &EngineTaskId, _delete_data: bool) -> Result<(), EngineError> {
        let mut tasks = self.inner.tasks.lock();
        tasks.remove(id).ok_or(EngineError::NotFound)?;
        drop(tasks);
        // 限速器条目随任务清理（防泄漏）。
        self.inner.limiters.lock().remove(id);
        Ok(())
    }

    /// 任务级下载限速（trait 扩展）。任务专属 limiter 登记进 limiters 表，
    /// 已持有的 Arc 热调速率 → 运行中的 chunk 立即按新速率节流，无需重启循环。
    /// 仅 down 方向有意义：up 请求被显式拒绝（HTTP/FTP 无上传概念）。
    async fn set_limits(
        &self,
        id: &EngineTaskId,
        down_kb_s: Option<u32>,
        up_kb_s: Option<u32>,
    ) -> Result<(), EngineError> {
        if up_kb_s.is_some() {
            return Err(EngineError::Other(
                "HTTP/FTP 引擎无上传方向，up_kb_s 不适用".to_string(),
            ));
        }
        let Some(kb) = down_kb_s else { return Ok(()) }; // 双 None = no-op
                                                         // 任务必须存在（不存在 → NotFound；remove 后迟到请求同理）
        {
            let mut tasks = self.inner.tasks.lock();
            if !tasks.contains_key(id) {
                return Err(EngineError::NotFound);
            }
            // 配置回显记到任务快照上（审计/透出口径）
            if let Some(t) = tasks.get_mut(id) {
                t.limit_kb_s = Some(kb);
            }
        }
        let mut limiters = self.inner.limiters.lock();
        match limiters.get(id) {
            Some(lim) => lim.set_rate_kb_s(kb), // 已有限速器 → 原地热调
            None => {
                limiters.insert(id.clone(), Arc::new(RateLimiter::new(kb)));
            }
        }
        Ok(())
    }

    /// 任务级顺序下载开关（trait 扩展）。语义：字段改写，下一次重下轮
    /// （换源 / 校验失败 / 续传轮）拾取；运行中的当前轮不变（收尾在飞段）。
    /// 新建任务在 add() 直接读 task.sequential → 立即生效。
    async fn set_sequential(&self, id: &EngineTaskId, on: bool) -> Result<(), EngineError> {
        let mut tasks = self.inner.tasks.lock();
        match tasks.get_mut(id) {
            Some(t) => {
                t.sequential = on;
                Ok(())
            }
            None => Err(EngineError::NotFound),
        }
    }

    async fn peers(&self, _id: &EngineTaskId) -> Result<Vec<PeerInfo>, EngineError> {
        Ok(vec![])
    }

    async fn update_sources(
        &self,
        id: &EngineTaskId,
        urls: Vec<String>,
    ) -> Result<(), EngineError> {
        if urls.is_empty() {
            return Err(EngineError::Other("empty source list".to_string()));
        }
        // 锁外探测（锁不跨 await：parking_lot guard 跨 await 会阻塞执行器）
        let headers = {
            let tasks = self.inner.tasks.lock();
            tasks.get(id).ok_or(EngineError::NotFound)?.headers.clone()
        };
        let probe = probe_range(&self.client, &urls[0], &headers).await?;

        let mut tasks = self.inner.tasks.lock();
        let t = tasks.get_mut(id).ok_or(EngineError::NotFound)?;
        let etag_changed = probe.etag.is_some() && t.etag.is_some() && probe.etag != t.etag;
        if etag_changed {
            // 新源内容变了 → 旧代次 .part 作废，重下（Q-B5：ETag 为准）。
            // gen+1 → 新循环用 `<dest>.<gen>.part`，与旧循环写隔离。
            let _ = std::fs::remove_file(part_path_of(&t.dest, t.gen));
            t.done = 0;
            t.verify_attempts = 0;
            t.state = EngineState::Downloading;
            t.error = None;
            t.gen += 1; // 废弃旧下载循环
        }
        t.mirrors = urls.clone();
        if let Some(e) = &probe.etag {
            t.etag = Some(e.clone());
        }
        let (spawn, new_gen) = if etag_changed {
            (true, t.gen)
        } else {
            (false, 0)
        };
        drop(tasks);
        if spawn {
            self.spawn_download(id.clone(), new_gen);
        }
        Ok(())
    }

    async fn add_url_seed(&self, _id: &EngineTaskId, _url: &str) -> Result<(), EngineError> {
        Ok(())
    }

    async fn ban_peer(&self, _id: &EngineTaskId, _peer: SocketAddr) -> Result<(), EngineError> {
        Ok(())
    }

    async fn read_piece(&self, _id: &EngineTaskId, _idx: u32) -> Result<Vec<u8>, EngineError> {
        Err(EngineError::Unsupported)
    }
}
