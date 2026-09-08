//! 状态查询（QueryTaskInfo / QueryTaskFlow）。
//!
//! 将 C 结构体（XLTaskInfo / XLTaskFlow）转换为 Rust 结构体（TaskInfo / TaskFlow）。

use tokio::task;

use crate::bindings::{XLTaskFlow, XLTaskInfo};
use crate::error::{Result, XunleiError};
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

    /// 查询任务流量/速度（XL_QueryTaskFlow，3 参数签名）。
    ///
    /// 来源（Task 5-a 签名补全，见 bindings.rs::XLQueryTaskFlowFn 注释）：
    /// - NEXT_ACTION.md:579：3 参数非 2 参数；
    /// - xunlei_research_complete.md RVA 0x178f0 prologue：`(u32 task_id, u32 flow_type, ptr out)`；
    /// - 速度字段**不在** XLTaskInfo，down_rate 需本接口单独查询。
    ///
    /// `flow_type` 语义待真机验证（假设 0=下载 / 1=上传，调用方可用常量
    /// [`TASK_FLOW_DOWNLOAD`] / [`TASK_FLOW_UPLOAD`]）。
    pub async fn query_task_flow(&self, task_id: &TaskId, flow_type: u32) -> Result<TaskFlow> {
        let inner = self.inner.clone();
        let tid = task_id.0 as u32;
        task::spawn_blocking(move || {
            let mut flow = XLTaskFlow {
                size: 0x18, // versioned struct；真实 size 待真机 dump 确认
                download_bytes: 0,
                upload_bytes: 0,
                _pad: 0,
            };

            unsafe {
                let r = (inner.symbols.XL_QueryTaskFlow)(tid, flow_type, &mut flow);
                if r != 0 {
                    return Err(XunleiError::with_context(r, "XL_QueryTaskFlow failed"));
                }
            }

            Ok(TaskFlow {
                download_bytes: flow.download_bytes,
                upload_bytes: flow.upload_bytes,
            })
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 查询任务下载速度（字节/秒）。
    ///
    /// 便捷封装：按假设 flow_type=0（下载）调 `query_task_flow`。
    /// ⚠️ 待真机验证：速度语义（瞬时 vs 累计流量）取决于 XLTaskFlow 布局 dump 结果。
    pub async fn query_download_speed(&self, task_id: &TaskId) -> Result<u64> {
        self.query_task_flow(task_id, TASK_FLOW_DOWNLOAD)
            .await
            .map(|f| f.download_bytes)
    }

    /// 查询全局下载速度（XLGetGlobalDownloadSpeed）。
    ///
    /// ⚠️ 该符号是 **macOS DownloadKit** 的 C API（macos_abi_reverse.md:111-119），
    /// Windows DownloadSDK.dll **无此导出**（sdk_export_inventory.md 全表核对），
    /// 故本方法在所有平台都返回明确错误：
    /// - Windows 侧全局速度：待逆向 `XL_QueryGlobalStat`（out size=0x1c，字段未还原）；
    /// - macOS 侧：待未来 `xunlei-ffi-macos` crate 落地（类型已预留
    ///   `bindings::XLGetGlobalDownloadSpeedFn`）。
    pub async fn global_download_speed(&self) -> Result<u64> {
        let _ = self.inner.symbols; // 保持与真实查询一致的调用面
        Err(XunleiError::Other(
            "XLGetGlobalDownloadSpeed 仅 macOS DownloadKit 提供（Windows 导出表无此符号）；\
             Windows 全局速度待 XL_QueryGlobalStat 布局逆向，任务级速度用 query_task_flow"
                .into(),
        ))
    }
}

/// 任务级速度/流量（XL_QueryTaskFlow 输出的 Rust 视图）。
///
/// ⚠️ 字段语义待真机验证：XLTaskFlow 布局（size=0x18 假设）未 dump 确认，
/// download/upload 字节含义（瞬时速率 or 累计流量）以真机 dump 为准。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TaskFlow {
    pub download_bytes: u64,
    pub upload_bytes: u64,
}

/// XL_QueryTaskFlow.flow_type 假设值：下载流量（待真机验证）。
pub const TASK_FLOW_DOWNLOAD: u32 = 0;
/// XL_QueryTaskFlow.flow_type 假设值：上传流量（待真机验证）。
pub const TASK_FLOW_UPLOAD: u32 = 1;
