//! Issue #120 回归：remove_with 错误传播——「删了还在下」的根因修复。
//! 旧实现三连静默（先删记录 → engine_for Err 跳过 → engine.remove Err 吞掉
//! → 恒 Ok）。新契约：引擎停止失败 → Err + 记录保留（可重试）；
//! 引擎 NotFound → 视为已停止，放行清理；无引擎句柄 → 直接清理。

use super::*;

async fn add_one(state: &DaemonState, i: usize) -> String {
    state
        .add_http_task(format!("https://example.com/f{i}.bin"), None)
        .await
        .unwrap()
}

/// 引擎 remove 失败必须传播，且任务记录保留（可重试）——Issue #120 主断言。
#[tokio::test]
async fn remove_with_propagates_engine_failure_and_keeps_record() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let id = add_one(&state, 1).await;

    fake.set_remove_result(Some(smart_dl_core::types::EngineError::Other(
        "boom".into(),
    )));
    let r = state.remove_with(&id, false).await;
    assert!(r.is_err(), "引擎停止失败必须传播（旧实现恒 Ok）");
    assert!(
        state.tasks.lock().get(&id).is_some(),
        "任务记录必须保留以便重试（旧实现记录已删、引擎任务失控无迹可寻）"
    );

    // 引擎恢复后重试 → 成功 + 记录清理
    fake.set_remove_result(None);
    state.remove_with(&id, false).await.unwrap();
    assert!(state.tasks.lock().get(&id).is_none(), "重试成功后记录清理");
}

/// 引擎 NotFound（会话重启/已退役）= 停止目标已达成 → 放行记录清理。
#[tokio::test]
async fn remove_with_engine_notfound_still_clears_record() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let id = add_one(&state, 2).await;

    fake.set_remove_result(Some(smart_dl_core::types::EngineError::NotFound));
    state.remove_with(&id, false).await.unwrap();
    assert!(
        state.tasks.lock().get(&id).is_none(),
        "引擎侧 NotFound 不得阻塞记录清理（否则任务永远删不掉）"
    );
}

/// 引擎不可用（engine_for Err）必须报错，不得静默跳过——
/// 旧实现在此场景下引擎侧任务继续跑、记录已删、恒 Ok。
#[tokio::test]
async fn remove_with_errors_when_engine_unavailable() {
    // 只装配 Http 引擎；插入一条 BT kind 记录（engine_tid 存在但引擎未装配）
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let rec = TaskRecord {
        seeding_since: None,
        task: DownloadTask {
            id: "t-bt-noengine".into(),
            canonical_id: CanonicalId {
                kind: CanonicalKind::Bt,
                identity: "ABC123".to_string(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Magnet("magnet:?xt=urn:btih:ABC123".into()),
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
                added_at_ms: 0,
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: 0,
                next_retry_at_unix: 0,
            },
            limits: None,
            max_connections: None,
            max_uploads: None,
            queue_priority: 0,
        },
        engine_tid: Some("bt-tid".to_string()),
        engine_kind: EngineKind::Bt,
        engine_status: None,
        events: vec![],
    };
    state.tasks.lock().insert("t-bt-noengine".into(), rec);

    let r = state.remove_with("t-bt-noengine", false).await;
    assert!(r.is_err(), "引擎不可用必须报错（旧实现静默跳过且恒 Ok）");
    assert!(
        state.tasks.lock().get("t-bt-noengine").is_some(),
        "记录保留——用户可见可诊断，而非无声消失"
    );
}

/// 无引擎句柄（provider 兜底完成 / 恢复失败记录）→ 直接清理（旧口径保留）。
#[tokio::test]
async fn remove_with_no_engine_handle_clears_record() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let rec = TaskRecord {
        seeding_since: None,
        task: DownloadTask {
            id: "t-nohandle".into(),
            canonical_id: CanonicalId {
                kind: CanonicalKind::Http,
                identity: "https://example.com/x".to_string(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Http {
                url: "https://example.com/x".into(),
                headers: vec![],
                backup_url: None,
                proxy: None,
                auth: None,
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
            state: TaskState::Failed,
            retry: Default::default(),
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
            max_uploads: None,
            queue_priority: 0,
        },
        engine_tid: None,
        engine_kind: EngineKind::Http,
        engine_status: None,
        events: vec![],
    };
    state.tasks.lock().insert("t-nohandle".into(), rec);

    state.remove_with("t-nohandle", false).await.unwrap();
    assert!(state.tasks.lock().get("t-nohandle").is_none());
}

/// 正常路径回归：引擎 remove 成功 → 记录清理 + 引擎收到 delete_data 透传。
#[tokio::test]
async fn remove_with_normal_path_still_works_and_forwards_delete_data() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let id = add_one(&state, 3).await;

    state.remove_with(&id, true).await.unwrap();
    assert!(state.tasks.lock().get(&id).is_none());
    let calls = fake.removed_calls();
    assert_eq!(calls.len(), 1, "引擎恰好收到一次 remove");
    assert!(calls[0].1, "delete_data=true 必须透传引擎");
}
