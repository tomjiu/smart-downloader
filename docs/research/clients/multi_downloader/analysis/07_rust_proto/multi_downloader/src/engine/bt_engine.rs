//! BT engine trait + placeholder implementation.
//!
//! Borrowed philosophy from qBittorrent (analysis §1) — do not reinvent BT;
//! wrap an existing engine. The trait here is shaped so that either
//! [`librqbit`](https://github.com/ihavechat/librqbit) or a future
//! `libtorrent-rs` port can plug in.
//!
//! The placeholder impl returns `Unimplemented` for every method, so the
//! prototype still compiles cleanly with the BT feature gated off.

use async_trait::async_trait;
use tracing::info;
use url::Url;

use super::protocol::{ProtocolEngine, ProtocolKind};
use crate::core::task::DownloadTask;
use crate::error::{DownloadError, ErrorCategory, Result};

/// Engine-level BT trait (separate from the `ProtocolEngine` routing trait so
/// that the BT subsystem can carry BT-specific methods like
/// `peer_count` / `swarm_stats` / etc.).
#[async_trait]
pub trait BtEngine: Send + Sync {
    /// Add a torrent from a magnet URI or .torrent URL.
    async fn add(&self, task: &DownloadTask) -> Result<()>;

    /// Cancel an active BT task.
    async fn cancel(&self, task_id: u64) -> Result<()>;

    /// Pause (suspend piece downloads) but keep peer connections open.
    async fn pause(&self, task_id: u64) -> Result<()>;

    /// Resume from pause.
    async fn resume(&self, task_id: u64) -> Result<()>;

    /// Current peer count for the swarm (or 0 if not started).
    async fn peer_count(&self, task_id: u64) -> u64;

    /// Current piece-progress in `[0.0, 1.0]`.
    async fn progress(&self, task_id: u64) -> f64;
}

/// Placeholder BT engine. **All methods return `Unimplemented`.**
///
/// The trait is wired into the routing table so a `magnet:?xt=urn:btih:...`
/// URL can be detected and rejected cleanly with a helpful error, instead of
/// being routed to the HTTP engine and producing a confusing 404.
pub struct BtEngineImpl {
    bt_enabled: bool,
}

impl BtEngineImpl {
    /// Construct a placeholder. Pass `bt_enabled = true` once a real BT
    /// backend is wired in.
    #[must_use]
    pub fn new(bt_enabled: bool) -> Self {
        Self { bt_enabled }
    }

    fn unimpl(&self) -> DownloadError {
        DownloadError::new(
            0,
            ErrorCategory::Unimplemented,
            "BT engine is a placeholder in this prototype; integrate librqbit / libtorrent-rs to enable",
        )
    }
}

#[async_trait]
impl BtEngine for BtEngineImpl {
    async fn add(&self, _task: &DownloadTask) -> Result<()> {
        info!(enabled = self.bt_enabled, "BtEngineImpl::add called (placeholder)");
        Err(self.unimpl())
    }
    async fn cancel(&self, _task_id: u64) -> Result<()> {
        Err(self.unimpl())
    }
    async fn pause(&self, _task_id: u64) -> Result<()> {
        Err(self.unimpl())
    }
    async fn resume(&self, _task_id: u64) -> Result<()> {
        Err(self.unimpl())
    }
    async fn peer_count(&self, _task_id: u64) -> u64 {
        0
    }
    async fn progress(&self, _task_id: u64) -> f64 {
        0.0
    }
}

#[async_trait]
impl ProtocolEngine for BtEngineImpl {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Magnet
    }
    fn accepts(&self, url: &Url) -> bool {
        matches!(ProtocolKind::from_url(url), ProtocolKind::Magnet | ProtocolKind::Torrent)
    }
    async fn run_task(&self, task: &DownloadTask) -> Result<()> {
        info!(task = task.task_id, "bt placeholder invoked");
        Err(self.unimpl())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::task::TaskKind;

    #[tokio::test]
    async fn placeholder_returns_unimplemented() {
        let e = BtEngineImpl::new(false);
        let t = DownloadTask::new(TaskKind::Magnet, Url::parse("magnet:?xt=urn:btih:abc").unwrap());
        let r = e.add(&t).await;
        assert!(r.is_err());
        let err = r.unwrap_err();
        assert_eq!(err.category, ErrorCategory::Unimplemented);
    }

    #[test]
    fn accepts_magnet_and_torrent() {
        let e = BtEngineImpl::new(false);
        let m = Url::parse("magnet:?xt=urn:btih:abc").unwrap();
        let t = Url::parse("https://x/y.torrent").unwrap();
        let h = Url::parse("https://x/y.html").unwrap();
        assert!(e.accepts(&m));
        assert!(e.accepts(&t));
        assert!(!e.accepts(&h));
    }
}
