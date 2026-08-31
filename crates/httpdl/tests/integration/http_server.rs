//! M4 测试基建：可配置 HTTP server（axum）。
//! 行为：支持/忽略 Range（206/200）、416、ETag、前 N 次 429、指定 Range 起点 404、
//! 首次坏内容（verify 用）、确定性大文件内容（64MB SHA256 用例）；记录 Range 起点。

// 按测试二进制编译，未使用的构造/helper 属正常
#![allow(dead_code)]

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use md5::{Digest as Md5Digest, Md5};
use parking_lot::Mutex;
use sha2::Sha256;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

/// 确定性内容：i % 251（质数周期，避免 256 对齐巧合）。
pub fn patterned(size: u64) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

/// 按测试二进制编译，未使用的 helper 属正常。
#[allow(dead_code)]
pub fn sha256_of(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// 备用源校验用 MD5（夸克 backup_md5 语义）。
#[allow(dead_code)]
pub fn md5_of(bytes: &[u8]) -> String {
    let mut h = Md5::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

#[derive(Clone)]
pub struct HttpServerConfig {
    /// 文件总大小（字节）。
    pub size: u64,
    /// 尊重 Range（206）；false → 忽略（总是 200 全文件）。
    pub range: bool,
    /// 所有 Range 请求回 416（+Content-Range: bytes */size）。
    pub always_416: bool,
    pub etag: Option<&'static str>,
    /// 前 N 次请求回 429（按请求计数）。
    pub retry_429: u32,
    /// 这些 Range 起点 → 404（模拟中途断流/mirror 失效）。
    pub fail_ranges: Vec<u64>,
    /// 与 fail_ranges 配合：Range 长度 < 该值才放行（模拟"大段失败、拆半后可下载"）。
    pub fail_ranges_min_len: Option<u64>,
    /// 前 N 次请求返回坏内容（verify "首次错后对" 用）；之后返回正常内容。
    pub bad_first: u32,
    /// 使用确定性模式内容（patterned），否则 0x5A 填充。
    pub patterned_content: bool,
    /// 自定义内容（覆盖 size/pattern；仅全文件语义，Range 仍按 size 切）。
    pub content: Option<Vec<u8>>,
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        HttpServerConfig {
            size: 1024,
            range: true,
            always_416: false,
            etag: Some("etag-1"),
            retry_429: 0,
            fail_ranges: vec![],
            fail_ranges_min_len: None,
            bad_first: 0,
            patterned_content: false,
            content: None,
        }
    }
}

pub struct HttpTestServer {
    pub addr: SocketAddr,
    /// 每次带 Range 的请求的起点（验证续传位置/段覆盖）。
    #[allow(dead_code)]
    pub range_starts: Arc<Mutex<Vec<u64>>>,
    #[allow(dead_code)]
    pub request_count: Arc<AtomicUsize>,
}

impl HttpTestServer {
    pub async fn start(cfg: HttpServerConfig) -> Self {
        let range_starts = Arc::new(Mutex::new(Vec::new()));
        let request_count = Arc::new(AtomicUsize::new(0));
        let body = match cfg.content.clone() {
            Some(b) => b,
            None if cfg.patterned_content => patterned(cfg.size),
            None => vec![0x5Au8; cfg.size as usize],
        };

        let app = Router::new()
            .route("/file", get(handler))
            .route("/404", get(|| async { StatusCode::NOT_FOUND }))
            .with_state(ServerState {
                cfg,
                body,
                range_starts: range_starts.clone(),
                request_count: request_count.clone(),
            });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        HttpTestServer {
            addr,
            range_starts,
            request_count,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }
}

#[derive(Clone)]
struct ServerState {
    cfg: HttpServerConfig,
    body: Vec<u8>,
    range_starts: Arc<Mutex<Vec<u64>>>,
    request_count: Arc<AtomicUsize>,
}

async fn handler(State(st): State<ServerState>, headers: HeaderMap) -> Response {
    let req_no = st.request_count.fetch_add(1, Ordering::SeqCst);
    if (req_no as u32) < st.cfg.retry_429 {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }

    let mut builder = Response::builder();
    if let Some(etag) = st.cfg.etag {
        builder = builder.header(header::ETAG, etag);
    }

    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    match range {
        Some(r) if !st.cfg.always_416 => {
            let start = parse_range_start(&r);
            let total = st.body.len() as u64;
            let end = parse_range_end(&r).unwrap_or(total - 1).min(total - 1);
            st.range_starts.lock().push(start);
            // 中途断流/mirror 失效：指定起点 → 404；配 fail_ranges_min_len 时仅对长度 >= 阈值的 Range 生效
            if st.cfg.fail_ranges.contains(&start) {
                let req_len = end - start + 1;
                let allow_small = st
                    .cfg
                    .fail_ranges_min_len
                    .map(|m| req_len < m)
                    .unwrap_or(false);
                if !allow_small {
                    return StatusCode::NOT_FOUND.into_response();
                }
            }
            if st.cfg.range {
                // 坏内容：前 bad_first 次请求返回错字节（其余正常）
                let payload: Vec<u8> = if (req_no as u32) < st.cfg.bad_first {
                    st.body
                        .get(start as usize..=end as usize)
                        .unwrap_or(&[])
                        .iter()
                        .map(|b| b ^ 0xFF)
                        .collect()
                } else {
                    st.body
                        .get(start as usize..=end as usize)
                        .unwrap_or(&[])
                        .to_vec()
                };
                let cr = format!("bytes {}-{}/{}", start, end, total);
                // 206：不设 Content-Length（hyper 按实际 body 长度填充）
                builder
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_RANGE, cr)
                    .body(axum::body::Body::from(payload))
                    .unwrap()
                    .into_response()
            } else {
                // 忽略 Range：200 全文件
                builder
                    .status(StatusCode::OK)
                    .header(header::CONTENT_LENGTH, st.body.len().to_string())
                    .body(axum::body::Body::from(st.body.clone()))
                    .unwrap()
                    .into_response()
            }
        }
        Some(_) => {
            // 416：Content-Range: bytes */total
            builder
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{}", st.body.len()))
                .body(axum::body::Body::empty())
                .unwrap()
                .into_response()
        }
        None => builder
            .status(StatusCode::OK)
            .header(header::CONTENT_LENGTH, st.body.len().to_string())
            .body(axum::body::Body::from(st.body.clone()))
            .unwrap()
            .into_response(),
    }
}

/// 解析 "bytes=START-END" 的起点。
fn parse_range_start(range: &str) -> u64 {
    range
        .strip_prefix("bytes=")
        .and_then(|r| r.split('-').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// 解析 "bytes=START-END" 的终点（可选）。
fn parse_range_end(range: &str) -> Option<u64> {
    range
        .strip_prefix("bytes=")
        .and_then(|r| r.split('-').nth(1))
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse().ok())
}
