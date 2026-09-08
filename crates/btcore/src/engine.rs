//! M1 安全门面：`BtCore`（FFI 全量的 safe API；unsafe 只在 ffi 层）。
//! 接口契约：`btcore::{BtCore, TorrentStatus, PeerInfo, Alert, ResumeBytes}`。

use std::path::Path;

use crate::alerts::Alert;
use crate::ffi::{self, lt_peer, lt_torrent_status, Session};
use crate::resume::ResumeBytes;

/// torrent 整体状态（state 语义对齐 lt.h：0 下载 1 完成 3 错误 4 元数据获取中；
/// paused 由 lt_torrent_status::paused 提供，替代纯 alert 轮询作为暂停同步点）
#[derive(Debug, Clone, PartialEq)]
pub struct TorrentStatus {
    pub state: i32,
    pub progress: f32,
    pub downloaded: i64,
    pub total: i64,
    pub down_rate: i64,
    pub up_rate: i64,
    pub num_peers: i32,
    pub num_seeds: i32,
    pub metadata_received: bool,
    pub paused: bool,
}

impl From<lt_torrent_status> for TorrentStatus {
    fn from(st: lt_torrent_status) -> Self {
        TorrentStatus {
            state: st.state,
            progress: st.progress,
            downloaded: st.downloaded,
            total: st.total,
            down_rate: st.down_rate,
            up_rate: st.up_rate,
            num_peers: st.num_peers,
            num_seeds: st.num_seeds,
            metadata_received: st.metadata_received != 0,
            paused: st.paused != 0,
        }
    }
}

/// peer 能力标志位（对应 LT_PEER_*）
pub mod peer_flags {
    pub const SEED: u32 = 1 << 0;
    pub const UPLOADER: u32 = 1 << 1;
    pub const INTERESTED: u32 = 1 << 2;
    pub const CHOKED: u32 = 1 << 3;
    pub const REMOTE_CHOKED: u32 = 1 << 4;
    pub const SNUBBED: u32 = 1 << 5;
    pub const CONNECTING: u32 = 1 << 6;
    pub const LOCAL: u32 = 1 << 7;
    pub const UTP: u32 = 1 << 8;
}

/// 一个已连接 peer 的富信息
#[derive(Debug, Clone, PartialEq)]
pub struct PeerInfo {
    pub ip: String,
    pub port: u16,
    pub peer_id: String,
    pub client: String,
    pub progress_ppm: u32,
    pub down_rate: i64,
    pub up_rate: i64,
    pub total_download: i64,
    pub total_upload: i64,
    pub last_active_sec: i64,
    pub flags: u32,
}

impl PeerInfo {
    pub fn is_seed(&self) -> bool {
        self.flags & peer_flags::SEED != 0
    }
    pub fn is_utp(&self) -> bool {
        self.flags & peer_flags::UTP != 0
    }
}

impl From<lt_peer> for PeerInfo {
    fn from(p: lt_peer) -> Self {
        PeerInfo {
            ip: field_str(&p.ip),
            port: p.port,
            peer_id: field_str(&p.peer_id),
            client: field_str(&p.client),
            progress_ppm: p.progress_ppm,
            down_rate: p.down_rate,
            up_rate: p.up_rate,
            total_download: p.total_download,
            total_upload: p.total_upload,
            last_active_sec: p.last_active_sec,
            flags: p.flags,
        }
    }
}

fn field_str<const N: usize>(arr: &[std::os::raw::c_char; N]) -> String {
    let bytes: Vec<u8> = arr
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// libtorrent 会话的 safe 门面（单 session)。Drop 复用 ffi::Session。
pub struct BtCore {
    sess: Session,
}

impl BtCore {
    pub fn new(save_path: &Path, session_id: &str) -> ffi::Result<Self> {
        Ok(BtCore {
            sess: Session::new(save_path, session_id)?,
        })
    }

    pub fn err_str(&self) -> String {
        self.sess.err_str().unwrap_or_else(|_| "?".into())
    }

    /// 全局网络策略：代理（可选） + 下载/上传限速（KiB/s）。见 `crate::ffi::parse_proxy`。
    pub fn apply_network(
        &self,
        proxy: Option<&crate::ffi::ProxyCfg>,
        down_kb_s: u32,
        up_kb_s: u32,
    ) -> ffi::Result<()> {
        self.sess.apply_network(proxy, down_kb_s, up_kb_s)
    }

    /// 发现层开关：DHT / LSD / UPnP（enable_upnp 同时控制 NAT-PMP——端口映射族）。
    /// 会话默认全关（M0 确定性语义）；本方法显式覆盖。
    pub fn apply_discovery(
        &self,
        enable_dht: bool,
        enable_lsd: bool,
        enable_upnp: bool,
    ) -> ffi::Result<()> {
        self.sess
            .apply_discovery(enable_dht, enable_lsd, enable_upnp)
    }

    // —— 添加 / 移除 ——

    pub fn add_magnet(&self, magnet: &str, web_seeds: &[String]) -> ffi::Result<String> {
        self.sess.add_magnet(magnet, web_seeds)
    }

    /// 本地 seeder 直连注入（测试/评估用，无需 tracker）
    pub fn add_peer(&self, ih: &str, ip: &str, port: u16) -> ffi::Result<()> {
        self.sess.add_peer(ih, ip, port)
    }

    pub fn add_torrent_file(&self, meta: &[u8], web_seeds: &[String]) -> ffi::Result<String> {
        self.sess.add_torrent_file(meta, web_seeds)
    }

    pub fn add_torrent_resume(&self, data: &[u8], web_seeds: &[String]) -> ffi::Result<String> {
        self.sess.add_torrent_resume(data, web_seeds)
    }

    /// 迅雷任务导入（M9）：接受 xunlei-convert 生成的 fastresume bencode，
    /// 语义上等价于 `add_torrent_resume`，但对外表达"从迅雷半成品恢复"的意图。
    pub fn add_xunlei_resume(&self, data: Vec<u8>) -> ffi::Result<String> {
        self.sess.add_torrent_resume(&data, &[])
    }

    pub fn pause(&self, ih: &str) -> ffi::Result<()> {
        self.sess.pause(ih)
    }

    pub fn resume(&self, ih: &str) -> ffi::Result<()> {
        self.sess.resume(ih)
    }

    pub fn remove(&self, ih: &str, delete_data: bool) -> ffi::Result<()> {
        self.sess.remove(ih, delete_data)
    }

    // —— 状态 / 进度 ——

    pub fn status(&self, ih: &str) -> ffi::Result<TorrentStatus> {
        Ok(TorrentStatus::from(self.sess.status(ih)?))
    }

    pub fn piece_count(&self, ih: &str) -> ffi::Result<i32> {
        self.sess.piece_count(ih)
    }

    pub fn bitfield(&self, ih: &str) -> ffi::Result<Vec<u8>> {
        self.sess.bitfield(ih)
    }

    pub fn file_count(&self, ih: &str) -> ffi::Result<i32> {
        self.sess.file_count(ih)
    }

    /// (已下载, 总大小) 每文件
    pub fn file_progress(&self, ih: &str) -> ffi::Result<Vec<(i64, i64)>> {
        self.sess.file_progress(ih)
    }

    // —— 富 peer ——

    pub fn peers(&self, ih: &str) -> ffi::Result<Vec<PeerInfo>> {
        Ok(self
            .sess
            .peers(ih)?
            .into_iter()
            .map(PeerInfo::from)
            .collect())
    }

    // —— alert ——

    pub fn set_alert_mask(&self, mask: u32) -> ffi::Result<()> {
        self.sess.set_alert_mask(mask)
    }

    pub fn pop_alerts(&self, cap: usize) -> ffi::Result<Vec<Alert>> {
        Ok(self.sess.pop_alerts(cap)?.iter().map(Alert::from).collect())
    }

    pub fn alerts_dropped(&self) -> ffi::Result<u32> {
        self.sess.alerts_dropped()
    }

    // —— resume 异步流（D16） ——

    pub fn request_save_resume(&self, ih: &str) -> ffi::Result<()> {
        self.sess.request_save_resume(ih)
    }

    pub fn take_resume_data(&self, ih: &str) -> ffi::Result<ResumeBytes> {
        Ok(ResumeBytes::from(self.sess.take_resume_data(ih)?))
    }

    // —— 控制 / 限制 ——

    pub fn add_url_seed(&self, ih: &str, url: &str) -> ffi::Result<()> {
        self.sess.add_url_seed(ih, url)
    }

    pub fn add_tracker(&self, ih: &str, url: &str) -> ffi::Result<()> {
        self.sess.add_tracker(ih, url)
    }

    pub fn set_sequential(&self, ih: &str, on: bool) -> ffi::Result<()> {
        self.sess.set_sequential(ih, on)
    }

    pub fn set_limits(&self, ih: &str, down: i64, up: i64) -> ffi::Result<()> {
        self.sess.set_limits(ih, down, up)
    }

    // —— 块读取（v2 轮询） ——

    pub fn read_piece(&self, ih: &str, idx: i32) -> ffi::Result<Option<Vec<u8>>> {
        self.sess.read_piece(ih, idx)
    }

    // —— 元数据导出（B-1：magnet → .torrent） ——

    /// 已收 metadata 的任务 → 标准 .torrent bencode；
    /// 未就绪 → Ok(None)（magnet 场景调用方先轮询 status.metadata_received）。
    pub fn metadata(&self, ih: &str) -> ffi::Result<Option<Vec<u8>>> {
        self.sess.metadata(ih)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_discovery_smoke_roundtrip() {
        // FFI 全链路冒烟：真实创建 session 后两态切换均应 Ok（参数封送 +
        // 内核 apply_settings 不抛异常）。内核 settings 值无读回接口，
        // 真实 DHT 冷启动拉 peer 属手动验证项。
        let dir = std::env::temp_dir();
        let core = BtCore::new(&dir, "test-discovery").expect("session init");
        core.apply_discovery(true, true, true).expect("全开应 Ok");
        core.apply_discovery(false, false, false)
            .expect("全关应 Ok");
    }
}
