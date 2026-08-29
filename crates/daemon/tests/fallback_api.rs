//! M6 e2e：BT 任务 → 云兜底（MockProvider 直链 → HttpEngine 传输 → Completed）。
//! 覆盖：暂停前置校验、无 provider 错误、直链成功落盘 + 内容一致。

#![cfg(feature = "bt")]

mod common;

use common::{patterned, TestServer};
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_provider::{MockProvider, ResolvedRemoteFile};
use std::sync::Arc;

const MAGNET: &str =
    "magnet:?xt=urn:btih:0d2c9c9d5c2d3e8f9a1b2c3d4e5f6a7b8c9d0e1f&dn=fallback-test";

async fn serve(
    dest: std::path::PathBuf,
    providers: Vec<Arc<dyn smart_dl_provider::RemoteProvider>>,
) -> (std::net::SocketAddr, Arc<DaemonState>) {
    let bt = smart_dl_daemon::bt::BtEngine::new(&dest, None, 0, 0, false, false, false).unwrap();
    let http = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let state = Arc::new(
        DaemonState::new(Arc::new(http), providers)
            .with_dest_root(dest.clone())
            .with_bt(Arc::new(bt)),
    );
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state)
}

async fn add_magnet(base: &str, client: &reqwest::Client) -> String {
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": MAGNET }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED, "magnet 应 201");
    resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn fallback_transfers_direct_link_and_completes() {
    let size: u64 = 256 * 1024;
    let body = patterned(size);
    let srv = TestServer::start(body.clone()).await;
    let mock = MockProvider::new("mock").with_files(vec![ResolvedRemoteFile {
        rel_path: "out.bin".into(),
        url: srv.url(),
        size,
        etag: None,
        expires_at: None,
    }]);
    let dir = tempfile::tempdir().unwrap();
    let (addr, _state) = serve(dir.path().to_path_buf(), vec![Arc::new(mock)]).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let tid = add_magnet(&base, &client).await;

    // 未暂停先兜底 → 409（串行策略：禁 BT/直链双份占盘）
    let fr = client
        .post(format!("{base}/tasks/{tid}/fallback"))
        .send()
        .await
        .unwrap();
    assert_eq!(fr.status(), reqwest::StatusCode::CONFLICT, "未暂停应 409");
    let fbody: serde_json::Value = fr.json().await.unwrap();
    assert!(
        fbody["error"].as_str().unwrap().contains("先暂停"),
        "应提示先暂停: {fbody}"
    );

    // 暂停 → 兜底成功
    let pr = client
        .post(format!("{base}/tasks/{tid}/pause"))
        .send()
        .await
        .unwrap();
    assert!(pr.status().is_success());
    let fb = client
        .post(format!("{base}/tasks/{tid}/fallback"))
        .send()
        .await
        .unwrap();
    let fbs = fb.status();
    let ftext = fb.text().await.unwrap();
    assert_eq!(fbs, reqwest::StatusCode::OK, "兜底应成功: {ftext}");
    let out: serde_json::Value = serde_json::from_str(&ftext).unwrap();
    assert_eq!(out["status"], "completed");
    assert_eq!(out["provider"], "mock");
    assert_eq!(out["transferred"][0], "out.bin");

    // 任务 Completed + 文件落盘且内容一致
    let snap: serde_json::Value = client
        .get(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snap["state"], "Completed", "{snap}");
    let got = std::fs::read(dir.path().join("out.bin")).unwrap();
    assert_eq!(got.len() as u64, size);
    assert_eq!(got, body, "直链传输内容必须与源一致");
}

#[tokio::test]
async fn fallback_without_providers_errors_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, _state) = serve(dir.path().to_path_buf(), vec![]).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let tid = add_magnet(&base, &client).await;

    let pr = client
        .post(format!("{base}/tasks/{tid}/pause"))
        .send()
        .await
        .unwrap();
    assert!(pr.status().is_success());

    let fb = client
        .post(format!("{base}/tasks/{tid}/fallback"))
        .send()
        .await
        .unwrap();
    assert_eq!(fb.status(), reqwest::StatusCode::CONFLICT);
    let fbody: serde_json::Value = fb.json().await.unwrap();
    assert!(
        fbody["error"].as_str().unwrap().contains("无可用 provider"),
        "应提示无 provider: {fbody}"
    );
}

#[tokio::test]
async fn fallback_skips_disabled_provider_and_uses_next() {
    let size: u64 = 256 * 1024;
    let body = patterned(size);
    let srv = TestServer::start(body.clone()).await;

    // 第一个 provider 禁用（submit 直接失败），第二个正常
    let disabled = MockProvider::new("disabled").disabled();
    let ok = MockProvider::new("ok").with_files(vec![ResolvedRemoteFile {
        rel_path: "out.bin".into(),
        url: srv.url(),
        size,
        etag: None,
        expires_at: None,
    }]);
    let dir = tempfile::tempdir().unwrap();
    let (addr, _state) = serve(dir.path().to_path_buf(), vec![Arc::new(disabled), Arc::new(ok)]).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let tid = add_magnet(&base, &client).await;

    let pr = client
        .post(format!("{base}/tasks/{tid}/pause"))
        .send()
        .await
        .unwrap();
    assert!(pr.status().is_success());

    let fb = client
        .post(format!("{base}/tasks/{tid}/fallback"))
        .send()
        .await
        .unwrap();
    let fbs = fb.status();
    let ftext = fb.text().await.unwrap();
    assert_eq!(fbs, reqwest::StatusCode::OK, "兜底应成功: {ftext}");
    let out: serde_json::Value = serde_json::from_str(&ftext).unwrap();
    assert_eq!(out["status"], "completed");
    assert_eq!(out["provider"], "ok", "应使用第二个可用 provider");
    assert_eq!(out["transferred"][0], "out.bin");

    let got = std::fs::read(dir.path().join("out.bin")).unwrap();
    assert_eq!(got.len() as u64, size);
    assert_eq!(got, body, "直链传输内容必须与源一致");
}

#[tokio::test]
async fn fallback_skips_quota_exhausted_provider_and_uses_next() {
    let size: u64 = 256 * 1024;
    let body = patterned(size);
    let srv = TestServer::start(body.clone()).await;

    // 第一个 provider 配额耗尽（submit 返回 Quota），第二个正常
    let quota_exhausted = MockProvider::new("quota_exhausted").with_quota(0);
    let ok = MockProvider::new("ok").with_files(vec![ResolvedRemoteFile {
        rel_path: "out.bin".into(),
        url: srv.url(),
        size,
        etag: None,
        expires_at: None,
    }]);
    let dir = tempfile::tempdir().unwrap();
    let (addr, _state) = serve(dir.path().to_path_buf(), vec![Arc::new(quota_exhausted), Arc::new(ok)]).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let tid = add_magnet(&base, &client).await;

    let pr = client
        .post(format!("{base}/tasks/{tid}/pause"))
        .send()
        .await
        .unwrap();
    assert!(pr.status().is_success());

    let fb = client
        .post(format!("{base}/tasks/{tid}/fallback"))
        .send()
        .await
        .unwrap();
    let fbs = fb.status();
    let ftext = fb.text().await.unwrap();
    assert_eq!(fbs, reqwest::StatusCode::OK, "兜底应成功: {ftext}");
    let out: serde_json::Value = serde_json::from_str(&ftext).unwrap();
    assert_eq!(out["status"], "completed");
    assert_eq!(out["provider"], "ok", "配额耗尽后应切换到第二个 provider");
    assert_eq!(out["transferred"][0], "out.bin");

    let got = std::fs::read(dir.path().join("out.bin")).unwrap();
    assert_eq!(got.len() as u64, size);
    assert_eq!(got, body, "直链传输内容必须与源一致");
}

#[tokio::test]
async fn fallback_skips_auth_failed_provider_and_uses_next() {
    let size: u64 = 256 * 1024;
    let body = patterned(size);
    let srv = TestServer::start(body.clone()).await;

    // 第一个 provider 未认证（submit 返回 Auth），第二个正常
    let auth_failed = MockProvider::new("auth_failed").unauthenticated();
    let ok = MockProvider::new("ok").with_files(vec![ResolvedRemoteFile {
        rel_path: "out.bin".into(),
        url: srv.url(),
        size,
        etag: None,
        expires_at: None,
    }]);
    let dir = tempfile::tempdir().unwrap();
    let (addr, _state) = serve(dir.path().to_path_buf(), vec![Arc::new(auth_failed), Arc::new(ok)]).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let tid = add_magnet(&base, &client).await;

    let pr = client
        .post(format!("{base}/tasks/{tid}/pause"))
        .send()
        .await
        .unwrap();
    assert!(pr.status().is_success());

    let fb = client
        .post(format!("{base}/tasks/{tid}/fallback"))
        .send()
        .await
        .unwrap();
    let fbs = fb.status();
    let ftext = fb.text().await.unwrap();
    assert_eq!(fbs, reqwest::StatusCode::OK, "兜底应成功: {ftext}");
    let out: serde_json::Value = serde_json::from_str(&ftext).unwrap();
    assert_eq!(out["status"], "completed");
    assert_eq!(out["provider"], "ok", "Auth 失败后应切换到第二个 provider");
    assert_eq!(out["transferred"][0], "out.bin");

    let got = std::fs::read(dir.path().join("out.bin")).unwrap();
    assert_eq!(got.len() as u64, size);
    assert_eq!(got, body, "直链传输内容必须与源一致");
}
