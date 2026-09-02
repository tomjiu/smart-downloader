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
        /// 备用源 URL（夸克 backup_url 机制：主源失败后切换；None = 无备用源）。
        #[serde(default)]
        backup_url: Option<String>,
    },
    Ftp {
        url: String,
        user: String,
        pass: String,
    },
    Thunder(String), // 解码为 Http（§7.1）
    /// 迅雷网盘分享链接（pan.xunlei.com/s/xxx?pwd=yyy）。
    XunleiShare(String),
    Ed2k(String), // v1 不支持 → Failed
}

/// HTTP(S) 认证（Digest 属 v2）。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Auth {
    Basic(String, String),
    Bearer(String),
}

/// 安全修复（V6，CWE-532）：敏感 query 参数名（值替换为 `***`）。
const SENSITIVE_QUERY_KEYS: &[&str] = &[
    "token",
    "key",
    "sign",
    "sig",
    "signature",
    "auth",
    "pwd",
    "password",
    "passwd",
    "secret",
];

/// 剥 URL userinfo（`scheme://user:pass@host` → `scheme://***@host`）并把敏感
/// query 参数值替换为 `***`。用于日志/快照等对外展示面，替代 `{:?}` 直通。
pub fn redact_url(url: &str) -> String {
    let (scheme_rest, has_scheme) = match url.split_once("://") {
        Some((_, r)) => (r, true),
        None => (url, false),
    };
    // 1) userinfo：只取最后一个 '@' 之前的部分（userinfo 内不会出现 '/'）
    let (host_part, query_part) = match scheme_rest.split_once(['?', '#']) {
        Some((h, rest)) => (h, Some(rest)),
        None => (scheme_rest, None),
    };
    let host_part = match host_part.rsplit_once('@') {
        Some((_, h)) => format!("***@{h}"),
        None => host_part.to_string(),
    };
    let mut out = String::with_capacity(url.len() + 8);
    if has_scheme {
        // 保留原 scheme（含长度）；直接从原文取 scheme:// 前缀
        let idx = url.find("://").unwrap();
        out.push_str(&url[..idx + 3]);
    }
    out.push_str(&host_part);
    if let Some(q) = query_part {
        // 分隔符（? 或 #）保留原样：从原文找第一个 '?'/'#' 的位置
        let sep_idx = scheme_rest.find(['?', '#']).unwrap();
        let sep = scheme_rest[sep_idx..].chars().next().unwrap();
        out.push(sep);
        let q = q.split('#').next().unwrap_or(q); // 丢弃 fragment
        let parts: Vec<String> = q
            .split('&')
            .map(|kv| match kv.split_once('=') {
                Some((k, _)) if SENSITIVE_QUERY_KEYS.contains(&k.to_ascii_lowercase().as_str()) => {
                    format!("{k}=***")
                }
                _ => kv.to_string(),
            })
            .collect();
        out.push_str(&parts.join("&"));
    }
    out
}

impl DownloadSource {
    /// 安全修复（V6）：脱敏 Debug 视图，替代 `format!("{:?}", source)` 直通——
    /// HTTP headers 值（可含 Cookie/Authorization）、auth 字段、FTP 密码与
    /// 链接 userinfo/敏感 query 全部替换为 `[REDACTED]`/`***`。
    pub fn redacted_debug(&self) -> String {
        match self {
            DownloadSource::Http {
                url,
                headers,
                auth,
                backup_url,
            } => format!(
                "Http {{ url: {:?}, headers: {:?}, auth: {}, backup_url: {} }}",
                redact_url(url),
                headers.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
                if auth.is_some() {
                    "Some([REDACTED])"
                } else {
                    "None"
                },
                backup_url
                    .as_deref()
                    .map(|u| format!("Some({:?})", redact_url(u)))
                    .unwrap_or_else(|| "None".into()),
            ),
            DownloadSource::Ftp { url, user, .. } => format!(
                "Ftp {{ url: {:?}, user: {:?}, pass: [REDACTED] }}",
                redact_url(url),
                user
            ),
            DownloadSource::Thunder(s) => format!("Thunder({:?})", redact_url(s)),
            DownloadSource::XunleiShare(s) => format!("XunleiShare({:?})", redact_url(s)),
            other => format!("{other:?}"),
        }
    }
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
    /// NAS 版迅雷引擎（xllite/pan-cli 远程托管，daemon feature `nas`，附录 E）。
    XunleiNas,
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
    async fn add_peer(
        &self,
        _id: &EngineTaskId,
        _peer: std::net::SocketAddr,
    ) -> Result<(), EngineError> {
        Ok(()) // 默认：不支持直连 peer 注入
    }
    async fn ban_peer(&self, id: &EngineTaskId, peer: SocketAddr) -> Result<(), EngineError>;
    async fn read_piece(&self, id: &EngineTaskId, idx: u32) -> Result<Vec<u8>, EngineError>;

    /// 任务级限速（KiB/s）。`None` 方向 = 不调整；`Some(0)` = 不限；
    /// `Some(n)` = 上限 n KiB/s。引擎无该方向（如 HTTP 无上传）时报
    /// `EngineError::Other`；整个操作不支持时返回 `EngineError::Unsupported`。
    /// 默认实现：不支持（FTP 等无限速基础设施的引擎无需覆写）。
    async fn set_limits(
        &self,
        _id: &EngineTaskId,
        _down_kb_s: Option<u32>,
        _up_kb_s: Option<u32>,
    ) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }

    /// 任务级子文件优先级批量设置（BT 多文件）。`priorities` =
    /// (文件下标, 0..=7)，0=不下载 / 1=低 / 4=默认 / 7=最高（libtorrent 语义）。
    /// 需要 metadata 的引擎（BT）在 metadata 未就绪时返回 `EngineError::Other`。
    async fn set_file_priorities(
        &self,
        _id: &EngineTaskId,
        _priorities: &[(usize, u32)],
    ) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }

    /// 读取当前各文件优先级（下标即文件序）。非 BT / 不支持 → `Unsupported`。
    async fn file_priorities(&self, _id: &EngineTaskId) -> Result<Vec<Option<u32>>, EngineError> {
        Err(EngineError::Unsupported)
    }

    /// 任务级顺序下载开关（边下边播）。HTTP = 收紧在飞段窗口（新建任务立即
    /// 生效；运行中任务自下一次重下轮起生效）；BT = sequential_download flag
    /// （即时生效，metadata 未就绪也可设）；不支持引擎（FTP）→ `Unsupported`。
    async fn set_sequential(&self, _id: &EngineTaskId, _on: bool) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }

    /// 迅雷任务导入（M9）：接受 xunlei-convert 生成的 fastresume bencode。
    /// 默认实现返回 `not supported`；BT 引擎（libtorrent）应Override为 `add_torrent_resume`。
    async fn add_xunlei_resume(&self, _data: Vec<u8>) -> Result<EngineTaskId, EngineError> {
        Err(EngineError::Other(
            "add_xunlei_resume not supported by this engine".into(),
        ))
    }
}

#[cfg(test)]
mod redact_tests {
    use super::*;

    #[test]
    fn redact_url_strips_userinfo() {
        assert_eq!(
            redact_url("ftp://alice:s3cret@host.example/file.bin"),
            "ftp://***@host.example/file.bin"
        );
        assert_eq!(
            redact_url("http://u:p@h/x?a=1&token=abc"),
            "http://***@h/x?a=1&token=***"
        );
    }

    #[test]
    fn redact_url_masks_sensitive_query() {
        assert_eq!(
            redact_url("https://pan.example.com/s/xyz?pwd=8888&t=1"),
            "https://pan.example.com/s/xyz?pwd=***&t=1"
        );
        // 无 scheme 的相对形态也要安全（保守不处理 userinfo）
        assert_eq!(redact_url("/a?signature=zzz&b=2"), "/a?signature=***&b=2");
    }

    #[test]
    fn redacted_debug_hides_http_credentials() {
        let s = DownloadSource::Http {
            url: "http://u:p@h/file".into(),
            headers: vec![("Cookie".into(), "SESSION=xyz".into())],
            auth: Some(Auth::Basic("a".into(), "b".into())),
            backup_url: None,
        };
        let d = s.redacted_debug();
        assert!(!d.contains("SESSION"), "headers 值不得出现: {d}");
        assert!(!d.contains(":p@"), "userinfo 不得出现: {d}");
        assert!(!d.contains("\"b\""), "auth 值不得出现: {d}");
        assert!(d.contains("[REDACTED]"), "应含 REDACTED 标记: {d}");
    }

    #[test]
    fn redacted_debug_hides_ftp_pass() {
        let s = DownloadSource::Ftp {
            url: "ftp://h/f".into(),
            user: "u".into(),
            pass: "topsecret".into(),
        };
        let d = s.redacted_debug();
        assert!(!d.contains("topsecret"), "pass 不得出现: {d}");
        assert!(d.contains("[REDACTED]"), "应含 REDACTED 标记: {d}");
    }
}
