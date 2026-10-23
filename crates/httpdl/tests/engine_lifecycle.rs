//! M4a: HttpEngine 生命周期（impl DownloadEngine）：add 探测+规划、status/pause/resume/remove、
//! peers Unsupported（TD-2 修复后契约：不再撒谎返回空表）、read_piece Unsupported。

mod common;
mod integration;

use common::make_http_task;
use smart_dl_core::types::{DownloadEngine, EngineError, EngineKind, EngineState};
use smart_dl_httpdl::HttpEngine;

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

#[tokio::test]
async fn add_returns_id_and_status_shows_metadata() {
    let srv = integration::http_server::HttpTestServer::start(
        integration::http_server::HttpServerConfig {
            size: 2048,
            ..Default::default()
        },
    )
    .await;
    let engine = HttpEngine::new(client());
    let task = make_http_task("h1", &srv.url("/file"));

    let tid = engine.add(&task).await.unwrap();
    assert_eq!(tid, "h1");

    let st = engine.status(&tid).await.unwrap();
    assert!(st.metadata_received, "探测后 metadata_received=true");
    assert_eq!(st.total, 2048);
    assert_eq!(st.state, EngineState::Downloading);
}

#[tokio::test]
async fn pause_resume_updates_state() {
    let srv = integration::http_server::HttpTestServer::start(Default::default()).await;
    let engine = HttpEngine::new(client());
    let tid = engine
        .add(&make_http_task("h2", &srv.url("/file")))
        .await
        .unwrap();

    engine.pause(&tid).await.unwrap();
    assert_eq!(
        engine.status(&tid).await.unwrap().state,
        EngineState::Paused
    );

    engine.resume(&tid).await.unwrap();
    assert_eq!(
        engine.status(&tid).await.unwrap().state,
        EngineState::Downloading
    );
}

#[tokio::test]
async fn remove_then_status_not_found() {
    let srv = integration::http_server::HttpTestServer::start(Default::default()).await;
    let engine = HttpEngine::new(client());
    let tid = engine
        .add(&make_http_task("h3", &srv.url("/file")))
        .await
        .unwrap();

    engine.remove(&tid, false).await.unwrap();
    assert!(matches!(
        engine.status(&tid).await,
        Err(EngineError::NotFound)
    ));
}

#[tokio::test]
async fn read_piece_is_unsupported() {
    // TD-2 Step 3 结构化终态：BT 语义方法（peers/read_piece 等）已从
    // DownloadEngine 收缩进 BtEngineExt——HttpEngine 上这些方法不再存在
    // （编译期保证），能力询问 bt() → None 即「不支持」的完整表达。
    // （Step 1 曾以运行时 Err(Unsupported) 修复撒谎；本测试为其回归演进。）
    let srv = integration::http_server::HttpTestServer::start(Default::default()).await;
    let engine = HttpEngine::new(client());
    let tid = engine
        .add(&make_http_task("h4", &srv.url("/file")))
        .await
        .unwrap();
    let dyn_engine: &dyn DownloadEngine = &engine;
    assert!(dyn_engine.bt().is_none(), "HTTP 引擎不得暴露 BT 面");
    // 任务本身可用（收缩不影响生命周期面）
    assert!(engine.status(&tid).await.is_ok());
}

#[tokio::test]
async fn peers_unsupported_for_http() {
    // TD-2（审查建议 5）修复回归 + Step 3 演进：HttpEngine::peers 原返回
    // Ok(vec![])（撒谎而非报错）——Step 1 改 Unsupported，Step 3 起 BT 方法
    // 只在 BtEngineExt 上存在。本测试验证 HTTP 面（HttpEngineExt）可达、
    // BT 面不可达——正反两面一次断言。
    let engine = HttpEngine::new(client());
    let dyn_engine: &dyn DownloadEngine = &engine;
    assert!(dyn_engine.bt().is_none(), "HTTP 引擎不得暴露 BT 面");
    assert!(
        dyn_engine.http().is_some(),
        "HttpEngine 必须暴露 HttpEngineExt（任务级代理等 HTTP 语义入口）"
    );
}

#[tokio::test]
async fn identity_and_capabilities() {
    let engine = HttpEngine::new(client());
    assert_eq!(engine.kind(), EngineKind::Http);
    let caps = engine.capabilities();
    assert!(caps.contains(&smart_dl_core::types::Capability::Http));
    assert!(caps.contains(&smart_dl_core::types::Capability::Range));
    assert!(caps.contains(&smart_dl_core::types::Capability::MultiConnection));
}

#[tokio::test]
async fn add_non_http_source_is_error() {
    let engine = HttpEngine::new(client());
    let task = make_http_task("h6", "http://x");
    // 换成非 Http 源（Ftp）
    let mut t = task;
    t.source = smart_dl_core::types::DownloadSource::Ftp {
        url: "ftp://x/f".into(),
        user: "u".into(),
        pass: "p".into(),
    };
    assert!(engine.add(&t).await.is_err());
}
