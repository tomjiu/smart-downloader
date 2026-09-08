//! M4c: FTP REST 续传 + 分段策略对齐（P4 账本统一进度真源）。
//! 分段策略与 HTTP 直链对齐后：`<part>.progress` 段账本是唯一续传凭据，
//! 旧「.part 长度前缀续传」语义废弃（预分配后长度恒为 total，不可信）。

#![cfg(feature = "ftp")]

mod common;
mod integration;

use common::{make_ftp_task, wait_terminal};
use integration::ftp_server::{patterned, FtpServerConfig, FtpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::ledger::{self, Ledger};
use smart_dl_httpdl::FtpEngine;

/// 构造账本文件（模拟中断产物）：.part 预分配 + 已完成段写入 + 账本落盘。
fn seed_ledger(
    part: &std::path::Path,
    total: u64,
    min_split: u64,
    done: &[(u64, u64)],
    src: &[u8],
) {
    seed_ledger_fp(part, total, min_split, done, src, None)
}

/// batch7：带指纹版本（last_modified 直写账本，MDTM 指纹回归用）。
fn seed_ledger_fp(
    part: &std::path::Path,
    total: u64,
    min_split: u64,
    done: &[(u64, u64)],
    src: &[u8],
    last_modified: Option<&str>,
) {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(part)
        .unwrap()
        .set_len(total)
        .unwrap();
    // 已完成段真实写入 .part（模拟中断前已落盘的数据）
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .truncate(false)
        .open(part)
        .unwrap();
    use std::io::{Seek, SeekFrom, Write};
    for &(start, end) in done {
        f.seek(SeekFrom::Start(start)).unwrap();
        f.write_all(&src[start as usize..=(end as usize).min(src.len() - 1)])
            .unwrap();
    }
    ledger::save(
        &ledger::ledger_path(part),
        &Ledger {
            version: ledger::LEDGER_VERSION,
            total,
            min_split,
            etag: None,
            last_modified: last_modified.map(|s| s.to_string()),
            done: done.to_vec(),
        },
    );
}

#[tokio::test]
async fn ledger_resume_skips_done_segments() {
    // 账本恢复：前 2/4 段（16KB 粒度 × 64KB 文件）已完成 → REST 只从段 2/3 起点发起
    let size = 64 * 1024;
    let min_split = 16 * 1024u64;
    let src = patterned(size);
    let srv = FtpTestServer::start(FtpServerConfig {
        size,
        content: Some(src.clone()),
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("r.bin.part");
    seed_ledger(
        &part,
        size,
        min_split,
        &[(0, min_split - 1), (min_split, 2 * min_split - 1)],
        &src,
    );

    let engine = FtpEngine::new().with_min_split(min_split);
    let task = make_ftp_task("r1", &srv.url("/r.bin"), dir.path().to_path_buf(), "r.bin");
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);

    let got = std::fs::read(dir.path().join("r.bin")).unwrap();
    assert_eq!(got, src, "账本续传后文件必须完整");
    let mut offsets = srv.rest_offsets.lock().clone();
    offsets.sort();
    // 已完成段（0/16384 起点）不再 RETR；仅缺失段 2/3（32768/49152 起点）重拉
    assert_eq!(
        offsets,
        vec![2 * min_split, 3 * min_split],
        "账本续传必须只拉缺失段，实际 offsets: {offsets:?}"
    );
}

#[tokio::test]
async fn multi_segment_parallel_integrity() {
    // 分段对齐核心断言：min_split 注入 16KB → 64KB 文件 4 段并行，
    // REST 起点集合覆盖全部段（并行领取顺序不定），内容与源逐字节一致。
    let size = 64 * 1024;
    let min_split = 16 * 1024u64;
    let src = patterned(size);
    let srv = FtpTestServer::start(FtpServerConfig {
        size,
        content: Some(src.clone()),
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = FtpEngine::new().with_min_split(min_split);
    let task = make_ftp_task("p1", &srv.url("/p.bin"), dir.path().to_path_buf(), "p.bin");
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);

    let got = std::fs::read(dir.path().join("p.bin")).unwrap();
    assert_eq!(got, src, "并行段下载后文件必须完整");
    let mut offsets = srv.rest_offsets.lock().clone();
    offsets.sort();
    offsets.dedup();
    assert_eq!(
        offsets,
        vec![0, min_split, 2 * min_split, 3 * min_split],
        "4 段必须全部由独立 REST 会话拉取，实际 offsets: {offsets:?}"
    );
}

#[tokio::test]
async fn rest_offset_zero_without_part() {
    // 无 .part 无账本 → 全新下载，REST 偏移 0
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
async fn part_without_ledger_discarded() {
    // 对齐语义：无账本的 .part 残留一律作废（预分配后长度不可信，G1/G2）
    // → 作废重下（REST 0）→ 完整源内容
    let size = 4096;
    let src = patterned(size);
    let srv = FtpTestServer::start(FtpServerConfig {
        size,
        content: Some(src.clone()),
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("b.bin.part");
    std::fs::write(&part, vec![0x5Au8; size as usize + 100]).unwrap();

    let engine = FtpEngine::new();
    let task = make_ftp_task("r3", &srv.url("/b.bin"), dir.path().to_path_buf(), "b.bin");
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    let got = std::fs::read(dir.path().join("b.bin")).unwrap();
    assert_eq!(got, src, "part 超长 → 重下为完整源内容");
    let offsets = srv.rest_offsets.lock().clone();
    assert!(
        offsets.contains(&0),
        "无账本 → 作废重下 REST 0，实际 {offsets:?}"
    );
}

#[tokio::test]
async fn corrupted_ledger_restarts() {
    // 账本损坏（非法 JSON / total 失配 / 段失配）→ 全新计划重下
    let size = 4096;
    let src = patterned(size);
    let srv = FtpTestServer::start(FtpServerConfig {
        size,
        content: Some(src.clone()),
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("c.bin.part");
    std::fs::write(&part, &src[..1024]).unwrap();
    std::fs::write(ledger::ledger_path(&part), "{ not json").unwrap();

    let engine = FtpEngine::new();
    let task = make_ftp_task("r4", &srv.url("/c.bin"), dir.path().to_path_buf(), "c.bin");
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    let got = std::fs::read(dir.path().join("c.bin")).unwrap();
    assert_eq!(got, src, "损坏账本 → 重下为完整源内容");
}

// ==================== batch7：MDTM 远端指纹（G2 类 FTP 残留根治） ====================

/// MDTM 指纹一致 → 账本照常续传（只拉缺失段）。
#[tokio::test]
async fn mdtm_fingerprint_match_resumes() {
    let size = 64 * 1024;
    let min_split = 16 * 1024u64;
    let src = patterned(size);
    let srv = FtpTestServer::start(FtpServerConfig {
        size,
        content: Some(src.clone()),
        mdtm: Some("20260907120000".into()),
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("r.bin.part");
    seed_ledger_fp(
        &part,
        size,
        min_split,
        &[(0, min_split - 1), (min_split, 2 * min_split - 1)],
        &src,
        Some("20260907120000"),
    );

    let engine = FtpEngine::new().with_min_split(min_split);
    let task = make_ftp_task("r1m", &srv.url("/r.bin"), dir.path().to_path_buf(), "r.bin");
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);

    let got = std::fs::read(dir.path().join("r.bin")).unwrap();
    assert_eq!(got, src, "MDTM 一致时账本续传文件必须完整");
    let mut offsets = srv.rest_offsets.lock().clone();
    offsets.sort();
    assert_eq!(
        offsets,
        vec![2 * min_split, 3 * min_split],
        "MDTM 一致必须只拉缺失段，实际 offsets: {offsets:?}"
    );
    // 账本快照必须带上指纹（后续轮次核对凭据）
    let led = ledger::load(&ledger::ledger_path(&part));
    assert!(led.is_none() || led.unwrap().last_modified == Some("20260907120000".into()));
}

/// MDTM 指纹变化（远端文件同长被替换）→ 账本作废整文件重下（G2 静默混合文件根治）。
#[tokio::test]
async fn mdtm_fingerprint_changed_invalidates_ledger() {
    let size = 64 * 1024;
    let min_split = 16 * 1024u64;
    let src = patterned(size);
    let srv = FtpTestServer::start(FtpServerConfig {
        size,
        content: Some(src.clone()),
        mdtm: Some("20991231235959".into()), // 与账本指纹不同
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("r.bin.part");
    seed_ledger_fp(
        &part,
        size,
        min_split,
        &[(0, min_split - 1), (min_split, 2 * min_split - 1)],
        &src,
        Some("20260907120000"), // 旧指纹
    );

    let engine = FtpEngine::new().with_min_split(min_split);
    let task = make_ftp_task("r2m", &srv.url("/r.bin"), dir.path().to_path_buf(), "r.bin");
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);

    let mut offsets = srv.rest_offsets.lock().clone();
    offsets.sort();
    assert_eq!(
        offsets,
        vec![0, min_split, 2 * min_split, 3 * min_split],
        "指纹失配必须全段重拉（作废旧账本），实际 offsets: {offsets:?}"
    );
}

/// 服务器不支持 MDTM（502）+ 账本无指纹（旧档）→ 降级旧 size-only 语义，照常续传。
#[tokio::test]
async fn mdtm_unsupported_falls_back_to_size_only() {
    let size = 64 * 1024;
    let min_split = 16 * 1024u64;
    let src = patterned(size);
    let srv = FtpTestServer::start(FtpServerConfig {
        size,
        content: Some(src.clone()),
        // mdtm: None → 502
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("r.bin.part");
    seed_ledger(
        &part,
        size,
        min_split,
        &[(0, min_split - 1), (min_split, 2 * min_split - 1)],
        &src,
    );

    let engine = FtpEngine::new().with_min_split(min_split);
    let task = make_ftp_task("r3m", &srv.url("/r.bin"), dir.path().to_path_buf(), "r.bin");
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);

    let mut offsets = srv.rest_offsets.lock().clone();
    offsets.sort();
    assert_eq!(
        offsets,
        vec![2 * min_split, 3 * min_split],
        "双 None（账本/探测）必须放行续传，实际 offsets: {offsets:?}"
    );
}

/// 账本有指纹而服务器不再支持 MDTM（宁枉勿纵）：无法确认未变 → 作废重下。
#[tokio::test]
async fn mdtm_fingerprint_disappeared_restarts() {
    let size = 64 * 1024;
    let min_split = 16 * 1024u64;
    let src = patterned(size);
    let srv = FtpTestServer::start(FtpServerConfig {
        size,
        content: Some(src.clone()),
        // mdtm: None → 探测 None
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("r.bin.part");
    seed_ledger_fp(
        &part,
        size,
        min_split,
        &[(0, min_split - 1), (min_split, 2 * min_split - 1)],
        &src,
        Some("20260907120000"), // 账本有指纹
    );

    let engine = FtpEngine::new().with_min_split(min_split);
    let task = make_ftp_task("r4m", &srv.url("/r.bin"), dir.path().to_path_buf(), "r.bin");
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);

    let mut offsets = srv.rest_offsets.lock().clone();
    offsets.sort();
    assert_eq!(
        offsets,
        vec![0, min_split, 2 * min_split, 3 * min_split],
        "指纹消失必须全段重拉（宁枉勿纵），实际 offsets: {offsets:?}"
    );
}
