//! E16 全局限速总阀门 e2e：`POST /config/limit` 热改 → 生效值回显 +
//! `GET /config` 快照同步 + `global_limits_changed` 事件广播。
//! 纯查询（双缺省）与无变化 no-op 不产生事件；部分字段缺省 = 沿用当前值。
//! 引擎下发形态（BT 双方向 / HTTP 单方向 / Unsupported 跳过）为 crate 内
//! 单测覆盖（FakeEngine 非导出），见 state.rs `global_limits_tests`。

use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;

/// 组装 daemon（真实 HTTP 引擎；API 表面测试不依赖引擎内部状态）。
async fn spawn_daemon() -> String {
    let engine = HttpEngine::new(reqwest::Client::new());
    let state = DaemonState::new(Arc::new(engine), vec![]).with_config(serde_json::json!({
        "dest_root": "./downloads",
        "max_download_kb_s": 0,
        "max_upload_kb_s": 0,
    }));
    let state = Arc::new(state);
    let app = http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn set_limit_updates_effective_config_and_broadcasts() {
    let base = spawn_daemon().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/config/limit"))
        .json(&serde_json::json!({
            "max_download_kb_s": 2048,
            "max_upload_kb_s": 512,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "POST /config/limit 应成功");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["max_download_kb_s"], 2048);
    assert_eq!(body["max_upload_kb_s"], 512);

    // GET /config 快照两键同步
    let snap: serde_json::Value = client
        .get(format!("{base}/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snap["max_download_kb_s"], 2048, "/config 应反映生效值");
    assert_eq!(snap["max_upload_kb_s"], 512);

    // 事件广播：global_limits_changed
    let events: serde_json::Value = client
        .get(format!("{base}/events?type=global_limits_changed"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let list = events["events"].as_array().expect("events 数组");
    assert_eq!(list.len(), 1, "恰好一条 global_limits_changed 事件");
    assert_eq!(list[0]["event"]["type"], "global_limits_changed");
    assert_eq!(list[0]["event"]["max_download_kb_s"], 2048);
    assert_eq!(list[0]["event"]["max_upload_kb_s"], 512);
}

#[tokio::test]
async fn empty_body_is_pure_query() {
    let base = spawn_daemon().await;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/config/limit"))
        .json(&serde_json::json!({ "max_download_kb_s": 1024 }))
        .send()
        .await
        .unwrap();

    // 双缺省 = 纯查询：返回当前值；type 过滤查询维持 1 条（无新事件）
    let resp = client
        .post(format!("{base}/config/limit"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["max_download_kb_s"], 1024, "纯查询返回当前值");
    assert_eq!(body["max_upload_kb_s"], 0);

    let events: serde_json::Value = client
        .get(format!("{base}/events?type=global_limits_changed"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(events["events"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn same_values_are_noop() {
    let base = spawn_daemon().await;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/config/limit"))
        .json(&serde_json::json!({ "max_download_kb_s": 1024 }))
        .send()
        .await
        .unwrap();

    // 同值重设 = 无变化 no-op：事件不再追加
    let resp = client
        .post(format!("{base}/config/limit"))
        .json(&serde_json::json!({ "max_download_kb_s": 1024 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let events: serde_json::Value = client
        .get(format!("{base}/events?type=global_limits_changed"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(events["events"].as_array().unwrap().len(), 1, "无新事件");
}

#[tokio::test]
async fn partial_field_merges_with_current() {
    let base = spawn_daemon().await;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/config/limit"))
        .json(&serde_json::json!({ "max_download_kb_s": 2048, "max_upload_kb_s": 512 }))
        .send()
        .await
        .unwrap();
    // 只改 up：down 沿用当前值（None = 不调整语义）
    let resp = client
        .post(format!("{base}/config/limit"))
        .json(&serde_json::json!({ "max_upload_kb_s": 256 }))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["max_download_kb_s"], 2048, "缺省方向沿用当前值");
    assert_eq!(body["max_upload_kb_s"], 256);
}

#[tokio::test]
async fn invalid_body_rejected() {
    let base = spawn_daemon().await;
    let client = reqwest::Client::new();
    // 类型错误（string 非 u32）→ Json extractor 拒绝（400/422），不入业务层
    let resp = client
        .post(format!("{base}/config/limit"))
        .json(&serde_json::json!({ "max_download_kb_s": "fast" }))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_client_error(),
        "非法类型应 4xx，实际 {}",
        resp.status()
    );
}
