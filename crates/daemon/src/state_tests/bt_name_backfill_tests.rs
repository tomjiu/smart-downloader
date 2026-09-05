//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! E28 BT 任务名回填：torrent metadata name → 轮询幂等回填（状态权威不变）。
#![cfg(test)]

use super::*;

/// 插入 Downloading(Bt) 状态的无名 magnet 任务记录（不联网）。
fn insert_bt_downloading(state: &DaemonState, id: &str, ih: &str) {
    let rec = TaskRecord {
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
            state: TaskState::Downloading(EngineKind::Bt),
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
    };
    state.tasks.lock().insert(id.into(), rec);
}

#[tokio::test]
async fn poll_backfills_bt_torrent_name_without_state_migration() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let state = DaemonState::new(fake.clone(), vec![]);
    let ih = "0d2c9c9d5c2d3e8f9a1b2c3d4e5f6a7b8c9d0e1f";
    insert_bt_downloading(&state, "t-bt", ih);
    fake.set_status_name("Ubuntu 24.04 ISO");

    let effects = state.poll_engine_states().await;
    assert!(
        effects.is_empty(),
        "BT 分支状态权威 = alert 流，轮询不得产生迁移 effect"
    );
    {
        let rec = state.tasks.lock().get("t-bt").cloned().unwrap();
        assert_eq!(
            rec.task.metadata.name.as_deref(),
            Some("Ubuntu 24.04 ISO"),
            "torrent metadata name 应回填"
        );
        assert!(
            rec.events
                .iter()
                .any(|e| e.op == "name_backfilled"
                    && e.detail.as_deref() == Some("Ubuntu 24.04 ISO")),
            "应有 name_backfilled 事件"
        );
        assert!(rec.engine_status.is_some(), "E11 快照缓存仍应生效");
    }

    // 幂等：回填一次后 name 非 None 自然停；引擎改名不得污染
    fake.set_status_name("renamed-should-not-apply");
    let _ = state.poll_engine_states().await;
    let rec = state.tasks.lock().get("t-bt").cloned().unwrap();
    assert_eq!(rec.task.metadata.name.as_deref(), Some("Ubuntu 24.04 ISO"));
    assert_eq!(
        rec.events
            .iter()
            .filter(|e| e.op == "name_backfilled")
            .count(),
        1,
        "回填事件恰好一次"
    );
}

#[tokio::test]
async fn poll_bt_never_overrides_explicit_name() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let state = DaemonState::new(fake.clone(), vec![]);
    let ih = "1d2c9c9d5c2d3e8f9a1b2c3d4e5f6a7b8c9d0e1f";
    insert_bt_downloading(&state, "t-bt2", ih);
    {
        let mut tasks = state.tasks.lock();
        tasks.get_mut("t-bt2").unwrap().task.metadata.name = Some("用户显式名".into());
    }
    fake.set_status_name("torrent-metadata-name");

    let _ = state.poll_engine_states().await;
    let rec = state.tasks.lock().get("t-bt2").cloned().unwrap();
    assert_eq!(
        rec.task.metadata.name.as_deref(),
        Some("用户显式名"),
        "显式名权威，E15 语义与 HTTP 侧一致"
    );
    assert!(
        !rec.events.iter().any(|e| e.op == "name_backfilled"),
        "不得有回填事件"
    );
}

#[tokio::test]
async fn poll_bt_skips_non_active_states() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let state = DaemonState::new(fake.clone(), vec![]);
    let ih = "2d2c9c9d5c2d3e8f9a1b2c3d4e5f6a7b8c9d0e1f";
    insert_bt_downloading(&state, "t-bt3", ih);
    // 非活跃态（与 apply_bt_alert 终态清零同口径；Queued = 调度未激活）
    {
        let mut tasks = state.tasks.lock();
        tasks.get_mut("t-bt3").unwrap().task.state = TaskState::Queued;
    }
    fake.set_status_name("should-not-backfill");

    let _ = state.poll_engine_states().await;
    let rec = state.tasks.lock().get("t-bt3").cloned().unwrap();
    assert!(rec.task.metadata.name.is_none(), "非活跃态不回填不缓存");
    assert!(rec.engine_status.is_none(), "非活跃态不缓存快照");
}
