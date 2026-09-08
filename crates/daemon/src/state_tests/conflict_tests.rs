//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! E21 文件冲突策略：改名候选 / skip 秒完成 / 默认覆盖不变。
#![cfg(test)]

use super::*;

#[test]
fn bump_conflict_name_skips_occupied() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    assert_eq!(
        DaemonState::bump_conflict_name(p, "a.bin").as_deref(),
        Some("a(1).bin"),
        "空闲目录取 (1)"
    );
    std::fs::write(p.join("a(1).bin"), b"x").unwrap();
    assert_eq!(
        DaemonState::bump_conflict_name(p, "a.bin").as_deref(),
        Some("a(2).bin"),
        "占用则顺延"
    );
    // 无扩展名
    assert_eq!(
        DaemonState::bump_conflict_name(p, "file").as_deref(),
        Some("file(1)")
    );
}

#[tokio::test]
async fn skip_policy_completes_without_engine_add() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dup.bin"), b"existing").unwrap();
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    // V2 白名单：显式 dest 落测试目录 → 注入为白名单根
    let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![])
        .with_dest_root(dir.path().to_path_buf());

    let id = state
        .add_http_task_opts(
            "https://example.com/dup.bin".into(),
            Some(dir.path().to_string_lossy().into_owned()),
            crate::state::AddHttpOpts {
                name: Some("dup.bin".into()),
                conflict: Some(ConflictPolicy::Skip),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // 引擎未收到 add
    assert!(fake.added.lock().is_empty(), "skip 不得入引擎");
    // 任务直接 Completed + 完成时刻入档 + 事件标记 conflict_skip
    {
        let tasks = state.tasks.lock();
        let rec = tasks.get(&id).unwrap();
        assert_eq!(rec.task.state, TaskState::Completed);
        assert!(rec.task.metadata.finished_at_unix > 0);
        assert!(
            rec.events
                .iter()
                .any(|e| e.detail.as_deref() == Some("conflict_skip")),
            "应有 conflict_skip 事件"
        );
        // identity.size 反映既有文件字节数（快照 total 口径来自引擎，
        // skip 无引擎任务 → 用 identity 断言）
        match &rec.task.identity {
            ContentIdentity::SingleFile { size, .. } => assert_eq!(*size, 8),
            other => panic!("identity 应为 SingleFile: {other:?}"),
        }
    }
    // 快照可读
    let snap = state.task_snapshot(&id).await.unwrap();
    assert_eq!(snap.state, "Completed");
    // 可正常删除
    assert!(state.remove_with(&id, false).await.is_ok());
}

#[tokio::test]
async fn no_policy_keeps_default_overwrite_flow() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dup.bin"), b"existing").unwrap();
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![])
        .with_dest_root(dir.path().to_path_buf());

    // 显式名 + 无策略 → 旧行为：照常入引擎（引擎 finalize 覆盖）
    let id = state
        .add_http_task_opts(
            "https://example.com/dup.bin".into(),
            Some(dir.path().to_string_lossy().into_owned()),
            crate::state::AddHttpOpts {
                name: Some("dup.bin".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(fake.added.lock().len(), 1, "默认照常入引擎");
    assert_eq!(state.task_snapshot(&id).await.unwrap().state, "Downloading");
}
