//! Xunlei BT 引擎（Windows-only）：通过 `xunlei-ffi` 调用迅雷 SDK 匿名 BT 能力。
//!
//! 免登录模式（UserID=0, VipType=0）可直接使用 BT/Tracker/DHT/FreeDCDN。
//! 带身份模式可在 Init 后注入 user_id / vip_type / token_mode / 加速证书，
//! 对齐 `xunlei-ffi` 的 `identity.rs` 三 setter + `handle.rs::set_user_info`。
//! 非 Windows 平台不可用（xunlei-ffi 本身限制）。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

use smart_dl_core::task::DownloadTask;
use smart_dl_core::types::{
    Capability, DownloadEngine, DownloadSource, EngineError, EngineKind, EngineState, EngineStatus,
    EngineTaskId, FileProgress, PeerInfo,
};
use xunlei_ffi::{query::TaskState as XlTaskState, task::TaskId as XlTaskId, XunleiHandle};

/// 迅雷 BT 引擎（匿名 + 可选带身份模式）。
#[derive(Clone)]
pub struct XunleiBtEngine {
    handle: XunleiHandle,
    save_path: std::path::PathBuf,
    /// EngineTaskId(String) -> xunlei TaskId(u64)
    tasks: Arc<Mutex<HashMap<String, XlTaskId>>>,
}

/// 迅雷 BT 引擎建造者（支持身份注入）。
///
/// 默认 = 匿名模式（UserID=0, token_mode=2），与旧 `new()` 行为一致。
/// 调用 `with_*` 注入身份后，`build()` 按顺序下发：
/// `set_token_mode` → `set_app_guid` → `set_user_info` → `set_accelerate_certification`。
pub struct XunleiBtEngineBuilder<'a> {
    sdk_dir: &'a Path,
    save_path: &'a Path,
    app_guid: String,
    token_mode: Option<u32>,
    user_id: Option<String>,
    vip_type: Option<String>,
    accelerate_cert: Option<String>,
}

impl<'a> XunleiBtEngineBuilder<'a> {
    /// 应用 GUID（与 `XL_Init` 的 app_guid 一致；默认 smart-dl-xunlei-001）。
    pub fn with_app_guid(mut self, guid: impl Into<String>) -> Self {
        self.app_guid = guid.into();
        self
    }

    /// 全局 token 模式（见 `xunlei-ffi::identity::set_token_mode`）。
    /// 匿名模式 = 2；带身份登录态 = 1（SDK 内部定义，以实测为准）。
    pub fn with_token_mode(mut self, mode: u32) -> Self {
        self.token_mode = Some(mode);
        self
    }

    /// 迅雷 user_id（数字串，如 "860599297"；来自 pan API JWT `sub`）。
    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    /// VIP 等级文本（语义待真机实测澄清；B 级推断："0"=免费/"1"=普通/"2"=超级）。
    /// 未提供时不调 `set_user_info`，避免 ABI 错配风险。
    pub fn with_vip_type(mut self, vip_type: impl Into<String>) -> Self {
        self.vip_type = Some(vip_type.into());
        self
    }

    /// 加速证书字符串（来自 `speed.auth.vip.xunlei.com/speed/speedup` 下发流程）。
    /// 来源尚未完全澄清（SPEEDUP_SYSTEM.md 遗留未知 #3），暂由调用方注入。
    pub fn with_accelerate_cert(mut self, cert: impl Into<String>) -> Self {
        self.accelerate_cert = Some(cert.into());
        self
    }

    /// 执行 Init + 身份注入。
    pub async fn build(self) -> Result<XunleiBtEngine, EngineError> {
        let handle =
            XunleiHandle::new(self.sdk_dir, self.save_path, self.save_path, &self.app_guid)
                .await
                .map_err(|e| EngineError::Other(format!("xunlei init failed: {e}")))?;

        // identity.rs 三 setter（对齐 sdk_export_inventory.md §5）。
        if let Some(mode) = self.token_mode {
            handle
                .set_token_mode(mode)
                .await
                .map_err(|e| EngineError::Other(format!("xunlei set_token_mode failed: {e}")))?;
        }
        handle
            .set_app_guid(&self.app_guid)
            .await
            .map_err(|e| EngineError::Other(format!("xunlei set_app_guid failed: {e}")))?;
        if let Some(cert) = self.accelerate_cert {
            handle
                .set_accelerate_certification(&cert)
                .await
                .map_err(|e| {
                    EngineError::Other(format!("xunlei set_accelerate_certification failed: {e}"))
                })?;
        }

        // handle.rs set_user_info（ABI 已修正为字符串参数；参数语义待真机澄清）。
        if let (Some(uid), Some(vip)) = (self.user_id, self.vip_type) {
            handle
                .set_user_info(&uid, &vip)
                .await
                .map_err(|e| EngineError::Other(format!("xunlei set_user_info failed: {e}")))?;
        }

        Ok(XunleiBtEngine {
            handle,
            save_path: self.save_path.to_path_buf(),
            tasks: Arc::new(Mutex::new(HashMap::new())),
        })
    }
}

impl XunleiBtEngine {
    fn engine_task_id(xl_task_id: u64) -> EngineTaskId {
        format!("xunlei-{}", xl_task_id)
    }

    async fn insert_task(&self, engine_id: EngineTaskId, xl_id: XlTaskId) {
        self.tasks.lock().await.insert(engine_id, xl_id);
    }

    async fn get_xl_task_id(&self, id: &EngineTaskId) -> Result<XlTaskId, EngineError> {
        let tasks = self.tasks.lock().await;
        tasks.get(id).copied().ok_or_else(|| EngineError::NotFound)
    }

    async fn remove_task(&self, id: &EngineTaskId) -> Option<XlTaskId> {
        self.tasks.lock().await.remove(id)
    }

    fn map_state(state: XlTaskState) -> EngineState {
        match state {
            XlTaskState::Pending => EngineState::MetadataPending,
            XlTaskState::Downloading => EngineState::Downloading,
            XlTaskState::Paused => EngineState::Paused,
            XlTaskState::Completed => EngineState::Seeding,
            // 其他状态（未真机确认）保守归为 Downloading
            XlTaskState::Unknown => EngineState::Downloading,
        }
    }
}

#[async_trait::async_trait]
impl DownloadEngine for XunleiBtEngine {
    fn id(&self) -> &str {
        "xunlei"
    }

    fn kind(&self) -> EngineKind {
        EngineKind::Bt
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Magnet,
            Capability::TorrentFile,
            Capability::Peer,
            Capability::Tracker,
            Capability::Dht,
            Capability::WebSeed,
            Capability::PeerBan,
        ]
    }

    async fn add(&self, task: &DownloadTask) -> Result<EngineTaskId, EngineError> {
        if task.dest_root != Path::new(".") && task.dest_root != self.save_path {
            return Err(EngineError::Other(format!(
                "Xunlei BT 引擎全局落盘于 {:?}，任务 dest {:#?} 不支持",
                self.save_path, task.dest_root
            )));
        }

        let xl_id = match &task.source {
            DownloadSource::Magnet(magnet) => self
                .handle
                .create_magnet_task(magnet, &self.save_path)
                .await
                .map_err(|e| EngineError::Other(format!("xunlei create magnet failed: {e}")))?,
            DownloadSource::TorrentFile(bytes) => self
                .handle
                .create_bt_task(bytes.as_slice(), &self.save_path)
                .await
                .map_err(|e| EngineError::Other(format!("xunlei create bt failed: {e}")))?,
            DownloadSource::Thunder(link) => {
                // 解码 thunder:// 为 HTTP URL，但 xunlei 引擎当前只支持 BT 任务，
                // 不直接支持 HTTP URL。此处返回 Unsupported 让路由层走 HTTP 引擎。
                return Err(EngineError::Other(format!(
                    "xunlei engine does not support thunder links directly: {link}"
                )));
            }
            DownloadSource::XunleiShare(_) => {
                return Err(EngineError::Other(
                    "xunlei share links require cloud login (not supported in anonymous mode)"
                        .to_string(),
                ));
            }
            _ => {
                return Err(EngineError::Other(
                    "unsupported source for xunlei engine".to_string(),
                ))
            }
        };

        let engine_id = Self::engine_task_id(xl_id.0);
        self.insert_task(engine_id.clone(), xl_id).await;

        // 启动任务
        self.handle
            .start_task(&xl_id)
            .await
            .map_err(|e| EngineError::Other(format!("xunlei start failed: {e}")))?;

        // 启用 FreeDCDN 加速（免登录可用）
        let _ = self.handle.enable_free_dcdn(&xl_id).await;

        Ok(engine_id)
    }

    async fn pause(&self, id: &EngineTaskId) -> Result<(), EngineError> {
        let xl_id = self.get_xl_task_id(id).await?;
        self.handle
            .stop_task(&xl_id)
            .await
            .map_err(|e| EngineError::Other(format!("xunlei stop failed: {e}")))
    }

    async fn resume(&self, id: &EngineTaskId) -> Result<(), EngineError> {
        let xl_id = self.get_xl_task_id(id).await?;
        self.handle
            .start_task(&xl_id)
            .await
            .map_err(|e| EngineError::Other(format!("xunlei start failed: {e}")))
    }

    async fn status(&self, id: &EngineTaskId) -> Result<EngineStatus, EngineError> {
        let xl_id = self.get_xl_task_id(id).await?;
        let info = self
            .handle
            .query_task_info(&xl_id)
            .await
            .map_err(|e| EngineError::Other(format!("xunlei query failed: {e}")))?;

        let state = Self::map_state(info.state);
        let total = info.file_size;
        let total_done = info.download_size;
        // 2026-08-27 真机铁证：XLTaskInfo 仅 dump 确认了 state/file_size/download_size/
        // peer_count/conn_count 字段；error_code/error_msg/速度字段尚未 dump 还原，暂不映射。
        let error = None;

        Ok(EngineStatus {
            state,
            metadata_received: true, // xunlei SDK 内部处理 metadata
            files: vec![FileProgress {
                rel_path: String::new(),
                done: total_done,
                size: total,
            }],
            total_done,
            total,
            down_rate: 0, // 速度字段待 dump 还原
            up_rate: 0,
            // E33：xunlei SDK 速度/累计字段尚未 dump 还原，恒 0（快照序列化省略）
            total_downloaded: 0,
            total_uploaded: 0,
            num_peers: info.peer_count,
            num_seeds: 0, // xunlei SDK 不区分 seed/peer
            error,
            // E9：xunlei 引擎暂不参与名字回填（XLTaskInfo 未还原 name 字段）
            name: None,
        })
    }

    async fn remove(&self, id: &EngineTaskId, delete_data: bool) -> Result<(), EngineError> {
        let xl_id = self.get_xl_task_id(id).await?;
        self.handle
            .delete_task(&xl_id, delete_data)
            .await
            .map_err(|e| EngineError::Other(format!("xunlei delete failed: {e}")))?;
        self.remove_task(id).await;
        Ok(())
    }

    async fn peers(&self, _id: &EngineTaskId) -> Result<Vec<PeerInfo>, EngineError> {
        // xunlei SDK 不提供任务级 peer 列表查询（仅统计数）。
        // 已注入的 peer 可通过内部跟踪补充，v1 先返回空。
        Ok(vec![])
    }

    async fn update_sources(
        &self,
        _id: &EngineTaskId,
        _urls: Vec<String>,
    ) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }

    async fn add_url_seed(&self, id: &EngineTaskId, url: &str) -> Result<(), EngineError> {
        let xl_id = self.get_xl_task_id(id).await?;
        self.handle
            .add_server(&xl_id, url)
            .await
            .map_err(|e| EngineError::Other(format!("xunlei add server failed: {e}")))
    }

    async fn ban_peer(&self, id: &EngineTaskId, peer: SocketAddr) -> Result<(), EngineError> {
        let xl_id = self.get_xl_task_id(id).await?;
        self.handle
            .discard_peer(&xl_id, peer)
            .await
            .map_err(|e| EngineError::Other(format!("xunlei discard peer failed: {e}")))
    }

    async fn read_piece(&self, _id: &EngineTaskId, _idx: u32) -> Result<Vec<u8>, EngineError> {
        Err(EngineError::Unsupported)
    }

    async fn add_xunlei_resume(&self, _data: Vec<u8>) -> Result<EngineTaskId, EngineError> {
        Err(EngineError::Other(
            "XunleiBtEngine 不支持 fastresume 导入，请使用 libtorrent 引擎".into(),
        ))
    }
}
