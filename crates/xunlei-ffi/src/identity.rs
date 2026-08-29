//! 用户身份 / 加速凭证 注入面（追加，不动既有声明）。
//!
//! 考古结论（见 docs/research/xunlei/sdk_export_inventory.md）：
//! DownloadSDK 是一套纯下载/P2P 加速引擎，**无账号登录能力**。
//! 本模块暴露的是 SDK 真实导出的「身份/凭证注入器」——调用方需先从
//! 迅雷云盘 Pan API（crates/provider/src/xunlei）拿到 user_id / token / 证书后，
//! 再喂给 SDK。这些函数本身不发起任何网络登录。
//!
//! 仅封装反编译已确认完整逻辑的 A 级函数：
//! - `XL_SetTokenMode`           全局 token 模式开关
//! - `XL_SetAppGuid`             注入应用 GUID 字符串（来源标识）
//! - `XL_SetAccelerateCertification`  注入加速证书字符串
//!
//! B 级函数（XL_EnableDcdnWithToken/Session/VipCert、XL_SetTaskEquityToken）
//! 因整型参数语义未确认，暂不封装，待 dump/实测确认后追加。

use std::ffi::CString;

use tokio::task;

use crate::error::{XunleiError, Result};
use crate::handle::XunleiHandle;

impl XunleiHandle {
    /// 设置全局 token 模式（XL_SetTokenMode）。
    ///
    /// `mode` 语义由 SDK 内部定义；仅作模式开关，与账号登录无关。
    pub async fn set_token_mode(&self, mode: u32) -> Result<()> {
        let sym = self.inner.symbols;
        task::spawn_blocking(move || unsafe {
            let r = (sym.XL_SetTokenMode)(mode);
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_SetTokenMode failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 注入应用 GUID（XL_SetAppGuid）。
    ///
    /// GUID 为调用方自取的全局唯一串，作为来源标识，与账号登录无关。
    /// 反编译确认该导出仅接收单一 GUID 字符串指针，无 handle 参数。
    pub async fn set_app_guid(&self, guid: &str) -> Result<()> {
        let sym = self.inner.symbols;
        let guid = guid.to_string();
        task::spawn_blocking(move || unsafe {
            let guid_c = CString::new(guid)
                .map_err(|_| XunleiError::InvalidParam("app_guid contains null byte".into()))?;
            let r = (sym.XL_SetAppGuid)(guid_c.as_ptr());
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_SetAppGuid failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 注入加速证书（XL_SetAccelerateCertification）。
    ///
    /// `cert` 为调用方已获取的加速证书字符串（来自迅雷加速体系），
    /// SDK 只做凭证注入与消费，不发起登录。
    pub async fn set_accelerate_certification(&self, cert: &str) -> Result<()> {
        let sym = self.inner.symbols;
        let cert = cert.to_string();
        task::spawn_blocking(move || unsafe {
            let cert_c = CString::new(cert)
                .map_err(|_| XunleiError::InvalidParam("cert contains null byte".into()))?;
            let r = (sym.XL_SetAccelerateCertification)(cert_c.as_ptr());
            if r != 0 {
                return Err(XunleiError::with_context(
                    r,
                    "XL_SetAccelerateCertification failed",
                ));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }
}
