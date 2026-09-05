//! xunlei-import 端到端集成测试（feature `xunlei-import`）：
//! POST /tasks/xunlei-import → DaemonState::add_xunlei_import_task → 201 + task_id
//! 覆盖：合法样本导入、错误 base64、xltd 数量不匹配。

#![cfg(all(test, feature = "xunlei-import"))]

mod common;

use base64::Engine;
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use std::sync::Arc;

fn e2e_sample_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../../tools/xunlei-migrate/e2e_out")
}

fn read_sample(name: &str) -> Vec<u8> {
    let path = e2e_sample_dir().join(name);
    if !path.exists() {
        eprintln!("SKIP: sample {} not found at {:?}", name, path);
        std::process::exit(0);
    }
    std::fs::read(&path).expect("read sample")
}

async fn serve(dest: std::path::PathBuf) -> (std::net::SocketAddr, Arc<DaemonState>) {
    let engine = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let bt = smart_dl_daemon::bt::BtEngine::new(&dest, None, 0, 0, false, false, false)
        .expect("bt engine");
    let state = Arc::new(
        DaemonState::new(Arc::new(engine), vec![])
            .with_dest_root(dest.clone())
            .with_bt(Arc::new(bt)),
    );
    let app =
        http::router(state.clone()).layer(axum::extract::DefaultBodyLimit::max(10 * 1024 * 1024));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state)
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

#[tokio::test]
async fn xunlei_import_creates_bt_task() {
    let _lt = crate::common::lt_gate::LT_SESSION_GATE.lock().await;
    let torrent = read_sample("test.torrent");
    let cfg = read_sample("test.xlbt.cfg");
    let xltd = read_sample("test.bt.xltd");

    let dir = tempfile::tempdir().expect("tempdir");
    let (addr, _state) = serve(dir.path().to_path_buf()).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let payload = serde_json::json!({
        "torrent_b64": b64(&torrent),
        "cfg_b64": b64(&cfg),
        "xltd_b64s": [b64(&xltd)],
        "dest": dir.path().to_str().unwrap(),
    });

    let resp = client
        .post(format!("{base}/tasks/xunlei-import"))
        .json(&payload)
        .send()
        .await
        .unwrap();

    let status = resp.status();
    let text = resp.text().await.unwrap();
    let body: serde_json::Value =
        serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text.clone()));
    if status != reqwest::StatusCode::CREATED {
        eprintln!("xunlei_import response: {status} {body:?}");
    }
    assert_eq!(
        status,
        reqwest::StatusCode::CREATED,
        "import body: {body:?}"
    );
    let tid = body["task_id"].as_str().expect("task_id").to_string();
    assert!(tid.starts_with('t'));

    // 任务快照确认引擎类型（真实 BtEngine 可能立即进入 Downloading）
    let snap: serde_json::Value = client
        .get(format!("{base}/tasks/{}", tid))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        snap["state"] == "Queued" || snap["state"] == "Downloading",
        "初始状态应为 Queued 或 Downloading: {}",
        snap["state"]
    );
    assert_eq!(
        snap.get("engine"),
        Some(serde_json::Value::String("bt".into())).as_ref()
    );
}

#[tokio::test]
async fn xunlei_import_rejects_bad_base64() {
    let _lt = crate::common::lt_gate::LT_SESSION_GATE.lock().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let (addr, _state) = serve(dir.path().to_path_buf()).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let payload = serde_json::json!({
        "torrent_b64": "!!!not-base64!!!",
        "cfg_b64": b64(b"foo"),
        "xltd_b64s": [b64(b"bar")],
    });

    let resp = client
        .post(format!("{base}/tasks/xunlei-import"))
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("base64"));
}

#[tokio::test]
async fn xunlei_import_rejects_xltd_count_mismatch() {
    let _lt = crate::common::lt_gate::LT_SESSION_GATE.lock().await;
    // 构造一个假 torrent（2 文件），但只传 1 个 xltd
    let mut torrent = Vec::new();
    torrent.extend_from_slice(b"d4:infod");
    torrent.extend_from_slice(b"12:piece lengthi16384e");
    torrent.extend_from_slice(b"6:pieces20:");
    torrent.extend_from_slice(&[0u8; 20]);
    torrent.extend_from_slice(b"4:name8:multidir");
    torrent.extend_from_slice(b"5:filesl");
    torrent.extend_from_slice(b"d6:lengthi10e4:pathl1:aee");
    torrent.extend_from_slice(b"d6:lengthi20e4:pathl1:bee");
    torrent.extend_from_slice(b"ee");

    let dir = tempfile::tempdir().expect("tempdir");
    let (addr, _state) = serve(dir.path().to_path_buf()).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let payload = serde_json::json!({
        "torrent_b64": b64(&torrent),
        "cfg_b64": b64(b"dummy cfg"),
        "xltd_b64s": [b64(b"only one xltd")],
    });

    let resp = client
        .post(format!("{base}/tasks/xunlei-import"))
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("不匹配"));
}
