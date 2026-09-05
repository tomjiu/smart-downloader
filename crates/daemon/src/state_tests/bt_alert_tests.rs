//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! BT alert 事件流单元测试（feature `bt`）：`transition_for` 迁移矩阵 + `apply_bt_alert`
//! 匹配/缓存写入。不依赖真实 libtorrent 会话（手工构造 TaskRecord）。
#![cfg(all(test, feature = "bt"))]

use super::*;
use smart_dl_btcore::{Alert, AlertKind};

fn make_state_with(rec: TaskRecord) -> DaemonState {
    let engine = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let state = DaemonState::new(Arc::new(engine), vec![]);
    // 测试同 crate 内可访问私有 tasks 表
    (*state.tasks.lock()).insert(rec.task.id.clone(), rec);
    state
}

fn bt_rec(state: TaskState, ih: &str) -> TaskRecord {
    TaskRecord {
        task: DownloadTask {
            id: "t1".into(),
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
            state,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential: false,
            metadata: TaskMetadata {
                name: None,
                added_at_unix: 0,
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: 0,
                next_retry_at_unix: 0,
            },
            limits: None,
        },
        engine_tid: Some(ih.to_string()),
        engine_kind: EngineKind::Bt,
        engine_status: None,
        events: vec![],
    }
}

#[test]
fn finished_alert_promotes_seeding() {
    let state = make_state_with(bt_rec(TaskState::Downloading(EngineKind::Bt), "ABC123"));
    let alert = Alert {
        kind: AlertKind::State,
        ih: "abc123".into(), // 大小写不同 → 归一化匹配
        msg: "torrent finished downloading".into(),
        at: 0,
        resume_ready: false,
    };
    let eff = state.apply_bt_alert(&alert).unwrap();
    assert_eq!(eff.from, TaskState::Downloading(EngineKind::Bt));
    assert_eq!(eff.to, TaskState::Seeding);
    let rec_lock = state.tasks.lock();
    let rec = rec_lock.get("t1").unwrap();
    assert_eq!(rec.task.state, TaskState::Seeding, "任务记录状态必须落盘");
}

#[test]
fn finished_from_queued_also_promotes() {
    // 任务还未被引擎快照驱动（仍 Queued）时，完成 alert 同样推进
    let state = make_state_with(bt_rec(TaskState::Queued, "AABB"));
    let alert = Alert {
        kind: AlertKind::State,
        ih: "aabb".into(),
        msg: "torrent finished downloading".into(),
        at: 0,
        resume_ready: false,
    };
    let eff = state.apply_bt_alert(&alert).unwrap();
    assert_eq!(eff.from, TaskState::Queued);
    assert_eq!(eff.to, TaskState::Seeding);
}

#[test]
fn error_alert_fails_with_message() {
    let state = make_state_with(bt_rec(TaskState::Downloading(EngineKind::Bt), "D9E8"));
    let alert = Alert {
        kind: AlertKind::State,
        ih: "d9e8".into(),
        msg: "torrent error: pex failed".into(),
        at: 0,
        resume_ready: false,
    };
    let eff = state.apply_bt_alert(&alert).unwrap();
    assert_eq!(eff.to, TaskState::Failed);
    assert_eq!(eff.message, "torrent error: pex failed");
    let rec_lock = state.tasks.lock();
    let rec = rec_lock.get("t1").unwrap();
    assert_eq!(rec.task.state, TaskState::Failed);
}

#[test]
fn paused_alert_ignored() {
    // v1 不处理 Paused alert（pause 由 API 直调时同步发布事件）
    let state = make_state_with(bt_rec(TaskState::Downloading(EngineKind::Bt), "P1"));
    let alert = Alert {
        kind: AlertKind::State,
        ih: "p1".into(),
        msg: "torrent paused".into(),
        at: 0,
        resume_ready: false,
    };
    assert!(state.apply_bt_alert(&alert).is_none());
}

#[test]
fn finished_alert_promotes_paused_to_seeding() {
    // Bug C：BT 在记录态 Paused 下被引擎实际完成（Bug A 复活后跑完），
    // Finished alert 应允许推进到 Seeding，避免记录态与引擎态错位。
    let state = make_state_with(bt_rec(TaskState::Paused, "C1"));
    let alert = Alert {
        kind: AlertKind::State,
        ih: "c1".into(),
        msg: "torrent finished downloading".into(),
        at: 0,
        resume_ready: false,
    };
    let eff = state.apply_bt_alert(&alert).unwrap();
    assert_eq!(eff.from, TaskState::Paused);
    assert_eq!(eff.to, TaskState::Seeding);
    let rec_lock = state.tasks.lock();
    let rec = rec_lock.get("t1").unwrap();
    assert_eq!(rec.task.state, TaskState::Seeding, "记录态必须落盘");
}

#[test]
fn non_bt_task_ignored() {
    // HTTP 任务（engine_kind=Http）不匹配 BT alert
    let mut rec = bt_rec(TaskState::Downloading(EngineKind::Bt), "XT77");
    rec.engine_kind = EngineKind::Http;
    let state = make_state_with(rec);
    let alert = Alert {
        kind: AlertKind::State,
        ih: "xt77".into(),
        msg: "torrent finished downloading".into(),
        at: 0,
        resume_ready: false,
    };
    assert!(state.apply_bt_alert(&alert).is_none());
}

#[test]
fn unknown_ih_ignored() {
    let state = make_state_with(bt_rec(TaskState::Downloading(EngineKind::Bt), "KN0WN"));
    let alert = Alert {
        kind: AlertKind::State,
        ih: "na-".into(),
        msg: "torrent finished downloading".into(),
        at: 0,
        resume_ready: false,
    };
    assert!(state.apply_bt_alert(&alert).is_none());
}

#[test]
fn error_alert_zeroes_cached_rates() {
    // E11：轮询缓存持最后窗口速率，Error alert → Failed（非活跃终态）
    // → 速率清零，否则 /stats 聚合把陈旧速率计入失败任务。
    let mut rec = bt_rec(TaskState::Downloading(EngineKind::Bt), "RATES01");
    rec.engine_status = Some(EngineStatus {
        down_rate: 3000,
        up_rate: 1500,
        ..EngineStatus::default()
    });
    let state = make_state_with(rec);
    let alert = Alert {
        kind: AlertKind::State,
        ih: "rates01".into(),
        msg: "torrent error: pex failed".into(),
        at: 0,
        resume_ready: false,
    };
    let eff = state.apply_bt_alert(&alert).unwrap();
    assert_eq!(eff.to, TaskState::Failed);
    let rec_lock = state.tasks.lock();
    let es = rec_lock.get("t1").unwrap().engine_status.as_ref().unwrap();
    assert_eq!(es.down_rate, 0, "Failed 后缓存下行速率必须清零");
    assert_eq!(es.up_rate, 0, "Failed 后缓存上行速率必须清零");
    assert_eq!(
        es.error.as_deref(),
        Some("torrent error: pex failed"),
        "错误信息随迁移写入缓存（task_logs 读取口径）"
    );
}

#[test]
fn finished_alert_keeps_rates_for_seeding_poll() {
    // Finished → Seeding：不做种前清零（Seeding 仍是活跃轮询候选，
    // 下一轮以引擎实时值刷新——上行速率对 /stats 有意义）。
    let mut rec = bt_rec(TaskState::Downloading(EngineKind::Bt), "RATES02");
    rec.engine_status = Some(EngineStatus {
        down_rate: 3000,
        up_rate: 1500,
        ..EngineStatus::default()
    });
    let state = make_state_with(rec);
    let alert = Alert {
        kind: AlertKind::State,
        ih: "rates02".into(),
        msg: "torrent finished downloading".into(),
        at: 0,
        resume_ready: false,
    };
    let eff = state.apply_bt_alert(&alert).unwrap();
    assert_eq!(eff.to, TaskState::Seeding);
    let rec_lock = state.tasks.lock();
    let es = rec_lock.get("t1").unwrap().engine_status.as_ref().unwrap();
    assert_eq!(es.down_rate, 3000, "Seeding 不清零（活跃候选待刷新）");
    assert_eq!(es.up_rate, 1500);
}

#[test]
fn peer_alert_ignored() {
    let state = make_state_with(bt_rec(TaskState::Downloading(EngineKind::Bt), "PR99"));
    let alert = Alert {
        kind: AlertKind::Peer,
        ih: "pr99".into(),
        msg: "peer connected".into(),
        at: 0,
        resume_ready: false,
    };
    assert!(state.apply_bt_alert(&alert).is_none());
}

/// Bug B 回归（重入自死锁）：storage 启用 + Finished alert 迁移 → autosave。
/// 修复前：apply_bt_alert 持 tasks 锁调用 autosave → persisted_tasks 同线程
/// 重入同一把非重入 Mutex → 本测试 5s 超时失败（真实事故 = 全端点永久 hang）。
#[tokio::test(flavor = "multi_thread")]
async fn finished_alert_with_storage_autosave_no_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let engine = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let state = DaemonState::new(Arc::new(engine), vec![]).with_storage(store.clone());
    let rec = bt_rec(TaskState::Queued, "BEEF");
    (*state.tasks.lock()).insert(rec.task.id.clone(), rec);
    let alert = Alert {
        kind: AlertKind::State,
        ih: "beef".into(),
        msg: "torrent finished downloading".into(),
        at: 0,
        resume_ready: false,
    };
    let work = state.apply_bt_alert(&alert);
    let eff = tokio::time::timeout(std::time::Duration::from_secs(5), async move { work })
        .await
        .expect("apply_bt_alert 死锁（Bug B 重入回归）");
    assert!(eff.is_some(), "Finished alert 应产生 Seeding 迁移");
    assert_eq!(eff.unwrap().to, TaskState::Seeding);
    // 落盘确实发生（autosave 在锁外完成）
    assert!(store.exists(), "状态迁移应触发持久化落盘");
}
