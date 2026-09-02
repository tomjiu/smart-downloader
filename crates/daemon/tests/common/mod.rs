//! M6 测试共享：直链 HTTP server（daemon 任务下载源）。

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

pub struct TestServer {
    pub addr: SocketAddr,
}

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
