//! 共享 mock 引擎：实现 `DownloadEngine`，可注入 peers/seeds/progress/error。

use async_trait::async_trait;
use parking_lot::Mutex;
use smart_dl_core::task::DownloadTask;
use smart_dl_core::types::{Capability, DownloadEngine, EngineError, EngineKind, EngineStatus};
use std::net::SocketAddr;

pub struct MockEngine {
    pub id: String,
    pub kind: EngineKind,
    pub caps: Vec<Capability>,
    pub status: Mutex<EngineStatus>,
    pub error: Mutex<Option<EngineError>>,
    pub added: Mutex<Vec<String>>,
}

// 各测试 binary 只用部分辅助方法；跨 binary 均编译 → 允许未用
#[allow(dead_code)]
impl MockEngine {
    pub fn new(id: &str, kind: EngineKind, caps: Vec<Capability>) -> Self {
        MockEngine {
            id: id.to_string(),
            kind,
            caps,
            status: Mutex::new(EngineStatus::default()),
            error: Mutex::new(None),
            added: Mutex::new(Vec::new()),
        }
    }

    pub fn bt() -> Self {
        Self::new(
            "bt",
            EngineKind::Bt,
            vec![
                Capability::Magnet,
                Capability::TorrentFile,
                Capability::Peer,
                Capability::Tracker,
                Capability::Dht,
                Capability::WebSeed,
            ],
        )
    }

    pub fn http() -> Self {
        Self::new(
            "http",
            EngineKind::Http,
            vec![
                Capability::Http,
                Capability::Https,
                Capability::Range,
                Capability::MultiConnection,
            ],
        )
    }

    pub fn ftp() -> Self {
        Self::new(
            "ftp",
            EngineKind::Ftp,
            vec![Capability::Ftp, Capability::FtpResume],
        )
    }

    pub fn set_peers_seeds(&self, peers: u32, seeds: u32) {
        let mut s = self.status.lock();
        s.num_peers = peers;
        s.num_seeds = seeds;
    }

    pub fn set_progress(&self, ratio: f64) {
        let mut s = self.status.lock();
        s.total = 1000;
        s.total_done = (1000.0 * ratio) as u64;
    }

    pub fn set_error(&self, err: EngineError) {
        *self.error.lock() = Some(err);
    }

    pub fn added_tasks(&self) -> Vec<String> {
        self.added.lock().clone()
    }
}

#[async_trait]
impl DownloadEngine for MockEngine {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> EngineKind {
        self.kind
    }

    fn capabilities(&self) -> Vec<Capability> {
        self.caps.clone()
    }

    async fn add(&self, task: &DownloadTask) -> Result<String, EngineError> {
        if let Some(e) = self.error.lock().as_ref() {
            if matches!(e, EngineError::Other(s) if s == "add") {
                return Err(EngineError::Other("add".into()));
            }
        }
        self.added.lock().push(task.canonical_id.identity.clone());
        Ok(format!("{}-1", self.id))
    }

    async fn pause(&self, _id: &String) -> Result<(), EngineError> {
        Ok(())
    }

    async fn resume(&self, _id: &String) -> Result<(), EngineError> {
        Ok(())
    }

    async fn status(&self, _id: &String) -> Result<EngineStatus, EngineError> {
        if let Some(e) = self.error.lock().clone() {
            return Err(e);
        }
        Ok(self.status.lock().clone())
    }

    async fn remove(&self, _id: &String, _delete_data: bool) -> Result<(), EngineError> {
        Ok(())
    }

    async fn peers(
        &self,
        _id: &String,
    ) -> Result<Vec<smart_dl_core::types::PeerInfo>, EngineError> {
        Ok(Vec::new())
    }

    async fn update_sources(&self, _id: &String, _urls: Vec<String>) -> Result<(), EngineError> {
        Ok(())
    }

    async fn add_url_seed(&self, _id: &String, _url: &str) -> Result<(), EngineError> {
        Ok(())
    }

    async fn ban_peer(&self, _id: &String, _peer: SocketAddr) -> Result<(), EngineError> {
        Ok(())
    }

    async fn read_piece(&self, _id: &String, _idx: u32) -> Result<Vec<u8>, EngineError> {
        Ok(Vec::new())
    }
}
