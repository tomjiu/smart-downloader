//! UNWIRED #3 e2e：QuarkProvider 装配后 `GET /providers` 必须列出 quark。
//! 覆盖：未登录（authenticated=false）与有效 cookie（authenticated=true）
//! 两种装配形态；未装配（默认关）不出现在列表。

use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_provider::RemoteProvider;
use std::sync::Arc;

async fn serve_with(
    providers: Vec<Arc<dyn RemoteProvider>>,
) -> (std::net::SocketAddr, Arc<DaemonState>) {
    let http = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let state = Arc::new(DaemonState::new(Arc::new(http), providers));
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state)
}

async fn providers_json(base: &str, client: &reqwest::Client) -> serde_json::Value {
    client
        .get(format!("{base}/providers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn quark_provider_listed_when_assembled() {
    // 有效 cookie 文件 → 装配 → /providers 含 quark + authenticated=true
    let dir = tempfile::tempdir().unwrap();
    let cookie = dir.path().join("quark_auth.json");
    std::fs::write(
        &cookie,
        r#"{"cookie":"__pus=demo; __puus=demo","user_id":"42"}"#,
    )
    .unwrap();
    let qp = smart_dl_provider::quark::QuarkProvider::new("quark", cookie);
    let (addr, _state) = serve_with(vec![Arc::new(qp)]).await;

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let list = providers_json(&base, &client).await;
    let quark = list
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["provider"] == "quark")
        .expect("/providers 必须列出 quark（UNWIRED #3 装配断言）");
    assert_eq!(quark["enabled"], true);
    assert_eq!(quark["authenticated"], true, "有效 cookie → 已登录");
}

#[tokio::test]
async fn quark_provider_unauthenticated_without_cookie() {
    // 无 cookie 文件：仍列出（登录态由服务端裁决）但 authenticated=false
    let dir = tempfile::tempdir().unwrap();
    let qp = smart_dl_provider::quark::QuarkProvider::new("quark", dir.path().join("no.json"));
    let (addr, _state) = serve_with(vec![Arc::new(qp)]).await;

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let list = providers_json(&base, &client).await;
    let quark = list
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["provider"] == "quark")
        .expect("未登录也必须在列表（用户可看到需要登录）");
    assert_eq!(quark["authenticated"], false);
}

#[tokio::test]
async fn quark_provider_absent_by_default() {
    // 默认不装配（[provider_quark] 缺省）：/providers 不含 quark
    let (addr, _state) = serve_with(vec![]).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let list = providers_json(&base, &client).await;
    assert!(
        !list
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["provider"] == "quark"),
        "默认关闭时不得出现"
    );
}
