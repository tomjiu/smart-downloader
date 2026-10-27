//! 体检 #182 C-1 回归：队列优先级持久透出（TaskSnapshot / TaskSummary）。
//! 原实现仅 set 响应回显——快照/列表无该字段，UI 重启后无法回显。
#![cfg(test)]

use super::*;

/// set 后快照与列表均透出 queue_priority；0（默认）序列化省略（向后兼容）。
#[tokio::test]
async fn queue_priority_exposed_in_snapshot_and_summary() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);

    let t1 = state
        .add_http_task("https://srv/prio-a.bin".into(), None)
        .await
        .unwrap();
    let t2 = state
        .add_http_task("https://srv/prio-b.bin".into(), None)
        .await
        .unwrap();

    // 默认 0：省略（序列化层断言）
    let snap = state.task_snapshot(&t1).await.expect("snapshot");
    assert_eq!(snap.queue_priority, 0);
    let json = serde_json::to_value(&snap).unwrap();
    assert!(
        json.get("queue_priority").is_none(),
        "默认 0 必须省略（向后兼容）: {json}"
    );

    // top：t1 顶置 → -1（数值越小越先出队）
    let v = state
        .set_task_priority(&t1, None, Some("top".into()))
        .await
        .unwrap();
    assert_eq!(v, -1);

    let snap = state.task_snapshot(&t1).await.expect("snapshot");
    assert_eq!(snap.queue_priority, -1);
    let json = serde_json::to_value(&snap).unwrap();
    assert_eq!(json["queue_priority"], -1, "set 后快照必须透出");

    // 列表口径同拍
    let (page, _total) = state.list_filtered(&ListQuery::default());
    let row = page.iter().find(|s| s.task_id == t1).expect("list row");
    assert_eq!(row.queue_priority, -1);
    let row_json = serde_json::to_value(row).unwrap();
    assert_eq!(row_json["queue_priority"], -1);

    // 未设置的任务仍省略
    let row2 = page.iter().find(|s| s.task_id == t2).expect("list row 2");
    let row2_json = serde_json::to_value(row2).unwrap();
    assert!(row2_json.get("queue_priority").is_none());

    // 绝对值设置（value 优先于 action 语义）也透出
    state.set_task_priority(&t2, Some(7), None).await.unwrap();
    let snap2 = state.task_snapshot(&t2).await.expect("snapshot 2");
    assert_eq!(snap2.queue_priority, 7);
}
