//! 统一能力模型（§4）：下载源、能力、引擎抽象、状态快照。

use crate::task::DownloadTask;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// 用户提交的下载源（§4）。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum DownloadSource {
    Magnet(String),
    TorrentFile(Vec<u8>),
    Http {
        url: String,
        headers: Vec<(String, String)>,
        auth: Option<Auth>,
    },
    Ftp {
        url: String,
        user: String,
        pass: String,
    },
    Thunder(String), // 解码为 Http（§7.1）
    /// 迅雷网盘分享链接（pan.xunlei.com/s/xxx?pwd=yyy）。
    XunleiShare(String),
    Ed2k(String),    // v1 不支持 → Failed
}

/// HTTP(S) 认证（Digest 属 v2）。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Auth {
    Basic(String, String),
    Bearer(String),
}

/// 引擎能力位（§4）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Capability {
    Magnet,
    TorrentFile,
    Peer,
    Tracker,
    Dht,
    WebSeed,
    PieceRead,
    PeerBan,
    Sequential,
    Stream,
    Http,
    Https,
    Range,
    MultiConnection,
    Mirror,
    UrlRefresh,
    Ftp,
    FtpResume,
    OfflineCache,
}

/// 引擎种类（用于配额与任务标注）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum EngineKind {
    Bt,
    Http,
    Ftp,
    Provider,
}

/// 引擎任务句柄（v1 用引擎侧生成字符串 id）。
pub type EngineTaskId = String;

/// 引擎错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    #[error("task not found")]
    NotFound,
    #[error("engine error: {0}")]
    Other(String),
    #[error("unsupported operation")]
    Unsupported,
}

/// 引擎状态快照（快照轮询，1s）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EngineStatus {
    pub state: EngineState,
    pub metadata_received: bool,
    pub files: Vec<FileProgress>,
    pub total_done: u64,
    pub total: u64,
    pub down_rate: u64,
    pub up_rate: u64,
    pub num_peers: u32,
    pub num_seeds: u32,
    pub error: Option<String>,
}

/// 引擎侧任务状态。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum EngineState {
    #[default]
    MetadataPending,
    Downloading,
    Completed,
    Paused,
    Error,
    Seeding,
}

/// 单文件进度。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileProgress {
    pub rel_path: String,
    pub done: u64,
    pub size: u64,
}

/// 富 peer 信息（§4 / M1 btcore 同构）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PeerInfo {
    pub ip: String,
    pub port: u16,
    pub peer_id: String,
    pub client: String,
    pub progress_ppm: u32,
    pub down_rate: u64,
    pub up_rate: u64,
    pub total_download: u64,
    pub total_upload: u64,
    pub last_active_sec: u64,
    pub flags: String,
}

/// 引擎统一抽象（§4）。M3/M5/M6 消费本 trait，不得改签名。
#[async_trait::async_trait]
pub trait DownloadEngine: Send + Sync {
    fn id(&self) -> &str;
    fn kind(&self) -> EngineKind;
    fn capabilities(&self) -> Vec<Capability>;

    async fn add(&self, task: &DownloadTask) -> Result<EngineTaskId, EngineError>;
    async fn pause(&self, id: &EngineTaskId) -> Result<(), EngineError>;
    async fn resume(&self, id: &EngineTaskId) -> Result<(), EngineError>;
    async fn status(&self, id: &EngineTaskId) -> Result<EngineStatus, EngineError>;
    async fn remove(&self, id: &EngineTaskId, delete_data: bool) -> Result<(), EngineError>;
    async fn peers(&self, id: &EngineTaskId) -> Result<Vec<PeerInfo>, EngineError>;
    async fn update_sources(&self, id: &EngineTaskId, urls: Vec<String>)
        -> Result<(), EngineError>;
    async fn add_url_seed(&self, id: &EngineTaskId, url: &str) -> Result<(), EngineError>;
    async fn ban_peer(&self, id: &EngineTaskId, peer: SocketAddr) -> Result<(), EngineError>;
    async fn read_piece(&self, id: &EngineTaskId, idx: u32) -> Result<Vec<u8>, EngineError>;
}
