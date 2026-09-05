//! FTP 协议子集（M4c，feature=`ftp`，设计 §15）：
//! 被动模式（PASV）、REST 断点续传（.part）、421 退避重试、目录下载（LIST 单层）；
//! 不支持 SFTP/FTPS 隐式/目录递归/FXP。

use crate::download::SEQUENTIAL_WINDOW;
use crate::ledger;
use crate::rate::{RateLimiter, RateSample};
use crate::retry::Backoff;
use crate::segment_manager::{
    Segment as DynSegment, SegmentManager, DEFAULT_MIN_SPLIT, MIN_RETRY_GRANULARITY,
};
use crate::static_split::segment_count;
use parking_lot::Mutex;
use smart_dl_core::session::output::OutputManager;
use smart_dl_core::task::DownloadTask;
use smart_dl_core::types::{
    Capability, DownloadEngine, DownloadSource, EngineError, EngineKind, EngineState, EngineStatus,
    EngineTaskId, FileProgress, PeerInfo,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::task::JoinSet;

/// FTP 传输流抽象（B2 FTPS）：控制/数据连接统一为 trait object，
/// 明文 TcpStream 与 AUTH TLS 升级后的 TlsStream 同一读写口径。
trait FtpIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> FtpIo for T {}

type BoxFtpIo = Box<dyn FtpIo>;

/// FTPS 客户端 TLS 配置（B2，生产路径）：webpki-roots 固定根集 + ring
/// provider（与 reqwest rustls-tls 同源，P1-3 安卓交叉兼容）。证书校验
/// 严格开启（v1 不暴露 insecure 口子；自签名/私有 CA 场景后续按需加
/// 配置面注入自定义根集）。rustls 栈经 tokio-rustls re-export 使用
///（不引入直接依赖，与 reqwest 共享同一 rustls 0.23 编译单元）。
fn ftps_connector() -> Result<tokio_rustls::TlsConnector, String> {
    use tokio_rustls::rustls::crypto::ring as ring_backend;
    let config = tokio_rustls::rustls::ClientConfig::builder_with_provider(
        ring_backend::default_provider().into(),
    )
    .with_safe_default_protocol_versions()
    .map_err(|e| format!("FTPS 协议版本配置失败: {e}"))?
    .with_root_certificates(ftps_roots())
    .with_no_client_auth();
    Ok(tokio_rustls::TlsConnector::from(Arc::new(config)))
}

fn ftps_roots() -> tokio_rustls::rustls::RootCertStore {
    tokio_rustls::rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    }
}

/// 控制连接 AUTH TLS 升级 / 数据连接 PROT P 握手共用：TcpStream → TlsStream。
/// connector 经 active_connector() 获取（测试可注入自签信任链）。
async fn ftps_upgrade(
    host: &str,
    tcp: TcpStream,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, String> {
    let connector = active_connector()?;
    let server_name = tokio_rustls::rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| format!("FTPS SNI 非法 {host:?}: {e}"))?;
    connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("FTPS 握手失败: {e}"))
}

/// 测试专用 connector 注入点（#[cfg(test)]）：自签证书 e2e 用，生产恒空。
#[cfg(test)]
static TEST_CONNECTOR: std::sync::OnceLock<tokio_rustls::TlsConnector> = std::sync::OnceLock::new();

/// 当前生效 connector：测试注入优先，生产回退 webpki-roots。
fn active_connector() -> Result<tokio_rustls::TlsConnector, String> {
    #[cfg(test)]
    {
        if let Some(c) = TEST_CONNECTOR.get() {
            return Ok(c.clone());
        }
    }
    ftps_connector()
}

/// 421/连接失败重试次数（连接层退避）。
const CONNECT_ATTEMPTS: u32 = 4;

/// FTP 段下载流式写入块大小：固定 64KB 缓冲（与 HTTP 侧 resp.chunk() 同级），
/// 避免整段驻留内存（8 worker × 16MB 段 = 峰值 128MB+）。部分写入无害：
/// 失败段不入账本，重试/恢复路径 seek 回 seg.start 全量重写。
const FTP_CHUNK: usize = 64 * 1024;

/// FTP URL 目标：单文件或目录。
#[derive(Debug, PartialEq, Eq)]
enum FtpTarget {
    /// 远端普通文件（绝对路径，不以 `/` 结尾）。
    File(String),
    /// 远端目录（绝对路径，以 `/` 结尾；根目录为 `/`）。
    Dir(String),
}

/// 目录任务的单文件条目（远端路径 + 进度/状态）。
struct FtpFile {
    /// 文件名（相对目录，即落盘相对路径）。
    name: String,
    /// 远端绝对路径（`<目录>/<文件名>`）。
    path: String,
    size: u64,
    done: u64,
    state: EngineState,
}

struct FtpTask {
    host: String,
    port: u16,
    user: String,
    pass: String,
    path: String,
    /// B2 FTPS：ftps:// = 显式 AUTH TLS + PROT P（控制/数据连接全加密）。
    use_tls: bool,
    /// 单文件任务：目标文件路径；目录任务：目标目录路径。
    dest: PathBuf,
    total: u64,
    state: EngineState,
    done: u64,
    /// 速率采样器（E11）：status() 读取时增量采样（B/s），daemon /stats 聚合消费。
    rate: RateSample,
    error: Option<String>,
    /// 目录任务的文件级进度（单文件任务为空）。
    files: Vec<FtpFile>,
    /// 任务级下载限速（KiB/s 配置回显；None = 走全局）。实际生效速在
    /// limiters 表的 RateLimiter 上（set_limits 运行中即时改率）。
    limit_kb_s: Option<u32>,
    /// 顺序下载（边下边播）：true = download_file 在飞段窗口收紧
    /// （SEQUENTIAL_WINDOW，与 HTTP 同值同语义：前缀尽快完整，FIFO 领取
    /// 不变）。set_sequential 运行中改写 → 下一次重下轮拾取；新建任务
    /// add() 直接读 task.sequential → 立即生效。
    sequential: bool,
}

struct EngineInner {
    tasks: Mutex<HashMap<EngineTaskId, FtpTask>>,
    /// 引擎全局限速器（E16 总阀门）：0 = 不限（wait 早退零开销）；
    /// set_global_limits 运行中热调。任务级限速经 limiters 表串联本
    /// limiter（上游），所有 FTP 任务的合计带宽仍受总阀门约束。
    limiter: Arc<RateLimiter>,
    /// 任务级限速登记表（E16）：set_limits 登记 → 下载循环 spawn 时取用；
    /// Arc 共享使已登记任务运行中热调（set_rate_kb_s）即时生效。
    limiters: Mutex<HashMap<EngineTaskId, Arc<RateLimiter>>>,
    /// 动态分段粒度（字节，0 = 默认 16MB；与 HTTP 直链同粒度同源）。
    /// 测试注入小粒度以覆盖多段/账本路径。
    min_split: u64,
}

/// FTP 引擎（动态分段并行下载：PASV + REST + RETR，分段策略与 HTTP 对齐）。
#[derive(Clone)]
pub struct FtpEngine {
    backoff: Backoff,
    inner: Arc<EngineInner>,
}

impl FtpEngine {
    pub fn new() -> Self {
        FtpEngine::with_backoff(Backoff::default())
    }

    /// 可注入退避（421 测试用短退避）。
    pub fn with_backoff(backoff: Backoff) -> Self {
        FtpEngine::with_backoff_limited(backoff, 0)
    }

    /// `download_kb_s` = 全局下载限速 KiB/s（0 = 不限；E16 总阀门，
    /// 所有 FTP 任务合计不超速，运行中可经 set_global_limits 热改）。
    pub fn new_limited(download_kb_s: u32) -> Self {
        FtpEngine::with_backoff_limited(Backoff::default(), download_kb_s)
    }

    /// 可注入退避 + 全局限速（E16）。
    pub fn with_backoff_limited(backoff: Backoff, download_kb_s: u32) -> Self {
        FtpEngine {
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
    /// 多段/账本续传路径；生产路径恒走默认（与 HTTP 直链同一粒度语义）。
    pub fn with_min_split(mut self, min_split: u64) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.min_split = min_split;
        }
        self
    }

    /// 目录分支：LIST 探测（421 → 退避重试）→ 解析文件列表 → 建任务 → spawn 目录循环。
    /// 落位 `dest_root/<目录名>/<文件名>`；目录名取 URL 路径最后一段非空名称（根目录 → host）。
    #[allow(clippy::too_many_arguments)] // 参数即协议会话要素，拆 struct 反而模糊调用点语义
    async fn add_directory(
        &self,
        task: &DownloadTask,
        host: String,
        port: u16,
        user: String,
        pass: String,
        path: String,
        use_tls: bool,
    ) -> Result<EngineTaskId, EngineError> {
        // LIST 探测：连接 + 登录 + PASV + LIST（421 → 退避重试）
        let listing = {
            let mut last = String::new();
            let mut listing: Option<String> = None;
            for attempt in 1..=CONNECT_ATTEMPTS {
                match probe_list(&host, port, &user, &pass, &path, use_tls).await {
                    Ok(text) => {
                        listing = Some(text);
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
            match listing {
                Some(t) => t,
                None => return Err(EngineError::Other(format!("ftp list failed: {last}"))),
            }
        };
        let entries = parse_list_listing(&listing);
        if entries.is_empty() {
            return Err(EngineError::Other("ftp directory is empty".to_string()));
        }

        // 落位目录：dest_root/<目录名>（URL 最后一段非空名称；根目录 → host）
        // 安全修复（V3）：目录名可能来自远端 LIST 响应，join 前净化。
        let dir_name = dir_name_of(&path, &host);
        let dir_pb = smart_dl_core::session::output::sanitize_rel(&dir_name)
            .map_err(|e| EngineError::Other(e.to_string()))?;
        let dest_dir = task.dest_root.join(&dir_pb);
        std::fs::create_dir_all(&dest_dir)
            .map_err(|e| EngineError::Other(format!("mkdir {}: {e}", dest_dir.display())))?;

        // 文件条目：远端路径 + .part 续传起点（超长作废）
        let dir_prefix = path.trim_end_matches('/').to_string();
        let mut files = Vec::new();
        let mut total = 0u64;
        let mut done = 0u64;
        for e in entries {
            let fpath = format!("{}/{}", dir_prefix, e.name);
            let dest = dest_dir.join(&e.name);
            let part = part_path_of(&dest);
            if let Ok(md) = std::fs::metadata(&part) {
                if md.len() > e.size {
                    let _ = std::fs::remove_file(&part);
                }
            }
            let d = part_done(&part);
            total += e.size;
            done += d;
            files.push(FtpFile {
                name: e.name,
                path: fpath,
                size: e.size,
                done: d,
                state: EngineState::MetadataPending,
            });
        }

        let tid = task.id.clone();
        {
            let mut tasks = self.inner.tasks.lock();
            tasks.insert(
                tid.clone(),
                FtpTask {
                    host,
                    port,
                    user,
                    pass,
                    path,
                    use_tls,
                    dest: dest_dir,
                    total,
                    state: EngineState::Downloading,
                    done,
                    rate: RateSample::default(),
                    error: None,
                    files,
                    limit_kb_s: None,
                    sequential: task.sequential,
                },
            );
        }
        let inner = self.inner.clone();
        let backoff = self.backoff;
        let spawn_tid = tid.clone();
        spawn_ftp_loop(download_dir_loop, inner, spawn_tid, backoff);
        Ok(tid)
    }
}

impl Default for FtpEngine {
    fn default() -> Self {
        FtpEngine::new()
    }
}

/// 可靠性修复（V11，报告第二轮）：spawn 下载循环 + panic 收尸监控——
/// 修复前 JoinHandle 直接丢弃，循环 panic 会静默变僵尸（任务状态永停
/// Downloading、无 Failed 事件、引擎名额永不释放）；现在监控任务捕获
/// panic → 任务标 Error（状态可见、上游轮询可正常推进）。
fn spawn_ftp_loop<F, Fut>(
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
                let msg = format!("FTP 下载循环 panic（V11 收尸）: {e}");
                tracing::error!("[V11] tid={tid}: {msg}");
                // V11 锁治理后 parking_lot 无中毒——无条件锁，收尸保证执行
                let mut tasks = inner.tasks.lock();
                if let Some(t) = tasks.get_mut(&tid) {
                    t.state = EngineState::Error;
                    t.error = Some(msg);
                }
            }
        }
    });
}

/// 解析 `ftp(s)://[user:pass@]host[:port]/path`。
/// 目录（空路径或以 `/` 结尾）→ `FtpTarget::Dir`；否则 `FtpTarget::File`。
/// 返回 `(host, port, target, use_tls)`：ftps:// = 显式 AUTH TLS（默认端口
/// 仍 21，与 FileZilla/wget 惯例一致；隐式 990 后续按需）。
fn parse_ftp_url(url: &str) -> Option<(String, u16, FtpTarget, bool)> {
    let rest = url
        .strip_prefix("ftps://")
        .map(|r| (r, true))
        .or_else(|| url.strip_prefix("ftp://").map(|r| (r, false)))?;
    let (rest, use_tls) = rest;
    // 无 `/` → 空路径（根目录）
    let (auth_host, raw_path) = match rest.split_once('/') {
        Some((ah, p)) => (ah, p),
        None => (rest, ""),
    };
    let host_port = auth_host
        .rsplit_once('@')
        .map(|(_, hp)| hp)
        .unwrap_or(auth_host);
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()?),
        None => (host_port.to_string(), 21),
    };
    if host.is_empty() {
        return None;
    }
    let path = format!("/{raw_path}");
    let target = if path.ends_with('/') {
        FtpTarget::Dir(path)
    } else {
        FtpTarget::File(path)
    };
    Some((host, port, target, use_tls))
}

/// 目录条目（LIST 解析结果：普通文件）。
#[derive(Debug, PartialEq, Eq)]
struct DirEntry {
    name: String,
    size: u64,
}

/// 解析 UNIX `ls -l` 风格 LIST 响应 → 普通文件列表（独立纯函数，便于单测）。
/// 容错：`total N` 头、空行、多空格分隔、字段不足/大小非数字的异常行；
/// 过滤子目录（权限位 `d`）与符号链接等非普通文件行、`.`/`..` 及含路径分隔符的名字（防穿越）。
fn parse_list_listing(text: &str) -> Vec<DirEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        // `total N` 头（部分服务器输出）
        if fields.first() == Some(&"total") {
            continue;
        }
        // 标准 9 字段：perm links owner group size month day time name...
        if fields.len() < 9 {
            continue;
        }
        // 只收普通文件（`-` 开头）；目录 `d`/链接 `l`/设备等跳过
        if !fields[0].starts_with('-') {
            continue;
        }
        let size = match fields[4].parse::<u64>() {
            Ok(s) => s,
            Err(_) => continue,
        };
        // 文件名 = 第 9 字段起（名字可含空格）
        let name = fields[8..].join(" ");
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\\')
        {
            continue;
        }
        out.push(DirEntry { name, size });
    }
    out
}

/// 落盘名净化：Windows 非法字符替换为 `_`（防穿越/防盘符语义）。
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}

/// 目录落位名：URL 路径最后一段非空名称；根目录（无名称）→ host。
fn dir_name_of(dir_path: &str, host: &str) -> String {
    let last = dir_path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    let name = sanitize_name(last);
    if name.is_empty() || name == "." || name == ".." {
        sanitize_name(host)
    } else {
        name
    }
}

/// 控制连接会话（单命令/响应流；B2：明文或 AUTH TLS 升级后同一口径）。
struct FtpSession {
    reader: BufReader<BoxFtpIo>,
}

impl FtpSession {
    /// 建立控制连接。`use_tls` = 显式 FTPS（RFC 4217）：明文 banner →
    /// AUTH TLS（234）→ TLS 握手（服务器证书严格校验）→ PBSZ 0 → PROT P
    /// （后续控制流与全部数据连接均加密）。
    async fn connect(host: &str, port: u16, use_tls: bool) -> Result<Self, String> {
        let tcp = TcpStream::connect((host, port))
            .await
            .map_err(|e| e.to_string())?;
        // 升级前以裸 TcpStream 交互（banner + AUTH TLS 响应），升级后再装箱
        let mut reader = BufReader::new(tcp);
        let banner = read_response(&mut reader).await?;
        if banner.starts_with("421") {
            return Err(format!("421 {banner}"));
        }
        if use_tls {
            let resp = write_cmd(&mut reader, "AUTH TLS").await?;
            if !resp.starts_with("234") {
                return Err(format!("AUTH TLS 被拒绝: {resp}"));
            }
            let tls = ftps_upgrade(host, reader.into_inner()).await?;
            let mut reader = BufReader::new(Box::new(tls) as BoxFtpIo);
            // RFC 4217：PBSZ 必须先于 PROT；PBSZ 0 = 流式传输无缓冲协商
            let pbsz = write_cmd(&mut reader, "PBSZ 0").await?;
            if !pbsz.starts_with("200") {
                return Err(format!("PBSZ 被拒绝: {pbsz}"));
            }
            let prot = write_cmd(&mut reader, "PROT P").await?;
            if !prot.starts_with("200") {
                return Err(format!("PROT P 被拒绝（仅支持私有数据连接）: {prot}"));
            }
            Ok(FtpSession { reader })
        } else {
            Ok(FtpSession {
                reader: BufReader::new(Box::new(reader.into_inner()) as BoxFtpIo),
            })
        }
    }

    async fn login(&mut self, user: &str, pass: &str) -> Result<(), String> {
        self.cmd(&format!("USER {user}")).await?;
        self.cmd(&format!("PASS {pass}")).await?;
        self.cmd("TYPE I").await?;
        Ok(())
    }

    /// PASV → 数据连接地址。
    async fn pasv(&mut self) -> Result<SocketAddr, String> {
        let resp = self.cmd("PASV").await?;
        parse_pasv(&resp)
    }

    async fn size(&mut self, path: &str) -> Result<u64, String> {
        let resp = self.cmd(&format!("SIZE {path}")).await?;
        resp.strip_prefix("213 ")
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| format!("SIZE failed: {resp}"))
    }

    /// 发命令并读单行响应。
    async fn cmd(&mut self, line: &str) -> Result<String, String> {
        let mut buf = line.to_string();
        buf.push_str("\r\n");
        self.reader
            .get_mut()
            .write_all(buf.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        read_response(&mut self.reader).await
    }

    async fn quit(&mut self) {
        let _ = self.cmd("QUIT").await;
    }
}

async fn read_response<T: AsyncRead + Unpin>(reader: &mut BufReader<T>) -> Result<String, String> {
    let mut line = String::new();
    let n = reader
        .read_line(&mut line)
        .await
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("connection closed by server".to_string());
    }
    Ok(line.trim_end().to_string())
}

/// 发送命令行并读单行响应（AUTH TLS 升级前后共用——升级需在 session
/// 成型前以裸 TcpStream 交互，故与 FtpSession::cmd 分立）。
async fn write_cmd<T: AsyncRead + AsyncWrite + Unpin>(
    reader: &mut BufReader<T>,
    line: &str,
) -> Result<String, String> {
    let mut buf = line.to_string();
    buf.push_str("\r\n");
    reader
        .get_mut()
        .write_all(buf.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    read_response(reader).await
}

/// 解析 `227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)`。
fn parse_pasv(resp: &str) -> Result<SocketAddr, String> {
    let nums: Vec<u32> = resp
        .split('(')
        .nth(1)
        .and_then(|s| s.split(')').next())
        .map(|s| s.split(',').filter_map(|n| n.trim().parse().ok()).collect())
        .ok_or_else(|| format!("bad PASV: {resp}"))?;
    if nums.len() != 6 {
        return Err(format!("bad PASV tuple: {resp}"));
    }
    Ok(SocketAddr::from((
        [nums[0] as u8, nums[1] as u8, nums[2] as u8, nums[3] as u8],
        (nums[4] * 256 + nums[5]) as u16,
    )))
}

/// 下载一个段（独立连接：连接+登录+PASV+REST+RETR），写入 .part 段位置。
#[allow(clippy::too_many_arguments)] // 参数即协议会话要素（E16 增 limiter），拆 struct 反而模糊
async fn download_segment(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    path: &str,
    seg: DynSegment,
    part: &Path,
    limiter: &RateLimiter,
    use_tls: bool,
) -> Result<(), String> {
    let mut s = FtpSession::connect(host, port, use_tls).await?;
    s.login(user, pass).await?;
    let data_addr = s.pasv().await?;
    s.cmd(&format!("REST {}", seg.start)).await?;
    // RETR 必须是 1xx 中间响应（150）；550 等错误直接终态失败
    let retr = s.cmd(&format!("RETR {path}")).await?;
    if !retr.starts_with('1') {
        return Err(retr);
    }
    let data_tcp = TcpStream::connect(data_addr)
        .await
        .map_err(|e| e.to_string())?;
    // PROT P：数据连接全程 TLS（B2）；PROT C（明文数据）不支持——connect
    // 阶段 PROT P 被拒即整体失败，此处无需分支。
    let mut data: BoxFtpIo = if use_tls {
        Box::new(ftps_upgrade(host, data_tcp).await?)
    } else {
        Box::new(data_tcp)
    };
    // 流式直写 .part 段位置（与 HTTP 侧流式语义对齐；段不相交 → 无锁）。
    // 固定 64KB 块缓冲，段长不再驻内存；失败时部分写入由重试/恢复路径
    // seek 回 seg.start 全量重写（段未入账本前不构成有效凭据）。
    use std::io::{Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(part)
        .map_err(|e| e.to_string())?;
    f.seek(SeekFrom::Start(seg.start))
        .map_err(|e| e.to_string())?;
    let need = seg.len() as usize;
    let mut chunk = vec![0u8; FTP_CHUNK];
    let mut got = 0usize;
    while got < need {
        let n = data.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err(format!("data connection closed early: {got}/{need}"));
        }
        // E16 全局限速：逐块消费全局预算（限速器内部计数，速率 0 早退零开销）
        limiter.wait(n as u64).await;
        f.write_all(&chunk[..n]).map_err(|e| e.to_string())?;
        got += n;
    }
    // 关键：读满子段配额后立即断开数据连接。RETR 无结束偏移语义，服务器从
    // REST 偏移一直发到 EOF——非末段场景客户端停止读取后，服务器 write_all
    // 会因流控阻塞（过量发送远超内核缓冲 ~1MB），226 永远不到达，双边死锁。
    // 主动断开 → 服务器 EPIPE 收尾（真实服务器回 426/226 或直接关闭，均
    // 无关紧要：数据完整性由 got==need 校验与账本语义保证）。
    drop(data);
    let _ = read_response(&mut s.reader).await; // 226（或 426/关闭，忽略）
    s.quit().await;
    Ok(())
}

/// 段下载 + 失败缩小粒度重试（P1，与 HTTP download.rs 同构）：失败段可拆
/// （len/2 >= MIN_RETRY_GRANULARITY，两侧共用同一常量口径）则二分重试栈继续，
/// 否则上抛；终态错误（5xx，协议级永久失败）不拆直接上抛。子段区间恒落在
/// 原段内 → 部分写入由后续成功尝试全量重写；账本仍按原段边界记账
/// （拆分仅是重试内部细节，不影响进度真源）。迭代式拆分栈（避免 async 递归装箱）。
// 参数即协议会话要素（主机/凭据/路径/段/目标/限速/退避），拆 struct 反而模糊调用点语义。
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
    use_tls: bool,
) -> Result<(), String> {
    let mut stack: Vec<DynSegment> = vec![seg];
    while let Some(cur) = stack.pop() {
        match download_segment_attempts(
            host, port, user, pass, path, cur, part, limiter, backoff, use_tls,
        )
        .await
        {
            Ok(()) => {}
            Err(e) if is_terminal(&e) => {
                return Err(format!("segment [{}, {}]: {e}", cur.start, cur.end))
            }
            Err(_) if cur.len() / 2 >= MIN_RETRY_GRANULARITY => {
                let mid = cur.start + cur.len() / 2;
                // 先压 right 再压 left → 先处理 left，与 HTTP 侧拆分顺序一致
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

/// 单个子段的连接层退避重试（421/IO；5xx 终态直接失败）——原 worker 内联
/// 逻辑提为函数，供二分重试栈逐子段调用（每次栈内尝试都保有完整退避预算，
/// 最坏尝试次数 = 拆分深度 × CONNECT_ATTEMPTS，有界）。
// 参数即协议会话要素（主机/凭据/路径/段/目标/限速/退避），拆 struct 反而模糊调用点语义。
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
    use_tls: bool,
) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 1..=CONNECT_ATTEMPTS {
        match download_segment(host, port, user, pass, path, seg, part, limiter, use_tls).await {
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

/// 单文件下载核心（单文件/目录任务共用）：动态分段 + worker 池并行 + 账本续传。
/// 分段策略与 HTTP 直链对齐（P0 方案A + P4 账本统一进度真源）：
/// - 段粒度 `min_split`（0 = 默认 16MB）FIFO 队列（<16MB 单段）；
/// - 并行 worker 数 = `segment_count(total)`（与 HTTP 同一公式，2-8）；
/// - 续传：`<part>.progress` 段账本为唯一凭据（缺失/损坏/失配 → 作废重下），
///   每段完成原子落盘，finalize 后清理；旧 .part 长度前缀续传语义废弃（G1/G2）。
// 参数即协议会话要素（主机/凭据/路径/目标/退避/进度回调），拆 struct 反而模糊调用点语义。
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
    use_tls: bool,
) -> Result<(), String> {
    let part = part_path_of(dest);
    let ledger_path = ledger::ledger_path(&part);
    // 段账本加载（P4 唯一进度真源，与 HTTP engine.rs 同口径）：合法账本 →
    // 恢复已完成段并沿用其粒度；缺失/损坏/total 失配 → 全新计划 + .part 作废。
    // 旧「.part 长度前缀续传」语义废弃（G1/G2：预分配后长度恒为 total，不可信）。
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

    // 顺序模式在飞闸门（与 HTTP download_dynamic 同构）：permit 从领取前
    // 持有到 complete 后释放（RAII），失败/panic 退出路径同样随作用域释放，
    // 无泄漏。在飞段数 ≤ SEQUENTIAL_WINDOW → 前缀尽快完整（边下边播）。
    let seq_gate: Option<Arc<tokio::sync::Semaphore>> = if sequential {
        Some(Arc::new(tokio::sync::Semaphore::new(SEQUENTIAL_WINDOW)))
    } else {
        None
    };

    // worker 数：与 HTTP 同一公式（静态 2-8）。<16MB 单段时多出的 worker
    // 领不到段（Drained）即退，零开销。
    let n_workers = segment_count(total);
    let mut workers = JoinSet::new();
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
        workers.spawn(async move {
            loop {
                // 顺序模式：先拿 permit 再领取段，保证「在飞段数 ≤ 窗口」
                //（先领后等会导致窗口外表内的段已占用 FIFO 游标）。
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
                // 失败缩小粒度重试（P1，与 HTTP download.rs 同构）+ 连接层退避：
                // 二分重试栈收敛瞬时故障，5xx 终态直接上抛
                download_segment_with_retry(
                    &host, port, &user, &pass, &path, seg, &part, &limiter, &backoff, use_tls,
                )
                .await?;
                // 段完成：记账 + 账本原子落盘 + 进度回报（锁内一并，
                // 保证账本视图与计数一致——与 HTTP download.rs 同模式）
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
    if let Some(e) = first_err {
        return Err(e);
    }

    // 全部段完成 → 清续传凭据 + 落位
    let _ = std::fs::remove_file(&ledger_path);
    finalize_part(&part, dest, total)
}

/// 单文件任务的下载循环：download_file 包装 + 任务状态落定。
async fn download_loop(inner: Arc<EngineInner>, tid: EngineTaskId, backoff: Backoff) {
    let (host, port, user, pass, path, dest, total, use_tls) = {
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
            t.use_tls,
        )
    };
    let inner2 = inner.clone();
    let tid2 = tid.clone();
    // 任务级限速优先，未登记回退全局（与 HTTP engine 同口径）；登记条目
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
    let progress: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |n| {
        let mut tasks = inner2.tasks.lock();
        if let Some(t) = tasks.get_mut(&tid2) {
            t.done += n;
        }
    });
    let r = download_file(
        &host, port, &user, &pass, &path, &dest, total, backoff, &limiter, min_split, sequential,
        progress, use_tls,
    )
    .await;
    match r {
        Ok(()) => finish(&inner, &tid, EngineState::Completed, None),
        Err(e) => finish(&inner, &tid, EngineState::Error, Some(e)),
    }
}

/// 目录任务下载循环：逐文件串行 download_file，落位 `<dest>/<文件名>`；
/// 任一文件终态失败 → 整任务 Error（错误消息带文件名）。
async fn download_dir_loop(inner: Arc<EngineInner>, tid: EngineTaskId, backoff: Backoff) {
    let (host, port, user, pass, dir_dest, files, use_tls) = {
        let tasks = inner.tasks.lock();
        let t = tasks.get(&tid).unwrap();
        (
            t.host.clone(),
            t.port,
            t.user.clone(),
            t.pass.clone(),
            t.dest.clone(),
            t.files
                .iter()
                .map(|f| (f.name.clone(), f.path.clone(), f.size))
                .collect::<Vec<_>>(),
            t.use_tls,
        )
    };
    // add 时已建目录；此处幂等兜底（目录被外部删除的场景）
    if let Err(e) = std::fs::create_dir_all(&dir_dest) {
        finish(
            &inner,
            &tid,
            EngineState::Error,
            Some(format!("mkdir: {e}")),
        );
        return;
    }
    // 目录任务按文件串行（既有语义），sequential 语义 = 文件内段在飞窗口收紧
    let sequential = {
        let tasks = inner.tasks.lock();
        tasks.get(&tid).map(|t| t.sequential).unwrap_or(false)
    };
    for (name, fpath, size) in files {
        let dest = dir_dest.join(&name);
        set_file_state(&inner, &tid, &name, EngineState::Downloading);
        let inner2 = inner.clone();
        let tid2 = tid.clone();
        let name2 = name.clone();
        // 任务级限速优先，未登记回退全局（与单文件循环同口径）
        let limiter = inner
            .limiters
            .lock()
            .get(&tid)
            .cloned()
            .unwrap_or_else(|| inner.limiter.clone());
        let min_split = inner.min_split;
        let progress: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |n| {
            let mut tasks = inner2.tasks.lock();
            if let Some(t) = tasks.get_mut(&tid2) {
                t.done += n;
                if let Some(f) = t.files.iter_mut().find(|f| f.name == name2) {
                    f.done += n;
                }
            }
        });
        let r = download_file(
            &host, port, &user, &pass, &fpath, &dest, size, backoff, &limiter, min_split,
            sequential, progress, use_tls,
        )
        .await;
        match r {
            Ok(()) => set_file_state(&inner, &tid, &name, EngineState::Completed),
            Err(e) => {
                set_file_state(&inner, &tid, &name, EngineState::Error);
                finish(
                    &inner,
                    &tid,
                    EngineState::Error,
                    Some(format!("{name}: {e}")),
                );
                return;
            }
        }
    }
    finish(&inner, &tid, EngineState::Completed, None);
}

/// 更新目录任务中某文件的状态。
fn set_file_state(inner: &Arc<EngineInner>, tid: &str, name: &str, state: EngineState) {
    let mut tasks = inner.tasks.lock();
    if let Some(t) = tasks.get_mut(tid) {
        if let Some(f) = t.files.iter_mut().find(|f| f.name == name) {
            f.state = state;
        }
    }
}

/// 421/连接类错误可重试；550/协议终态不重试。
fn is_terminal(e: &str) -> bool {
    e.starts_with("550") || e.starts_with("5")
}

fn part_path_of(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

fn finalize_part(part: &Path, dest: &Path, total: u64) -> Result<(), String> {
    let om = OutputManager::new(PathBuf::from("."));
    om.finalize_to(part, dest, total).map_err(|e| e.to_string())
}

fn finish(inner: &Arc<EngineInner>, tid: &str, state: EngineState, error: Option<String>) {
    let mut tasks = inner.tasks.lock();
    if let Some(t) = tasks.get_mut(tid) {
        t.state = state;
        t.error = error;
        if state == EngineState::Completed {
            t.done = t.total;
            // 目录任务：逐文件推进到完成态
            for f in t.files.iter_mut() {
                f.done = f.size;
                f.state = EngineState::Completed;
            }
        }
    }
}

#[async_trait::async_trait]
impl DownloadEngine for FtpEngine {
    fn id(&self) -> &str {
        "ftp"
    }

    fn kind(&self) -> EngineKind {
        EngineKind::Ftp
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Ftp, Capability::FtpResume]
    }

    /// 引擎全局限速热改（E16 trait 扩展）：热调引擎全局 limiter，所有 FTP
    /// 任务合计带宽立即按新速率节流。仅 down 方向：up 请求被显式拒绝。
    async fn set_global_limits(
        &self,
        down_kb_s: Option<u32>,
        up_kb_s: Option<u32>,
    ) -> Result<(), EngineError> {
        if up_kb_s.is_some() {
            return Err(EngineError::Other(
                "HTTP/FTP 引擎无上传方向，up_kb_s 不适用".to_string(),
            ));
        }
        if let Some(kb) = down_kb_s {
            self.inner.limiter.set_rate_kb_s(kb);
        }
        Ok(())
    }

    /// 任务级下载限速（trait 扩展，与 HTTP 引擎同口径）：任务专属 limiter
    /// 登记进 limiters 表（串联全局上游，总阀门对任务级限速任务同样生效）；
    /// 已登记 → 原地热调（运行中任务经 Arc 共享即时生效）；未登记（含
    /// 运行中走全局的任务）→ 新登记，下一次重下轮拾取。仅 down 方向。
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

    /// 任务级顺序下载开关（trait 扩展）：字段改写，下一次重下轮拾取；
    /// 运行中的当前轮不变（收尾在飞段）。新建任务在 add() 直接读
    /// task.sequential → 立即生效（FTP 单轮下载，无 resume 重入路径）。
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
        let (url, user, pass) = match &task.source {
            DownloadSource::Ftp { url, user, pass } => (url.clone(), user.clone(), pass.clone()),
            _ => return Err(EngineError::Other("source is not ftp".to_string())),
        };
        let (host, port, target, use_tls) =
            parse_ftp_url(&url).ok_or_else(|| EngineError::Other("invalid ftp url".to_string()))?;

        match target {
            // 目录分支：LIST 探测 → 逐文件条目 → 目录下载循环
            FtpTarget::Dir(path) => {
                self.add_directory(task, host, port, user, pass, path, use_tls)
                    .await
            }
            FtpTarget::File(path) => {
                // 探测：连接 + 登录 + SIZE（421 → 退避重试）
                let total = {
                    let mut last = String::new();
                    let mut size: Option<u64> = None;
                    for attempt in 1..=CONNECT_ATTEMPTS {
                        match probe_size(&host, port, &user, &pass, &path, use_tls).await {
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
                        None => {
                            return Err(EngineError::Other(format!("ftp probe failed: {last}")))
                        }
                    }
                };

                let rel = task
                    .metadata
                    .name
                    .clone()
                    .unwrap_or_else(|| "download.bin".to_string());
                // 安全修复（V3）：任务名净化后再 join（拒 .. / 绝对路径）。
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
                        FtpTask {
                            host,
                            port,
                            user,
                            pass,
                            path,
                            use_tls,
                            dest,
                            total,
                            state: EngineState::Downloading,
                            done: part_done(&part),
                            rate: RateSample::default(),
                            error: None,
                            files: vec![],
                            limit_kb_s: None,
                            sequential: task.sequential,
                        },
                    );
                }
                let inner = self.inner.clone();
                let backoff = self.backoff;
                let spawn_tid = tid.clone();
                spawn_ftp_loop(download_loop, inner, spawn_tid, backoff);
                Ok(tid)
            }
        }
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
        let mut tasks = self.inner.tasks.lock();
        let t = tasks.get_mut(id).ok_or(EngineError::NotFound)?;
        // 目录任务 → 文件级进度（FileProgress）；单文件任务 → 空（保持既有行为）
        let files = t
            .files
            .iter()
            .map(|f| FileProgress {
                rel_path: f.name.clone(),
                done: f.done,
                size: f.size,
            })
            .collect();
        let down_rate = t.rate.sample(t.done);
        Ok(EngineStatus {
            state: t.state,
            metadata_received: true,
            files,
            total_done: t.done,
            total: t.total,
            down_rate,
            up_rate: 0,
            // E33：FTP 单向引擎无累计统计口径，恒 0（快照序列化省略）
            total_downloaded: 0,
            total_uploaded: 0,
            num_peers: 0,
            num_seeds: 0,
            error: t.error.clone(),
            // FTP 不参与 E9 名字回填：daemon add 时已派生 URL 末段名
            name: None,
        })
    }

    async fn remove(&self, id: &EngineTaskId, _delete_data: bool) -> Result<(), EngineError> {
        let mut tasks = self.inner.tasks.lock();
        tasks.remove(id).ok_or(EngineError::NotFound)?;
        // 任务级限速登记一并回收（防表无限增长；与 HTTP engine 同口径）
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

    async fn ban_peer(&self, _id: &EngineTaskId, _peer: SocketAddr) -> Result<(), EngineError> {
        Ok(())
    }

    async fn read_piece(&self, _id: &EngineTaskId, _idx: u32) -> Result<Vec<u8>, EngineError> {
        Err(EngineError::Unsupported)
    }
}

/// 探测文件大小：连接 + 登录 + SIZE。
async fn probe_size(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    path: &str,
    use_tls: bool,
) -> Result<u64, String> {
    let mut s = FtpSession::connect(host, port, use_tls).await?;
    s.login(user, pass).await?;
    let size = s.size(path).await;
    s.quit().await;
    size
}

/// 现有 .part 已下载字节数（续传起点）。
fn part_done(part: &Path) -> u64 {
    std::fs::metadata(part).map(|m| m.len()).unwrap_or(0)
}

/// 探测目录列表：连接 + 登录 + PASV + LIST → 目录文本（数据连接读到 EOF）。
async fn probe_list(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    path: &str,
    use_tls: bool,
) -> Result<String, String> {
    let mut s = FtpSession::connect(host, port, use_tls).await?;
    s.login(user, pass).await?;
    let data_addr = s.pasv().await?;
    // LIST 必须是 1xx 中间响应（150/125）；550 等错误直接终态失败
    let resp = s.cmd(&format!("LIST {path}")).await?;
    if !resp.starts_with('1') {
        return Err(resp);
    }
    let data_tcp = TcpStream::connect(data_addr)
        .await
        .map_err(|e| e.to_string())?;
    let mut data: BoxFtpIo = if use_tls {
        Box::new(ftps_upgrade(host, data_tcp).await?)
    } else {
        Box::new(data_tcp)
    };
    let mut bytes = Vec::new();
    data.read_to_end(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    let _ = read_response(&mut s.reader).await; // 226
    s.quit().await;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FTPS e2e（B2）：内嵌显式 FTPS 服务器（自签证书 + tokio-rustls），
    /// 走 download_segment 全路径——控制连接 AUTH TLS 升级 + PBSZ/PROT P +
    /// 数据连接 TLS + REST/RETR 断点偏移，落盘内容逐字节断言。
    ///
    /// 服务器协议循环（RFC 4217/959 最小子集）：banner 220 → AUTH TLS 234
    /// → [TLS] PBSZ 200 / PROT 200 / USER 331 / PASS 230 / TYPE 200 /
    /// PASV 227（同步起数据监听）→ REST 350 / RETR 150 → 数据连接 TLS
    /// accept 后发送内容 → 226 → QUIT 221。
    #[tokio::test]
    async fn ftps_end_to_end_download_via_download_segment() {
        // —— 自签证书（SAN 含 127.0.0.1，SNI/校验均走 IP）——
        let ck = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let cert_der =
            tokio_rustls::rustls::pki_types::CertificateDer::from(ck.cert.der().to_vec());
        let key_der =
            tokio_rustls::rustls::pki_types::PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der());
        // 客户端 connector：直接信任自签证书（注入 TEST_CONNECTOR）
        let mut roots = tokio_rustls::rustls::RootCertStore::empty();
        roots.add(cert_der.clone()).unwrap();
        let client_config = tokio_rustls::rustls::ClientConfig::builder_with_provider(
            tokio_rustls::rustls::crypto::ring::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        TEST_CONNECTOR
            .set(tokio_rustls::TlsConnector::from(Arc::new(client_config)))
            .ok();

        // —— server TLS（rustls 同一构建单元；builder 显式 ring provider）——
        let server_config = tokio_rustls::rustls::ServerConfig::builder_with_provider(
            tokio_rustls::rustls::crypto::ring::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der.into())
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

        let content: Arc<Vec<u8>> = Arc::new((0..1024u32).map(|i| (i % 251) as u8).collect());

        // 数据监听先行（PASV 响应需要端口）
        let data_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let data_port = data_listener.local_addr().unwrap().port();
        let ctrl_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ctrl_port = ctrl_listener.local_addr().unwrap().port();

        let content2 = content.clone();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
            let (sock, _) = ctrl_listener.accept().await.unwrap();
            let mut reader = BufReader::new(sock);
            let mut line = String::new();
            // FTP 服务器先发 banner，客户端 connect 才能读到
            reader
                .get_mut()
                .write_all(b"220 ftps-e2e ready\r\n")
                .await
                .unwrap();
            loop {
                line.clear();
                reader.read_line(&mut line).await.unwrap();
                let cmd = line.trim_end().to_string();
                match cmd.as_str() {
                    "AUTH TLS" => {
                        reader
                            .get_mut()
                            .write_all(b"234 proceeding\r\n")
                            .await
                            .unwrap();
                        let tls = acceptor.accept(reader.into_inner()).await.unwrap();
                        let mut reader = BufReader::new(tls);
                        // 加密段协议循环
                        loop {
                            line.clear();
                            reader.read_line(&mut line).await.unwrap();
                            let cmd = line.trim_end().to_string();
                            match cmd.as_str() {
                                "PBSZ 0" => {
                                    reader.get_mut().write_all(b"200 ok\r\n").await.unwrap()
                                }
                                "PROT P" => {
                                    reader.get_mut().write_all(b"200 ok\r\n").await.unwrap()
                                }
                                "USER anonymous" => reader
                                    .get_mut()
                                    .write_all(b"331 need pass\r\n")
                                    .await
                                    .unwrap(),
                                // 客户端 "PASS " + 空密码，trim_end 后为 "PASS"
                                "PASS" | "PASS " => reader
                                    .get_mut()
                                    .write_all(b"230 logged in\r\n")
                                    .await
                                    .unwrap(),
                                "TYPE I" => {
                                    reader.get_mut().write_all(b"200 ok\r\n").await.unwrap()
                                }
                                "PASV" => {
                                    let p1 = data_port / 256;
                                    let p2 = data_port % 256;
                                    reader
                                        .get_mut()
                                        .write_all(
                                            format!(
                                                "227 Entering Passive Mode (127,0,0,1,{p1},{p2})\r\n"
                                            )
                                            .as_bytes(),
                                        )
                                        .await
                                        .unwrap();
                                }
                                "REST 0" => reader
                                    .get_mut()
                                    .write_all(b"350 restart ok\r\n")
                                    .await
                                    .unwrap(),
                                "RETR /f.bin" => {
                                    reader
                                        .get_mut()
                                        .write_all(b"150 opening data\r\n")
                                        .await
                                        .unwrap();
                                    // 数据连接：客户端连上后立即 TLS（PROT P）
                                    let (dsock, _) = data_listener.accept().await.unwrap();
                                    let mut dtls = acceptor.accept(dsock).await.unwrap();
                                    dtls.write_all(&content2).await.unwrap();
                                    dtls.shutdown().await.unwrap();
                                    reader
                                        .get_mut()
                                        .write_all(b"226 transfer done\r\n")
                                        .await
                                        .unwrap();
                                }
                                "QUIT" => {
                                    reader.get_mut().write_all(b"221 bye\r\n").await.unwrap();
                                    return;
                                }
                                _ => reader
                                    .get_mut()
                                    .write_all(b"502 not impl\r\n")
                                    .await
                                    .unwrap(),
                            }
                        }
                    }
                    _ => reader
                        .get_mut()
                        .write_all(b"502 need AUTH TLS\r\n")
                        .await
                        .unwrap(),
                }
            }
        });

        // —— 客户端全路径：connect(true) → login → pasv/REST/RETR → 数据 TLS ——
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("f.bin.part");
        let seg = DynSegment {
            start: 0,
            end: content.len() as u64 - 1,
        };
        download_segment(
            "127.0.0.1",
            ctrl_port,
            "anonymous",
            "",
            "/f.bin",
            seg,
            &part,
            &RateLimiter::new(0),
            true,
        )
        .await
        .unwrap();
        let got = std::fs::read(&part).unwrap();
        assert_eq!(got, *content, "FTPS 数据连接内容逐字节一致（TLS 解密正确）");
        server.abort();
    }

    /// LIST 解析：两个文件行 + `total` 头 + 子目录被过滤 + 多空格分隔。
    #[test]
    fn parse_list_listing_two_files_and_subdir_filtered() {
        let text = "total 8\r\n\
                    -rw-r--r--  1 owner  group      1024 Jan 01 12:00 a.bin\r\n\
                    drwxr-xr-x  2 owner  group      4096 Jan 01 12:00 subdir\r\n\
                    -rw-r--r--  1 owner    group     4096 Jan 01 12:00 b.bin\r\n";
        assert_eq!(
            parse_list_listing(text),
            vec![
                DirEntry {
                    name: "a.bin".to_string(),
                    size: 1024
                },
                DirEntry {
                    name: "b.bin".to_string(),
                    size: 4096
                },
            ]
        );
    }

    /// LIST 解析容错：空行/字段不足/大小非数字/符号链接被跳过；含空格文件名保留。
    #[test]
    fn parse_list_listing_tolerates_malformed_lines() {
        let text = "\r\n\
                    total 12\r\n\
                    garbage line\r\n\
                    -rw-r--r-- 1 owner group abc Jan 01 12:00 badsize.bin\r\n\
                    lrwxrwxrwx 1 owner group 5 Jan 01 12:00 link.bin\r\n\
                    -rw-r--r-- 1 owner group 7 Jan 01 12:00 with space.bin\r\n\
                    -rw-r--r-- 1 owner group 3 Jan 01 12:00 ..\r\n";
        let files = parse_list_listing(text);
        assert_eq!(files.len(), 1, "只应保留 1 个普通文件: {files:?}");
        assert_eq!(files[0].name, "with space.bin");
        assert_eq!(files[0].size, 7);
    }

    /// 目录识别：以 `/` 结尾或空路径 → Dir；否则 File；非法 URL → None。
    #[test]
    fn parse_ftp_url_distinguishes_file_and_dir() {
        let (h, p, t, tls) = parse_ftp_url("ftp://u:p@host:2121/pub/dir/").unwrap();
        assert_eq!((h.as_str(), p), ("host", 2121));
        assert_eq!(t, FtpTarget::Dir("/pub/dir/".to_string()));
        assert!(!tls);

        let (_, _, t, _) = parse_ftp_url("ftp://host/").unwrap();
        assert_eq!(t, FtpTarget::Dir("/".to_string()));

        // 无路径（空路径）→ 根目录
        let (_, _, t, _) = parse_ftp_url("ftp://host").unwrap();
        assert_eq!(t, FtpTarget::Dir("/".to_string()));

        let (_h, _p, _t, tls) = parse_ftp_url("ftp://host/file.bin").unwrap();
        assert!(!tls);
    }

    #[test]
    fn parse_ftp_url_ftps_flag() {
        let (h, p, t, tls) = parse_ftp_url("ftps://u:p@host:990/file.bin").unwrap();
        assert_eq!((h.as_str(), p), ("host", 990));
        assert_eq!(t, FtpTarget::File("/file.bin".to_string()));
        assert!(tls);

        // 默认端口仍 21（显式 AUTH TLS 惯例）
        let (h, p, _, tls) = parse_ftp_url("ftps://host/dir/").unwrap();
        assert_eq!((h.as_str(), p), ("host", 21));
        assert!(tls);
        assert_eq!((h.as_str(), p), ("host", 21));
        assert_eq!(t, FtpTarget::File("/file.bin".to_string()));

        assert!(parse_ftp_url("http://host/x").is_none());
        assert!(parse_ftp_url("ftp://").is_none());
    }

    /// 目录落位名：最后一段非空名称；根目录 → host（含端口净化）。
    #[test]
    fn dir_name_of_last_segment_or_host() {
        assert_eq!(dir_name_of("/pub/files/", "h"), "files");
        assert_eq!(dir_name_of("/", "127.0.0.1:2121"), "127.0.0.1_2121");
        assert_eq!(dir_name_of("/..", "h"), "h");
    }
}
