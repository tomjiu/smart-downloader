//! SFTP 协议子集（C-S1，feature=`sftp`）：
//! russh（SSH2 传输，纯 Rust 栈：curve25519 kex + chacha20/aes RustCrypto
//! cipher，无 OpenSSL/aws-lc-rs）+ russh-sftp（SFTP v3 子系统）。
//! 单文件下载：动态分段并行（SFTP read 随机偏移天然支持 REST 语义）+
//! 段账本续传（P4 唯一进度真源）+ 顺序下载窗口（边下边播）+ 任务级/全局
//! 限速（E16）——分段策略与 FTP/HTTP 直链同一套（static_split/segment_count）。
//! v1 边界：不支持目录下载（`/` 结尾 → 路由层报错）、公钥/交互认证
//! （仅密码认证）、known_hosts 主机密校验（见 AcceptHostKey 注释）。

use crate::download::SEQUENTIAL_WINDOW;
use crate::ledger;
use crate::rate::{RateLimiter, RateSample};
use crate::retry::Backoff;
use crate::segment_manager::{
    Segment as DynSegment, SegmentManager, DEFAULT_MIN_SPLIT, MIN_RETRY_GRANULARITY,
};
use crate::static_split::segment_count;
use parking_lot::Mutex;
use smart_dl_core::task::DownloadTask;
use smart_dl_core::types::{
    Capability, DownloadEngine, DownloadSource, EngineError, EngineKind, EngineState, EngineStatus,
    EngineTaskId, PeerInfo,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// SFTP 段下载流式写入块大小：64KB（与 FTP/HTTP 块缓冲同级），避免整段驻留
/// 内存；russh-sftp 客户端内部再按 max_packet_len（~32KB）切片发 READ。
/// 失败段不入账本，重试/恢复路径 seek 回 seg.start 全量重写。
const SFTP_CHUNK: usize = 64 * 1024;

/// 连接/认证失败重试次数（连接层退避；与 ftp.rs CONNECT_ATTEMPTS 同口径）。
const CONNECT_ATTEMPTS: u32 = 4;

/// v1 主机密策略：`check_server_key` 全接受。
///
/// 背景：严格校验需要 known_hosts 基础设施（TOFU 首录 + 换钥提示 + 存储/更新
/// 面），v1 不引入。风险敞口 = 首次连接的 MITM（密码仍走加密通道传输，
/// 不明文暴露）；与 v1 FTPS 不暴露 insecure 口子的立场差异：SFTP 的
/// 主机密是「每次连接都要决策」的交互面，静默失败比显式接受更伤可用性，
/// 故 v1 显式接受 + 文档声明，known_hosts 后续按需补（rustls 不受限）。
struct AcceptHostKey;

impl russh::client::Handler for AcceptHostKey {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// 解析 `sftp://[user[:pass]@]host[:port]/path`（v1 单文件）：
/// 目录（路径以 `/` 结尾）或空路径 → None（路由层给出明确错误）；
/// user 缺失 → None（SSH 无匿名惯例，必须显式提供）。
/// 返回 `(host, port, user, pass, path)`，默认端口 22。
fn parse_sftp_url(url: &str) -> Option<(String, u16, String, String, String)> {
    let rest = url.strip_prefix("sftp://")?;
    let (auth_host, raw_path) = rest.split_once('/')?;
    // 空路径（`sftp://host/` 或 `sftp://host`）→ v1 不支持
    if raw_path.is_empty() || raw_path.ends_with('/') {
        return None;
    }
    let (auth, host_port) = match auth_host.rsplit_once('@') {
        Some((a, hp)) => (a, hp),
        None => ("", auth_host),
    };
    let (user, pass) = match auth.split_once(':') {
        Some((u, p)) => (u.to_string(), p.to_string()),
        _ => (auth.to_string(), String::new()),
    };
    if user.is_empty() {
        return None; // SSH 无匿名惯例：user 必填
    }
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()?),
        None => (host_port.to_string(), 22),
    };
    if host.is_empty() {
        return None;
    }
    Some((host, port, user, pass, format!("/{raw_path}")))
}

/// 终态错误判定：NoSuchFile/PermissionDenied 为协议级永久失败（重试无意义，
/// 不参与二分拆分栈）；其余（IO/超时/连接断开）可重试。错误串匹配：
/// russh-sftp Status 展示为 "{status_code}: {error_message}"，状态码枚举
/// Display 固定为 "No such file"/"Permission denied"（SFTP v3 RFC 口径）。
fn is_terminal(e: &str) -> bool {
    let l = e.to_ascii_lowercase();
    l.contains("no such file") || l.contains("permission denied")
}

struct SftpTask {
    host: String,
    port: u16,
    user: String,
    pass: String,
    path: String,
    /// 单文件任务：目标文件路径。
    dest: PathBuf,
    total: u64,
    state: EngineState,
    done: u64,
    /// 速率采样器（E11）：status() 读取时增量采样（B/s）。
    rate: RateSample,
    error: Option<String>,
    /// 任务级下载限速（KiB/s 配置回显；None = 走全局）。
    limit_kb_s: Option<u32>,
    /// 顺序下载（边下边播）：与 FTP 同语义（在飞段窗口收紧）。
    sequential: bool,
    /// 审计修复（P1-4）：真暂停闸门（与 FTP/HTTP 同构）。
    pause: Arc<AtomicBool>,
    /// batch3-P0 epoch 单写者闸门（与 FTP 同构）：resume/remove 先自增；
    /// 循环过期即退出且绝不 finalize，根治 pause→resume 竞态与双循环并发写。
    epoch: Arc<AtomicU64>,
}

struct EngineInner {
    tasks: Mutex<HashMap<EngineTaskId, SftpTask>>,
    /// 引擎全局限速器（E16 总阀门）：0 = 不限。
    limiter: Arc<RateLimiter>,
    /// 任务级限速登记表（E16）：Arc 共享使运行中热调即时生效。
    limiters: Mutex<HashMap<EngineTaskId, Arc<RateLimiter>>>,
    /// 动态分段粒度（字节，0 = 默认 16MB）；测试注入小粒度覆盖多段路径。
    min_split: u64,
}

/// SFTP 引擎（动态分段并行下载：每段独立 SSH 会话 + SFTP offset 读取）。
#[derive(Clone)]
pub struct SftpEngine {
    backoff: Backoff,
    inner: Arc<EngineInner>,
}

impl SftpEngine {
    pub fn new() -> Self {
        SftpEngine::with_backoff(Backoff::default())
    }

    /// 可注入退避（连接失败退避测试用短退避）。
    pub fn with_backoff(backoff: Backoff) -> Self {
        SftpEngine::with_backoff_limited(backoff, 0)
    }

    /// `download_kb_s` = 全局下载限速 KiB/s（0 = 不限；E16 总阀门）。
    pub fn new_limited(download_kb_s: u32) -> Self {
        SftpEngine::with_backoff_limited(Backoff::default(), download_kb_s)
    }

    /// 可注入退避 + 全局限速（E16）。
    pub fn with_backoff_limited(backoff: Backoff, download_kb_s: u32) -> Self {
        SftpEngine {
            backoff,
            inner: Arc::new(EngineInner {
                tasks: Mutex::new(HashMap::new()),
                limiter: Arc::new(RateLimiter::new(download_kb_s)),
                limiters: Mutex::new(HashMap::new()),
                min_split: 0,
            }),
        }
    }

    /// 注入动态分段粒度（字节，0 = 默认 16MB）。测试用小粒度覆盖
    /// 多段/账本续传路径；生产路径恒走默认（与 FTP/HTTP 同一粒度语义）。
    pub fn with_min_split(mut self, min_split: u64) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.min_split = min_split;
        }
        self
    }
}

impl Default for SftpEngine {
    fn default() -> Self {
        SftpEngine::new()
    }
}

/// 建立 SSH 连接 + 密码认证 + 打开 SFTP 子系统会话。
/// v1 仅密码认证（公钥/agent/交互认证后续按需）；主机密策略见 AcceptHostKey。
/// 审计修复（P1-6）：SSH 连接建立超时——russh 默认无超时，半开/黑洞地址
/// 时 worker 永久阻塞。
const SFTP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// 审计修复（P1-6）：SFTP 段数据读取超时（对齐 HTTP/FTP 口径）。
const SFTP_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

async fn connect_sftp(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
) -> Result<russh_sftp::client::SftpSession, String> {
    let config = Arc::new(russh::client::Config::default());
    let mut handle = tokio::time::timeout(
        SFTP_CONNECT_TIMEOUT,
        russh::client::connect(config, (host, port), AcceptHostKey),
    )
    .await
    .map_err(|_| "ssh connect timeout".to_string())?
    .map_err(|e| format!("ssh 连接失败: {e}"))?;
    let auth = handle
        .authenticate_password(user, pass)
        .await
        .map_err(|e| format!("ssh 认证失败: {e}"))?;
    if !matches!(auth, russh::client::AuthResult::Success) {
        return Err(format!("sftp 认证被拒绝（user={user}）"));
    }
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("ssh channel 打开失败: {e}"))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("sftp 子系统请求失败: {e}"))?;
    russh_sftp::client::SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| format!("sftp 会话初始化失败: {e}"))
}

/// 探测文件大小：连接 + 认证 + stat（目录 → 明确报错；无 size 属性 → 报错）。
async fn probe_size(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    path: &str,
) -> Result<u64, String> {
    let sftp = connect_sftp(host, port, user, pass).await?;
    let md = sftp
        .metadata(path)
        .await
        .map_err(|e| format!("sftp stat {path}: {e}"))?;
    if md.is_dir() {
        return Err(format!("sftp {path}: 目录不支持（v1 单文件下载）"));
    }
    md.size
        .ok_or_else(|| format!("sftp stat {path}: 服务器未提供 size 属性"))
}

/// 下载一个段（独立 SSH 会话：连接+认证+open+seek(offset)+read），写入 .part 段位置。
/// SFTP read 协议天然带偏移（对比 FTP 需 REST 预置），短读不终止流（客户端
/// File 内部按 max_packet_len 切片，0 字节读 = 服务器 EOF）。
// 参数即协议会话要素（主机/凭据/路径/段/目标/限速），拆 struct 反而模糊调用点语义。
#[allow(clippy::too_many_arguments)]
async fn download_segment(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    path: &str,
    seg: DynSegment,
    part: &Path,
    limiter: &RateLimiter,
) -> Result<(), String> {
    let sftp = connect_sftp(host, port, user, pass).await?;
    let mut f = sftp
        .open(path)
        .await
        .map_err(|e| format!("sftp open {path}: {e}"))?;
    f.seek(std::io::SeekFrom::Start(seg.start))
        .await
        .map_err(|e| format!("sftp seek {}: {e}", seg.start))?;
    // 流式直写 .part 段位置（段不相交 → 无锁，与 FTP/HTTP 同）。
    // 固定 64KB 块缓冲；失败时部分写入由重试/恢复路径 seek 回 seg.start
    // 全量重写（段未入账本前不构成有效凭据）。
    use std::io::{Seek, SeekFrom, Write};
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(part)
        .map_err(|e| e.to_string())?;
    out.seek(SeekFrom::Start(seg.start))
        .map_err(|e| e.to_string())?;
    let need = seg.len() as usize;
    let mut chunk = vec![0u8; SFTP_CHUNK];
    let mut got = 0usize;
    while got < need {
        let want = (need - got).min(SFTP_CHUNK);
        // 审计修复（P1-6）：段读取超时——服务器停发时不再永久阻塞
        let n = tokio::time::timeout(SFTP_IO_TIMEOUT, f.read(&mut chunk[..want]))
            .await
            .map_err(|_| "sftp read timeout".to_string())?
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err(format!("sftp read closed early: {got}/{need}"));
        }
        // E16 全局限速：逐块消费全局预算（限速器内部计数，速率 0 早退零开销）
        limiter.wait(n as u64).await;
        out.write_all(&chunk[..n]).map_err(|e| e.to_string())?;
        got += n;
    }
    Ok(())
}

/// 段下载 + 失败缩小粒度重试（P1，与 FTP/HTTP 同构）：失败段可拆
/// （len/2 >= MIN_RETRY_GRANULARITY）则二分重试栈继续，否则上抛；终态错误
/// （NoSuchFile/PermissionDenied，协议级永久失败）不拆直接上抛。子段区间恒
/// 落在原段内 → 部分写入由后续成功尝试全量重写；账本仍按原段边界记账。
// 参数即协议会话要素（主机/凭据/路径/段/目标/限速），拆 struct 反而模糊调用点语义。
#[allow(clippy::too_many_arguments)]
async fn download_segment_with_retry(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    path: &str,
    seg: DynSegment,
    part: &Path,
    limiter: &RateLimiter,
    backoff: &Backoff,
) -> Result<(), String> {
    let mut stack: Vec<DynSegment> = vec![seg];
    while let Some(cur) = stack.pop() {
        match download_segment_attempts(host, port, user, pass, path, cur, part, limiter, backoff)
            .await
        {
            Ok(()) => {}
            Err(e) if is_terminal(&e) => {
                return Err(format!("segment [{}, {}]: {e}", cur.start, cur.end))
            }
            Err(_) if cur.len() / 2 >= MIN_RETRY_GRANULARITY => {
                let mid = cur.start + cur.len() / 2;
                // 先压 right 再压 left → 先处理 left，与 HTTP/FTP 侧拆分顺序一致
                stack.push(DynSegment {
                    start: mid,
                    end: cur.end,
                });
                stack.push(DynSegment {
                    start: cur.start,
                    end: mid - 1,
                });
            }
            Err(e) => return Err(format!("segment [{}, {}]: {e}", cur.start, cur.end)),
        }
    }
    Ok(())
}

/// 单个子段的连接层退避重试（连接失败/IO；NoSuchFile/PermissionDenied 终态
/// 直接失败）——每次栈内尝试都保有完整退避预算，最坏尝试次数 =
/// 拆分深度 × CONNECT_ATTEMPTS，有界。
// 参数即协议会话要素，拆 struct 反而模糊调用点语义。
#[allow(clippy::too_many_arguments)]
async fn download_segment_attempts(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    path: &str,
    seg: DynSegment,
    part: &Path,
    limiter: &RateLimiter,
    backoff: &Backoff,
) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 1..=CONNECT_ATTEMPTS {
        match download_segment(host, port, user, pass, path, seg, part, limiter).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                if attempt < CONNECT_ATTEMPTS && !is_terminal(&e) {
                    tokio::time::sleep(backoff.next_delay(attempt)).await;
                    continue;
                }
                last = e;
                break;
            }
        }
    }
    Err(last)
}

/// download_file 结局（审计修复 P1-4）：Completed / Paused（语义与 FTP 同）。
enum SftpOutcome {
    Completed,
    Paused,
}

/// batch3-P0：段边界暂停检查（与 FTP 同构）——旗标置位 → 锁存退出原因。
fn pause_hit(pause: &Option<Arc<AtomicBool>>, paused_seen: &Arc<AtomicBool>) -> bool {
    if pause.as_ref().is_some_and(|p| p.load(Ordering::SeqCst)) {
        paused_seen.store(true, Ordering::SeqCst);
        return true;
    }
    false
}

#[allow(clippy::too_many_arguments)]
async fn download_file(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    path: &str,
    dest: &Path,
    total: u64,
    backoff: Backoff,
    limiter: &RateLimiter,
    min_split: u64,
    sequential: bool,
    on_progress: Arc<dyn Fn(u64) + Send + Sync>,
    pause: Option<Arc<AtomicBool>>,
    epoch: Arc<AtomicU64>,
) -> Result<SftpOutcome, String> {
    let epoch0 = epoch.load(Ordering::SeqCst);
    let part = part_path_of(dest);
    let ledger_path = ledger::ledger_path(&part);
    // 段账本加载（P4 唯一进度真源，与 FTP/HTTP 同口径）：合法账本 →
    // 恢复已完成段并沿用其粒度；缺失/损坏/total 失配 → 全新计划 + .part 作废。
    let loaded = ledger::load(&ledger_path).filter(|l| l.total == total && l.validate_segments());
    if loaded.is_none() {
        let _ = std::fs::remove_file(&part);
    }
    // 生效粒度：账本恢复沿用其粒度，否则用调用方注入（0 = 默认 16MB）
    let eff_min_split = loaded
        .as_ref()
        .map(|l| l.min_split)
        .unwrap_or(if min_split == 0 {
            DEFAULT_MIN_SPLIT
        } else {
            min_split
        });

    // 预分配 .part（续传场景：旧 .part 保留只写缺失段，不截断）
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&part)
        .map_err(|e| format!("part open: {e}"))?
        .set_len(total)
        .map_err(|e| format!("part open: {e}"))?;

    // 段管理器：账本恢复（跳过已完成段，折算 done_bytes）或全新计划
    let manager = Arc::new(Mutex::new(match &loaded {
        Some(l) => SegmentManager::new_with_done(total, l.min_split, &l.done),
        None => SegmentManager::new(total, 0, eff_min_split),
    }));
    // 恢复进度立即可见（账本折算字节，daemon 轮询无需等首段）
    let done0 = manager.lock().done_bytes();
    if done0 > 0 {
        on_progress(done0);
    }

    // 顺序模式在飞闸门（与 FTP/HTTP 同构）：permit 从领取前持有到 complete
    // 后释放（RAII）。在飞段数 ≤ SEQUENTIAL_WINDOW → 前缀尽快完整（边下边播）。
    let seq_gate: Option<Arc<tokio::sync::Semaphore>> = if sequential {
        Some(Arc::new(tokio::sync::Semaphore::new(SEQUENTIAL_WINDOW)))
    } else {
        None
    };
    // batch3-P0：暂停退出原因锁存槽（与 FTP 同构）
    let paused_seen: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

    // worker 数：与 FTP/HTTP 同一公式（静态 2-8）。<16MB 单段时多出的 worker
    // 领不到段（Drained）即退，零开销。
    let n_workers = segment_count(total);
    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..n_workers {
        let host = host.to_string();
        let user = user.to_string();
        let pass = pass.to_string();
        let path = path.to_string();
        let part = part.clone();
        let limiter = limiter.clone();
        let manager = manager.clone();
        let seq_gate = seq_gate.clone();
        let ledger_path = ledger_path.clone();
        let on_progress = on_progress.clone();
        let pause = pause.clone();
        let paused_seen = paused_seen.clone();
        let epoch = epoch.clone();
        workers.spawn(async move {
            loop {
                // 审计修复（P1-4）：段边界检查暂停旗标（与 FTP 同构）；
                // batch3-P0：epoch 过期即退出（新循环已成唯一写者）
                if epoch.load(Ordering::SeqCst) != epoch0 {
                    return Ok::<(), String>(());
                }
                if pause_hit(&pause, &paused_seen) {
                    return Ok::<(), String>(());
                }
                // 顺序模式：先拿 permit 再领取段，保证「在飞段数 ≤ 窗口」
                let _permit = match &seq_gate {
                    Some(g) => Some(
                        g.clone()
                            .acquire_owned()
                            .await
                            .map_err(|_| "sequential gate closed".to_string())?,
                    ),
                    None => None,
                };
                // FIFO 领取：段天然无重叠 → .part 分区写无需文件锁
                let seg: DynSegment = {
                    let mut m = manager.lock();
                    match m.take_segment() {
                        Some(s) => s,
                        None => return Ok::<(), String>(()),
                    }
                };
                download_segment_with_retry(
                    &host, port, &user, &pass, &path, seg, &part, &limiter, &backoff,
                )
                .await?;
                // 段完成：记账 + 账本原子落盘 + 进度回报（锁内一并）
                {
                    let mut m = manager.lock();
                    m.complete(seg);
                    let snapshot = ledger::Ledger {
                        version: ledger::LEDGER_VERSION,
                        total,
                        min_split: eff_min_split,
                        etag: None,
                        last_modified: None,
                        done: m.done_ranges().to_vec(),
                    };
                    ledger::save(&ledger_path, &snapshot);
                    on_progress(m.done_bytes());
                }
            }
        });
    }
    // 任一 worker 失败 → 整体失败，取消其余；账本保留已成功段 → 下次续传
    let mut first_err: Option<String> = None;
    while let Some(res) = workers.join_next().await {
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                workers.abort_all();
                first_err = Some(e);
                break;
            }
            Err(e) => {
                workers.abort_all();
                first_err = Some(format!("worker panicked: {e}"));
                break;
            }
        }
    }
    drop(workers);
    // 暂停优先于错误（审计修复 P1-4，与 FTP 同构）：置位后任何 worker
    // 错误/panic 都视为暂停退出；不清账本、不落位。
    // batch3-P0：退出原因以锁存槽为准，epoch 过期同样视作暂停退出
    //（不落位）；旗标活读仅兜底 err 退出未过检查点场景（保持原语义）。
    if epoch0 != epoch.load(Ordering::SeqCst)
        || paused_seen.load(Ordering::SeqCst)
        || pause.as_ref().is_some_and(|p| p.load(Ordering::SeqCst))
    {
        return Ok(SftpOutcome::Paused);
    }
    if let Some(e) = first_err {
        return Err(e);
    }

    // 全部段完成 → 清续传凭据 + 落位
    let _ = std::fs::remove_file(&ledger_path);
    finalize_part(&part, dest, total)?;
    Ok(SftpOutcome::Completed)
}

/// 单文件任务的下载循环：download_file 包装 + 任务状态落定。
async fn download_loop(inner: Arc<EngineInner>, tid: EngineTaskId, backoff: Backoff) {
    let (host, port, user, pass, path, dest, total) = {
        let tasks = inner.tasks.lock();
        let t = tasks.get(&tid).unwrap();
        (
            t.host.clone(),
            t.port,
            t.user.clone(),
            t.pass.clone(),
            t.path.clone(),
            t.dest.clone(),
            t.total,
        )
    };
    let inner2 = inner.clone();
    let tid2 = tid.clone();
    // 任务级限速优先，未登记回退全局（与 FTP/HTTP 同口径）；登记条目
    // 已在 set_limits 时串联全局上游（E16），此处直接取用即可。
    let limiter = inner
        .limiters
        .lock()
        .get(&tid)
        .cloned()
        .unwrap_or_else(|| inner.limiter.clone());
    let sequential = {
        let tasks = inner.tasks.lock();
        tasks.get(&tid).map(|t| t.sequential).unwrap_or(false)
    };
    let min_split = inner.min_split;
    let (pause_flag, epoch_flag) = {
        let tasks = inner.tasks.lock();
        tasks
            .get(&tid)
            .map(|t| (Some(t.pause.clone()), t.epoch.clone()))
            .unwrap_or((None, Arc::new(AtomicU64::new(0))))
    };
    // 审计修复（P1-3）：进度改绝对赋值（max 语义，与 FTP/HTTP 同口径）——
    // 旧实现按增量 += 累加账本折算的绝对值 → 进度虚报。
    let progress: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |n| {
        let mut tasks = inner2.tasks.lock();
        if let Some(t) = tasks.get_mut(&tid2) {
            t.done = t.done.max(n.min(t.total));
        }
    });
    let r = download_file(
        &host, port, &user, &pass, &path, &dest, total, backoff, &limiter, min_split, sequential,
        progress, pause_flag, epoch_flag,
    )
    .await;
    match r {
        Ok(SftpOutcome::Completed) => finish(&inner, &tid, EngineState::Completed, None),
        // 审计修复（P1-4）：暂停退出不落定状态（pause() 已置 Paused）
        Ok(SftpOutcome::Paused) => {}
        Err(e) => finish(&inner, &tid, EngineState::Error, Some(e)),
    }
}

/// 可靠性（V11 同款）：spawn 下载循环 + panic 收尸监控——循环 panic 静默变
/// 僵尸的缺陷在 FTP 侧已修，SFTP 直接以修复后形态落地（状态可见、轮询可推进）。
fn spawn_sftp_loop<F, Fut>(
    f: F,
    inner: std::sync::Arc<EngineInner>,
    tid: EngineTaskId,
    backoff: Backoff,
) where
    F: FnOnce(std::sync::Arc<EngineInner>, EngineTaskId, Backoff) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let handle = tokio::spawn(f(inner.clone(), tid.clone(), backoff));
    tokio::spawn(async move {
        if let Err(e) = handle.await {
            if e.is_panic() {
                let msg = format!("SFTP 下载循环 panic（V11 收尸）: {e}");
                tracing::error!("[V11] tid={tid}: {msg}");
                let mut tasks = inner.tasks.lock();
                if let Some(t) = tasks.get_mut(&tid) {
                    t.state = EngineState::Error;
                    t.error = Some(msg);
                }
            }
        }
    });
}

fn part_path_of(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

fn finalize_part(part: &Path, dest: &Path, total: u64) -> Result<(), String> {
    let om = smart_dl_core::session::output::OutputManager::new(PathBuf::from("."));
    om.finalize_to(part, dest, total).map_err(|e| e.to_string())
}

fn finish(inner: &Arc<EngineInner>, tid: &str, state: EngineState, error: Option<String>) {
    let mut tasks = inner.tasks.lock();
    if let Some(t) = tasks.get_mut(tid) {
        t.state = state;
        t.error = error;
        if state == EngineState::Completed {
            t.done = t.total;
        }
    }
}

/// 现有 .part 已下载字节数（续传起点折算，仅展示口径；账本才是真源）。
/// batch3-P1：段账本折算已完成字节（P4 唯一进度真源）；无账本 → 0。
fn ledger_done_bytes(part: &Path) -> u64 {
    let lp = ledger::ledger_path(part);
    ledger::load(&lp)
        .filter(|l| l.validate_segments())
        .map(|l| l.done.iter().map(|(s, e)| e - s + 1).sum::<u64>())
        .unwrap_or(0)
}

#[async_trait::async_trait]
impl DownloadEngine for SftpEngine {
    fn id(&self) -> &str {
        "sftp"
    }

    fn kind(&self) -> EngineKind {
        EngineKind::Sftp
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Sftp]
    }

    /// 引擎全局限速热改（E16 trait 扩展）：与 FTP 同口径。仅 down 方向。
    async fn set_global_limits(
        &self,
        down_kb_s: Option<u32>,
        up_kb_s: Option<u32>,
    ) -> Result<(), EngineError> {
        if up_kb_s.is_some() {
            return Err(EngineError::Other(
                "SFTP 引擎无上传方向，up_kb_s 不适用".to_string(),
            ));
        }
        if let Some(kb) = down_kb_s {
            self.inner.limiter.set_rate_kb_s(kb);
        }
        Ok(())
    }

    /// 任务级下载限速（E16，与 FTP 同口径）：任务专属 limiter 登记进 limiters
    /// 表（串联全局上游）；已登记 → 原地热调；未登记 → 新登记，下一次重下轮
    /// 拾取。仅 down 方向。
    async fn set_limits(
        &self,
        id: &EngineTaskId,
        down_kb_s: Option<u32>,
        up_kb_s: Option<u32>,
    ) -> Result<(), EngineError> {
        if up_kb_s.is_some() {
            return Err(EngineError::Other(
                "SFTP 引擎无上传方向，up_kb_s 不适用".to_string(),
            ));
        }
        let Some(kb) = down_kb_s else { return Ok(()) }; // 双 None = no-op
        {
            let mut tasks = self.inner.tasks.lock();
            let Some(t) = tasks.get_mut(id) else {
                return Err(EngineError::NotFound);
            };
            // 配置回显记到任务快照上（审计/透出口径）
            t.limit_kb_s = Some(kb);
        }
        let mut limiters = self.inner.limiters.lock();
        match limiters.get(id) {
            Some(lim) => lim.set_rate_kb_s(kb), // 已有限速器 → 原地热调
            None => {
                limiters.insert(
                    id.clone(),
                    Arc::new(RateLimiter::new_chained(kb, &self.inner.limiter)),
                );
            }
        }
        Ok(())
    }

    /// 任务级顺序下载开关（与 FTP 同语义）：字段改写，下一次重下轮拾取；
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

    async fn add(&self, task: &DownloadTask) -> Result<EngineTaskId, EngineError> {
        let (url, _user, _pass) = match &task.source {
            // user/pass 以 URL 重解析为准（parse_sftp_url 顺带校验 user 必填）
            DownloadSource::Sftp { url, .. } => (url.clone(), String::new(), String::new()),
            _ => return Err(EngineError::Other("source is not sftp".to_string())),
        };
        let (host, port, user, pass, path) = parse_sftp_url(&url).ok_or_else(|| {
            EngineError::Other("invalid sftp url（需 sftp://user@host/path 单文件）".to_string())
        })?;

        // 探测：连接 + 认证 + stat（连接类失败 → 退避重试；NoSuchFile/
        // PermissionDenied 终态直接失败）
        let total = {
            let mut last = String::new();
            let mut size: Option<u64> = None;
            for attempt in 1..=CONNECT_ATTEMPTS {
                match probe_size(&host, port, &user, &pass, &path).await {
                    Ok(t) => {
                        size = Some(t);
                        break;
                    }
                    Err(e) => {
                        last = e;
                        if attempt < CONNECT_ATTEMPTS && !is_terminal(&last) {
                            tokio::time::sleep(self.backoff.next_delay(attempt)).await;
                            continue;
                        }
                        break;
                    }
                }
            }
            match size {
                Some(t) => t,
                None => return Err(EngineError::Other(format!("sftp probe failed: {last}"))),
            }
        };

        let rel = task
            .metadata
            .name
            .clone()
            .unwrap_or_else(|| "download.bin".to_string());
        // 安全修复（V3 同款）：任务名净化后再 join（拒 .. / 绝对路径）。
        let rel_pb = smart_dl_core::session::output::sanitize_rel(&rel)
            .map_err(|e| EngineError::Other(e.to_string()))?;
        let dest = task.dest_root.join(&rel_pb);
        // .part 超长（源变小）→ 作废
        let part = part_path_of(&dest);
        if let Ok(md) = std::fs::metadata(&part) {
            if md.len() > total {
                let _ = std::fs::remove_file(&part);
            }
        }

        let tid = task.id.clone();
        {
            let mut tasks = self.inner.tasks.lock();
            tasks.insert(
                tid.clone(),
                SftpTask {
                    host,
                    port,
                    user,
                    pass,
                    path,
                    dest,
                    total,
                    state: EngineState::Downloading,
                    // batch3-P1：账本折算（.part 预分配长度恒 total，
                    // 旧值会让重启后进度恒 100% 且 max 语义不可回退）
                    done: ledger_done_bytes(&part),
                    rate: RateSample::default(),
                    error: None,
                    limit_kb_s: None,
                    sequential: task.sequential,
                    pause: Arc::new(AtomicBool::new(false)),
                    epoch: Arc::new(AtomicU64::new(0)),
                },
            );
        }
        let inner = self.inner.clone();
        let backoff = self.backoff;
        let spawn_tid = tid.clone();
        spawn_sftp_loop(download_loop, inner, spawn_tid, backoff);
        Ok(tid)
    }

    /// 审计修复（P1-4）：真暂停（与 FTP 同构）——置位闸门 + 状态 Paused；
    /// worker 在段边界退出，download_loop 识别 Paused 结局不 finish。
    async fn pause(&self, id: &EngineTaskId) -> Result<(), EngineError> {
        let mut tasks = self.inner.tasks.lock();
        let t = tasks.get_mut(id).ok_or(EngineError::NotFound)?;
        t.pause.store(true, Ordering::SeqCst);
        t.state = EngineState::Paused;
        Ok(())
    }

    /// 审计修复（P1-4）：恢复 = 清闸门 + 重新 spawn 下载循环（账本续传，
    /// 进度 max 语义不重复累计）。
    async fn resume(&self, id: &EngineTaskId) -> Result<(), EngineError> {
        let was_paused = {
            let mut tasks = self.inner.tasks.lock();
            let t = tasks.get_mut(id).ok_or(EngineError::NotFound)?;
            let was = t.state == EngineState::Paused;
            // batch3-P0：先自增 epoch 再清旗标（与 FTP 同构）
            t.epoch.fetch_add(1, Ordering::SeqCst);
            t.pause.store(false, Ordering::SeqCst);
            t.state = EngineState::Downloading;
            was
        };
        // 非暂停态的 resume 维持既有无操作语义，不重复 spawn
        if !was_paused {
            return Ok(());
        }
        let inner = self.inner.clone();
        let backoff = self.backoff;
        let spawn_tid = id.clone();
        spawn_sftp_loop(download_loop, inner, spawn_tid, backoff);
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
            // E33：SFTP 单向引擎无累计统计口径，恒 0（快照序列化省略）
            total_downloaded: 0,
            total_uploaded: 0,
            num_peers: 0,
            num_seeds: 0,
            error: t.error.clone(),
            // SFTP 不参与 E9 名字回填：daemon add 时已派生 URL 末段名
            name: None,
        })
    }

    async fn remove(&self, id: &EngineTaskId, _delete_data: bool) -> Result<(), EngineError> {
        // 审计修复（P1-4）：置位暂停闸门再移除——运行中循环在段边界退出，
        // 不再继续占用带宽/写 .part/把文件 rename 落位。
        {
            let tasks = self.inner.tasks.lock();
            if let Some(t) = tasks.get(id) {
                t.epoch.fetch_add(1, Ordering::SeqCst);
                t.pause.store(true, Ordering::SeqCst);
            }
        }
        let mut tasks = self.inner.tasks.lock();
        tasks.remove(id).ok_or(EngineError::NotFound)?;
        // 任务级限速登记一并回收（防表无限增长；与 FTP/HTTP 同口径）
        self.inner.limiters.lock().remove(id);
        Ok(())
    }

    async fn peers(&self, _id: &EngineTaskId) -> Result<Vec<PeerInfo>, EngineError> {
        Ok(vec![])
    }

    async fn update_sources(
        &self,
        _id: &EngineTaskId,
        _urls: Vec<String>,
    ) -> Result<(), EngineError> {
        Ok(())
    }

    async fn add_url_seed(&self, _id: &EngineTaskId, _url: &str) -> Result<(), EngineError> {
        Ok(())
    }

    async fn ban_peer(
        &self,
        _id: &EngineTaskId,
        _peer: std::net::SocketAddr,
    ) -> Result<(), EngineError> {
        Ok(())
    }

    async fn read_piece(&self, _id: &EngineTaskId, _idx: u32) -> Result<Vec<u8>, EngineError> {
        Err(EngineError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::server::{Auth, ChannelOpenHandle, Msg, Session as ServerSession};
    use russh::Channel;
    use russh_sftp::protocol::{Attrs, Data, FileAttributes, Handle, Status, StatusCode, Version};
    use std::time::Duration;

    // ---------- hermetic SFTP 测试桩：russh 传输 + russh-sftp 子系统 ----------
    // 最小文件后端（stat/open/read/close），内容驻内存；127.0.0.1:0 临时端口。

    #[derive(Clone)]
    struct TestSshServer {
        user: String,
        pass: String,
        files: Arc<HashMap<String, Arc<Vec<u8>>>>,
        channels: Arc<tokio::sync::Mutex<HashMap<russh::ChannelId, Channel<Msg>>>>,
    }

    impl russh::server::Handler for TestSshServer {
        type Error = russh::Error;

        async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
            if user == self.user && password == self.pass {
                Ok(Auth::Accept)
            } else {
                Ok(Auth::reject())
            }
        }

        async fn channel_open_session(
            &mut self,
            channel: Channel<Msg>,
            reply: ChannelOpenHandle,
            _session: &mut ServerSession,
        ) -> Result<(), Self::Error> {
            self.channels.lock().await.insert(channel.id(), channel);
            reply.accept().await;
            Ok(())
        }

        async fn subsystem_request(
            &mut self,
            channel_id: russh::ChannelId,
            name: &str,
            session: &mut ServerSession,
        ) -> Result<(), Self::Error> {
            if name != "sftp" {
                session.channel_failure(channel_id)?;
                return Ok(());
            }
            let Some(channel) = self.channels.lock().await.remove(&channel_id) else {
                return Ok(());
            };
            session.channel_success(channel_id)?;
            let fs = TestSftpFs {
                files: self.files.clone(),
            };
            russh_sftp::server::run(channel.into_stream(), fs).await;
            Ok(())
        }
    }

    struct TestSftpFs {
        files: Arc<HashMap<String, Arc<Vec<u8>>>>,
    }

    impl russh_sftp::server::Handler for TestSftpFs {
        type Error = StatusCode;

        fn unimplemented(&self) -> Self::Error {
            StatusCode::OpUnsupported
        }

        async fn init(
            &mut self,
            _version: u32,
            _extensions: HashMap<String, String>,
        ) -> Result<Version, Self::Error> {
            Ok(Version::new())
        }

        async fn open(
            &mut self,
            id: u32,
            filename: String,
            _pflags: russh_sftp::protocol::OpenFlags,
            _attrs: FileAttributes,
        ) -> Result<Handle, Self::Error> {
            if !self.files.contains_key(&filename) {
                return Err(StatusCode::NoSuchFile);
            }
            Ok(Handle {
                id,
                handle: filename,
            })
        }

        async fn read(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            len: u32,
        ) -> Result<Data, Self::Error> {
            let f = self.files.get(&handle).ok_or(StatusCode::NoSuchFile)?;
            if offset >= f.len() as u64 {
                return Err(StatusCode::Eof);
            }
            let end = std::cmp::min(offset + len as u64, f.len() as u64) as usize;
            Ok(Data {
                id,
                data: f[offset as usize..end].to_vec(),
            })
        }

        async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
            let f = self.files.get(&handle).ok_or(StatusCode::NoSuchFile)?;
            Ok(Attrs {
                id,
                attrs: FileAttributes {
                    size: Some(f.len() as u64),
                    ..Default::default()
                },
            })
        }

        async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            let f = self.files.get(&path).ok_or(StatusCode::NoSuchFile)?;
            Ok(Attrs {
                id,
                attrs: FileAttributes {
                    size: Some(f.len() as u64),
                    ..Default::default()
                },
            })
        }

        async fn close(&mut self, id: u32, _handle: String) -> Result<Status, Self::Error> {
            Ok(Status {
                id,
                status_code: StatusCode::Ok,
                error_message: "Ok".to_string(),
                language_tag: "en-US".to_string(),
            })
        }
    }

    /// 起一个 127.0.0.1:0 的 hermetic SFTP 服务，返回端口号。
    async fn spawn_test_sftp_server(
        files: HashMap<String, Vec<u8>>,
        user: &str,
        pass: &str,
    ) -> u16 {
        let config = Arc::new(russh::server::Config {
            auth_rejection_time: Duration::from_secs(1),
            auth_rejection_time_initial: Some(Duration::from_secs(0)),
            keys: vec![russh::keys::PrivateKey::random(
                &mut rand::rng(),
                russh::keys::Algorithm::Ed25519,
            )
            .unwrap()],
            ..Default::default()
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = TestSshServer {
            user: user.to_string(),
            pass: pass.to_string(),
            files: Arc::new(files.into_iter().map(|(k, v)| (k, Arc::new(v))).collect()),
            channels: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        };
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                let handler = state.clone();
                let cfg = config.clone();
                tokio::spawn(async move {
                    let _ = russh::server::run_stream(cfg, tcp, handler).await;
                });
            }
        });
        port
    }

    // ---------- 单元测试 ----------

    #[test]
    fn parse_sftp_url_ok() {
        let (h, p, u, pw, path) = parse_sftp_url("sftp://u:p@host:2222/dir/f.bin").unwrap();
        assert_eq!(
            (h.as_str(), p, u.as_str(), pw.as_str(), path.as_str()),
            ("host", 2222, "u", "p", "/dir/f.bin")
        );

        // 默认端口 22
        let (h, p, u, _, path) = parse_sftp_url("sftp://alice@host/data.bin").unwrap();
        assert_eq!(
            (h.as_str(), p, u.as_str(), path.as_str()),
            ("host", 22, "alice", "/data.bin")
        );
    }

    #[test]
    fn parse_sftp_url_rejects() {
        // user 必填（SSH 无匿名惯例）
        assert!(parse_sftp_url("sftp://host/f.bin").is_none());
        // 目录 / 空路径（v1 单文件）
        assert!(parse_sftp_url("sftp://u@host/").is_none());
        assert!(parse_sftp_url("sftp://u@host/dir/").is_none());
        assert!(parse_sftp_url("sftp://u@host").is_none());
        // 非法端口 / 非 sftp scheme
        assert!(parse_sftp_url("sftp://u@host:abc/f").is_none());
        assert!(parse_sftp_url("ftp://u@host/f").is_none());
    }

    #[test]
    fn terminal_errors_never_retried() {
        assert!(is_terminal("sftp stat /x: No such file: nope"));
        assert!(is_terminal("sftp open /x: Permission denied"));
        assert!(!is_terminal("ssh 连接失败: timeout"));
        assert!(!is_terminal("sftp read closed early: 3/100"));
    }

    // ---------- e2e ----------

    /// 单段经 download_segment 全路径：SSH 握手 + 密码认证 + SFTP open/
    /// seek/read + 落盘逐字节断言（与 ftp.rs FTPS e2e 同粒度）。
    #[tokio::test]
    async fn sftp_download_segment_end_to_end() {
        let content: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
        let files = HashMap::from([("/f.bin".to_string(), content.clone())]);
        let port = spawn_test_sftp_server(files, "u", "p").await;

        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("f.bin.part");
        download_segment(
            "127.0.0.1",
            port,
            "u",
            "p",
            "/f.bin",
            DynSegment {
                start: 0,
                end: content.len() as u64 - 1,
            },
            &part,
            &RateLimiter::new(0),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), content, "逐字节一致");
    }

    /// 引擎全路径：1MB × 128KB 粒度 → 8 段并行 + 账本 + finalize 落位。
    #[tokio::test]
    async fn sftp_engine_end_to_end_multi_segment() {
        let content: Vec<u8> = (0..1_048_576u32)
            .map(|i| ((i as u64 * 7) % 251) as u8)
            .collect();
        let files = HashMap::from([("/data.bin".to_string(), content.clone())]);
        let port = spawn_test_sftp_server(files, "u", "p").await;

        let dir = tempfile::tempdir().unwrap();
        let task = DownloadTask {
            id: "t-sftp-1".to_string(),
            canonical_id: smart_dl_core::identity::CanonicalId {
                kind: smart_dl_core::identity::CanonicalKind::Sftp,
                identity: "sftp-e2e".to_string(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Sftp {
                url: format!("sftp://u:p@127.0.0.1:{port}/data.bin"),
                user: "u".to_string(),
                pass: "p".to_string(),
            },
            identity: smart_dl_core::identity::ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                sha1: None,
                md5: None,
                backup_md5: None,
            },
            dest_root: dir.path().to_path_buf(),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: smart_dl_core::state_machine::TaskState::Queued,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential: false,
            metadata: smart_dl_core::task::TaskMetadata {
                name: Some("data.bin".to_string()),
                added_at_unix: 0,
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: 0,
                next_retry_at_unix: 0,
            },
            limits: None,
            max_connections: None,
            queue_priority: 0,
        };

        let engine = SftpEngine::new().with_min_split(128 * 1024);
        let tid = engine.add(&task).await.unwrap();

        // 轮询至终态（上限 30s）
        for _ in 0..600 {
            let st = engine.status(&tid).await.unwrap();
            match st.state {
                EngineState::Completed => break,
                EngineState::Error => panic!("下载失败: {:?}", st.error),
                _ => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
        let st = engine.status(&tid).await.unwrap();
        assert_eq!(st.state, EngineState::Completed);
        assert_eq!(st.total_done, content.len() as u64);
        assert_eq!(
            std::fs::read(dir.path().join("data.bin")).unwrap(),
            content,
            "多段并行 + finalize 后逐字节一致"
        );
    }

    /// 认证失败 → add 明确报错（错误信息含「认证被拒绝」）。
    #[tokio::test]
    async fn sftp_engine_auth_failure_is_reported() {
        let files = HashMap::from([("/f.bin".to_string(), vec![1u8; 16])]);
        let port = spawn_test_sftp_server(files, "u", "p").await;

        let dir = tempfile::tempdir().unwrap();
        let task = DownloadTask {
            id: "t-sftp-bad".to_string(),
            canonical_id: smart_dl_core::identity::CanonicalId {
                kind: smart_dl_core::identity::CanonicalKind::Sftp,
                identity: "sftp-bad".to_string(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Sftp {
                url: format!("sftp://u:WRONG@127.0.0.1:{port}/f.bin"),
                user: "u".to_string(),
                pass: "WRONG".to_string(),
            },
            identity: smart_dl_core::identity::ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                sha1: None,
                md5: None,
                backup_md5: None,
            },
            dest_root: dir.path().to_path_buf(),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: smart_dl_core::state_machine::TaskState::Queued,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential: false,
            metadata: smart_dl_core::task::TaskMetadata {
                name: Some("f.bin".to_string()),
                added_at_unix: 0,
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: 0,
                next_retry_at_unix: 0,
            },
            limits: None,
            max_connections: None,
            queue_priority: 0,
        };
        let engine = SftpEngine::with_backoff(Backoff::default());
        let err = engine.add(&task).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("认证被拒绝"), "实际: {msg}");
    }

    /// NoSuchFile → 终态不重试（add 快速失败，错误含 No such file）。
    #[tokio::test]
    async fn sftp_engine_missing_file_is_terminal() {
        let port = spawn_test_sftp_server(HashMap::new(), "u", "p").await;
        let dir = tempfile::tempdir().unwrap();
        let task = DownloadTask {
            id: "t-sftp-miss".to_string(),
            canonical_id: smart_dl_core::identity::CanonicalId {
                kind: smart_dl_core::identity::CanonicalKind::Sftp,
                identity: "sftp-miss".to_string(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Sftp {
                url: format!("sftp://u:p@127.0.0.1:{port}/missing.bin"),
                user: "u".to_string(),
                pass: "p".to_string(),
            },
            identity: smart_dl_core::identity::ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                sha1: None,
                md5: None,
                backup_md5: None,
            },
            dest_root: dir.path().to_path_buf(),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: smart_dl_core::state_machine::TaskState::Queued,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential: false,
            metadata: smart_dl_core::task::TaskMetadata {
                name: Some("missing.bin".to_string()),
                added_at_unix: 0,
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: 0,
                next_retry_at_unix: 0,
            },
            limits: None,
            max_connections: None,
            queue_priority: 0,
        };
        let engine = SftpEngine::with_backoff(Backoff::default());
        let started = std::time::Instant::now();
        let err = engine.add(&task).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("No such file"), "实际: {msg}");
        // 终态错误不退避重试（4 次 × 退避延迟会显著 > 5s）
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "NoSuchFile 应终态失败不重试，实际耗时 {:?}",
            started.elapsed()
        );
    }
}
