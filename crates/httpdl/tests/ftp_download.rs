//! M4c: FTP 下载（PASV 被动模式小文件 / 421 退避重试 / 550 错误）。
//! 目录下载用例见 ftp_directory.rs（任务卡 A）。

#![cfg(feature = "ftp")]

mod common;
mod integration;

use common::{make_ftp_task, wait_terminal};
use integration::ftp_server::{patterned, FtpServerConfig, FtpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineError, EngineState};
use smart_dl_httpdl::retry::Backoff;
use smart_dl_httpdl::FtpEngine;
use std::time::Duration;

#[tokio::test]
async fn pasv_download_small_file_matches_source() {
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
        "f1",
        &srv.url("/data.bin"),
        dir.path().to_path_buf(),
        "out.bin",
    );
    let tid = engine.add(&task).await.unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    let got = std::fs::read(dir.path().join("out.bin")).unwrap();
    assert_eq!(got, src, "PASV 下载内容必须与源一致");
    assert!(
        !dir.path().join("out.bin.part").exists(),
        "完成后 .part 应清理"
    );
    assert!(
        srv.retr_count.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "至少一次 RETR"
    );
}

#[tokio::test]
async fn reject_421_retries_then_succeeds() {
    // 前 2 次控制连接 421 → 退避重试 → 第 3 次成功
    let srv = FtpTestServer::start(FtpServerConfig {
        size: 1024,
        reject_421: 2,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = FtpEngine::with_backoff(smart_dl_httpdl::retry::Backoff {
        base: std::time::Duration::from_millis(10),
        max: std::time::Duration::from_millis(40),
    });
    let task = make_ftp_task("f3", &srv.url("/a.bin"), dir.path().to_path_buf(), "a.bin");
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(
        st.state,
        EngineState::Completed,
        "421 退避后必须成功: {:?}",
        st.error
    );
    let conns = srv
        .control_connections
        .load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        conns >= 3,
        "421 前 2 次 + 成功 1 次 = 至少 3 次连接，实际 {conns}"
    );
}

#[tokio::test]
async fn non_ftp_source_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let engine = FtpEngine::new();
    let task = common::make_http_task_to(
        "f4",
        "http://127.0.0.1:1/x",
        dir.path().to_path_buf(),
        Some("x.bin"),
    );
    let r = engine.add(&task).await;
    assert!(
        matches!(r, Err(EngineError::Other(_))),
        "非 FTP source 必须拒绝"
    );
}

#[tokio::test]
async fn missing_file_550_reports_error() {
    // SIZE 探测成功（add 通过），RETR 时文件不存在（550）→ 终态 Error
    let srv = FtpTestServer::start(FtpServerConfig {
        size: 4096,
        retr_550: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = FtpEngine::new();
    let task = make_ftp_task(
        "f5",
        &srv.url("/ghost.bin"),
        dir.path().to_path_buf(),
        "ghost.bin",
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Error, "550 必须终态失败");
    assert!(st.error.is_some());
}

#[tokio::test]
async fn failed_large_segment_recovers_by_halving() {
    // P1 失败缩小粒度重试（与 HTTP 侧 failed_large_segment_recovers_by_halving 对齐）：
    // REST 起点 16MB 且请求长度 ≥ 8MB → 421（非终态）。32MB 文件默认 16MB 分段 →
    // 整段 [16MB,32MB) 失败 → 拆半收敛：left [16MB,24MB) 仍命中（8MB ≥ 8MB）再拆 →
    // left2 [16MB,20MB) 放行；right2 [20MB,24MB) 与 right [24MB,32MB) 起点不命中放行。
    let mb = 1024 * 1024u64;
    let size = 32 * mb;
    let src = patterned(size);
    let srv = FtpTestServer::start(FtpServerConfig {
        size,
        content: Some(src.clone()),
        fail_ranges: vec![16 * mb],
        // 次数上限 = 两层大段尝试的退避预算（整段 4 次 + left 4 次），
        // 耗尽后 left2 [16MB,20MB) 放行 → 拆分收敛
        fail_ranges_max_hits: Some(8),
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    // 毫秒级退避：421 重试路径真实走连接层退避，但不拖慢测试
    let engine = FtpEngine::with_backoff(Backoff {
        base: Duration::from_millis(10),
        max: Duration::from_millis(20),
    });
    let task = make_ftp_task(
        "f9",
        &srv.url("/data.bin"),
        dir.path().to_path_buf(),
        "out9.bin",
    );
    let tid = engine.add(&task).await.unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(
        st.state,
        EngineState::Completed,
        "缩小粒度重试应完成: {:?}",
        st.error
    );
    let got = std::fs::read(dir.path().join("out9.bin")).unwrap();
    assert_eq!(got, src, "缩小粒度重试后文件必须完整");

    // 拆分过程留痕：整段（16MB）、left 再拆（20MB）、right（24MB）都应出现在 REST 起点里
    let starts = srv.rest_offsets.lock();
    for want in [0u64, 16 * mb, 20 * mb, 24 * mb] {
        assert!(
            starts.contains(&want),
            "REST 起点缺失 {want}，拆分过程未按预期发生（实际 {starts:?}）"
        );
    }
}
