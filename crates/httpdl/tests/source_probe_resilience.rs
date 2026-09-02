//! 源探测韧性回归（update_sources 并发探测 + add 备用源兜底）。
//!
//! 原行为：
//! - `update_sources` 只探测 `urls[0]`，首个候选死 → 整表拒绝（尽管下载循环
//!   本可逐段轮换到其余存活源）；
//! - `add` 主源探测失败 → 任务直接建失败（即使配置了存活备用源）。
//!
//! 新契约：
//! 1. `update_sources` 并发探测全部候选：任一存活 → 安装全表并继续
//!    （etag 决策取输入序首个成功；探测结果播种 mirror 评分，死源沉底）；
//! 2. 全部候选死亡 → Err（拒绝语义保留）；
//! 3. `add` 主源死 + 备用源活 → 以备用源建任务并完成（身份切换与运行时
//!    切备用源同语义：sha256 → None / md5 ← backup_md5）。

mod common;
mod integration;

use common::{make_http_task_backup, make_http_task_sha256, wait_terminal};
use integration::http_server::{md5_of, patterned, sha256_of, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::HttpEngine;
use std::sync::atomic::Ordering;

const MB: u64 = 1024 * 1024;
const SIZE: u64 = 8 * MB;

/// 已关闭端口的 URL（连接即 refused，探测必然快速失败）。
async fn dead_url() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    format!("http://{addr}/file")
}

async fn alive_server(etag: &'static str) -> HttpTestServer {
    HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        range: true,
        patterned_content: true,
        etag: Some(etag),
        ..Default::default()
    })
    .await
}

/// 1. update_sources：首个候选死、后续存活 → 换源成功并完成（原实现整表拒绝）。
#[tokio::test]
async fn update_sources_survives_dead_first_url() {
    let a = alive_server("etag-a").await;
    let b = alive_server("etag-b").await; // 同内容不同 etag → etag_changed → 新代次重下
    let dead = dead_url().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_sha256(
        "us-dead-first",
        &a.url("/file"),
        dir.path().to_path_buf(),
        "resilience.bin",
        &sha256_of(&patterned(SIZE)),
    );
    let tid = engine.add(&task).await.expect("主源存活，add 应成功");

    engine
        .update_sources(&tid, vec![dead, b.url("/file")])
        .await
        .expect("首个候选死亡不得拒绝换源（韧性语义）");
    assert!(
        b.request_count.load(Ordering::SeqCst) >= 1,
        "update_sources 必须已并发探测到存活候选（B）"
    );

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error={:?}", st.error);
    let dest = dir.path().join("resilience.bin");
    assert_eq!(
        sha256_of(&std::fs::read(&dest).unwrap()),
        sha256_of(&patterned(SIZE)),
        "换源后落位内容必须与源一致"
    );
}

/// 2. update_sources：全部候选死亡 → Err（拒绝语义保留）。
#[tokio::test]
async fn update_sources_all_dead_rejects() {
    let a = alive_server("etag-a2").await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_sha256(
        "us-all-dead",
        &a.url("/file"),
        dir.path().to_path_buf(),
        "x.bin",
        &sha256_of(&patterned(SIZE)),
    );
    let tid = engine.add(&task).await.expect("add 应成功");
    let d1 = dead_url().await;
    let d2 = dead_url().await;
    let r = engine.update_sources(&tid, vec![d1, d2]).await;
    assert!(r.is_err(), "全部候选死亡必须拒绝换源");
}

/// 3. add：主源死 + 备用源活 → 以备用源建任务并完成（原实现任务直接建失败）。
#[tokio::test]
async fn add_falls_back_to_alive_backup_when_primary_dead() {
    let b = alive_server("etag-b3").await;
    let dead = dead_url().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_backup(
        "add-backup-fallback",
        &dead,           // 主源死
        &b.url("/file"), // 备用源活
        &md5_of(&patterned(SIZE)),
        dir.path().to_path_buf(),
        "fallback.bin",
        &sha256_of(&patterned(SIZE)), // 主源身份（主源死，不参与校验）
    );
    let tid = engine
        .add(&task)
        .await
        .expect("主死备活 → add 应自动以备用源成功");
    assert!(
        b.request_count.load(Ordering::SeqCst) >= 1,
        "add 必须已探测到备用源"
    );

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error={:?}", st.error);
    let dest = dir.path().join("fallback.bin");
    assert_eq!(
        sha256_of(&std::fs::read(&dest).unwrap()),
        sha256_of(&patterned(SIZE)),
        "备用源路径的落位内容必须与源一致（md5 校验通过）"
    );
}

/// 4. add：主源、备用源均死 → 任务拒绝（双错信息）。
#[tokio::test]
async fn add_all_dead_rejects() {
    let dead1 = dead_url().await;
    let dead2 = dead_url().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_backup(
        "add-all-dead",
        &dead1,
        &dead2,
        &md5_of(&patterned(SIZE)),
        dir.path().to_path_buf(),
        "x.bin",
        &sha256_of(&patterned(SIZE)),
    );
    let r = engine.add(&task).await;
    assert!(r.is_err(), "双源均死必须拒绝建任务");
}
