//! E31 探测预览 e2e：`POST /probe` —— add 前元数据预览（不建任务）。
//! 直链 206 → total/range_supported/suggest_name；CD 声明文件名；404 源 →
//! 502；非 http(s) URL → 400。探测请求不产生任何任务记录。

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::Response,
    routing::get,
    Router,
};
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::net::SocketAddr;
use std::sync::Arc;

fn patterned(size: u64) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

async fn spawn_app(state: Arc<DaemonState>) -> SocketAddr {
    let app = http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

fn new_state() -> Arc<DaemonState> {
    let state = DaemonState::new(Arc::new(HttpEngine::new(reqwest::Client::new())), vec![])
        .with_dest_root(std::env::temp_dir());
    Arc::new(state)
}

/// 直链源（206 Range 支持，与 common::TestServer 同构——本文件自持避免引入
/// 未使用的 common 项触发 bt 构建 dead_code）。
async fn direct_server(body: Vec<u8>) -> String {
    #[derive(Clone)]
    struct S {
        body: Arc<Vec<u8>>,
    }
    async fn handler(State(st): State<S>, headers: HeaderMap) -> Response {
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
                let payload = st.body[start as usize..=(end.min(total - 1)) as usize].to_vec();
                Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(
                        header::CONTENT_RANGE,
                        format!("bytes {start}-{end}/{total}"),
                    )
                    .header(header::CONTENT_TYPE, "application/octet-stream")
                    .body(axum::body::Body::from(payload))
                    .unwrap()
            }
            None => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_LENGTH, st.body.len())
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(axum::body::Body::from((*st.body).clone()))
                .unwrap(),
        }
    }
    let app = Router::new().route("/file", get(handler)).with_state(S {
        body: Arc::new(body),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/file")
}

/// CD 声明文件名源（探测响应带 Content-Disposition）。
async fn cd_server(filename: &'static str) -> String {
    async fn handler(State(f): State<&'static str>) -> Response {
        Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, "bytes 0-0/4096")
            .header(
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{f}\""),
            )
            .body(axum::body::Body::from(vec![1u8]))
            .unwrap()
    }
    let app = Router::new()
        .route("/cd", get(handler))
        .with_state(filename);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/cd")
}

/// 恒 404 源。
async fn missing_server() -> String {
    let app = Router::new().route("/missing", get(|| async { StatusCode::NOT_FOUND }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/missing")
}

#[tokio::test]
async fn probe_direct_link_returns_metadata_without_creating_task() {
    let body = patterned(16 * 1024);
    let url = direct_server(body).await;
    let addr = spawn_app(new_state()).await;
    let client = reqwest::Client::new();

    let before = client
        .get(format!("http://{addr}/tasks"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(before.as_array().map(|a| a.len()), Some(0));

    let resp = client
        .post(format!("http://{addr}/probe"))
        .json(&serde_json::json!({ "url": url }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v = resp.json::<serde_json::Value>().await.unwrap();
    assert_eq!(v["state"], "ok");
    assert_eq!(v["total"], 16384);
    assert_eq!(v["range_supported"], true);
    assert_eq!(v["suggest_name"], "file", "URL 末段派生（无 CD）");
    assert_eq!(
        v["content_type"], "application/octet-stream",
        "Content-Type 透出"
    );

    // 探测零副作用：任务列表仍为空
    let after = client
        .get(format!("http://{addr}/tasks"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(after.as_array().map(|a| a.len()), Some(0));
}

#[tokio::test]
async fn probe_surfaces_content_disposition_filename() {
    let url = cd_server("server-declared.bin").await;
    let addr = spawn_app(new_state()).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{addr}/probe"))
        .json(&serde_json::json!({ "url": url }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v = resp.json::<serde_json::Value>().await.unwrap();
    assert_eq!(v["filename"], "server-declared.bin", "CD 原样透出");
    assert_eq!(
        v["suggest_name"], "server-declared.bin",
        "suggest_name 与引擎派生链同序：CD 优先于 URL 末段"
    );
}

#[tokio::test]
async fn probe_unreachable_source_maps_502() {
    let url = missing_server().await;
    let addr = spawn_app(new_state()).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{addr}/probe"))
        .json(&serde_json::json!({ "url": url }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 502);
    let v = resp.json::<serde_json::Value>().await.unwrap();
    assert_eq!(v["state"], "unreachable");
    assert!(v["error"].as_str().unwrap().contains("probe status 404"));
}

#[tokio::test]
async fn probe_rejects_non_http_url() {
    let addr = spawn_app(new_state()).await;
    let client = reqwest::Client::new();

    for bad in ["ftp://host/f.bin", "magnet:?xt=urn:btih:ABC", "not a url"] {
        let resp = client
            .post(format!("http://{addr}/probe"))
            .json(&serde_json::json!({ "url": bad }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "非法 URL {bad:?} 应 400");
    }

    // 请求体缺 url 字段 → JSON 提取失败（400/422 均为拒绝语义）
    let resp = client
        .post(format!("http://{addr}/probe"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_client_error(),
        "缺 url 应客户端错误: {}",
        resp.status()
    );
}
