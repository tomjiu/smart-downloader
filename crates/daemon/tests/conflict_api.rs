//! E21 文件冲突策略 e2e：显式名任务的目标文件已存在时——
//! `rename` → 自动改名 name(1).bin 落盘（原文件不动）；
//! `skip` → 任务直接 Completed（引擎零参与，原文件不动）；
//! 默认（缺省/overwrite）→ 旧行为照常下载覆盖；
//! 非法策略值 → 400。

mod common;

use common::TestServer;
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;
use std::time::Duration;

async fn serve(dest: std::path::PathBuf) -> String {
    let engine = HttpEngine::new(reqwest::Client::new());
    // V2 白名单：dest 落点即测试目录
    let state = DaemonState::new(Arc::new(engine), vec![]).with_dest_root(dest);
    let state = Arc::new(state);
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
async fn rename_policy_lands_bumped_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.bin"), b"OLD").unwrap();
    let base = serve(dir.path().to_path_buf()).await;
    let client = reqwest::Client::new();
    let srv = TestServer::start(common::patterned(4096)).await;

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": srv.url(),
            "dest": dir.path().to_str().unwrap(),
            "name": "f.bin",
            "conflict_policy": "rename",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let id = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_completed(&client, &base, &id).await;

    // 原文件未被触碰；新文件落在 f(1).bin
    assert_eq!(std::fs::read(dir.path().join("f.bin")).unwrap(), b"OLD");
    assert_eq!(
        std::fs::read(dir.path().join("f(1).bin")).unwrap(),
        common::patterned(4096),
        "下载应落在改名后的候选路径"
    );
}

#[tokio::test]
async fn skip_policy_completes_instantly_and_keeps_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.bin"), b"KEEP-ME").unwrap();
    let base = serve(dir.path().to_path_buf()).await;
    let client = reqwest::Client::new();
    let srv = TestServer::start(common::patterned(4096)).await;

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": srv.url(),
            "dest": dir.path().to_str().unwrap(),
            "name": "f.bin",
            "conflict_policy": "skip",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let id = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 立即（≤2s 内多次轮询首次即应）Completed
    wait_completed(&client, &base, &id).await;
    // 原文件保持原样；不存在 f(1).bin 等副本
    assert_eq!(std::fs::read(dir.path().join("f.bin")).unwrap(), b"KEEP-ME");
    assert!(!dir.path().join("f(1).bin").exists());
}

#[tokio::test]
async fn invalid_policy_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.bin"), b"OLD").unwrap();
    let base = serve(dir.path().to_path_buf()).await;
    let client = reqwest::Client::new();
    let srv = TestServer::start(common::patterned(1024)).await;

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": srv.url(),
            "dest": dir.path().to_str().unwrap(),
            "name": "f.bin",
            "conflict_policy": "overwrite-nothing",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}
