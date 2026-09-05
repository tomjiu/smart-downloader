//! A4：`task_speed_samples`（/metrics histogram 数据源）白盒单测。
//! 口径：仅含任一方向速率 > 0 的任务；engine 标签复用 kind_label；
//! 全零任务 / 无引擎缓存任务不进样本。

#![cfg(test)]

use super::*;

fn http_rec_with_rates(id: &str, down: u64, up: u64) -> TaskRecord {
    TaskRecord {
        task: DownloadTask {
            id: id.into(),
            canonical_id: CanonicalId {
                kind: CanonicalKind::Http,
                identity: format!("https://x/{id}.bin"),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Http {
                url: format!("https://x/{id}.bin"),
                headers: vec![],
                auth: None,
                backup_url: None,
                proxy: None,
            },
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
            state: TaskState::Downloading(EngineKind::Http),
            retry: RetryState::default(),
            created_at: std::time::Instant::now(),
            metadata: TaskMetadata {
                name: None,
                added_at_unix: 0,
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: 0,
                next_retry_at_unix: 0,
            },
            limits: None,
            file_priorities: None,
            sequential: false,
        },
        engine_tid: Some(format!("eng-{id}")),
        engine_kind: EngineKind::Http,
        engine_status: Some(EngineStatus {
            down_rate: down,
            up_rate: up,
            ..Default::default()
        }),
        events: vec![],
    }
}

fn state_with(recs: Vec<TaskRecord>) -> DaemonState {
    let engine = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let state = DaemonState::new(Arc::new(engine), vec![]);
    {
        let mut tasks = state.tasks.lock();
        for r in recs {
            tasks.insert(r.task.id.clone(), r);
        }
    }
    state
}

#[test]
fn speed_samples_filters_zero_and_missing_cache() {
    let state = state_with(vec![
        http_rec_with_rates("a", 100, 0), // 活跃 down → 进样本
        http_rec_with_rates("b", 0, 50),  // 仅 up → 进样本（BT 上传口径）
        http_rec_with_rates("c", 0, 0),   // 全零 → 排除
        http_rec_with_rates("d", 1, 1),   // 双向 → 进样本
    ]);
    // d 无引擎缓存（置 None）→ 排除
    state.tasks.lock().get_mut("d").unwrap().engine_status = None;

    let mut got = state.task_speed_samples();
    got.sort_by_key(|(e, d, u)| (*e, *d, *u));
    assert_eq!(got.len(), 2, "全零/无缓存任务必须排除: {got:?}");
    assert!(got.contains(&("http", 100, 0)), "实际: {got:?}");
    assert!(got.contains(&("http", 0, 50)), "实际: {got:?}");
}

#[test]
fn speed_samples_empty_state_is_empty() {
    let state = state_with(vec![]);
    assert!(state.task_speed_samples().is_empty());
}
