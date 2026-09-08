//! batch7-P2：ban 重放失败脏标记重试测试（FakeEngine，不联网）。
//! 覆盖：重放单条失败挂入待重试集 / 引擎不可用全量入集 / 重试泵下发成功收敛 /
//! 用户 unban 后重试不再复活已解封条目。
#![cfg(test)]

use super::*;

/// 重放失败条目挂入待重试集，恢复后重试泵下发成功并清空集合。
#[tokio::test]
async fn replay_failure_enters_retry_set_and_retry_applies() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let state = DaemonState::new(fake.clone(), vec![]);
    *state.bt_bans.lock() = vec!["1.2.3.4".into(), "5.6.7.0-5.6.7.255".into()];
    // 注入区间条目下发失败（旧实现仅 warn 后整会话丢失）
    fake.fail_ban("5.6.7.0-5.6.7.255");

    state.replay_bans().await;
    assert_eq!(
        fake.bans(),
        vec!["1.2.3.4".to_string()],
        "未失败条目必须已下发引擎"
    );
    assert_eq!(
        *state.ban_replay_failed.lock(),
        vec!["5.6.7.0-5.6.7.255".to_string()],
        "失败条目必须挂入待重试集"
    );

    // 故障恢复：重试泵补发成功
    fake.unfail_ban("5.6.7.0-5.6.7.255");
    state.retry_pending_bans().await;
    assert!(fake.bans().contains(&"5.6.7.0-5.6.7.255".to_string()));
    assert!(
        state.ban_replay_failed.lock().is_empty(),
        "重试成功后待重试集必须清空"
    );
}

/// 引擎不可用时重放整体入集（不再静默放弃），待引擎可用后重试收敛。
#[tokio::test]
async fn replay_with_engine_unavailable_queues_all_entries() {
    // 只有 HTTP 引擎 → BT 引擎不可用（engine_for(Bt) Err）
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake, vec![]);
    *state.bt_bans.lock() = vec!["1.2.3.4".into(), "9.8.7.6".into()];

    state.replay_bans().await;
    assert_eq!(
        *state.ban_replay_failed.lock(),
        vec!["1.2.3.4".to_string(), "9.8.7.6".to_string()],
        "引擎不可用必须全量入集而非放弃"
    );

    // 引擎仍不可用：重试泵保集不动（下轮再试）
    state.retry_pending_bans().await;
    assert_eq!(state.ban_replay_failed.lock().len(), 2);
}

/// 用户在待重试期间解封：重试泵不得把已移除条目重新封回去（以 bt_bans
/// 权威表为准）。
#[tokio::test]
async fn retry_skips_entries_unbanned_by_user() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let state = DaemonState::new(fake.clone(), vec![]);
    *state.bt_bans.lock() = vec!["1.2.3.4".into(), "9.9.9.9".into()];
    fake.fail_ban("1.2.3.4");
    fake.fail_ban("9.9.9.9");
    state.replay_bans().await;
    assert_eq!(state.ban_replay_failed.lock().len(), 2);

    // 用户解封 9.9.9.9（直接改权威表 = unban 落盘后的内存终态）
    state.bt_bans.lock().retain(|b| b != "9.9.9.9");

    fake.unfail_ban("1.2.3.4");
    fake.unfail_ban("9.9.9.9");
    state.retry_pending_bans().await;

    assert!(
        fake.bans().contains(&"1.2.3.4".to_string()),
        "仍在权威表内的条目必须重试下发"
    );
    assert!(
        !fake.bans().contains(&"9.9.9.9".to_string()),
        "已解封条目不得被重试复活"
    );
    assert!(state.ban_replay_failed.lock().is_empty());
}
