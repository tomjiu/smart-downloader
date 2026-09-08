//! M6 测试共享：直链 HTTP server（daemon 任务下载源）。

pub mod lt_gate;

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;

/// 确定性内容（与 httpdl/provider 测试基建同构）。
/// 部分测试二进制（如 sequential_api）不用 → 按二进制关闭 dead_code。
#[allow(dead_code)]
pub fn patterned(size: u64) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

#[derive(Clone)]
pub struct DirectLinkServer {
    pub body: Arc<Vec<u8>>,
}

#[allow(dead_code)] // 仅部分 binary（http_api 等）构造；纯 lt_gate 使用者（bt_metadata 等）全 dead
pub struct TestServer {
    pub addr: SocketAddr,
}

#[allow(dead_code)]
impl TestServer {
    pub async fn start(body: Vec<u8>) -> Self {
        let st = DirectLinkServer {
            body: Arc::new(body),
        };
        let app = Router::new().route("/file", get(handler)).with_state(st);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        TestServer { addr }
    }

    pub fn url(&self) -> String {
        format!("http://{}/file", self.addr)
    }
}

#[allow(dead_code)] // 同 TestServer：仅 DirectLinkServer 使用者 binary 活跃
async fn handler(State(st): State<DirectLinkServer>, headers: HeaderMap) -> Response {
    match headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        Some(r) => {
            let spec = r.strip_prefix("bytes=").unwrap_or("");
            let mut parts = spec.split('-');
            let start: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let end: u64 = parts
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(u64::MAX)
                .min(st.body.len() as u64 - 1);
            let total = st.body.len() as u64;
            let payload: Vec<u8> = st
                .body
                .get(start as usize..=(end as usize).min(total as usize - 1))
                .unwrap_or(&[])
                .to_vec();
            axum::response::Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{total}"),
                )
                .body(axum::body::Body::from(payload))
                .unwrap()
                .into_response()
        }
        None => axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_LENGTH, st.body.len().to_string())
            .body(axum::body::Body::from(st.body.as_ref().clone()))
            .unwrap()
            .into_response(),
    }
}

/// 慢速流式直链 server（E11 速率聚合 e2e 用）：按固定 chunk 间隔发送，
/// 下载持续时长 ≥ 数个轮询窗口，速率采样才能捕获非零窗口。
/// 文件 < DEFAULT_MIN_SPLIT（16MiB）→ 单连接整流，chunk 节奏即速率节奏。
/// 部分测试二进制不用 → 按二进制关闭 dead_code（同 `patterned` 惯例）。
#[allow(dead_code)]
pub struct SlowTestServer {
    pub addr: SocketAddr,
}

#[allow(dead_code)]
impl SlowTestServer {
    /// `body` 全量内容，分 `chunks` 块、每块间隔 `chunk_ms` 发送
    /// （总时长 ≈ chunks × chunk_ms + 探测往返）。
    pub async fn start(body: Vec<u8>, chunks: usize, chunk_ms: u64) -> Self {
        let st = DirectLinkServer {
            body: Arc::new(body),
        };
        let app = Router::new()
            .route("/file", get(slow_handler))
            .with_state((st, chunks, chunk_ms));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        SlowTestServer { addr }
    }

    pub fn url(&self) -> String {
        format!("http://{}/file", self.addr)
    }
}

type SlowState = (DirectLinkServer, usize, u64);

#[allow(dead_code)]
async fn slow_handler(
    State((st, chunks, chunk_ms)): State<SlowState>,
    headers: HeaderMap,
) -> Response {
    // 探测（bytes=0-0）与 Range 段请求统一按区间慢速流式回 206
    let (start, end) = match headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        Some(r) => {
            let spec = r.strip_prefix("bytes=").unwrap_or("");
            let mut parts = spec.split('-');
            let s: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let e: u64 = parts
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(u64::MAX)
                .min(st.body.len() as u64 - 1);
            (s, e)
        }
        None => (0, st.body.len() as u64 - 1),
    };
    let total = st.body.len() as u64;
    let slice: Arc<[u8]> = st.body[start as usize..=(end as usize).min(total as usize - 1)]
        .to_vec()
        .into();
    let slice_len = slice.len() as u64;
    let chunk_size = (slice_len as usize).div_ceil(chunks.max(1)).max(1);
    let stream = futures::stream::unfold(0usize, move |i| {
        let slice = slice.clone();
        async move {
            let s = i * chunk_size;
            if s >= slice.len() {
                return None;
            }
            tokio::time::sleep(std::time::Duration::from_millis(chunk_ms)).await;
            let e = (s + chunk_size).min(slice.len());
            Some((Ok::<_, std::io::Error>(slice[s..e].to_vec()), i + 1))
        }
    });
    axum::response::Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{total}"),
        )
        .header(header::CONTENT_LENGTH, slice_len.to_string())
        .body(axum::body::Body::from_stream(stream))
        .unwrap()
        .into_response()
}
