//! alert 扁平化 → Rust 结构（D31 预算 ≤12 种；Rust 持有值拷贝，pop 后内核缓冲不可用）。

use std::os::raw::c_char;

use crate::ffi::{
    lt_alert, lt_alert_mask_LT_ALERT_ERROR, lt_alert_mask_LT_ALERT_METADATA,
    lt_alert_mask_LT_ALERT_PEER, lt_alert_mask_LT_ALERT_PIECE, lt_alert_mask_LT_ALERT_RESUME,
    lt_alert_mask_LT_ALERT_STATE, lt_alert_mask_LT_ALERT_TRACKER,
};

/// 扁平化 kind（对应 lt_alert_mask 位）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Tracker,
    Peer,
    Error,
    Metadata,
    State,
    Resume,
    Piece,
    Other(i32),
}

impl From<i32> for AlertKind {
    fn from(kind: i32) -> Self {
        match kind {
            lt_alert_mask_LT_ALERT_TRACKER => AlertKind::Tracker,
            lt_alert_mask_LT_ALERT_PEER => AlertKind::Peer,
            lt_alert_mask_LT_ALERT_ERROR => AlertKind::Error,
            lt_alert_mask_LT_ALERT_METADATA => AlertKind::Metadata,
            lt_alert_mask_LT_ALERT_STATE => AlertKind::State,
            lt_alert_mask_LT_ALERT_RESUME => AlertKind::Resume,
            lt_alert_mask_LT_ALERT_PIECE => AlertKind::Piece,
            other => AlertKind::Other(other),
        }
    }
}

/// STATE 桶内子类型（§8.5：msg 前缀由内核 flattener 写入）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateSubKind {
    Finished,
    Paused,
    Error,
    Other,
}

/// 一条扁平化 alert（值拷贝，跨 pop 仍有效）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub kind: AlertKind,
    /// 相关 torrent infohash（非 torrent 类为空串）
    pub ih: String,
    /// 人类可读（STATE 桶含子类型前缀）
    pub msg: String,
    /// 毫秒时间戳（内核侧 timestamp）
    pub at: i64,
    /// RESUME 时：1=可调 take_resume_data
    pub resume_ready: bool,
}

impl Alert {
    /// STATE 桶子类型识别（fin/paused 由内核 msg 前缀约定，§8.5）
    pub fn state_subkind(&self) -> StateSubKind {
        if self.kind != AlertKind::State {
            return StateSubKind::Other;
        }
        if self.msg.contains("torrent finished") {
            StateSubKind::Finished
        } else if self.msg.contains("torrent paused") {
            StateSubKind::Paused
        } else if self.msg.contains("torrent error") {
            // P2-25 修复（审查第三轮）：匹配 FFI 规范化前缀（lt_kernel.cpp
            // 把 torrent_error_alert 统一改写为 "torrent error: <原始>"），
            // 而非裸 "error"——种子名含 "error"（如 "error.mkv state changed
            // to: downloading"）不再被误判 Error → 任务错标 Failed。
            StateSubKind::Error
        } else {
            StateSubKind::Other
        }
    }

    pub fn is_resume_ready(&self) -> bool {
        self.kind == AlertKind::Resume && self.resume_ready
    }
}

impl From<&lt_alert> for Alert {
    fn from(a: &lt_alert) -> Self {
        Alert {
            kind: AlertKind::from(a.kind),
            ih: cstr_field(&a.ih),
            msg: cstr_field(&a.msg),
            at: a.at,
            resume_ready: a.resume_ready != 0,
        }
    }
}

fn cstr_field<const N: usize>(arr: &[c_char; N]) -> String {
    let bytes: Vec<u8> = arr
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}
