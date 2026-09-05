//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! E9 名字回填：引擎 status().name（落盘名决策结果）→ 轮询回填
//! metadata.name（空缺时）→ 列表/快照透出链。显式名权威不被覆盖；幂等。
#![cfg(test)]

use super::*;

#[tokio::test]
async fn poll_backfills_derived_name_once() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task("https://example.com/f.bin".into(), None)
        .await
        .unwrap();
    assert!(
        state
            .tasks
            .lock()
            .get(&tid)
            .unwrap()
            .task
            .metadata
            .name
            .is_none(),
        "前置：无显式名任务 metadata.name 为空"
    );
    fake.set_status_name("cd-derived.bin");

    // 首轮：迁移（Queued→Downloading，FakeEngine 恒 MetadataPending）+ 回填
    let effects = state.poll_engine_states().await;
    assert_eq!(effects.len(), 1, "首轮应有状态迁移");
    {
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(
            rec.task.metadata.name.as_deref(),
            Some("cd-derived.bin"),
            "引擎报的派生名必须回填 metadata.name"
        );
        assert!(
            rec.events.iter().any(|e| e.op == "name_backfilled"),
            "回填必须留事件痕迹"
        );
    }

    // 幂等：次轮零迁移零重复回填（to==from 纯无操作）
    let effects2 = state.poll_engine_states().await;
    assert!(effects2.is_empty(), "to==from 纯回填轮次不产生迁移广播");
    let rec2 = state.tasks.lock().get(&tid).cloned().unwrap();
    assert_eq!(rec2.task.metadata.name.as_deref(), Some("cd-derived.bin"));
    let n = rec2
        .events
        .iter()
        .filter(|e| e.op == "name_backfilled")
        .count();
    assert_eq!(n, 1, "回填事件恰好一次（幂等）");

    // E7 透出链：列表条目 name 生效
    let list = state.list();
    assert_eq!(list[0].name.as_deref(), Some("cd-derived.bin"));
}

#[tokio::test]
async fn poll_never_overrides_explicit_name() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task_opts(
            "https://example.com/f.bin".into(),
            None,
            AddHttpOpts {
                name: Some("explicit.bin".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fake.set_status_name("engine-name.bin");

    state.poll_engine_states().await;
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    assert_eq!(
        rec.task.metadata.name.as_deref(),
        Some("explicit.bin"),
        "显式名权威——引擎回显同值也不覆盖"
    );
    assert!(
        !rec.events.iter().any(|e| e.op == "name_backfilled"),
        "显式名任务无回填事件"
    );
}

#[tokio::test]
async fn poll_skips_tasks_with_engine_silent_name() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task("https://example.com/f.bin".into(), None)
        .await
        .unwrap();
    // 引擎不透出 name（status_name 恒 None）→ 不回填不事件
    state.poll_engine_states().await;
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    assert!(rec.task.metadata.name.is_none(), "引擎未报 → 保持 None");
    assert!(!rec.events.iter().any(|e| e.op == "name_backfilled"));
}
