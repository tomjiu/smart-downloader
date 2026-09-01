//! DLL 加载（DownloadSDKProxy.dll）。
//!
//! 负责：
//! 1. 设置 DLL 搜索路径（SetDllDirectoryW）
//! 2. 加载 DownloadSDKProxy.dll（libloading）
//! 3. 获取所有函数指针并缓存
//!
//! 若 SDK 版本新增/删除导出函数，只改这里和 bindings.rs。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::bindings::*;
use crate::error::{XunleiError, Result};

/// SDK 必需的 DLL/EXE 列表（按加载顺序）。
const REQUIRED_FILES: &[&str] = &[
    "DownloadSDKProxy.dll",
    "DownloadSDKServer.exe",
    "DownloadSDK.dll",
];

/// 已加载的 SDK 目录（全局，只初始化一次）。
static LOADED_SDK_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 所有已解析的 XL_* 函数指针。
#[allow(non_snake_case)]
pub struct Symbols {
    pub XL_Init: XLInitFn,
    pub XL_UnInit: XLUnInitFn,
    pub XL_CreateBTTask_V2: XLCreateBTTaskV2Fn,
    pub XL_CreateMagnetTask: XLCreateMagnetTaskFn,
    pub XL_CreateP2spTask: XLCreateP2spTaskFn,
    pub XL_StartTask: XLStartTaskFn,
    pub XL_StopTask: XLStopTaskFn,
    pub XL_DeleteTask: XLDeleteTaskFn,
    pub XL_QueryTaskInfo: XLQueryTaskInfoFn,
    pub XL_AddPeer: XLAddPeerFn,
    pub XL_BatchAddPeer: XLBatchAddPeerFn,
    pub XL_BatchAddBTTracker: XLBatchAddBTTrackerFn,
    pub XL_DiscardPeer: XLDiscardPeerFn,
    pub XL_BatchDiscardPeer: XLBatchDiscardPeerFn,
    pub XL_EnableFreeDcdn: XLEnableFreeDcdnFn,
    pub XL_DisableFreeDcdn: XLDisableFreeDcdnFn,
    pub XL_QueryTaskFlow: XLQueryTaskFlowFn,
    pub XL_SetTaskUserAgent: XLSetTaskUserAgentFn,
    pub XL_AddHttpHeaderField: XLAddHttpHeaderFieldFn,
    pub XL_SetTaskDownloadSpeedLimit: XLSetTaskDownloadSpeedLimitFn,
    pub XL_SetUserInfo: XLSetUserInfoFn,
    pub XL_SetTokenMode: XLSetTokenModeFn,
    pub XL_SetAppGuid: XLSetAppGuidFn,
    pub XL_SetAccelerateCertification: XLSetAccelerateCertificationFn,
    pub XL_SetUserAgent: XLSetUserAgentFn,
    pub XL_SetProxy: XLSetProxyFn,
    pub XL_SetCacheSize: XLSetCacheSizeFn,
    pub XL_SetDownloadWindow: XLSetDownloadWindowFn,
    pub XL_SetGlobalConnectionLimit: XLSetGlobalConnectionLimitFn,
    pub XL_AddServer: XLAddServerFn,
    /// B 级 DCDN/VIP 凭证注入（附录 A #4，UNTESTED）：Option 解析，
    /// 旧版本 DLL 缺失导出时为 None，不影响其余符号加载。
    pub XL_EnableDcdnWithToken: Option<XLEnableDcdnWithTokenFn>,
    pub XL_EnableDcdnWithSession: Option<XLEnableDcdnWithSessionFn>,
    pub XL_EnableDcdnWithVipCert: Option<XLEnableDcdnWithVipCertFn>,
    pub XL_SetTaskEquityToken: Option<XLSetTaskEquityTokenFn>,
}

static SYMBOLS: OnceLock<Symbols> = OnceLock::new();

/// 确保 SDK DLL 已加载并解析所有函数指针。
///
/// `sdk_dir` 应包含 DownloadSDKProxy.dll 等全套文件。
/// 调用前应确保 sdk_dir 存在且包含必需文件。
///
/// 平台门控（2026-08-30 Task 5-a）：非 Windows 上直接返回可读错误短路，
/// 不触碰任何 FFI（本 crate 全平台可编译，但 SDK 运行时仅 Windows 提供）。
pub fn ensure_dlls_loaded(sdk_dir: &Path) -> Result<()> {
    #[cfg(not(windows))]
    {
        let _ = sdk_dir; // 非 Windows 不消费路径
        Err(XunleiError::Other(
            "xunlei-ffi 仅支持 Windows（需要 DownloadSDKProxy.dll + DownloadSDKServer.exe）；\
             当前平台仅提供类型/解析能力，SDK 运行时不可用"
                .into(),
        ))
    }

    #[cfg(windows)]
    {
        ensure_dlls_loaded_windows(sdk_dir)
    }
}

/// Windows 真实实现（DLL 搜索路径 + libloading 解析全部 XL_* 符号）。
#[cfg(windows)]
fn ensure_dlls_loaded_windows(sdk_dir: &Path) -> Result<()> {
    // 只初始化一次
    if let Some(dir) = LOADED_SDK_DIR.get() {
        if dir == sdk_dir {
            return Ok(());
        }
    }

    // 检查必需文件存在
    for file in REQUIRED_FILES {
        let path = sdk_dir.join(file);
        if !path.exists() {
            return Err(XunleiError::DllLoad(format!(
                "missing required file: {}",
                path.display()
            )));
        }
    }

    // 设置 DLL 搜索路径（Windows API）
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        extern "system" {
            fn SetDllDirectoryW(lpPathName: *const u16) -> i32;
        }

        let wide: Vec<u16> = sdk_dir
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            SetDllDirectoryW(wide.as_ptr());
        }
    }

    // 加载 DownloadSDKProxy.dll
    let dll_path = sdk_dir.join("DownloadSDKProxy.dll");
    let lib = unsafe {
        libloading::Library::new(&dll_path)
            .map_err(|e| XunleiError::DllLoad(format!("failed to load {}: {}", dll_path.display(), e)))?
    };

    // 保持 Library 句柄存活（泄漏到 'static）
    let lib = Box::leak(Box::new(lib));

    // 获取所有函数指针
    let symbols = unsafe {
        Symbols {
            XL_Init: *lib.get(b"XL_Init\0").map_err(|e| XunleiError::DllLoad(format!("XL_Init not found: {}", e)))?,
            XL_UnInit: *lib.get(b"XL_UnInit\0").map_err(|e| XunleiError::DllLoad(format!("XL_UnInit not found: {}", e)))?,
            XL_CreateBTTask_V2: *lib.get(b"XL_CreateBTTask_V2\0").map_err(|e| XunleiError::DllLoad(format!("XL_CreateBTTask_V2 not found: {}", e)))?,
            XL_CreateMagnetTask: *lib.get(b"XL_CreateMagnetTask\0").map_err(|e| XunleiError::DllLoad(format!("XL_CreateMagnetTask not found: {}", e)))?,
            XL_CreateP2spTask: *lib.get(b"XL_CreateP2spTask\0").map_err(|e| XunleiError::DllLoad(format!("XL_CreateP2spTask not found: {}", e)))?,
            XL_StartTask: *lib.get(b"XL_StartTask\0").map_err(|e| XunleiError::DllLoad(format!("XL_StartTask not found: {}", e)))?,
            XL_StopTask: *lib.get(b"XL_StopTask\0").map_err(|e| XunleiError::DllLoad(format!("XL_StopTask not found: {}", e)))?,
            XL_DeleteTask: *lib.get(b"XL_DeleteTask\0").map_err(|e| XunleiError::DllLoad(format!("XL_DeleteTask not found: {}", e)))?,
            XL_QueryTaskInfo: *lib.get(b"XL_QueryTaskInfo\0").map_err(|e| XunleiError::DllLoad(format!("XL_QueryTaskInfo not found: {}", e)))?,
            XL_AddPeer: *lib.get(b"XL_AddPeer\0").map_err(|e| XunleiError::DllLoad(format!("XL_AddPeer not found: {}", e)))?,
            XL_BatchAddPeer: *lib.get(b"XL_BatchAddPeer\0").map_err(|e| XunleiError::DllLoad(format!("XL_BatchAddPeer not found: {}", e)))?,
            XL_BatchAddBTTracker: *lib.get(b"XL_BatchAddBTTracker\0").map_err(|e| XunleiError::DllLoad(format!("XL_BatchAddBTTracker not found: {}", e)))?,
            XL_DiscardPeer: *lib.get(b"XL_DiscardPeer\0").map_err(|e| XunleiError::DllLoad(format!("XL_DiscardPeer not found: {}", e)))?,
            XL_BatchDiscardPeer: *lib.get(b"XL_BatchDiscardPeer\0").map_err(|e| XunleiError::DllLoad(format!("XL_BatchDiscardPeer not found: {}", e)))?,
            XL_EnableFreeDcdn: *lib.get(b"XL_EnableFreeDcdn\0").map_err(|e| XunleiError::DllLoad(format!("XL_EnableFreeDcdn not found: {}", e)))?,
            XL_DisableFreeDcdn: *lib.get(b"XL_DisableFreeDcdn\0").map_err(|e| XunleiError::DllLoad(format!("XL_DisableFreeDcdn not found: {}", e)))?,
            XL_QueryTaskFlow: *lib.get(b"XL_QueryTaskFlow\0").map_err(|e| XunleiError::DllLoad(format!("XL_QueryTaskFlow not found: {}", e)))?,
            XL_SetTaskUserAgent: *lib.get(b"XL_SetTaskUserAgent\0").map_err(|e| XunleiError::DllLoad(format!("XL_SetTaskUserAgent not found: {}", e)))?,
            XL_AddHttpHeaderField: *lib.get(b"XL_AddHttpHeaderField\0").map_err(|e| XunleiError::DllLoad(format!("XL_AddHttpHeaderField not found: {}", e)))?,
            XL_SetTaskDownloadSpeedLimit: *lib.get(b"XL_SetTaskDownloadSpeedLimit\0").map_err(|e| XunleiError::DllLoad(format!("XL_SetTaskDownloadSpeedLimit not found: {}", e)))?,
            XL_SetUserInfo: *lib.get(b"XL_SetUserInfo\0").map_err(|e| XunleiError::DllLoad(format!("XL_SetUserInfo not found: {}", e)))?,
            XL_SetTokenMode: *lib.get(b"XL_SetTokenMode\0").map_err(|e| XunleiError::DllLoad(format!("XL_SetTokenMode not found: {}", e)))?,
            XL_SetAppGuid: *lib.get(b"XL_SetAppGuid\0").map_err(|e| XunleiError::DllLoad(format!("XL_SetAppGuid not found: {}", e)))?,
            XL_SetAccelerateCertification: *lib.get(b"XL_SetAccelerateCertification\0").map_err(|e| XunleiError::DllLoad(format!("XL_SetAccelerateCertification not found: {}", e)))?,
            XL_SetUserAgent: *lib.get(b"XL_SetUserAgent\0").map_err(|e| XunleiError::DllLoad(format!("XL_SetUserAgent not found: {}", e)))?,
            XL_SetProxy: *lib.get(b"XL_SetProxy\0").map_err(|e| XunleiError::DllLoad(format!("XL_SetProxy not found: {}", e)))?,
            XL_SetCacheSize: *lib.get(b"XL_SetCacheSize\0").map_err(|e| XunleiError::DllLoad(format!("XL_SetCacheSize not found: {}", e)))?,
            XL_SetDownloadWindow: *lib.get(b"XL_SetDownloadWindow\0").map_err(|e| XunleiError::DllLoad(format!("XL_SetDownloadWindow not found: {}", e)))?,
            XL_SetGlobalConnectionLimit: *lib.get(b"XL_SetGlobalConnectionLimit\0").map_err(|e| XunleiError::DllLoad(format!("XL_SetGlobalConnectionLimit not found: {}", e)))?,
            XL_AddServer: *lib.get(b"XL_AddServer\0").map_err(|e| XunleiError::DllLoad(format!("XL_AddServer not found: {}", e)))?,
            XL_EnableDcdnWithToken: lib.get(b"XL_EnableDcdnWithToken\0").ok().map(|f| *f),
            XL_EnableDcdnWithSession: lib.get(b"XL_EnableDcdnWithSession\0").ok().map(|f| *f),
            XL_EnableDcdnWithVipCert: lib.get(b"XL_EnableDcdnWithVipCert\0").ok().map(|f| *f),
            XL_SetTaskEquityToken: lib.get(b"XL_SetTaskEquityToken\0").ok().map(|f| *f),
        }
    };

    LOADED_SDK_DIR
        .set(sdk_dir.to_path_buf())
        .map_err(|_| XunleiError::DllLoad("SDK already loaded from different directory".into()))?;

    SYMBOLS
        .set(symbols)
        .map_err(|_| XunleiError::DllLoad("symbols already loaded".into()))?;

    Ok(())
}

/// 获取已解析的函数指针。
pub fn symbols() -> &'static Symbols {
    SYMBOLS.get().expect("xunlei SDK not loaded; call ensure_dlls_loaded first")
}

/// 获取已加载的 SDK 目录。
pub fn loaded_sdk_dir() -> Option<&'static PathBuf> {
    LOADED_SDK_DIR.get()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_ensure_dlls_loaded_checks_existence() {
        let fake_dir = PathBuf::from("/nonexistent/xunlei-sdk");
        let result = ensure_dlls_loaded(&fake_dir);
        assert!(result.is_err());
        // 非 Windows：入口短路，返回「仅支持 Windows」错误（Task 5-a 跨平台门控）
        #[cfg(not(windows))]
        match &result.unwrap_err() {
            XunleiError::Other(msg) => assert!(
                msg.contains("仅支持 Windows"),
                "非 Windows 应返回可读平台错误，msg={msg}"
            ),
            other => panic!("expected Other(仅支持 Windows), got {other:?}"),
        }
        // Windows：真实逻辑 —— 必需文件缺失 → DllLoad
        #[cfg(windows)]
        assert!(matches!(result.unwrap_err(), XunleiError::DllLoad(_)));
    }
}
