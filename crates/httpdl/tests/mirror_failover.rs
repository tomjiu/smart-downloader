//! M4b: 镜像 failover（§14 镜像轮换）。mirror1 中途 404 → mirror2 接管，文件完整。

mod common;
mod integration;

use common::{make_http_task_to, wait_terminal};
use integration::http_server::{patterned, sha256_of, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::HttpEngine;

const MB: u64 = 1024 * 1024;

#[tokio::test]
async fn mirror1_404_then_mirror2_takes_over() {
    // 核心：段在 mirror1 上 404 → mirror2 补上 → 文件完整
    // 32MB → 动态分段 {0,16MB} 两段；第 2 段起点 16MB 在 mirror1 404
    let size = 32 * MB;
    let src = patterned(size);
    let expected = sha256_of(&src);

    // mirror1：第 2 段起点（16MB）404
    let m1 = HttpTestServer::start(HttpServerConfig {
        size,
        fail_ranges: vec![16 * MB],
        patterned_content: true,
        ..Default::default()
    })
    .await;
    // mirror2：全好
    let m2 = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "mir1",
        &m1.url("/file"),
        dir.path().to_path_buf(),
        Some("o.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    engine
        .update_sources(&tid, vec![m1.url("/file"), m2.url("/file")])
        .await
        .unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    let got = std::fs::read(dir.path().join("o.bin")).unwrap();
    assert_eq!(sha256_of(&got), expected, "镜像接管后文件必须完整");
}

#[tokio::test]
async fn healthy_mirror1_never_uses_mirror2() {
    let size = MB;
    let m1 = HttpTestServer::start(HttpServerConfig {
        size,
        ..Default::default()
    })
    .await;
    let m2 = HttpTestServer::start(HttpServerConfig {
        size,
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "mir2",
        &m1.url("/file"),
        dir.path().to_path_buf(),
        Some("o2.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    engine
        .update_sources(&tid, vec![m1.url("/file"), m2.url("/file")])
        .await
        .unwrap();
    wait_terminal(&engine, &tid).await;

    let m2_requests = m2.request_count.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(m2_requests, 0, "mirror1 全好时不应触碰 mirror2");
}

#[tokio::test]
async fn all_mirrors_dead_reports_error() {
    // 32MB → 动态分段 {0,16MB}；两源对第 2 段起点 16MB 都 404
    // （probe 走起点 0，不受影响）→ 段全源失败 → 整体 Error（不做部分成功利用）
    let size = 32 * MB;
    let m1 = HttpTestServer::start(HttpServerConfig {
        size,
        fail_ranges: vec![16 * MB],
        ..Default::default()
    })
    .await;
    let m2 = HttpTestServer::start(HttpServerConfig {
        size,
        fail_ranges: vec![16 * MB],
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "mir3",
        &m1.url("/file"),
        dir.path().to_path_buf(),
        Some("o3.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    engine
        .update_sources(&tid, vec![m1.url("/file"), m2.url("/file")])
        .await
        .unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Error, "全部 mirror 失败 → Error");
    assert!(st.error.is_some());
}
