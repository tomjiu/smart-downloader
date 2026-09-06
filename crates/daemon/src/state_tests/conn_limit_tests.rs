//! S1-c 任务级连接数上限测试（FakeEngine，不联网；BT 语义层）。
//! 覆盖：非 BT 预拒（409 口径）/ BT 设置+持久化+复位 / 恢复重放。
#![cfg(test)]

use super::*;

/// 非 BT 任务预拒（daemon 侧定性，不触碰引擎）。
#[tokio::test]
async fn conn_limit_rejects_non_bt_task() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task("https://srv/a.bin".into(), None)
        .await
        .unwrap();
    let err = state.set_task_max_connections(&tid, 120).await.unwrap_err();
    assert!(
        matches!(err, DaemonError::UnsupportedOp(_)),
        "HTTP 任务必须 409 预拒: {err:?}"
    );
    assert!(fake.max_conn_calls().is_empty(), "预拒不得触达引擎");
}

/// BT 任务：设置 → 引擎下发 + 记录字段 + 落盘；0 = 复位（仍记录 Some(0)，
/// 恢复重放按 0 语义复位会话默认）。
#[cfg(feature = "bt")]
#[tokio::test]
async fn conn_limit_bt_sets_resets_and_persists() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let state = DaemonState::new(fake.clone(), vec![]).with_storage(store.clone());

    let tid = state
        .add_bt_task_opts(
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
            None,
            false,
            None,
        )
        .await
        .unwrap();
    assert!(fake.added.lock().len() == 1, "magnet 立即入引擎");

    state.set_task_max_connections(&tid, 120).await.unwrap();
    assert_eq!(fake.max_conn_calls().len(), 1);
    assert_eq!(fake.max_conn_calls()[0].1, 120);
    assert_eq!(
        state.tasks.lock().get(&tid).unwrap().task.max_connections,
        Some(120)
    );

    // 复位（0 = 会话默认）
    state.set_task_max_connections(&tid, 0).await.unwrap();
    let calls = fake.max_conn_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].1, 0);
    assert_eq!(
        state.tasks.lock().get(&tid).unwrap().task.max_connections,
        Some(0)
    );

    // 落盘校验（Some(0) 序列化保留——恢复重放按复位语义下发）
    let text = std::fs::read_to_string(&store).unwrap();
    assert!(text.contains("max_connections"), "tasks.json 必须含新字段");
}

/// 恢复重放：持久化的 max_connections 在 restore 后原样下发引擎。
#[cfg(feature = "bt")]
#[tokio::test]
async fn conn_limit_replays_on_restore() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
    let pts = vec![PersistedTask {
        task: DownloadTask {
            id: "t1".into(),
            canonical_id: CanonicalId {
                kind: CanonicalKind::Bt,
                identity: magnet.into(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Magnet(magnet.into()),
            identity: ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                sha1: None,
                md5: None,
                backup_md5: None,
            },
            dest_root: std::path::PathBuf::from("./downloads"),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential: false,
            max_connections: Some(80),
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
        engine_kind: EngineKind::Bt,
        paused: false,
    }];
    std::fs::write(&store, serde_json::to_vec(&pts).unwrap()).unwrap();

    let state = DaemonState::new(fake.clone(), vec![]);
    let n = state.restore_from(&store).await.unwrap();
    assert_eq!(n, 1);
    let calls = fake.max_conn_calls();
    assert_eq!(calls.len(), 1, "恢复后必须原样重放连接数上限");
    assert_eq!(calls[0].1, 80);
}
