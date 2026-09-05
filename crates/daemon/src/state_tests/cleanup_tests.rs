//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! E20 已完成任务自动清扫：判龄 / 禁用 / 数据处置。
#![cfg(test)]

use super::*;

/// 白盒插入 Completed 任务，可编程完成时刻与引擎槽位。
fn insert_completed(state: &DaemonState, id: &str, finished_at_unix: u64) {
    let rec = TaskRecord {
        task: DownloadTask {
            id: id.into(),
            canonical_id: CanonicalId {
                kind: CanonicalKind::Http,
                identity: format!("https://example.com/{id}"),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Http {
                url: format!("https://example.com/{id}"),
                headers: vec![],
                auth: None,
                backup_url: None,
                proxy: None,
            },
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
            state: TaskState::Completed,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential: false,
            metadata: TaskMetadata {
                name: None,
                added_at_unix: 0,
                tags: Vec::new(),
                finished_at_unix,
                start_at_unix: 0,
                next_retry_at_unix: 0,
            },
            limits: None,
        },
        engine_tid: Some(id.to_string()),
        engine_kind: EngineKind::Http,
        engine_status: None,
        events: vec![],
    };
    state.tasks.lock().insert(id.into(), rec);
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[tokio::test]
async fn sweep_respects_age_and_disabled() {
    // days=0 禁用
    let state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![]).with_cleanup(
        crate::config::CleanupCfg {
            auto_remove_completed_days: 0,
            auto_remove_keep_data: true,
        },
    );
    insert_completed(&state, "old1", now_secs() - 86_400 * 30);
    assert!(
        state.sweep_completed_cleanup().await.is_empty(),
        "禁用时空转"
    );

    // days=7：老任务清扫，年轻任务保留
    let state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![]).with_cleanup(
        crate::config::CleanupCfg {
            auto_remove_completed_days: 7,
            auto_remove_keep_data: true,
        },
    );
    insert_completed(&state, "old1", now_secs() - 86_400 * 8);
    insert_completed(&state, "new1", now_secs() - 86_400);
    insert_completed(&state, "old_nots", now_secs() - 86_400 * 30);
    {
        // 无完成时刻（旧档）→ 跳过不猜龄
        let mut tasks = state.tasks.lock();
        tasks
            .get_mut("old_nots")
            .unwrap()
            .task
            .metadata
            .finished_at_unix = 0;
    }
    let swept = state.sweep_completed_cleanup().await;
    assert_eq!(
        swept,
        vec!["old1".to_string()],
        "仅超龄且有完成时刻的被清扫"
    );
    let tasks = state.tasks.lock();
    assert!(tasks.get("new1").is_some(), "年轻任务保留");
    assert!(tasks.get("old_nots").is_some(), "无时刻任务保留");
    assert!(tasks.get("old1").is_none(), "超龄任务已移除");
}

#[tokio::test]
async fn sweep_data_disposition_follows_config() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]).with_cleanup(
        crate::config::CleanupCfg {
            auto_remove_completed_days: 1,
            auto_remove_keep_data: false, // 连数据一起删
        },
    );
    insert_completed(&state, "old1", now_secs() - 86_400 * 2);
    let swept = state.sweep_completed_cleanup().await;
    assert_eq!(swept.len(), 1);
    assert_eq!(
        fake.removed_calls(),
        vec![("old1".to_string(), true)],
        "keep_data=false → 引擎侧删数据"
    );
}

#[tokio::test]
async fn sweep_ignores_non_completed_states() {
    let state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![]).with_cleanup(
        crate::config::CleanupCfg {
            auto_remove_completed_days: 1,
            auto_remove_keep_data: true,
        },
    );
    // Seeding 任务带超龄完成时刻——状态非 Completed 不清扫（做种保护）
    {
        insert_completed(&state, "seed1", now_secs() - 86_400 * 30);
        let mut tasks = state.tasks.lock();
        tasks.get_mut("seed1").unwrap().task.state = TaskState::Seeding;
    }
    let swept = state.sweep_completed_cleanup().await;
    assert!(swept.is_empty(), "非 Completed 状态不清扫");
}
