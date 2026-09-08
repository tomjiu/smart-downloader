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
//! 原因「整型参数语义未确认，暂不封装」已在 2026-08-30 附录 A #4 解除：
//! 绑定形状来自反编译推断（bindings.rs），**UNTESTED** —— 已封装但任何调用前
//! 必须真机 dump 校准两个 c_int 参数；loader 以 Option 解析（缺导出 → 返回错误）。

use std::ffi::CString;

use tokio::task;

use crate::error::{Result, XunleiError};
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

    // ======== B 级 DCDN/VIP 凭证注入（附录 A #4，UNTESTED，2026-08-30）========
    //
    // 以下四个封装按反编译推断形状绑定（bindings.rs B 级段）。约束：
    // 1. 首次真机调用前必须 dump 校准两个 c_int（channel/flags 推测）与
    //    SetTaskEquityToken 的整体签名；
    // 2. 符号经 Option 解析，DLL 缺失导出时返回 DllLoad 可读错误（不 panic）；
    // 3. 全部为「凭证消费」接口 —— token/session/cert 必须来自
    //    crates/provider/src/xunlei/vip_speedup.rs（VIP 通道）或调用方自有凭证。

    /// 用 token 激活 DCDN（XL_EnableDcdnWithToken，UNTESTED）。
    ///
    /// `channel`/`flags` 语义未确认（反编译仅见两个 c_int + 两个窄字符串）。
    /// `extra` 允许 None（param_4 是否可空**待验证**，None 时传 null）。
    pub async fn enable_dcdn_with_token(
        &self,
        channel: i32,
        flags: i32,
        token: &str,
        extra: Option<&str>,
    ) -> Result<()> {
        let sym = self.inner.symbols;
        let token = token.to_string();
        let extra = extra.map(str::to_string);
        task::spawn_blocking(move || unsafe {
            let sym = sym.XL_EnableDcdnWithToken.ok_or_else(|| {
                XunleiError::DllLoad(
                    "XL_EnableDcdnWithToken not resolved (DLL missing export)".into(),
                )
            })?;
            let token_c = cstring_or_err(&token, "token")?;
            // CString 必须绑定存活到 FFI 调用点（临时值 .as_ptr() 会在语句末析构 → 悬垂 UB）
            let extra_c = match &extra {
                Some(s) => Some(cstring_or_err(s, "extra")?),
                None => None,
            };
            let r = sym(
                channel,
                flags,
                token_c.as_ptr(),
                extra_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            );
            if r != 0 {
                return Err(XunleiError::with_context(
                    r,
                    "XL_EnableDcdnWithToken failed",
                ));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 用 session 激活 DCDN（XL_EnableDcdnWithSession，UNTESTED）。
    ///
    /// 反编译可见 3 个窄字符串（session/k1/k2，k1/k2 语义未确认）。
    /// `k1`/`k2` 允许 None（是否可空**待验证**）。
    pub async fn enable_dcdn_with_session(
        &self,
        channel: i32,
        flags: i32,
        session: &str,
        k1: Option<&str>,
        k2: Option<&str>,
    ) -> Result<()> {
        let sym = self.inner.symbols;
        let session = session.to_string();
        let k1 = k1.map(str::to_string);
        let k2 = k2.map(str::to_string);
        task::spawn_blocking(move || unsafe {
            let sym = sym.XL_EnableDcdnWithSession.ok_or_else(|| {
                XunleiError::DllLoad(
                    "XL_EnableDcdnWithSession not resolved (DLL missing export)".into(),
                )
            })?;
            let session_c = cstring_or_err(&session, "session")?;
            // 同上：k1/k2 的 CString 绑定存活到调用点，避免临时值悬垂
            let k1_c = match &k1 {
                Some(s) => Some(cstring_or_err(s, "k1")?),
                None => None,
            };
            let k2_c = match &k2 {
                Some(s) => Some(cstring_or_err(s, "k2")?),
                None => None,
            };
            let r = sym(
                channel,
                flags,
                session_c.as_ptr(),
                k1_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                k2_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            );
            if r != 0 {
                return Err(XunleiError::with_context(
                    r,
                    "XL_EnableDcdnWithSession failed",
                ));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 用 VIP 证书激活 DCDN（XL_EnableDcdnWithVipCert，UNTESTED）。
    pub async fn enable_dcdn_with_vip_cert(
        &self,
        channel: i32,
        flags: i32,
        cert: &str,
    ) -> Result<()> {
        let sym = self.inner.symbols;
        let cert = cert.to_string();
        task::spawn_blocking(move || unsafe {
            let sym = sym.XL_EnableDcdnWithVipCert.ok_or_else(|| {
                XunleiError::DllLoad(
                    "XL_EnableDcdnWithVipCert not resolved (DLL missing export)".into(),
                )
            })?;
            let cert_c = cstring_or_err(&cert, "cert")?;
            let r = sym(channel, flags, cert_c.as_ptr());
            if r != 0 {
                return Err(XunleiError::with_context(
                    r,
                    "XL_EnableDcdnWithVipCert failed",
                ));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 给指定任务注入 equity token（XL_SetTaskEquityToken，UNTESTED·最高风险）。
    ///
    /// ⚠️ 无反编译签名样本，(task_id, token*) 为假设形状；
    /// 首次调用前必须以 dump 法确认（见 sdk_export_inventory.md §2.5 缺口）。
    pub async fn set_task_equity_token(&self, task_id: u32, token: &str) -> Result<()> {
        let sym = self.inner.symbols;
        let token = token.to_string();
        task::spawn_blocking(move || unsafe {
            let sym = sym.XL_SetTaskEquityToken.ok_or_else(|| {
                XunleiError::DllLoad(
                    "XL_SetTaskEquityToken not resolved (DLL missing export)".into(),
                )
            })?;
            let token_c = cstring_or_err(&token, "token")?;
            let r = sym(task_id, token_c.as_ptr());
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_SetTaskEquityToken failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }
}

/// CString 构造统一入口（NUL 字节 → InvalidParam），B 级封装与单测共用。
fn cstring_or_err(s: &str, label: &str) -> Result<CString> {
    CString::new(s).map_err(|_| XunleiError::InvalidParam(format!("{label} contains null byte")))
}

#[cfg(test)]
mod bgrade_tests {
    use super::*;

    #[test]
    fn cstring_helper_rejects_nul() {
        assert!(matches!(cstring_or_err("ok", "x"), Ok(_)));
        let err = cstring_or_err("bad\0string", "x").unwrap_err();
        assert!(matches!(err, XunleiError::InvalidParam(m) if m.contains("null byte")));
    }
}
