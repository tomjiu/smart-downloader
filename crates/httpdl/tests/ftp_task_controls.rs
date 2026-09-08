//! A3：FTP 任务级限速（set_limits，与 HTTP 引擎同口径）+ 任务级顺序下载
//! （set_sequential / task.sequential → 在飞段窗口收紧）。
//! 语义边界：FTP 单轮下载，set_* 运行中改写 = 配置回显 + 已登记 limiter
//! 热调即时生效；sequential 热改在下一重下轮拾取（实际生效点 add/恢复重放）。

#![cfg(feature = "ftp")]

mod common;
mod integration;

use common::{make_ftp_task, wait_terminal};
use integration::ftp_server::{patterned, FtpServerConfig, FtpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineError, EngineState};
use smart_dl_httpdl::FtpEngine;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn task_limit_up_direction_rejected() {
    let srv = FtpTestServer::start(FtpServerConfig {
        size: 1024,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = FtpEngine::new();
    let task = make_ftp_task(
        "lim-up",
        &srv.url("/u.bin"),
        dir.path().to_path_buf(),
        "u.bin",
    );
    let tid = engine.add(&task).await.unwrap();
    // FTP 单向引擎：up 方向显式拒绝（Other），与 HTTP 引擎同口径
    let err = engine
        .set_limits(&tid, None, Some(10))
        .await
        .expect_err("up 方向必须报错");
    assert!(matches!(err, EngineError::Other(_)), "实际: {err:?}");
    // 不存在任务 → NotFound（先于方向校验之外的路径）
    let err2 = engine
        .set_limits(&"no-such-task".to_string(), Some(64), None)
        .await
        .expect_err("缺失任务必须 NotFound");
    assert!(matches!(err2, EngineError::NotFound), "实际: {err2:?}");
    let _ = wait_terminal(&engine, &tid).await;
}

#[tokio::test]
async fn task_limit_down_applies_hot_and_completes() {
    // add 后立即（下载运行中）设任务级限速 → 登记串联全局的 chained limiter
    //（热调路径首次 insert）→ 再次 set_limits（原地热调路径）→ 两种路径
    // 都不得破坏下载，最终内容一致。
    let size = 64 * 1024;
    let src = patterned(size);
    let srv = FtpTestServer::start(FtpServerConfig {
        size,
        content: Some(src.clone()),
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = FtpEngine::new();
    let task = make_ftp_task(
        "lim-dn",
        &srv.url("/d.bin"),
        dir.path().to_path_buf(),
        "d.bin",
    );
    let tid = engine.add(&task).await.unwrap();
    engine.set_limits(&tid, Some(512), None).await.unwrap();
    engine.set_limits(&tid, Some(1024), None).await.unwrap(); // 热调已登记条目
                                                              // 双 None = no-op 合法
    engine.set_limits(&tid, None, None).await.unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    let got = std::fs::read(dir.path().join("d.bin")).unwrap();
    assert_eq!(got, src, "限速不得影响内容正确性");
}

#[tokio::test]
async fn sequential_multi_segment_completes() {
    // task.sequential=true（add 直读路径）：64KB / min_split 16KB → 多段，
    // 在飞窗口收紧（2）下前缀尽快完整；内容正确性 + 多段 RETR 存在性。
    let size = 64 * 1024;
    let src = patterned(size);
    let srv = FtpTestServer::start(FtpServerConfig {
        size,
        content: Some(src.clone()),
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = FtpEngine::new().with_min_split(16 * 1024);
    let mut task = make_ftp_task(
        "seq1",
        &srv.url("/s.bin"),
        dir.path().to_path_buf(),
        "s.bin",
    );
    task.sequential = true;
    let tid = engine.add(&task).await.unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    let got = std::fs::read(dir.path().join("s.bin")).unwrap();
    assert_eq!(got, src, "顺序窗口不得影响内容正确性");
    assert!(
        srv.retr_count.load(Ordering::SeqCst) >= 4,
        "多段应产生多次 RETR（16KB×4）"
    );
}

#[tokio::test]
async fn set_sequential_echo_and_not_found() {
    let srv = FtpTestServer::start(FtpServerConfig {
        size: 8 * 1024,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = FtpEngine::new();
    let task = make_ftp_task(
        "seq2",
        &srv.url("/e.bin"),
        dir.path().to_path_buf(),
        "e.bin",
    );
    let tid = engine.add(&task).await.unwrap();
    // 运行中热改 = 字段回显（下一轮拾取；FTP 单轮 → 本次下载不变）
    engine.set_sequential(&tid, true).await.unwrap();
    engine.set_sequential(&tid, false).await.unwrap();
    let err = engine
        .set_sequential(&"no-such-task".to_string(), true)
        .await
        .expect_err("缺失任务必须 NotFound");
    assert!(matches!(err, EngineError::NotFound), "实际: {err:?}");

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
}
