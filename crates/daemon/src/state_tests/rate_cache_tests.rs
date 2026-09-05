//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! E11 速率缓存：轮询器把引擎快照写入 `engine_status` → `/stats` 聚合生效；
//! BT 仅缓存不迁移（状态权威 = alert 流）；暂停/终态清零防陈旧速率。
#![cfg(test)]

use super::*;

/// 插入指定状态的 BT 任务记录（不联网，engine_tid = infohash）。
fn insert_bt_rec_with(state: &DaemonState, id: &str, ih: &str, st: TaskState) {
    let rec = TaskRecord {
        task: DownloadTask {
            id: id.into(),
            canonical_id: CanonicalId {
                kind: CanonicalKind::Bt,
                identity: ih.to_string(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Magnet(format!("magnet:?xt=urn:btih:{ih}")),
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
            state: st,
            retry: Default::default(),
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
        engine_tid: Some(ih.to_string()),
        engine_kind: EngineKind::Bt,
        engine_status: None,
        events: vec![],
    };
    state.tasks.lock().insert(id.into(), rec);
}

#[tokio::test]
async fn poll_caches_rates_into_stats_and_pause_zeroes() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task("https://example.com/f.bin".into(), None)
        .await
        .unwrap();
    assert_eq!(state.stats().down_bytes_s, 0, "轮询前缓存为空 → 聚合速率 0");

    fake.set_status_rates(1000, 50);
    state.poll_engine_states().await;
    let st = state.stats();
    assert_eq!(st.down_bytes_s, 1000, "HTTP 引擎报的下行速率应入 /stats");
    assert_eq!(st.up_bytes_s, 50);
    {
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        let es = rec.engine_status.expect("缓存应有引擎快照");
        assert_eq!(es.down_rate, 1000);
        assert_eq!(es.up_rate, 50);
    }

    // 暂停：清零缓存速率（轮询器不再光顾暂停任务，不清则聚合虚高）
    state.pause(&tid).await.unwrap();
    let st = state.stats();
    assert_eq!(st.down_bytes_s, 0, "暂停后聚合下行速率必须清零");
    assert_eq!(st.up_bytes_s, 0, "暂停后聚合上行速率必须清零");
    assert_eq!(
        state.task_logs(&tid).unwrap()["state"],
        serde_json::json!("Paused")
    );
}

#[tokio::test]
async fn poll_refreshes_rates_each_round() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    state
        .add_http_task("https://example.com/f.bin".into(), None)
        .await
        .unwrap();

    fake.set_status_rates(500, 0);
    state.poll_engine_states().await;
    assert_eq!(state.stats().down_bytes_s, 500);

    fake.set_status_rates(0, 0);
    state.poll_engine_states().await;
    assert_eq!(state.stats().down_bytes_s, 0, "缓存跟随引擎每轮刷新");
}

#[tokio::test]
async fn bt_rates_cached_without_transition() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let state = DaemonState::new(fake.clone(), vec![]);
    insert_bt_rec_with(
        &state,
        "t-bt",
        "ABC123",
        TaskState::Downloading(EngineKind::Bt),
    );
    fake.set_status_rates(2000, 1000);

    let effects = state.poll_engine_states().await;
    assert!(effects.is_empty(), "BT 轮询仅缓存，不产生迁移效果");
    {
        let rec = state.tasks.lock().get("t-bt").cloned().unwrap();
        assert_eq!(
            rec.task.state,
            TaskState::Downloading(EngineKind::Bt),
            "BT 状态权威 = alert 流，轮询不得迁移"
        );
        let es = rec.engine_status.expect("BT 快照应入缓存");
        assert_eq!(es.down_rate, 2000);
        assert_eq!(es.up_rate, 1000);
    }
    let st = state.stats();
    assert_eq!(st.down_bytes_s, 2000);
    assert_eq!(st.up_bytes_s, 1000);
}

#[tokio::test]
async fn seeding_bt_task_rates_cached() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let state = DaemonState::new(fake.clone(), vec![]);
    insert_bt_rec_with(&state, "t-seed", "DEF456", TaskState::Seeding);
    fake.set_status_rates(0, 800);

    let effects = state.poll_engine_states().await;
    assert!(effects.is_empty());
    let st = state.stats();
    assert_eq!(st.up_bytes_s, 800, "做种中上行速率应入聚合");
    assert_eq!(st.down_bytes_s, 0);
}

#[tokio::test]
async fn paused_bt_task_not_polled() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let state = DaemonState::new(fake.clone(), vec![]);
    insert_bt_rec_with(&state, "t-paused", "FFF111", TaskState::Paused);
    fake.set_status_rates(999, 999);

    state.poll_engine_states().await;
    let rec = state.tasks.lock().get("t-paused").cloned().unwrap();
    assert!(
        rec.engine_status.is_none(),
        "暂停任务不在候选集——缓存不得被写入陈旧速率"
    );
    assert_eq!(state.stats().down_bytes_s, 0);
}

/// E13：快照速率取自实时引擎快照——轮询器未跑（缓存恒 None）也能透出，
/// 锁定「实时源而非缓存源」的设计语义。
#[tokio::test]
async fn snapshot_exposes_live_engine_rates_without_cache() {
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
            .engine_status
            .is_none(),
        "前提：无轮询器 → 缓存必须为空（证明速率来自实时快照）"
    );
    fake.set_status_rates(1234, 56);
    let snap = state.task_snapshot(&tid).await.unwrap();
    assert_eq!(
        snap.rates,
        Some(TaskRates {
            down_bytes_s: 1234,
            up_bytes_s: 56
        }),
        "快照速率应来自引擎实时 status()，与缓存无关"
    );
    // 引擎报零 → 速率字段仍在（Some），形状恒定不省略
    fake.set_status_rates(0, 0);
    let snap = state.task_snapshot(&tid).await.unwrap();
    assert_eq!(
        snap.rates,
        Some(TaskRates {
            down_bytes_s: 0,
            up_bytes_s: 0
        })
    );
}

/// E13：记录级 Paused 是显示权威（qB 式），快照速率对齐 pause() 清零
/// 语义——引擎侧 <200ms 平滑窗口的陈旧非零值不得穿透到暂停任务快照。
#[tokio::test]
async fn snapshot_zeroes_rates_for_paused_record() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task("https://example.com/f.bin".into(), None)
        .await
        .unwrap();
    fake.set_status_rates(9999, 888);
    state.pause(&tid).await.unwrap();
    let snap = state.task_snapshot(&tid).await.unwrap();
    assert_eq!(snap.state, "Paused");
    // FakeEngine.status() 仍报 (9999, 888) + Downloading——清零只能来自
    // 记录级 Paused 守卫（引擎实时值与显示权威的裁决点）。
    assert_eq!(
        snap.rates,
        Some(TaskRates {
            down_bytes_s: 0,
            up_bytes_s: 0
        }),
        "暂停任务快照速率必须清零（防平滑窗口陈旧值毛刺）"
    );
}

/// E33：BT 累计统计与分享率透出——快照字段、JSON 形状（非零才出现）、
/// share_ratio 精度三合一。FakeEngine 模拟引擎侧 all_time_* 回显。
#[tokio::test]
async fn snapshot_exposes_bt_totals_and_share_ratio() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let state = DaemonState::new(fake.clone(), vec![]);
    insert_bt_rec_with(
        &state,
        "t-bt",
        "TOTL01",
        TaskState::Downloading(EngineKind::Bt),
    );
    // (累计下行 2MB, 累计上行 512KB) → 分享率 0.25
    fake.set_status_totals(2 * 1024 * 1024, 512 * 1024);
    let snap = state.task_snapshot("t-bt").await.unwrap();
    assert_eq!(snap.total_downloaded, 2 * 1024 * 1024);
    assert_eq!(snap.total_uploaded, 512 * 1024);
    assert_eq!(snap.share_ratio, Some(0.25));
    let json = serde_json::to_string(&snap).unwrap();
    assert!(json.contains("\"total_downloaded\":2097152"), "{json}");
    assert!(json.contains("\"total_uploaded\":524288"), "{json}");
    assert!(json.contains("\"share_ratio\":0.25"), "{json}");
}

/// E33：零值省略——无累计数据（HTTP 引擎默认/引擎不可达）时三字段均不
/// 出现在 JSON 里（非破坏增量，旧消费者零感知）。
#[tokio::test]
async fn snapshot_omits_totals_when_zero() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task("https://example.com/f.bin".into(), None)
        .await
        .unwrap();
    let snap = state.task_snapshot(&tid).await.unwrap();
    assert_eq!(snap.total_downloaded, 0);
    assert_eq!(snap.total_uploaded, 0);
    assert_eq!(snap.share_ratio, None);
    let json = serde_json::to_string(&snap).unwrap();
    assert!(!json.contains("total_downloaded"), "{json}");
    assert!(!json.contains("total_uploaded"), "{json}");
    assert!(!json.contains("share_ratio"), "{json}");
}

/// E33：累计语义与速率相反——记录级 Paused 只清瞬时速率（E13），累计
/// 统计是全生命周期事实（做种贡献），暂停不清零。
#[tokio::test]
async fn snapshot_keeps_totals_for_paused_record() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let state = DaemonState::new(fake.clone(), vec![]);
    insert_bt_rec_with(
        &state,
        "t-bt",
        "KEPT01",
        TaskState::Downloading(EngineKind::Bt),
    );
    fake.set_status_totals(1_000_000, 2_000_000);
    state.pause("t-bt").await.unwrap();
    let snap = state.task_snapshot("t-bt").await.unwrap();
    assert_eq!(snap.state, "Paused");
    // 速率清零（E13 语义不变）……
    assert_eq!(
        snap.rates,
        Some(TaskRates {
            down_bytes_s: 0,
            up_bytes_s: 0
        })
    );
    // ……累计与分享率保留
    assert_eq!(snap.total_downloaded, 1_000_000);
    assert_eq!(snap.total_uploaded, 2_000_000);
    assert_eq!(snap.share_ratio, Some(2.0));
}

/// E33：share_ratio 纯函数——零除保护 + 3 位小数舍入（qB 同级精度）。
#[test]
fn share_ratio_rules() {
    assert_eq!(share_ratio(0, 0), None, "无下行 → None");
    assert_eq!(share_ratio(500, 0), None, "纯上传侧比率无意义 → None");
    assert_eq!(share_ratio(500_000, 2_000_000), Some(0.25));
    assert_eq!(share_ratio(2_000_000, 500_000), Some(4.0));
    assert_eq!(share_ratio(1, 3), Some(0.333), "1/3 舍入到 3 位小数");
    assert_eq!(share_ratio(2, 3), Some(0.667), "2/3 进位到 3 位小数");
}
