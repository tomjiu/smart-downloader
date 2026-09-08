//! 生命周期管理（XL_Init / XL_UnInit）。
//!
//! XunleiHandle 是线程安全的引擎句柄，内部封装已 Init 的 SDK。
//! 所有 FFI 调用通过 tokio::task::spawn_blocking 包装（SDK 是同步阻塞的）。

use std::path::Path;
use std::sync::Arc;
use tokio::task;

use crate::bindings::XLInitParam;
use crate::error::{Result, XunleiError};
use crate::loader::{ensure_dlls_loaded, symbols, Symbols};

/// 迅雷引擎句柄（线程安全，可 clone）。
#[derive(Clone)]
pub struct XunleiHandle {
    pub(crate) inner: Arc<HandleInner>,
}

pub(crate) struct HandleInner {
    // NOTE(2026-08-27 真机铁证): handle 是 SDK 全局状态（无输出参数），
    // 无需存储。此字段保留为「已初始化」占位，避免 Drop 逻辑依赖缺失。
    pub(crate) symbols: &'static Symbols,
}

impl XunleiHandle {
    /// 初始化引擎。
    ///
    /// `sdk_dir` — 包含 DownloadSDKProxy.dll 等全套文件的目录
    /// `log_dir` — 日志目录（新 ABI 中已移入 JSON 配置，此参数暂保留未用）
    /// `config_dir` — 配置目录（同上）
    /// `app_guid` — 应用 GUID（自取唯一串，如 "smart-downloader-001"）
    pub async fn new(
        sdk_dir: &Path,
        _log_dir: &Path,
        _config_dir: &Path,
        app_guid: &str,
    ) -> Result<Self> {
        ensure_dlls_loaded(sdk_dir)?;
        let sym = symbols();

        // 将路径和字符串转换为拥有所有权的类型，以便传入 spawn_blocking
        let sdk_dir = sdk_dir.to_path_buf();
        let app_guid = app_guid.to_string();

        // XL_Init 是同步调用，通过 spawn_blocking 包装
        // NOTE(2026-08-27 真机铁证): XL_Init 无 out_handle 参数，handle 是 SDK 全局状态。
        task::spawn_blocking(move || -> Result<()> {
            unsafe {
                let server_path = sdk_dir.join("DownloadSDKServer.exe");
                let server_path_c = path_to_cstring(&server_path);

                // NOTE: server_path 有 100 字符长度限制（server 端 cmp 0x64），
                // 超长会返回错误码 2。此处检测并给出明确错误提示。
                let server_path_len = server_path.to_string_lossy().len();
                if server_path_len > 100 {
                    return Err(XunleiError::Other(format!(
                        "SDK 目录路径过长（{} > 100 字符），DownloadSDKServer.exe 无法启动。\
                         请将 SDK 复制到短路径（如 C:\\xl\\）",
                        server_path_len
                    )));
                }

                // XLInitParam 真实布局（2026-08-27 真机铁证）：
                //   size(4) + u32(4) + word(2) + JSON(30) = 40
                // field8 = 0（空 JSON）实测成功；0xffff（无 JSON 哨兵）会返回 1。
                let mut json_buf = [0i8; 30];
                let mut field8: u16 = 0; // 空 JSON（实测 rc=0）
                // 若 app_guid 非空，构造 `{app_guid:xxx}` JSON 填入
                if !app_guid.is_empty() {
                    let json = format!("{{app_guid:{}}}", app_guid);
                    let bytes = json.as_bytes();
                    if bytes.len() <= 30 {
                        for (i, b) in bytes.iter().enumerate() {
                            json_buf[i] = *b as i8;
                        }
                        field8 = bytes.len() as u16;
                    }
                }

                let param = XLInitParam {
                    size: 0x28,
                    field4: 0,
                    field8,
                    json: json_buf,
                };

                let r = (sym.XL_Init)(server_path_c.as_ptr(), &param);
                if r != 0 {
                    return Err(XunleiError::with_context(
                        r,
                        "XL_Init failed (check DownloadSDKServer.exe exists and SDK version matches)",
                    ));
                }

                Ok(())
            }
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))??;

        Ok(Self {
            inner: Arc::new(HandleInner { symbols: sym }),
        })
    }

    /// 设置用户信息（可选，匿名模式不调用）。
    ///
    /// ABI 修正（2026-08-25）：反编译实证两参数均为 `const char*`
    /// （C 侧做 strlen + XPF_String 构造），旧整数签名存在 strlen(整数) 段错误风险，
    /// 已按考古证据改为字符串。参数语义待真机实测澄清（user_id 文本 / vip 等级文本？）。
    pub async fn set_user_info(&self, user_id: &str, vip_type: &str) -> Result<()> {
        let sym = self.inner.symbols;
        let user_id = std::ffi::CString::new(user_id)
            .map_err(|e| XunleiError::Other(format!("user_id CString: {}", e)))?;
        let vip_type = std::ffi::CString::new(vip_type)
            .map_err(|e| XunleiError::Other(format!("vip_type CString: {}", e)))?;
        task::spawn_blocking(move || unsafe {
            let r = (sym.XL_SetUserInfo)(user_id.as_ptr(), vip_type.as_ptr());
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_SetUserInfo failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 设置全局 User-Agent。
    pub async fn set_user_agent(&self, ua: &str) -> Result<()> {
        let sym = self.inner.symbols;
        let ua = ua.to_string();
        task::spawn_blocking(move || unsafe {
            let ua_c = std::ffi::CString::new(ua)
                .map_err(|_| XunleiError::InvalidParam("user_agent contains null byte".into()))?;
            let r = (sym.XL_SetUserAgent)(ua_c.as_ptr());
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_SetUserAgent failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 设置代理。
    pub async fn set_proxy(&self, proxy: &str) -> Result<()> {
        let sym = self.inner.symbols;
        let proxy = proxy.to_string();
        task::spawn_blocking(move || unsafe {
            let proxy_c = std::ffi::CString::new(proxy)
                .map_err(|_| XunleiError::InvalidParam("proxy contains null byte".into()))?;
            let r = (sym.XL_SetProxy)(proxy_c.as_ptr());
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_SetProxy failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 设置缓存大小（MB）。
    pub async fn set_cache_size(&self, size_mb: u32) -> Result<()> {
        let sym = self.inner.symbols;
        task::spawn_blocking(move || unsafe {
            let r = (sym.XL_SetCacheSize)(size_mb);
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_SetCacheSize failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 设置下载窗口大小。
    pub async fn set_download_window(&self, window: u32) -> Result<()> {
        let sym = self.inner.symbols;
        task::spawn_blocking(move || unsafe {
            let r = (sym.XL_SetDownloadWindow)(window);
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_SetDownloadWindow failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 设置全局连接限制。
    pub async fn set_global_connection_limit(&self, limit: u32) -> Result<()> {
        let sym = self.inner.symbols;
        task::spawn_blocking(move || unsafe {
            let r = (sym.XL_SetGlobalConnectionLimit)(limit);
            if r != 0 {
                return Err(XunleiError::with_context(
                    r,
                    "XL_SetGlobalConnectionLimit failed",
                ));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }
}

impl Drop for HandleInner {
    fn drop(&mut self) {
        // XL_UnInit 是同步调用，在 drop 中直接调用（不通过 spawn_blocking）
        // 注意：Drop 不能是 async，所以这里只能同步调用
        // NOTE(2026-08-27): XL_UnInit 是 0 参数（handle 是 SDK 全局状态）。
        unsafe {
            (self.symbols.XL_UnInit)();
        }
    }
}

/// 路径转 CString（UTF-8）。
fn path_to_cstring(path: &Path) -> std::ffi::CString {
    let s = path.to_string_lossy().into_owned();
    std::ffi::CString::new(s).expect("path contains null byte")
}
