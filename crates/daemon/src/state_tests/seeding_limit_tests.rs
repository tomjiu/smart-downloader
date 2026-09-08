//! Task 39 做种限制执法测试：share_ratio / seeding_time 双限制达标自动暂停。
//! 全部 FakeEngine 白盒构造，无真实网络/无真实 libtorrent（feature `bt`：
//! with_bt 引擎注册 + poll BT 分支均为 bt 门控）。

#![cfg(all(test, feature = "bt"))]

use super::*;
use std::sync::Arc;

/// 构造 BT 做种态任务记录（seeding_since 可编程）。
fn seeding_rec(id: &str, ih: &str, seeding_since: Option<std::time::Instant>) -> TaskRecord {
    let mut rec = TaskRecord {
        task: DownloadTask {
            id: id.into(),
            canonical_id: CanonicalId {
                kind: CanonicalKind::Bt,
                identity: ih.to_string(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Magnet(format!("magnet:?xt=urn:btih:{ih}")),
            identity: ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                sha1: None,
                md5: None,
                backup_md5: None,
            },
            dest_root: PathBuf::from("."),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Seeding,
            retry: RetryState {
                retries: 0,
                max_retries: 0,
            },
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential: false,
            metadata: TaskMetadata {
                name: None,
                added_at_unix: 0,
                added_at_ms: 0,
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: 0,
                next_retry_at_unix: 0,
            },
            limits: None,
            max_connections: None,
            queue_priority: 0,
        },
        engine_tid: Some(ih.to_string()),
        engine_kind: EngineKind::Bt,
        engine_status: None,
        seeding_since,
        events: vec![],
    };
    rec.push_event("add", None);
    rec
}

/// 组装：Bt FakeEngine（可编程 status/limits）+ 已注册做种任务。
async fn setup(
    totals: (u64, u64),
    ratio_limit: Option<f64>,
    time_limit: Option<u32>,
    seeding_since: Option<std::time::Instant>,
) -> (Arc<DaemonState>, Arc<FakeEngine>, String) {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    *fake.status_state.lock() = Some(smart_dl_core::types::EngineState::Seeding);
    *fake.status_totals.lock() = totals;
    *fake.seeding_ratio_limit.lock() = ratio_limit;
    *fake.seeding_time_limit.lock() = time_limit;
    let state = Arc::new(
        DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![]).with_bt(fake.clone()),
    );
    let id = "tSeed".to_string();
    state.tasks.lock().insert(
        id.clone(),
        seeding_rec(
            &id,
            "0d2c9c9d5c2d3e8f9a1b2c3d4e5f6a7b8c9d0e1f",
            seeding_since,
        ),
    );
    (state, fake, id)
}

#[tokio::test]
async fn ratio_limit_reached_pauses_seeding_task() {
    // down=4M up=8M → ratio=2.0；limit=1.5 → 达标 → 引擎暂停 + 记录 Paused
    let (state, fake, id) = setup(
        (4_000_000, 8_000_000),
        Some(1.5),
        None,
        Some(std::time::Instant::now()),
    )
    .await;
    let effects = state.poll_engine_states().await;
    let _ = effects;
    assert!(!fake.paused.lock().is_empty(), "达标必须触发引擎暂停");
    let tasks = state.tasks.lock();
    let rec = tasks.get(&id).unwrap();
    assert_eq!(rec.task.state, TaskState::Paused, "记录同步 Paused");
    assert!(rec.seeding_since.is_none(), "达标暂停后清计时");
    assert!(
        rec.events.iter().any(|e| e.op == "seeding_limit_reached"),
        "必须有 seeding_limit_reached 事件: {:?}",
        rec.events
    );
    assert!(
        rec.events.iter().any(|e| e.op == "pause"),
        "走完整 pause 语义（pause 事件）"
    );
}

#[tokio::test]
async fn ratio_below_limit_keeps_seeding() {
    // ratio=2.0 < limit=2.5 → 不动
    let (state, fake, _id) = setup(
        (4_000_000, 8_000_000),
        Some(2.5),
        None,
        Some(std::time::Instant::now()),
    )
    .await;
    let _ = state.poll_engine_states().await;
    assert!(fake.paused.lock().is_empty(), "未达标不得暂停");
}

#[tokio::test]
async fn seeding_time_limit_reached_pauses() {
    // 做种 31 分钟 ≥ 上限 30 分钟 → 暂停（ratio 未启用）
    let since = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(31 * 60))
        .expect("instant 减法安全");
    let (state, fake, id) = setup((0, 0), None, Some(30), Some(since)).await;
    let _ = state.poll_engine_states().await;
    assert!(!fake.paused.lock().is_empty(), "时长达标必须暂停");
    let rec = state.tasks.lock().get(&id).unwrap().clone();
    assert_eq!(rec.task.state, TaskState::Paused);
    assert!(rec.seeding_since.is_none());
}

#[tokio::test]
async fn seeding_time_below_limit_keeps_seeding() {
    // 刚开始做种 < 30 分钟 → 不动
    let (state, fake, _id) = setup((0, 0), None, Some(30), Some(std::time::Instant::now())).await;
    let _ = state.poll_engine_states().await;
    assert!(fake.paused.lock().is_empty(), "未达时长不得暂停");
}

#[tokio::test]
async fn no_limits_never_pauses() {
    // 双限未启用 → 大上传率也不暂停
    let (state, fake, _id) = setup(
        (100_000_000, 1_000_000),
        None,
        None,
        Some(std::time::Instant::now()),
    )
    .await;
    let _ = state.poll_engine_states().await;
    assert!(fake.paused.lock().is_empty(), "无限值不得暂停");
}

#[tokio::test]
async fn no_seeding_since_skips_time_limit() {
    // 已 Seeding 但计时缺失（重启恢复口径）→ 时长限不触发（ratio 仍可触发）
    let (state, fake, _id) = setup((8_000_000, 4_000_000), None, Some(1), None).await;
    let _ = state.poll_engine_states().await;
    assert!(
        fake.paused.lock().is_empty(),
        "无计时 + 无 ratio 限 → 不暂停"
    );
}

#[tokio::test]
async fn downloading_state_not_enforced() {
    // 引擎报 Downloading（非 Seeding）→ 执法跳过
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    *fake.status_state.lock() = Some(smart_dl_core::types::EngineState::Downloading);
    *fake.status_totals.lock() = (4_000_000, 8_000_000);
    *fake.seeding_ratio_limit.lock() = Some(0.001);
    let state = Arc::new(
        DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![]).with_bt(fake.clone()),
    );
    let id = "tDl".to_string();
    let mut rec = seeding_rec(&id, "0d2c9c9d5c2d3e8f9a1b2c3d4e5f6a7b8c9d0e1f", None);
    rec.task.state = TaskState::Downloading(EngineKind::Bt);
    state.tasks.lock().insert(id, rec);
    let _ = state.poll_engine_states().await;
    assert!(fake.paused.lock().is_empty(), "非 Seeding 态不执法");
}
