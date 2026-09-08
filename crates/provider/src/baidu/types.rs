//! 百度网盘 Provider 类型：错误分类 + 端点常量（B3-a）。
//!
//! 常量取自 2026-09-05 真实分享链接实测（`docs/research/baidu/share_protocol.md`）：
//! verify/list 对 UA 与 Referer 敏感（缺失或非浏览器 UA 触发风控 -12），
//! 故 UA 固定为桌面 Chrome 形状；APP_ID 为 web 端公共参数。

use thiserror::Error;

/// 百度网盘 Web 基址。
pub const BASE: &str = "https://pan.baidu.com";

/// 浏览器 UA（verify/list 均对 UA 敏感，非浏览器 UA 会被风控拦截）。
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// verify 接口 web 端公共 app_id（实测值）。
pub const APP_ID: &str = "250528";

/// 百度 Provider 错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BaiduError {
    #[error("not a pan.baidu.com share link (支持 /s/1xxx 与 /share/init?surl= 形态)")]
    NotShareLink,
    #[error("提取码错误或被风控拦截 (errno -12)")]
    WrongPasscode,
    #[error("分享需要提取码校验 (errno {0})")]
    NeedVerify(i64),
    #[error("分享页 HTML 中未找到 shareid/uk（分享可能已失效或风控）")]
    MetaParse,
    #[error("百度协议错误 (errno {0})")]
    Protocol(i64),
    #[error("http error: {0}")]
    Http(String),
}

impl From<reqwest::Error> for BaiduError {
    fn from(e: reqwest::Error) -> Self {
        BaiduError::Http(e.to_string())
    }
}
