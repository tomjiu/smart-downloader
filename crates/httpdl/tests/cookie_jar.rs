//! A5：cookie jar（reqwest cookies feature）端到端回归。
//!
//! 契约：引擎共享 client 启用内存 cookie 存储——add 探测响应的 Set-Cookie
//! 必须被存入 jar，且**后续同站请求（下载本体）自动携带**；否则登录型源
//! （探测放行、段请求校验会话）在下载阶段 403。服务端语义：
//! - 首个无 cookie 请求 = 引导请求 → 200/206 正常服务 + Set-Cookie: sid=ck-42
//! - 其后任何无 cookie 请求 → 403 + 计数（jar 未生效的证据）
//! - 携带正确 cookie 的请求 → 正常服务 + 计数
//!
//! 断言：任务 Completed 且内容一致（jar 不破坏既有链路）+ cookie 请求 ≥ 1
//! （jar 确实发出）+ 引导后无 cookie 请求数 == 0（每个后续请求都带）。

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
use integration::http_server::patterned;
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::HttpEngine;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

const COOKIE_NAME: &str = "sid";
const COOKIE_VALUE: &str = "ck-42";

#[derive(Clone)]
struct SrvState {
    body: Arc<Vec<u8>>,
    with_cookie: Arc<AtomicUsize>,
    /// 引导请求（首个无 cookie 请求）之后仍无 cookie 的请求数
    missing_after_bootstrap: Arc<AtomicUsize>,
    bootstrapped: Arc<AtomicBool>,
}

struct CookieServer {
    addr: SocketAddr,
    state: SrvState,
}

impl CookieServer {
    async fn start(size: u64) -> Self {
        let st = SrvState {
            body: Arc::new(patterned(size)),
            with_cookie: Arc::new(AtomicUsize::new(0)),
            missing_after_bootstrap: Arc::new(AtomicUsize::new(0)),
            bootstrapped: Arc::new(AtomicBool::new(false)),
        };
        let app = Router::new()
            .route("/file", get(handler))
            .with_state(st.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        CookieServer { addr, state: st }
    }

    fn url(&self) -> String {
        format!("http://{}/file", self.addr)
    }

    fn with_cookie(&self) -> usize {
        self.state.with_cookie.load(Ordering::SeqCst)
    }

    fn missing_after_bootstrap(&self) -> usize {
        self.state.missing_after_bootstrap.load(Ordering::SeqCst)
    }
}

fn serve_range(st: &SrvState, headers: &HeaderMap, set_cookie: bool) -> Response {
    let body = st.body.as_ref();
    let total = body.len() as u64;
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.strip_prefix("bytes="))
        .map(str::to_string);
    let mut resp = match range {
        None => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_LENGTH, total.to_string())
            .body(axum::body::Body::from(body.to_vec()))
            .unwrap(),
        Some(r) => {
            let (s, e) = match r.split_once('-') {
                Some((s, e)) => (s.parse::<u64>().unwrap_or(0), e.parse::<u64>().ok()),
                None => (0, None),
            };
            let end = e.unwrap_or(total - 1).min(total - 1);
            let start = s.min(end);
            Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{total}"),
                )
                .body(axum::body::Body::from(
                    body[start as usize..=(end) as usize].to_vec(),
                ))
                .unwrap()
        }
    };
    if set_cookie {
        resp.headers_mut().insert(
            header::SET_COOKIE,
            format!("{COOKIE_NAME}={COOKIE_VALUE}; Path=/")
                .parse()
                .unwrap(),
        );
    }
    resp.into_response()
}

async fn handler(State(st): State<SrvState>, headers: HeaderMap) -> Response {
    let has_sid = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|c| {
            c.split(';').any(|kv| {
                let kv = kv.trim();
                kv == format!("{COOKIE_NAME}={COOKIE_VALUE}")
            })
        })
        .unwrap_or(false);
    if has_sid {
        st.with_cookie.fetch_add(1, Ordering::SeqCst);
        return serve_range(&st, &headers, false);
    }
    if st.bootstrapped.swap(true, Ordering::SeqCst) {
        // 引导之后仍无 cookie → jar 未生效
        st.missing_after_bootstrap.fetch_add(1, Ordering::SeqCst);
        return StatusCode::FORBIDDEN.into_response();
    }
    // 引导请求：正常服务 + 下发 Set-Cookie
    serve_range(&st, &headers, true)
}

#[tokio::test]
async fn cookie_jar_carries_set_cookie_to_downstream_requests() {
    let size = 256 * 1024;
    let src = patterned(size);
    let srv = CookieServer::start(size).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(
        reqwest::Client::builder()
            // A5 契约本体：daemon serve 全局 client 同口径（cookie_store(true)）
            .cookie_store(true)
            .build()
            .unwrap(),
    );
    let task = make_http_task_to("ck1", &srv.url(), dir.path().to_path_buf(), Some("ck.bin"));
    let tid = engine.add(&task).await.unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    let got = std::fs::read(dir.path().join("ck.bin")).unwrap();
    assert_eq!(got, src, "cookie jar 不得影响内容正确性");
    assert!(srv.with_cookie() >= 1, "探测后同站请求必须携带 cookie");
    assert_eq!(
        srv.missing_after_bootstrap(),
        0,
        "引导请求之后不允许出现无 cookie 请求（jar 未生效）"
    );
}
