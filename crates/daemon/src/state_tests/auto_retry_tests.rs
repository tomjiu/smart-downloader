//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! E30 失败自动重试：退避纯函数序列 / 失败处置矩阵 / 调度门控 /
//! poll Error 拦截全环 / add 失败安排重试后源恢复重试成功。
//! 全部 FakeEngine 白盒构造，无真实网络。
#![cfg(test)]

use super::*;

fn rec_with_retry(max: u32, st: TaskState) -> TaskRecord {
    let mut rec = TaskRecord {
        task: DownloadTask {
            id: "tX".into(),
            canonical_id: CanonicalId {
                kind: CanonicalKind::Http,
                identity: "https://x/f.bin".into(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Http {
                url: "https://x/f.bin".into(),
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
            state: st.clone(),
            retry: RetryState {
                retries: 0,
                max_retries: max,
            },
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
        engine_tid: Some("fk1".into()),
        engine_kind: EngineKind::Http,
        engine_status: None,
        events: vec![],
    };
    // state 与句柄一致性前置校验：Failed 用例应无句柄语义混淆——
    // 统一从 Downloading 出发（真实失败路径正是活跃 → 失败）。
    if st == TaskState::Failed {
        rec.task.state = TaskState::Failed;
        rec.engine_tid = None;
    }
    rec
}

#[test]
fn backoff_delay_sequence_and_cap() {
    // 2/4/8/16/32 → 封顶 60
    assert_eq!(retry_backoff_delay_s(1), 2);
    assert_eq!(retry_backoff_delay_s(2), 4);
    assert_eq!(retry_backoff_delay_s(3), 8);
    assert_eq!(retry_backoff_delay_s(4), 16);
    assert_eq!(retry_backoff_delay_s(5), 32);
    assert_eq!(retry_backoff_delay_s(6), 60, "2^6=64 → 封顶 60");
    assert_eq!(retry_backoff_delay_s(7), 60);
    assert_eq!(retry_backoff_delay_s(25), 60, "远超上限恒封顶");
}

#[test]
fn fail_with_zero_budget_is_terminal() {
    let mut rec = rec_with_retry(0, TaskState::Downloading(EngineKind::Http));
    let to = rec.fail_or_schedule_retry(Some("boom"));
    assert_eq!(to, TaskState::Failed);
    assert_eq!(rec.task.state, TaskState::Failed);
    assert_eq!(rec.task.retry.retries, 0, "max=0 不消耗计数");
    assert_eq!(rec.task.metadata.next_retry_at_unix, 0);
    assert!(
        rec.events.iter().all(|e| e.op != "auto_retry"),
        "终态不产生重试事件"
    );
}

#[test]
fn fail_within_budget_schedules_retry() {
    let mut rec = rec_with_retry(1, TaskState::Downloading(EngineKind::Http));
    let to = rec.fail_or_schedule_retry(Some("boom"));
    assert_eq!(to, TaskState::Queued, "预算未用尽 → 回队列等待退避");
    assert_eq!(rec.task.state, TaskState::Queued);
    assert_eq!(rec.task.retry.retries, 1);
    assert!(
        rec.task.metadata.next_retry_at_unix > now_unix(),
        "next_retry 应为未来时刻（退避延迟）"
    );
    assert!(rec.engine_tid.is_none(), "重试等待任务必须无引擎句柄");
    assert!(
        rec.events.iter().any(|e| e.op == "auto_retry"),
        "必须落 auto_retry 事件"
    );

    // 第二次失败：预算（max=1）用尽 → 终态
    let to2 = rec.fail_or_schedule_retry(Some("boom again"));
    assert_eq!(to2, TaskState::Failed);
    assert_eq!(rec.task.retry.retries, 1, "retries 停在 max，不再递增");
    assert_eq!(rec.task.state, TaskState::Failed);
}

#[tokio::test]
async fn poll_error_schedules_retry_then_exhausts() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task_opts(
            "https://example.com/f.bin".into(),
            None,
            AddHttpOpts {
                auto_retry: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // 白盒推进到 Downloading（真实失败路径：活跃中报 Error）
    state.tasks.lock().get_mut(&tid).unwrap().task.state = TaskState::Downloading(EngineKind::Http);
    fake.set_status_state(EngineState::Error);

    // 首轮 poll：Error → 预算未用尽 → Queued + 重试安排
    let effects = state.poll_engine_states().await;
    assert_eq!(effects.len(), 1, "重试安排也是一次状态迁移（广播）");
    assert_eq!(
        effects[0].to,
        TaskState::Queued,
        "effect 目标必须是 Queued（非 Failed）"
    );
    {
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(rec.task.state, TaskState::Queued);
        assert_eq!(rec.task.retry.retries, 1);
        assert_eq!(rec.task.retry.max_retries, 1);
        assert!(rec.task.metadata.next_retry_at_unix > now_unix());
        assert!(rec.engine_tid.is_none());
    }

    // 未到期：调度循环不激活
    let activated = state.activate_due_tasks().await;
    assert!(activated.is_empty(), "退避期内不得激活");

    // 白盒到期 → 激活（引擎 add 恒成功）→ 恢复 Downloading → next_retry 消费
    state
        .tasks
        .lock()
        .get_mut(&tid)
        .unwrap()
        .task
        .metadata
        .next_retry_at_unix = 1;
    let activated = state.activate_due_tasks().await;
    assert_eq!(activated, vec![tid.clone()]);
    {
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        // 重试 = 重新 add → 引擎侧新句柄（FakeEngine counter 自增：fk2）
        assert!(rec.engine_tid.is_some(), "重试激活必须重建引擎句柄");
        assert_eq!(
            rec.task.metadata.next_retry_at_unix, 0,
            "激活即消费重试安排"
        );
    }

    // 再次 Error：预算用尽 → Failed 终态
    state.tasks.lock().get_mut(&tid).unwrap().task.state = TaskState::Downloading(EngineKind::Http);
    let effects = state.poll_engine_states().await;
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].to, TaskState::Failed);
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    assert_eq!(rec.task.state, TaskState::Failed);
    assert_eq!(rec.task.retry.retries, 1);
}

#[tokio::test]
async fn poll_zero_budget_keeps_legacy_failed_semantics() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task("https://example.com/f.bin".into(), None)
        .await
        .unwrap();
    state.tasks.lock().get_mut(&tid).unwrap().task.state = TaskState::Downloading(EngineKind::Http);
    fake.set_status_state(EngineState::Error);

    state.poll_engine_states().await;
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    assert_eq!(
        rec.task.state,
        TaskState::Failed,
        "默认 auto_retry=0 必须保持既有一次性失败语义"
    );
    assert_eq!(rec.task.metadata.next_retry_at_unix, 0);
}

#[tokio::test]
async fn add_failure_schedules_retry_and_succeeds_after_recovery() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    fake.fail_url("https://example.com/f.bin");
    // 白盒建「重试等待」记录（首次 add 失败/restore 激活失败后的形态）：
    // Queued + 句柄空 + next_retry 已到期
    let tid = "t901".to_string();
    state.tasks.lock().insert(
        tid.clone(),
        TaskRecord {
            task: DownloadTask {
                id: tid.clone(),
                canonical_id: CanonicalId {
                    kind: CanonicalKind::Http,
                    identity: "https://example.com/f.bin".into(),
                    validator: None,
                    token_sensitive: false,
                },
                source: DownloadSource::Http {
                    url: "https://example.com/f.bin".into(),
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
                state: TaskState::Queued,
                retry: RetryState {
                    retries: 0,
                    max_retries: 1,
                },
                created_at: std::time::Instant::now(),
                file_priorities: None,
                sequential: false,
                metadata: TaskMetadata {
                    name: None,
                    added_at_unix: 0,
                    tags: Vec::new(),
                    finished_at_unix: 0,
                    start_at_unix: 0,
                    next_retry_at_unix: 1, // 已到期
                },
                limits: None,
            },
            engine_tid: None,
            engine_kind: EngineKind::Http,
            engine_status: None,
            events: vec![],
        },
    );

    // add 仍败：重试预算消耗 → 再次安排（retries=1，继续等待）
    let activated = state.activate_due_tasks().await;
    assert!(activated.is_empty(), "重试轮仍失败不进 activated");
    {
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(rec.task.state, TaskState::Queued, "预算内 → 继续等待");
        assert_eq!(rec.task.retry.retries, 1);
        assert!(rec.task.metadata.next_retry_at_unix > 1, "新退避时刻已安排");
    }

    // 源恢复 → 到期重激活成功
    fake.unfail_url("https://example.com/f.bin");
    state
        .tasks
        .lock()
        .get_mut(&tid)
        .unwrap()
        .task
        .metadata
        .next_retry_at_unix = 1;
    let activated = state.activate_due_tasks().await;
    assert_eq!(activated, vec![tid.clone()]);
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    assert!(rec.engine_tid.is_some());
    assert_eq!(
        fake.added().len(),
        1,
        "恢复后重试 add 成功 1 次（首败在记录前）"
    );
}

#[tokio::test]
async fn snapshot_and_summary_surface_retry_fields() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task_opts(
            "https://example.com/f.bin".into(),
            None,
            AddHttpOpts {
                auto_retry: 3,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let snap = state.task_snapshot(&tid).await.unwrap();
    let json = serde_json::to_value(&snap).unwrap();
    assert!(json.get("retries").is_none(), "retries=0 序列化省略");
    assert_eq!(json["max_retries"], 3);
    assert!(
        json.get("next_retry_at_unix").is_none(),
        "无重试安排序列化省略"
    );

    // 安排重试后：三字段全透出
    state
        .tasks
        .lock()
        .get_mut(&tid)
        .unwrap()
        .fail_or_schedule_retry(Some("x"));
    let snap = state.task_snapshot(&tid).await.unwrap();
    let json = serde_json::to_value(&snap).unwrap();
    assert_eq!(json["retries"], 1);
    assert_eq!(json["max_retries"], 3);
    assert!(json["next_retry_at_unix"].as_u64().unwrap() > 0);

    let (summaries, _) = state.list_filtered(&ListQuery::default());
    let s = summaries.iter().find(|s| s.task_id == tid).unwrap();
    assert_eq!(s.retries, 1);
    assert_eq!(s.max_retries, 3);
}

#[tokio::test]
async fn resume_failed_without_handle_retries_manually() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    // 白盒建「预算耗尽 Failed」任务（无句柄——激活失败路径形态）
    let tid = "t902".to_string();
    state.tasks.lock().insert(
        tid.clone(),
        TaskRecord {
            task: DownloadTask {
                id: tid.clone(),
                canonical_id: CanonicalId {
                    kind: CanonicalKind::Http,
                    identity: "https://example.com/f.bin".into(),
                    validator: None,
                    token_sensitive: false,
                },
                source: DownloadSource::Http {
                    url: "https://example.com/f.bin".into(),
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
                state: TaskState::Failed,
                retry: RetryState {
                    retries: 1,
                    max_retries: 1,
                },
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
            engine_tid: None,
            engine_kind: EngineKind::Http,
            engine_status: None,
            events: vec![],
        },
    );

    // 手动重试：resume 成功 → 引擎重新接入 → Downloading
    state.resume(&tid).await.unwrap();
    {
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(rec.task.state, TaskState::Downloading(EngineKind::Http));
        assert!(rec.engine_tid.is_some(), "重试必须重建引擎句柄");
        assert_eq!(fake.added().len(), 1);
        assert!(
            rec.events.iter().any(|e| e.op == "retry"),
            "Failed 来源的 resume 必须落 retry 事件（区分普通恢复）: {:?}",
            rec.events.iter().map(|e| e.op.clone()).collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn resume_retry_keeps_exhausted_budget_no_infinite_loop() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task_opts(
            "https://example.com/f.bin".into(),
            None,
            AddHttpOpts {
                auto_retry: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // 首轮失败：预算内 → 重试安排；二轮失败：耗尽 → Failed（句柄保留）
    state.tasks.lock().get_mut(&tid).unwrap().task.state = TaskState::Downloading(EngineKind::Http);
    fake.set_status_state(EngineState::Error);
    state.poll_engine_states().await;
    state
        .tasks
        .lock()
        .get_mut(&tid)
        .unwrap()
        .task
        .metadata
        .next_retry_at_unix = 1;
    state.activate_due_tasks().await;
    state.tasks.lock().get_mut(&tid).unwrap().task.state = TaskState::Downloading(EngineKind::Http);
    state.poll_engine_states().await;
    assert_eq!(
        state.tasks.lock().get(&tid).unwrap().task.state,
        TaskState::Failed
    );

    // E32 手动重试（有句柄 → 引擎侧 resume 分支）→ 再失败：
    // auto_retry 预算【不重置】→ fail_or_schedule_retry 直接终态（无循环）
    state.resume(&tid).await.unwrap();
    assert_eq!(
        state.tasks.lock().get(&tid).unwrap().task.state,
        TaskState::Downloading(EngineKind::Http)
    );
    let effects = state.poll_engine_states().await;
    assert_eq!(effects[0].to, TaskState::Failed, "预算耗尽 → 不再自动重试");
    assert_eq!(
        state.tasks.lock().get(&tid).unwrap().task.retry.retries,
        1,
        "retries 停在 max，手动重试不白给预算"
    );
}
