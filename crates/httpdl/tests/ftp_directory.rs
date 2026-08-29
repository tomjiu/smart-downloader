//! 任务卡 A：FTP 目录下载（LIST → 逐文件串行下载 → dest_root/<目录名>/ 落盘）。

#![cfg(feature = "ftp")]

mod common;
mod integration;

use common::{make_ftp_task, wait_terminal};
use integration::ftp_server::{patterned, FtpServerConfig, FtpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::FtpEngine;

/// 目录下载：2 个文件按 dest_root/<目录名>/ 落盘且内容正确；
/// 子目录被 LIST 解析过滤；.part 清理；文件级进度（files/total/total_done）齐全。
#[tokio::test]
async fn directory_download_lands_files_under_dir_name() {
    let a = patterned(1024);
    let b = patterned(4096);
    let srv = FtpTestServer::start(FtpServerConfig {
        files: vec![
            ("/pub/files/a.bin".to_string(), a.clone()),
            ("/pub/files/b.bin".to_string(), b.clone()),
        ],
        list_subdirs: vec!["subdir".to_string()],
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = FtpEngine::new();
    // URL 以 / 结尾 → 目录（metadata.name 在目录任务中不参与落位）
    let task = make_ftp_task("d1", &srv.url("/pub/files/"), dir.path().to_path_buf(), "x");
    let tid = engine.add(&task).await.unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);

    // 落位：dest_root/files/<文件名>，内容一致
    let root = dir.path().join("files");
    assert_eq!(std::fs::read(root.join("a.bin")).unwrap(), a, "a.bin 内容必须一致");
    assert_eq!(std::fs::read(root.join("b.bin")).unwrap(), b, "b.bin 内容必须一致");
    // 子目录被过滤 → 不落盘；.part 完成后清理
    assert!(!root.join("subdir").exists(), "子目录不得落盘");
    assert!(!root.join("a.bin.part").exists(), "完成后 .part 应清理");
    assert!(!root.join("b.bin.part").exists(), "完成后 .part 应清理");

    // 文件级进度：total = 各文件 size 之和；files 覆盖两个文件且 done == size
    assert_eq!(st.total, 1024 + 4096);
    assert_eq!(st.total_done, 1024 + 4096);
    let mut fps = st.files;
    fps.sort_by(|x, y| x.rel_path.cmp(&y.rel_path));
    assert_eq!(fps.len(), 2, "必须上报 2 个文件进度");
    assert_eq!(fps[0].rel_path, "a.bin");
    assert_eq!((fps[0].done, fps[0].size), (1024, 1024));
    assert_eq!(fps[1].rel_path, "b.bin");
    assert_eq!((fps[1].done, fps[1].size), (4096, 4096));
}

/// 根目录 URL（ftp://host/）→ 落位 dest_root/<host 净化名>/。
#[tokio::test]
async fn root_directory_lands_under_host_name() {
    let a = patterned(512);
    let srv = FtpTestServer::start(FtpServerConfig {
        files: vec![("/a.bin".to_string(), a.clone())],
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = FtpEngine::new();
    let task = make_ftp_task("d2", &srv.url("/"), dir.path().to_path_buf(), "x");
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    // 根目录无名称 → 落位目录名取 host（已解析出端口，纯主机名）
    let host_dir = dir.path().join("127.0.0.1");
    assert_eq!(std::fs::read(host_dir.join("a.bin")).unwrap(), a);
}

/// 空目录（LIST 无普通文件行）→ add 报错（无可下载内容）。
#[tokio::test]
async fn empty_directory_fails_add() {
    let srv = FtpTestServer::start(FtpServerConfig {
        files: vec![],
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = FtpEngine::new();
    let task = make_ftp_task("d3", &srv.url("/empty/"), dir.path().to_path_buf(), "x");
    let r = engine.add(&task).await;
    assert!(r.is_err(), "空目录 add 必须失败");
}
