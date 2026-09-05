//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! 定时/错峰下载（E23）：延迟入引擎 + 调度激活 + 调度中 pause/resume +
//! 恢复不误启动。
#![cfg(test)]

use super::*;

/// 等待持久化文件出现（autosave 异步时序；本模块独立实现——
/// wait_file 定义于 persist_tests 模块内部，跨模块不可见）。
fn wait_file(path: &std::path::Path, timeout_ms: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("等待持久化文件超时: {path:?}");
}

/// 白盒插入调度等待任务（engine_tid 空 + start_at 指定时刻；不联网）。
fn insert_scheduled(state: &DaemonState, id: &str, kind: EngineKind, start_at: u64, st: TaskState) {
    let rec = TaskRecord {
        task: DownloadTask {
            id: id.into(),
            canonical_id: CanonicalId {
                kind: CanonicalKind::Http,
                identity: format!("https://s.example/{id}"),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Http {
                url: format!("https://s.example/{id}"),
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
                start_at_unix: start_at,
                next_retry_at_unix: 0,
            },
            limits: None,
        },
        engine_tid: None,
        engine_kind: kind,
        engine_status: None,
        events: vec![],
    };
    state.tasks.lock().insert(id.into(), rec);
}

#[tokio::test]
async fn add_with_future_start_at_defers_engine() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]);
    let future = now_unix() + 3600;
    let tid = state
        .add_http_task_opts(
            "https://s.example/later.bin".into(),
            None,
            AddHttpOpts {
                start_at_unix: Some(future),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // 引擎未被触碰（延迟入引擎的核心证据）
    assert!(
        fake.added.lock().is_empty(),
        "定时任务不应在 add 时接入引擎"
    );
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    assert!(rec.engine_tid.is_none(), "调度等待中无引擎句柄");
    assert_eq!(rec.task.state, TaskState::Queued);
    assert_eq!(rec.task.metadata.start_at_unix, future);
    // 列表/快照透出（0 省略、非 0 出现）
    let list = state.list();
    assert_eq!(list[0].start_at_unix, future);
    let snap = state.task_snapshot(&tid).await.unwrap();
    assert_eq!(snap.start_at_unix, future);
    assert_eq!(snap.state, "Queued");
    // 对照：未指定 start_at 的任务立即入引擎且字段省略
    let tid2 = state
        .add_http_task("https://s.example/now.bin".into(), None)
        .await
        .unwrap();
    assert_eq!(fake.added.lock().len(), 1, "普通任务照常入引擎");
    let snap2 = state.task_snapshot(&tid2).await.unwrap();
    assert_eq!(snap2.start_at_unix, 0);
}

#[tokio::test]
async fn activate_due_tasks_activates_only_due() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]);
    let past = now_unix() - 10;
    let future = now_unix() + 3600;
    insert_scheduled(&state, "t1", EngineKind::Http, past, TaskState::Queued);
    insert_scheduled(&state, "t2", EngineKind::Http, future, TaskState::Queued);
    insert_scheduled(&state, "t3", EngineKind::Http, 0, TaskState::Queued); // 无调度（0）
    let activated = state.activate_due_tasks().await;
    assert_eq!(activated, vec!["t1".to_string()], "仅到期任务被激活");
    assert_eq!(fake.added.lock().len(), 1);
    {
        let tasks = state.tasks.lock();
        assert!(tasks.get("t1").unwrap().engine_tid.is_some());
        assert_eq!(
            tasks.get("t1").unwrap().task.state,
            TaskState::Queued,
            "激活不改记录态（轮询器对齐）"
        );
        assert!(
            tasks.get("t2").unwrap().engine_tid.is_none(),
            "未到期不激活"
        );
        assert!(
            tasks.get("t3").unwrap().engine_tid.is_none(),
            "无调度不激活"
        );
    }
    // 事件：t1 有 TaskActivated（通过任务事件链验证 scheduled_start）
    let rec = state.tasks.lock().get("t1").cloned().unwrap();
    assert!(
        rec.events.iter().any(|e| e.op == "scheduled_start"),
        "激活应落任务事件: {:?}",
        rec.events
    );
}

#[tokio::test]
async fn activate_without_engine_marks_failed() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]);
    // Provider 引擎未装配 → engine_for 失败 → Failed 终态
    insert_scheduled(
        &state,
        "t9",
        EngineKind::Provider,
        now_unix() - 5,
        TaskState::Queued,
    );
    let activated = state.activate_due_tasks().await;
    assert!(activated.is_empty(), "激活失败不进返回集");
    {
        let tasks = state.tasks.lock();
        let rec = tasks.get("t9").unwrap();
        assert_eq!(rec.task.state, TaskState::Failed);
        assert!(rec.engine_tid.is_none());
    }
}

#[tokio::test]
async fn pause_resume_on_scheduled_task_lifecycle() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]);
    insert_scheduled(
        &state,
        "t1",
        EngineKind::Http,
        now_unix() + 3600,
        TaskState::Queued,
    );
    // pause = 取消自动启动（记录级，无引擎调用）
    state.pause("t1").await.unwrap();
    assert!(fake.paused_calls().is_empty(), "调度中暂停不触碰引擎");
    assert_eq!(
        state.tasks.lock().get("t1").unwrap().task.state,
        TaskState::Paused
    );
    // resume = 立即激活（消费定时）
    state.resume("t1").await.unwrap();
    assert_eq!(fake.added.lock().len(), 1, "resume 应接入引擎");
    assert_eq!(
        state.tasks.lock().get("t1").unwrap().task.state,
        TaskState::Downloading(EngineKind::Http)
    );
    // 激活后再次 pause → 走引擎侧原路径
    state.pause("t1").await.unwrap();
    assert_eq!(fake.paused_calls().len(), 1);
}

#[test]
fn resolve_start_at_explicit_and_jitter() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    // 显式值直传：过去时刻原样保留（= 立即语义），不受 jitter 影响
    let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]);
    assert_eq!(state.resolve_start_at(Some(12345)), 12345);
    assert_eq!(state.resolve_start_at(Some(0)), 0);
    // jitter 未配置 → None = 0（立即）
    assert_eq!(state.resolve_start_at(None), 0);
    // jitter 配置后：None → now..=now+jitter；显式值不被抖动叠加
    let state2 =
        DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]).with_start_jitter(5);
    for _ in 0..20 {
        let t = state2.resolve_start_at(None);
        assert!(
            t > now_unix() - 1 && t <= now_unix() + 5,
            "jitter 时刻应在 (now, now+5] 内: {t}"
        );
    }
    assert_eq!(
        state2.resolve_start_at(Some(999)),
        999,
        "显式 start_at 不叠加抖动"
    );
}

#[tokio::test]
async fn restore_keeps_future_scheduled_out_of_engine() {
    // 定时任务重启：未到期任务恢复为记录（不入引擎），到期后可被激活
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = Arc::new(
        DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![])
            .with_storage(store.clone()),
    );
    let future = now_unix() + 3600;
    let tid = state
        .add_http_task_opts(
            "https://s.example/boot.bin".into(),
            None,
            AddHttpOpts {
                start_at_unix: Some(future),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(fake.added.lock().is_empty());
    wait_file(&store, 2000);

    // 新 state 恢复：不入引擎、保持 Queued、start_at 保留
    let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
    let state2 = DaemonState::new(fake2.clone() as Arc<dyn DownloadEngine>, vec![]);
    let n = state2.restore_from(&store).await.unwrap();
    assert_eq!(n, 1);
    assert!(fake2.added.lock().is_empty(), "未到期任务恢复时不得误启动");
    {
        let tasks = state2.tasks.lock();
        let rec = tasks.get(&tid).unwrap();
        assert!(rec.engine_tid.is_none());
        assert_eq!(rec.task.state, TaskState::Queued);
        assert_eq!(rec.task.metadata.start_at_unix, future);
    }
    // 手工把时刻改为已到期（模拟"等待期间时刻流逝"）→ 调度循环可激活
    {
        let mut tasks = state2.tasks.lock();
        tasks.get_mut(&tid).unwrap().task.metadata.start_at_unix = now_unix() - 1;
    }
    let activated = state2.activate_due_tasks().await;
    assert_eq!(activated, vec![tid.clone()]);
    assert_eq!(fake2.added.lock().len(), 1);
}
