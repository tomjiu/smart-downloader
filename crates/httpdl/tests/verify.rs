//! M4b: ContentIdentity 校验（§14 Q-B5）。
//! sha256 校验失败 → 重下 1 次 → 仍失败 → 降级接受 + 告警；无 sha256 → 不校验。

mod common;
mod integration;

use common::{
    make_http_task_md5, make_http_task_sha1, make_http_task_sha256, make_http_task_to,
    wait_terminal,
};
use integration::http_server::{
    md5_of, patterned, sha1_of, sha256_of, HttpServerConfig, HttpTestServer,
};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::HttpEngine;

const MB: u64 = 1024 * 1024;

#[tokio::test]
async fn matching_sha256_completes_without_warning() {
    let size = MB;
    let src = patterned(size);
    let srv = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_sha256(
        "v1",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        "ok.bin",
        &sha256_of(&src),
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);
    assert!(st.error.is_none(), "校验通过不得告警");
}

#[tokio::test]
async fn sha256_mismatch_redownloads_then_downgrades_with_warning() {
    // server 内容与声明的 sha256 不符 → 重下 1 次 → 仍不符 → 降级接受 + error 告警
    let size = MB;
    let wrong_sha = sha256_of(&vec![0u8; size as usize]); // 故意错误
    let srv = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_sha256(
        "v2",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        "bad.bin",
        &wrong_sha,
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "降级接受仍算完成");
    assert!(
        st.error.as_deref().unwrap_or("").contains("sha256"),
        "必须告警 sha256 不匹配"
    );
    // 重下 1 次语义：verify 至少尝试过（请求数 > 段数）
    assert!(srv.request_count.load(std::sync::atomic::Ordering::SeqCst) > 2);
}

#[tokio::test]
async fn first_bad_then_good_redownload_succeeds() {
    // 第一次内容错（bad_first）→ 重下 → 内容对 → Completed 无告警
    let size = MB;
    let src = patterned(size);
    let good_sha = sha256_of(&src);
    let srv = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        bad_first: 2, // 前 2 次请求（2 段）返回坏内容
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_sha256(
        "v3",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        "re.bin",
        &good_sha,
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);
    assert!(st.error.is_none(), "重下成功不得告警");
    let got = std::fs::read(dir.path().join("re.bin")).unwrap();
    assert_eq!(sha256_of(&got), good_sha);
}

#[tokio::test]
async fn no_sha256_skips_verification() {
    let srv = HttpTestServer::start(HttpServerConfig {
        size: MB,
        patterned_content: true,
        bad_first: 99, // 内容全程坏也不影响（不校验）
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "v4",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("nover.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);
    assert!(st.error.is_none(), "未提供 sha256 → 不校验不告警");
}

// ==================== E25：主源 sha1/md5 校验（与 sha256 互斥，同处置链） ====================

#[tokio::test]
async fn matching_sha1_completes_without_warning() {
    // E25 主例：主源 sha1 校验通过 → Completed 无告警
    let size = MB;
    let src = patterned(size);
    let srv = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_sha1(
        "v-sha1-ok",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        "sha1-ok.bin",
        &sha1_of(&src),
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);
    assert!(st.error.is_none(), "sha1 校验通过不得告警");
    let got = std::fs::read(dir.path().join("sha1-ok.bin")).unwrap();
    assert_eq!(got, src);
}

#[tokio::test]
async fn matching_md5_completes_without_warning() {
    // E25 主例：主源 md5 校验通过 → Completed 无告警
    let size = MB;
    let src = patterned(size);
    let srv = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_md5(
        "v-md5-ok",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        "md5-ok.bin",
        &md5_of(&src),
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);
    assert!(st.error.is_none(), "md5 校验通过不得告警");
}

#[tokio::test]
async fn sha1_mismatch_redownloads_then_downgrades_with_sha1_warning() {
    // E25 处置链同构：主源 sha1 两次校验失败 → 降级接受 + 告警点名 sha1
    let size = MB;
    let wrong_sha1 = sha1_of(&vec![0u8; size as usize]); // 与 patterned 不符
    let srv = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_sha1(
        "v-sha1-bad",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        "sha1-bad.bin",
        &wrong_sha1,
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "降级接受仍算完成");
    assert!(
        st.error.as_deref().unwrap_or("").contains("sha1"),
        "告警必须点名 sha1: {:?}",
        st.error
    );
    assert!(!st.error.as_deref().unwrap_or("").contains("md5"));
}

#[tokio::test]
async fn md5_mismatch_downgrades_with_md5_warning() {
    // E25：主源 md5（非备用源路径）两次校验失败 → 降级接受 + 告警点名 md5
    let size = MB;
    let wrong_md5 = md5_of(&vec![0u8; size as usize]);
    let srv = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_md5(
        "v-md5-bad",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        "md5-bad.bin",
        &wrong_md5,
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "降级接受仍算完成");
    assert!(
        st.error.as_deref().unwrap_or("").contains("md5"),
        "告警必须点名 md5: {:?}",
        st.error
    );
}
