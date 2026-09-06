//! S1-b 并发队列门控测试（FakeEngine，不联网）。
//! 覆盖：配额满落队 / FIFO 递补 / 0 = 不限（向后兼容）/ 手动 resume 强制
//！开始 / 定时任务到点仍受配额 / Seeding 不占下载槽 / 引擎间配额隔离（ftp 门）。
#![cfg(test)]

use super::*;

fn queue_cfg(bt: u32, http: u32, ftp: u32) -> crate::config::QueueCfg {
    crate::config::QueueCfg {
        max_active_bt: bt,
        max_active_http: http,
        max_active_ftp: ftp,
    }
}

/// 主链路：HTTP 配额 1 → 第 2/3 个任务排队；逐个释放槽位后按 FIFO 递补。
#[tokio::test]
async fn queue_gate_http_over_quota_enqueues_and_refills_fifo() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]).with_queue_cfg(queue_cfg(0, 1, 0));

    let t1 = state
        .add_http_task("https://srv/a1.bin".into(), None)
        .await
        .unwrap();
    let t2 = state
        .add_http_task("https://srv/a2.bin".into(), None)
        .await
        .unwrap();
    let t3 = state
        .add_http_task("https://srv/a3.bin".into(), None)
        .await
        .unwrap();

    // t1 立即入引擎；t2/t3 排队（无句柄、Queued、带 queue_wait 事件）
    {
        let tasks = state.tasks.lock();
        assert!(tasks.get(&t1).unwrap().engine_tid.is_some());
        let r2 = tasks.get(&t2).unwrap();
        assert!(r2.engine_tid.is_none());
        assert_eq!(r2.task.state, TaskState::Queued);
        assert!(
            r2.events
                .iter()
                .any(|e| e.op == "add" && e.detail.as_deref().unwrap_or("").contains("queue_wait")),
            "排队任务必须带 queue_wait 事件: {:?}",
            r2.events
        );
        assert!(tasks.get(&t3).unwrap().engine_tid.is_none());
    }

    // 释放一槽（暂停 t1）→ 只有队首 t2 递补
    state.pause(&t1).await.unwrap();
    let act = state.activate_due_tasks().await;
    assert_eq!(act, vec![t2.clone()], "递补必须 FIFO：先 t2 后 t3");
    assert!(state.tasks.lock().get(&t2).unwrap().engine_tid.is_some());
    assert!(state.tasks.lock().get(&t3).unwrap().engine_tid.is_none());

    // 再释放一槽 → t3
    state.pause(&t2).await.unwrap();
    let act = state.activate_due_tasks().await;
    assert_eq!(act, vec![t3.clone()]);
    assert!(state.tasks.lock().get(&t3).unwrap().engine_tid.is_some());
}

/// 0 = 不限（默认配置）：连发多任务全部立即入引擎（升级前行为零变化）。
#[tokio::test]
async fn queue_gate_zero_quota_means_unlimited() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    // 默认 QueueCfg = 0/0/0（不显式配置即不限）
    let state = DaemonState::new(fake.clone(), vec![]);
    for i in 0..4 {
        state
            .add_http_task(format!("https://srv/u{i}.bin"), None)
            .await
            .unwrap();
    }
    let tasks = state.tasks.lock();
    assert!(
        tasks.values().all(|r| r.engine_tid.is_some()),
        "配额 0 必须全部直接入引擎"
    );
}

/// 手动 resume = 强制开始：配额满也立即激活（对齐 qbit 强制继续语义）。
#[tokio::test]
async fn queue_gate_manual_resume_forces_start_over_quota() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]).with_queue_cfg(queue_cfg(0, 1, 0));
    let t1 = state
        .add_http_task("https://srv/a1.bin".into(), None)
        .await
        .unwrap();
    let t2 = state
        .add_http_task("https://srv/a2.bin".into(), None)
        .await
        .unwrap();
    assert!(state.tasks.lock().get(&t2).unwrap().engine_tid.is_none());

    // 配额仍满（t1 在传）→ 手动 resume 跳过闸门强制激活
    state.resume(&t2).await.unwrap();
    assert!(state.tasks.lock().get(&t2).unwrap().engine_tid.is_some());

    // 清理：t1 仍占槽（slot 计数含 t1/t2 两个在传——超卖由用户显式意图保证）
    assert!(state.tasks.lock().get(&t1).unwrap().engine_tid.is_some());
}

/// 定时任务到点仍受配额：到期但槽位不足 → 继续等；出槽后递补。
#[tokio::test]
async fn queue_gate_scheduled_due_waits_for_slot() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]).with_queue_cfg(queue_cfg(0, 1, 0));
    let t1 = state
        .add_http_task("https://srv/a1.bin".into(), None)
        .await
        .unwrap();

    // t2 定时在未来 → 落调度记录（无句柄）
    let t2 = state
        .add_http_task_opts(
            "https://srv/a2.bin".into(),
            None,
            AddHttpOpts {
                start_at_unix: Some(now_unix() + 3600),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(state.tasks.lock().get(&t2).unwrap().engine_tid.is_none());

    // 时间到点（直接拨 start_at 到过去）但配额满（t1 在传）→ 不激活
    state
        .tasks
        .lock()
        .get_mut(&t2)
        .unwrap()
        .task
        .metadata
        .start_at_unix = now_unix() - 1;
    let act = state.activate_due_tasks().await;
    assert!(act.is_empty(), "到期但槽位不足必须继续等待");
    assert!(state.tasks.lock().get(&t2).unwrap().engine_tid.is_none());

    // 出槽 → 递补
    state.pause(&t1).await.unwrap();
    let act = state.activate_due_tasks().await;
    assert_eq!(act, vec![t2.clone()]);
}

/// Seeding 不占下载槽：BT 完成转做种后下载配额即刻释放（FTP fake 模拟，
/// 槽位计数逻辑与引擎种类无关——直接驱动 active_slot_counts 口径）。
#[tokio::test]
async fn queue_gate_seeding_does_not_hold_download_slot() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]).with_queue_cfg(queue_cfg(0, 1, 0));
    let t1 = state
        .add_http_task("https://srv/a1.bin".into(), None)
        .await
        .unwrap();
    assert_eq!(state.active_slot_counts(), [0, 1, 0]);

    // 模拟任务转入 Seeding（有句柄、非传输态）
    state.tasks.lock().get_mut(&t1).unwrap().task.state = TaskState::Seeding;
    assert_eq!(
        state.active_slot_counts(),
        [0, 0, 0],
        "Seeding 必须释放下载槽"
    );

    // 新任务直接入引擎（不被排队）
    let t2 = state
        .add_http_task("https://srv/a2.bin".into(), None)
        .await
        .unwrap();
    assert!(state.tasks.lock().get(&t2).unwrap().engine_tid.is_some());
}

/// 引擎间配额隔离（feature ftp）：HTTP 满 → FTP 任务不受影响（各自配额）。
#[cfg(feature = "ftp")]
#[tokio::test]
async fn queue_gate_engine_kinds_isolated() {
    let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let ftp_fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
    let state = DaemonState::new(http_fake.clone(), vec![])
        .with_ftp(ftp_fake.clone())
        .with_queue_cfg(queue_cfg(0, 1, 0));

    let h1 = state
        .add_http_task("https://srv/a1.bin".into(), None)
        .await
        .unwrap();
    let h2 = state
        .add_http_task("https://srv/a2.bin".into(), None)
        .await
        .unwrap();
    assert!(state.tasks.lock().get(&h1).unwrap().engine_tid.is_some());
    assert!(state.tasks.lock().get(&h2).unwrap().engine_tid.is_none());

    // HTTP 配额满，但 FTP 配额独立（0 = 不限）→ FTP 任务立即入引擎
    let f1 = state
        .add_ftp_task("ftp://srv/f1.bin".into(), None)
        .await
        .unwrap();
    assert!(
        state.tasks.lock().get(&f1).unwrap().engine_tid.is_some(),
        "FTP 配额独立于 HTTP，不得被 HTTP 满槽连坐"
    );
}
