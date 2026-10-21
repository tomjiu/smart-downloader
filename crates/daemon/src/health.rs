//! 生态健康 v1（§11）：富 peer 反吸血（qBittorrent-EE 黑名单规则，不 ban）+ 上传/下载比。

use serde::{Deserialize, Serialize};
use smart_dl_core::types::PeerInfo;
use ts_rs::TS;

/// 健康事件类型。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum HealthEventKind {
    /// 反吸血黑名单命中（peer_id 前缀 / client 标识），仅记录不 ban。
    LeechDetected,
    /// 累计上传/下载比 <0.5，仅统计告警。
    RatioLow,
}

/// 黑名单前缀（qBittorrent-EE：-XL / XL / -SD / -BN / -DT）。
const LEECH_PREFIXES: [&str; 5] = ["-XL", "XL", "-SD", "-BN", "-DT"];

/// 反吸血判定：peer_id 前缀或 client 标识命中 → Some(原因)；否则 None。
pub fn leech_reason(peer: &PeerInfo) -> Option<String> {
    if LEECH_PREFIXES.iter().any(|p| peer.peer_id.starts_with(p)) {
        return Some(format!("peer_id '{}' matches blacklist", peer.peer_id));
    }
    let client = peer.client.to_lowercase();
    if client.contains("xunlei") || client.contains("thunder") {
        return Some(format!("client '{}' is a known leech", peer.client));
    }
    None
}

/// detect_leech：命中 → LeechDetected。
pub fn detect_leech(peer: &PeerInfo) -> Option<HealthEventKind> {
    leech_reason(peer).map(|_| HealthEventKind::LeechDetected)
}

/// 上传/下载比 <0.5（累计字节；无下载量不算告警）。
pub fn ratio_low(total_upload: u64, total_download: u64) -> bool {
    if total_download == 0 {
        return false;
    }
    (total_upload as f64) / (total_download as f64) < 0.5
}

/// 累计 ratio 检查 → RatioLow（仅统计告警，不阻断）。
pub fn check_ratio(total_upload: u64, total_download: u64) -> Option<HealthEventKind> {
    if ratio_low(total_upload, total_download) {
        Some(HealthEventKind::RatioLow)
    } else {
        None
    }
}
