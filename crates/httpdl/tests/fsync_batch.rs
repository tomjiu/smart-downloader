//! fsync 批量落盘策略（fsync 性能开关，PR #114 审查 §3.3 跟进项）契约测试。
//!
//! 契约（SegmentSyncPolicy）：
//! 1. **默认每段 fsync**（P0-4 WAL 语义不变）：每个段完成时 `.progress`
//!    账本立即落盘——首个进度回调即可观测账本存在。
//! 2. **批量模式推迟落盘**：`batch_segments`/`batch_interval` 均未达标时，
//!    段完成不触发 fsync 与账本落盘（进度回调时刻账本必须不存在）。
//! 3. **段阈值触发**：累计 N 段即冲刷（N=1 等价每段模式）。
//! 4. **间隔阈值触发**：T 毫秒内有段完成即冲刷（慢速服务器拉开时间轴，
//!    中途可观测账本出现）。
//! 5. **收尾冲刷**：Completed/Paused 出口统一冲脏批——结束后账本完整
//!    覆盖 [0, total)，内容逐字节一致。
//! 6. **暂停→恢复闭环**：批量模式暂停退出后账本完整，恢复跳过已完成段
//!    并最终内容正确（WAL 粒度放宽后的 resume 语义）。
//! 7. **引擎接线**：`with_sync_policy` 注入的策略真实传导到下载循环。
//!
//! 时序口径：测试 4 用 150ms/请求慢速服务器 + 8 段/2 worker = 最少 4 轮
//! ≥600ms 总时长，300ms 间隔在第 3 轮前必然超时（CI 变慢只会让间隔更早
//! 超时，断言方向安全）。

mod common;
mod integration;

use common::{make_http_task_to, wait_terminal};
use integration::http_server::{patterned, sha256_of, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::download::{
    download_dynamic, DynamicLedger, DynamicOutcome, SegmentSyncPolicy,
};
use smart_dl_httpdl::rate::RateLimiter;
use smart_dl_httpdl::HttpEngine;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

const MB: u64 = 1024 * 1024;
/// 8MB / 1MB 段 = 8 段；worker 数 = clamp(8MB/64MB, 2, 8) = 2。
const SIZE: u64 = 8 * MB;
const MIN_SPLIT: u64 = MB;

fn batch_policy(segments: u32, interval_ms: u64) -> SegmentSyncPolicy {
    SegmentSyncPolicy {
        per_segment: false,
        batch_segments: segments,
        batch_interval: std::time::Duration::from_millis(interval_ms),
    }
}

fn ledger_handle(part: &std::path::Path) -> DynamicLedger {
    DynamicLedger {
        path: smart_dl_httpdl::ledger::ledger_path(part),
        etag: Some("etag-1".to_string()),
        last_modified: None,
        min_split: MIN_SPLIT,
        done: vec![],
    }
}

fn ledger_exists(part: &std::path::Path) -> bool {
    smart_dl_httpdl::ledger::ledger_path(part).exists()
}

/// 断言账本 done 区间完整覆盖 [0, total)。
fn assert_ledger_complete(part: &std::path::Path, total: u64) {
    let l = smart_dl_httpdl::ledger::load(&smart_dl_httpdl::ledger::ledger_path(part))
        .expect("完成后账本必须存在且合法");
    assert_eq!(l.total, total);
    let mut covered: u64 = 0;
    for (s, e) in &l.done {
        assert_eq!(*s, covered, "账本区间必须从 {covered} 起（无空洞/无重叠）");
        covered = e + 1;
    }
    assert_eq!(
        covered, total,
        "账本必须完整覆盖 [0, {total})，实际到 {covered}"
    );
}

async fn run_download(
    srv: &HttpTestServer,
    part: &std::path::Path,
    policy: SegmentSyncPolicy,
    ledger: Option<DynamicLedger>,
    progress: Option<Arc<dyn Fn(u64) + Send + Sync>>,
) -> Result<DynamicOutcome, String> {
    download_dynamic(
        &reqwest::Client::new(),
        part,
        SIZE,
        MIN_SPLIT,
        &[srv.url("/file")],
        &[],
        Arc::new(RateLimiter::new(0)),
        None,
        false,
        ledger,
        None,
        progress,
        policy,
        0,
    )
    .await
}

// —— 1. 默认（每段 fsync）回归锚：首个进度回调即可见账本 —— //

#[tokio::test]
async fn per_segment_default_ledger_written_every_segment() {
    let srv = HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        range: true,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("a.part");

    let seen_first = Arc::new(AtomicBool::new(false));
    let ledger_at_first = Arc::new(AtomicBool::new(false));
    let progress = {
        let seen_first = seen_first.clone();
        let ledger_at_first = ledger_at_first.clone();
        let part_c = part.clone();
        let p: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |done| {
            // 跳过进度初报（done=0，段完成前触发）：只锚首个真段完成
            if done == 0 {
                return;
            }
            if !seen_first.swap(true, Ordering::SeqCst) {
                ledger_at_first.store(ledger_exists(&part_c), Ordering::SeqCst);
            }
        });
        p
    };

    let r = run_download(
        &srv,
        &part,
        SegmentSyncPolicy::default(),
        Some(ledger_handle(&part)),
        Some(progress),
    )
    .await;
    assert!(r.is_ok(), "默认模式下载应成功: {r:?}");
    assert!(
        seen_first.load(Ordering::SeqCst),
        "应有至少一次段完成回调（排除初报）"
    );
    assert!(
        ledger_at_first.load(Ordering::SeqCst),
        "P0-4 默认：首个段完成回调时账本必须已落盘（每段 fsync 语义回归锚）"
    );
    assert_ledger_complete(&part, SIZE);
    assert_eq!(
        std::fs::read(&part).unwrap(),
        patterned(SIZE),
        "内容必须逐字节一致"
    );
}

// —— 2. 批量模式推迟：阈值未达标时进度回调时刻账本不存在 —— //

#[tokio::test]
async fn batch_mode_defers_ledger_until_threshold() {
    let srv = HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        range: true,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("b.part");

    let any_ledger_during = Arc::new(AtomicBool::new(false));
    let progress = {
        let any = any_ledger_during.clone();
        let part_c = part.clone();
        let p: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |_done| {
            if ledger_exists(&part_c) {
                any.store(true, Ordering::SeqCst);
            }
        });
        p
    };

    // 8 段 < 100 阈值，1h 间隔不可达 → 全程无冲刷（确定性）
    let r = run_download(
        &srv,
        &part,
        batch_policy(100, 3_600_000),
        Some(ledger_handle(&part)),
        Some(progress),
    )
    .await;
    assert!(r.is_ok(), "批量模式下载应成功: {r:?}");
    assert!(
        !any_ledger_during.load(Ordering::SeqCst),
        "阈值未达标：所有进度回调时刻账本必须不存在（推迟语义）"
    );
    // 收尾冲刷：结束后账本完整
    assert_ledger_complete(&part, SIZE);
    assert_eq!(std::fs::read(&part).unwrap(), patterned(SIZE));
}

// —— 3. 段阈值触发：N=1 等价每段模式（首个回调即可见账本） —— //

#[tokio::test]
async fn batch_threshold_one_flushes_every_segment() {
    let srv = HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        range: true,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("c.part");

    let seen_first = Arc::new(AtomicBool::new(false));
    let ledger_at_first = Arc::new(AtomicBool::new(false));
    let progress = {
        let seen_first = seen_first.clone();
        let ledger_at_first = ledger_at_first.clone();
        let part_c = part.clone();
        let p: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |done| {
            if done == 0 {
                return; // 跳过进度初报
            }
            if !seen_first.swap(true, Ordering::SeqCst) {
                ledger_at_first.store(ledger_exists(&part_c), Ordering::SeqCst);
            }
        });
        p
    };

    let r = run_download(
        &srv,
        &part,
        batch_policy(1, 3_600_000),
        Some(ledger_handle(&part)),
        Some(progress),
    )
    .await;
    assert!(r.is_ok(), "N=1 批量模式下载应成功: {r:?}");
    assert!(
        ledger_at_first.load(Ordering::SeqCst),
        "batch_segments=1：每段冲刷，首个段完成回调时账本必须存在"
    );
    assert_ledger_complete(&part, SIZE);
}

// —— 4. 间隔阈值触发：慢速服务器 + 300ms 间隔，中途必须出现账本 —— //

#[tokio::test]
async fn batch_interval_triggers_midflight_flush() {
    // 150ms/请求 × 8 段 / 2 worker ≥ 4 轮 ≥ 600ms 总时长；
    // 300ms 间隔在第 3 轮完成前必然超时 → 中途冲刷必然发生
    let srv = HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        range: true,
        patterned_content: true,
        delay_ms: 150,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("d.part");

    let midflight_ledger = Arc::new(AtomicBool::new(false));
    let progress = {
        let mid = midflight_ledger.clone();
        let part_c = part.clone();
        let p: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |_done| {
            if ledger_exists(&part_c) {
                mid.store(true, Ordering::SeqCst);
            }
        });
        p
    };

    let r = run_download(
        &srv,
        &part,
        batch_policy(100, 300),
        Some(ledger_handle(&part)),
        Some(progress),
    )
    .await;
    assert!(r.is_ok(), "间隔触发模式下载应成功: {r:?}");
    assert!(
        midflight_ledger.load(Ordering::SeqCst),
        "间隔 300ms：下载过程中（某次进度回调时）账本必须已出现（中途冲刷）"
    );
    assert_ledger_complete(&part, SIZE);
    assert_eq!(std::fs::read(&part).unwrap(), patterned(SIZE));
}

// —— 5. 暂停→恢复闭环：批量模式暂停出口冲刷 + resume 跳过已完成段 —— //

#[tokio::test]
async fn batch_mode_pause_flushes_and_resume_completes() {
    // 慢速服务器确保暂停发生在中途（首段完成后）
    let srv = HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        range: true,
        patterned_content: true,
        delay_ms: 200,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("e.part");

    let pause = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pause_c = pause.clone();
    let progress: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |done| {
        // 首个段完成后即请求暂停（跳过 done=0 初报——初报时置位会让
        // worker 在领取任何段之前就退出，收尾无脏可冲）
        if done > 0 {
            pause_c.store(true, Ordering::SeqCst);
        }
    });

    let r = download_dynamic(
        &reqwest::Client::new(),
        &part,
        SIZE,
        MIN_SPLIT,
        &[srv.url("/file")],
        &[],
        Arc::new(RateLimiter::new(0)),
        None,
        false,
        Some(ledger_handle(&part)),
        Some(pause),
        Some(progress),
        batch_policy(100, 3_600_000), // 全程无中途冲刷 → 只能靠收尾冲刷
        0,
    )
    .await;
    assert_eq!(r, Ok(DynamicOutcome::Paused), "应暂停退出: {r:?}");
    // 暂停出口收尾冲刷：账本必须存在且记录了已完成段（≥1 段）
    let l = smart_dl_httpdl::ledger::load(&smart_dl_httpdl::ledger::ledger_path(&part))
        .expect("暂停退出后账本必须存在（收尾冲刷语义）");
    assert!(
        !l.done.is_empty(),
        "暂停前完成的段必须已入账（批量收尾冲刷）"
    );
    assert!(
        l.validate_segments(),
        "乱序完成（多 worker 并发常态）后的账本必须可被 resume 消费——
        旧实现按完成顺序 append，validate 拒绝乱序账本 → 全量重下"
    );
    let done_before = l.done.clone();
    let done_count = done_before.len();
    let req_at_pause = srv.request_count.load(Ordering::SeqCst);

    // 恢复：从账本继续（batch 模式同样路径）
    let r2 = download_dynamic(
        &reqwest::Client::new(),
        &part,
        SIZE,
        MIN_SPLIT,
        &[srv.url("/file")],
        &[],
        Arc::new(RateLimiter::new(0)),
        None,
        false,
        Some(DynamicLedger {
            path: smart_dl_httpdl::ledger::ledger_path(&part),
            etag: Some("etag-1".to_string()),
            last_modified: None,
            min_split: MIN_SPLIT,
            done: done_before,
        }),
        None,
        None,
        batch_policy(100, 3_600_000),
        0,
    )
    .await;
    assert_eq!(r2, Ok(DynamicOutcome::Completed), "恢复应完成: {r2:?}");
    assert_ledger_complete(&part, SIZE);
    assert_eq!(
        std::fs::read(&part).unwrap(),
        patterned(SIZE),
        "恢复后内容必须逐字节一致"
    );
    // 判别锚（乱序账本全量重下缺陷）：恢复后新增请求数必须恰好等于剩余段数
    // ——旧实现（账本被 validate 拒绝 → 全新计划）会重发全部 8 段请求。
    // 直调 download_dynamic 无探测请求，请求计数即段请求数（服务器健康
    // 无重试/拆半，每段恰 1 请求）。
    let req_after = srv.request_count.load(Ordering::SeqCst);
    let total_segments = (SIZE / MIN_SPLIT) as usize;
    assert_eq!(
        req_after - req_at_pause,
        total_segments - done_count,
        "resume 必须只补齐缺口段（已完成 {done_count} 段不重下）；
        全量重下说明账本未被消费"
    );
}

// —— 7. 引擎接线：with_sync_policy 真实传导到下载循环 —— //

#[tokio::test]
async fn engine_with_sync_policy_batch_completes_and_verifies() {
    let srv = HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        range: true,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    // 批量模式注入：若策略未传导（回落每段 fsync），下载仍会完成——
    // 因此本测是「不回归」冒烟（接线存在 + 结果正确），策略传导的
    // 行为差异由上面直调测试锁定。
    let engine = HttpEngine::new(reqwest::Client::new()).with_sync_policy(batch_policy(4, 500));
    let task = make_http_task_to(
        "fsync-engine",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("out.bin"),
    );
    let tid = engine.add(&task).await.unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    let got = std::fs::read(dir.path().join("out.bin")).unwrap();
    assert_eq!(sha256_of(&got), sha256_of(&patterned(SIZE)));
}
