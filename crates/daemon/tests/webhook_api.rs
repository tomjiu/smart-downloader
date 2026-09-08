//! E17 任务完成 Webhook e2e：任务完成 → daemon 向配置 URL POST JSON 通知
//! （payload 含 task_id/name/engine/total_bytes/finished_at_unix）；
//! 未配置 URL 时零投递。走完整链路：API add → 引擎下载 → 轮询推进 Completed
//! → 统一出口 publish_task_completed → 投递。

mod common;

use axum::Router;
use common::{patterned, TestServer};
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;
use std::time::Duration;

/// 本地 Webhook 接收器：POST /hook 记录 body 到共享缓冲。
async fn spawn_receiver() -> (String, Arc<parking_lot::Mutex<Vec<serde_json::Value>>>) {
    let hits: Arc<parking_lot::Mutex<Vec<serde_json::Value>>> =
        Arc::new(parking_lot::Mutex::new(Vec::new()));
    let hits_for_route = hits.clone();
    let app = Router::new().route(
        "/hook",
        axum::routing::post(move |body: String| async move {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                hits_for_route.lock().push(v);
            }
            "ok"
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/hook"), hits)
}

/// 组装 daemon（带 webhook 注入）+ 100ms 加速轮询循环。
async fn serve_with_webhook(url: Option<String>) -> (std::net::SocketAddr, Arc<DaemonState>) {
    let engine = HttpEngine::new(reqwest::Client::new());
    // V2 dest 白名单：测试显式 dest 落系统临时目录 → 注入为白名单根
    let state = DaemonState::new(Arc::new(engine), vec![])
        .with_dest_root(std::env::temp_dir())
        .with_webhook_url(url);
    let state = Arc::new(state);
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let _h =
        smart_dl_daemon::http_events::spawn_http_events(state.clone(), Duration::from_millis(100));
    (addr, state)
}

#[tokio::test]
async fn completion_posts_webhook_payload() {
    let body = patterned(64 * 1024);
    let srv = TestServer::start(body).await;
    let (hook_url, hits) = spawn_receiver().await;
    let (addr, _state) = serve_with_webhook(Some(hook_url)).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let dest = std::env::temp_dir().join(format!("e17-hook-{}", std::process::id()));
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": srv.url(),
            "dest": dest.to_str().unwrap(),
            "name": "hooked.bin",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 等任务完成（轮询循环推进 → Completed 事件 → Webhook 投递）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let snap: serde_json::Value = client
            .get(format!("{base}/tasks/{tid}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if snap["state"] == "Completed" {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "30s 内任务未完成");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 等 Webhook 投递（fire-and-forget，异步到达）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        // 锁作用域收窄：克隆首条后锁外断言（guard 不跨 await，clippy await_holding_lock）
        let first = {
            let got = hits.lock();
            got.first().cloned()
        };
        if let Some(payload) = first {
            assert_eq!(payload["event"], "task_completed");
            assert_eq!(payload["task_id"], tid.as_str());
            assert_eq!(payload["name"], "hooked.bin");
            assert_eq!(payload["engine"], "http");
            assert_eq!(payload["total_bytes"], 64 * 1024);
            assert!(
                payload["finished_at_unix"].as_u64().unwrap_or(0) > 0,
                "finished_at_unix 应为正"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "10s 内未收到 Webhook 投递"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn webhook_disabled_by_default() {
    let body = patterned(8 * 1024);
    let srv = TestServer::start(body).await;
    let (hook_url, hits) = spawn_receiver().await; // 接收器存在但 daemon 未配置
    let _unused = hook_url;
    let (addr, _state) = serve_with_webhook(None).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let dest = std::env::temp_dir().join(format!("e17-off-{}", std::process::id()));
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": srv.url(),
            "dest": dest.to_str().unwrap(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 等任务完成
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let snap: serde_json::Value = client
            .get(format!("{base}/tasks/{tid}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if snap["state"] == "Completed" {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "30s 内任务未完成");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 稍等一拍确认零投递
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(hits.lock().is_empty(), "未配置 webhook_url 时不得投递");
}
