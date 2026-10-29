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
        max_active_total: 0,
    }
}

/// B6（#214 §三-10）总量闸简写：分引擎全不限 + 全局总量 = total。
/// 仅跨引擎用例使用（ftp 特性门控）。
#[cfg(feature = "ftp")]
fn queue_cfg_total(total: u32) -> crate::config::QueueCfg {
    crate::config::QueueCfg {
        max_active_total: total,
        ..queue_cfg(0, 0, 0)
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

// ==================== batch7-P1：预留槽位（add 在途窗口 TOCTOU 根治） ====================

/// 并发 add 不超卖：配额 1 + 引擎 add 延迟 100ms，6 个并发 add 只有 1 个
/// 过闸入引擎，其余全部落队。旧实现（记录 add 成功后才插入）在延迟窗口内
/// 全部过闸 → 6 连超卖，本用例回归锚定。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slot_reservation_closes_inflight_oversell() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    fake.set_add_delay_ms(100);
    let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_queue_cfg(queue_cfg(0, 1, 0)));

    let mut futs = Vec::new();
    for i in 0..6 {
        let st = state.clone();
        futs.push(tokio::spawn(async move {
            st.add_http_task(format!("https://srv/c{i}.bin"), None)
                .await
                .unwrap()
        }));
    }
    let mut ids = Vec::new();
    for f in futs {
        ids.push(f.await.unwrap());
    }

    let (active, queued) = {
        let tasks = state.tasks.lock();
        let active = tasks.values().filter(|r| r.engine_tid.is_some()).count();
        let queued = tasks
            .values()
            .filter(|r| r.engine_tid.is_none() && r.task.state == TaskState::Queued)
            .count();
        (active, queued)
    };
    assert_eq!(active, 1, "配额 1 必须只放行 1 个任务入引擎（超卖即缺陷）");
    assert_eq!(queued, 5, "其余任务必须全部落队");
    assert_eq!(fake.added().len(), 1, "引擎侧不得收到超额任务");
    assert_eq!(ids.len(), 6);
    // 预留集必须已全部摘除（add 均已完成 attach/落队）
    assert!(state.slot_reservations.lock().is_empty());
}

/// engine.add 失败 → 预留回滚：占位记录删除（对齐旧「add 失败 = 无记录」
/// 语义），槽位计数归零，后续 add 不被残留占位卡死。
#[tokio::test]
async fn slot_rollback_on_engine_add_failure() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]).with_queue_cfg(queue_cfg(0, 1, 0));
    fake.fail_url("https://srv/bad.bin");

    let err = state
        .add_http_task("https://srv/bad.bin".into(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, DaemonError::Engine(_)));
    // 占位记录必须回滚删除（空状态不得残留任何记录）+ 预留集摘除
    assert!(state.tasks.lock().is_empty(), "add 失败不得残留占位记录");
    assert!(state.slot_reservations.lock().is_empty());
    assert_eq!(state.active_slot_counts()[1], 0, "失败任务不得占槽");

    // 恢复后同一闸门可正常放行（无幽灵占位）
    fake.unfail_url("https://srv/bad.bin");
    let tid = state
        .add_http_task("https://srv/bad.bin".into(), None)
        .await
        .unwrap();
    assert!(state.tasks.lock().get(&tid).unwrap().engine_tid.is_some());
}

/// add 在途窗口 resume = 幂等成功（不重复 engine.add，不产生双引擎任务）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_during_reservation_is_idempotent() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    fake.set_add_delay_ms(150);
    let state = Arc::new(DaemonState::new(fake.clone(), vec![]));

    let st = state.clone();
    let adder = tokio::spawn(async move {
        st.add_http_task("https://srv/res.bin".into(), None)
            .await
            .unwrap()
    });
    // 等待进入预留窗口（占位记录已插入，engine.add 还在 sleep）
    let mut tid = String::new();
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let reserved = state.slot_reservations.lock();
        if let Some(id) = reserved.iter().next() {
            tid = id.clone();
            break;
        }
    }
    assert!(!tid.is_empty(), "必须先进入预留窗口");
    // 在途窗口 resume：幂等 Ok，不得二次 engine.add
    state.resume(&tid).await.unwrap();
    // 等 add 全链收尾
    adder.await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        fake.added().len(),
        1,
        "resume 不得在预留窗口内重复激活（双引擎任务）"
    );
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    assert!(rec.engine_tid.is_some());
    // batch3-P1 口径：HTTP add 后记录态留 Queued（首轮轮询迁移 Downloading）；
    // 断言核心 = 有句柄且无重复引擎任务
    assert_eq!(rec.task.state, TaskState::Queued);
}

// ==================== PR #114 催修 P0-2：锁序统一（tasks → slot_reservations） ====================

/// 并发 gate / attach / rollback / 计数混跑不得死锁（锁序回归锚定）。
///
/// parking_lot::Mutex 不可重入、无超时——锁序反转死锁即永久挂死。
/// 修复前 attach_reserved_handle / rollback_slot_reservation 语义上为
/// 反向锁序（slot_res → tasks），与 gate_or_enqueue / 递补筛选 /
/// active_slot_counts（tasks → slot_res）方向相反；修复后统一为
/// tasks → slot_reservations（摘除与落位/回滚并入同一 tasks 临界区）。
///
/// 本测试用 timeout 兜底：死锁时以超时失败暴露（而非挂死 CI），正常时
/// 秒级完成。四类路径混跑最大化两把锁的交叉获取窗口：
/// - 正常 add（gate → engine.add(5ms 延迟) → attach_reserved_handle）
/// - 失败 add（gate → engine.add 失败 → rollback_slot_reservation）
/// - active_slot_counts（快照读，tasks → slot_res 同向）
/// - 在途 resume（另一 tasks 临界区 + 预留集 contains 检查）
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_gate_attach_rollback_no_deadlock() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    // 5ms 延迟扩大 gate（计数+登记）与 attach（摘除+落位）的重叠窗口
    fake.set_add_delay_ms(5);
    let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_queue_cfg(queue_cfg(0, 4, 0)));
    for i in 0..12 {
        fake.fail_url(&format!("https://srv/bad{i}.bin"));
    }

    let done = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        // ① 正常 add ×24（配额 4：gate 计数与 attach/落队高频交叉）
        let mut add_futs = Vec::new();
        for i in 0..24 {
            let st = state.clone();
            add_futs.push(tokio::spawn(async move {
                st.add_http_task(format!("https://srv/ok{i}.bin"), None)
                    .await
                    .unwrap()
            }));
        }
        // ② 失败 add ×12（rollback 路径）
        let mut bad_futs = Vec::new();
        for i in 0..12 {
            let st = state.clone();
            bad_futs.push(tokio::spawn(async move {
                let _ = st
                    .add_http_task(format!("https://srv/bad{i}.bin"), None)
                    .await;
            }));
        }
        // ③ 快照计数 ×12（同向锁序的读路径，参与竞争）
        let mut cnt_futs = Vec::new();
        for _ in 0..12 {
            let st = state.clone();
            cnt_futs.push(tokio::spawn(async move {
                let _ = st.active_slot_counts();
            }));
        }
        let mut ids = Vec::new();
        for f in add_futs {
            ids.push(f.await.unwrap());
        }
        for f in bad_futs {
            f.await.unwrap();
        }
        for f in cnt_futs {
            f.await.unwrap();
        }
        // ④ 收尾后在途窗口外的 resume（activate_one → tasks 临界区）
        for id in ids.iter().take(4) {
            let _ = state.resume(id).await;
        }
        ids
    })
    .await;

    let ids = done.expect(
        "60s 内未完成：gate/attach/rollback 并发死锁（锁序 tasks → slot_reservations 回归）",
    );
    // 收尾一致性：预留集必须清空（attach/rollback 全部走完）
    assert!(
        state.slot_reservations.lock().is_empty(),
        "并发收尾后预留集必须清空"
    );
    // 24 个正常 add 全部返回（入引擎或落队，皆有记录）
    assert_eq!(ids.len(), 24);
    // 引擎侧绝无 bad URL（fail_url 任务永不入引擎，无论过闸失败还是落队）
    let added = fake.added();
    assert!(
        added.iter().all(|u| u.starts_with("https://srv/ok")),
        "失败 URL 不得进入引擎: {added:?}"
    );
    // 任务记录数：正常 24 全保留 + bad 过闸失败即删（0 条）/ 落队即保留
    //（≤12 条，配额时序决定）→ 确定性区间 [24, 36]
    let len = state.tasks.lock().len();
    assert!(
        (24..=36).contains(&len),
        "任务记录数异常（应为 24 正常 + 落队 bad）: {len}"
    );
}

// ==================== B6（#214 §三-10）：跨引擎全局总量闸 ====================

/// 总量闸比分引擎配额更紧时由总量决定排队：分引擎配额 5（宽松），总量 1
/// → 第 2/3 个任务排队，queue_wait 事件必须点名「全局并发总量」（而非
/// 引擎配额——排队原因可观测性）；释放唯一总量槽后 FIFO 递补一个。
#[tokio::test]
async fn total_gate_tighter_than_engine_quota_reports_global_wait() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]).with_queue_cfg(crate::config::QueueCfg {
        max_active_http: 5,
        max_active_total: 1,
        ..queue_cfg(0, 0, 0)
    });

    let t1 = state
        .add_http_task("https://srv/g1.bin".into(), None)
        .await
        .unwrap();
    let t2 = state
        .add_http_task("https://srv/g2.bin".into(), None)
        .await
        .unwrap();
    let t3 = state
        .add_http_task("https://srv/g3.bin".into(), None)
        .await
        .unwrap();

    // t1 入引擎；t2/t3 被总量闸排队（引擎配额 5 未触——消息须区分）
    {
        let tasks = state.tasks.lock();
        assert!(tasks.get(&t1).unwrap().engine_tid.is_some());
        for (id, label) in [(&t2, "t2"), (&t3, "t3")] {
            let r = tasks.get(id).unwrap();
            assert!(r.engine_tid.is_none(), "{label} 应被总量闸排队");
            assert!(
                r.events
                    .iter()
                    .any(|e| e.op == "add"
                        && e.detail.as_deref().unwrap_or("").contains("全局并发总量")),
                "{label} queue_wait 事件须点名全局总量（排队原因可观测）: {:?}",
                r.events
            );
        }
    }

    // 释放唯一总量槽（暂停 t1）→ FIFO 递补 t2；t3 仍排队（总量复满）
    state.pause(&t1).await.unwrap();
    let act = state.activate_due_tasks().await;
    assert_eq!(act, vec![t2.clone()], "总量 1：递补后复满，仅 t2 激活");
    assert!(state.tasks.lock().get(&t3).unwrap().engine_tid.is_none());
}

/// 跨引擎总量口径（feature ftp）：分引擎配额全不限，总量 2——HTTP 1 +
/// FTP 1 在传即满，第 3 个（任意引擎）排队；释放 FTP 槽后 HTTP 排队任务
/// 递补（总量槽跨引擎共享，不按引擎归属——qB「全局上限」语义）。
#[cfg(feature = "ftp")]
#[tokio::test]
async fn total_gate_cross_engine_shared_slots() {
    let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let ftp_fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
    let state = DaemonState::new(http_fake.clone(), vec![])
        .with_ftp(ftp_fake.clone())
        .with_queue_cfg(queue_cfg_total(2));

    let h1 = state
        .add_http_task("https://srv/h1.bin".into(), None)
        .await
        .unwrap();
    let f1 = state
        .add_ftp_task("ftp://srv/f1.bin".into(), None)
        .await
        .unwrap();
    assert!(state.tasks.lock().get(&h1).unwrap().engine_tid.is_some());
    assert!(state.tasks.lock().get(&f1).unwrap().engine_tid.is_some());

    // 总量 2 已满（h1+f1）→ 第 3 个 HTTP 任务排队（引擎配额不限，纯总量闸）
    let h2 = state
        .add_http_task("https://srv/h2.bin".into(), None)
        .await
        .unwrap();
    let r2 = state.tasks.lock().get(&h2).unwrap().clone();
    assert!(r2.engine_tid.is_none(), "总量满：跨引擎之和 2 = 上限");
    assert!(
        r2.events
            .iter()
            .any(|e| e.op == "add" && e.detail.as_deref().unwrap_or("").contains("全局并发总量")),
        "queue_wait 事件须点名全局总量: {:?}",
        r2.events
    );

    // 释放 FTP 槽 → 排队的 HTTP 任务递补（总量槽不分引擎归属）
    state.pause(&f1).await.unwrap();
    let act = state.activate_due_tasks().await;
    assert_eq!(
        act,
        vec![h2.clone()],
        "跨引擎递补：释放的是 FTP 槽，递补的是 HTTP 任务"
    );
    assert!(state.tasks.lock().get(&h2).unwrap().engine_tid.is_some());
}
