//! 校验失败自动轮换 mirror（E3 隔离试错）回归。
//!
//! 契约：
//! 1. 多候选表两次校验失败 → 池播种（评分降序，同分稳定序）→ 逐一以唯一源
//!    身份隔离试错；任一健康候选校验通过 → Completed 无告警，落位 = 健康源内容；
//! 2. 全部候选耗尽 → 降级接受 + 告警（Q-B5 语义保留），且每个候选都被触碰；
//! 3. 多候选表 + 备用源：备用源切换前播种轮换池；备用源也失败 → 隔离试错
//!    接力（备用源 → 候选 → … → 降级），全链路每个源都被触碰。

mod common;
mod integration;

use common::{make_http_task_backup, make_http_task_sha256, wait_terminal};
use integration::http_server::{md5_of, patterned, sha256_of, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::HttpEngine;
use std::sync::atomic::Ordering;

const MB: u64 = 1024 * 1024;
/// 8MB：给 update_sources 探测留出竞态余量（探测必在两次完整重下内完成）。
const SIZE: u64 = 8 * MB;

async fn server_content(content: Vec<u8>) -> HttpTestServer {
    HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        range: true,
        content: Some(content),
        ..Default::default()
    })
    .await
}

fn count(srv: &HttpTestServer) -> usize {
    srv.request_count.load(Ordering::SeqCst)
}

/// 1. 多候选表集体校验失败 → 隔离试错轮换到健康候选 → 完成无告警。
#[tokio::test]
async fn collective_failure_rotates_to_healthy_solo() {
    let good = patterned(SIZE);
    let bad = vec![0u8; SIZE as usize];
    let srv_a = server_content(bad).await; // 坏源（内容与声明 sha256 不符）
    let srv_b = server_content(good.clone()).await; // 健康源

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_sha256(
        "vr1",
        &srv_a.url("/file"),
        dir.path().to_path_buf(),
        "vr1.bin",
        &sha256_of(&good),
    );
    let tid = engine.add(&task).await.unwrap();
    // 安装多候选表（探测需 Range 支持；两源传输层均存活 → 全表安装）
    engine
        .update_sources(&tid, vec![srv_a.url("/file"), srv_b.url("/file")])
        .await
        .unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(
        st.state,
        EngineState::Completed,
        "隔离试错到健康候选必须完成"
    );
    assert!(st.error.is_none(), "健康候选校验通过不得告警");
    let got = std::fs::read(dir.path().join("vr1.bin")).unwrap();
    assert_eq!(sha256_of(&got), sha256_of(&good), "落位应为健康源内容");
    assert!(count(&srv_b) > 0, "健康候选必须被轮换触达");
}

/// 2. 全部候选隔离试错耗尽 → 降级接受 + 告警（Q-B5 保留），每个候选都被触碰。
#[tokio::test]
async fn rotation_pool_exhausts_then_downgrades() {
    let wrong_sha = sha256_of(&patterned(SIZE)); // 与所有源内容都不符
    let srv_a = server_content(vec![0u8; SIZE as usize]).await;
    let srv_b = server_content(vec![0x11u8; SIZE as usize]).await;
    let srv_c = server_content(vec![0x22u8; SIZE as usize]).await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_sha256(
        "vr2",
        &srv_a.url("/file"),
        dir.path().to_path_buf(),
        "vr2.bin",
        &wrong_sha,
    );
    let tid = engine.add(&task).await.unwrap();
    engine
        .update_sources(
            &tid,
            vec![srv_a.url("/file"), srv_b.url("/file"), srv_c.url("/file")],
        )
        .await
        .unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(
        st.state,
        EngineState::Completed,
        "候选耗尽 → 降级接受仍算完成"
    );
    assert!(
        st.error.as_deref().unwrap_or("").contains("sha256"),
        "降级必须告警 sha256 不匹配"
    );
    assert!(
        count(&srv_a) > 0 && count(&srv_b) > 0 && count(&srv_c) > 0,
        "隔离试错必须逐一触碰全部候选"
    );
}

/// 3. 多候选表 + 备用源：备用源也坏 → 备用源切换前播种的轮换池接力隔离试错 → 耗尽降级。
#[tokio::test]
async fn backup_failure_relays_into_rotation_pool() {
    let good = patterned(SIZE);
    let srv_a = server_content(vec![0u8; SIZE as usize]).await;
    let srv_b = server_content(vec![0x11u8; SIZE as usize]).await;
    let srv_bk = server_content(vec![0x33u8; SIZE as usize]).await; // 备用源内容也不符

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_backup(
        "vr3",
        &srv_a.url("/file"),
        &srv_bk.url("/file"),
        &md5_of(&good), // 备用源内容与 backup_md5 不符
        dir.path().to_path_buf(),
        "vr3.bin",
        &sha256_of(&good),
    );
    let tid = engine.add(&task).await.unwrap();
    // 安装多候选表 → 集体校验失败时，备用源切换前播种轮换池
    engine
        .update_sources(&tid, vec![srv_a.url("/file"), srv_b.url("/file")])
        .await
        .unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "全链路耗尽 → 降级接受");
    assert!(
        st.error.as_deref().unwrap_or("").contains("md5"),
        "备用源 md5 校验链耗尽必须告警 md5"
    );
    assert!(
        count(&srv_a) > 0 && count(&srv_b) > 0 && count(&srv_bk) > 0,
        "备用源 + 隔离试错接力必须触碰全部源"
    );
}
