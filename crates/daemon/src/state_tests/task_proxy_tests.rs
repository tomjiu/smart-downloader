//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! E5 任务级代理：add 入口校验 + source.proxy 持久化回显（FakeEngine 不联网）。
#![cfg(test)]

use super::*;

#[tokio::test]
async fn add_rejects_invalid_proxy_url() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    for bad in ["", "http://127.0.0.1:70000"] {
        let r = state
            .add_http_task_opts(
                "https://example.com/f.bin".into(),
                None,
                AddHttpOpts {
                    proxy: Some(bad.to_string()),
                    ..Default::default()
                },
            )
            .await;
        match r {
            Err(DaemonError::InvalidSource(m)) => {
                assert!(m.contains("proxy"), "错误信息应定性 proxy: {m}");
            }
            other => panic!("非法 proxy {bad:?} 必须在入队前拒绝: {other:?}"),
        }
    }
    assert!(fake.added().is_empty(), "被拒任务不得进入引擎");
}

#[tokio::test]
async fn add_persists_wellformed_proxy_in_source() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task_opts(
            "https://example.com/f.bin".into(),
            None,
            AddHttpOpts {
                proxy: Some("http://127.0.0.1:8080".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    match &rec.task.source {
        DownloadSource::Http { proxy, .. } => {
            assert_eq!(
                proxy.as_deref(),
                Some("http://127.0.0.1:8080"),
                "proxy 应在 source 持久化回显"
            );
        }
        other => panic!("source 应为 Http: {other:?}"),
    }
    // 默认路径：proxy=None 正常建任务
    let tid2 = state
        .add_http_task("https://example.com/g.bin".into(), None)
        .await
        .unwrap();
    let rec2 = state.tasks.lock().get(&tid2).cloned().unwrap();
    match &rec2.task.source {
        DownloadSource::Http { proxy, .. } => assert!(proxy.is_none()),
        other => panic!("source 应为 Http: {other:?}"),
    }
}
