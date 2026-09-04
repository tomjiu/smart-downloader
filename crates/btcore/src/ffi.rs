//! btcore 唯一 unsafe 层：bindgen 绑定 + 低层包装（内存规则 D13：
//! 输出缓冲 Rust 预分配 + cap；`LT_ERR_BUFFER_TOO_SMALL` → 扩容重试；字符串立即拷贝）。
//! 契约对齐 ffi/lt.h（v0.6 全量 ~28 函数）。

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::Path;
use std::ptr;

// bindgen 产物（build.rs 在 ffi/lt.h 变更时重新生成）。
// 无 libclang 平台（如 Linux CI/云环境）：build.rs 回退使用仓库内已提交的
// bindings.rs 的净化版（写入 OUT_DIR，剥离平台相关布局断言），经
// rustc-cfg=lt_bindings_fallback 切换到该副本。
#[cfg(lt_bindings_fallback)]
include!(concat!(env!("OUT_DIR"), "/bindings_fallback.rs"));
#[cfg(not(lt_bindings_fallback))]
include!("../bindings.rs");

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum Error {
    #[error("invalid argument")]
    Arg,
    #[error("engine error")]
    Engine,
    #[error("io error")]
    Io,
    #[error("torrent not found: {0}")]
    NotFound(String),
    #[error("buffer too small")]
    BufferTooSmall,
}

impl From<c_int> for Error {
    fn from(code: c_int) -> Self {
        match code {
            lt_err_LT_ERR_ARG => Error::Arg,
            lt_err_LT_ERR_ENGINE => Error::Engine,
            lt_err_LT_ERR_IO => Error::Io,
            lt_err_LT_ERR_NOT_FOUND => Error::NotFound("".into()),
            lt_err_LT_ERR_BUFFER_TOO_SMALL => Error::BufferTooSmall,
            _ => Error::Engine,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// C++ 侧内核会话（libtorrent session 包装）。Send/Sync 由内核保证
/// （libtorrent session 线程安全；本层以 mutex 保护 ring/maps）。
pub struct Session {
    raw: *mut lt_session,
}

unsafe impl Send for Session {}
unsafe impl Sync for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe { lt_session_free(self.raw) };
    }
}

fn cstr(s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| Error::Arg)
}

fn call<T>(code: c_int, f: impl FnOnce() -> Result<T>) -> Result<T> {
    if code == lt_err_LT_OK {
        f()
    } else {
        Err(Error::from(code))
    }
}

/// 代理类型（对齐 libtorrent settings_pack::proxy_type 1..5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyType {
    Socks4 = 1,
    Socks5 = 2,
    Socks5Auth = 3,
    Http = 4,
    HttpAuth = 5,
}

/// 解析后的代理配置（供 `apply_network`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyCfg {
    pub kind: ProxyType,
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub pass: Option<String>,
}

/// 解析代理 URL：`http://host:port` / `socks5://host:port` / `socks4://host:port`，
/// 可选 `user:pass@`（IPv6 字面量支持 `[::1]:port`）。默认端口 1080。
pub fn parse_proxy(url: &str) -> Result<ProxyCfg> {
    let (scheme, rest) = url.split_once("://").ok_or(Error::Arg)?;
    let kind = match scheme {
        "http" => ProxyType::Http,
        "socks5" => ProxyType::Socks5,
        "socks4" => ProxyType::Socks4,
        _ => return Err(Error::Arg),
    };
    let (auth, hostport) = match rest.rsplit_once('@') {
        Some((a, h)) => {
            let (u, p) = a.split_once(':').unwrap_or((a, ""));
            (Some((u.to_string(), p.to_string())), h)
        }
        None => (None, rest),
    };
    let (host, port) = if let Some(stripped) = hostport.strip_prefix('[') {
        let end = stripped.find(']').ok_or(Error::Arg)?;
        let host = &stripped[..end];
        let after = &stripped[end + 1..];
        let port = after
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(1080);
        (host.to_string(), port)
    } else if let Some((h, p)) = hostport.rsplit_once(':') {
        let port: u16 = p.parse().map_err(|_| Error::Arg)?;
        (h.to_string(), port)
    } else {
        (hostport.to_string(), 1080)
    };
    let (user, pass) = match auth {
        Some((u, p)) => (Some(u), Some(p)),
        None => (None, None),
    };
    if rest.is_empty() || host.is_empty() {
        return Err(Error::Arg);
    }
    let mut kind = kind;
    // 凭据 → 切到带认证类型
    if user.is_some() {
        kind = match kind {
            ProxyType::Socks5 => ProxyType::Socks5Auth,
            ProxyType::Http => ProxyType::HttpAuth,
            other => other,
        };
    }
    Ok(ProxyCfg {
        kind,
        host,
        port,
        user,
        pass,
    })
}

impl Session {
    pub fn new(save_path: &Path, session_id: &str) -> Result<Self> {
        let sp = cstr(&save_path.to_string_lossy())?;
        let id = cstr(session_id)?;
        let mut raw: *mut lt_session = ptr::null_mut();
        let code = unsafe { lt_session_new(sp.as_ptr(), id.as_ptr(), &mut raw) };
        if code != lt_err_LT_OK || raw.is_null() {
            return Err(Error::from(code));
        }
        Ok(Session { raw })
    }

    /// 全局网络策略：代理（可选）+ 下载/上传限速（KiB/s；0 = 不限/不设置由调用方控制）。
    pub fn apply_network(
        &self,
        proxy: Option<&ProxyCfg>,
        down_kb_s: u32,
        up_kb_s: u32,
    ) -> Result<()> {
        let (ptype, phost, pport, puser, ppass): (
            i32,
            Option<CString>,
            i32,
            Option<CString>,
            Option<CString>,
        ) = match proxy {
            Some(p) => (
                p.kind as i32,
                Some(cstr(&p.host)?),
                p.port as i32,
                p.user.as_deref().map(cstr).transpose()?,
                p.pass.as_deref().map(cstr).transpose()?,
            ),
            None => (0, None, 0, None, None),
        };
        let host_ptr = phost.as_ref().map_or(ptr::null(), |c| c.as_ptr());
        let user_ptr = puser.as_ref().map_or(ptr::null(), |c| c.as_ptr());
        let pass_ptr = ppass.as_ref().map_or(ptr::null(), |c| c.as_ptr());
        let code = unsafe {
            lt_apply_network(
                self.raw,
                ptype,
                host_ptr,
                pport,
                user_ptr,
                pass_ptr,
                down_kb_s as i64 * 1024,
                up_kb_s as i64 * 1024,
            )
        };
        call(code, || Ok(()))
    }

    /// 发现层开关：DHT / LSD / UPnP（enable_upnp 同时控制 NAT-PMP——端口映射族）。
    /// 会话默认全关（M0 确定性语义）；本方法显式覆盖。
    pub fn apply_discovery(
        &self,
        enable_dht: bool,
        enable_lsd: bool,
        enable_upnp: bool,
    ) -> Result<()> {
        let code = unsafe {
            lt_apply_discovery(
                self.raw,
                enable_dht as c_int,
                enable_lsd as c_int,
                enable_upnp as c_int,
            )
        };
        call(code, || Ok(()))
    }

    /// 最近一次错误（内核侧维护的人类可读文本）
    pub fn err_str(&self) -> Result<String> {
        let mut buf = vec![0u8; 1024];
        let mut n: usize = 0;
        let code =
            unsafe { lt_err_str(self.raw, buf.as_mut_ptr() as *mut c_char, buf.len(), &mut n) };
        if code != lt_err_LT_OK && code != lt_err_LT_ERR_BUFFER_TOO_SMALL {
            return Err(Error::from(code));
        }
        if code == lt_err_LT_ERR_BUFFER_TOO_SMALL {
            let mut big = vec![0u8; n];
            let code2 =
                unsafe { lt_err_str(self.raw, big.as_mut_ptr() as *mut c_char, big.len(), &mut n) };
            if code2 != lt_err_LT_OK {
                return Err(Error::from(code2));
            }
            return Ok(CStr::from_bytes_with_nul(&big)
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned());
        }
        Ok(CStr::from_bytes_with_nul(&buf[..n + 1])
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned())
    }

    fn ih(&self, ih: &str) -> Result<CString> {
        cstr(ih)
    }

    fn out_ih(&self) -> [c_char; 41] {
        [0 as c_char; 41]
    }

    pub fn add_magnet(&self, magnet: &str, web_seeds: &[String]) -> Result<String> {
        let m = cstr(magnet)?;
        let mut ih = self.out_ih();
        let (_own, ws) = cstr_array(web_seeds);
        let code = unsafe { lt_add_magnet(self.raw, m.as_ptr(), ws_mut(&ws), ih.as_mut_ptr()) };
        call(code, || Ok(ih_str(&ih)))
    }

    pub fn add_torrent_file(&self, meta: &[u8], web_seeds: &[String]) -> Result<String> {
        let mut ih = self.out_ih();
        let (_own, ws) = cstr_array(web_seeds);
        let code = unsafe {
            lt_add_torrent_file(
                self.raw,
                meta.as_ptr(),
                meta.len(),
                ws_mut(&ws),
                ih.as_mut_ptr(),
            )
        };
        call(code, || Ok(ih_str(&ih)))
    }

    pub fn add_torrent_resume(&self, data: &[u8], web_seeds: &[String]) -> Result<String> {
        let mut ih = self.out_ih();
        let (_own, ws) = cstr_array(web_seeds);
        let code = unsafe {
            lt_add_torrent_resume(
                self.raw,
                data.as_ptr(),
                data.len(),
                ws_mut(&ws),
                ih.as_mut_ptr(),
            )
        };
        call(code, || Ok(ih_str(&ih)))
    }

    pub fn pause(&self, ih: &str) -> Result<()> {
        let i = self.ih(ih)?;
        call(unsafe { lt_pause(self.raw, i.as_ptr()) }, || Ok(()))
    }

    /// 本地 seeder 直连注入（M0 补：无需 tracker）
    pub fn add_peer(&self, ih: &str, ip: &str, port: u16) -> Result<()> {
        let i = self.ih(ih)?;
        let a = cstr(ip)?;
        call(
            unsafe { lt_add_peer(self.raw, i.as_ptr(), a.as_ptr(), port) },
            || Ok(()),
        )
    }

    pub fn resume(&self, ih: &str) -> Result<()> {
        let i = self.ih(ih)?;
        call(unsafe { lt_resume(self.raw, i.as_ptr()) }, || Ok(()))
    }

    pub fn remove(&self, ih: &str, delete_data: bool) -> Result<()> {
        let i = self.ih(ih)?;
        call(
            unsafe { lt_remove(self.raw, i.as_ptr(), delete_data as c_int) },
            || Ok(()),
        )
    }

    pub fn status(&self, ih: &str) -> Result<lt_torrent_status> {
        let i = self.ih(ih)?;
        let mut st = lt_torrent_status {
            state: 0,
            progress: 0.0,
            downloaded: 0,
            total: 0,
            down_rate: 0,
            up_rate: 0,
            num_peers: 0,
            num_seeds: 0,
            metadata_received: 0,
            paused: 0,
            name: [0; 256],
            all_time_download: 0,
            all_time_upload: 0,
        };
        let code = unsafe { lt_status(self.raw, i.as_ptr(), &mut st) };
        call(code, || Ok(st))
    }

    pub fn piece_count(&self, ih: &str) -> Result<i32> {
        let i = self.ih(ih)?;
        let mut n: c_int = 0;
        let code = unsafe { lt_piece_count(self.raw, i.as_ptr(), &mut n) };
        call(code, || Ok(n))
    }

    /// 位打包 bitfield（LSB 先）；自动扩容重试
    pub fn bitfield(&self, ih: &str) -> Result<Vec<u8>> {
        let i = self.ih(ih)?;
        let mut cap = 64usize;
        loop {
            let mut buf = vec![0u8; cap];
            let mut n: usize = 0;
            let code =
                unsafe { lt_bitfield(self.raw, i.as_ptr(), buf.as_mut_ptr(), buf.len(), &mut n) };
            if code == lt_err_LT_OK {
                buf.truncate(n);
                return Ok(buf);
            }
            if code == lt_err_LT_ERR_BUFFER_TOO_SMALL {
                cap = n.max(cap + 1);
                continue;
            }
            return Err(Error::from(code));
        }
    }

    pub fn file_count(&self, ih: &str) -> Result<i32> {
        let i = self.ih(ih)?;
        let mut n: c_int = 0;
        let code = unsafe { lt_file_count(self.raw, i.as_ptr(), &mut n) };
        call(code, || Ok(n))
    }

    /// (已下载, 总大小) 每文件
    pub fn file_progress(&self, ih: &str) -> Result<Vec<(i64, i64)>> {
        let n = self.file_count(ih)?;
        if n <= 0 {
            return Ok(Vec::new());
        }
        let i = self.ih(ih)?;
        let nf = n as usize;
        let mut done = vec![0i64; nf];
        let mut size = vec![0i64; nf];
        let code = unsafe {
            lt_file_progress(
                self.raw,
                i.as_ptr(),
                done.as_mut_ptr(),
                size.as_mut_ptr(),
                n,
            )
        };
        call(code, || Ok(done.into_iter().zip(size).collect()))
    }

    /// 子文件优先级批量设置（P1 任务级能力）。`prio` = (文件下标, 0..=7)，
    /// 内核两段式（全量校验→应用）。需要 metadata（否则 NOT_FOUND）。
    pub fn set_file_priorities(&self, ih: &str, prio: &[(i32, i32)]) -> Result<()> {
        if prio.is_empty() {
            return Ok(());
        }
        let i = self.ih(ih)?;
        let idxs: Vec<i32> = prio.iter().map(|(idx, _)| *idx).collect();
        let prios: Vec<i32> = prio.iter().map(|(_, p)| *p).collect();
        let code = unsafe {
            lt_set_file_priorities(
                self.raw,
                i.as_ptr(),
                idxs.as_ptr(),
                prios.as_ptr(),
                idxs.len() as c_int,
            )
        };
        call(code, || Ok(()))
    }

    /// 读取当前各文件优先级（下标即文件序）。需要 metadata。
    pub fn file_priorities(&self, ih: &str) -> Result<Vec<i32>> {
        let n = self.file_count(ih)?;
        if n <= 0 {
            return Ok(Vec::new());
        }
        let i = self.ih(ih)?;
        let mut out = vec![0 as c_int; n as usize];
        let code = unsafe { lt_get_file_priorities(self.raw, i.as_ptr(), out.as_mut_ptr(), n) };
        call(code, || Ok(out))
    }

    /// 富 peer 列表；尺寸查询 → 分配 → 填充（查询阶段恒传 null/0，避免 !buf 恒真空转）
    pub fn peers(&self, ih: &str) -> Result<Vec<lt_peer>> {
        let i = self.ih(ih)?;
        let mut need: usize = 0;
        let code = unsafe { lt_peers(self.raw, i.as_ptr(), ptr::null_mut(), 0, &mut need) };
        match code {
            lt_err_LT_OK => return Ok(Vec::new()), // 空列表
            lt_err_LT_ERR_BUFFER_TOO_SMALL => {}   // need 已为所需数
            c => return Err(Error::from(c)),
        }
        let mut cap = need;
        loop {
            let mut buf = zeroed_peers(cap);
            let mut n: usize = 0;
            let code2 =
                unsafe { lt_peers(self.raw, i.as_ptr(), buf.as_mut_ptr(), buf.len(), &mut n) };
            if code2 == lt_err_LT_OK {
                buf.truncate(n);
                return Ok(buf);
            }
            if code2 == lt_err_LT_ERR_BUFFER_TOO_SMALL {
                cap = n;
                continue;
            }
            return Err(Error::from(code2));
        }
    }

    pub fn set_alert_mask(&self, mask: u32) -> Result<()> {
        let code = unsafe { lt_set_alert_mask(self.raw, ptr::null(), mask) };
        call(code, || Ok(()))
    }

    /// 弹出 ≤cap 条 alert（剩余留在内核 ring，不丢失）
    pub fn pop_alerts(&self, cap: usize) -> Result<Vec<lt_alert>> {
        let mut buf = zeroed_alerts(cap.max(1));
        let mut n: usize = 0;
        let code = unsafe { lt_pop_alerts(self.raw, buf.as_mut_ptr(), buf.len(), &mut n) };
        if code != lt_err_LT_OK {
            return Err(Error::from(code));
        }
        buf.truncate(n);
        Ok(buf)
    }

    pub fn alerts_dropped(&self) -> Result<u32> {
        let mut n: u32 = 0;
        let code = unsafe { lt_alerts_dropped(self.raw, &mut n) };
        call(code, || Ok(n))
    }

    pub fn request_save_resume(&self, ih: &str) -> Result<()> {
        let i = self.ih(ih)?;
        let code = unsafe { lt_request_save_resume(self.raw, i.as_ptr()) };
        call(code, || Ok(()))
    }

    /// resume bencode；未就绪 → NotFound（调用方需先收到 RESUME alert）
    pub fn take_resume_data(&self, ih: &str) -> Result<Vec<u8>> {
        let i = self.ih(ih)?;
        let mut cap = 0usize;
        loop {
            let mut buf = vec![0u8; cap];
            let mut n: usize = 0;
            let code = unsafe {
                lt_take_resume_data(self.raw, i.as_ptr(), buf.as_mut_ptr(), buf.len(), &mut n)
            };
            if code == lt_err_LT_OK {
                buf.truncate(n);
                return Ok(buf);
            }
            if code == lt_err_LT_ERR_BUFFER_TOO_SMALL {
                cap = n;
                continue;
            }
            return Err(Error::from(code));
        }
    }

    pub fn add_url_seed(&self, ih: &str, url: &str) -> Result<()> {
        let i = self.ih(ih)?;
        let u = cstr(url)?;
        call(
            unsafe { lt_add_url_seed(self.raw, i.as_ptr(), u.as_ptr()) },
            || Ok(()),
        )
    }

    pub fn add_tracker(&self, ih: &str, url: &str) -> Result<()> {
        let i = self.ih(ih)?;
        let u = cstr(url)?;
        call(
            unsafe { lt_add_tracker(self.raw, i.as_ptr(), u.as_ptr()) },
            || Ok(()),
        )
    }

    /// tracker 表列举（E29）：两段式（cap=0 探测 → BUFFER_TOO_SMALL → 扩容重试），
    /// 与 peers() 同构。空表 → Ok(vec![])。
    pub fn list_trackers(&self, ih: &str) -> Result<Vec<lt_tracker_info>> {
        let i = self.ih(ih)?;
        let mut need: c_int = 0;
        let code = unsafe { lt_list_trackers(self.raw, i.as_ptr(), ptr::null_mut(), 0, &mut need) };
        match code {
            lt_err_LT_OK => return Ok(Vec::new()), // 空表
            lt_err_LT_ERR_BUFFER_TOO_SMALL => {}   // need 已为所需数
            c => return Err(Error::from(c)),
        }
        let cap = need as usize;
        let mut buf: Vec<lt_tracker_info> = Vec::with_capacity(cap);
        buf.resize_with(cap, || lt_tracker_info {
            url: [0; 256],
            tier: 0,
        });
        let mut n: c_int = 0;
        let code2 = unsafe {
            lt_list_trackers(self.raw, i.as_ptr(), buf.as_mut_ptr(), cap as c_int, &mut n)
        };
        if code2 != lt_err_LT_OK {
            return Err(Error::from(code2));
        }
        buf.truncate(n.max(0) as usize);
        Ok(buf)
    }

    /// 删 tracker（E29）：内核按 URL 精确匹配，无匹配 → Error（NotFound 定性）。
    pub fn remove_tracker(&self, ih: &str, url: &str) -> Result<()> {
        let i = self.ih(ih)?;
        let u = cstr(url)?;
        call(
            unsafe { lt_remove_tracker(self.raw, i.as_ptr(), u.as_ptr()) },
            || Ok(()),
        )
    }

    pub fn set_sequential(&self, ih: &str, on: bool) -> Result<()> {
        let i = self.ih(ih)?;
        call(
            unsafe { lt_set_sequential(self.raw, i.as_ptr(), on as c_int) },
            || Ok(()),
        )
    }

    pub fn set_limits(&self, ih: &str, down: i64, up: i64) -> Result<()> {
        let i = self.ih(ih)?;
        call(
            unsafe { lt_set_limits(self.raw, i.as_ptr(), down, up) },
            || Ok(()),
        )
    }

    /// 异步 read_piece 轮询：Ok(None) = 尚未就绪（内核持续读取中）
    pub fn read_piece(&self, ih: &str, idx: i32) -> Result<Option<Vec<u8>>> {
        let i = self.ih(ih)?;
        let mut cap = 256usize;
        loop {
            let mut buf = vec![0u8; cap];
            let mut n: usize = 0;
            let code = unsafe {
                lt_read_piece(
                    self.raw,
                    i.as_ptr(),
                    idx,
                    buf.as_mut_ptr(),
                    buf.len(),
                    &mut n,
                )
            };
            match code {
                lt_err_LT_OK => {
                    buf.truncate(n);
                    return Ok(Some(buf));
                }
                lt_err_LT_ERR_NOT_FOUND => return Ok(None),
                lt_err_LT_ERR_BUFFER_TOO_SMALL => {
                    cap = n.max(cap + 1);
                    continue;
                }
                c => return Err(Error::from(c)),
            }
        }
    }

    /// torrent 元数据导出（B-1：magnet → .torrent）。
    /// Ok(None) = metadata 未就绪（调用方先轮询 status.metadata_received）；
    /// Ok(Some(bytes)) = 标准 .torrent bencode。
    pub fn metadata(&self, ih: &str) -> Result<Option<Vec<u8>>> {
        let i = self.ih(ih)?;
        // 初始 64 KiB：绝大多数 .torrent 在此以内（超限自动扩容一次到位）
        let mut cap = 64 * 1024usize;
        loop {
            let mut buf = vec![0u8; cap];
            let mut n: usize = 0;
            let code =
                unsafe { lt_metadata(self.raw, i.as_ptr(), buf.as_mut_ptr(), buf.len(), &mut n) };
            match code {
                lt_err_LT_OK => {
                    buf.truncate(n);
                    return Ok(Some(buf));
                }
                lt_err_LT_ERR_NOT_FOUND => return Ok(None),
                lt_err_LT_ERR_BUFFER_TOO_SMALL => {
                    cap = n.max(cap * 2);
                    continue;
                }
                c => return Err(Error::from(c)),
            }
        }
    }
}

fn ih_str(ih: &[c_char; 41]) -> String {
    let p = ih.as_ptr() as *const c_char;
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// web_seeds → NULL 结尾的 C 字符串指针数组（owned CString 在此借期内存活）
fn cstr_array(web_seeds: &[String]) -> (Vec<CString>, Vec<*const c_char>) {
    let owned: Vec<CString> = web_seeds
        .iter()
        .filter_map(|s| CString::new(s.as_str()).ok())
        .collect();
    let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
    ptrs.push(ptr::null());
    (owned, ptrs)
}

/// web_seeds 指针数组的 C 侧可变指针（bindgen: `*mut *const c_char`）
fn ws_mut(ws: &[*const c_char]) -> *mut *const c_char {
    ws.as_ptr() as *mut *const c_char
}

fn zeroed_alerts(cap: usize) -> Vec<lt_alert> {
    let a = lt_alert {
        kind: 0,
        ih: [0 as c_char; 41],
        msg: [0 as c_char; 512],
        at: 0,
        resume_ready: 0,
    };
    vec![a; cap]
}

fn zeroed_peers(cap: usize) -> Vec<lt_peer> {
    let p = lt_peer {
        ip: [0 as c_char; 64],
        port: 0,
        peer_id: [0 as c_char; 64],
        client: [0 as c_char; 128],
        progress_ppm: 0,
        down_rate: 0,
        up_rate: 0,
        total_download: 0,
        total_upload: 0,
        last_active_sec: 0,
        flags: 0,
    };
    vec![p; cap]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_proxy_socks5_default_port() {
        let c = parse_proxy("socks5://127.0.0.1:1080").unwrap();
        assert_eq!(c.kind, ProxyType::Socks5);
        assert_eq!(c.host, "127.0.0.1");
        assert_eq!(c.port, 1080);
        assert!(c.user.is_none());
    }

    #[test]
    fn parse_proxy_http_with_credentials() {
        let c = parse_proxy("http://user:pass@proxy.example.com").unwrap();
        assert_eq!(c.kind, ProxyType::HttpAuth, "凭据 → HttpAuth");
        assert_eq!(c.host, "proxy.example.com");
        assert_eq!(c.port, 1080, "未给端口默认 1080");
        assert_eq!(c.user.as_deref(), Some("user"));
        assert_eq!(c.pass.as_deref(), Some("pass"));
    }

    #[test]
    fn parse_proxy_socks4_isolation() {
        let c = parse_proxy("socks4://1.2.3.4:9999").unwrap();
        assert_eq!(c.kind, ProxyType::Socks4);
        assert_eq!(c.port, 9999);
    }

    #[test]
    fn parse_proxy_ipv6_literal() {
        let c = parse_proxy("socks5://[::1]:1081").unwrap();
        assert_eq!(c.host, "::1");
        assert_eq!(c.port, 1081);
    }

    #[test]
    fn parse_proxy_bad_scheme_or_shape_errors() {
        assert!(parse_proxy("ftp://x:1").is_err(), "未知 scheme");
        assert!(parse_proxy("socks5://").is_err(), "空");
        assert!(parse_proxy("no-scheme").is_err(), "无 scheme");
        assert!(parse_proxy("socks5://h:notaport").is_err(), "坏端口");
    }
}
