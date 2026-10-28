//! B2（#214 设置覆盖度）HTTP 调谐面单测：
//! - no_proxy 后缀匹配语义（aria2 `--no-proxy` 对标）
//! - build_tuned_client 的 UA / 超时口径（本地 axum 服务器断言请求头）
//! - 引擎 set_tuning 热改 → 新任务生效（自定义 UA 落到实际请求）
//! - 分段粒度（min_split_mb）→ 新任务账本粒度
//!
//! worker cap（max_connections_per_task）在 download_dynamic 层已是
//! 纯 clamp 逻辑（min(cap, 公式)），由 split_plan/sequential_window 的
//! 既有公式测覆盖；此处聚焦引擎接线（调谐快照 → 请求行为）。

mod common;
mod integration;

use common::{make_http_task_to, wait_terminal};
use integration::http_server::{HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState, HttpEngineExt, HttpTuning};
use smart_dl_httpdl::engine::{build_tuned_client, no_proxy_hit};
use smart_dl_httpdl::HttpEngine;

/// no_proxy 后缀匹配：精确 / 后缀（点边界）/ 前导点等价 / 不命中中段 /
/// 大小写不敏感 / 空条目不匹配。
#[test]
fn no_proxy_matching_semantics() {
    let list = vec!["lan".to_string(), ".corp.example".to_string()];
    assert!(no_proxy_hit("lan", &list), "精确命中");
    assert!(no_proxy_hit("host.lan", &list), "后缀命中（点边界）");
    assert!(no_proxy_hit("a.b.corp.example", &list), "前导点条目命中");
    assert!(no_proxy_hit("HOST.LAN", &list), "大小写不敏感");
    assert!(
        !no_proxy_hit("host.lan.example.com", &list),
        "后缀不得命中中段"
    );
    assert!(!no_proxy_hit("example", &list), "前缀非后缀不命中");
    assert!(!no_proxy_hit("", &list), "空 host 不命中");
    assert!(!no_proxy_hit("x.com", &[]), "空表不命中");
    assert!(!no_proxy_hit("x.com", &["".to_string()]), "空条目不命中");
    assert!(
        no_proxy_hit("x.com", &["  .COM ".to_string()]),
        "条目 trim + 剥前导点 + 大小写归一"
    );
}

/// 调谐 client 的 UA：自定义 UA 精确落到请求头；空 UA 回落产品默认。
#[tokio::test]
async fn tuned_client_sends_custom_user_agent() {
    let hits = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let hits2 = hits.clone();
    let app = axum::Router::new().route(
        "/ua",
        axum::routing::get(move |h: axum::http::HeaderMap| {
            let ua = h
                .get("user-agent")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            hits2.lock().push(ua);
            async move { "x" }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // 自定义 UA
    let t = HttpTuning {
        user_agent: "my-dl-agent/9.9".into(),
        ..HttpTuning::default()
    };
    let c = build_tuned_client(None, None, &t).unwrap();
    c.get(format!("http://{addr}/ua")).send().await.unwrap();
    // 空 UA → 产品默认（含 smart-dl/ 前缀）
    let c2 = build_tuned_client(None, None, &HttpTuning::default()).unwrap();
    c2.get(format!("http://{addr}/ua")).send().await.unwrap();

    let got = hits.lock();
    assert_eq!(got[0], "my-dl-agent/9.9", "自定义 UA 应精确落请求头");
    assert!(
        got[1].starts_with("smart-dl/"),
        "空 UA 应回落产品默认：{}",
        got[1]
    );
}

/// 引擎级 set_tuning 热改：新任务实际请求带新 UA（端到端接线——
/// spawn 路径调谐 client 构建 + 探测/段请求同源）。
#[tokio::test]
async fn engine_set_tuning_applies_to_new_tasks() {
    let uas = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let uas2 = uas.clone();
    let app = axum::Router::new().route(
        "/file",
        axum::routing::get(move |h: axum::http::HeaderMap| {
            let ua = h
                .get("user-agent")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            uas2.lock().push(ua);
            async move { "A".repeat(1024) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    // 热改：自定义 UA
    engine
        .set_tuning(HttpTuning {
            user_agent: "tuned-ua/1.0".into(),
            ..HttpTuning::default()
        })
        .await
        .unwrap();
    assert_eq!(engine.tuning_snapshot().user_agent, "tuned-ua/1.0");

    let task = make_http_task_to(
        "t1",
        &format!("http://{addr}/file"),
        dir.path().to_path_buf(),
        Some("tuned.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    wait_terminal(&engine, &tid).await;
    let st = engine.status(&tid).await.unwrap();
    assert_eq!(st.state, EngineState::Completed, "下载应完成: {st:?}");

    // 全部请求（探测 + 段）都应带新 UA
    let got = uas.lock();
    assert!(!got.is_empty(), "应有请求发生");
    assert!(
        got.iter().all(|u| u == "tuned-ua/1.0"),
        "所有请求应带调谐 UA：{got:?}"
    );
}

/// 分段粒度热改：min_split_mb → 新任务账本粒度（4MB 文件 + 1MB 粒度 =
/// 4 段）。慢速服务器（delay_ms=400）制造检查窗口：完成前轮询账本文件，
/// 断言 min_split = 1048576（调谐值被消费，而非默认 16MB）。
#[tokio::test]
async fn engine_min_split_tuning_shapes_ledger_granularity() {
    let srv = HttpTestServer::start(HttpServerConfig {
        size: 4 * 1024 * 1024,
        range: true,
        patterned_content: true,
        delay_ms: 400,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    engine
        .set_tuning(HttpTuning {
            min_split_mb: 1, // 4MB 文件 → 4 段（默认 16MB → 1 段）
            ..HttpTuning::default()
        })
        .await
        .unwrap();

    let task = make_http_task_to(
        "t2",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("gran.bin"),
    );
    let tid = engine.add(&task).await.unwrap();

    // 慢速窗口内轮询账本（首个段完成后落盘；400ms/请求足够窗口）
    let ledger_path = dir.path().join("gran.bin.part.progress");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let found = loop {
        if let Ok(text) = std::fs::read_to_string(&ledger_path) {
            break text;
        }
        assert!(std::time::Instant::now() < deadline, "账本未在 10s 内出现");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    assert!(
        found.contains("\"min_split\":1048576"),
        "账本粒度应为调谐 1MB：{found}"
    );
    // 收尾：等终态（防引擎后台任务跨测试泄漏句柄）
    let _ = wait_terminal(&engine, &tid).await;
}
