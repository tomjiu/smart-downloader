//! Bug C 回归：httpdl 落位时目标文件已存在（典型：BT 抢先下完同名文件）。
//!
//! finalize_part 先删 dest 再 rename 落位，保证 .part（本次直链下载内容）
//! 真正落位，不被已存在文件干扰；完成后 .part 与 etag 副文件均清理。

mod common;
mod integration;

use common::{make_http_task_to, wait_terminal};
use integration::http_server::{patterned, sha256_of, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::HttpEngine;

const MB: u64 = 1024 * 1024;

async fn run_with_preexisting_dest(
    name: &str,
    preexisting: Option<&[u8]>,
) -> (tempfile::TempDir, String) {
    let size = 8 * MB;
    let src = patterned(size);
    let expected = sha256_of(&src);
    let srv = HttpTestServer::start(HttpServerConfig {
        size,
        range: true,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    if let Some(content) = preexisting {
        std::fs::write(dir.path().join(name), content).unwrap();
    }
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "bugc",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some(name),
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    (dir, expected)
}

#[tokio::test]
async fn preexisting_dest_same_size_finalizes_over_it() {
    // BT 抢先下完同名文件（大小一致）：httpdl 仍以 .part 落位 → Completed
    let size = 8 * MB;
    let dir = tempfile::tempdir().unwrap();
    // 预先放置与源一致的完整文件（模拟 BT Seeder 抢先完成）
    std::fs::write(dir.path().join("out.bin"), patterned(size)).unwrap();

    let srv = HttpTestServer::start(HttpServerConfig {
        size,
        range: true,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "bugc1",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("out.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);

    let got = std::fs::read(dir.path().join("out.bin")).unwrap();
    assert_eq!(sha256_of(&got), sha256_of(&patterned(size)));
    assert!(
        !dir.path().join("out.bin.part").exists(),
        "完成后 .part 应清理（Bug C：不得残留）"
    );
}

#[tokio::test]
async fn preexisting_dest_wrong_content_overwritten() {
    // 目标已存在但内容错误（同大小）：必须被 .part 真实内容覆盖，不得短路保留旧文件
    let (dir, expected) = run_with_preexisting_dest("out.bin", Some(&[0xAA; 8 * MB as usize])).await;
    let got = std::fs::read(dir.path().join("out.bin")).unwrap();
    assert_eq!(sha256_of(&got), expected, "旧内容不得残留");
    assert!(!dir.path().join("out.bin.part").exists());
}

#[tokio::test]
async fn preexisting_dest_different_size_finalizes() {
    // 目标已存在但大小不同：同样覆盖落位
    let (dir, expected) = run_with_preexisting_dest("out.bin", Some(b"stale-short")).await;
    let got = std::fs::read(dir.path().join("out.bin")).unwrap();
    assert_eq!(sha256_of(&got), expected);
    assert_eq!(got.len(), (8 * MB) as usize);
    assert!(!dir.path().join("out.bin.part").exists());
}

#[tokio::test]
async fn preexisting_dest_zero_len_finalizes() {
    // 边界：目标已存在但为空文件
    let (dir, expected) = run_with_preexisting_dest("out.bin", Some(b"")).await;
    let got = std::fs::read(dir.path().join("out.bin")).unwrap();
    assert_eq!(sha256_of(&got), expected);
}
