//! B-1：magnet → .torrent 元数据抓取（独立临时 session，不触碰任务表）。
//!
//! 定位：任务创建前的「预览/预取」sidecar——拿到 info dict 即收手，
//! 不下载 payload、不进 registry、不写 fastresume。
//!
//! 流程（阻塞式，调用方放 `tokio::task::spawn_blocking`）：
//! 1. `parse_magnet` 校验 + 提取（v1 40 hex；v2-only 显式拒绝）
//! 2. 专用临时 `BtCore` session（与下载 session 隔离；scratch 目录由调用方给）
//! 3. add_magnet（内核语义：paused + 非 auto_managed）→ resume
//! 4. 注入 bootstrap peers / 追加 tracker（best-effort，坏 URL 不阻断）
//! 5. 轮询 `metadata_received`（超时 → [`FetchError::Timeout`]；state=ERROR → 带引擎错误串报错）
//! 6. `metadata()` 导出 .torrent bencode → core `parse_torrent` 摘要
//!    + infohash 交叉校验（引擎 vs 摘要不一致 → 报错）
//! 7. remove(delete_data) 清理 + Drop session

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use smart_dl_core::source_parse::magnet::{parse_magnet, MagnetError};
use smart_dl_core::torrent_meta::{parse_torrent, TorrentMetaError, TorrentSummary};

use crate::engine::BtCore;
use crate::ffi::Error as FfiError;

/// 抓取参数。
#[derive(Debug, Clone)]
pub struct FetchOpts {
    /// 总超时（metadata 仍未到手 → [`FetchError::Timeout`]）。
    pub timeout: Duration,
    /// 追加 tracker（magnet 自带 tr 之外；DHT 冷启动慢时的常规加速手段）。
    pub extra_trackers: Vec<String>,
    /// 已知 peer 引导（本地 seeder / 手动注入；无 tracker 环境的主路径）。
    pub bootstrap_peers: Vec<SocketAddr>,
    /// DHT 开关（公网环境 true；纯内网/直连测试 false 更确定）。
    pub enable_dht: bool,
    /// 轮询间隔。
    pub poll_interval: Duration,
}

impl Default for FetchOpts {
    fn default() -> Self {
        FetchOpts {
            timeout: Duration::from_secs(60),
            extra_trackers: vec![],
            bootstrap_peers: vec![],
            enable_dht: true,
            poll_interval: Duration::from_millis(500),
        }
    }
}

/// 抓取产物：bencode 字节 + 解析好的摘要。
#[derive(Debug, Clone)]
pub struct FetchedTorrent {
    /// 引擎侧 infohash（40 hex，与 `summary.infohash_v1` 已交叉校验一致）。
    pub infohash: String,
    pub summary: TorrentSummary,
    /// 标准 .torrent bencode（可直接落盘为 .torrent）。
    pub torrent: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error(transparent)]
    Magnet(#[from] MagnetError),
    #[error("bencode 摘要解析失败: {0}")]
    Summary(#[from] TorrentMetaError),
    #[error("engine: {0:?}")]
    Ffi(#[from] FfiError),
    #[error("metadata 抓取超时（{timeout:?}）")]
    Timeout { timeout: Duration },
    #[error("{0}")]
    Other(String),
}

/// magnet → .torrent 元数据。阻塞式；scratch 目录须已存在（临时 session 的
/// save_path；抓取中途可能写入少量部分文件，结束时按 delete_data 清理）。
pub fn fetch_metadata(
    magnet: &str,
    scratch_dir: &Path,
    opts: &FetchOpts,
) -> Result<FetchedTorrent, FetchError> {
    // magnet 自带 tr/ws 由引擎 parse_magnet_uri 处理；Rust 侧解析仅用于校验
    // （非法输入在此拒绝，不建 session）与参数面（避免重复注入 web seed）。
    parse_magnet(magnet)?;
    let core = BtCore::new(scratch_dir, "magnet-fetch")
        .map_err(|e| FetchError::Other(format!("临时 session 初始化失败: {e:?}")))?;
    // 会话默认发现层全关（M0 确定性语义）；此处按 opts 显式打开 DHT。
    core.apply_discovery(opts.enable_dht, false, false)
        .map_err(|e| FetchError::Other(format!("discovery 设置失败: {e:?}")))?;
    // alert 全开（诊断友好；抓取循环只看 status，alert 随 session Drop 丢弃）
    let _ = core.set_alert_mask(0xFFFF);
    let ih = core.add_magnet(magnet, &[])?;
    // 内核语义（Bug A 修复）：add 即 paused + 非 auto_managed → 必须 resume
    // 才会 announce / 连 peer 抓 metadata。
    core.resume(&ih)?;
    for tr in &opts.extra_trackers {
        // best-effort：单个坏 tracker 不阻断整体抓取
        let _ = core.add_tracker(&ih, tr);
    }
    for p in &opts.bootstrap_peers {
        let _ = core.add_peer(&ih, &p.ip().to_string(), p.port());
    }

    let deadline = Instant::now() + opts.timeout;
    let bytes = loop {
        let st = core.status(&ih)?;
        if st.metadata_received {
            break core
                .metadata(&ih)?
                .ok_or_else(|| FetchError::Other("metadata_received 但导出为空（引擎状态竞态）".into()))?;
        }
        if st.state == 3 {
            // state==ERROR：附 err_str 便于排障
            return Err(FetchError::Other(format!("引擎报错: {}", core.err_str())));
        }
        if Instant::now() >= deadline {
            return Err(FetchError::Timeout {
                timeout: opts.timeout,
            });
        }
        std::thread::sleep(opts.poll_interval);
    };

    // 元数据到手即清理任务与部分数据（临时 session 随后 Drop）
    let _ = core.remove(&ih, true);

    let summary = parse_torrent(&bytes)?;
    if summary.infohash_v1 != ih {
        return Err(FetchError::Other(format!(
            "infohash 交叉校验失败: 引擎 {ih} ≠ 摘要 {}（混合 hash 算法 torrent？）",
            summary.infohash_v1
        )));
    }
    Ok(FetchedTorrent {
        infohash: ih,
        summary,
        torrent: bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FFI 链接环境（Windows 门禁 / 本地 LT 构建）才能跑的烟囱测试：
    /// 坏 magnet 应在 session 初始化之前就被 parse 拒绝（不真正建 session）。
    #[test]
    fn invalid_magnet_rejected_before_session() {
        let dir = std::env::temp_dir().join("smart-dl-magnet-invalid-test");
        let _ = std::fs::create_dir_all(&dir);
        let err = fetch_metadata("magnet:?dn=no-hash", &dir, &FetchOpts::default()).unwrap_err();
        assert!(matches!(err, FetchError::Magnet(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn v2_only_rejected_before_session() {
        let dir = std::env::temp_dir().join("smart-dl-magnet-v2-test");
        let _ = std::fs::create_dir_all(&dir);
        let uri = "magnet:?xt=urn:btmh:1220abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let err = fetch_metadata(uri, &dir, &FetchOpts::default()).unwrap_err();
        assert!(matches!(
            err,
            FetchError::Magnet(MagnetError::UnsupportedV2)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
