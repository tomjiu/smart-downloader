//! feature `bt`：magnet 链接 → libtorrent 引擎（BtEngine）端到端。
//! 无该 feature 时整个文件被跳过（编译基线不链接 libtorrent）。

#![cfg(feature = "bt")]

mod common;

use common::TestServer;
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use std::sync::Arc;

use base64::Engine as _;

async fn serve_bt() -> (std::net::SocketAddr, Arc<DaemonState>) {
    let dir = tempfile::tempdir().unwrap();
    let bt = smart_dl_daemon::bt::BtEngine::new(dir.path(), None, 0, 0, false, false, false).unwrap();
    let http = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let state = Arc::new(DaemonState::new(Arc::new(http), vec![]).with_bt(Arc::new(bt)));
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state)
}

const MAGNET: &str = "magnet:?xt=urn:btih:0d2c9c9d5c2d3e8f9a1b2c3d4e5f6a7b8c9d0e1f&dn=test";

#[tokio::test]
async fn magnet_add_creates_bt_task() {
    let (addr, state) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": MAGNET }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED, "magnet 应 201");
    let body: serde_json::Value = resp.json().await.unwrap();
    let tid = body["task_id"].as_str().unwrap().to_string();

    // 快照：engine=bt、source 为 Magnet、进度可读（libtorrent 实时状态）
    let snap: serde_json::Value = client
        .get(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snap["engine"], "bt", "引擎必须标注 bt");
    assert!(snap["source"].as_str().unwrap().contains("Magnet"));
    assert_eq!(snap["task_id"], tid);

    // 列表含该任务
    let list: serde_json::Value = client
        .get(format!("{base}/tasks"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);

    // TaskCreated + StateChanged(Bt) 事件已发布
    let drained = state.hub().drain();
    let events: Vec<&smart_dl_daemon::events::SchedulerEvent> =
        drained.iter().map(|e| &e.event).collect();
    assert!(events.iter().any(|e| matches!(
        e,
        smart_dl_daemon::events::SchedulerEvent::TaskCreated { .. }
    )));
}

#[tokio::test]
async fn same_magnet_deduped_409() {
    let (addr, _state) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let first = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": MAGNET }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::CREATED);

    let second = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": MAGNET }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        second.status(),
        reqwest::StatusCode::CONFLICT,
        "同 btih 必须判重"
    );
}

#[tokio::test]
async fn torrent_file_add_creates_task() {
    // 最小 .torrent（手写 bencode）→ torrent_b64 上传 → 201 + engine=bt
    let mut t = b"d4:infod6:lengthi123e4:name4:test12:piece lengthi16384e6:pieces20:".to_vec();
    t.extend_from_slice(&[0xAB; 20]);
    t.extend_from_slice(b"ee");
    let b64 = base64::engine::general_purpose::STANDARD.encode(&t);

    let (addr, state) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "torrent_b64": b64 }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        ".torrent 应 201"
    );
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    let snap: serde_json::Value = client
        .get(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snap["engine"], "bt", "torrent 任务必须走 BT 引擎");
    assert!(
        snap["source"].as_str().unwrap().contains("TorrentFile"),
        "source 应标注 TorrentFile"
    );

    // 同一 .torrent 重复 → 409（infohash canonical 查重）
    let dup = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "torrent_b64": b64 }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        dup.status(),
        reqwest::StatusCode::CONFLICT,
        "同 infohash 必须判重"
    );

    // 事件已发布（TaskCreated）
    let drained = state.hub().drain();
    let events: Vec<&smart_dl_daemon::events::SchedulerEvent> =
        drained.iter().map(|e| &e.event).collect();
    assert!(events.iter().any(|e| matches!(
        e,
        smart_dl_daemon::events::SchedulerEvent::TaskCreated { .. }
    )));
}

#[tokio::test]
async fn invalid_base64_rejected() {
    let (addr, _state) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "torrent_b64": "!!!not-base64!!!" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn http_task_with_nested_dest_auto_created() {
    // B10：dest 指向不存在目录 → 自动创建 + 201（HTTP 任务 per-task dest 真实生效；
    // BT 引擎 v1 全局落盘不接受自定义 dest，见 bt_task_with_custom_dest_rejected）
    let body = common::patterned(8 * 1024);
    let srv = TestServer::start(body).await;
    let url = srv.url();
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("some/deep/dir");
    let (addr, _state) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": url,
            "dest": nested.to_string_lossy()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "缺失 dest 应自动创建"
    );
    assert!(nested.is_dir(), "dest 目录必须被创建");
}

#[tokio::test]
async fn magnet_and_http_coexist() {
    // 同一 daemon 内 BT + HTTP 任务并存（引擎统一抽象）
    let body = common::patterned(16 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let bt = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": MAGNET }))
        .send()
        .await
        .unwrap();
    assert_eq!(bt.status(), reqwest::StatusCode::CREATED);

    let http = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": srv.url() }))
        .send()
        .await
        .unwrap();
    let http_status = http.status();
    if http_status != reqwest::StatusCode::CREATED {
        eprintln!("HTTP add body: {:?}", http.text().await.unwrap());
        panic!("http add should be 201, got {http_status}");
    }

    let list: serde_json::Value = client
        .get(format!("{base}/tasks"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2, "BT+HTTP 任务并存");
}

#[tokio::test]
async fn magnet_remove_ok() {
    let (addr, _state) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": MAGNET }))
        .send()
        .await
        .unwrap();
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    let r = client
        .delete(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "BT 任务删除应成功");

    let snap = client
        .get(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap();
    assert_eq!(snap.status(), 404, "删除后快照应 404");
}

#[tokio::test]
async fn bt_task_with_custom_dest_rejected() {
    // BT 引擎 v1 全局落盘（serve bt.save_path）：任务级 dest 与全局目录不一致 → 400
    let (addr, _state) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let custom = tempfile::tempdir().unwrap();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": MAGNET,
            "dest": custom.path().to_string_lossy()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "自定义 dest 应被拒绝（诚实约束，避免静默落错目录）"
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains("全局落盘"), "错误信息应说明落盘约束: {body}");
}

#[tokio::test]
async fn readd_same_magnet_after_restart_ok() {
    // 重启续传前提：新 session 同一 save_path 重新 add 同一 magnet → 成功（libtorrent
    // 磁盘检查复用已下载块）。daemon 持久化恢复走的就是这条路径。
    let dir = tempfile::tempdir().unwrap();
    let http = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());

    // 第一次"运行"
    let r1 = {
        let bt = smart_dl_daemon::bt::BtEngine::new(dir.path(), None, 0, 0, false, false, false).unwrap();
        let state = DaemonState::new(Arc::new(http.clone()), vec![]).with_bt(Arc::new(bt));
        state.add_link_task(MAGNET.to_string(), None).await
    };
    assert!(r1.is_ok(), "首次 add 应成功: {:?}", r1.err());

    // "重启"：新 session（同 save_path）重新 add
    let bt2 = smart_dl_daemon::bt::BtEngine::new(dir.path(), None, 0, 0, false, false, false).unwrap();
    let state2 = DaemonState::new(Arc::new(http), vec![]).with_bt(Arc::new(bt2));
    let r2 = state2.add_link_task(MAGNET.to_string(), None).await;
    assert!(
        r2.is_ok(),
        "重启后同 ih 重新 add 应成功（续传前提）: {:?}",
        r2.err()
    );
}
