//! FTP 协议子集（M4c，feature=`ftp`，设计 §15）：
//! 被动模式（PASV）、REST 断点续传（.part）、421 退避重试、目录下载（LIST 单层）；
//! 不支持 SFTP/FTPS 隐式/目录递归/FXP。

use crate::rate::{RateLimiter, RateSample};
use crate::retry::Backoff;
use crate::static_split::plan_segments;
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
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// 421/连接失败重试次数（连接层退避）。
const CONNECT_ATTEMPTS: u32 = 4;

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
}

struct EngineInner {
    tasks: Mutex<HashMap<EngineTaskId, FtpTask>>,
    /// 引擎全局限速器（E16 总阀门）：0 = 不限（wait 早退零开销）；
    /// set_global_limits 运行中热调。FTP 无任务级限速（引擎能力边界），
    /// 所有 FTP 任务的合计带宽由本 limiter 统一约束。
    limiter: Arc<RateLimiter>,
}

/// FTP 引擎（串行段下载：PASV + REST + RETR）。
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
            }),
        }
    }

    /// 目录分支：LIST 探测（421 → 退避重试）→ 解析文件列表 → 建任务 → spawn 目录循环。
    /// 落位 `dest_root/<目录名>/<文件名>`；目录名取 URL 路径最后一段非空名称（根目录 → host）。
    async fn add_directory(
        &self,
        task: &DownloadTask,
        host: String,
        port: u16,
        user: String,
        pass: String,
        path: String,
    ) -> Result<EngineTaskId, EngineError> {
        // LIST 探测：连接 + 登录 + PASV + LIST（421 → 退避重试）
        let listing = {
            let mut last = String::new();
            let mut listing: Option<String> = None;
            for attempt in 1..=CONNECT_ATTEMPTS {
                match probe_list(&host, port, &user, &pass, &path).await {
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
                    dest: dest_dir,
                    total,
                    state: EngineState::Downloading,
                    done,
                    rate: RateSample::default(),
                    error: None,
                    files,
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

/// 解析 `ftp://[user:pass@]host[:port]/path`。
/// 目录（空路径或以 `/` 结尾）→ `FtpTarget::Dir`；否则 `FtpTarget::File`。
fn parse_ftp_url(url: &str) -> Option<(String, u16, FtpTarget)> {
    let rest = url.strip_prefix("ftp://")?;
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
    Some((host, port, target))
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

/// 控制连接会话（单命令/响应流）。
struct FtpSession {
    reader: BufReader<TcpStream>,
}

impl FtpSession {
    async fn connect(host: &str, port: u16) -> Result<Self, String> {
        let stream = TcpStream::connect((host, port))
            .await
            .map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(stream);
        let banner = read_response(&mut reader).await?;
        if banner.starts_with("421") {
            return Err(format!("421 {banner}"));
        }
        Ok(FtpSession { reader })
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

async fn read_response(reader: &mut BufReader<TcpStream>) -> Result<String, String> {
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
    seg: crate::static_split::Segment,
    part: &Path,
    limiter: &RateLimiter,
) -> Result<(), String> {
    let mut s = FtpSession::connect(host, port).await?;
    s.login(user, pass).await?;
    let data_addr = s.pasv().await?;
    s.cmd(&format!("REST {}", seg.start)).await?;
    // RETR 必须是 1xx 中间响应（150）；550 等错误直接终态失败
    let retr = s.cmd(&format!("RETR {path}")).await?;
    if !retr.starts_with('1') {
        return Err(retr);
    }
    let mut data = TcpStream::connect(data_addr)
        .await
        .map_err(|e| e.to_string())?;
    let need = seg.len() as usize;
    let mut buf = vec![0u8; need];
    let mut got = 0usize;
    while got < need {
        let n = data
            .read(&mut buf[got..])
            .await
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err(format!("data connection closed early: {got}/{need}"));
        }
        // E16 全局限速：逐块消费全局预算（限速器内部计数，速率 0 早退零开销）
        limiter.wait(n as u64).await;
        got += n;
    }
    let _ = read_response(&mut s.reader).await; // 226
    s.quit().await;

    // 写 .part 段位置（段不相交 → 无锁）
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(part)
        .map_err(|e| e.to_string())?;
    use std::io::{Seek, SeekFrom, Write};
    f.seek(SeekFrom::Start(seg.start))
        .map_err(|e| e.to_string())?;
    f.write_all(&buf).map_err(|e| e.to_string())?;
    Ok(())
}

/// 单文件下载核心（单文件/目录任务共用）：串行段下载 + 退避重试 + .part 落位。
/// 续传：.part 存在（>0 且 < total）→ 单段 REST 从 part 大小续到文件尾；
/// 无 .part（或已满）→ 正常分块下载。每段完成经 `on_progress(len)` 上报增量。
// 参数即协议会话要素（主机/凭据/路径/目标/退避/进度回调），拆 struct 反而模糊调用点语义。
#[allow(clippy::too_many_arguments)]
async fn download_file<F: Fn(u64)>(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    path: &str,
    dest: &Path,
    total: u64,
    backoff: Backoff,
    limiter: &RateLimiter,
    on_progress: F,
) -> Result<(), String> {
    let part = part_path_of(dest);
    let part_done = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let segments = if part_done > 0 && part_done < total {
        // .part 续传：从偏移续到文件尾（单段）
        vec![crate::static_split::Segment {
            start: part_done,
            end: total - 1,
        }]
    } else {
        plan_segments(total)
    };

    // 预分配 .part
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&part)
        .map_err(|e| format!("part open: {e}"))?
        .set_len(total)
        .map_err(|e| format!("part open: {e}"))?;

    for seg in segments {
        for attempt in 1..=CONNECT_ATTEMPTS {
            match download_segment(host, port, user, pass, path, seg, &part, limiter).await {
                Ok(()) => break,
                Err(e) => {
                    // 连接层失败（421/IO）→ 退避重试；550 等终态 → 直接失败
                    if attempt < CONNECT_ATTEMPTS && !is_terminal(&e) {
                        tokio::time::sleep(backoff.next_delay(attempt)).await;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        // 进度：段完成
        on_progress(seg.len());
    }

    // 全部段完成 → 落位
    finalize_part(&part, dest, total)
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
    let limiter = inner.limiter.clone();
    let r = download_file(
        &host,
        port,
        &user,
        &pass,
        &path,
        &dest,
        total,
        backoff,
        &limiter,
        move |n| {
            let mut tasks = inner2.tasks.lock();
            if let Some(t) = tasks.get_mut(&tid2) {
                t.done += n;
            }
        },
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
    let (host, port, user, pass, dir_dest, files) = {
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
    for (name, fpath, size) in files {
        let dest = dir_dest.join(&name);
        set_file_state(&inner, &tid, &name, EngineState::Downloading);
        let inner2 = inner.clone();
        let tid2 = tid.clone();
        let name2 = name.clone();
        let limiter = inner.limiter.clone();
        let r = download_file(
            &host,
            port,
            &user,
            &pass,
            &fpath,
            &dest,
            size,
            backoff,
            &limiter,
            move |n| {
                let mut tasks = inner2.tasks.lock();
                if let Some(t) = tasks.get_mut(&tid2) {
                    t.done += n;
                    if let Some(f) = t.files.iter_mut().find(|f| f.name == name2) {
                        f.done += n;
                    }
                }
            },
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

    async fn add(&self, task: &DownloadTask) -> Result<EngineTaskId, EngineError> {
        let (url, user, pass) = match &task.source {
            DownloadSource::Ftp { url, user, pass } => (url.clone(), user.clone(), pass.clone()),
            _ => return Err(EngineError::Other("source is not ftp".to_string())),
        };
        let (host, port, target) =
            parse_ftp_url(&url).ok_or_else(|| EngineError::Other("invalid ftp url".to_string()))?;

        match target {
            // 目录分支：LIST 探测 → 逐文件条目 → 目录下载循环
            FtpTarget::Dir(path) => self.add_directory(task, host, port, user, pass, path).await,
            FtpTarget::File(path) => {
                // 探测：连接 + 登录 + SIZE（421 → 退避重试）
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
                            dest,
                            total,
                            state: EngineState::Downloading,
                            done: part_done(&part),
                            rate: RateSample::default(),
                            error: None,
                            files: vec![],
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
) -> Result<u64, String> {
    let mut s = FtpSession::connect(host, port).await?;
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
) -> Result<String, String> {
    let mut s = FtpSession::connect(host, port).await?;
    s.login(user, pass).await?;
    let data_addr = s.pasv().await?;
    // LIST 必须是 1xx 中间响应（150/125）；550 等错误直接终态失败
    let resp = s.cmd(&format!("LIST {path}")).await?;
    if !resp.starts_with('1') {
        return Err(resp);
    }
    let mut data = TcpStream::connect(data_addr)
        .await
        .map_err(|e| e.to_string())?;
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
        let (h, p, t) = parse_ftp_url("ftp://u:p@host:2121/pub/dir/").unwrap();
        assert_eq!((h.as_str(), p), ("host", 2121));
        assert_eq!(t, FtpTarget::Dir("/pub/dir/".to_string()));

        let (_, _, t) = parse_ftp_url("ftp://host/").unwrap();
        assert_eq!(t, FtpTarget::Dir("/".to_string()));

        // 无路径（空路径）→ 根目录
        let (_, _, t) = parse_ftp_url("ftp://host").unwrap();
        assert_eq!(t, FtpTarget::Dir("/".to_string()));

        let (h, p, t) = parse_ftp_url("ftp://host/file.bin").unwrap();
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
