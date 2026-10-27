//! 体检 #182 C-2 回归：HTTP 单文件任务的 status().files 透出。
//!
//! 契约：EngineStatus.files 原对 HTTP 恒为空数组——daemon 快照的文件区对
//! HTTP 任务无内容，落盘路径不透出（三方核验只能扫目录）。现 httpdl 在
//! status() 中合成单文件 FileProgress（rel_path = E9 决策链落盘名，
//! done/size 与任务进度同拍），与 BT 多文件 / FTP 目录口径对齐。
//!
//! 断言三路：
//! ① 显式名任务：files[0].rel_path = 显式名，完成态 done == size；
//! ② CD 派生名任务：files[0].rel_path = Content-Disposition 声明名；
//! ③ 未决名（极端兜底）不 panic，files 保持合法结构。

mod common;
mod integration;

use common::{make_http_task_to, wait_terminal};
use integration::http_server::{patterned, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::HttpEngine;

const MB: u64 = 1024 * 1024;

/// ① 显式名：status.files 透出 [rel_path=显式名, done=total=size]。
#[tokio::test]
async fn status_files_exposes_explicit_name_and_progress() {
    let size = 2 * MB;
    let srv = HttpTestServer::start(HttpServerConfig {
        size,
        range: true,
        patterned_content: true,
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "sf1",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("explicit-name.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);

    assert_eq!(
        st.files.len(),
        1,
        "HTTP 单文件必须透出 1 条: {:?}",
        st.files
    );
    let f = &st.files[0];
    assert_eq!(f.rel_path, "explicit-name.bin");
    assert_eq!(f.size, size);
    assert_eq!(f.done, size, "完成态 done 必须 == size");
    // 与 EngineStatus 顶层进度同拍（同一快照源）
    assert_eq!(f.done, st.total_done);
    assert_eq!(f.size, st.total);
}

/// ② CD 派生名：files[0].rel_path = 服务端 Content-Disposition 声明名
/// （E9 决策链：显式名 → CD → URL 末段 → 兜底）。
#[tokio::test]
async fn status_files_exposes_cd_derived_name() {
    let srv = HttpTestServer::start(HttpServerConfig {
        size: MB,
        range: true,
        patterned_content: true,
        content_disposition: Some("attachment; filename=\"cd-name.zip\"".to_string()),
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "sf2",
        &format!("{}?token=abc", srv.url("/file")),
        dir.path().to_path_buf(),
        None,
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);

    assert_eq!(st.files.len(), 1);
    assert_eq!(st.files[0].rel_path, "cd-name.zip");
    assert_eq!(st.files[0].done, MB);
}

/// ③ 一致性：files 透出的 rel_path 与磁盘真实落盘文件一致（三口径闭环）。
#[tokio::test]
async fn status_files_rel_path_matches_disk() {
    let size = MB;
    let _ = patterned(size);
    let srv = HttpTestServer::start(HttpServerConfig {
        size,
        range: true,
        patterned_content: true,
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "sf3",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("disk-check.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);

    let f = &st.files[0];
    let on_disk = dir.path().join(&f.rel_path);
    let meta = std::fs::metadata(&on_disk).unwrap_or_else(|e| panic!("落盘文件缺失 {e}"));
    assert_eq!(meta.len(), f.size, "磁盘大小与 files.size 一致");
}
