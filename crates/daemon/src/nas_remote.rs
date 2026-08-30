//! NAS 远程引擎适配器（B-2 骨架，feature `nas`）：把 xllite（DriveListen TCP 面）
//! 包装为 `DownloadEngine` trait，接入 daemon 引擎表与热度路由。
//!
//! ⚠ 端点形状全部为**假设区 #9**（登录门后 API 实测校准项）——本文件只落实
//! trait 适配与控制流，HTTP 端点/字段名以扫码实测结果为准填空：
//!   - 任务提交：POST /device/v1/tasks（形状未验证）
//!   - 任务列表：GET  /device/v1/tasks
//!   - 暂停/恢复：POST /device/v1/tasks/{id}/pause|resume（未验证）
//!   - try_speed：GET /device/v1/try_speed/get_info（端点字符串已实证，参数未验证）
//! 已实证事实（附录 E）：DriveListen 为 gin HTTP（默认 127.0.0.1:5050）；
//! 登录门前 web 不监听；token 预置路径 `HOME/auth_token.json`（#8）。
//!
//! 因此本适配器所有调用在引擎未登录时返回 `EngineError::Other("nas engine
//! offline/login pending")` 语义——daemon 侧按引擎不可用兜底（回落主线引擎）。

use std::sync::Arc;

use smart_dl_core::task::DownloadTask;
use smart_dl_core::types::{
    Capability, DownloadEngine, EngineError, EngineKind, EngineTaskId, EngineStatus, PeerInfo,
};

use crate::nas::NasManager;

/// xllite 远程引擎（经 DriveListen HTTP 面）。
pub struct NasRemoteEngine {
    mgr: Arc<NasManager>,
    // #9 实测校准后启用（端点表填空时即用）
    #[allow(dead_code)]
    client: reqwest::Client,
    #[allow(dead_code)]
    base: String,
}

impl NasRemoteEngine {
    pub fn new(mgr: Arc<NasManager>) -> Self {
        let base = format!("http://{}", mgr_status_listen(&mgr));
        Self {
            mgr,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
            base,
        }
    }

    /// 引擎在线探活（HTTP 200/30x 皆算在线；401/403 视为已登录但鉴权拒绝）。
    async fn online(&self) -> Result<(), EngineError> {
        let st = self.mgr.status().await;
        if !st.proc_alive {
            return Err(EngineError::Other("nas engine not running".into()));
        }
        match st.http_code {
            Some(_) => Ok(()),
            None => Err(EngineError::Other("nas engine offline/login pending".into())),
        }
    }
}

fn mgr_status_listen(mgr: &NasManager) -> String {
    // 与 NasConfig 默认一致；管理器构造后 listen 不变，取 status 不必要——
    // 直接读默认（跨配置请在 NasConfig::default 改）。
    let _ = mgr;
    "127.0.0.1:5050".to_string()
}

#[async_trait::async_trait]
impl DownloadEngine for NasRemoteEngine {
    fn id(&self) -> &str {
        "xllite-nas"
    }
    fn kind(&self) -> EngineKind {
        EngineKind::XunleiNas
    }
    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Http,
            Capability::Https,
            Capability::Magnet,
            Capability::TorrentFile,
            Capability::OfflineCache, // 云盘/离线族能力（withHighSpeedFlowCtrl 实证）
        ]
    }

    async fn add(&self, task: &DownloadTask) -> Result<EngineTaskId, EngineError> {
        self.online().await?;
        // TODO(#9 实测校准): POST {base}/device/v1/tasks
        //   body 形状（候选）：{"urls": task.urls, "type": "url"|"magnet"|"torrent", "dir": DownloadPATH}
        Err(EngineError::Other(format!(
            "nas add({}) 端点未校准（假设区 #9）",
            task.id
        )))
    }

    async fn pause(&self, _id: &EngineTaskId) -> Result<(), EngineError> {
        self.online().await?;
        Err(EngineError::Other("nas pause 端点未校准（#9）".into()))
    }

    async fn resume(&self, _id: &EngineTaskId) -> Result<(), EngineError> {
        self.online().await?;
        Err(EngineError::Other("nas resume 端点未校准（#9）".into()))
    }

    async fn status(&self, _id: &EngineTaskId) -> Result<EngineStatus, EngineError> {
        self.online().await?;
        Err(EngineError::Other("nas status 端点未校准（#9）".into()))
    }

    async fn remove(&self, _id: &EngineTaskId, _delete_data: bool) -> Result<(), EngineError> {
        self.online().await?;
        Err(EngineError::Other("nas remove 端点未校准（#9）".into()))
    }

    async fn peers(&self, _id: &EngineTaskId) -> Result<Vec<PeerInfo>, EngineError> {
        self.online().await?;
        Err(EngineError::Other("nas peers 端点未校准（#9）".into()))
    }

    async fn update_sources(&self, _id: &EngineTaskId, _urls: Vec<String>) -> Result<(), EngineError> {
        self.online().await?;
        Err(EngineError::Other("nas update_sources 端点未校准（#9）".into()))
    }

    async fn add_url_seed(&self, _id: &EngineTaskId, _url: &str) -> Result<(), EngineError> {
        self.online().await?;
        Err(EngineError::Other("nas add_url_seed 端点未校准（#9）".into()))
    }

    async fn ban_peer(&self, _id: &EngineTaskId, _peer: std::net::SocketAddr) -> Result<(), EngineError> {
        self.online().await?;
        Err(EngineError::Other("nas ban_peer 端点未校准（#9）".into()))
    }

    async fn read_piece(&self, _id: &EngineTaskId, _idx: u32) -> Result<Vec<u8>, EngineError> {
        self.online().await?;
        Err(EngineError::Other("nas read_piece 端点未校准（#9）".into()))
    }
}
