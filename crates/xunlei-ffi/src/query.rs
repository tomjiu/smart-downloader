//! 状态查询（QueryTaskInfo）。
//!
//! 将 C 结构体（XLTaskInfo）转换为 Rust 结构体（TaskInfo）。

use tokio::task;

use crate::bindings::XLTaskInfo;
use crate::error::{XunleiError, Result};
use crate::handle::XunleiHandle;
use crate::task::TaskId;

/// 任务状态（对应 C XLTaskInfo.task_state）。
///
/// 2026-08-27 真机 dump 铁证（本地 HTTP server + P2SP 任务完整生命周期观察）：
///   0 = 未启动（创建后、start 前）
///   3 = 下载中（download_size 增长）
///   5 = 暂停（XL_StopTask 后）
///   7 = 完成（download_size == file_size，本地 HTTP 秒下 5MB 铁证）
/// 未观察到的值（1/2/4/6/8/9）归为 Unknown。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    /// 未启动（创建后、start 前，铁证 state=0）
    Pending = 0,
    /// 下载中（铁证 state=3，download_size 增长）
    Downloading = 3,
    /// 暂停（铁证 state=5，XL_StopTask 后）
    Paused = 5,
    /// 完成（铁证 state=7，download_size == file_size）
    Completed = 7,
    /// 其他未确认状态（1/2/4/6/8/9）
    Unknown = 0xff,
}

impl From<u32> for TaskState {
    fn from(v: u32) -> Self {
        match v {
            0 => TaskState::Pending,
            3 => TaskState::Downloading,
            5 => TaskState::Paused,
            7 => TaskState::Completed,
            _ => TaskState::Unknown,
        }
    }
}

/// 任务信息（从 XLTaskInfo 转换）。
///
/// ⚠️ 2026-08-27 真机铁证：download_size/file_size 是 u32（非 u64）。
/// 仅保留已 dump 确认的字段，其余（速度/peer/DHT 等）待后续 dump 还原。
#[derive(Debug, Clone)]
pub struct TaskInfo {
    pub state: TaskState,
    pub file_size: u64,
    pub download_size: u64,
    pub peer_count: u32,
    pub conn_count: u32,
}

impl XunleiHandle {
    /// 查询任务信息。
    pub async fn query_task_info(&self, task_id: &TaskId) -> Result<TaskInfo> {
        let inner = self.inner.clone();
        let tid = task_id.0;
        task::spawn_blocking(move || {
            let mut info = XLTaskInfo {
                size: 0x39c, // 反汇编铁证 = 924，非 size_of::<Self>()
                task_state: 0,
                field8: 0,
                file_size: 0,
                field10: 0,
                download_size: 0,
                field18: 0,
                download_size_dup: 0,
                field20: 0,
                count24: 0,
                field28: 0,
                peer_count: 0,
                conn_count: 0,
                download_size_dup2: 0,
                _remaining: [0u8; 924 - 0x38],
            };

            unsafe {
                let tid = tid as u32;
                let r = (inner.symbols.XL_QueryTaskInfo)(tid, &mut info);
                if r != 0 {
                    return Err(XunleiError::QueryFailed(r));
                }
            }

            Ok(TaskInfo {
                state: TaskState::from(info.task_state),
                file_size: info.file_size as u64,
                download_size: info.download_size as u64,
                peer_count: info.peer_count,
                conn_count: info.conn_count,
            })
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }
}
