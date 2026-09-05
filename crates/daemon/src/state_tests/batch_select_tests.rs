//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! E19 按条件批量：选择器解析 / 非破坏性约束 / 命中集执行。
#![cfg(test)]

use super::*;

/// 白盒插入指定状态/引擎种类的任务记录（不联网）。
fn insert_rec(state: &DaemonState, id: &str, kind: EngineKind, st: TaskState) {
    let rec = TaskRecord {
        task: DownloadTask {
            id: id.into(),
            canonical_id: CanonicalId {
                kind: CanonicalKind::Bt,
                identity: id.to_string(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Magnet(format!("magnet:?xt=urn:btih:{id}")),
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
            state: st,
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
        engine_tid: Some(id.to_string()),
        engine_kind: kind,
        engine_status: None,
        events: vec![],
    };
    state.tasks.lock().insert(id.into(), rec);
}

#[tokio::test]
async fn resume_all_failed_hits_only_failed() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]);
    insert_rec(&state, "t1", EngineKind::Bt, TaskState::Failed);
    insert_rec(&state, "t2", EngineKind::Bt, TaskState::Failed);
    insert_rec(&state, "t3", EngineKind::Bt, TaskState::Queued);

    let outcome = state
        .batch_select(
            &ListQuery {
                states: vec!["failed".into()],
                ..Default::default()
            },
            BatchAction::Resume,
        )
        .await
        .unwrap();
    assert_eq!(outcome.succeeded, 2, "两个 Failed 任务恢复成功");
    assert_eq!(outcome.failed, 0);
    // Queued 未被波及
    {
        let tasks = state.tasks.lock();
        assert_eq!(tasks.get("t3").unwrap().task.state, TaskState::Queued);
        // Failed → Downloading（记录态推进）
        assert_eq!(
            tasks.get("t1").unwrap().task.state,
            TaskState::Downloading(EngineKind::Bt)
        );
    }
}

#[tokio::test]
async fn pause_by_engine_kind() {
    let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let mut state = DaemonState::new(http_fake.clone() as Arc<dyn DownloadEngine>, vec![]);
    // bt 槽注入独立 Fake（pause 走引擎调用，槽内引擎按种类解析）
    state.engines.insert(EngineKind::Bt, fake_http_stub());
    insert_rec(
        &state,
        "h1",
        EngineKind::Http,
        TaskState::Downloading(EngineKind::Http),
    );
    insert_rec(
        &state,
        "b1",
        EngineKind::Bt,
        TaskState::Downloading(EngineKind::Bt),
    );

    let outcome = state
        .batch_select(
            &ListQuery {
                engines: vec!["bt".into()],
                ..Default::default()
            },
            BatchAction::Pause,
        )
        .await
        .unwrap();
    assert_eq!(outcome.succeeded, 1, "仅 bt 命中");
    assert_eq!(outcome.results[0].id, "b1");
    {
        let tasks = state.tasks.lock();
        assert!(
            matches!(
                tasks.get("h1").unwrap().task.state,
                TaskState::Downloading(_)
            ),
            "http 任务不受影响"
        );
    }
}

fn fake_http_stub() -> Arc<dyn DownloadEngine> {
    Arc::new(FakeEngine::new(EngineKind::Bt))
}

#[tokio::test]
async fn remove_via_select_rejected() {
    let state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![]);
    let err = state
        .batch_select(
            &ListQuery {
                states: vec!["completed".into()],
                ..Default::default()
            },
            BatchAction::Remove { delete_data: false },
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("不支持 remove"), "{err}");
}

#[tokio::test]
async fn empty_hit_set_is_idempotent_outcome() {
    let state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![]);
    let outcome = state
        .batch_select(
            &ListQuery {
                states: vec!["failed".into()],
                ..Default::default()
            },
            BatchAction::Resume,
        )
        .await
        .unwrap();
    assert_eq!(
        (outcome.succeeded, outcome.failed),
        (0, 0),
        "空命中 = 空结果"
    );
}

#[tokio::test]
async fn tag_and_search_selectors_work() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]);
    insert_rec(&state, "t1", EngineKind::Http, TaskState::Failed);
    insert_rec(&state, "t2", EngineKind::Http, TaskState::Failed);
    state
        .set_task_tags("t1", Some(vec!["movie".into()]))
        .unwrap();

    // tag 选择器
    let outcome = state
        .batch_select(
            &ListQuery {
                tags: vec!["movie".into()],
                ..Default::default()
            },
            BatchAction::Resume,
        )
        .await
        .unwrap();
    assert_eq!(outcome.succeeded, 1);
    assert_eq!(outcome.results[0].id, "t1");

    // search 选择器（source 语料 magnet btih 含 id）
    let outcome = state
        .batch_select(
            &ListQuery {
                search: Some("t2".into()),
                ..Default::default()
            },
            BatchAction::Pause,
        )
        .await
        .unwrap();
    assert_eq!(outcome.succeeded, 1);
    assert_eq!(outcome.results[0].id, "t2");
}
