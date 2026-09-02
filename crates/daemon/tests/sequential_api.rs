//! 任务级顺序下载 API（`POST /tasks/:id/sequential` + add 时 `sequential`
//! 字段）：快照透出、任务级切换、tasks.json 持久化、重启恢复重放。
//!
//! 引擎语义分工：HTTP（本文件覆盖）= 字段改写下轮拾取；BT（feature bt）=
//! sequential flag，由 bt_api 覆盖引擎侧行为；此处 API 面双构建通用。

mod common;

use common::TestServer;
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;

/// serve 变体：可选注入 tasks.json 持久化路径（恢复 e2e 用）。
async fn serve_with_storage(
    store: Option<std::path::PathBuf>,
) -> (std::net::SocketAddr, Arc<DaemonState>) {
    let engine = HttpEngine::new(reqwest::Client::new());
    // V2 白名单根：显式 dest 均落系统临时目录（与 http_api 同口径）
    let mut state = DaemonState::new(Arc::new(engine), vec![]).with_dest_root(std::env::temp_dir());
    if let Some(p) = store {
        state = state.with_storage(p);
    }
    let state = Arc::new(state);
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state)
}

async fn serve() -> (std::net::SocketAddr, Arc<DaemonState>) {
    serve_with_storage(None).await
}

/// 唯一 dest（并行测试防碰撞）。
fn unique_dest() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir()
        .join(format!(
            "seq-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ))
        .to_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn add_with_sequential_exposed_in_snapshot() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let srv = TestServer::start(vec![0x5Au8; 1024]).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": srv.url(),
            "dest": unique_dest(),
            "sequential": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    let snap = client
        .get(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(
        snap["sequential"],
        serde_json::json!(true),
        "add 时 sequential=true 必须透出快照"
    );
}

#[tokio::test]
async fn sequential_endpoint_toggles_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let (addr, _state) = serve_with_storage(Some(store.clone())).await;
    let base = format!("http://{addr}");
    let srv = TestServer::start(vec![0x5Au8; 1024]).await;
    let client = reqwest::Client::new();

    // 默认（缺省 false）：快照无 sequential 字段（skip_serializing_if）
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": srv.url(), "dest": unique_dest() }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 切换 true → 快照透出 + tasks.json 持久化
    let resp = client
        .post(format!("{base}/tasks/{tid}/sequential"))
        .json(&serde_json::json!({ "sequential": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let snap = resp.json::<serde_json::Value>().await.unwrap();
    assert_eq!(snap["sequential"], serde_json::json!(true));

    // autosave 异步落盘 → 轮询 tasks.json
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if let Ok(text) = std::fs::read_to_string(&store) {
            let hit = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| {
                    v.as_array().map(|a| {
                        a.iter()
                            .any(|t| t["task"]["sequential"] == serde_json::json!(true))
                    })
                })
                .unwrap_or(false);
            if hit {
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tasks.json 必须持久化 sequential=true"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // 切回 false → 快照回退（false 不序列化 → 字段缺失）
    let resp = client
        .post(format!("{base}/tasks/{tid}/sequential"))
        .json(&serde_json::json!({ "sequential": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let snap = resp.json::<serde_json::Value>().await.unwrap();
    assert!(
        snap.get("sequential").is_none() || snap["sequential"] == serde_json::json!(false),
        "切回 false 后快照不得再携带 sequential=true"
    );
}

#[tokio::test]
async fn sequential_endpoint_404_unknown_task() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/tasks/t9999/sequential"))
        .json(&serde_json::json!({ "sequential": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn restore_replays_sequential_flag() {
    // 生命周期 e2e：sequential=true 建任务 → tasks.json 落盘 →
    // "重启"（新 state + restore_from）→ 恢复快照仍 sequential=true
    //（恢复重放把 flag 原样下发新引擎，HTTP 引擎字段拾取）。
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let (addr, _state) = serve_with_storage(Some(store.clone())).await;
    let base = format!("http://{addr}");
    let srv = TestServer::start(vec![0x5Au8; 1024]).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": srv.url(),
            "dest": unique_dest(),
            "sequential": true
        }))
        .send()
        .await
        .unwrap();
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 等 tasks.json 落盘
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if let Ok(text) = std::fs::read_to_string(&store) {
            if text.contains("\"sequential\": true") {
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tasks.json 必须落盘 sequential"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // "重启"：新 state（同 storage 根下的 dest 白名单）+ restore_from
    let http2 = HttpEngine::new(reqwest::Client::new());
    let state2 =
        Arc::new(DaemonState::new(Arc::new(http2), vec![]).with_dest_root(std::env::temp_dir()));
    let n = state2.restore_from(&store).await.unwrap();
    assert_eq!(n, 1, "应恢复 1 条任务");
    let snap = state2.task_snapshot(&tid).await.expect("恢复后快照存在");
    assert!(snap.sequential, "恢复重放后任务 sequential 必须保持 true");
}
