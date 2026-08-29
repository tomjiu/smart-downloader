//! Protocol abstraction trait — borrowed from FileCentipede's protocol matrix
//! (analysis §3).
//!
//! FileCentipede exposes a uniform `ext::uri` → `task` pipeline for every
//! protocol it supports (HTTP/HTTPS/FTP/FTPS/SSH/WebDAV/BT/Magnet/ed2k/HLS).
//! The trait here captures that contract: every protocol knows how to
//! (a) recognise a URL as belonging to it, (b) run a task to completion,
//! (c) cancel it, and (d) report its kind for routing.

use async_trait::async_trait;
use url::Url;

use crate::core::task::DownloadTask;
use crate::error::Result;

/// Identifies a protocol family for routing decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtocolKind {
    /// Plain HTTP.
    Http,
    /// HTTPS (rustls).
    Https,
    /// FTP / FTPS (placeholder).
    Ftp,
    /// BitTorrent magnet URI (`magnet:?xt=urn:btih:...`).
    Magnet,
    /// `.torrent` file URL (will be fetched then handed to BT engine).
    Torrent,
    /// HLS `.m3u8` (placeholder).
    Hls,
    /// Unknown / unsupported.
    Unknown,
}

impl ProtocolKind {
    /// Route a URL to its protocol kind.
    ///
    /// Magnet handling mirrors Tixati's parsing
    /// (`analysis/04_tixati/tixati_architecture.md` §4.3):
    ///   `magnet:?xt=urn:btih:<hash>&dn=<name>&tr=<tracker>...`
    #[must_use]
    pub fn from_url(u: &Url) -> Self {
        match u.scheme() {
            "http" => Self::Http,
            "https" => Self::Https,
            "ftp" | "ftps" => Self::Ftp,
            "magnet" => Self::Magnet,
            _ => {
                // Detect .torrent file by extension.
                let path = u.path();
                if path.ends_with(".torrent") {
                    Self::Torrent
                } else if path.ends_with(".m3u8") || path.ends_with(".m3u") {
                    Self::Hls
                } else {
                    Self::Unknown
                }
            }
        }
    }
}

/// Every protocol engine implements this trait. The trait is `Send + Sync`
/// so a single engine instance can be shared across tasks.
#[async_trait]
pub trait ProtocolEngine: Send + Sync {
    /// Protocol kind handled by this engine.
    fn kind(&self) -> ProtocolKind;

    /// True if this engine can handle the given URL.
    fn accepts(&self, url: &Url) -> bool {
        ProtocolKind::from_url(url) == self.kind()
    }

    /// Drive the task to completion. Returns `Ok(())` on success, `Err` on
    /// permanent failure. Transient failures should be retried internally
    /// before bubbling up.
    async fn run_task(&self, task: &DownloadTask) -> Result<()>;

    /// Best-effort cancellation (optional; engines may no-op).
    async fn cancel(&self, _task_id: u64) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_common_protocols() {
        assert_eq!(ProtocolKind::from_url(&Url::parse("http://x/y").unwrap()), ProtocolKind::Http);
        assert_eq!(ProtocolKind::from_url(&Url::parse("https://x/y").unwrap()), ProtocolKind::Https);
        assert_eq!(
            ProtocolKind::from_url(&Url::parse("magnet:?xt=urn:btih:abc").unwrap()),
            ProtocolKind::Magnet
        );
        assert_eq!(
            ProtocolKind::from_url(&Url::parse("https://x/y.torrent").unwrap()),
            ProtocolKind::Torrent
        );
        assert_eq!(
            ProtocolKind::from_url(&Url::parse("https://x/playlist.m3u8").unwrap()),
            ProtocolKind::Hls
        );
        assert_eq!(
            ProtocolKind::from_url(&Url::parse("ftp://x/y").unwrap()),
            ProtocolKind::Ftp
        );
        assert_eq!(
            ProtocolKind::from_url(&Url::parse("weird://x/y").unwrap()),
            ProtocolKind::Unknown
        );
    }
}
