//! 错误类型与错误码映射（独立模块）。
//!
//! 错误码来源：DownloadSDK.dll 反汇编 + 实际运行观察。
//! 若 SDK 版本更新导致错误码变化，只改这个文件。

use std::fmt;

/// Xunlei SDK 错误码（部分已知值）。
/// 完整列表随 SDK 版本变化，这里只覆盖已验证的常见码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum XunleiErrCode {
    Ok = 0,
    // 初始化
    InitFailed = -1,
    InvalidParam = -2,
    AlreadyInitialized = -3,
    // 任务生命周期
    TaskNotFound = -1001,
    TaskAlreadyExists = -1002,
    TaskInvalidState = -1003,
    // 网络/IO
    NetworkError = -2001,
    DnsResolveFailed = -2002,
    ConnectionFailed = -2003,
    // 文件
    FileNotFound = -3001,
    FileWriteError = -3002,
    DiskFull = -3003,
    // DCDN
    DcdnNotEnabled = -4001,
    DcdnAuthFailed = -4002,
    // 通用
    Unsupported = -5001,
    Internal = -9999,
}

impl XunleiErrCode {
    /// 从原始 i32 错误码转换为枚举（未知码返回 Internal）。
    pub fn from_i32(code: i32) -> Self {
        match code {
            0 => Self::Ok,
            -1 => Self::InitFailed,
            -2 => Self::InvalidParam,
            -3 => Self::AlreadyInitialized,
            -1001 => Self::TaskNotFound,
            -1002 => Self::TaskAlreadyExists,
            -1003 => Self::TaskInvalidState,
            -2001 => Self::NetworkError,
            -2002 => Self::DnsResolveFailed,
            -2003 => Self::ConnectionFailed,
            -3001 => Self::FileNotFound,
            -3002 => Self::FileWriteError,
            -3003 => Self::DiskFull,
            -4001 => Self::DcdnNotEnabled,
            -4002 => Self::DcdnAuthFailed,
            -5001 => Self::Unsupported,
            _ => Self::Internal,
        }
    }
}

impl fmt::Display for XunleiErrCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Ok => "OK",
            Self::InitFailed => "InitFailed",
            Self::InvalidParam => "InvalidParam",
            Self::AlreadyInitialized => "AlreadyInitialized",
            Self::TaskNotFound => "TaskNotFound",
            Self::TaskAlreadyExists => "TaskAlreadyExists",
            Self::TaskInvalidState => "TaskInvalidState",
            Self::NetworkError => "NetworkError",
            Self::DnsResolveFailed => "DnsResolveFailed",
            Self::ConnectionFailed => "ConnectionFailed",
            Self::FileNotFound => "FileNotFound",
            Self::FileWriteError => "FileWriteError",
            Self::DiskFull => "DiskFull",
            Self::DcdnNotEnabled => "DcdnNotEnabled",
            Self::DcdnAuthFailed => "DcdnAuthFailed",
            Self::Unsupported => "Unsupported",
            Self::Internal => "Internal",
        };
        write!(f, "{}", s)
    }
}

/// Xunlei SDK 错误。
#[derive(Debug, thiserror::Error)]
pub enum XunleiError {
    #[error("init failed: code={0}")]
    InitFailed(i32),

    #[error("create failed: code={0}")]
    CreateFailed(i32),

    #[error("start failed: code={0}")]
    StartFailed(i32),

    #[error("query failed: code={0}")]
    QueryFailed(i32),

    #[error("dll load error: {0}")]
    DllLoad(String),

    #[error("invalid parameter: {0}")]
    InvalidParam(String),

    #[error("task not found: {0}")]
    TaskNotFound(String),

    #[error("other error: {0}")]
    Other(String),
}

impl XunleiError {
    /// 从原始错误码构造。
    pub fn from_sdk_code(code: i32) -> Self {
        let err_code = XunleiErrCode::from_i32(code);
        match err_code {
            XunleiErrCode::InitFailed => Self::InitFailed(code),
            XunleiErrCode::TaskNotFound => Self::TaskNotFound(code.to_string()),
            XunleiErrCode::Internal => Self::Other(format!("unknown error code: {}", code)),
            _ => Self::Other(format!("{}: {}", err_code, code)),
        }
    }

    /// 从原始错误码构造，带上下文。
    pub fn with_context(code: i32, ctx: &'static str) -> Self {
        Self::Other(format!("{}: code={}", ctx, code))
    }
}

/// 结果类型别名。
pub type Result<T> = std::result::Result<T, XunleiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_i32_maps_known_codes() {
        assert_eq!(XunleiErrCode::from_i32(0), XunleiErrCode::Ok);
        assert_eq!(XunleiErrCode::from_i32(-1), XunleiErrCode::InitFailed);
        assert_eq!(XunleiErrCode::from_i32(-2), XunleiErrCode::InvalidParam);
        assert_eq!(XunleiErrCode::from_i32(-3), XunleiErrCode::AlreadyInitialized);
        assert_eq!(XunleiErrCode::from_i32(-1001), XunleiErrCode::TaskNotFound);
        assert_eq!(XunleiErrCode::from_i32(-1002), XunleiErrCode::TaskAlreadyExists);
        assert_eq!(XunleiErrCode::from_i32(-1003), XunleiErrCode::TaskInvalidState);
        assert_eq!(XunleiErrCode::from_i32(-2001), XunleiErrCode::NetworkError);
        assert_eq!(XunleiErrCode::from_i32(-2002), XunleiErrCode::DnsResolveFailed);
        assert_eq!(XunleiErrCode::from_i32(-2003), XunleiErrCode::ConnectionFailed);
        assert_eq!(XunleiErrCode::from_i32(-3001), XunleiErrCode::FileNotFound);
        assert_eq!(XunleiErrCode::from_i32(-3002), XunleiErrCode::FileWriteError);
        assert_eq!(XunleiErrCode::from_i32(-3003), XunleiErrCode::DiskFull);
        assert_eq!(XunleiErrCode::from_i32(-4001), XunleiErrCode::DcdnNotEnabled);
        assert_eq!(XunleiErrCode::from_i32(-4002), XunleiErrCode::DcdnAuthFailed);
        assert_eq!(XunleiErrCode::from_i32(-5001), XunleiErrCode::Unsupported);
        assert_eq!(XunleiErrCode::from_i32(-9999), XunleiErrCode::Internal);
    }

    #[test]
    fn from_i32_unknown_maps_to_internal() {
        // 任意未收录码 → Internal（保守兜底）
        assert_eq!(XunleiErrCode::from_i32(-7777), XunleiErrCode::Internal);
        assert_eq!(XunleiErrCode::from_i32(12345), XunleiErrCode::Internal);
    }

    #[test]
    fn display_prints_symbolic_name() {
        assert_eq!(XunleiErrCode::Ok.to_string(), "OK");
        assert_eq!(XunleiErrCode::TaskNotFound.to_string(), "TaskNotFound");
        assert_eq!(XunleiErrCode::Internal.to_string(), "Internal");
    }

    #[test]
    fn from_sdk_code_routes_by_category() {
        // InitFailed → 专用 InitFailed 变体
        assert!(matches!(XunleiError::from_sdk_code(-1), XunleiError::InitFailed(-1)));
        // TaskNotFound → 专用 TaskNotFound 变体
        assert!(matches!(
            XunleiError::from_sdk_code(-1001),
            XunleiError::TaskNotFound(_)
        ));
        // 未知码 → Other（含原始码）
        match XunleiError::from_sdk_code(-7777) {
            XunleiError::Other(msg) => assert!(msg.contains("-7777"), "msg={msg}"),
            other => panic!("expected Other, got {other:?}"),
        }
        // 已知但无专用变体的码（如 NetworkError）→ Other 并带符号名
        match XunleiError::from_sdk_code(-2001) {
            XunleiError::Other(msg) => assert!(msg.contains("NetworkError"), "msg={msg}"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn with_context_embeds_ctx_and_code() {
        let e = XunleiError::with_context(-1, "XL_Init");
        match e {
            XunleiError::Other(msg) => {
                assert!(msg.contains("XL_Init"), "msg={msg}");
                assert!(msg.contains("-1"), "msg={msg}");
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }
}
