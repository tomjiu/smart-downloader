//! BtEngine（feature `bt`）：libtorrent 薄核（smart-dl-btcore）接入 DownloadEngine。
//!
//! 单个 BtCore session（save_path = 任务默认落盘目录）；engine_tid = libtorrent
//! 返回的 infohash（40 hex）。magnet / .torrent 文件 → add_magnet / add_torrent_file。
//! 状态映射：lt state 0 下载 / 1 完成 / 3 错误 / 4 元数据获取中（ABI100 无暂停态，
//! 暂停以 alert 同步——v1 用 pause/resume 直调，状态以 status 为准）。
//!
//! **断点续传（#5 fastresume 显式保存）**：remove/pause 前 `request_save_resume` →
//! 轮询 RESUME·ready alert → `take_resume_data` → 原子写 `<save_path>/<ih>.fastresume`。
//! 重启后 add 同一 magnet/torrent 时按 infohash 查 `.fastresume` → `add_torrent_resume`
//! 回灌 → libtorrent 恢复 piece 位图 + metadata，免全盘 checking / 免重新抓取 metadata。

use smart_dl_btcore::{AlertKind, BtCore, TorrentStatus};
use smart_dl_core::task::DownloadTask;
use smart_dl_core::types::{
    Capability, DownloadEngine, DownloadSource, EngineError, EngineKind, EngineState, EngineStatus,
    EngineTaskId, FileProgress, PeerInfo,
};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const LT_STATE_COMPLETED: i32 = 1;
const LT_STATE_ERROR: i32 = 3;
const LT_STATE_METADATA: i32 = 4;

fn map_state(st: i32) -> EngineState {
    match st {
        LT_STATE_METADATA => EngineState::MetadataPending,
        LT_STATE_COMPLETED => EngineState::Seeding,
        LT_STATE_ERROR => EngineState::Error,
        _ => EngineState::Downloading,
    }
}

fn map_status(st: &TorrentStatus) -> EngineStatus {
    let state = map_state(st.state);
    let total = st.total.max(0) as u64;
    let mut es = EngineStatus {
        state,
        metadata_received: st.metadata_received,
        files: vec![],
        total_done: st.downloaded.max(0) as u64,
        total,
        down_rate: st.down_rate.max(0) as u64,
        up_rate: st.up_rate.max(0) as u64,
        num_peers: st.num_peers.max(0) as u32,
        num_seeds: st.num_seeds.max(0) as u32,
        error: (state == EngineState::Error).then(|| "bt error".to_string()),
    };
    if total > 0 {
        es.files.push(FileProgress {
            rel_path: String::new(),
            done: st.downloaded.max(0) as u64,
            size: total,
        });
    }
    es
}

/// libtorrent 薄核引擎（单 session）。
/// **落盘语义（v1）**：单 session 全局落盘目录（`BtEngine::new` 的 save_path，serve 配置
/// `[bt] save_path`）。`DownloadTask.dest_root` 仅接受与全局目录一致或默认 `"."`——
/// 显式指定其他目录会返回错误（避免"用户指定 A 目录、实际落 B 目录"的静默错位）。
/// **断点续传（#5）**：remove/pause 显式保存 `.fastresume`；重启后 add 回灌。
pub struct BtEngine {
    core: Arc<BtCore>,
    save_path: PathBuf,
    /// 暂停意图表（engine_tid → 上次采样 done）：lt auto_managed 队列会在
    /// metadata 到达后反复复活用户暂停的任务，单次压制无效——
    /// 改为持续执法：alert 循环周期性对比 done 增长，增长即再压（Bug A，调度层）。
    pause_intents: parking_lot::Mutex<std::collections::HashMap<String, u64>>,
}

impl BtEngine {
    /// 新建 BT 会话（save_path 为全局落盘目录，须已存在）。
    /// `proxy` = 代理 URL（`http://` / `socks5://` / `socks4://`，可带 `user:pass@`；None = 直连）；
    /// `down_kb_s`/`up_kb_s` = 全局下载/上传限速（KiB/s；0 = 不限）。
    /// `enable_dht`/`enable_lsd`/`enable_upnp` = 发现层开关（默认语义全关，M0 确定性；
    /// enable_upnp 同时控制 NAT-PMP——端口映射族）。启动时一次 apply，不参与热重载。
    pub fn new(
        save_path: &Path,
        proxy: Option<&str>,
        down_kb_s: u32,
        up_kb_s: u32,
        enable_dht: bool,
        enable_lsd: bool,
        enable_upnp: bool,
    ) -> Result<Self, String> {
        let core = BtCore::new(save_path, "smart-dl-daemon")
            .map_err(|e| format!("bt session init: {}", core_err(&e)))?;
        // 全量 alert mask：状态推进（bt_events）+ 续传凭据（save_resume_data alert）都需要
        let _ = core.set_alert_mask(0xFFFF);
        // 全局网络策略（代理 + 限速）：启动时一次 apply（代理/限速不参与热重载）
        let proxy_cfg = match proxy {
            Some(u) if !u.is_empty() => match smart_dl_btcore::ffi::parse_proxy(u) {
                Ok(c) => Some(c),
                Err(e) => return Err(format!("bt proxy 解析失败 {u:?}: {e:?}")),
            },
            _ => None,
        };
        core.apply_network(proxy_cfg.as_ref(), down_kb_s, up_kb_s)
            .map_err(|e| format!("bt apply_network: {e:?}"))?;
        // 发现层开关（DHT/LSD/UPnP）：无条件显式调用（默认 false 保持 M0 确定性）
        core.apply_discovery(enable_dht, enable_lsd, enable_upnp)
            .map_err(|e| format!("bt apply_discovery: {e:?}"))?;
        Ok(BtEngine {
            core: Arc::new(core),
            save_path: save_path.to_path_buf(),
            pause_intents: parking_lot::Mutex::new(std::collections::HashMap::new()),
        })
    }

    pub fn core(&self) -> Arc<BtCore> {
        self.core.clone()
    }

    /// 暂停意图登记（true=登记并采样当前 done 基线；false=清除）。
    pub fn set_pause_intent(&self, id: &str, intended: bool) {
        let mut m = self.pause_intents.lock();
        if intended {
            let done = self
                .core
                .status(id)
                .map(|s| s.downloaded.max(0) as u64)
                .unwrap_or(0);
            m.insert(id.to_string(), done);
        } else {
            m.remove(id);
        }
    }

    pub fn pause_intended(&self, id: &str) -> bool {
        self.pause_intents.lock().contains_key(id)
    }

    /// 持续执法：对每个带暂停意图的任务，每轮直接下发 pause。
    /// 这样无论 lt auto_managed 队列 / checking_files 完成态 / 任何内部复活路径，
    /// 只要意图仍在，每 500ms 至少重压一次，真正把"保持暂停"从"检测后反应"
    /// 变成"持续压制"（Bug A 终局修复）。
    pub fn enforce_pauses(&self) {
        let ids: Vec<String> = self.pause_intents.lock().keys().cloned().collect();
        for id in ids {
            let _ = self.core.pause(&id);
            if let Ok(st) = self.core.status(&id) {
                self.pause_intents
                    .lock()
                    .insert(id.clone(), st.downloaded.max(0) as u64);
            }
        }
    }

    /// pop_alerts + 持续执法入口（alert 循环每轮调用）。
    pub fn pop_alerts_enforcing_pause(&self, cap: usize) -> Vec<smart_dl_btcore::Alert> {
        self.enforce_pauses();
        self.core.pop_alerts(cap).unwrap_or_default()
    }

    /// .fastresume 文件路径（按 infohash 命名——避开文件名转义问题，且 magnet 无需
    /// 知道 torrent 名即可定位）。
    fn fastresume_path(&self, ih: &str) -> PathBuf {
        self.save_path.join(format!("{ih}.fastresume"))
    }

    /// 读取已保存的 fastresume 数据（无 → None）。
    fn load_fastresume(&self, ih: &str) -> Option<Vec<u8>> {
        let p = self.fastresume_path(ih);
        p.exists().then(|| std::fs::read(&p).ok()).flatten()
    }

    /// 显式保存 fastresume（#5）：request → 轮询 RESUME·ready alert（≤3s）→ take →
    /// 原子写（tmp+rename）。resume 未就绪（暂无 metadata/超时）→ Ok(None) 不落盘。
    /// 注意：这里同步轮询 pop_alerts，会与 bt_events 消费循环短暂竞态（v1 接受——
    /// remove/pause 为低频操作，窗口 ≤3s；丢失的仅为非终态 alert）。
    fn save_fastresume(&self, ih: &str) -> Result<Option<PathBuf>, EngineError> {
        self.core
            .request_save_resume(ih)
            .map_err(|e| EngineError::Other(core_err(&e)))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut saved: Option<smart_dl_btcore::ResumeBytes> = None;
        let mut seen = 0usize;
        while std::time::Instant::now() < deadline {
            for a in self
                .core
                .pop_alerts(256)
                .map_err(|e| EngineError::Other(core_err(&e)))?
            {
                seen += 1;
                if a.kind == AlertKind::Resume {
                    tracing::debug!("fastresume: RESUME alert ready={}", a.is_resume_ready());
                    if a.is_resume_ready() {
                        if let Ok(r) = self.core.take_resume_data(ih) {
                            saved = Some(r);
                        } else {
                            tracing::warn!("fastresume: take_resume_data 失败（未就绪）");
                        }
                    }
                }
            }
            if saved.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if saved.is_none() {
            tracing::warn!("fastresume: TIMEOUT ih={ih} alerts_seen={seen}");
            return Ok(None);
        }
        tracing::debug!("fastresume: ready ih={ih}");
        let r = saved.expect("saved checked");
        let p = self.fastresume_path(ih);
        let tmp = p.with_extension("fastresume.tmp");
        std::fs::write(&tmp, r.as_bytes())
            .map_err(|e| EngineError::Other(format!("写 fastresume 失败: {e}")))?;
        std::fs::rename(&tmp, &p)
            .map_err(|e| EngineError::Other(format!("落位 fastresume 失败: {e}")))?;
        Ok(Some(p))
    }

    /// 删除 .fastresume（delete_data 时清理）。
    fn remove_fastresume(&self, ih: &str) {
        let _ = std::fs::remove_file(self.fastresume_path(ih));
    }
}

fn core_err(e: &smart_dl_btcore::Error) -> String {
    format!("{:?}", e)
}

/// 从任务 source 提取 infohash hint（fastresume 定位用）：magnet → btih；.torrent → SHA1(info)。
fn btih_hint(task: &DownloadTask) -> Option<String> {
    match &task.source {
        DownloadSource::Magnet(m) => crate::state::btih_of(m),
        DownloadSource::TorrentFile(b) => crate::state::torrent_infohash(b),
        _ => None,
    }
}

#[async_trait::async_trait]
impl DownloadEngine for BtEngine {
    fn id(&self) -> &str {
        "bt"
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
            Capability::PieceRead,
        ]
    }

    async fn add(&self, task: &DownloadTask) -> Result<EngineTaskId, EngineError> {
        // v1 落盘约束：任务级 dest 仅接受默认 "." 或与全局 save_path 一致
        if task.dest_root != Path::new(".") && task.dest_root != self.save_path {
            return Err(EngineError::Other(format!(
                "BT 引擎 v1 全局落盘于 {:?}，任务 dest {:#?} 不支持（请用全局目录或默认）",
                self.save_path, task.dest_root
            )));
        }
        // #5 fastresume 回灌：按输入提取 ih → 查 `.fastresume` → add_torrent_resume
        // （恢复 piece 位图 + metadata，免全盘 checking / 免重新抓取）。
        let ih_hint = btih_hint(task);
        let fastresume = ih_hint.as_deref().and_then(|ih| self.load_fastresume(ih));
        let web_seeds: Vec<String> = vec![];
        let ih = match &task.source {
            DownloadSource::Magnet(m) => match &fastresume {
                Some(data) => self.core.add_torrent_resume(data, &web_seeds),
                None => self.core.add_magnet(m, &web_seeds),
            },
            DownloadSource::TorrentFile(bytes) => match &fastresume {
                Some(data) => self.core.add_torrent_resume(data, &web_seeds),
                None => self.core.add_torrent_file(bytes, &web_seeds),
            },
            _ => return Err(EngineError::Other("source is not bt".to_string())),
        };
        ih.map_err(|e| EngineError::Other(core_err(&e)))
            .map(|ih| ih)
    }

    async fn pause(&self, id: &EngineTaskId) -> Result<(), EngineError> {
        self.set_pause_intent(id, true); // Bug A：登记意图，metadata 复活时由 alert 循环压制
        self.core
            .pause(id)
            .map_err(|e| EngineError::Other(core_err(&e)))?;
        // 暂停时保存进度（best-effort；无 metadata 等场景静默跳过）
        let _ = self.save_fastresume(id);
        Ok(())
    }

    async fn resume(&self, id: &EngineTaskId) -> Result<(), EngineError> {
        self.set_pause_intent(id, false);
        self.core
            .resume(id)
            .map_err(|e| EngineError::Other(core_err(&e)))
    }

    async fn status(&self, id: &EngineTaskId) -> Result<EngineStatus, EngineError> {
        // 会话内未注册的 infohash → NotFound（任务已移除/从未添加）。
        match self.core.status(id) {
            Ok(st) => Ok(map_status(&st)),
            Err(_) => Err(EngineError::NotFound),
        }
    }

    async fn remove(&self, id: &EngineTaskId, delete_data: bool) -> Result<(), EngineError> {
        // 移除前显式保存 fastresume（重启后重新 add 同一 magnet → 回灌续传）。
        // 失败不阻断移除（best-effort）。
        let _ = self.save_fastresume(id);
        self.set_pause_intent(id, false);
        let r = self.core.remove(id, delete_data);
        let _ = r.map_err(|e| EngineError::Other(core_err(&e)))?;
        // 数据删除 → 续传凭据一并清理
        if delete_data {
            self.remove_fastresume(id);
        }
        Ok(())
    }

    async fn peers(&self, id: &EngineTaskId) -> Result<Vec<PeerInfo>, EngineError> {
        self.core
            .peers(id)
            .map(|ps| {
                ps.into_iter()
                    .map(|p| PeerInfo {
                        ip: p.ip,
                        port: p.port,
                        peer_id: p.peer_id,
                        client: p.client,
                        progress_ppm: p.progress_ppm,
                        down_rate: p.down_rate.max(0) as u64,
                        up_rate: p.up_rate.max(0) as u64,
                        total_download: p.total_download.max(0) as u64,
                        total_upload: p.total_upload.max(0) as u64,
                        last_active_sec: p.last_active_sec.max(0) as u64,
                        flags: format!("{:08x}", p.flags),
                    })
                    .collect()
            })
            .map_err(|e| EngineError::Other(core_err(&e)))
    }

    async fn update_sources(
        &self,
        _id: &EngineTaskId,
        _urls: Vec<String>,
    ) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }

    async fn add_url_seed(&self, id: &EngineTaskId, url: &str) -> Result<(), EngineError> {
        self.core
            .add_url_seed(id, url)
            .map_err(|e| EngineError::Other(core_err(&e)))
    }

    async fn add_peer(
        &self,
        id: &EngineTaskId,
        peer: std::net::SocketAddr,
    ) -> Result<(), EngineError> {
        self.core
            .add_peer(id, &peer.ip().to_string(), peer.port())
            .map_err(|e| EngineError::Other(core_err(&e)))
    }

    async fn ban_peer(&self, _id: &EngineTaskId, _peer: SocketAddr) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }

    async fn read_piece(&self, id: &EngineTaskId, idx: u32) -> Result<Vec<u8>, EngineError> {
        self.core
            .read_piece(id, idx as i32)
            .map(|o| o.unwrap_or_default())
            .map_err(|e| EngineError::Other(core_err(&e)))
    }

    async fn add_xunlei_resume(&self, data: Vec<u8>) -> Result<EngineTaskId, EngineError> {
        self.core
            .add_torrent_resume(&data, &[])
            .map_err(|e| EngineError::Other(core_err(&e)))
    }
}
