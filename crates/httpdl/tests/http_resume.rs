//! M4a续（#4）：HTTP 断点续传集成——预置 .part（+ .etag 副文件）→ add →
//! 续传决策 → 从偏移继续 → 文件完整；覆盖 ETag 一致续传 / ETag 不一致但服务器
//! 尊重 Range 续传 / .part 超长作废重下。

mod common;
mod integration;

use common::{make_http_task_to, wait_terminal};
use integration::http_server::{patterned, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::HttpEngine;
use std::path::{Path, PathBuf};

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

/// 预置 .part：写入前 `n` 字节（模拟中断的下载；内容 = 服务器 patterned 前缀）。
/// 附带 .etag 副文件（若提供）。
fn preset_part(dir: &Path, name: &str, n: u64, etag: Option<&str>) -> PathBuf {
    let part = dir.join(format!("{name}.part"));
    let bytes: Vec<u8> = patterned(n);
    std::fs::write(&part, &bytes).unwrap();
    if let Some(e) = etag {
        std::fs::write(dir.join(format!("{name}.part.etag")), e).unwrap();
    }
    part
}

#[tokio::test]
async fn part_continues_from_offset_with_matching_etag() {
    // ETag 一致 → 从 .part 偏移续传 → 文件完整 + 服务器收到偏移起点的 Range
    let srv = HttpTestServer::start(HttpServerConfig {
        size: 64 * 1024,
        range: true,
        etag: Some("etag-r"),
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let part = preset_part(dir.path(), "r.bin", 40 * 1024, Some("etag-r"));

    let engine = HttpEngine::new(client());
    let tid = engine
        .add(&make_http_task_to(
            "r1",
            &srv.url("/file"),
            dir.path().to_path_buf(),
            Some("r.bin"),
        ))
        .await
        .unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "续传后应完成: {st:?}");

    // 文件完整 + 内容确定性一致
    assert!(!part.exists(), "完成后 .part 应清理");
    let final_bytes = std::fs::read(dir.path().join("r.bin")).unwrap();
    assert_eq!(final_bytes.len(), 64 * 1024);
    assert_eq!(final_bytes, patterned(64 * 1024), "内容必须完整一致");

    // 续传凭据清理
    assert!(
        !dir.path().join("r.bin.part.etag").exists(),
        "完成后 .etag 副文件应清理"
    );

    // 服务器确实收到了从 40KB 起的 Range（续传位置验证）
    let starts = srv.range_starts.lock().clone();
    assert!(
        starts.contains(&(40 * 1024)),
        "必须从 .part 偏移续传，实际 Range 起点: {starts:?}"
    );
}

#[tokio::test]
async fn etag_mismatch_but_range_respected_still_resumes() {
    // ETag 变了（换源/重生成）但服务器仍尊重 Range → 试探性从偏移续传
    let srv = HttpTestServer::start(HttpServerConfig {
        size: 48 * 1024,
        range: true,
        etag: Some("etag-new"),
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    preset_part(dir.path(), "m.bin", 20 * 1024, Some("etag-old"));

    let engine = HttpEngine::new(client());
    let tid = engine
        .add(&make_http_task_to(
            "r2",
            &srv.url("/file"),
            dir.path().to_path_buf(),
            Some("m.bin"),
        ))
        .await
        .unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(
        st.state,
        EngineState::Completed,
        "range 支持 → 试探续传: {st:?}"
    );

    let final_bytes = std::fs::read(dir.path().join("m.bin")).unwrap();
    assert_eq!(final_bytes.len(), 48 * 1024);
    assert_eq!(final_bytes, patterned(48 * 1024));

    let starts = srv.range_starts.lock().clone();
    assert!(
        starts.contains(&(20 * 1024)),
        "ETag 不一致但 range 支持 → 仍从偏移续，实际: {starts:?}"
    );
}

#[tokio::test]
async fn part_longer_than_file_restarts() {
    // .part 超过文件总长（源变小）→ 作废重下 → 从头下载
    let srv = HttpTestServer::start(HttpServerConfig {
        size: 32 * 1024,
        range: true,
        etag: Some("etag-s"),
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    // 预置 .part 40KB > 32KB
    let part = preset_part(dir.path(), "b.bin", 40 * 1024, Some("etag-s"));

    let engine = HttpEngine::new(client());
    let tid = engine
        .add(&make_http_task_to(
            "r3",
            &srv.url("/file"),
            dir.path().to_path_buf(),
            Some("b.bin"),
        ))
        .await
        .unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "作废重下后应完成: {st:?}");

    let final_bytes = std::fs::read(dir.path().join("b.bin")).unwrap();
    assert_eq!(final_bytes.len(), 32 * 1024);
    assert_eq!(final_bytes, patterned(32 * 1024), "重下内容必须正确");

    // 重下覆盖旧 .part：Range 起点含 0（从头）
    let starts = srv.range_starts.lock().clone();
    assert!(
        starts.contains(&0),
        "作废重下应从 0 开始，实际 Range 起点: {starts:?}"
    );
    assert!(
        !starts.contains(&(40 * 1024)),
        "超长 .part 不应从 40KB 续传: {starts:?}"
    );
    assert!(!part.exists(), "完成后 .part 应清理");
}

#[tokio::test]
async fn fresh_add_without_part_downloads_full() {
    // 无 .part → 全量下载（对照基线：续传逻辑不破坏首次下载）
    let srv = HttpTestServer::start(HttpServerConfig {
        size: 16 * 1024,
        range: true,
        etag: Some("etag-f"),
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();

    let engine = HttpEngine::new(client());
    let tid = engine
        .add(&make_http_task_to(
            "r4",
            &srv.url("/file"),
            dir.path().to_path_buf(),
            Some("f.bin"),
        ))
        .await
        .unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);

    let final_bytes = std::fs::read(dir.path().join("f.bin")).unwrap();
    assert_eq!(final_bytes, patterned(16 * 1024));
    assert!(!dir.path().join("f.bin.part").exists());
}
