//! P4 段账本续传集成测试：预置中断现场（.part 预分配全尺寸 + 段账本）→
//! add → 决策 → 恢复/作废 → 文件正确。覆盖：
//! - 账本 + ETag 一致 → 从已完成段之后续传（不再请求已完成段）
//! - ETag 失配 → 作废重下（G2：不再"试探续传"产出混合文件）
//! - 预分配 .part 无账本 → 作废重下（G1：文件长度不可作为进度证据）
//! - .part 超长 → 作废重下
//! - 无 .part → 全量下载

mod common;
mod integration;

use common::{make_http_task_to, wait_terminal};
use integration::http_server::{patterned, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::ledger::{Ledger, LEDGER_VERSION};
use smart_dl_httpdl::HttpEngine;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MS: u64 = 8 * 1024; // 测试粒度 8KB（真实默认 16MB 对小文件不可表达部分完成）

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

/// 预置中断现场：.part 预分配全尺寸（前 `n` 字节 = 真实内容，其余稀疏零），
/// 附带段账本（前 `n` 字节 = `MS` 粒度的整段已完成）。`n` 必须与 `MS` 对齐。
fn preset_interrupted(dir: &Path, name: &str, total: u64, n: u64, etag: Option<&str>) -> PathBuf {
    assert_eq!(n % MS, 0, "n 必须与测试粒度对齐");
    assert!(n <= total);
    let part = dir.join(format!("{name}.part"));
    // 预分配语义（与 download_dynamic 同款）：文件长度恒等于 total
    {
        let f = std::fs::File::create(&part).unwrap();
        f.set_len(total).unwrap();
    }
    {
        let mut f = std::fs::OpenOptions::new().write(true).open(&part).unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.write_all(&patterned(n)).unwrap();
    }
    let mut done = Vec::new();
    let mut s = 0u64;
    while s < n {
        let e = (s + MS).min(total) - 1;
        done.push((s, e));
        s = e + 1;
    }
    let ledger = Ledger {
        version: LEDGER_VERSION,
        total,
        min_split: MS,
        etag: etag.map(str::to_string),
        done,
    };
    std::fs::write(
        dir.join(format!("{name}.part.progress")),
        serde_json::to_vec(&ledger).unwrap(),
    )
    .unwrap();
    part
}

#[tokio::test]
async fn ledger_and_etag_match_resume_after_done_segments() {
    // 账本 + ETag 一致 → 已完成段不再请求，从其后继续 → 文件完整
    let total = 64 * 1024u64;
    let n = 40 * 1024u64; // 5 个 8KB 段已完成
    let srv = HttpTestServer::start(HttpServerConfig {
        size: total,
        range: true,
        etag: Some("etag-r"),
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let part = preset_interrupted(dir.path(), "r.bin", total, n, Some("etag-r"));

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
    assert_eq!(st.total_done, total, "完成后进度应等于总长");

    // 文件完整 + 内容确定性一致
    assert!(!part.exists(), "完成后 .part 应清理");
    let final_bytes = std::fs::read(dir.path().join("r.bin")).unwrap();
    assert_eq!(final_bytes.len() as u64, total);
    assert_eq!(final_bytes, patterned(total), "内容必须完整一致");

    // 续传凭据清理（etag 副文件 + 段账本）
    assert!(!dir.path().join("r.bin.part.etag").exists());
    assert!(!dir.path().join("r.bin.part.progress").exists());

    // 服务器只应收到已完成段之后的 Range（40KB 起），已完成段不重下
    let starts = srv.range_starts.lock().clone();
    assert!(
        starts.contains(&(40 * 1024)),
        "必须从账本已完成段之后续传，实际 Range 起点: {starts:?}"
    );
    assert!(
        !starts.contains(&(8 * 1024)),
        "已完成段不应重新请求，实际 Range 起点: {starts:?}"
    );
}

#[tokio::test]
async fn etag_mismatch_discards_part_and_redownloads() {
    // ETag 变了 = 内容变化证据 → 作废 .part + 账本 → 全量重下（G2 修复）
    let total = 48 * 1024u64;
    let n = 24 * 1024u64;
    let srv = HttpTestServer::start(HttpServerConfig {
        size: total,
        range: true,
        etag: Some("etag-new"),
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    preset_interrupted(dir.path(), "m.bin", total, n, Some("etag-old"));

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
    assert_eq!(st.state, EngineState::Completed, "{st:?}");

    let final_bytes = std::fs::read(dir.path().join("m.bin")).unwrap();
    assert_eq!(final_bytes, patterned(total), "重下内容必须正确");

    // 所有数据面 Range 起点都应为 0（全新下载；探测本身也是 bytes=0-0）
    let starts = srv.range_starts.lock().clone();
    assert!(
        starts.iter().all(|&s| s == 0),
        "ETag 失配必须全量重下（不得出现 {n} 等续传起点），实际: {starts:?}"
    );
}

#[tokio::test]
async fn preallocated_part_without_ledger_restarts() {
    // G1 核心回归：预分配 .part（文件长度 == total、内容全是稀疏零）无账本
    // → 旧语义会把它当"下载完成"直接落位（产出损坏文件）；
    //   新语义：无账本 = 凭据不可信 → 作废重下 → 内容正确。
    let total = 32 * 1024u64;
    let srv = HttpTestServer::start(HttpServerConfig {
        size: total,
        range: true,
        etag: Some("etag-g1"),
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("g1.bin.part");
    {
        let f = std::fs::File::create(&part).unwrap();
        f.set_len(total).unwrap();
    }

    let engine = HttpEngine::new(client());
    let tid = engine
        .add(&make_http_task_to(
            "r3",
            &srv.url("/file"),
            dir.path().to_path_buf(),
            Some("g1.bin"),
        ))
        .await
        .unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "{st:?}");

    let final_bytes = std::fs::read(dir.path().join("g1.bin")).unwrap();
    assert_eq!(
        final_bytes,
        patterned(total),
        "无账本预分配 .part 必须作废重下，产出正确内容（而非稀疏零）"
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
    // 预置 .part 40KB > 32KB（含账本也应因 total 失配被拒）
    let part = preset_interrupted(dir.path(), "b.bin", 40 * 1024, 16 * 1024, Some("etag-s"));

    let engine = HttpEngine::new(client());
    let tid = engine
        .add(&make_http_task_to(
            "r4",
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

    let starts = srv.range_starts.lock().clone();
    assert!(
        !starts.contains(&(16 * 1024)),
        "超长 .part 不应续传: {starts:?}"
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
            "r5",
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
    assert!(!dir.path().join("f.bin.part.progress").exists());
}

#[tokio::test]
async fn tampered_ledger_restarts() {
    // 篡改账本（未对齐段）→ 校验失败 → 作废重下（不信任外部进度声明）
    let total = 32 * 1024u64;
    let srv = HttpTestServer::start(HttpServerConfig {
        size: total,
        range: true,
        etag: Some("etag-t"),
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("t.bin.part");
    {
        let f = std::fs::File::create(&part).unwrap();
        f.set_len(total).unwrap();
    }
    let bad = Ledger {
        version: LEDGER_VERSION,
        total,
        min_split: MS,
        etag: Some("etag-t".to_string()),
        done: vec![(7, 8 * 1024 - 1)], // 未对齐
    };
    std::fs::write(
        dir.path().join("t.bin.part.progress"),
        serde_json::to_vec(&bad).unwrap(),
    )
    .unwrap();

    let engine = HttpEngine::new(client());
    let tid = engine
        .add(&make_http_task_to(
            "r6",
            &srv.url("/file"),
            dir.path().to_path_buf(),
            Some("t.bin"),
        ))
        .await
        .unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "{st:?}");
    let final_bytes = std::fs::read(dir.path().join("t.bin")).unwrap();
    assert_eq!(final_bytes, patterned(total));
}
