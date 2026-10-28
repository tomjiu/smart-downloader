//! R24: 跨源 CORS 支持（静态托管 UI → 远程 daemon 模式）。
//!
//! 背景：UI 的 `dl.daemon.base` 地址覆盖（Settings「daemon 地址」）此前在
//! 浏览器跨源下永不可用——daemon 不发 CORS 头，fetch 一律被浏览器拦截。
//!
//! 放行口径与 tokenless 守卫同构：回环 Origin（任意端口）始终放行 +
//! `extra_allowed_origins` 显式追加；其余 Origin 无 ACAO 头（浏览器拦截）。
//! 预检 OPTIONS 短路在 auth 之前（浏览器预检不携带凭据）。

use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;

async fn serve_plain() -> std::net::SocketAddr {
    let engine = HttpEngine::new(reqwest::Client::new());
    let state = Arc::new(
        DaemonState::new(Arc::new(engine), vec![])
            .with_dest_root(std::env::temp_dir())
            .with_extra_allowed_origins(vec!["http://ui.lan:3000".to_string()]),
    );
    spawn(state).await
}

async fn serve_token(token: &str) -> std::net::SocketAddr {
    let engine = HttpEngine::new(reqwest::Client::new());
    let state = Arc::new(
        DaemonState::new(Arc::new(engine), vec![])
            .with_dest_root(std::env::temp_dir())
            .with_http_token(Some(token.to_string()))
            .with_extra_allowed_origins(vec!["http://ui.lan:3000".to_string()]),
    );
    spawn(state).await
}

async fn spawn(state: Arc<DaemonState>) -> std::net::SocketAddr {
    let app = http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

/// 回环 Origin（跨端口）→ 200 + ACAO 回显 + Vary 补列。
/// 这是 UI 远程模式的主路径（开发态 next dev :3000 → daemon :8787）。
#[tokio::test]
async fn loopback_origin_gets_allow_origin_header() {
    let addr = serve_plain().await;
    let c = reqwest::Client::new();
    let r = c
        .get(format!("http://{addr}/tasks"))
        .header("Origin", "http://127.0.0.1:3000")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.headers().get("access-control-allow-origin").unwrap(),
        "http://127.0.0.1:3000"
    );
    let vary = r.headers().get("vary").and_then(|v| v.to_str().ok());
    assert!(
        vary.map(|v| v.to_ascii_lowercase().contains("origin"))
            .unwrap_or(false),
        "Vary 应补列 Origin（缓存正确性），实际: {vary:?}"
    );
}

/// localhost 变体与 ::1 同样放行（回环名全集）。
#[tokio::test]
async fn localhost_and_ipv6_origins_allowed() {
    let addr = serve_plain().await;
    let c = reqwest::Client::new();
    for o in ["http://localhost:5173", "http://[::1]:8080"] {
        let r = c
            .get(format!("http://{addr}/health"))
            .header("Origin", o)
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{o} 应放行");
        assert_eq!(
            r.headers().get("access-control-allow-origin").unwrap(),
            o,
            "{o} 应回显"
        );
    }
}

/// 非回环未登记 Origin：tokenless 守卫 403 + 无 ACAO（双保险）。
#[tokio::test]
async fn evil_origin_no_cors_and_403() {
    let addr = serve_plain().await;
    let c = reqwest::Client::new();
    let r = c
        .get(format!("http://{addr}/tasks"))
        .header("Origin", "https://evil.com")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    assert!(
        r.headers().get("access-control-allow-origin").is_none(),
        "evil.com 不得有 ACAO 头"
    );
}

/// 无 Origin（同源 GET / CLI）→ 不发 CORS 头（无事发生）。
#[tokio::test]
async fn same_origin_request_has_no_cors_headers() {
    let addr = serve_plain().await;
    let c = reqwest::Client::new();
    let r = c.get(format!("http://{addr}/health")).send().await.unwrap();
    assert!(r.status().is_success());
    assert!(r.headers().get("access-control-allow-origin").is_none());
}

/// 预检 OPTIONS：放行 → 204 + 全套许可头；必须在 auth 之前短路
/// （浏览器预检不携带 Authorization——若先过 auth 会拿 401 被判 CORS 失败）。
#[tokio::test]
async fn preflight_options_short_circuits_before_auth() {
    // token 模式：预检无凭据也必须 204（不是 401）
    let addr = serve_token("tok-xyz").await;
    let c = reqwest::Client::new();
    let r = c
        .request(reqwest::Method::OPTIONS, format!("http://{addr}/tasks"))
        .header("Origin", "http://127.0.0.1:3000")
        .header("Access-Control-Request-Method", "POST")
        .header(
            "Access-Control-Request-Headers",
            "content-type, authorization",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204, "预检应 204 短路（非 401）");
    assert_eq!(
        r.headers().get("access-control-allow-origin").unwrap(),
        "http://127.0.0.1:3000"
    );
    let methods = r.headers().get("access-control-allow-methods").unwrap();
    for m in ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
        assert!(methods.to_str().unwrap().contains(m), "缺方法 {m}");
    }
    let headers = r.headers().get("access-control-allow-headers").unwrap();
    assert!(headers.to_str().unwrap().contains("Authorization"));
    assert!(headers.to_str().unwrap().contains("Content-Type"));
    assert!(r.headers().get("access-control-max-age").is_some());
}

/// 预检（非放行 Origin）→ 204 无 ACAO：浏览器判 CORS 失败，服务端无副作用。
#[tokio::test]
async fn preflight_evil_origin_no_allow_headers() {
    let addr = serve_plain().await;
    let c = reqwest::Client::new();
    let r = c
        .request(reqwest::Method::OPTIONS, format!("http://{addr}/tasks"))
        .header("Origin", "https://evil.com")
        .header("Access-Control-Request-Method", "POST")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    assert!(r.headers().get("access-control-allow-origin").is_none());
}

/// token 模式：跨源请求 401 也带 ACAO——UI 才能读到真实错误（如 token
/// 失效）而非笼统「无法连接」；带 Bearer → 200 + ACAO。
#[tokio::test]
async fn token_mode_401_readable_cross_origin() {
    let addr = serve_token("tok-xyz").await;
    let c = reqwest::Client::new();
    let r = c
        .get(format!("http://{addr}/tasks"))
        .header("Origin", "http://127.0.0.1:3000")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(
        r.headers().get("access-control-allow-origin").unwrap(),
        "http://127.0.0.1:3000",
        "401 响应也应带 ACAO（错误可读）"
    );
    let r = c
        .get(format!("http://{addr}/tasks"))
        .header("Origin", "http://127.0.0.1:3000")
        .header("Authorization", "Bearer tok-xyz")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.headers().get("access-control-allow-origin").unwrap(),
        "http://127.0.0.1:3000"
    );
}

/// extra_allowed_origins 登记的非回环 Origin（token 模式远程 UI 场景）：
/// 带 Bearer → 200 + ACAO；未登记的 evil.com 带 Bearer → 200 但无 ACAO
/// （服务端鉴权通过，浏览器读取仍被拦——正确的分层）。
#[tokio::test]
async fn extra_allowed_origins_remote_ui_scenario() {
    let addr = serve_token("tok-xyz").await;
    let c = reqwest::Client::new();
    let r = c
        .get(format!("http://{addr}/tasks"))
        .header("Origin", "http://ui.lan:3000")
        .header("Authorization", "Bearer tok-xyz")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.headers().get("access-control-allow-origin").unwrap(),
        "http://ui.lan:3000"
    );
    // 大小写卫生：登记侧归一（builder trim+lowercase），放行匹配不敏感；
    // 回显按请求原样（CORS 规范：ACAO 与 Origin 逐字节匹配才被浏览器
    // 接受——真实浏览器发送前已按 fetch 规范小写化，此处验证的是匹配
    // 逻辑而非回显改写）。
    let r = c
        .get(format!("http://{addr}/tasks"))
        .header("Origin", "http://UI.LAN:3000")
        .header("Authorization", "Bearer tok-xyz")
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("http://UI.LAN:3000"),
        "大小写不敏感命中放行；回显保持请求原样"
    );
    let r = c
        .get(format!("http://{addr}/tasks"))
        .header("Origin", "https://evil.com")
        .header("Authorization", "Bearer tok-xyz")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "token 鉴权通过（服务端不拦持有 token 者）");
    assert!(
        r.headers().get("access-control-allow-origin").is_none(),
        "未登记 Origin 不发 ACAO（浏览器侧拦截）"
    );
}

/// SSE 事件流（EventSource）跨源可读：ACAO 必须在流式响应上（EventSource
/// 无法自定义头，token 模式走 ?token= 回退——此处 tokenless 验证头存在）。
#[tokio::test]
async fn sse_stream_has_cors_header() {
    let addr = serve_plain().await;
    let c = reqwest::Client::new();
    let r = c
        .get(format!("http://{addr}/events/stream"))
        .header("Origin", "http://127.0.0.1:3000")
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "SSE 端点应可达");
    assert_eq!(
        r.headers().get("access-control-allow-origin").unwrap(),
        "http://127.0.0.1:3000",
        "EventSource 跨源读取需要 ACAO"
    );
}

/// null Origin（沙箱 iframe）不得放行（与守卫同口径）。
#[tokio::test]
async fn null_origin_rejected() {
    let addr = serve_token("tok-xyz").await;
    let c = reqwest::Client::new();
    let r = c
        .get(format!("http://{addr}/tasks"))
        .header("Origin", "null")
        .header("Authorization", "Bearer tok-xyz")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(
        r.headers().get("access-control-allow-origin").is_none(),
        "null Origin 不发 ACAO"
    );
}
