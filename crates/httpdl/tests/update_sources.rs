//! M4b: update_sources 换源（§14）。直链过期（下载中 404）→ 新 URL 继续；
//! 新 ETag 不一致 → 单文件 .part 作废重下。

mod common;
mod integration;

use common::{make_http_task_to, wait_terminal};
use integration::http_server::{patterned, sha256_of, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::HttpEngine;

const MB: u64 = 1024 * 1024;

#[tokio::test]
async fn expired_direct_link_falls_over_to_new_url() {
    // url1 第 2 段 404（直链过期）→ update_sources(url2) → 新 URL 继续 → 完成
    let size = MB;
    let src = patterned(size);
    let expected = sha256_of(&src);
    let old = HttpTestServer::start(HttpServerConfig {
        size,
        fail_ranges: vec![size / 2],
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let fresh = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "s1",
        &old.url("/file"),
        dir.path().to_path_buf(),
        Some("f.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    engine
        .update_sources(&tid, vec![old.url("/file"), fresh.url("/file")])
        .await
        .unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    let got = std::fs::read(dir.path().join("f.bin")).unwrap();
    assert_eq!(sha256_of(&got), expected, "换源后文件必须完整");
}

#[tokio::test]
async fn new_source_etag_mismatch_discards_part_and_redownloads() {
    // url1 内容 A、url2 内容 B（不同 ETag）→ 换源后 .part 作废重下 → 文件 = B
    let size = MB;
    let old = HttpTestServer::start(HttpServerConfig {
        size,
        etag: Some("etag-A"),
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let new = HttpTestServer::start(HttpServerConfig {
        size,
        etag: Some("etag-B"),
        patterned_content: false, // 0x5A 内容 ≠ patterned A
        ..Default::default()
    })
    .await;
    let expected_b = sha256_of(&vec![0x5Au8; size as usize]);

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "s2",
        &old.url("/file"),
        dir.path().to_path_buf(),
        Some("g.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    // 换源为新 URL（ETag 不同）
    engine
        .update_sources(&tid, vec![new.url("/file")])
        .await
        .unwrap();

    // ETag 变化 → 旧内容落位后必须重下为新源内容：
    // wait_terminal 会在旧内容首次 Completed 时返回，故轮询文件直到出现新内容 B
    let out = dir.path().join("g.bin");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Ok(bytes) = std::fs::read(&out) {
            if sha256_of(&bytes) == expected_b {
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "ETag 变化后未出现新源内容"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn update_sources_unknown_task_is_error() {
    let engine = HttpEngine::new(reqwest::Client::new());
    let r = engine
        .update_sources(&"nope".to_string(), vec!["http://x".into()])
        .await;
    assert!(matches!(
        r,
        Err(smart_dl_core::types::EngineError::NotFound)
    ));
}

/// P0-5 语义锁定（整合批）：无指纹源（服务器不发 ETag/Last-Modified）之间
/// 换源**不作废段账本**——「saved 本就无该指纹 → 放行」是兜底换链场景
///（M6 fallback 刷新直链指向同一云文件）的续传生命线：字面执行
/// 「无法证明相同即作废」会令其整档重下。本测试锁死该放行语义，
/// 防后续「修复」静默破坏 fallback 续传。total 核对仍把关内容变化。
#[tokio::test]
async fn no_fingerprint_cross_source_keeps_ledger_semantics() {
    let size = MB;
    // 两源同内容（patterned）、均无 ETag（HttpServerConfig 默认 None）
    let a = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let b = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let expected = sha256_of(&patterned(size));

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "s4",
        &a.url("/file"),
        dir.path().to_path_buf(),
        Some("h.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);

    let requests_on_b_before = b.request_count.load(std::sync::atomic::Ordering::SeqCst);
    // 换源到 B（无指纹 + URL 变化）：不得触发整档重下——
    // 已完成任务无重启迹象（状态不回到 Downloading、B 无段请求增量）
    engine
        .update_sources(&tid, vec![b.url("/file")])
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let st2 = engine.status(&tid).await.unwrap();
    assert_eq!(
        st2.state,
        EngineState::Completed,
        "无指纹换源不得作废已完成任务（fallback 续传生命线）"
    );
    let requests_on_b_after = b.request_count.load(std::sync::atomic::Ordering::SeqCst);
    // B 仅增加 update_sources 的探测请求（1 次），无下载段
    assert!(
        requests_on_b_after <= requests_on_b_before + 1,
        "无指纹换源不得触发段重下（B 请求数 {requests_on_b_before} → {requests_on_b_after}）"
    );
    let got = std::fs::read(dir.path().join("h.bin")).unwrap();
    assert_eq!(sha256_of(&got), expected);
}
