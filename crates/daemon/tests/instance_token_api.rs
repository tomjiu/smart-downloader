//! #212 桌面壳身份验证探针 e2e：`GET /instance-token/:nonce`。
//! - 启动 nonce 匹配 → 200 `{"ok":true}`（桌面壳就绪判定依据）
//! - 不匹配 / daemon 未设置 nonce / 空串注入 → 404（残留 daemon 与外来
//!   HTTP 服务对本次启动的壳恒 404 → 壳 fail-fast，不再静默连旧实例）
//! - token 模式：探针与其他端点同受 auth_mw 保护（无 Bearer → 401）

use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::net::SocketAddr;
use std::sync::Arc;

async fn spawn_app(state: Arc<DaemonState>) -> SocketAddr {
    let app = http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

fn state_with(instance_token: Option<&str>) -> Arc<DaemonState> {
    Arc::new(
        DaemonState::new(Arc::new(HttpEngine::new(reqwest::Client::new())), vec![])
            .with_dest_root(std::env::temp_dir())
            .with_instance_token(instance_token.map(|s| s.to_string())),
    )
}

async fn get(addr: SocketAddr, path: &str) -> (u16, String) {
    let resp = reqwest::get(format!("http://{addr}{path}")).await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.text().await.unwrap())
}

#[tokio::test]
async fn matching_nonce_returns_200() {
    let addr = spawn_app(state_with(Some("deadbeefcafe0001"))).await;
    let (status, body) = get(addr, "/instance-token/deadbeefcafe0001").await;
    assert_eq!(status, 200, "匹配 nonce 必须 200");
    assert!(body.contains("\"ok\":true"), "body: {body}");
}

#[tokio::test]
async fn mismatched_nonce_returns_404() {
    let addr = spawn_app(state_with(Some("deadbeefcafe0001"))).await;
    // 残留 daemon（旧 nonce）/ 旁观进程视角：任意错误 nonce 恒 404
    let (status, body) = get(addr, "/instance-token/0000000000000000").await;
    assert_eq!(status, 404, "不匹配 nonce 必须 404（壳据此拒绝连接）");
    assert!(body.contains("\"ok\":false"), "body: {body}");
    // 长度不同同样 404（ct 比较的前置长度检查）
    let (status, _) = get(addr, "/instance-token/short").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn no_instance_token_always_404() {
    // 手动 `serve`（无 --instance-token）：探针恒 404 —— 桌面壳不会把
    // 「用户手动启动的常驻 daemon」误认成自己拉起的子进程
    let addr = spawn_app(state_with(None)).await;
    let (status, _) = get(addr, "/instance-token/anything").await;
    assert_eq!(status, 404);
    // 空串注入同样视作未设置（with_instance_token 过滤）——探非空路径：
    // 「/instance-token/」空尾段不匹配 :nonce 路由，nas 构建下会落入
    // nas_proxy fallback，测不到本 handler。
    let addr = spawn_app(state_with(Some(""))).await;
    let (status, _) = get(addr, "/instance-token/x").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn token_mode_probe_requires_bearer() {
    // token 模式：探针受 auth_mw 保护，无 Bearer → 401（桌面场景不配 token；
    // 若配置则壳探测得到 401 → fail-fast，与 UI 无法加载的既成事实一致）
    let state = Arc::new(
        DaemonState::new(Arc::new(HttpEngine::new(reqwest::Client::new())), vec![])
            .with_dest_root(std::env::temp_dir())
            .with_http_token(Some("s3cret".to_string()))
            .with_instance_token(Some("nonce-abc".to_string())),
    );
    let addr = spawn_app(state).await;
    let (status, _) = get(addr, "/instance-token/nonce-abc").await;
    assert_eq!(status, 401, "token 模式探针必须 401（无 Bearer）");
    // 带 Bearer 则放行且匹配判定正常
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/instance-token/nonce-abc"))
        .bearer_auth("s3cret")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
}

#[tokio::test]
async fn nonce_survives_full_serve_style_builder_chain() {
    // serve.rs 实际装配链路上的位置（with_http_token 之后）：nonce 不被
    // 后续 builder 覆盖/丢失
    let state = DaemonState::new(Arc::new(HttpEngine::new(reqwest::Client::new())), vec![])
        .with_dest_root(std::env::temp_dir())
        .with_http_token(None)
        .with_instance_token(Some("chain-nonce-1".to_string()))
        .with_extra_allowed_hosts(vec![])
        .with_config_path(None);
    let addr = spawn_app(Arc::new(state)).await;
    let (status, _) = get(addr, "/instance-token/chain-nonce-1").await;
    assert_eq!(status, 200);
}
