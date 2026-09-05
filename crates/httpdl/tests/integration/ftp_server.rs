//! 测试基建：本地最小 FTP server（tokio 手写，FTP 协议子集）。
//! 支持：USER/PASS 登录、TYPE I 二进制、PASV 被动模式、SIZE、REST 断点、RETR 传输、
//! LIST 目录列表（UNIX ls -l 风格 + total 头 + 注入子目录行）、
//! 前 N 次控制连接回 421（退避重试测试）；记录 REST 偏移与连接数。

// 按测试二进制编译，未使用的构造/helper 属正常
#![allow(dead_code)]

use parking_lot::Mutex;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone)]
pub struct FtpServerConfig {
    pub size: u64,
    /// 内容（默认 0x5A 填充）。
    pub content: Option<Vec<u8>>,
    /// 前 N 次控制连接回 421（服务不可用）。
    pub reject_421: u32,
    /// RETR 总是回 550（文件不存在）。
    pub retr_550: bool,
    /// REST 起点命中时 RETR 回 421（P1 缩小粒度重试注入；非终态，触发客户端拆分重试）。
    pub fail_ranges: Vec<u64>,
    /// fail_ranges 注入的最大命中次数（FTP 的 RETR 无请求长度语义，改用次数上限
    /// 保证拆分收敛：耗尽后同起点请求放行；None = 无限注入）。
    pub fail_ranges_max_hits: Option<usize>,
    /// 目录场景：`(远端绝对路径, 内容)`。SIZE/RETR 按路径匹配；LIST 列出直接子文件。
    pub files: Vec<(String, Vec<u8>)>,
    /// LIST 响应中额外插入的子目录行名（目录下载过滤测试用）。
    pub list_subdirs: Vec<String>,
}

impl Default for FtpServerConfig {
    fn default() -> Self {
        FtpServerConfig {
            size: 1024,
            content: None,
            reject_421: 0,
            retr_550: false,
            fail_ranges: Vec::new(),
            fail_ranges_max_hits: None,
            files: Vec::new(),
            list_subdirs: Vec::new(),
        }
    }
}

pub struct FtpTestServer {
    pub addr: SocketAddr,
    /// 每次 RETR 的 REST 偏移（续传验证）。
    pub rest_offsets: Arc<Mutex<Vec<u64>>>,
    /// 控制连接计数。
    pub control_connections: Arc<AtomicUsize>,
    /// RETR 请求计数。
    pub retr_count: Arc<AtomicUsize>,
}

impl FtpTestServer {
    pub async fn start(cfg: FtpServerConfig) -> Self {
        let rest_offsets = Arc::new(Mutex::new(Vec::new()));
        let control_connections = Arc::new(AtomicUsize::new(0));
        let retr_count = Arc::new(AtomicUsize::new(0));
        let fail_hits = Arc::new(AtomicUsize::new(0));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (ro, cc, rc, fh) = (
            rest_offsets.clone(),
            control_connections.clone(),
            retr_count.clone(),
            fail_hits.clone(),
        );
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let (ro, cc, rc, fh) = (ro.clone(), cc.clone(), rc.clone(), fh.clone());
                let cfg = cfg.clone();
                tokio::spawn(async move {
                    handle_control(stream, cfg, ro, cc, rc, fh).await;
                });
            }
        });
        FtpTestServer {
            addr,
            rest_offsets,
            control_connections,
            retr_count,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("ftp://user:pass@{}:{}", self.addr.ip(), self.addr.port()) + path
    }
}

async fn handle_control(
    mut stream: TcpStream,
    cfg: FtpServerConfig,
    rest_offsets: Arc<Mutex<Vec<u64>>>,
    control_connections: Arc<AtomicUsize>,
    retr_count: Arc<AtomicUsize>,
    fail_hits: Arc<AtomicUsize>,
) {
    let conn_no = control_connections.fetch_add(1, Ordering::SeqCst);
    if (conn_no as u32) < cfg.reject_421 {
        let _ = stream.write_all(b"421 Service not available\r\n").await;
        return;
    }
    let _ = stream.write_all(b"220 test ftp ready\r\n").await;

    let mut reader = BufReader::new(stream);
    let mut rest: u64 = 0;
    let mut data_listener: Option<TcpListener> = None;
    let mut body: Option<Vec<u8>> = None; // RETR 数据（懒构建）

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let cmd = line.trim_end_matches("\r\n").trim_end();
        let (verb, arg) = match cmd.split_once(' ') {
            Some((v, a)) => (v, a.trim()),
            None => (cmd, ""),
        };
        let conn = reader.get_mut();
        match verb {
            "USER" | "PASS" => {
                let _ = conn.write_all(b"230 logged in\r\n").await;
            }
            "TYPE" => {
                let _ = conn.write_all(b"200 type set\r\n").await;
            }
            "PASV" => {
                // 数据监听：bind 随机端口
                let dl = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let da = dl.local_addr().unwrap();
                let p1 = da.port() / 256;
                let p2 = da.port() % 256;
                let resp = format!("227 Entering Passive Mode (127,0,0,1,{},{})\r\n", p1, p2);
                let _ = conn.write_all(resp.as_bytes()).await;
                data_listener = Some(dl);
            }
            "SIZE" => {
                // 目录场景：按路径匹配 files；未命中 → 单文件场景（cfg.size）
                let n = match find_file(&cfg.files, arg) {
                    Some((_, d)) => d.len() as u64,
                    None => cfg.size,
                };
                let _ = conn.write_all(format!("213 {n}\r\n").as_bytes()).await;
            }
            "REST" => {
                if let Ok(n) = arg.parse::<u64>() {
                    rest = n;
                    let _ = conn.write_all(b"350 Restarting at position\r\n").await;
                } else {
                    let _ = conn.write_all(b"501 bad REST\r\n").await;
                }
            }
            "LIST" => {
                // 目录列表：files 中该目录的直接子文件 + 注入的子目录行（UNIX ls -l 风格）
                let dir = arg.trim_end_matches('/');
                let prefix = format!("{dir}/");
                let mut lines: Vec<String> = Vec::new();
                for (p, d) in &cfg.files {
                    if let Some(name) = p.strip_prefix(&prefix) {
                        if !name.is_empty() && !name.contains('/') {
                            lines.push(format!(
                                "-rw-r--r--  1 owner  group  {:>8} Jan 01 12:00 {name}",
                                d.len()
                            ));
                        }
                    }
                }
                for sub in &cfg.list_subdirs {
                    lines.push(format!(
                        "drwxr-xr-x  2 owner  group  {:>8} Jan 01 12:00 {sub}",
                        4096
                    ));
                }
                let mut text = format!("total {}\r\n", lines.len());
                text.push_str(&lines.join("\r\n"));
                text.push_str("\r\n");
                if data_listener.is_none() {
                    let _ = conn.write_all(b"425 no data connection\r\n").await;
                    continue;
                }
                let _ = conn.write_all(b"150 opening data connection\r\n").await;
                if let Some(dl) = data_listener.as_ref() {
                    if let Ok((mut data, _)) = dl.accept().await {
                        let _ = data.write_all(text.as_bytes()).await;
                        let _ = data.shutdown().await;
                    }
                }
                let _ = conn.write_all(b"226 transfer complete\r\n").await;
                data_listener = None;
            }
            "RETR" => {
                rest_offsets.lock().push(rest);
                retr_count.fetch_add(1, Ordering::SeqCst);
                if cfg.retr_550 {
                    let _ = conn.write_all(b"550 file unavailable\r\n").await;
                    continue;
                }
                // P1 注入：起点命中 fail_ranges 且未耗尽命中上限 → 421
                // （非终态；对应 HTTP 侧 fail_ranges 语义，触发缩小粒度重试）
                if cfg.fail_ranges.contains(&rest) {
                    let hits = fail_hits.fetch_add(1, Ordering::SeqCst);
                    if hits < cfg.fail_ranges_max_hits.unwrap_or(usize::MAX) {
                        let _ = conn.write_all(b"421 fail injection\r\n").await;
                        continue;
                    }
                }
                // 目录场景：按路径匹配 files；未命中 → 单文件场景（懒构建 body）
                let data: Vec<u8> = match find_file(&cfg.files, arg) {
                    Some((_, d)) => d.clone(),
                    None => {
                        if body.is_none() {
                            body = Some(
                                cfg.content
                                    .clone()
                                    .unwrap_or_else(|| vec![0x5Au8; cfg.size as usize]),
                            );
                        }
                        body.clone().unwrap()
                    }
                };
                if rest > data.len() as u64 {
                    let _ = conn.write_all(b"550 file unavailable\r\n").await;
                    continue;
                }
                let _ = conn.write_all(b"150 opening data connection\r\n").await;
                if let Some(dl) = data_listener.as_ref() {
                    if let Ok((mut data_conn, _)) = dl.accept().await {
                        let chunk = &data[rest as usize..];
                        let _ = data_conn.write_all(chunk).await;
                        let _ = data_conn.shutdown().await;
                    }
                }
                let _ = conn.write_all(b"226 transfer complete\r\n").await;
                rest = 0;
                data_listener = None;
            }
            "QUIT" => {
                let _ = conn.write_all(b"221 bye\r\n").await;
                return;
            }
            _ => {
                let _ = conn.write_all(b"502 unknown\r\n").await;
            }
        }
    }
}

/// 便捷：确定性内容（与 http_server 的 patterned 同构）。
pub fn patterned(size: u64) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

/// 按远端绝对路径查 files 条目。
fn find_file<'a>(files: &'a [(String, Vec<u8>)], path: &str) -> Option<&'a (String, Vec<u8>)> {
    files.iter().find(|(p, _)| p == path)
}
