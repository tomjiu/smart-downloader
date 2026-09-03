//! HttpEngine（§14，impl DownloadEngine）：M4a 骨架 + M4b 多连接并行下载/镜像/换源/校验。
//! add = 探测 → 规划 → 登记 → 后台下载循环（主源探测失败自动落备用源）；段失败 → 镜像轮换；
//! 校验失败 → 重下 1 次 → 隔离试错轮换（备用源优先，多候选逐一隔离）→ 降级接受。update_sources 并发探测全部候选源 + 评分播种，
//! 任一存活即换源成功（不再首源死即拒）。P4 续传：段账本（`<part>.progress`）为唯一进度真源；
//! pause 真停（段边界退出）；epoch 单写者模型（resume 无条件新 epoch 循环，旧循环在检查点自杀且永不 finalize）。

use crate::download::{download_dynamic, update_score, DynamicLedger, DynamicOutcome, SCORE_MIN};
use crate::ledger;
use crate::range::{probe_range, Probe};
use crate::rate::{RateLimiter, RateSample};
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 换源竞态窗口：段失败后等待 update_sources 到达（mirrors 变化则重试）。
const SOURCE_WINDOW: Duration = Duration::from_millis(500);

/// 引擎内任务。
struct HttpTask {
    headers: Vec<(String, String)>,
    /// 候选源列表（add 初始单 URL；update_sources 替换）。
    mirrors: Vec<String>,
    /// 当前源 ETag（换源对比 + 账本写入）。
    etag: Option<String>,
    dest: PathBuf,
    state: EngineState,
    done: u64,
    total: u64,
    /// 速率采样器（E11）：status() 读取时增量采样（B/s），daemon /stats 聚合消费。
    rate: RateSample,
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
    /// 校验失败轮换池（E3 隔离试错）：多候选表两次校验失败后，逐一以唯一源
    /// 身份重下重校验的候选队列（评分降序；backup_url 除外——它走专属优先
    /// 分支）。空池且当前表单源 = 无隔离试错价值 → 走降级接受（Q-B5 保留）。
    rotate_pool: Vec<String>,
    /// 换源代次：etag 变化 → gen+1 → 旧下载循环退出、新循环启动。
    /// .part 路径随 gen 隔离（`dest.<gen>.part`），避免新旧循环并发写同一文件。
    gen: u64,
    /// 循环代次（P4）：resume/重试重入 → epoch+1 并无条件 spawn 新循环；
    /// 旧循环在 gen/epoch 检查点自杀，且永不 finalize（单写者收敛）。
    epoch: u64,
    /// 真暂停标志：置位后 worker 在段边界退出（在飞段收尾即止，不再领新段）。
    pause: Arc<AtomicBool>,
    /// 任务级下载限速（KiB/s 配置回显；None = 走全局）。实际生效速在
    /// limiters 表的 RateLimiter 上（set_limits 运行中即时改率）。
    limit_kb_s: Option<u32>,
    /// 顺序下载（边下边播）：true = download_loop 每轮传给 download_dynamic，
    /// 在飞段窗口收紧。set_sequential 运行中改写 → 下一次重下轮拾取。
    sequential: bool,
    /// 任务级代理 URL（E5 配置回显）：Some = 该任务专用 client 仅装此代理
    /// （覆盖全局）；None = 引擎共享 client。实际生效 client 在 spawn/
    /// update_sources/add 探测时按此字段构建。
    proxy: Option<String>,
    /// 最终落盘名（E9 配置回显）：add 落盘名决策结果（显式名回显同值；派生
    /// 名 = CD → URL 末段 → 兑底链，已 sanitize_rel 终审）。status() 透出供
    /// daemon 回填 metadata.name——引擎内部派生的名字不再对 daemon 隐身。
    resolved_name: Option<String>,
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

/// 从代理 URL 提取 `user:pass@`（E5：任务级代理 reqwest basic_auth 用；
/// 与 daemon serve.rs 全局代理同一格式约定，BT 引擎由 btcore::parse_proxy
/// 解析同一格式）。无凭据 → None。
pub fn proxy_auth_of(url: &str) -> Option<(String, String)> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let (auth, _) = rest.rsplit_once('@')?;
    let (u, p) = auth.split_once(':').unwrap_or((auth, ""));
    Some((u.to_string(), p.to_string()))
}

/// 任务级代理 client 构建（E5）：配置与 serve.rs 全局 client 同口径——
/// connect 10s + read 30s（H-9：不设总超时护长下载）；proxy URL 非法返回 Err
/// （daemon add 时调用本函数校验，spawn 时重建失败则任务标 Error）。
pub fn build_proxied_client(proxy_url: &str) -> Result<reqwest::Client, String> {
    let proxy = reqwest::Proxy::all(proxy_url).map_err(|e| e.to_string())?;
    // 凭据与代理体合成同一个 Proxy（ClientBuilder.proxy() 为追加语义：
    // 先 push 无凭据再 push 带凭据会由前者先命中，凭据丢失 → 条件单次）
    let proxy = match proxy_auth_of(proxy_url) {
        Some((u, p)) => proxy.basic_auth(&u, &p),
        None => proxy,
    };
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .read_timeout(std::time::Duration::from_secs(30))
        .proxy(proxy)
        .build()
        .map_err(|e| e.to_string())
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

    /// 启动下载循环（代次 gen + 循环代次 epoch）。
    /// 可靠性修复（V11，报告第二轮）：不再丢弃 JoinHandle——监控任务捕获
    /// 下载循环 panic，把任务标 Error（修复前 panic 任务静默变僵尸：状态
    /// 永停 Downloading、无 Failed 事件、无收尸路径）。
    fn spawn_download(&self, tid: EngineTaskId, gen: u64, epoch: u64) {
        // E5 任务级代理：Some(proxy) → 构建任务专用 client（仅装该代理，覆盖
        // 全局）；None → 引擎共享 client（可能含全局 [download] proxy）。
        // 构建失败（add 时已校验过，此处兜底）→ 任务标 Error 不 spawn。
        let (client, spawn_err) = {
            let tasks = self.inner.tasks.lock();
            match tasks.get(&tid).and_then(|t| t.proxy.as_deref()) {
                Some(p) => match build_proxied_client(p) {
                    Ok(c) => (c, None),
                    Err(e) => (
                        self.client.clone(),
                        Some(format!("任务级 proxy client 构建失败: {e}")),
                    ),
                },
                None => (self.client.clone(), None),
            }
        };
        if let Some(msg) = spawn_err {
            let mut tasks = self.inner.tasks.lock();
            if let Some(t) = tasks.get_mut(&tid) {
                t.state = EngineState::Error;
                t.error = Some(msg);
            }
            return;
        }
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
            download_loop(&client, limiter, inner, tid, gen, epoch).await;
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
                        // 仅当前 gen/epoch 的循环 panic 才标记任务
                        //（过期循环的死与任务状态无关）
                        if t.gen == gen && t.epoch == epoch {
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
    epoch: u64,
) {
    loop {
        // 快照任务参数（不跨 await 持锁）；gen/epoch 失配 → 本循环作废
        let (part, mirrors_raw, total, sha256, md5, sequential, etag, pause_flag, headers) = {
            let tasks = inner.tasks.lock();
            let t = match tasks.get(&tid) {
                Some(t) if t.gen == gen && t.epoch == epoch => t,
                _ => return, // 换源代次/续传 epoch 已推进 → 本循环作废
            };
            (
                part_path_of(&t.dest, gen),
                t.mirrors.clone(),
                t.total,
                t.sha256.clone(),
                t.md5.clone(),
                t.sequential,
                t.etag.clone(),
                t.pause.clone(),
                t.headers.clone(),
            )
        };

        // Mirror 加权评分：按历史分数降序稳定排序（同分保持原序），优先健康源。
        let mut mirrors = mirrors_raw.clone();
        {
            let scores = inner.mirror_scores.lock();
            mirrors.sort_by_key(|u| -scores.get(u).copied().unwrap_or(0));
        }

        // 段账本加载（P4 唯一进度真源）：合法账本 → 恢复已完成段 + 沿用其粒度；
        // 缺失/损坏/total 失配 → 全新计划（add 决策已保证此处账本可信，
        // 二次校验只防御运行中文件被外部改动）。
        let ledger_path = ledger::ledger_path(&part);
        let loaded =
            ledger::load(&ledger_path).filter(|l| l.total == total && l.validate_segments());
        let ledger_handle = DynamicLedger {
            path: ledger_path,
            etag: etag.clone(),
            min_split: loaded
                .as_ref()
                .map(|l| l.min_split)
                .unwrap_or(DEFAULT_MIN_SPLIT),
            done: loaded.map(|l| l.done).unwrap_or_default(),
        };
        let min_split = ledger_handle.min_split;

        // 进度回调：单调更新 t.done（gen/epoch 门控：过期循环不污染进度）
        let progress: Arc<dyn Fn(u64) + Send + Sync> = {
            let inner = inner.clone();
            let tid = tid.clone();
            Arc::new(move |done| {
                let mut tasks = inner.tasks.lock();
                if let Some(t) = tasks.get_mut(&tid) {
                    if t.gen == gen && t.epoch == epoch {
                        t.done = t.done.max(done.min(t.total));
                    }
                }
            })
        };

        match download_dynamic(
            client,
            &part,
            total,
            min_split,
            &mirrors,
            &headers,
            limiter.clone(),
            Some(inner.mirror_scores.clone()),
            sequential,
            Some(ledger_handle),
            Some(pause_flag),
            Some(progress),
        )
        .await
        {
            Ok(DynamicOutcome::Paused) => {
                // 真暂停退出：pause() 已置状态，在飞段已收尾并记账；
                // 不得对未完整文件做校验/落位。
                return;
            }
            Ok(DynamicOutcome::Completed) => {
                // finalize 前检查 gen/epoch：换源/续传重入已发生 → 本循环结果作废
                let still_current = inner
                    .tasks
                    .lock()
                    .get(&tid)
                    .map(|t| t.gen == gen && t.epoch == epoch)
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
                                // 落位完成 → 清理续传凭据（.part 已改名，etag 副文件 + 段账本删除）
                                remove_credentials(&part);
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
                            // 重下 1 次：作废 .part（含 etag 副文件 + 段账本）
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
                            // 归因评分（E3）：唯一源两次校验失败 → 中毒直达下限（比段
                            // 传输失败更强的证据，跨任务避雷）；多候选集体失败无法
                            // 归因单一源（段可能混流）→ 不评分。
                            if t.mirrors.len() == 1 {
                                let bad = t.mirrors[0].clone();
                                update_score(&inner.mirror_scores, &bad, SCORE_MIN);
                            }
                            // 播种轮换池（E3）：旧表多候选 → 备用源也失败后仍可逐一
                            // 隔离试错（评分降序，排除备用源自身）；单候选表无隔离
                            // 价值——它刚以唯一源身份整体失败两次。
                            if t.rotate_pool.is_empty() && t.mirrors.len() > 1 {
                                let mut pool: Vec<String> =
                                    t.mirrors.iter().filter(|u| *u != bu).cloned().collect();
                                {
                                    let scores = inner.mirror_scores.lock();
                                    pool.sort_by_key(|u| -scores.get(u).copied().unwrap_or(0));
                                }
                                t.rotate_pool = pool;
                            }
                            t.backup_used = true;
                            t.mirrors = vec![bu.clone()];
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
                        // 隔离试错轮换（E3）：池空且当前表多候选 → 播种（评分降序）；
                        // 池非空 → 弹出下一候选，以唯一源身份完整重下重校验。
                        // 归因评分同上：唯一源两次校验失败 → 中毒直达下限。
                        let rotated = {
                            let mut tasks = inner.tasks.lock();
                            let t = tasks.get_mut(&tid).unwrap();
                            if t.mirrors.len() == 1 {
                                let bad = t.mirrors[0].clone();
                                update_score(&inner.mirror_scores, &bad, SCORE_MIN);
                            }
                            if t.rotate_pool.is_empty() && t.mirrors.len() > 1 {
                                let mut pool = t.mirrors.clone();
                                {
                                    let scores = inner.mirror_scores.lock();
                                    pool.sort_by_key(|u| -scores.get(u).copied().unwrap_or(0));
                                }
                                t.rotate_pool = pool;
                            }
                            if t.rotate_pool.is_empty() {
                                None
                            } else {
                                Some(t.rotate_pool.remove(0))
                            }
                        };
                        if let Some(cand) = rotated {
                            let mut tasks = inner.tasks.lock();
                            let t = tasks.get_mut(&tid).unwrap();
                            t.mirrors = vec![cand.clone()];
                            t.verify_attempts = 0;
                            t.error = None;
                            t.state = EngineState::Downloading;
                            drop(tasks);
                            println!("ttpdl] {}: 校验失败，隔离试错轮换 → {cand}", tid);
                            remove_part(&part);
                            continue;
                        }
                        // 备用源与轮换池均耗尽（或未配置）→ 降级接受 + 告警（Q-B5）
                        let warn = if md5.is_some() {
                            "md5 mismatch, accepted downgrade".to_string()
                        } else {
                            "sha256 mismatch, accepted downgrade".to_string()
                        };
                        match finalize_part(&part, &dest, total) {
                            Ok(()) => {
                                cleanup_old_parts(&dest, gen);
                                // 落位完成 → 清理续传凭据（.part 已改名，etag 副文件 + 段账本删除）
                                remove_credentials(&part);
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
        t.error = error;
        // done 不再强设 total（P4）：进度由账本回调单调维护，
        // Error 退出时保留真实进度供 status 透出。
    }
}

/// URL 路径末段（E4 弱信号文件名候选）：剥 query/hash，空段（目录型 URL）
/// → None。不做 percent-decode（避免双重解码；服务端真名由 CD 路径负责）。
fn url_basename(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let base = path.trim_end_matches('/').rsplit('/').next()?;
    let base = base.trim();
    if base.is_empty() {
        None
    } else {
        Some(base.to_string())
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

/// 删除续传凭据（etag 副文件 + 段账本；.part 本体不动——finalize 改名后仅凭据残留）。
fn remove_credentials(part: &Path) {
    let _ = std::fs::remove_file(ledger::etag_sidecar_path(part));
    ledger::remove(&ledger::ledger_path(part));
}

/// 删除 .part 及其全部续传凭据（作废重下/清理共用）。
fn remove_part(part: &Path) {
    let _ = std::fs::remove_file(part);
    remove_credentials(part);
}

/// finalize 成功后清理旧代次的 .part 及其凭据（gen0 的 `<dest>.part` 或上一 gen）。
fn cleanup_old_parts(dest: &Path, gen: u64) {
    if gen > 0 {
        remove_part(&part_path_of(dest, gen - 1));
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
        let (url, headers, backup_url, proxy) = match &task.source {
            DownloadSource::Http {
                url,
                headers,
                backup_url,
                proxy,
                ..
            } => (
                url.clone(),
                headers.clone(),
                backup_url.clone(),
                proxy.clone(),
            ),
            _ => return Err(EngineError::Other("source is not http".to_string())),
        };
        // E5 任务级代理：探测/下载全链均走任务专用 client（Some(proxy) 时）。
        // 任务级代理的意义正是「只有代理可达源」——探测若用共享 client 直连，
        // 代理专属源会在 add 时误判死亡。client 构建失败 → 任务拒绝（daemon
        // 层已校验，此处同一函数再拦，双保险）。
        let client = match &proxy {
            Some(p) => build_proxied_client(p)
                .map_err(|e| EngineError::Other(format!("任务级 proxy client 构建失败: {e}")))?,
            None => self.client.clone(),
        };
        // 探测韧性（与 update_sources 并发探测同批）：主源探测失败且配置了备用源
        // → 改用备用源建任务（身份切换与运行时“主源校验失败切备用源”同语义：
        // sha256 → None、md5 ← backup_md5、backup_used = true 防运行时重复切换）。
        // 双源均失败 → 返回双错（任务拒绝，同原语义）；无备用源 → 原样返回首错。
        let mut fell_back_to: Option<String> = None;
        let probe = match probe_range(&client, &url, &headers).await {
            Ok(p) => p,
            Err(primary_err) => match &backup_url {
                Some(bu) => {
                    println!(
                        "[httpdl] {}: 主源探测失败（{primary_err}），尝试备用源",
                        task.id
                    );
                    match probe_range(&client, bu, &headers).await {
                        Ok(p) => {
                            fell_back_to = Some(bu.clone());
                            p
                        }
                        Err(backup_err) => {
                            return Err(EngineError::Other(format!(
                                "主源与备用源探测均失败: {primary_err} / {backup_err}"
                            )));
                        }
                    }
                }
                None => return Err(primary_err),
            },
        };
        let total = probe.total.unwrap_or(0);

        // 落盘名决策（E4）：
        // - 用户显式名（metadata.name）→ 权威，非法即拒任务（V3 语义不变）；
        // - 未提供 → 自动派生链：探测响应 Content-Disposition 文件名（服务端
        //   声明最权威，含 RFC 5987 解码与目录成分剥离）→ URL 末段（弱信号，
        //   与 FTP 引擎同语义）→ "download.bin" 兜底。派生候选经 sanitize_rel
        //   终审，失败则逐级跳过（远端可控字段不得拒杀任务/制造穿越）。
        let rel_pb = match &task.metadata.name {
            Some(rel) => smart_dl_core::session::output::sanitize_rel(rel)
                .map_err(|e| EngineError::Other(e.to_string()))?,
            None => {
                let mut chosen = std::path::PathBuf::from("download.bin");
                let url_cand = url_basename(&url);
                let candidates = probe.filename.iter().chain(url_cand.iter());
                for cand in candidates {
                    if let Ok(pb) = smart_dl_core::session::output::sanitize_rel(cand) {
                        chosen = pb;
                        break;
                    }
                }
                chosen
            }
        };
        let dest = task.dest_root.join(&rel_pb);
        // E9：决策结果回显（显式名/派生名同口径）——daemon 轮询回填 metadata.name
        let resolved_name = rel_pb.to_string_lossy().into_owned();
        // 断点续传（P4 段账本版）：`<dest>.part` + `<dest>.part.progress` 账本为
        // 唯一可信进度凭据。预分配 .part 的文件长度恒等于 total，不可作为进度
        // 证据（G1：空洞文件假完成）；ETag 失配即作废（G2：混合内容文件）。
        let part0 = part_path_of(&dest, 0);
        let mut resume_done: Vec<(u64, u64)> = Vec::new();
        if total == 0 {
            // 未知长度（探测无 Content-Length/Content-Range）：无法分段，
            // 旧凭据不可用（同 legacy 语义：无 total 不续传）
            remove_part(&part0);
        } else if part0.exists() {
            let part_len = std::fs::metadata(&part0).map(|m| m.len()).unwrap_or(0);
            let ld = ledger::load(&ledger::ledger_path(&part0));
            match ledger::decide(part_len, ld.as_ref(), &probe) {
                ledger::ResumeDecision::Resume { done, .. } => {
                    println!(
                        "[httpdl] 断点续传 {}: 恢复 {} 个已完成段（{}/{} 字节）",
                        task.id,
                        done.len(),
                        done.iter().map(|(s, e)| e - s + 1).sum::<u64>(),
                        total
                    );
                    resume_done = done;
                }
                ledger::ResumeDecision::Restart => {
                    println!("[httpdl] 断点续传 {}: 凭据不可信，作废重下", task.id);
                    remove_part(&part0);
                }
            }
        } else {
            // 无 .part：清理孤儿凭据（上次 finalize 中断残留）
            remove_credentials(&part0);
        }
        let done0: u64 = resume_done.iter().map(|(s, e)| e - s + 1).sum();
        let (sha256, backup_md5) = match &task.identity {
            ContentIdentity::SingleFile {
                sha256, backup_md5, ..
            } => (sha256.clone(), backup_md5.clone()),
            _ => (None, None),
        };
        // 探测韧性初始化：主源失败已落备用源 → mirrors 以备用源起步，身份切换
        // （sha256 → None / md5 ← backup_md5）与运行时切备用源完全同语义；
        // backup_used = true —— 运行时再遇校验失败不再重复切向同一备用源。
        let (init_mirrors, init_sha256, init_md5, init_backup_used) = match &fell_back_to {
            Some(bu) => (vec![bu.clone()], None, backup_md5.clone(), true),
            None => (vec![url.clone()], sha256, None, false),
        };

        let tid = task.id.clone();
        {
            let mut tasks = self.inner.tasks.lock();
            tasks.insert(
                tid.clone(),
                HttpTask {
                    headers,
                    mirrors: init_mirrors,
                    etag: probe.etag.clone(),
                    dest,
                    state: EngineState::Downloading,
                    done: done0,
                    total,
                    rate: RateSample::default(),
                    error: None,
                    sha256: init_sha256,
                    md5: init_md5,
                    verify_attempts: 0,
                    backup_url,
                    backup_md5,
                    backup_used: init_backup_used,
                    rotate_pool: Vec::new(),
                    gen: 0,
                    epoch: 1,
                    pause: Arc::new(AtomicBool::new(false)),
                    limit_kb_s: None,
                    sequential: task.sequential,
                    proxy,
                    resolved_name: Some(resolved_name),
                },
            );
        }
        self.spawn_download(tid.clone(), 0, 1);
        Ok(tid)
    }

    /// 真暂停（P4）：置位暂停标志 → worker 在段边界退出（在飞段收尾即止，
    /// 最多延迟一个段长）；下载循环见 Paused 结局后直接返回，不对未完整
    /// 文件做校验/落位。进度凭据（段账本）已随段完成逐段落盘。
    async fn pause(&self, id: &EngineTaskId) -> Result<(), EngineError> {
        let mut tasks = self.inner.tasks.lock();
        let t = tasks.get_mut(id).ok_or(EngineError::NotFound)?;
        t.pause.store(true, Ordering::SeqCst);
        t.state = EngineState::Paused;
        Ok(())
    }

    /// 真恢复（P4）：清暂停标志 + epoch+1 无条件 spawn 新循环（从段账本
    /// 恢复已完成段）。旧循环（若仍在收尾）在下一 gen/epoch 检查点自杀，
    /// 且永不 finalize——并发下载仅重复写同内容字节，幂等无害。
    async fn resume(&self, id: &EngineTaskId) -> Result<(), EngineError> {
        let (gen, epoch) = {
            let mut tasks = self.inner.tasks.lock();
            let t = tasks.get_mut(id).ok_or(EngineError::NotFound)?;
            t.pause.store(false, Ordering::SeqCst);
            t.state = EngineState::Downloading;
            t.epoch += 1;
            (t.gen, t.epoch)
        };
        self.spawn_download(id.clone(), gen, epoch);
        Ok(())
    }

    async fn status(&self, id: &EngineTaskId) -> Result<EngineStatus, EngineError> {
        let mut tasks = self.inner.tasks.lock();
        let t = tasks.get_mut(id).ok_or(EngineError::NotFound)?;
        let down_rate = t.rate.sample(t.done);
        Ok(EngineStatus {
            state: t.state,
            metadata_received: true,
            files: vec![],
            total_done: t.done,
            total: t.total,
            down_rate,
            up_rate: 0,
            num_peers: 0,
            num_seeds: 0,
            error: t.error.clone(),
            name: t.resolved_name.clone(),
        })
    }

    async fn remove(&self, id: &EngineTaskId, _delete_data: bool) -> Result<(), EngineError> {
        // 先取暂停标志快照并置位：在飞 worker 在段边界尽快退出，
        // 不再向已移除任务的 .part 继续写入。
        let pause_flag = self.inner.tasks.lock().get(id).map(|t| t.pause.clone());
        let mut tasks = self.inner.tasks.lock();
        tasks.remove(id).ok_or(EngineError::NotFound)?;
        drop(tasks);
        if let Some(f) = pause_flag {
            f.store(true, Ordering::SeqCst);
        }
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

    /// 任务级代理热改（E8 trait 扩展，仅 HTTP 引擎支持）。
    ///
    /// 语义分档：
    /// - `Some(url)` 非法（reqwest 解析失败）→ `Other`，**不动现任务**（配置
    ///   错误在调用方定性为入参 400，任务保持原状继续原 client 传输）。
    /// - 下载中（`Downloading`）任务：锁内换 `proxy` 字段 + epoch+1，锁外
    ///   `spawn_download` 重入（resume 同款路径）——新循环捕获新 client（从
    ///   段账本恢复已完成段），旧循环在下一 gen/epoch 检查点自杀且永不
    ///   finalize（并发收尾幂等，P4 既有收敛语义）。
    /// - 暂停 / 终态（Completed/Error）任务：只改配置字段——下次 resume/
    ///   spawn 自然用新 client；对已完成任务改 proxy 无传输意义但保持配置
    ///   回显一致（审计口径：API 写了什么快照就是什么）。
    async fn set_task_proxy(
        &self,
        id: &EngineTaskId,
        proxy: Option<String>,
    ) -> Result<(), EngineError> {
        // 先试水新 client（非法 → 拒绝且零副作用；合法 → 构建产物即弃，
        // spawn_download 会按任务字段重建——构建成本 ~µs 级，不值得跨锁传）
        if let Some(p) = &proxy {
            let _ = build_proxied_client(p).map_err(EngineError::Other)?;
        }
        let restart = {
            let mut tasks = self.inner.tasks.lock();
            let t = tasks.get_mut(id).ok_or(EngineError::NotFound)?;
            t.proxy = proxy;
            let running = t.state == EngineState::Downloading;
            if running {
                t.epoch += 1;
            }
            running
        };
        if restart {
            // 锁外取当前 gen/epoch（set 与 spawn 间无 await 点，无竞态窗口）
            let (gen, epoch) = {
                let tasks = self.inner.tasks.lock();
                let t = tasks.get(id).ok_or(EngineError::NotFound)?;
                (t.gen, t.epoch)
            };
            self.spawn_download(id.clone(), gen, epoch);
        }
        Ok(())
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
        // E5：探测 client 按任务级代理构建（与 add/下载循环同口径——代理专属
        // 候选源只有经代理才可达，用共享 client 探测会误判死亡）。
        let (headers, task_client) = {
            let tasks = self.inner.tasks.lock();
            let t = tasks.get(id).ok_or(EngineError::NotFound)?;
            let client = match &t.proxy {
                Some(p) => build_proxied_client(p).map_err(EngineError::Other)?,
                None => self.client.clone(),
            };
            (t.headers.clone(), client)
        };
        // 并发探测全部候选源（源探测韧性）：原实现只探 urls[0]，首个候选死即
        // 整表拒绝——尽管下载循环本可逐段轮换到其余存活源。现改为：任一候选
        // 探测成功 → 安装全表（etag 决策取输入序首个成功，确定性与单探一致）；
        // 全失败 → 返回首错（拒绝语义保留）。探测结果播种 mirror 评分
        // （成功 +1 / 失败 -1，与段评分同 clamp 口径）→ 后续段调度
        // （分数降序稳定排序）自动优先存活/健康源，死源沉底。
        let mut probes = tokio::task::JoinSet::new();
        for u in &urls {
            let client = task_client.clone();
            let u = u.clone();
            let headers = headers.clone();
            probes.spawn(async move {
                let r = probe_range(&client, &u, &headers).await;
                (u, r)
            });
        }
        let mut by_url: HashMap<String, Result<Probe, EngineError>> = HashMap::new();
        let mut join_errs: Vec<String> = Vec::new();
        while let Some(joined) = probes.join_next().await {
            match joined {
                Ok((u, r)) => {
                    by_url.insert(u, r);
                }
                Err(je) => join_errs.push(je.to_string()), // 探测任务 panic（不预期）；无 url 可归因
            }
        }
        let probe = urls
            .iter()
            .find_map(|u| by_url.get(u).and_then(|r| r.as_ref().ok()).cloned())
            .ok_or_else(|| {
                urls.iter()
                    .find_map(|u| by_url.get(u).and_then(|r| r.as_ref().err()).cloned())
                    .unwrap_or_else(|| {
                        EngineError::Other(format!("all source probes failed: {join_errs:?}"))
                    })
            })?;
        // 评分播种：按输入序去重（重复 url 只计一次，避免重复加/扣分）
        {
            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for u in &urls {
                if !seen.insert(u.as_str()) {
                    continue;
                }
                let ok = by_url.get(u).is_some_and(|r| r.is_ok());
                update_score(&self.inner.mirror_scores, u, if ok { 1 } else { -1 });
            }
        }

        let mut tasks = self.inner.tasks.lock();
        let t = tasks.get_mut(id).ok_or(EngineError::NotFound)?;
        let etag_changed = probe.etag.is_some() && t.etag.is_some() && probe.etag != t.etag;
        if etag_changed {
            // 新源内容变了 → 旧代次 .part 作废，重下（Q-B5：ETag 为准）。
            // gen+1 → 新循环用 `<dest>.<gen>.part`，与旧循环写隔离。
            remove_part(&part_path_of(&t.dest, t.gen));
            t.done = 0;
            t.verify_attempts = 0;
            t.state = EngineState::Downloading;
            t.error = None;
            t.gen += 1; // 废弃旧下载循环
        }
        t.mirrors = urls.clone();
        // 新候选宇宙 → 轮换池重置（E3：旧池候选集合已过时）
        t.rotate_pool.clear();
        if let Some(e) = &probe.etag {
            t.etag = Some(e.clone());
        }
        let (spawn, new_gen, epoch) = if etag_changed {
            (true, t.gen, t.epoch)
        } else {
            (false, 0, 0)
        };
        drop(tasks);
        if spawn {
            self.spawn_download(id.clone(), new_gen, epoch);
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
