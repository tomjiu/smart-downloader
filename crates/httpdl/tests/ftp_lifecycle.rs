//! M4c: FtpEngine 生命周期（add/status/pause/resume/remove/peers/read_piece/kind/capabilities）。

#![cfg(feature = "ftp")]

mod common;
mod integration;

use common::{make_ftp_task, wait_terminal};
use integration::ftp_server::FtpServerConfig;
use integration::ftp_server::FtpTestServer;
use smart_dl_core::types::{Capability, DownloadEngine, EngineError, EngineKind, EngineState};
use smart_dl_httpdl::FtpEngine;

#[tokio::test]
async fn add_returns_tid_and_status_metadata() {
    let srv = FtpTestServer::start(FtpServerConfig {
        size: 2048,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = FtpEngine::new();
    let task = make_ftp_task("l1", &srv.url("/m.bin"), dir.path().to_path_buf(), "m.bin");
    let tid = engine.add(&task).await.unwrap();
    assert_eq!(tid, "l1");

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);
    assert!(st.metadata_received, "SIZE 探测后 metadata 应就绪");
    assert_eq!(st.total, 2048);
    assert_eq!(st.total_done, 2048);
}

#[tokio::test]
async fn pause_resume_remove() {
    let srv = FtpTestServer::start(FtpServerConfig {
        size: 2048,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = FtpEngine::new();
    let task = make_ftp_task("l2", &srv.url("/p.bin"), dir.path().to_path_buf(), "p.bin");
    let tid = engine.add(&task).await.unwrap();

    engine.pause(&tid).await.unwrap();
    assert_eq!(
        engine.status(&tid).await.unwrap().state,
        EngineState::Paused
    );
    engine.resume(&tid).await.unwrap();
    wait_terminal(&engine, &tid).await;

    engine.remove(&tid, false).await.unwrap();
    assert!(matches!(
        engine.status(&tid).await,
        Err(EngineError::NotFound)
    ));
}

#[tokio::test]
async fn peers_empty_read_piece_unsupported() {
    // TD-2 Step 3：FtpEngine 骨架曾对 peers 撒谎返回 Ok(vec![])——收缩后
    // BT 语义方法只在 BtEngineExt 上存在，FtpEngine 无该方法（编译期消除
    // 撒谎面），能力询问 bt() → None；update_sources 等生命周期面不受影响。
    let srv = FtpTestServer::start(FtpServerConfig::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = FtpEngine::new();
    let task = make_ftp_task("l3", &srv.url("/q.bin"), dir.path().to_path_buf(), "q.bin");
    let tid = engine.add(&task).await.unwrap();

    let dyn_engine: &dyn DownloadEngine = &engine;
    assert!(dyn_engine.bt().is_none(), "FTP 引擎不得暴露 BT 面");
    // 骨架方法不报错
    engine.update_sources(&tid, vec![]).await.unwrap();
}

#[tokio::test]
async fn kind_and_capabilities() {
    let engine = FtpEngine::new();
    assert_eq!(engine.id(), "ftp");
    assert_eq!(engine.kind(), EngineKind::Ftp);
    let caps = engine.capabilities();
    assert!(caps.contains(&Capability::Ftp));
    assert!(caps.contains(&Capability::FtpResume));
}

#[tokio::test]
async fn status_unknown_task_not_found() {
    let engine = FtpEngine::new();
    assert!(matches!(
        engine.status(&"nope".to_string()).await,
        Err(EngineError::NotFound)
    ));
}
