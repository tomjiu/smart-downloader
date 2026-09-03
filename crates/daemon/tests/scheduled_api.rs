//! E23 定时/错峰下载 e2e：`POST /tasks` 携带 `start_at_unix`（未来 1s）→
//! 任务停留 Queued 不入引擎（无进度）→ 到点由调度循环激活 → 正常下载完成。
//! 全链路：API add → 调度循环 activate_due_tasks → 引擎下载 → 轮询推进。

mod common;

use common::{patterned, TestServer};
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;
use std::time::Duration;

/// 组装 daemon + 100ms 加速轮询/调度循环（生产为 2s/1s 周期，测试加速收敛）。
async fn serve_with_scheduler() -> (std::net::SocketAddr, Arc<DaemonState>) {
    let engine = HttpEngine::new(reqwest::Client::new());
    let state = DaemonState::new(Arc::new(engine), vec![]).with_dest_root(std::env::temp_dir());
    let state = Arc::new(state);
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let _h =
        smart_dl_daemon::http_events::spawn_http_events(state.clone(), Duration::from_millis(100));
    let st = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = st.activate_due_tasks().await;
        }
    });
    (addr, state)
}

#[tokio::test]
async fn scheduled_task_waits_then_activates_and_completes() {
    let body = patterned(16 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve_with_scheduler().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    // 未来 1 秒启动（unix 秒粒度；调度循环 100ms 周期下 1-2s 内应完成激活+下载）
    let start_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 1;
    let dest = std::env::temp_dir().join(format!("e23-sched-{}", std::process::id()));
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": srv.url(),
            "dest": dest.to_str().unwrap(),
            "name": "scheduled.bin",
            "start_at_unix": start_at,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 入队即查：Queued + start_at_unix 透出 + 引擎零进度（尚未探测/下载）
    let snap = client
        .get(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap();
    assert_eq!(snap.status(), reqwest::StatusCode::OK);
    let snap = snap.json::<serde_json::Value>().await.unwrap();
    assert_eq!(snap["state"], "Queued", "到点前停留 Queued");
    assert_eq!(snap["start_at_unix"], start_at);
    assert_eq!(snap["done"], 0, "未激活不应有进度");
    assert_eq!(snap["total"], 0, "未激活不应有探测到的总大小");

    // 对照列表：字段同样透出
    let list = client
        .get(format!("{base}/tasks?limit=500"))
        .send()
        .await
        .unwrap();
    let arr = list.json::<serde_json::Value>().await.unwrap();
    let item = arr
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task_id"] == tid.as_str())
        .unwrap();
    assert_eq!(item["start_at_unix"], start_at);

    // 到点激活 + 下载完成（宽限 10s：激活 1s + 下载 + 轮询收敛）
    let mut final_state = String::new();
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let snap = client
            .get(format!("{base}/tasks/{tid}"))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        final_state = snap["state"].as_str().unwrap().to_string();
        if final_state == "Completed" {
            break;
        }
    }
    assert_eq!(final_state, "Completed", "定时任务到点后应激活并下载完成");

    // 落盘文件存在（真下载而非跳过）
    let p = dest.join("scheduled.bin");
    assert!(p.exists(), "定时下载应产生落盘文件");
    assert_eq!(std::fs::metadata(&p).unwrap().len(), 16 * 1024);
}

#[tokio::test]
async fn immediate_task_omits_start_at_and_downloads_normally() {
    // 回归锁：不带 start_at_unix 的任务行为完全不变（字段省略 + 正常完成）
    let body = patterned(8 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve_with_scheduler().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let dest = std::env::temp_dir().join(format!("e23-now-{}", std::process::id()));
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": srv.url(),
            "dest": dest.to_str().unwrap(),
            "name": "now.bin",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut final_state = String::new();
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let snap = client
            .get(format!("{base}/tasks/{tid}"))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        final_state = snap["state"].as_str().unwrap().to_string();
        if final_state == "Completed" {
            break;
        }
    }
    assert_eq!(final_state, "Completed");
    // 快照 JSON 无 start_at_unix 键（0 省略 = 非破坏增量）
    let snap = client
        .get(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(
        snap.get("start_at_unix").is_none(),
        "未调度任务的快照不应出现 start_at_unix 噪声字段"
    );
}
