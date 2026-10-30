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

// ==================== B7（#214 §三-7）：IP 过滤 URL 订阅 ====================

/// 可变内容订阅源（axum）：refresh 测试用——两次拉取间改内容验证 diff。
async fn serve_mutable_p2p() -> (String, Arc<parking_lot::Mutex<String>>) {
    let content = Arc::new(parking_lot::Mutex::new(String::new()));
    let app = {
        let content = content.clone();
        axum::Router::new().route(
            "/list.p2p",
            axum::routing::get(move || {
                let content = content.clone();
                async move {
                    let body = content.lock().clone();
                    axum::response::Response::builder()
                        .status(axum::http::StatusCode::OK)
                        .body(axum::body::Body::from(body))
                        .unwrap()
                }
            }),
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/list.p2p"), content)
}

/// 全链路：拉取应用 → 手动封禁分账 → diff 增删 → 部分失败重试 → 禁用清除。
/// FakeEngine(Bt) 暴露完整 BT 扩展面（ban/unban/区间），逻辑与真引擎同构。
#[tokio::test]
async fn ip_filter_refresh_applies_diffs_and_clears() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::new(fake.clone(), vec![])
        .with_storage(dir.path().join("tasks.json"))
        .with_live_config(crate::config::Config::default());
    let (url, content) = serve_mutable_p2p().await;

    // 预置手动封禁（分账语义：订阅刷新永不动它）
    *state.bt_bans.lock() = vec!["9.9.9.9".into()];

    // ---- 第一轮：两条区间 + 一条单 IP 生效 ----
    *content.lock() =
        "# level1\n1.2.3.0-1.2.3.255,bad isp\nrange:5.6.7.0-5.6.7.255,xx\n7.7.7.7\ngarbage-line\n"
            .into();
    let n = state.refresh_ip_filter(&url).await.unwrap();
    assert_eq!(n, 3, "两条区间 + 一条单 IP（garbage 行跳过）");
    {
        let bans = state.bt_bans.lock().clone();
        for e in [
            "1.2.3.0-1.2.3.255",
            "5.6.7.0-5.6.7.255",
            "7.7.7.7",
            "9.9.9.9",
        ] {
            assert!(bans.contains(&e.to_string()), "缺 {e}: {bans:?}");
        }
    }
    assert!(fake.bans().contains(&"1.2.3.0-1.2.3.255".to_string()));
    {
        let st = state.ip_filter.lock();
        assert_eq!(st.entries.len(), 3);
        assert_eq!(st.source_url, url);
        assert!(st.last_ok_unix > 0);
        // garbage 行 = 部分失败摘要（诚实回显；不阻断整体应用）
        assert!(
            st.last_error.as_deref().unwrap_or("").contains("无法解析"),
            "部分行失败须记账: {:?}",
            st.last_error
        );
    }
    // ip_filter.json 已落盘（重启 diff 基准）
    let ipf_text = std::fs::read_to_string(dir.path().join("ip_filter.json")).unwrap();
    assert!(ipf_text.contains("1.2.3.0-1.2.3.255"), "归属基准须持久化");

    // ---- 第二轮：上游删 5.6.7 段 + 加 8.8.8.0 段 → diff 增删 ----
    *content.lock() = "1.2.3.0-1.2.3.255,bad isp\n8.8.8.0-8.8.8.255,yy\n7.7.7.7\n".into();
    let n = state.refresh_ip_filter(&url).await.unwrap();
    assert_eq!(n, 3);
    {
        let bans = state.bt_bans.lock().clone();
        assert!(
            !bans.contains(&"5.6.7.0-5.6.7.255".to_string()),
            "上游删除须同步解除"
        );
        assert!(
            bans.contains(&"8.8.8.0-8.8.8.255".to_string()),
            "上游新增须生效"
        );
        assert!(
            bans.contains(&"9.9.9.9".to_string()),
            "手动封禁永不受订阅影响"
        );
    }
    assert!(!fake.bans().contains(&"5.6.7.0-5.6.7.255".to_string()));
    // bans.json 同步落盘
    let bans_disk = std::fs::read_to_string(dir.path().join("bans.json")).unwrap();
    assert!(bans_disk.contains("8.8.8.0-8.8.8.255") && !bans_disk.contains("5.6.7.0"));

    // ---- 第三轮：部分引擎失败 → 条目不入表 + last_error 记账 + 下轮重试 ----
    fake.fail_ban("6.6.6.0-6.6.6.255");
    *content.lock() = "1.2.3.0-1.2.3.255,bad isp\n6.6.6.0-6.6.6.255,zz\n7.7.7.7\n".into();
    let n = state.refresh_ip_filter(&url).await.unwrap();
    assert_eq!(n, 2, "失败条目不入生效集（下轮 diff 重试）");
    {
        let st = state.ip_filter.lock();
        assert!(st.last_error.as_deref().unwrap_or("").contains("引擎失败"));
    }
    // 故障恢复 → 重试收敛（diff 视角：6.6.6 段仍在 entries 差集外 → 重新加）
    fake.unfail_ban("6.6.6.0-6.6.6.255");
    let n = state.refresh_ip_filter(&url).await.unwrap();
    assert_eq!(n, 3, "恢复后重试成功");
    assert!(state
        .bt_bans
        .lock()
        .contains(&"6.6.6.0-6.6.6.255".to_string()));

    // ---- 禁用：清除订阅条目，手动封禁保留 ----
    state.clear_ip_filter_subscription().await;
    {
        let bans = state.bt_bans.lock().clone();
        assert_eq!(bans, vec!["9.9.9.9".to_string()], "只清订阅条目");
    }
    let st = state.ip_filter.lock();
    assert!(st.entries.is_empty() && st.source_url.is_empty() && st.last_ok_unix == 0);
    assert!(!fake.bans().contains(&"1.2.3.0-1.2.3.255".to_string()));
}

/// 拉取失败保持：源 404 → Err + last_error 记账 + 条目/引擎不动（#160 防假阳性
/// ——瞬时故障绝不清空防线）。
#[tokio::test]
async fn ip_filter_refresh_bad_source_keeps_last_applied() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let state =
        DaemonState::new(fake.clone(), vec![]).with_live_config(crate::config::Config::default());
    let (url, content) = serve_mutable_p2p().await;
    *content.lock() = "1.2.3.4\n".into();
    state.refresh_ip_filter(&url).await.unwrap();
    assert_eq!(state.bt_bans.lock().len(), 1);

    // 404 源
    let app = axum::Router::new().route(
        "/gone",
        axum::routing::get(|| async { axum::http::StatusCode::NOT_FOUND }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bad_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let err = state
        .refresh_ip_filter(&format!("http://{bad_addr}/gone"))
        .await
        .unwrap_err();
    assert!(err.contains("404"), "错误须含状态码: {err}");
    assert_eq!(state.bt_bans.lock().len(), 1, "失败保持：条目不动");
    {
        let st = state.ip_filter.lock();
        assert_eq!(st.entries.len(), 1);
        assert!(st.last_error.is_some());
        assert_eq!(st.source_url, url, "失败不更新成功源记账");
    }
}

/// BT 引擎不可用 → Err（订阅是 BT-only 功能；不静默入表制造「看似生效」）。
#[tokio::test]
async fn ip_filter_refresh_requires_bt_engine() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake, vec![]).with_live_config(crate::config::Config::default());
    let (url, content) = serve_mutable_p2p().await;
    *content.lock() = "1.2.3.4\n".into();
    let err = state.refresh_ip_filter(&url).await.unwrap_err();
    assert!(err.contains("BT 引擎不可用"), "须明示引擎门槛: {err}");
    assert!(state.bt_bans.lock().is_empty(), "不得静默入表");
    assert!(state.ip_filter.lock().last_error.is_some());
}

/// 安全护栏：源返回 200 但内容全为无法解析的行（HTML 错误页）→ 拒绝应用，
/// 保持上次生效集（防误指 URL 时「空集清空全部订阅条目」）；真正的空列表
/// （零条目零错误）才合法清空。
#[tokio::test]
async fn ip_filter_garbage_source_does_not_clear_entries() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let state =
        DaemonState::new(fake.clone(), vec![]).with_live_config(crate::config::Config::default());
    let (url, content) = serve_mutable_p2p().await;
    *content.lock() = "1.2.3.4\n".into();
    state.refresh_ip_filter(&url).await.unwrap();
    assert_eq!(state.bt_bans.lock().len(), 1);

    // 换成 HTML 页（200 + 全垃圾行）
    *content.lock() = "<html><body>404 not found</body></html>\n<meta foo>\n".into();
    let err = state.refresh_ip_filter(&url).await.unwrap_err();
    assert!(err.contains("无法解析"), "护栏须拒绝垃圾源: {err}");
    assert_eq!(state.bt_bans.lock().len(), 1, "条目不动");

    // 真正的空列表（零条目零错误）= 合法清空信号
    *content.lock() = "# cleared\n".into();
    let n = state.refresh_ip_filter(&url).await.unwrap();
    assert_eq!(n, 0);
    assert!(state.bt_bans.lock().is_empty(), "空列表合法清空订阅条目");
}
