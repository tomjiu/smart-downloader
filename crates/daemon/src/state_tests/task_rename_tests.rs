//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! E15 任务重命名：显示层改名 + 清除回退 + 入参校验（V3 终审同函数）。
//! 落盘路径在引擎 add 时已定，改名不迁移文件（显示/检索字段）。
#![cfg(test)]

use super::*;

#[tokio::test]
async fn rename_sets_name_list_search_and_event() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task("https://x.lan/f.bin".into(), None)
        .await
        .unwrap();

    state
        .set_task_name(&tid, Some("renamed.bin".into()))
        .unwrap();
    // 快照透出（E7 链路）
    let snap = state.task_snapshot(&tid).await.unwrap();
    assert_eq!(snap.name.as_deref(), Some("renamed.bin"));
    // 列表透出
    let list = state.list();
    assert!(list
        .iter()
        .any(|r| r.task_id == tid && r.name.as_deref() == Some("renamed.bin")));
    // E14 搜索语料联动：按新名命中（大小写不敏感）
    let (page, total) = state.list_filtered(&ListQuery {
        search: Some("RENAMED".into()),
        ..Default::default()
    });
    assert_eq!(total, 1);
    assert_eq!(page[0].task_id, tid);
    // 事件链（detail 只记 set/cleared，名字本体不进事件）
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    assert!(rec
        .events
        .iter()
        .any(|e| e.op == "name_changed" && e.detail.as_deref() == Some("set")));
}

#[tokio::test]
async fn rename_validation_clear_and_notfound() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task("https://x.lan/f.bin".into(), None)
        .await
        .unwrap();

    // 空白拒绝（清除语义由 None 承担）
    let e = state.set_task_name(&tid, Some("   ".into())).unwrap_err();
    assert!(matches!(e, DaemonError::InvalidSource(_)));
    // 非法路径分量拒绝（sanitize_rel 与 add 同一裁决点）
    let e = state
        .set_task_name(&tid, Some("../evil".into()))
        .unwrap_err();
    assert!(matches!(e, DaemonError::InvalidSource(_)));
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
        "非法改名必须零副作用"
    );

    // 设置 → 清除 → 快照 name 省略 + cleared 事件
    state.set_task_name(&tid, Some("tmp.bin".into())).unwrap();
    state.set_task_name(&tid, None).unwrap();
    let snap = state.task_snapshot(&tid).await.unwrap();
    assert_eq!(snap.name, None);
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    assert!(rec
        .events
        .iter()
        .any(|e| e.op == "name_changed" && e.detail.as_deref() == Some("cleared")));

    // 未知任务
    assert!(matches!(
        state.set_task_name("t404", Some("x".into())),
        Err(DaemonError::NotFound(_))
    ));
}
