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
        /// 任务级代理 URL（E5）：`http(s)://` / `socks5://` / `socks4://`，可带
        /// `user:pass@`。None = 走引擎共享 client（可能含全局 `[download] proxy`）；
        /// Some = 该任务专用 client 仅装此代理（覆盖全局）。仅 HTTP 任务生效。
        #[serde(default)]
        proxy: Option<String>,
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
                proxy,
            } => format!(
                "Http {{ url: {:?}, headers: {:?}, auth: {}, backup_url: {}, proxy: {} }}",
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
                // E5：proxy URL 可含 user:pass@ 凭据，同 url/backup_url 口径脱敏
                proxy
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

    /// 搜索语料（E14）：来源中可供关键字匹配的 URL 集合——Http 主源 + 备用源、
    /// Ftp url、Magnet/Thunder/XunleiShare/Ed2k 链接；TorrentFile 纯二进制
    /// 无 URL → 空集。脱敏复用 `redact_url`（userinfo/敏感 query → [REDACTED]），
    /// 与快照展示口径一致——按凭据片段搜索命中不了，防止 search 侧信道泄漏。
    pub fn search_urls(&self) -> Vec<String> {
        match self {
            DownloadSource::Magnet(u)
            | DownloadSource::Thunder(u)
            | DownloadSource::XunleiShare(u)
            | DownloadSource::Ed2k(u) => vec![redact_url(u)],
            DownloadSource::TorrentFile(_) => vec![],
            DownloadSource::Http {
                url, backup_url, ..
            } => {
                let mut v = vec![redact_url(url)];
                if let Some(b) = backup_url {
                    v.push(redact_url(b));
                }
                v
            }
            DownloadSource::Ftp { url, .. } => vec![redact_url(url)],
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

/// BT 任务 tracker 表项（E29 运行时增删查；tier 同 libtorrent 语义，小者优先）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TrackerEntry {
    pub url: String,
    pub tier: i32,
}

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
    /// 任务累计统计（E33）：BT = libtorrent all_time_download/all_time_upload
    /// （全生命周期含历史，随 resume data 跨会话持久，暂停不清零）。
    /// HTTP/FTP 等单向引擎无对等口径恒 0（daemon 快照序列化时省略）。
    pub total_downloaded: u64,
    pub total_uploaded: u64,
    pub num_peers: u32,
    pub num_seeds: u32,
    pub error: Option<String>,
    /// 引擎侧最终落盘名（E9 透出）：HTTP 引擎在 add 探测后即定——显式名回显
    /// 同值；派生名 = CD → URL 末段 → 兜底链结果（已 sanitize_rel 终审）。
    /// daemon 轮询据此回填 `metadata.name`（空缺时），使派生名进入列表/快照
    /// 透出链。None = 引擎未透出（BT/FTP/NAS/xunlei 暂不参与回填）。
    pub name: Option<String>,
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

    /// 引擎全局限速热改（E16 总阀门）：作用于该引擎的**所有**任务合计速率。
    /// 方向语义与 `set_limits` 一致：`None` = 不调整；`Some(0)` = 不限；
    /// `Some(n)` = 合计上限 n KiB/s。引擎无该方向（HTTP/FTP 无上传）报
    /// `EngineError::Other`；整个操作不支持时返回 `EngineError::Unsupported`。
    /// 默认实现：不支持（引擎可安全忽略，daemon 侧按「尽力而为」处理）。
    async fn set_global_limits(
        &self,
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

    /// 任务级代理热改（E8）：`Some(url)` = 切任务专用 client（覆盖全局，语义
    /// 与 add 时设定一致）；`None` = 清除回引擎共享 client。HTTP 引擎实现：
    /// 非法 URL → `Other`（调用方定性入参错误，不动现任务）；下载中任务
    /// epoch+1 重入——旧循环在 gen/epoch 检查点自杀，新循环从段账本恢复并
    /// 用新 client；暂停/终态任务只改配置（下次 spawn 生效）。不支持引擎
    /// （BT 代理属会话级 / FTP）→ `Unsupported`。
    async fn set_task_proxy(
        &self,
        _id: &EngineTaskId,
        _proxy: Option<String>,
    ) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }

    /// 批量追加 tracker（E29，BT 任务；announce/webseed 无关，metadata 未
    /// 就绪也可设）。不支持引擎（HTTP/FTP）→ `Unsupported`。
    async fn add_trackers(&self, _id: &EngineTaskId, _urls: &[String]) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }

    /// 删 tracker（E29，BT 任务）：URL 精确匹配，无匹配 → `NotFound`。
    async fn remove_tracker(&self, _id: &EngineTaskId, _url: &str) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }

    /// 列举 tracker 表（E29，BT 任务）：返回当前 announce 表（URL + tier）。
    async fn list_trackers(&self, _id: &EngineTaskId) -> Result<Vec<TrackerEntry>, EngineError> {
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
            proxy: None,
        };
        let d = s.redacted_debug();
        assert!(!d.contains("SESSION"), "headers 值不得出现: {d}");
        assert!(!d.contains(":p@"), "userinfo 不得出现: {d}");
        assert!(!d.contains("\"b\""), "auth 值不得出现: {d}");
        assert!(d.contains("[REDACTED]"), "应含 REDACTED 标记: {d}");
    }

    /// E14 搜索语料：Http 主源 + 备用源全集、userinfo/敏感 query 脱敏、
    /// TorrentFile 空语料——按凭据片段搜索命中不了（防 search 侧信道）。
    #[test]
    fn search_urls_redacts_and_covers_backup() {
        let http = DownloadSource::Http {
            url: "http://u:p@host.lan/file.iso?token=zzz".into(),
            headers: vec![],
            auth: None,
            backup_url: Some("http://bak.lan/file.iso".into()),
            proxy: None,
        };
        let urls = http.search_urls();
        assert_eq!(urls.len(), 2, "主源 + 备用源: {urls:?}");
        assert!(
            urls.iter()
                .all(|u| !u.contains(":p@") && !u.contains("zzz")),
            "凭据/敏感 query 不得进入搜索语料: {urls:?}"
        );
        assert!(urls[0].contains("host.lan/file.iso"));
        assert!(urls[1].contains("bak.lan"));

        assert!(DownloadSource::TorrentFile(vec![1, 2, 3])
            .search_urls()
            .is_empty());
        let mg = DownloadSource::Magnet("magnet:?xt=urn:btih:ABC".into());
        assert_eq!(mg.search_urls(), vec!["magnet:?xt=urn:btih:ABC"]);
    }

    /// E5 任务级代理：proxy URL（可含 user:pass@）参与 serde 往返，
    /// redacted_debug 按同口径脱敏（凭据不得出现）。
    #[test]
    fn http_proxy_field_roundtrip_and_redaction() {
        let s = DownloadSource::Http {
            url: "http://h/file".into(),
            headers: vec![],
            auth: None,
            backup_url: None,
            proxy: Some("http://alice:secret123@proxy.lan:8080".into()),
        };
        // serde 往返：旧数据（无 proxy 字段）反序列化 → None 已由 serde(default) 保证
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("proxy"), "proxy 应序列化: {json}");
        let back: DownloadSource = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s, "serde 往返应保真");

        // redacted_debug：凭据脱敏，代理主机保留（可运维定位）
        let d = s.redacted_debug();
        assert!(
            !d.contains("alice") && !d.contains("secret123"),
            "proxy 凭据不得出现: {d}"
        );
        assert!(d.contains("proxy.lan"), "proxy 主机应保留: {d}");
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
