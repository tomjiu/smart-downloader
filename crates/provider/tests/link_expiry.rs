//! M5: 直链过期（§13）——Transferring 中直链过期 → update_sources(≤3) →
//! resubmit(≤2) → 超限 Failed。

mod common;

use smart_dl_core::state_machine::TaskState;
use smart_dl_core::types::DownloadSource;
use smart_dl_provider::{
    FallbackCoordinator, HttpSink, MockProvider, ProviderError, ResolvedRemoteFile, SinkError,
};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn mock_task() -> smart_dl_core::task::DownloadTask {
    smart_dl_core::task::DownloadTask {
        id: "exp1".to_string(),
        canonical_id: smart_dl_core::identity::CanonicalId {
            kind: smart_dl_core::identity::CanonicalKind::Bt,
            identity: "magnet:?xt=urn:btih:exp".to_string(),
            validator: None,
            token_sensitive: false,
        },
        source: DownloadSource::Magnet("magnet:?xt=urn:btih:exp".to_string()),
        identity: smart_dl_core::identity::ContentIdentity::SingleFile {
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
        state: TaskState::Queued,
        retry: Default::default(),
        created_at: std::time::Instant::now(),
        file_priorities: None,
        sequential: false,
        metadata: smart_dl_core::task::TaskMetadata {
            name: None,
            added_at_unix: 0,
            tags: Vec::new(),
            finished_at_unix: 0,
            start_at_unix: 0,
        },
        limits: None,
    }
}

/// 记录调用次数的 sink：update_sources 可配置失败；transfer 可配置 Expired/Restart。
struct RecordingSink {
    updates: Arc<AtomicUsize>,
    transfers: Arc<AtomicUsize>,
    fail_updates: bool,
    expire_updates: bool,
    fail_transfer: bool,
}

impl RecordingSink {
    fn new() -> Self {
        RecordingSink {
            updates: Arc::new(AtomicUsize::new(0)),
            transfers: Arc::new(AtomicUsize::new(0)),
            fail_updates: false,
            expire_updates: false,
            fail_transfer: false,
        }
    }
    fn updates(&self) -> usize {
        self.updates.load(Ordering::SeqCst)
    }
    fn transfers(&self) -> usize {
        self.transfers.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl HttpSink for RecordingSink {
    async fn transfer(
        &self,
        _id: &str,
        _url: &str,
        _dest: PathBuf,
        _name: Option<String>,
    ) -> Result<(), SinkError> {
        self.transfers.fetch_add(1, Ordering::SeqCst);
        if self.fail_transfer {
            Err(SinkError::Expired)
        } else {
            Ok(())
        }
    }
    async fn update_sources(&self, _id: &str, _urls: Vec<String>) -> Result<(), SinkError> {
        self.updates.fetch_add(1, Ordering::SeqCst);
        if self.expire_updates {
            Err(SinkError::Expired)
        } else if self.fail_updates {
            Err(SinkError::Failed("update rejected".into()))
        } else {
            Ok(())
        }
    }
}

fn expired_file(rel: &str, url: &str) -> ResolvedRemoteFile {
    ResolvedRemoteFile {
        rel_path: rel.to_string(),
        url: url.to_string(),
        size: 1024,
        etag: Some("etag-x".to_string()),
        expires_at: Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - 60, // 已过期
        ),
    }
}

#[tokio::test]
async fn expired_link_not_transferred_directly() {
    // 直链已过期 → 不走 transfer，直接进 update_sources 流
    let p = MockProvider::new("exp").with_files(vec![expired_file("a.bin", "http://x/a")]);
    let sink = RecordingSink::new();
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], Default::default());
    let r = coord.begin_fallback(&mock_task(), 0.1, true, &sink).await;
    // 过期 → update_sources 尝试；本用例 update 返回 Ok 但 provider 无新文件 → resubmit → 无新文件 → 超限
    assert!(
        matches!(r, Err(ProviderError::RetriesExhausted)),
        "过期后无新源且无 resubmit 机会 → 超限"
    );
    assert_eq!(sink.transfers(), 0, "过期直链不得 transfer");
}

#[tokio::test]
async fn resubmit_yields_fresh_link_then_transfers() {
    // 首次 resolve 过期 → resubmit 成功（新任务 resolve 不过期）→ transfer
    let p = MockProvider::new("resh").with_files(vec![expired_file("a.bin", "http://x/a")]);
    // resubmit 轮次（第 2 次 submit 起）返回不过期文件
    p.set_resubmit_files(vec![ResolvedRemoteFile {
        rel_path: "a.bin".to_string(),
        url: "http://fresh/a".to_string(),
        size: 1024,
        etag: None,
        expires_at: None,
    }]);
    let sink = RecordingSink::new();
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], Default::default());
    let outcome = coord
        .begin_fallback(&mock_task(), 0.1, true, &sink)
        .await
        .unwrap();
    assert_eq!(outcome.transferred.len(), 1);
    assert_eq!(sink.transfers(), 1, "resubmit 拿到新直链后必须 transfer");
}

#[tokio::test]
async fn updates_then_fresh_link_transfers() {
    // update_sources 后（新 URL 有效）→ 继续 transfer（≤3 次 update 内）
    let p = MockProvider::new("upd").with_files(vec![expired_file("a.bin", "http://x/a")]);
    p.set_update_urls(vec!["http://renewed/a".to_string()]); // update_sources 携带新 URL → 再次 resolve 有效
    let sink = RecordingSink::new();
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], Default::default());
    let outcome = coord
        .begin_fallback(&mock_task(), 0.1, true, &sink)
        .await
        .unwrap();
    assert_eq!(outcome.transferred.len(), 1);
    assert!(sink.updates() >= 1, "必须尝试 update_sources");
    assert!(sink.updates() <= 3, "update_sources ≤3 次");
}

#[tokio::test]
async fn expired_transfer_triggers_update_flow() {
    // transfer 中 Expired（sink 报错）→ coordinator 走 update_sources 修复
    let mut sink = RecordingSink::new();
    sink.fail_transfer = true;
    // 第一次 transfer Expired → update_sources(ok) → 重试 transfer 仍 Expired → Failed
    let p = MockProvider::new("txf").with_files(vec![ResolvedRemoteFile {
        rel_path: "a.bin".to_string(),
        url: "http://x/a".to_string(),
        size: 1024,
        etag: None,
        expires_at: None,
    }]);
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], Default::default());
    let r = coord.begin_fallback(&mock_task(), 0.1, true, &sink).await;
    assert!(
        matches!(r, Err(ProviderError::RetriesExhausted)),
        "持续 Expired → 超限 Failed"
    );
    assert!(sink.updates() <= 3, "update_sources ≤3");
}

#[tokio::test]
async fn update_returning_expired_maps_to_provider_error() {
    // refresh 提供新 URL 但 sink.update_sources 报 Expired → ProviderError::Expired
    let mut sink = RecordingSink::new();
    sink.expire_updates = true;
    let p = MockProvider::new("upe").with_files(vec![expired_file("a.bin", "http://x/a")]);
    p.set_update_urls(vec!["http://new/a".to_string()]);
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], Default::default());
    let r = coord.begin_fallback(&mock_task(), 0.1, true, &sink).await;
    assert!(
        matches!(r, Err(ProviderError::Expired)),
        "update_sources Expired → ProviderError::Expired"
    );
}
