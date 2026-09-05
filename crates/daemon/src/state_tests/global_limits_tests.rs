//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! E16 全局限速总阀门：引擎下发形态 / 纯查询 / 无变化 no-op / 失败回滚。
#![cfg(test)]

use super::*;

#[tokio::test]
async fn dispatches_down_to_http_and_both_to_bt() {
    let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let bt_fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let mut state = DaemonState::new(http_fake.clone() as Arc<dyn DownloadEngine>, vec![]);
    // 直接注入 Bt 槽位（with_bt 是 feature 门控；测试模块内私有字段可及）
    state
        .engines
        .insert(EngineKind::Bt, bt_fake.clone() as Arc<dyn DownloadEngine>);

    let g = state
        .apply_global_limits(Some(2048), Some(512))
        .await
        .unwrap();
    assert_eq!(g.max_download_kb_s, 2048);
    assert_eq!(g.max_upload_kb_s, 512);
    // HTTP 位：仅 down 方向（up 请求不该下发，HTTP/FTP 无上传概念）
    assert_eq!(
        http_fake.global_sets(),
        vec![(Some(2048), None)],
        "HTTP 位应只收 down 方向"
    );
    // BT 位：全量两方向（settings_pack 全量语义）
    assert_eq!(bt_fake.global_sets(), vec![(Some(2048), Some(512))]);

    // 内存值同步
    let cur = state.global_limits();
    assert_eq!(cur.max_download_kb_s, 2048);
    assert_eq!(cur.max_upload_kb_s, 512);

    // 事件广播
    let envs = state.hub().read_after(0, 100);
    assert!(
        envs.iter()
            .any(|e| e.event.type_label() == "global_limits_changed"),
        "应广播 global_limits_changed 事件"
    );
}

#[tokio::test]
async fn both_none_is_pure_query() {
    let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(http_fake.clone() as Arc<dyn DownloadEngine>, vec![])
        .with_global_limits(1024, 256);

    let g = state.apply_global_limits(None, None).await.unwrap();
    assert_eq!(g.max_download_kb_s, 1024, "纯查询返回注入值");
    assert_eq!(g.max_upload_kb_s, 256);
    assert!(http_fake.global_sets().is_empty(), "纯查询不得产生引擎调用");
    assert!(state.hub().read_after(0, 100).is_empty(), "纯查询无事件");
}

#[tokio::test]
async fn unchanged_values_are_noop() {
    let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(http_fake.clone() as Arc<dyn DownloadEngine>, vec![])
        .with_global_limits(1024, 0);

    state.apply_global_limits(Some(1024), None).await.unwrap();
    assert!(
        http_fake.global_sets().is_empty(),
        "合并后与当前一致 → no-op（引擎侧已是该值）"
    );
    assert!(state.hub().read_after(0, 100).is_empty(), "无变化不发事件");
}

#[tokio::test]
async fn partial_merge_keeps_unset_direction() {
    let bt_fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let mut state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![])
        .with_global_limits(1024, 512);
    state
        .engines
        .insert(EngineKind::Bt, bt_fake.clone() as Arc<dyn DownloadEngine>);

    let g = state.apply_global_limits(None, Some(256)).await.unwrap();
    assert_eq!(g.max_download_kb_s, 1024, "None 方向沿用当前值");
    assert_eq!(g.max_upload_kb_s, 256);
    assert_eq!(bt_fake.global_sets(), vec![(Some(1024), Some(256))]);
}

#[tokio::test]
async fn engine_failure_keeps_valve_unchanged() {
    let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let bt_fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    bt_fake.fail_global_limits(EngineError::Other("settings_pack boom".into()));
    let mut state = DaemonState::new(http_fake.clone() as Arc<dyn DownloadEngine>, vec![])
        .with_global_limits(1024, 128);
    state
        .engines
        .insert(EngineKind::Bt, bt_fake.clone() as Arc<dyn DownloadEngine>);

    let err = state
        .apply_global_limits(Some(4096), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("全局限速下发失败"),
        "错误应定性为阀门下发失败: {err}"
    );
    // BT 先行失败 → HTTP 未下发、内存值不变（近全有或全无）
    assert!(
        http_fake.global_sets().is_empty(),
        "BT 失败时 HTTP 不得已改动"
    );
    let cur = state.global_limits();
    assert_eq!(cur.max_download_kb_s, 1024, "阀门保持旧值");
    assert!(state.hub().read_after(0, 100).is_empty(), "失败无事件");
}

#[tokio::test]
async fn unsupported_engine_is_skipped_silently() {
    // 引擎无该设施（返回 Unsupported，如未来 NAS 远程引擎同位替换）→
    // 静默跳过，不阻塞阀门生效（内存值照常更新 + 事件照发）
    let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
    http_fake.fail_global_limits(EngineError::Unsupported);
    let state = DaemonState::new(http_fake.clone() as Arc<dyn DownloadEngine>, vec![]);

    let g = state.apply_global_limits(Some(2048), None).await.unwrap();
    assert_eq!(g.max_download_kb_s, 2048, "阀门照常生效（内存值）");
    assert!(!state.hub().read_after(0, 100).is_empty(), "事件照发");
}

#[tokio::test]
async fn snapshot_overlay_reflects_effective_values() {
    let state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![]).with_config(
        serde_json::json!({
            "dest_root": "./downloads",
            "max_download_kb_s": 0,
            "max_upload_kb_s": 0,
        }),
    );
    state
        .apply_global_limits(Some(2048), Some(512))
        .await
        .unwrap();
    let snap = state.config_snapshot();
    assert_eq!(snap["max_download_kb_s"], 2048, "/config 快照覆盖");
    assert_eq!(snap["max_upload_kb_s"], 512);
}
