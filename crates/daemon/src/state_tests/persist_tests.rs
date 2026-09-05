//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! 任务持久化往返测试（FakeEngine，不联网）。
#![cfg(test)]

use super::*;

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

#[tokio::test]
async fn persist_then_restore_keeps_task() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
    let tid = state
        .add_http_task("https://example.com/file.bin".into(), None)
        .await
        .unwrap();
    wait_file(&store, 2000);

    // 新 state（新引擎）恢复
    let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
    let state2 = DaemonState::new(fake2.clone(), vec![]);
    let n = state2.restore_from(&store).await.unwrap();
    assert_eq!(n, 1, "应恢复 1 条任务");
    let rec = state2.tasks.lock().get(&tid).cloned().unwrap();
    assert_eq!(rec.task.id, tid, "task_id 必须保留");
    assert_eq!(rec.engine_kind, EngineKind::Http);
    assert_eq!(rec.task.state, TaskState::Queued, "恢复后重新入队");
    // 引擎重新 add 被调用
    assert_eq!(
        fake2.added(),
        vec!["https://example.com/file.bin".to_string()]
    );
}

#[tokio::test]
async fn next_id_advances_after_restore() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
    let _ = state
        .add_http_task("https://example.com/a.bin".into(), None)
        .await
        .unwrap();
    let _ = state
        .add_http_task("https://example.com/b.bin".into(), None)
        .await
        .unwrap();
    wait_file(&store, 2000);

    let state2 = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![]);
    state2.restore_from(&store).await.unwrap();
    let new_tid = state2
        .add_http_task("https://example.com/c.bin".into(), None)
        .await
        .unwrap();
    let num: u64 = new_tid.strip_prefix('t').unwrap().parse().unwrap();
    assert!(num >= 3, "恢复后新任务 id 应跳过已用 id: {new_tid}");
}

#[tokio::test]
async fn restore_replays_task_limits_to_engine() {
    // 限速重放：持久化的 task.limits 在 restore 后原样下发引擎
    // （FakeEngine set_limits 记录（tid, down, up）三元组）。
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
    let tid = state
        .add_http_task("https://example.com/limited.bin".into(), None)
        .await
        .unwrap();
    let merged = state.set_task_limits(&tid, Some(128), None).await.unwrap();
    assert_eq!(merged.down_kb_s, Some(128));
    assert_eq!(merged.up_kb_s, None, "HTTP 任务 up 方向应保持未设");
    wait_file(&store, 2000);

    // 新 state（新引擎）恢复 → 引擎收到原样限速下发
    let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
    let state2 = DaemonState::new(fake2.clone(), vec![]);
    let n = state2.restore_from(&store).await.unwrap();
    assert_eq!(n, 1);
    let calls = fake2.limits();
    assert_eq!(
        calls.len(),
        1,
        "恢复后必须向引擎重放一次 set_limits: {calls:?}"
    );
    let (etid, down, up) = &calls[0];
    assert_eq!(down, &Some(128), "down 方向原样重放");
    assert_eq!(up, &None, "up 方向 None 原样传递（不触发方向预拒）");
    // 内存中的 limits 配置也随恢复保留
    let rec = state2.tasks.lock().get(&tid).cloned().unwrap();
    assert_eq!(
        rec.task.limits,
        Some(smart_dl_core::task::TaskLimits {
            down_kb_s: Some(128),
            up_kb_s: None,
        })
    );
    assert!(
        etid.starts_with("fk"),
        "engine_tid 应为 FakeEngine 返回的句柄: {etid}"
    );
}

#[cfg(feature = "bt")]
fn bt_prio_task(id: &str, prios: Option<Vec<u32>>) -> DownloadTask {
    let magnet = "magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    DownloadTask {
        id: id.to_string(),
        canonical_id: CanonicalId {
            kind: CanonicalKind::Bt,
            identity: magnet.to_string(),
            validator: None,
            token_sensitive: false,
        },
        source: DownloadSource::Magnet(magnet.to_string()),
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
        retry: Default::default(),
        created_at: std::time::Instant::now(),
        file_priorities: prios,
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
    }
}

#[cfg(feature = "bt")]
#[tokio::test]
async fn restore_replays_file_priorities_when_metadata_ready() {
    // metadata 已就绪（readback 非空表）：restore 必须直接全量重放一次
    // （.torrent 恢复场景；magnet 已解析场景同路）
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let pts = vec![PersistedTask {
        task: bt_prio_task("t1", Some(vec![0, 7])),
        engine_kind: EngineKind::Bt,
        paused: false,
    }];
    std::fs::write(&store, serde_json::to_vec(&pts).unwrap()).unwrap();

    let fake2 = Arc::new(FakeEngine::new(EngineKind::Bt));
    let state2 = DaemonState::new(fake2.clone(), vec![]);
    let n = state2.restore_from(&store).await.unwrap();
    assert_eq!(n, 1);
    let calls = fake2.prio_calls();
    assert_eq!(calls.len(), 1, "metadata 就绪时必须立即重放: {calls:?}");
    assert_eq!(
        calls[0].1,
        vec![(0, 0), (1, 7)],
        "下标-值对必须与持久化全量表一致"
    );
}

#[cfg(feature = "bt")]
#[tokio::test]
async fn file_priorities_pending_replay_converges_when_metadata_arrives() {
    // magnet 恢复且 metadata 未就绪（readback NotFound，与真实引擎分类一致）
    // → restore 挂 pending → 就绪后由 replay 收敛 → 再跑幂等
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    fake.set_prio_readback(Some(Err(smart_dl_core::types::EngineError::NotFound)));
    let state = Arc::new(DaemonState::new(fake.clone(), vec![]));

    let pts = vec![PersistedTask {
        task: bt_prio_task("t1", Some(vec![0, 7])),
        engine_kind: EngineKind::Bt,
        paused: false,
    }];
    std::fs::write(&store, serde_json::to_vec(&pts).unwrap()).unwrap();
    let n = state.restore_from(&store).await.unwrap();
    assert_eq!(n, 1);
    assert!(
        !fake.prio_calls().iter().any(|(_, p)| !p.is_empty()),
        "未就绪时不得成功重放: {:?}",
        fake.prio_calls()
    );
    assert!(
        state.pending_file_prio.lock().contains("t1"),
        "未就绪必须挂 pending 集合"
    );

    // metadata 到达（readback 返回 2 文件表）→ 单轮收敛
    fake.set_prio_readback(Some(Ok(vec![Some(0), Some(7)])));
    let done = state.replay_pending_file_priorities().await;
    assert_eq!(done, vec!["t1".to_string()]);
    let calls = fake.prio_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, vec![(0, 0), (1, 7)]);
    // pending 清空 + 幂等（再跑无调用）
    assert!(!state.pending_file_prio.lock().contains("t1"));
    assert!(state.replay_pending_file_priorities().await.is_empty());
    assert_eq!(fake.prio_calls().len(), 1);
}

#[tokio::test]
async fn restore_replays_pause_intent_and_marks_paused() {
    // P4 G5：用户暂停的任务持久化 paused=true → 重启恢复后重放 engine.pause
    // 且记录态回写 Paused（不再被当作运行任务重新入队）。
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
    let tid = state
        .add_http_task("https://example.com/paused.bin".into(), None)
        .await
        .unwrap();
    state.pause(&tid).await.unwrap();
    // pause 处理器应立即 autosave（否则暂停意图丢失）；轮询到 paused=true 落盘
    //（文件可能在 add 时已存在，不能只等文件存在）
    {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let persisted = std::fs::read_to_string(&store)
                .ok()
                .and_then(|s| serde_json::from_str::<Vec<PersistedTask>>(&s).ok())
                .map(|pts| pts.first().map(|p| p.paused).unwrap_or(false))
                .unwrap_or(false);
            if persisted {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "暂停意图必须落盘 paused=true"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    // "重启"：新引擎 + restore → 引擎收到 pause 重放，记录态 Paused
    let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
    let state2 = DaemonState::new(fake2.clone(), vec![]);
    state2.restore_from(&store).await.unwrap();
    assert_eq!(
        fake2.paused_calls(),
        vec!["fk1".to_string()],
        "暂停意图必须重放到引擎"
    );
    assert!(fake2.resumed_calls().is_empty(), "暂停任务不得 resume");
    let rec = state2.tasks.lock().get(&tid).cloned().unwrap();
    assert_eq!(rec.task.state, TaskState::Paused, "记录态应回写 Paused");
}

#[tokio::test]
async fn running_task_restored_without_pause_replay_on_non_bt() {
    // P4 G5 对称面：非暂停任务恢复不重放 pause；HTTP 引擎不得 resume
    //（add 已自启下载循环，重复 resume 会产生多余 epoch 循环）。
    // BT 的 resume 重放由 fastresume e2e（真实内核）覆盖。
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
    state
        .add_http_task("https://example.com/running.bin".into(), None)
        .await
        .unwrap();
    wait_file(&store, 2000);

    let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
    let state2 = DaemonState::new(fake2.clone(), vec![]);
    state2.restore_from(&store).await.unwrap();
    assert!(fake2.paused_calls().is_empty());
    assert!(fake2.resumed_calls().is_empty(), "HTTP 恢复不得重复 resume");
    let rec = state2.tasks.lock().values().next().cloned().unwrap();
    assert_eq!(rec.task.state, TaskState::Queued);
}

#[tokio::test]
async fn restore_add_failure_marks_failed() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
    let tid = state
        .add_http_task("https://example.com/gone.bin".into(), None)
        .await
        .unwrap();
    wait_file(&store, 2000);

    let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
    fake2.fail_url("https://example.com/gone.bin");
    let state2 = DaemonState::new(fake2.clone(), vec![]);
    let n = state2.restore_from(&store).await.unwrap();
    assert_eq!(n, 0, "add 失败不计入恢复数");
    let rec = state2.tasks.lock().get(&tid).cloned().unwrap();
    assert_eq!(rec.task.state, TaskState::Failed, "add 失败任务标 Failed");
    assert!(rec.engine_tid.is_none());
}

#[tokio::test]
async fn no_storage_no_autosave() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let _ = state
        .add_http_task("https://example.com/x.bin".into(), None)
        .await
        .unwrap();
    // 无 persist_path → 无写盘（autosave 直接 return）
    // 此测试验证不 panic；写盘路径由 with_storage 测试覆盖。
    assert!(fake.added().len() == 1);
}

#[tokio::test]
async fn dest_none_uses_default_dest_root() {
    // with_dest_root 注入默认目录后，dest 未指定 → 任务落默认目录（而非 daemon cwd）
    // Task 5-a：用临时目录替代硬编码 /data/default-dl（沙盒无 /data 写权限 → Permission denied）
    let tmp = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(tmp.path().to_path_buf());
    let tid = state
        .add_http_task("https://example.com/dest.bin".into(), None)
        .await
        .unwrap();
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    assert_eq!(
        rec.task.dest_root,
        tmp.path().to_path_buf(),
        "dest 未指定应落到默认 dest_root"
    );
}

#[tokio::test]
async fn explicit_dest_overrides_default() {
    // Task 5-a：默认 dest_root 同样改为临时目录（保持沙盒可跑）。
    // 安全修复（V2）：显式 dest 必须落在白名单内——用 dest_root 子目录验证
    // 「显式 dest 优先于默认」契约保持；白名单外由 reject_dest_outside_roots 覆盖。
    let tmp = tempfile::tempdir().unwrap();
    let sub = tmp.path().join("custom");
    std::fs::create_dir_all(&sub).unwrap();
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(tmp.path().to_path_buf());
    let tid = state
        .add_http_task(
            "https://example.com/override.bin".into(),
            Some(sub.to_string_lossy().into_owned()),
        )
        .await
        .unwrap();
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    assert_eq!(rec.task.dest_root, sub, "显式 dest（白名单内）优先于默认");
}

#[tokio::test]
async fn reject_dest_outside_roots() {
    // 安全回归（V2）：显式 dest 在白名单外 → 拒绝任务（不再任意目录可写）。
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap(); // 白名单外独立目录
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(tmp.path().to_path_buf());
    let r = state
        .add_http_task(
            "https://example.com/escape.bin".into(),
            Some(outside.path().to_string_lossy().into_owned()),
        )
        .await;
    assert!(
        matches!(&r, Err(DaemonError::InvalidSource(m)) if m.contains("越界")),
        "白名单外 dest 必须拒绝: {r:?}"
    );
}

/// Bug B 回归（重入自死锁，HTTP 推进路径）：storage 启用 + 引擎状态推进
/// （Queued → Downloading）触发锁内 autosave。修复前：poll_engine_states
/// 持 tasks 锁调 autosave → persisted_tasks 同线程重入 → 5s 超时失败。
#[tokio::test(flavor = "multi_thread")]
async fn http_poll_transition_with_storage_autosave_no_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]).with_storage(store.clone());
    let _tid = state
        .add_http_task("https://example.com/wedge.bin".into(), None)
        .await
        .unwrap();
    // FakeEngine::status 默认 MetadataPending → 推进 Queued→Downloading(Http)
    //（迁移必发生 → 必走 autosave 路径）
    let work = state.poll_engine_states();
    let effects = tokio::time::timeout(std::time::Duration::from_secs(5), work)
        .await
        .expect("poll_engine_states 死锁（Bug B 重入回归）");
    assert_eq!(effects.len(), 1, "应推进一条任务状态");
    assert!(store.exists(), "状态推进应触发持久化落盘");
}
