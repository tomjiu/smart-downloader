//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! E8 任务级代理热改：记录回显 + 引擎调用 + 入参校验 + 非 HTTP 预拒
//! （FakeEngine 记录调用；BT 任务白盒插记录，precheck 在 engine_for 之前，
//! 非 bt 构建同样可测）。
#![cfg(test)]

use super::*;
use smart_dl_core::identity::{CanonicalId, CanonicalKind, ContentIdentity};

/// 手工插一条 BT kind 记录（不注册 BT 引擎——set_task_proxy 的 kind
/// 预拒发生在 engine_for 之前，未注册引擎不影响断言路径）。
fn insert_bt_rec(state: &DaemonState, id: &str, ih: &str) {
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
            state: TaskState::Downloading(EngineKind::Bt),
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

fn source_proxy(state: &DaemonState, id: &str) -> Option<String> {
    match &state.tasks.lock().get(id).unwrap().task.source {
        DownloadSource::Http { proxy, .. } => proxy.clone(),
        other => panic!("source 应为 Http: {other:?}"),
    }
}

#[tokio::test]
async fn set_task_proxy_updates_record_and_engine() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task("https://example.com/f.bin".into(), None)
        .await
        .unwrap();

    state
        .set_task_proxy(&tid, Some("http://127.0.0.1:8080".into()))
        .await
        .unwrap();
    assert_eq!(
        source_proxy(&state, &tid),
        Some("http://127.0.0.1:8080".into()),
        "proxy 必须回显到任务 source（持久化口径）"
    );
    let engine_tid = state.tasks.lock().get(&tid).unwrap().engine_tid.clone();
    assert_eq!(
        fake.proxy_set_calls(),
        vec![(
            engine_tid.clone().unwrap(),
            Some("http://127.0.0.1:8080".into())
        )],
        "引擎必须收到热改调用（按 engine_tid 记录）"
    );
    // 清除回共享 client
    state.set_task_proxy(&tid, None).await.unwrap();
    assert_eq!(
        source_proxy(&state, &tid),
        None,
        "清除后 source.proxy = None"
    );
    assert_eq!(
        fake.proxy_set_calls()[1],
        (engine_tid.unwrap(), None),
        "清除语义必须透传引擎（None 而非空串）"
    );
}

#[tokio::test]
async fn set_task_proxy_rejects_invalid_without_side_effect() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task("https://example.com/f.bin".into(), None)
        .await
        .unwrap();

    for bad in [
        "",                       // 空串：非法 URL 不是清除（清除传 None）
        "http://127.0.0.1:70000", // 端口越界
    ] {
        let r = state.set_task_proxy(&tid, Some(bad.to_string())).await;
        match r {
            Err(DaemonError::InvalidSource(m)) => {
                assert!(!m.is_empty(), "{bad:?} 必须带错误说明");
            }
            other => panic!("非法 proxy {bad:?} 必须 InvalidSource: {other:?}"),
        }
    }
    assert!(
        fake.proxy_set_calls().is_empty(),
        "非法 proxy 不得触达引擎（零副作用）"
    );
    assert_eq!(source_proxy(&state, &tid), None, "记录保持原状");
    // 不存在的任务 → NotFound
    assert!(matches!(
        state
            .set_task_proxy("t404", Some("http://127.0.0.1:1".into()))
            .await,
        Err(DaemonError::NotFound(_))
    ));
}

#[tokio::test]
async fn set_task_proxy_rejects_non_http_task() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    insert_bt_rec(&state, "t-bt", "ABC123");

    let r = state
        .set_task_proxy("t-bt", Some("http://127.0.0.1:8080".into()))
        .await;
    match r {
        Err(DaemonError::UnsupportedOp(m)) => {
            assert!(m.contains("HTTP"), "409 错误信息应定性仅 HTTP 支持: {m}");
        }
        other => panic!("BT 任务 set proxy 必须 UnsupportedOp(409): {other:?}"),
    }
    assert!(
        fake.proxy_set_calls().is_empty(),
        "HTTP 引擎不得收到 BT 任务的调用"
    );
}
