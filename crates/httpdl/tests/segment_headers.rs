//! H-8 回归：段请求必须携带任务级自定义 headers。
//!
//! 契约：`download_dynamic` 的每个段 Range 请求（含拆半重试子段）都要下发
//! 任务级自定义头（与 `probe_range` 同语义）；否则鉴权型源（Cookie/Token）
//! 在 add/probe 阶段通过、段请求阶段 403 —— 下载不可用。
//!
//! 服务端强制校验 `x-test-token`：缺失/错值即 403 并分类计数。断言双重：
//! ① 任务 Completed 且内容一致（headers 确实生效）；
//! ② 服务端记录的缺头请求数 == 0（没有任何一次段请求漏带）。
//! 负例对照：错值 token 的任务必然失败（证明服务端校验真实生效，非假绿）。

mod common;
mod integration;

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use common::{make_http_task_to, wait_terminal};
use integration::http_server::{patterned, sha256_of};
use parking_lot::Mutex;
use smart_dl_core::identity::ContentIdentity;
use smart_dl_core::types::{DownloadEngine, DownloadSource, EngineState};
use smart_dl_httpdl::HttpEngine;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

const TOKEN_HEADER: &str = "x-test-token";
const TOKEN_VALUE: &str = "secret-42";

/// 服务端状态：内容 + 缺头/错值计数 + token 观测序列。
#[derive(Clone)]
struct SrvState {
    body: Arc<Vec<u8>>,
    missing: Arc<AtomicUsize>,
    wrong: Arc<AtomicUsize>,
    seen_tokens: Arc<Mutex<Vec<Option<String>>>>,
}

/// 强制校验自定义头的 Range 测试服务端：缺头/错值 → 403 + 计数；合法 → 206 段内容。
struct TokenServer {
    addr: SocketAddr,
    state: SrvState,
}

impl TokenServer {
    async fn start(size: u64) -> Self {
        let st = SrvState {
            body: Arc::new(patterned(size)),
            missing: Arc::new(AtomicUsize::new(0)),
            wrong: Arc::new(AtomicUsize::new(0)),
            seen_tokens: Arc::new(Mutex::new(Vec::new())),
        };
        let app = Router::new()
            .route("/file", get(handler))
            .with_state(st.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        TokenServer { addr, state: st }
    }

    fn url(&self) -> String {
        format!("http://{}/file", self.addr)
    }

    fn missing(&self) -> usize {
        self.state.missing.load(Ordering::SeqCst)
    }

    fn wrong(&self) -> usize {
        self.state.wrong.load(Ordering::SeqCst)
    }
}

async fn handler(State(st): State<SrvState>, headers: HeaderMap) -> Response {
    let token = headers
        .get(TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    st.seen_tokens.lock().push(token.clone());
    let Some(token) = token else {
        st.missing.fetch_add(1, Ordering::SeqCst);
        return StatusCode::FORBIDDEN.into_response();
    };
    if token != TOKEN_VALUE {
        st.wrong.fetch_add(1, Ordering::SeqCst);
        return StatusCode::FORBIDDEN.into_response();
    }

    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.strip_prefix("bytes="))
        .map(str::to_string);
    let body = st.body.as_ref();
    let total = body.len() as u64;
    let Some(range) = range else {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_LENGTH, total.to_string())
            .body(axum::body::Body::from(body.to_vec()))
            .unwrap()
            .into_response();
    };
    let (s, e) = match range.split_once('-') {
        Some((s, e)) => (s.parse::<u64>().unwrap_or(0), e.parse::<u64>().ok()),
        None => (0, None),
    };
    let end = e.unwrap_or(total - 1).min(total - 1);
    if s > end || s >= total {
        return Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{total}"))
            .body(axum::body::Body::empty())
            .unwrap()
            .into_response();
    }
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(header::CONTENT_RANGE, format!("bytes {s}-{end}/{total}"))
        .body(axum::body::Body::from(
            body[s as usize..=(end as usize)].to_vec(),
        ))
        .unwrap()
        .into_response()
}

/// 构造带自定义头的 Http 任务（DownloadSource::Http.headers）。
fn task_with_headers(
    id: &str,
    url: &str,
    dest_root: &std::path::Path,
    headers: Vec<(String, String)>,
) -> smart_dl_core::task::DownloadTask {
    let mut t = make_http_task_to(id, url, dest_root.to_path_buf(), Some("token.bin"));
    t.source = DownloadSource::Http {
        url: url.to_string(),
        headers,
        auth: None,
        backup_url: None,
    };
    t
}

// —— 直接调 download_dynamic：headers 透传到段请求（API 层契约）—— //

#[tokio::test]
async fn dynamic_segments_carry_custom_headers() {
    use smart_dl_httpdl::download::download_dynamic;
    use smart_dl_httpdl::rate::RateLimiter;
    const MB: u64 = 1024 * 1024;
    const SIZE: u64 = 64 * MB; // 2 worker × 16MB 段

    let srv = TokenServer::start(SIZE).await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("h.part");
    let mirrors = vec![srv.url()];
    let headers = vec![
        (TOKEN_HEADER.to_string(), TOKEN_VALUE.to_string()),
        ("x-extra".to_string(), "v1".to_string()),
    ];

    let r = download_dynamic(
        &reqwest::Client::new(),
        &part,
        SIZE,
        16 * MB,
        &mirrors,
        &headers,
        Arc::new(RateLimiter::new(0)),
        None,
        false,
        None,
        None,
        None,
    )
    .await;
    assert!(r.is_ok(), "带头段下载应成功: {r:?}");
    assert_eq!(std::fs::metadata(&part).unwrap().len(), SIZE);
    assert_eq!(srv.missing(), 0, "所有段请求都必须携带 x-test-token");
    assert_eq!(srv.wrong(), 0);
    assert!(
        srv.state
            .seen_tokens
            .lock()
            .iter()
            .all(|t| t.as_deref() == Some(TOKEN_VALUE)),
        "token 值必须逐字下发"
    );
}

// —— 引擎接线：task.headers → add → 段请求 → Completed —— //

#[tokio::test]
async fn engine_task_headers_reach_segments() {
    const MB: u64 = 1024 * 1024;
    const SIZE: u64 = 64 * MB;

    let srv = TokenServer::start(SIZE).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let mut task = task_with_headers(
        "h-engine",
        &srv.url(),
        dir.path(),
        vec![(TOKEN_HEADER.to_string(), TOKEN_VALUE.to_string())],
    );
    task.identity = ContentIdentity::SingleFile {
        size: 0,
        etag: None,
        sha256: Some(sha256_of(&patterned(SIZE))),
        backup_md5: None,
    };

    let tid = engine.add(&task).await.expect("add 应成功（probe 已带头）");
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(
        st.state,
        EngineState::Completed,
        "带头任务应完成（error={:?}）",
        st.error
    );
    assert_eq!(srv.missing(), 0, "引擎路径下任何段请求都不得漏带任务头");
    let dest = dir.path().join("token.bin");
    assert_eq!(
        sha256_of(&std::fs::read(&dest).unwrap()),
        sha256_of(&patterned(SIZE)),
        "落位内容必须与源一致"
    );
}

// —— 负例对照：错值 token → 全程 403 → 失败（证明服务端校验真实生效）—— //

#[tokio::test]
async fn engine_wrong_token_fails() {
    const MB: u64 = 1024 * 1024;
    const SIZE: u64 = 8 * MB; // 小文件，缩短失败路径

    let srv = TokenServer::start(SIZE).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = task_with_headers(
        "h-neg",
        &srv.url(),
        dir.path(),
        vec![(TOKEN_HEADER.to_string(), "wrong-token".to_string())],
    );
    match engine.add(&task).await {
        // probe 阶段即拒（probe 同样带头 → 403）——同样是头语义生效的证据
        Err(_) => {}
        Ok(tid) => {
            let st = wait_terminal(&engine, &tid).await;
            assert_eq!(st.state, EngineState::Error, "错 token 必须以失败收场");
        }
    }
    assert!(srv.missing() == 0, "负例不产生缺头请求（错值≠缺头）");
    assert!(srv.wrong() > 0, "服务端必须真的拒绝了错值 token");
}
