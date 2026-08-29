//! FreeDCDN 加速（免登录）。
//!
//! 匿名身份 UserID=0 可直接启用 FreeDCDN，无需登录或 VIP 证书。

use tokio::task;

use crate::error::{XunleiError, Result};
use crate::handle::XunleiHandle;
use crate::task::TaskId;

impl XunleiHandle {
    /// 启用 FreeDCDN 加速（免登录）。
    pub async fn enable_free_dcdn(&self, id: &TaskId) -> Result<()> {
        let sym = self.inner.symbols;
        let task_id_u32 = id.0 as u32;

        task::spawn_blocking(move || unsafe {
            let r = (sym.XL_EnableFreeDcdn)(task_id_u32, 1);
            if r != 0 {
                return Err(XunleiError::with_context(
                    r,
                    "XL_EnableFreeDcdn failed",
                ));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 禁用 FreeDCDN。
    pub async fn disable_free_dcdn(&self, id: &TaskId) -> Result<()> {
        let sym = self.inner.symbols;
        let task_id_u32 = id.0 as u32;

        task::spawn_blocking(move || unsafe {
            let r = (sym.XL_DisableFreeDcdn)(task_id_u32);
            if r != 0 {
                return Err(XunleiError::with_context(
                    r,
                    "XL_DisableFreeDcdn failed",
                ));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }
}
