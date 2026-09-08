//! E19 按条件批量 e2e：`POST /tasks/batch` 带 `select`（与 `ids` 二选一）——
//! engine 选择器批量暂停 → state 选择器批量恢复（一键重试场景经记录态白盒
//! 不便，e2e 用 queued→pause→resume 链路验证执行器与响应形状）；非破坏性
//! 约束（remove → 400）、互斥校验（ids+select / 全空 select → 400）、
//! 非法标签 → 400。

mod common;

use common::TestServer;
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;

async fn serve() -> String {
    let engine = HttpEngine::new(reqwest::Client::new());
    let state = DaemonState::new(Arc::new(engine), vec![]).with_dest_root(std::env::temp_dir());
    let state = Arc::new(state);
    let app = http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn add_task(client: &reqwest::Client, base: &str, url: &str, n: u64) -> String {
    let dest = std::env::temp_dir().join(format!("e19-batch-{}-{n}", std::process::id()));
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": url, "dest": dest.to_str().unwrap() }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn batch_select_pause_then_resume_by_state() {
    let base = serve().await;
    let client = reqwest::Client::new();
    let mut servers = Vec::new();
    let mut ids = Vec::new();
    for n in 0..3u64 {
        let srv = TestServer::start(common::patterned(1024 * (n + 1))).await;
        servers.push(srv);
    }
    for (n, srv) in servers.iter().enumerate() {
        ids.push(add_task(&client, &base, &srv.url(), n as u64).await);
    }

    // engine 选择器批量暂停（3 条均为 http）
    let resp = client
        .post(format!("{base}/tasks/batch"))
        .json(&serde_json::json!({
            "action": "pause",
            "select": { "engine": "http" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let outcome: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(outcome["succeeded"], 3, "3 条 http 任务全部暂停");
    assert_eq!(outcome["failed"], 0);

    // 列表确认 Paused
    let rows: serde_json::Value = client
        .get(format!("{base}/tasks?state=paused"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 3);

    // state 选择器批量恢复（一键恢复全部暂停）
    let resp = client
        .post(format!("{base}/tasks/batch"))
        .json(&serde_json::json!({
            "action": "resume",
            "select": { "state": "paused" },
        }))
        .send()
        .await
        .unwrap();
    let outcome: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(outcome["succeeded"], 3);
    // 全部回到 Downloading（记录态）
    let rows: serde_json::Value = client
        .get(format!("{base}/tasks?state=paused"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(rows.as_array().unwrap().is_empty(), "无剩余 Paused");
}

#[tokio::test]
async fn batch_select_validation_rejected() {
    let base = serve().await;
    let client = reqwest::Client::new();

    // remove 经选择器 → 400（非破坏性原则）
    let resp = client
        .post(format!("{base}/tasks/batch"))
        .json(&serde_json::json!({
            "action": "remove",
            "select": { "state": "completed" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // select 与 ids 同时提供 → 400
    let resp = client
        .post(format!("{base}/tasks/batch"))
        .json(&serde_json::json!({
            "action": "pause",
            "ids": ["t1"],
            "select": { "state": "queued" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 全空 select → 400
    let resp = client
        .post(format!("{base}/tasks/batch"))
        .json(&serde_json::json!({ "action": "pause", "select": {} }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 非法 state 标签 → 400
    let resp = client
        .post(format!("{base}/tasks/batch"))
        .json(&serde_json::json!({
            "action": "pause",
            "select": { "state": "nope" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 空命中集 → 200 空结果（幂等便利）
    let resp = client
        .post(format!("{base}/tasks/batch"))
        .json(&serde_json::json!({
            "action": "resume",
            "select": { "state": "failed" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let outcome: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(outcome["succeeded"], 0);
}

/// E22 Prometheus 指标端点：/metrics 暴露 text/plain 格式任务/速率指标
/// （从 stats() 聚合派生）。
#[tokio::test]
async fn metrics_endpoint_exposes_prometheus_format() {
    let base = serve().await;
    let client = reqwest::Client::new();
    let srv = TestServer::start(common::patterned(1024)).await;
    let id = add_task(&client, &base, &srv.url(), 1).await;
    let _ = id;

    let resp = client.get(format!("{base}/metrics")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ct.starts_with("text/plain"),
        "content-type 应 text/plain: {ct}"
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains("# HELP smart_dl_tasks_total"));
    assert!(body.contains("# TYPE smart_dl_tasks_total gauge"));
    // queued 任务计数（新任务记录态 Queued）
    assert!(
        body.contains("dimension=\"state\",label=\"Queued\""),
        "应含 Queued 状态指标: {body}"
    );
    assert!(body.contains("direction=\"down\""));
    assert!(body.contains("direction=\"up\""));
}
