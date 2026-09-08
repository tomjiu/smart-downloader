//! B1 Metalink4 e2e（RFC 5854 → 逐 `<file>` 展开为 HTTP 任务集）：
//! 1. `metalink_b64`（本地 XML 内容）→ 多文件展开 → 全部 Completed + 落盘一致
//!    + XML 内建 sha256 直通 E3 校验链；
//! 2. `.meta4` URL 后缀 → daemon 引导拉取 XML → 展开任务集（响应 task_ids/count）；
//! 3. priority 排序：主源 404 → 次高 priority 备源 failover（E2/E3 直通）；
//! 4. 非法 XML（无 `<file>`）→ 400，错误信息透出。

mod common;

use axum::response::IntoResponse;
use axum::routing::get;
use common::patterned;
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;
use std::time::Duration;

fn alpha_vec() -> Vec<u8> {
    patterned(64)
}
/// patterned(64) 的 sha256（XML 内建校验直通 E3 链路）
const ALPHA_SHA256: &str = "fdeab9acf3710362bd2658cdc9a29e8f9c757fcf9811603a8c447cd1d9151108";

/// 带 Range 支持的静态文件 handler（对齐 common::DirectLinkServer 语义：
/// Range → 206 + Content-Range；无 Range → 200 全量）。httpdl 段下载期望
/// 206，200 全量会被判为段失败。
async fn file_with_range(
    axum::extract::State(body): axum::extract::State<Arc<Vec<u8>>>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::http::header;
    match headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        Some(r) => {
            let spec = r.strip_prefix("bytes=").unwrap_or("");
            let mut parts = spec.split('-');
            let start: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let end: u64 = parts
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(u64::MAX)
                .min(body.len() as u64 - 1);
            let total = body.len() as u64;
            let payload: Vec<u8> = body
                .get(start as usize..=(end as usize).min(total as usize - 1))
                .unwrap_or(&[])
                .to_vec();
            axum::response::Response::builder()
                .status(axum::http::StatusCode::PARTIAL_CONTENT)
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{total}"),
                )
                .body(axum::body::Body::from(payload))
                .unwrap()
        }
        None => axum::response::Response::builder()
            .status(axum::http::StatusCode::OK)
            .header(header::CONTENT_LENGTH, body.len().to_string())
            .body(axum::body::Body::from((*body).clone()))
            .unwrap(),
    }
}

/// metalink 测试源站：
/// - `/live/alpha.bin` → 200 ALPHA（alpha 主源）
/// - `/dead/beta.bin` → 404（beta 主源，验证 failover）
/// - `/live/beta.bin` → 200 BETA（beta 备源）
/// - `/list.meta4` → 200 引用上述 URL 的 metalink4 XML
async fn serve_metalink_host() -> String {
    let alpha = Arc::new(alpha_vec());
    let beta = Arc::new(b"beta-payload-0123456789".to_vec());
    let app = axum::Router::new()
        .route(
            "/live/alpha.bin",
            get(move |h: axum::http::HeaderMap| {
                file_with_range(axum::extract::State(alpha.clone()), h)
            }),
        )
        .route(
            "/live/beta.bin",
            get(move |h: axum::http::HeaderMap| {
                file_with_range(axum::extract::State(beta.clone()), h)
            }),
        )
        .route(
            "/dead/beta.bin",
            get(move || async move { (axum::http::StatusCode::NOT_FOUND, Vec::<u8>::new()) }),
        )
        .route("/list.meta4", get(serve_meta4_xml))
        .fallback(not_found);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// 动态生成 metalink4 XML（host 运行时已知）。
async fn serve_meta4_xml(axum::extract::Host(host): axum::extract::Host) -> impl IntoResponse {
    let xml = metalink_xml(&host);
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/metalink4+xml",
        )],
        xml,
    )
}

fn metalink_xml(host: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <publisher><name>metalink-e2e</name></publisher>
  <file name="alpha.bin">
    <size>64</size>
    <hash type="sha256">{ALPHA_SHA256}</hash>
    <url priority="1">http://{host}/live/alpha.bin</url>
    <url location="FR" priority="2">http://{host}/dead/alpha.bin</url>
  </file>
  <file name="beta.bin">
    <url priority="1">http://{host}/dead/beta.bin</url>
    <url priority="2">http://{host}/live/beta.bin</url>
  </file>
</metalink>
"#
    )
}

async fn not_found() -> impl IntoResponse {
    axum::http::StatusCode::NOT_FOUND
}

async fn serve_daemon(dest: std::path::PathBuf, bootstrap: bool) -> String {
    let engine = HttpEngine::new(reqwest::Client::new());
    let state = DaemonState::new(Arc::new(engine), vec![]).with_dest_root(dest);
    // bootstrap=false 时依赖 fetch_metalink_xml 的裸 client 兜底路径
    let state = if bootstrap {
        Arc::new(state.with_bootstrap_client(reqwest::Client::new()))
    } else {
        Arc::new(state)
    };
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let _h = smart_dl_daemon::http_events::spawn_http_events(state, Duration::from_millis(100));
    format!("http://{addr}")
}

async fn wait_completed(client: &reqwest::Client, base: &str, id: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let snap: serde_json::Value = client
            .get(format!("{base}/tasks/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if snap["state"] == "Completed" {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "30s 内未完成: {snap}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn metalink_b64_expands_files_and_verifies_hash() {
    let host = serve_metalink_host().await;
    // metalink_xml 接受纯 authority（scheme 由模板提供），剥掉 base 的 http://
    let host = host.trim_start_matches("http://").to_string();
    let dir = tempfile::tempdir().unwrap();
    let base = serve_daemon(dir.path().to_path_buf(), false).await;
    let client = reqwest::Client::new();

    let xml = metalink_xml(&host);
    let b64 = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(xml.as_bytes())
    };
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "metalink_b64": b64, "dest": dir.path().to_str().unwrap() }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "metalink_b64 展开 201");
    let body: serde_json::Value = resp.json().await.unwrap();
    let ids: Vec<String> = body["task_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 2, "两个 <file> → 两个任务");
    assert_eq!(body["count"].as_u64().unwrap(), 2);
    assert_eq!(body["task_id"].as_str().unwrap(), ids[0], "task_id = 首个");

    for id in &ids {
        wait_completed(&client, &base, id).await;
    }
    // 落盘内容一致（alpha 走 E3 sha256 校验链；beta 主源死 → failover 备源）
    let alpha = std::fs::read(dir.path().join("alpha.bin")).unwrap();
    assert_eq!(alpha, alpha_vec(), "alpha 内容一致（sha256 校验通过）");
    let beta = std::fs::read(dir.path().join("beta.bin")).unwrap();
    assert_eq!(
        beta, b"beta-payload-0123456789",
        "beta 内容一致（failover 备源）"
    );
}

#[tokio::test]
async fn meta4_url_bootstrap_fetch_expands() {
    let host = serve_metalink_host().await;
    let dir = tempfile::tempdir().unwrap();
    let base = serve_daemon(dir.path().to_path_buf(), true).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": format!("{host}/list.meta4"),
            "dest": dir.path().to_str().unwrap(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, ".meta4 URL 引导展开 201");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["count"].as_u64().unwrap(), 2);

    let ids: Vec<String> = body["task_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    for id in &ids {
        wait_completed(&client, &base, id).await;
    }
    assert!(dir.path().join("alpha.bin").exists());
    assert!(dir.path().join("beta.bin").exists());
}

#[tokio::test]
async fn metalink_bad_xml_rejected_400() {
    let dir = tempfile::tempdir().unwrap();
    let base = serve_daemon(dir.path().to_path_buf(), false).await;
    let client = reqwest::Client::new();

    let b64 = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(b"<metalink/>")
    };
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "metalink_b64": b64 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("无 <file>"),
        "解析错误信息透出: {body}"
    );

    // 非法 base64 同样 400
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "metalink_b64": "!!not-base64!!" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}
