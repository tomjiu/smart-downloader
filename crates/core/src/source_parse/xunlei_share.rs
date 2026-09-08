//! 迅雷网盘分享链接解析（pan.xunlei.com）。
//!
//! 仅做 URL 结构解析与校验，不涉及云 API 调用（云取回需登录，v1 不实现）。

use std::error::Error;
use std::fmt;

/// 分享链接信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XunleiShareInfo {
    /// 分享 ID（路径段 /s/xxx）。
    pub share_id: String,
    /// 提取码（?pwd=yyy），无则为 None。
    pub password: Option<String>,
}

/// 解析错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XunleiShareError {
    /// 不是有效的 pan.xunlei.com 分享链接。
    InvalidUrl,
    /// 缺少 /s/ 分享 ID。
    MissingShareId,
    /// 提取码格式错误。
    InvalidPassword,
}

impl fmt::Display for XunleiShareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl => write!(f, "invalid xunlei share url"),
            Self::MissingShareId => write!(f, "missing share id"),
            Self::InvalidPassword => write!(f, "invalid password"),
        }
    }
}

impl Error for XunleiShareError {}

/// 解析迅雷网盘分享链接。
///
/// 支持格式：
/// - `https://pan.xunlei.com/s/xxx`
/// - `https://pan.xunlei.com/s/xxx?pwd=yyy`
/// - `http://pan.xunlei.com/s/xxx`
pub fn parse_xunlei_share(link: &str) -> Result<XunleiShareInfo, XunleiShareError> {
    let url = url::Url::parse(link).map_err(|_| XunleiShareError::InvalidUrl)?;

    if url.host_str() != Some("pan.xunlei.com") {
        return Err(XunleiShareError::InvalidUrl);
    }

    let path = url.path();
    let share_id = path
        .strip_prefix('/')
        .and_then(|p| p.strip_prefix("s/"))
        .filter(|s| !s.is_empty() && !s.contains('/'))
        .ok_or(XunleiShareError::MissingShareId)?
        .to_string();

    let password = url
        .query_pairs()
        .find(|(k, _)| k == "pwd")
        .map(|(_, v)| v.to_string());

    Ok(XunleiShareInfo { share_id, password })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_share_id_only() {
        let info = parse_xunlei_share("https://pan.xunlei.com/s/abc123").unwrap();
        assert_eq!(info.share_id, "abc123");
        assert_eq!(info.password, None);
    }

    #[test]
    fn parse_share_with_password() {
        let info = parse_xunlei_share("https://pan.xunlei.com/s/abc123?pwd=1234").unwrap();
        assert_eq!(info.share_id, "abc123");
        assert_eq!(info.password, Some("1234".to_string()));
    }

    #[test]
    fn reject_non_pan_host() {
        assert!(parse_xunlei_share("https://example.com/s/abc123").is_err());
    }

    #[test]
    fn reject_missing_share_id() {
        assert!(parse_xunlei_share("https://pan.xunlei.com/s/").is_err());
    }

    #[test]
    fn reject_invalid_url() {
        assert!(parse_xunlei_share("not a url").is_err());
    }
}
