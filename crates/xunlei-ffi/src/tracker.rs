//! Tracker 管理（BatchAddBTTracker）。
//!
//! 匿名模式下可用，用于添加自定义 BT tracker 加速资源发现。

use std::os::raw::c_char;
use tokio::task;

use crate::error::{Result, XunleiError};
use crate::handle::XunleiHandle;
use crate::task::TaskId;

impl XunleiHandle {
    /// 批量添加 BT tracker。
    ///
    /// `trackers` 是 tracker URL 列表（如 `["udp://tracker.example.com:6969/announce"]`）。
    pub async fn batch_add_tracker(&self, id: &TaskId, trackers: &[&str]) -> Result<()> {
        if trackers.is_empty() {
            return Ok(());
        }

        let sym = self.inner.symbols;
        // Clone strings into owned Vec<String> so they can be moved into spawn_blocking
        let trackers: Vec<String> = trackers.iter().map(|s| s.to_string()).collect();
        let task_id_u32 = id.0 as u32;

        task::spawn_blocking(move || unsafe {
            // 在 blocking 线程内转换为 C 字符串数组
            let mut c_strings = Vec::new();
            for s in &trackers {
                let c = std::ffi::CString::new(s.clone()).map_err(|_| {
                    XunleiError::InvalidParam("tracker url contains null byte".into())
                })?;
                c_strings.push(c);
            }

            let ptrs: Vec<*const c_char> = c_strings.iter().map(|s| s.as_ptr()).collect();

            let r = (sym.XL_BatchAddBTTracker)(task_id_u32, ptrs.as_ptr(), ptrs.len() as u32);
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_BatchAddBTTracker failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }
}
