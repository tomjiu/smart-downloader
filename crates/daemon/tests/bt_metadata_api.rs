//! B-1 HTTP API：`POST /bt/metadata`（magnet → .torrent 抓取）。
//! - feature `bt`：400（坏 magnet）/ 408（不可达 infohash 超时）语义；
//!   成功路径（真实 seeder）由 btcore/tests/magnet_metadata.rs 覆盖。
//! - 无 `bt`：端点恒 400（提示编译开关）。

use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use std::sync::Arc;

const FAKE_IH: &str = "0123456789abcdef0123456789abcdef01234567";

/// —— 无 bt 构建：端点恒 400 ——
#[cfg(not(feature = "bt"))]
async fn serve() -> std::net::SocketAddr {
    let http = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let state = Arc::new(DaemonState::new(Arc::new(http), vec![]));
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

/// —— 无 bt 构建：端点恒 400 ——
#[cfg(not(feature = "bt"))]
#[tokio::test]
async fn metadata_endpoint_disabled_without_bt() {
    let addr = serve().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/bt/metadata"))
        .json(&serde_json::json!({ "magnet": format!("magnet:?xt=urn:btih:{FAKE_IH}") }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("BT 引擎"));
}

#[cfg(feature = "bt")]
mod bt_enabled {
    use super::*;

    async fn serve_with_bt() -> std::net::SocketAddr {
        let dir = tempfile::tempdir().unwrap();
        let bt = smart_dl_daemon::bt::BtEngine::new(dir.path(), None, 0, 0, false, false, false)
            .unwrap();
        let http = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
        let state = Arc::new(DaemonState::new(Arc::new(http), vec![]).with_bt(Arc::new(bt)));
        let app = http::router(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    }

    #[tokio::test]
    async fn bad_magnet_is_400() {
        let addr = serve_with_bt().await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/bt/metadata"))
            .json(&serde_json::json!({ "magnet": "magnet:?dn=no-hash" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST, "缺 xt → 400");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["error"].as_str().unwrap().contains("xt"));
    }

    #[tokio::test]
    async fn bad_peer_is_400() {
        let addr = serve_with_bt().await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/bt/metadata"))
            .json(&serde_json::json!({
                "magnet": format!("magnet:?xt=urn:btih:{FAKE_IH}"),
                "peers": ["not-a-sockaddr"],
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST, "peer 解析失败 → 400");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["error"].as_str().unwrap().contains("peers"));
    }

    #[tokio::test]
    async fn unreachable_infohash_times_out_408() {
        // 随机 infohash 无任何来源（无 peer/tracker、DHT 关）→ 必然超时
        let addr = serve_with_bt().await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/bt/metadata"))
            .json(&serde_json::json!({
                "magnet": format!("magnet:?xt=urn:btih:{FAKE_IH}&dn=unreachable"),
                "timeout_s": 5,
                "dht": false,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::REQUEST_TIMEOUT, "超时 → 408");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["error"].as_str().unwrap().contains("超时"));
    }
}
