//! M4c: FTP REST 续传（.part 存在 → 从偏移续；REST 偏移记录验证）。

#![cfg(feature = "ftp")]

mod common;
mod integration;

use common::{make_ftp_task, wait_terminal};
use integration::ftp_server::{patterned, FtpServerConfig, FtpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::FtpEngine;

#[tokio::test]
async fn rest_resumes_from_part_offset() {
    // 预置 .part（前 40KB 已下载）→ add → REST 从 40960 续 → 文件完整
    let size = 64 * 1024;
    let src = patterned(size);
    let srv = FtpTestServer::start(FtpServerConfig {
        size,
        content: Some(src.clone()),
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    // 预置 .part：写入前 40KB（模拟中断的下载）
    let part = dir.path().join("r.bin.part");
    std::fs::write(&part, &src[..40 * 1024]).unwrap();

    let engine = FtpEngine::new();
    let task = make_ftp_task("r1", &srv.url("/r.bin"), dir.path().to_path_buf(), "r.bin");
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);

    let got = std::fs::read(dir.path().join("r.bin")).unwrap();
    assert_eq!(got, src, "续传后文件必须完整");
    let offsets = srv.rest_offsets.lock().clone();
    assert!(
        offsets.contains(&(40 * 1024)),
        "必须从 .part 偏移 40960 续传，实际 offsets: {offsets:?}"
    );
}

#[tokio::test]
async fn rest_offset_zero_without_part() {
    // 无 .part → REST 偏移 0
    let size = 4096;
    let src = patterned(size);
    let srv = FtpTestServer::start(FtpServerConfig {
        size,
        content: Some(src),
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = FtpEngine::new();
    let task = make_ftp_task("r2", &srv.url("/z.bin"), dir.path().to_path_buf(), "z.bin");
    let tid = engine.add(&task).await.unwrap();
    wait_terminal(&engine, &tid).await;
    let offsets = srv.rest_offsets.lock().clone();
    assert!(offsets.contains(&0), "无 .part → REST 0，实际 {offsets:?}");
}

#[tokio::test]
async fn part_larger_than_total_restarts() {
    // .part 超过文件总长（源变小了）→ 作废重下
    let size = 4096;
    let src = patterned(size);
    let srv = FtpTestServer::start(FtpServerConfig {
        size,
        content: Some(src.clone()),
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    // .part 比 total 大
    let part = dir.path().join("b.bin.part");
    std::fs::write(&part, vec![0x5Au8; size as usize + 100]).unwrap();

    let engine = FtpEngine::new();
    let task = make_ftp_task("r3", &srv.url("/b.bin"), dir.path().to_path_buf(), "b.bin");
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    let got = std::fs::read(dir.path().join("b.bin")).unwrap();
    assert_eq!(got, src, "part 超长 → 重下为完整源内容");
}
