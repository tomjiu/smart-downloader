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

use smart_dl_btcore::{BtCore, TorrentStatus};
use smart_dl_core::task::DownloadTask;
use smart_dl_core::types::{
    BtSessionPatch, Capability, DownloadEngine, DownloadSource, EngineError, EngineKind,
    EngineState, EngineStatus, EngineTaskId, FileProgress, PeerInfo, TrackerEntry,
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
        // E33：全生命周期累计上/下行（BT all_time_* 口径，暂停不清零）
        total_downloaded: st.all_time_download.max(0) as u64,
        total_uploaded: st.all_time_upload.max(0) as u64,
        error: (state == EngineState::Error).then(|| "bt error".to_string()),
        // E28：BT 任务名回填接入 —— torrent metadata name 经 FFI status 透出，
        // daemon E9 轮询幂等回填（同 CD 链口径：一次成功后 name 非 None 自然停）
        name: st.name.clone(),
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
    /// 会话级网络策略当前值（E16）：set_global_limits 热改全局限速时需整包
    /// re-apply（libtorrent settings_pack 全量语义）——保存启动时代理与双
    /// 方向速率，热改限速时代理原样重放（BT 代理属会话级，E8 边界不变）。
    network: parking_lot::Mutex<BtNetwork>,
    /// 会话级发现/传输/连接设置快照（S1）：apply_bt_session 部分补丁合并
    /// 基准（apply_discovery/apply_transport 全量签名需要四/双值）。
    session: parking_lot::Mutex<BtSessionCfg>,
    /// batch3-P1：RESUME alert 分发注册表（ih → 等待者）。alert ring 单消费者化：
    /// save_fastresume 不再自行 pop_alerts（旧实现整环消费吞掉其他任务终态
    /// alert，且与 bt_events 循环互吞 RESUME → fastresume 超时/任务卡态），
    /// 改为注册等待 + 由 bt_events 唯一消费者经 dispatch 分发。
    resume_waiters: Arc<
        parking_lot::Mutex<
            std::collections::HashMap<
                String,
                Vec<tokio::sync::mpsc::UnboundedSender<smart_dl_btcore::Alert>>,
            >,
        >,
    >,
    /// batch3-P1：alert ring 是否已有常驻消费者（bt_events spawn_alert_loop）。
    /// true → save_fastresume 纯等分发；false（测试/无循环装配）→ 自行 pop 兜底。
    alert_loop_active: Arc<std::sync::atomic::AtomicBool>,
}

/// BtEngine 会话级网络策略快照（E16）。`proxy_url` 保存原始 URL 串（None =
/// 直连），re-apply 时重新 parse（避免依赖 btcore FFI 结构 Clone）。
#[derive(Debug, Clone)]
struct BtNetwork {
    proxy_url: Option<String>,
    down_kb_s: u32,
    up_kb_s: u32,
}

/// BtEngine 会话级设置快照（S1）：发现/传输/连接三组当前值。
/// `listen_port`/`max_connections` = 0 语义与内核一致（不下发）。
#[derive(Debug, Clone)]
struct BtSessionCfg {
    enable_dht: bool,
    enable_lsd: bool,
    enable_upnp: bool,
    enable_pex: bool,
    enable_utp: bool,
    encrypt: String,
    listen_port: u16,
    max_connections: u32,
    /// 新建任务自动追加 tracker（qbit 对标）；add() 时读取。
    extra_trackers: Vec<String>,
    /// 做种分享率上限（qbit Share Ratio Limit 对标）；0.0 = 不启用。
    max_share_ratio: f64,
    /// 做种时长上限（分钟，qbit「做种时间限制」对标）；0 = 不启用。
    max_seeding_time_min: u32,
}

impl BtEngine {
    /// 新建 BT 会话（save_path 为全局落盘目录，须已存在）。
    /// `proxy` = 代理 URL（`http://` / `socks5://` / `socks4://`，可带 `user:pass@`；None = 直连）；
    /// `down_kb_s`/`up_kb_s` = 全局下载/上传限速（KiB/s；0 = 不限）。
    /// `enable_dht`/`enable_lsd`/`enable_upnp`/`enable_pex` = 发现层开关（默认语义全关，
    /// M0 确定性；enable_upnp 同时控制 NAT-PMP——端口映射族）。启动时一次 apply，不参与热重载。
    /// `enable_utp` = uTP 双向开关（incoming/outgoing 同进退）；`encrypt` = MSE 加密
    /// 策略字符串（disable/allow/require，非法值报错）。启动时一次 apply。
    /// `extra_trackers` = 新建任务自动追加 tracker（qbit 对标，可空）；
    /// `max_share_ratio` = 做种分享率上限（0 = 不启用）；
    /// `max_seeding_time_min` = 做种时长上限分钟（0 = 不启用）。运行中均可热改。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        save_path: &Path,
        proxy: Option<&str>,
        down_kb_s: u32,
        up_kb_s: u32,
        enable_dht: bool,
        enable_lsd: bool,
        enable_upnp: bool,
        enable_pex: bool,
        enable_utp: bool,
        encrypt: &str,
        extra_trackers: &[String],
        max_share_ratio: f64,
        max_seeding_time_min: u32,
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
        // 发现层开关（DHT/LSD/UPnP/PEX）：无条件显式调用（默认 false 保持 M0 确定性）
        core.apply_discovery(enable_dht, enable_lsd, enable_upnp, enable_pex)
            .map_err(|e| format!("bt apply_discovery: {e:?}"))?;
        // 传输层开关（uTP + MSE 加密）：启动时一次 apply，不参与热重载
        let enc_policy =
            smart_dl_btcore::engine::parse_encrypt_policy(encrypt).ok_or_else(|| {
                format!("bt.encrypt 解析失败 {encrypt:?}: 仅支持 disable/allow/require")
            })?;
        core.apply_transport(enable_utp, enc_policy)
            .map_err(|e| format!("bt apply_transport: {e:?}"))?;
        Ok(BtEngine {
            core: Arc::new(core),
            save_path: save_path.to_path_buf(),
            pause_intents: parking_lot::Mutex::new(std::collections::HashMap::new()),
            resume_waiters: Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
            alert_loop_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            network: parking_lot::Mutex::new(BtNetwork {
                proxy_url: proxy.map(|s| s.to_string()).filter(|s| !s.is_empty()),
                down_kb_s,
                up_kb_s,
            }),
            session: parking_lot::Mutex::new(BtSessionCfg {
                enable_dht,
                enable_lsd,
                enable_upnp,
                enable_pex,
                enable_utp,
                encrypt: encrypt.to_string(),
                listen_port: 0,
                max_connections: 0,
                extra_trackers: extra_trackers.to_vec(),
                max_share_ratio,
                max_seeding_time_min,
            }),
        })
    }

    /// 启动期会话连接参数注入（S1）：serve 装配从 `[bt] listen_port /
    /// max_connections` 调用（>0 才下发）；运行中变更走 apply_bt_session。
    pub fn apply_startup_conn(&self, listen_port: u16, max_connections: u32) -> Result<(), String> {
        if listen_port == 0 && max_connections == 0 {
            return Ok(());
        }
        {
            let mut s = self.session.lock();
            if listen_port > 0 {
                s.listen_port = listen_port;
            }
            if max_connections > 0 {
                s.max_connections = max_connections;
            }
        }
        self.core
            .apply_conn(listen_port, max_connections)
            .map_err(|e| format!("bt apply_conn: {e:?}"))
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
        let alerts = self.core.pop_alerts(cap).unwrap_or_default();
        self.dispatch_resume_alerts(&alerts);
        alerts
    }

    /// batch3-P1：把 RESUME alert 分发给注册的等待者（唯一消费者 pop 后回填）。
    /// 无等待者时丢弃（与旧行为一致）；等待者已离开（recv 端关闭）→ 清除。
    fn dispatch_resume_alerts(&self, alerts: &[smart_dl_btcore::Alert]) {
        dispatch_resume_alerts(&self.resume_waiters, alerts);
    }

    /// batch3-P1：spawn_alert_loop 启动时置位（此后 save_fastresume 纯等分发）。
    pub fn mark_alert_loop_active(&self) {
        self.alert_loop_active
            .store(true, std::sync::atomic::Ordering::SeqCst);
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

    /// 显式保存 fastresume（#5）：request → 注册等待者 → bt_events 唯一消费者
    /// 分发 RESUME alert → take → 原子写（tmp+rename）。
    /// batch3-P1：不再自行 pop_alerts——旧实现整环消费（≤3s 窗口 × 每 5 分钟 ×
    /// 每活跃任务）吞掉其他任务 torrent_finished/error 等终态 alert（任务卡
    /// Downloading/漏 Failed），且与 bt_events 循环互吞 RESUME（fastresume 超时）。
    /// alert ring 单消费者化后本函数仅等待分发，其他 alert 全部流向 bt_events。
    /// resume 未就绪（暂无 metadata/超时）→ Ok(None) 不落盘。
    fn save_fastresume(&self, ih: &str) -> Result<Option<PathBuf>, EngineError> {
        save_fastresume_impl(
            &self.core,
            &self.resume_waiters,
            &self.save_path,
            &self.alert_loop_active,
            ih,
        )
    }

    /// 保存指定任务的 fastresume（公开入口：daemon 周期/退出时机保存，P4 G4）。
    /// 返回落盘路径；未就绪（无 metadata）/超时/失败 → None（best-effort）。
    /// 同步轮询 alert（≤3s），调用方应放 spawn_blocking 或退出路径直调。
    pub fn save_resume_now(&self, ih: &str) -> Option<PathBuf> {
        self.save_fastresume(ih).ok().flatten()
    }

    /// 删除 .fastresume（delete_data 时清理）。
    fn remove_fastresume(&self, ih: &str) {
        let _ = std::fs::remove_file(self.fastresume_path(ih));
    }
}

type ResumeWaiters = Arc<
    parking_lot::Mutex<
        std::collections::HashMap<
            String,
            Vec<tokio::sync::mpsc::UnboundedSender<smart_dl_btcore::Alert>>,
        >,
    >,
>;

/// batch3-P1：把 RESUME alert 分发给注册的等待者（唯一消费者 pop 后回填）。
/// 无等待者时丢弃（与旧行为一致）；等待者已离开（recv 端关闭）→ 清除。
fn dispatch_resume_alerts(waiters: &ResumeWaiters, alerts: &[smart_dl_btcore::Alert]) {
    if alerts.is_empty() {
        return;
    }
    let mut waiters = waiters.lock();
    if waiters.is_empty() {
        return;
    }
    for a in alerts {
        if a.kind != smart_dl_btcore::AlertKind::Resume || a.ih.is_empty() {
            continue;
        }
        if let Some(list) = waiters.get_mut(&a.ih) {
            list.retain(|tx| tx.send(a.clone()).is_ok());
        }
    }
    waiters.retain(|_, l| !l.is_empty());
}

/// batch3-P1：fastresume 保存核心（request → 注册等待 → 等待分发 → take →
/// 原子写）。free fn 形态便于 spawn_blocking 捕获可克隆字段（Arc/PathBuf）。
/// 执行语境：调用方（pause/remove 经 spawn_blocking、周期保存、退出路径）
/// 均不在 bt_events pop_alerts 循环线程上——分发为 unbounded send 不阻塞，
/// 无死锁。
fn save_fastresume_impl(
    core: &Arc<BtCore>,
    waiters: &ResumeWaiters,
    save_path: &Path,
    loop_active: &std::sync::atomic::AtomicBool,
    ih: &str,
) -> Result<Option<PathBuf>, EngineError> {
    core.request_save_resume(ih)
        .map_err(|e| EngineError::Other(core_err(&e)))?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    waiters
        .lock()
        .entry(ih.to_string())
        .or_default()
        .push(tx.clone());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut saved: Option<smart_dl_btcore::ResumeBytes> = None;
    // batch3-P1：常驻 alert 消费者存在 → 纯等分发（不自行 pop，双消费者互吞
    // 是本批修的缺陷）；无消费者（单元测试直连 BtEngine）→ 自行 pop 兜底，
    // 与旧行为一致（此时 ring 无其他消费者，无互吞问题）。
    let poll_ring = !loop_active.load(std::sync::atomic::Ordering::SeqCst);
    while std::time::Instant::now() < deadline {
        if poll_ring {
            if let Ok(alerts) = core.pop_alerts(256) {
                for a in alerts {
                    if a.kind == smart_dl_btcore::AlertKind::Resume && a.is_resume_ready() {
                        if let Ok(r) = core.take_resume_data(ih) {
                            saved = Some(r);
                        } else {
                            tracing::warn!("fastresume: take_resume_data 失败（未就绪）");
                        }
                    }
                }
            }
        } else {
            match rx.try_recv() {
                Ok(a) => {
                    tracing::debug!("fastresume: RESUME alert ready={}", a.is_resume_ready());
                    if a.is_resume_ready() {
                        if let Ok(r) = core.take_resume_data(ih) {
                            saved = Some(r);
                        } else {
                            tracing::warn!("fastresume: take_resume_data 失败（未就绪）");
                        }
                    }
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
            }
        }
        if saved.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // 注销等待者（防止表无限增长）
    if let Some(list) = waiters.lock().get_mut(ih) {
        list.retain(|w| !w.same_channel(&tx));
    }
    if saved.is_none() {
        tracing::warn!("fastresume: TIMEOUT ih={ih}");
        return Ok(None);
    }
    tracing::debug!("fastresume: ready ih={ih}");
    let r = saved.expect("saved checked");
    let p = save_path.join(format!("{ih}.fastresume"));
    let tmp = p.with_extension("fastresume.tmp");
    std::fs::write(&tmp, r.as_bytes())
        .map_err(|e| EngineError::Other(format!("写 fastresume 失败: {e}")))?;
    std::fs::rename(&tmp, &p)
        .map_err(|e| EngineError::Other(format!("落位 fastresume 失败: {e}")))?;
    Ok(Some(p))
}

fn core_err(e: &smart_dl_btcore::Error) -> String {
    format!("{:?}", e)
}

/// RFC 3986 保守 percent-encode（magnet dn/tr 参数拼装用；unreserved 之外全转义）。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// ffi 错误分类：NotFound（torrent/metadata 缺失）→ EngineError::NotFound，
/// 其余 → Other（供子文件优先级链路区分 404 与「metadata 未就绪」409）。
fn bt_engine_err(e: smart_dl_btcore::Error) -> EngineError {
    match e {
        smart_dl_btcore::Error::NotFound(_) => EngineError::NotFound,
        other => EngineError::Other(core_err(&other)),
    }
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
        let ih = ih.map_err(|e| EngineError::Other(core_err(&e)))?;
        // 内核统一语义（Bug A 修复）：三个 add 入口（magnet / .torrent /
        // fastresume 回灌）均 paused + 非 auto_managed 落库。任务层「添加即
        // 下载」：首次 add 成功即 resume——否则 handle 永久暂停，magnet 元数
        // 据抓取与 .torrent 下载都不启动（恢复重放路径 restore_from 已有对
        // 称 resume；本路径此前缺失，实测 x.pe 本地闭环暴露）。新建任务无
        // 用户暂停意图，resume 与 P4 G5 不冲突；后续用户 pause 走 pause API
        // （intent 标志位在 alert 循环持续压制复活）。
        self.core
            .resume(&ih)
            .map_err(|e| EngineError::Other(core_err(&e)))?;
        // extra_trackers（qbit「自动添加 tracker 到新任务」对标）：add 成功后
        // 逐条注入（best-effort——单条 URL 失败不阻断建任务，任务可经
        // /tasks/:id/trackers 手动补）；恢复重放路径不重复注入（fastresume
        // 已带 tracker 集合）。
        let extra = self.session.lock().extra_trackers.clone();
        for url in extra {
            let _ = self.core.add_tracker(&ih, &url);
        }
        Ok(ih)
    }

    async fn pause(&self, id: &EngineTaskId) -> Result<(), EngineError> {
        self.set_pause_intent(id, true); // Bug A：登记意图，metadata 复活时由 alert 循环压制
        self.core
            .pause(id)
            .map_err(|e| EngineError::Other(core_err(&e)))?;
        // 暂停时保存进度（best-effort；无 metadata 等场景静默跳过）
        // batch3-P2：同步等待 RESUME（≤3s）→ spawn_blocking 不阻塞 tokio worker
        let core = self.core.clone();
        let waiters = self.resume_waiters.clone();
        let save_path = self.save_path.clone();
        let loop_active = self.alert_loop_active.clone();
        let ih = id.clone();
        let _ = tokio::task::spawn_blocking(move || {
            save_fastresume_impl(&core, &waiters, &save_path, &loop_active, &ih)
        })
        .await;
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
        // batch3-P2：同步等待 RESUME（≤3s）→ spawn_blocking 不阻塞 tokio worker
        let core = self.core.clone();
        let waiters = self.resume_waiters.clone();
        let save_path = self.save_path.clone();
        let loop_active = self.alert_loop_active.clone();
        let ih = id.clone();
        let _ = tokio::task::spawn_blocking(move || {
            save_fastresume_impl(&core, &waiters, &save_path, &loop_active, &ih)
        })
        .await;
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

    /// tracker 运行时增删查（E29）：批量追加 / URL 精确删除（无匹配
    /// NotFound 定性）/ 列举当前 announce 表。
    async fn add_trackers(&self, id: &EngineTaskId, urls: &[String]) -> Result<(), EngineError> {
        for url in urls {
            self.core.add_tracker(id, url).map_err(bt_engine_err)?;
        }
        Ok(())
    }

    async fn remove_tracker(&self, id: &EngineTaskId, url: &str) -> Result<(), EngineError> {
        self.core.remove_tracker(id, url).map_err(bt_engine_err)
    }

    async fn list_trackers(&self, id: &EngineTaskId) -> Result<Vec<TrackerEntry>, EngineError> {
        self.core.list_trackers(id).map_err(bt_engine_err)
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

    /// 任务上下文封禁 peer（Task 46 真实现）：session 级 ip_filter（libtorrent
    /// 2.x 无 per-endpoint ban；作用域 = 全 session，与 qbit「永久封禁」一致）。
    /// id 仅用于任务存在性验证（NotFound 语义）。
    async fn ban_peer(&self, id: &EngineTaskId, peer: SocketAddr) -> Result<(), EngineError> {
        self.core
            .ban_ip(Some(id), &peer.ip().to_string())
            .map_err(bt_engine_err)
    }

    async fn read_piece(&self, id: &EngineTaskId, idx: u32) -> Result<Vec<u8>, EngineError> {
        self.core
            .read_piece(id, idx as i32)
            .map(|o| o.unwrap_or_default())
            .map_err(|e| EngineError::Other(core_err(&e)))
    }

    /// BT per-torrent 限速（trait 扩展）。`id` = engine_tid = infohash（add 返回
    /// 口径）。引擎语义：None 方向按 0（不限）下发——daemon state 层负责把既有
    /// 配置合并成全量两方向快照后调用，避免部分更新把已设方向清零。
    async fn set_limits(
        &self,
        id: &EngineTaskId,
        down_kb_s: Option<u32>,
        up_kb_s: Option<u32>,
    ) -> Result<(), EngineError> {
        let down = down_kb_s.unwrap_or(0) as i64 * 1024;
        let up = up_kb_s.unwrap_or(0) as i64 * 1024;
        self.core
            .set_limits(id, down, up)
            .map_err(|e| EngineError::Other(core_err(&e)))
    }

    /// 引擎全局限速热改（E16 trait 扩展）：双方向合并进 network 快照后整包
    /// re-apply（settings_pack 全量语义，代理原样重放）。任一方向 None =
    /// 沿用当前值；Some(n)/Some(0) = 上限 n KiB/s / 不限。
    async fn set_global_limits(
        &self,
        down_kb_s: Option<u32>,
        up_kb_s: Option<u32>,
    ) -> Result<(), EngineError> {
        let net = {
            let mut n = self.network.lock();
            if let Some(d) = down_kb_s {
                n.down_kb_s = d;
            }
            if let Some(u) = up_kb_s {
                n.up_kb_s = u;
            }
            n.clone()
        };
        // 启动时代理原样重放（会话级边界不变，E8）；重新 parse 失败 → Other
        //（启动时已校验过，此处兜底）
        let proxy_cfg = match net.proxy_url.as_deref() {
            Some(u) => match smart_dl_btcore::ffi::parse_proxy(u) {
                Ok(c) => Some(c),
                Err(e) => {
                    return Err(EngineError::Other(format!(
                        "bt proxy 解析失败 {u:?}: {e:?}"
                    )))
                }
            },
            None => None,
        };
        self.core
            .apply_network(proxy_cfg.as_ref(), net.down_kb_s, net.up_kb_s)
            .map_err(|e| EngineError::Other(format!("bt apply_network: {e:?}")))
    }

    /// BT 会话级设置热改（S1 trait 扩展）：发现/传输/连接三组按需下发。
    /// 部分补丁先合并进会话快照，再以全量语义重放（apply_discovery /
    /// apply_transport / apply_conn 均为 settings_pack 全量签名）。任一组
    /// 下发失败即返回错误（后续组不再尝试）——调用方 daemon 层降级 warn，
    /// 不阻塞设置保存；快照只记录成功组（失败组保持旧值，重试幂等）。
    async fn apply_bt_session(&self, patch: BtSessionPatch) -> Result<(), EngineError> {
        let snap = self.session.lock().clone();
        let mut merged = snap.clone();
        merged.enable_dht = patch.enable_dht.unwrap_or(snap.enable_dht);
        merged.enable_lsd = patch.enable_lsd.unwrap_or(snap.enable_lsd);
        merged.enable_upnp = patch.enable_upnp.unwrap_or(snap.enable_upnp);
        merged.enable_pex = patch.enable_pex.unwrap_or(snap.enable_pex);
        merged.enable_utp = patch.enable_utp.unwrap_or(snap.enable_utp);
        merged.encrypt = patch
            .encrypt
            .clone()
            .map(|e| e.trim().to_string())
            .unwrap_or_else(|| snap.encrypt.clone());
        merged.listen_port = patch.listen_port.unwrap_or(snap.listen_port);
        merged.max_connections = patch.max_connections.unwrap_or(snap.max_connections);
        // extra_trackers：None = 不调整；Some = 整表替换（trim 后空串过滤）。
        // 仅影响后续新建任务（存量任务的 tracker 走 /tasks/:id/trackers 管理）。
        if let Some(v) = &patch.extra_trackers {
            merged.extra_trackers = v
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        merged.max_share_ratio = patch.max_share_ratio.unwrap_or(snap.max_share_ratio);
        merged.max_seeding_time_min = patch
            .max_seeding_time_min
            .unwrap_or(snap.max_seeding_time_min);

        // 1) 发现层（DHT/LSD/UPnP/PEX）
        let discovery_changed = merged.enable_dht != snap.enable_dht
            || merged.enable_lsd != snap.enable_lsd
            || merged.enable_upnp != snap.enable_upnp
            || merged.enable_pex != snap.enable_pex;
        if discovery_changed {
            self.core
                .apply_discovery(
                    merged.enable_dht,
                    merged.enable_lsd,
                    merged.enable_upnp,
                    merged.enable_pex,
                )
                .map_err(|e| EngineError::Other(format!("bt apply_discovery: {e:?}")))?;
        }
        // 2) 传输层（uTP + MSE 加密）：加密值校验后解析
        let transport_changed =
            merged.enable_utp != snap.enable_utp || merged.encrypt != snap.encrypt;
        if transport_changed {
            let enc_policy = smart_dl_btcore::engine::parse_encrypt_policy(&merged.encrypt)
                .ok_or_else(|| {
                    EngineError::Other(format!(
                        "bt.encrypt 解析失败 {:?}: 仅支持 disable/allow/require",
                        merged.encrypt
                    ))
                })?;
            self.core
                .apply_transport(merged.enable_utp, enc_policy)
                .map_err(|e| EngineError::Other(format!("bt apply_transport: {e:?}")))?;
        }
        // 3) 连接参数（监听端口 / 全局连接数上限；0 = 该项不下发）
        let conn_changed = merged.listen_port != snap.listen_port
            || merged.max_connections != snap.max_connections;
        if conn_changed {
            self.core
                .apply_conn(merged.listen_port, merged.max_connections)
                .map_err(|e| EngineError::Other(format!("bt apply_conn: {e:?}")))?;
        }
        // 全部成功才提交快照（失败组保持旧值，重试幂等）
        *self.session.lock() = merged;
        Ok(())
    }

    /// 引擎级全局代理热改（S1 trait 扩展）：更新 network 快照后整包 re-apply
    /// （settings_pack 全量语义，限速原样重放）；None = 清除代理（直连）。
    async fn set_global_proxy(&self, proxy: Option<&str>) -> Result<(), EngineError> {
        let net = {
            let mut n = self.network.lock();
            n.proxy_url = proxy
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            n.clone()
        };
        let proxy_cfg = match net.proxy_url.as_deref() {
            Some(u) => match smart_dl_btcore::ffi::parse_proxy(u) {
                Ok(c) => Some(c),
                Err(e) => {
                    return Err(EngineError::Other(format!(
                        "bt proxy 解析失败 {u:?}: {e:?}"
                    )))
                }
            },
            None => None,
        };
        self.core
            .apply_network(proxy_cfg.as_ref(), net.down_kb_s, net.up_kb_s)
            .map_err(|e| EngineError::Other(format!("bt apply_network: {e:?}")))
    }

    /// BT 子文件优先级批量设置（trait 扩展；需 metadata 就绪）。
    /// NotFound（torrent/metadata 缺失）按 `EngineError::NotFound` 透传，
    /// 供 state 层区分「任务不存在」与「metadata 未就绪」。
    async fn set_file_priorities(
        &self,
        id: &EngineTaskId,
        priorities: &[(usize, u32)],
    ) -> Result<(), EngineError> {
        let prio: Vec<(i32, i32)> = priorities
            .iter()
            .map(|(idx, p)| (*idx as i32, *p as i32))
            .collect();
        self.core
            .set_file_priorities(id, &prio)
            .map_err(bt_engine_err)
    }

    /// 读取当前各文件优先级（下标即文件序；需 metadata 就绪）。
    async fn file_priorities(&self, id: &EngineTaskId) -> Result<Vec<Option<u32>>, EngineError> {
        let prios = self.core.file_priorities(id).map_err(bt_engine_err)?;
        Ok(prios.into_iter().map(|p| Some(p as u32)).collect())
    }

    /// 任务级顺序下载（trait 扩展）：libtorrent sequential_download flag。
    /// 2.0.x = torrent_flags（on/off 均可）；2.1 = set_sequential_range（仅 on）。
    /// metadata 未就绪也可设（handle 级 flag，随 metadata 到达持续生效）。
    async fn set_sequential(&self, id: &EngineTaskId, on: bool) -> Result<(), EngineError> {
        self.core.set_sequential(id, on).map_err(bt_engine_err)
    }

    /// 任务级连接数上限（S1-c）：>0 = 上限；0 = 复位会话级默认。
    /// metadata 未就绪也可设（handle 级参数，随任务存续生效）。
    async fn set_max_connections(&self, id: &EngineTaskId, n: u32) -> Result<(), EngineError> {
        self.core.set_max_connections(id, n).map_err(bt_engine_err)
    }

    async fn set_super_seeding(&self, id: &EngineTaskId, on: bool) -> Result<(), EngineError> {
        self.core.set_super_seeding(id, on).map_err(bt_engine_err)
    }

    /// 强制向全部 tracker 立即宣告（Task 46，qbit/BitComet 任务右键对标）。
    async fn force_reannounce(&self, id: &EngineTaskId) -> Result<(), EngineError> {
        self.core.force_reannounce(id).map_err(bt_engine_err)
    }

    /// 强制 DHT 宣告（DHT 未启用时内核 no-op 不报错）。
    async fn force_dht_announce(&self, id: &EngineTaskId) -> Result<(), EngineError> {
        self.core.force_dht_announce(id).map_err(bt_engine_err)
    }

    /// 强制重新校验（任务转入 checking；校验期下载/做种挂起）。
    async fn force_recheck(&self, id: &EngineTaskId) -> Result<(), EngineError> {
        self.core.force_recheck(id).map_err(bt_engine_err)
    }

    /// 导出 .torrent（Task 46）：metainfo bencode；magnet 任务 metadata
    /// 未就绪 → Other（调用方 409）。
    async fn export_torrent(&self, id: &EngineTaskId) -> Result<Vec<u8>, EngineError> {
        match self.core.metadata(id).map_err(bt_engine_err)? {
            Some(bytes) if !bytes.is_empty() => Ok(bytes),
            _ => Err(EngineError::Other(
                "元数据未就绪（magnet 任务需先收到 metadata）".into(),
            )),
        }
    }

    /// 生成 magnet URI（Task 46）：btih + dn（status.name）+ 全量 tracker。
    async fn magnet_uri(&self, id: &EngineTaskId) -> Result<String, EngineError> {
        let st = self.core.status(id).map_err(bt_engine_err)?;
        let trackers = self.core.list_trackers(id).map_err(bt_engine_err)?;
        let mut uri = format!("magnet:?xt=urn:btih:{}", id);
        if let Some(name) = st.name.as_deref().filter(|n| !n.is_empty()) {
            uri.push_str("&dn=");
            uri.push_str(&percent_encode(name));
        }
        for t in &trackers {
            if t.url.is_empty() {
                continue;
            }
            uri.push_str("&tr=");
            uri.push_str(&percent_encode(&t.url));
        }
        Ok(uri)
    }

    /// Session 级 IP 封禁（Task 46；幂等）。
    async fn ban_ip(&self, ip: &str) -> Result<(), EngineError> {
        self.core.ban_ip(None, ip).map_err(bt_engine_err)
    }

    /// 解除 Session 级 IP 封禁（Task 46；幂等）。
    async fn unban_ip(&self, ip: &str) -> Result<(), EngineError> {
        self.core.unban_ip(ip).map_err(bt_engine_err)
    }

    fn seeding_ratio_limit(&self) -> Option<f64> {
        let r = self.session.lock().max_share_ratio;
        (r > 0.0).then_some(r)
    }

    fn seeding_time_limit(&self) -> Option<u32> {
        let m = self.session.lock().max_seeding_time_min;
        (m > 0).then_some(m)
    }

    async fn add_xunlei_resume(&self, data: Vec<u8>) -> Result<EngineTaskId, EngineError> {
        self.core
            .add_torrent_resume(&data, &[])
            .map_err(|e| EngineError::Other(core_err(&e)))
    }
}
