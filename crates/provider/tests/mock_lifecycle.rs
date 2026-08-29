//! M5: MockProvider 全生命周期 —— submit→Queued→Downloading→Ready→resolve(2 文件)
//! → HttpEngine 传输 → Completed。

mod common;

use common::{patterned, TestServer};
use smart_dl_core::state_machine::TaskState;
use smart_dl_core::types::DownloadSource;
use smart_dl_core::types::{Capability, DownloadEngine};
use smart_dl_httpdl::HttpEngine;
use smart_dl_provider::{
    FallbackCoordinator, HttpSink, MockProvider, ProviderError, ProviderRuntime, ProviderStatus,
    ProviderTaskId, RemoteProvider, ResolvedRemoteFile, SinkError,
};
use std::path::PathBuf;
use std::sync::Arc;

fn mock_files(size: u64, url: &str) -> Vec<ResolvedRemoteFile> {
    vec![
        ResolvedRemoteFile {
            rel_path: "a.bin".to_string(),
            url: url.to_string(),
            size,
            etag: Some("etag-p1".to_string()),
            expires_at: None,
        },
        ResolvedRemoteFile {
            rel_path: "sub/b.bin".to_string(),
            url: url.to_string(),
            size,
            etag: None,
            expires_at: None,
        },
    ]
}

#[tokio::test]
async fn submit_then_status_advances_to_ready() {
    let p = MockProvider::new("mock1");
    let tid: ProviderTaskId = p
        .submit(&DownloadSource::Magnet("magnet:?xt=urn:btih:abc".into()))
        .await
        .unwrap();
    // 自动推进：Queued → Downloading → Ready
    let s1 = p.status(&tid).await.unwrap();
    assert_eq!(s1, ProviderStatus::Queued);
    let s2 = p.status(&tid).await.unwrap();
    assert_eq!(s2, ProviderStatus::Downloading);
    let s3 = p.status(&tid).await.unwrap();
    assert_eq!(s3, ProviderStatus::Ready);
}

#[tokio::test]
async fn resolve_returns_two_files() {
    let url = "http://direct.local/f";
    let p = MockProvider::new("mock2").with_files(mock_files(1024, url));
    let tid = p
        .submit(&DownloadSource::Magnet("magnet:?xt=urn:btih:def".into()))
        .await
        .unwrap();
    // 推进到 Ready
    for _ in 0..3 {
        let _ = p.status(&tid).await.unwrap();
    }
    let files = p.resolve(&tid).await.unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].rel_path, "a.bin");
    assert_eq!(files[0].size, 1024);
    assert_eq!(files[0].etag.as_deref(), Some("etag-p1"));
    assert_eq!(files[1].rel_path, "sub/b.bin");
}

/// HttpEngine 适配为 HttpSink（M5 直链传输）。
struct EngineSink {
    engine: HttpEngine,
}

#[async_trait::async_trait]
impl HttpSink for EngineSink {
    async fn transfer(
        &self,
        task_id: &str,
        url: &str,
        dest_root: PathBuf,
        name: Option<String>,
    ) -> Result<(), SinkError> {
        // 先建目标父目录（rel_path 可能含子目录，如 sub/b.bin）
        if let Some(rel) = &name {
            if let Some(parent) = dest_root.join(rel).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let task = smart_dl_core::task::DownloadTask {
            id: task_id.to_string(),
            canonical_id: smart_dl_core::identity::CanonicalId {
                kind: smart_dl_core::identity::CanonicalKind::Http,
                identity: url.to_string(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Http {
                url: url.to_string(),
                headers: vec![],
                auth: None,
                backup_url: None,
            },
            identity: smart_dl_core::identity::ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                backup_md5: None,
            },
            dest_root,
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            metadata: smart_dl_core::task::TaskMetadata {
                name,
                added_at_unix: 0,
            },
        };
        let tid = self
            .engine
            .add(&task)
            .await
            .map_err(|e| SinkError::Failed(e.to_string()))?;
        // 轮询到完成
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let st = self
                .engine
                .status(&tid)
                .await
                .map_err(|e| SinkError::Failed(e.to_string()))?;
            match st.state {
                smart_dl_core::types::EngineState::Completed => return Ok(()),
                smart_dl_core::types::EngineState::Error => {
                    return Err(SinkError::Failed(
                        st.error.unwrap_or_else(|| "engine error".into()),
                    ))
                }
                _ => {
                    assert!(std::time::Instant::now() < deadline, "transfer 30s 未完成");
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }
    }

    async fn update_sources(&self, _task_id: &str, _urls: Vec<String>) -> Result<(), SinkError> {
        Ok(())
    }
}

#[tokio::test]
async fn full_flow_transfers_two_files_to_disk() {
    let size = 64 * 1024;
    let body = patterned(size);
    let srv = TestServer::start(body.clone(), Some("etag-d".to_string())).await;
    let files = mock_files(size, &srv.url());
    let p = MockProvider::new("mock3").with_files(files);

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let sink = EngineSink { engine };
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], Default::default());

    let task = smart_dl_core::task::DownloadTask {
        id: "life1".to_string(),
        canonical_id: smart_dl_core::identity::CanonicalId {
            kind: smart_dl_core::identity::CanonicalKind::Bt,
            identity: "magnet:?xt=urn:btih:xyz".to_string(),
            validator: None,
            token_sensitive: false,
        },
        source: DownloadSource::Magnet("magnet:?xt=urn:btih:xyz".to_string()),
        identity: smart_dl_core::identity::ContentIdentity::SingleFile {
            size: 0,
            etag: None,
            sha256: None,
            backup_md5: None,
        },
        dest_root: dir.path().to_path_buf(),
        files: vec![],
        acquisitions: vec![],
        aggregate: Default::default(),
        state: TaskState::Queued,
        retry: Default::default(),
        created_at: std::time::Instant::now(),
        metadata: smart_dl_core::task::TaskMetadata {
            name: None,
            added_at_unix: 0,
        },
    };

    let outcome = coord.begin_fallback(&task, 0.1, true, &sink).await.unwrap();
    assert_eq!(outcome.provider, "mock3");
    assert_eq!(outcome.transferred.len(), 2);

    let got_a = std::fs::read(dir.path().join("a.bin")).unwrap();
    assert_eq!(got_a, body, "直链传输内容必须一致");
    let got_b = std::fs::read(dir.path().join("sub/b.bin")).unwrap();
    assert_eq!(got_b, body);
}

#[tokio::test]
async fn remove_then_status_not_found() {
    let p = MockProvider::new("mock4");
    let tid = p.submit(&DownloadSource::Magnet("m".into())).await.unwrap();
    p.remove(&tid).await.unwrap();
    assert!(matches!(p.status(&tid).await, Err(ProviderError::NotFound)));
}

#[tokio::test]
async fn runtime_reflects_config() {
    let p = MockProvider::new("mock5")
        .with_quota(10)
        .with_concurrency(2);
    let rt: ProviderRuntime = p.runtime();
    assert!(rt.enabled);
    assert!(rt.authenticated);
    assert_eq!(rt.quota_remaining, 10);
    assert_eq!(rt.concurrency_limit, 2);
    assert_eq!(p.capabilities(), vec![Capability::OfflineCache]);
    assert_eq!(p.name(), "mock5");
}
