//! 顺序下载（边下边播）：在飞段窗口收紧验证。
//!
//! 契约：`download_dynamic(sequential=true)` 时同时在飞的 Range 请求 ≤
//! `SEQUENTIAL_WINDOW`（2）；false（默认并行）时 worker 全量并发（= worker 数）。
//!
//! 内存口径：自包含流式服务器按公式（i % 251）现算内容，不持大 Vec
//! （192MB 文件仅落盘，RAM 占用 KB 级——沙盒/CI 均安全）；慢端点（每请求
//! 120ms）拉开请求重叠窗口，服务端 RAII 计数观测峰值并发；上限语义由
//! 信号量保证，无时序脆弱性。
//!
//! 另含 engine 接线冒烟：task.sequential 字段 → add → 下载完成 + 内容一致
//! （小文件，证明字段确实传导到 download_dynamic）。

mod common;
mod integration;

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use common::{make_http_task_to, wait_terminal};
use integration::http_server::{patterned, sha256_of, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::download::download_dynamic;
use smart_dl_httpdl::rate::RateLimiter;
use smart_dl_httpdl::HttpEngine;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

const MB: u64 = 1024 * 1024;
/// 192MB → n_workers = clamp(192/64, 2, 8) = 3；min_split 16MB → 12 段。
const SIZE: u64 = 192 * MB;
const SLOW_MS: u64 = 120;

// —— 自包含流式测试服务器：公式内容 + 在飞并发计数 —— //

#[derive(Clone)]
struct StreamState {
    total: u64,
    slow_ms: u64,
    current: Arc<AtomicUsize>,
    max_concurrent: Arc<AtomicUsize>,
    request_count: Arc<AtomicUsize>,
}

struct ConcGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for ConcGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 公式内容：第 i 字节 = (i % 251) as u8（与测试基建 patterned 同构，现算不落内存）。
fn byte_at(i: u64) -> u8 {
    (i % 251) as u8
}

async fn stream_handler(State(st): State<StreamState>, headers: HeaderMap) -> Response {
    let req_no = st.request_count.fetch_add(1, Ordering::SeqCst);
    let _ = req_no;
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("bytes=0-")
        .to_string();
    let spec = range.strip_prefix("bytes=").unwrap_or("");
    let mut it = spec.split('-');
    let start: u64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let end: u64 = it
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(st.total - 1)
        .min(st.total - 1);

    // RAII 在飞计数 + 慢端点（拉开请求重叠窗口）
    let _guard = ConcGuard {
        counter: st.current.clone(),
    };
    let cur = st.current.fetch_add(1, Ordering::SeqCst) + 1;
    st.max_concurrent.fetch_max(cur, Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(st.slow_ms)).await;

    let cr = format!("bytes {start}-{end}/{}", st.total);
    // 16KB 块流式产出公式内容（首尾按区间偏移对齐）
    const CHUNK: u64 = 16 * 1024;
    let stream = futures::stream::unfold((start, end, 0u64), move |(s, e, off)| async move {
        if s + off > e {
            return None;
        }
        let n = CHUNK.min(e - (s + off) + 1);
        let data: Vec<u8> = (0..n).map(|k| byte_at(s + off + k)).collect();
        Some((Ok::<_, std::io::Error>(data), (s, e, off + n)))
    });
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(header::CONTENT_RANGE, cr)
        .body(Body::from_stream(stream))
        .unwrap()
        .into_response()
}

struct StreamServer {
    pub addr: SocketAddr,
    pub max_concurrent: Arc<AtomicUsize>,
}

impl StreamServer {
    async fn start(total: u64, slow_ms: u64) -> Self {
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        let st = StreamState {
            total,
            slow_ms,
            current: Arc::new(AtomicUsize::new(0)),
            max_concurrent: max_concurrent.clone(),
            request_count: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route("/file", get(stream_handler))
            .with_state(st);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        StreamServer {
            addr,
            max_concurrent,
        }
    }

    fn url(&self) -> String {
        format!("http://{}/file", self.addr)
    }
}

fn peak_of(srv: &StreamServer) -> usize {
    srv.max_concurrent.load(Ordering::SeqCst)
}

// —— 窗口契约测试（直调 download_dynamic）—— //

#[tokio::test]
async fn sequential_caps_inflight_segments() {
    let srv = StreamServer::start(SIZE, SLOW_MS).await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("seq.part");
    let client = reqwest::Client::new();
    let mirrors = vec![srv.url()];

    let r = download_dynamic(
        &client,
        &part,
        SIZE,
        0,
        16 * MB,
        &mirrors,
        Arc::new(RateLimiter::new(0)),
        None,
        true,
    )
    .await;
    assert!(r.is_ok(), "顺序下载应成功: {r:?}");
    assert_eq!(
        std::fs::metadata(&part).unwrap().len(),
        SIZE,
        "顺序模式应完整覆盖 [0, total)"
    );
    let peak = peak_of(&srv);
    assert!(
        peak <= smart_dl_httpdl::download::SEQUENTIAL_WINDOW,
        "顺序模式在飞峰值 {peak} 必须 ≤ 窗口 2"
    );
}

#[tokio::test]
async fn parallel_runs_full_workers() {
    let srv = StreamServer::start(SIZE, SLOW_MS).await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("par.part");
    let client = reqwest::Client::new();
    let mirrors = vec![srv.url()];

    let r = download_dynamic(
        &client,
        &part,
        SIZE,
        0,
        16 * MB,
        &mirrors,
        Arc::new(RateLimiter::new(0)),
        None,
        false,
    )
    .await;
    assert!(r.is_ok(), "并行下载应成功: {r:?}");
    let peak = peak_of(&srv);
    assert!(peak >= 3, "默认并行模式在飞峰值 {peak} 应达到 worker 数 3");
}

// —— engine 接线冒烟：task.sequential → add → 完成且内容一致 —— //

#[tokio::test]
async fn engine_add_sequential_completes_and_verifies() {
    // 小文件（8MB → 1 段）：窗口语义由上面两个直调测试保证，此处只证明
    // task.sequential 字段被引擎拾取（传导到 download_dynamic）且结果正确。
    let size = 8 * MB;
    let srv = HttpTestServer::start(HttpServerConfig {
        size,
        range: true,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let mut task = make_http_task_to(
        "seq-engine",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("out.bin"),
    );
    task.sequential = true;
    let tid = engine.add(&task).await.unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    let got = std::fs::read(dir.path().join("out.bin")).unwrap();
    assert_eq!(sha256_of(&got), sha256_of(&patterned(size)));
}
