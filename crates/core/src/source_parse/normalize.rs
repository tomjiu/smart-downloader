//! 用户输入链接归一化（Daemon add 入口）：
//! 迅雷链接家族（thunder:// / qqdl://）解码为真实 HTTP URL；其余分类透传。
//!
//! - `thunder://` 内容 = base64("AA" + 真实URL + "ZZ")（§7.1 D36）
//! - `qqdl://`  内容 = base64(真实URL)，无 AA/ZZ 壳

use crate::source_parse::thunder::{decode_base64_lenient, decode_thunder};
use crate::source_parse::xunlei_share::parse_xunlei_share;

/// 归一化后的下载源分类（DaemonState::add_link_task 消费）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NormalizedSource {
    /// 可直接交给 HTTP 引擎的真实 URL（http/https）。
    Http(String),
    /// BT 磁力链接（v1 无 BT 引擎 → 由调用方报 Unsupported）。
    Magnet(String),
    /// FTP 链接（含可选 user:pass@，匿名 → anonymous）。
    Ftp(String),
    /// eD2k（本项目不支持）。
    Ed2k(String),
    /// 迅雷网盘分享链接。
    XunleiShare(String),
    /// 无法识别/无法解码的输入（保原始串，供报错）。
    Unsupported(String),
}

fn is_http(u: &str) -> bool {
    u.starts_with("http://") || u.starts_with("https://")
}

fn is_ftp(u: &str) -> bool {
    u.starts_with("ftp://")
}

/// 归一化用户提交的任意链接 → 下载源分类。
pub fn normalize_user_link(link: &str) -> NormalizedSource {
    if is_http(link) {
        return NormalizedSource::Http(link.to_string());
    }
    if is_ftp(link) {
        return NormalizedSource::Ftp(link.to_string());
    }
    if let Some(rest) = link.strip_prefix("thunder://") {
        return match decode_thunder(link) {
            Ok(real) if is_http(&real) => NormalizedSource::Http(real),
            Ok(other) => {
                NormalizedSource::Unsupported(format!("thunder:// 解码为非 HTTP 协议: {other}"))
            }
            Err(_) => NormalizedSource::Unsupported(format!("thunder:// 解码失败: {rest}")),
        };
    }
    if let Some(rest) = link.strip_prefix("qqdl://") {
        return match decode_base64_lenient(rest) {
            Ok(bytes) => {
                let real = String::from_utf8_lossy(&bytes).into_owned();
                if is_http(&real) {
                    NormalizedSource::Http(real)
                } else {
                    NormalizedSource::Unsupported(format!("qqdl:// 解码为非 HTTP 协议: {real}"))
                }
            }
            Err(_) => NormalizedSource::Unsupported(format!("qqdl:// 解码失败: {rest}")),
        };
    }
    if link.starts_with("magnet:") {
        return NormalizedSource::Magnet(link.to_string());
    }
    if link.starts_with("ed2k://") {
        return NormalizedSource::Ed2k(link.to_string());
    }
    // 迅雷网盘分享链接（pan.xunlei.com/s/xxx?pwd=yyy）
    if link.contains("pan.xunlei.com/s/") {
        if parse_xunlei_share(link).is_ok() {
            return NormalizedSource::XunleiShare(link.to_string());
        }
    }
    NormalizedSource::Unsupported(link.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;

    fn enc_b64(s: &str) -> String {
        B64.encode(s.as_bytes())
    }

    #[test]
    fn http_passthrough() {
        assert_eq!(
            normalize_user_link("http://example.com/a.bin"),
            NormalizedSource::Http("http://example.com/a.bin".into())
        );
        assert_eq!(
            normalize_user_link("https://example.com/f.bin?x=1"),
            NormalizedSource::Http("https://example.com/f.bin?x=1".into())
        );
    }

    #[test]
    fn thunder_decodes_to_http() {
        let link = format!("thunder://{}", enc_b64("AAhttp://example.com/a.binZZ"));
        assert_eq!(
            normalize_user_link(&link),
            NormalizedSource::Http("http://example.com/a.bin".into())
        );
    }

    #[test]
    fn thunder_non_http_inner_is_unsupported() {
        let link = format!("thunder://{}", enc_b64("AAftp://example.com/a.binZZ"));
        assert!(matches!(
            normalize_user_link(&link),
            NormalizedSource::Unsupported(_)
        ));
    }

    #[test]
    fn thunder_bad_base64_is_unsupported() {
        assert!(matches!(
            normalize_user_link("thunder://!!!not-base64!!!"),
            NormalizedSource::Unsupported(_)
        ));
    }

    #[test]
    fn qqdl_decodes_to_http() {
        let link = format!("qqdl://{}", enc_b64("http://example.com/b.bin"));
        assert_eq!(
            normalize_user_link(&link),
            NormalizedSource::Http("http://example.com/b.bin".into())
        );
    }

    #[test]
    fn qqdl_bad_inner_is_unsupported() {
        let link = format!("qqdl://{}", enc_b64("ed2k://x"));
        assert!(matches!(
            normalize_user_link(&link),
            NormalizedSource::Unsupported(_)
        ));
    }

    #[test]
    fn magnet_classified() {
        assert_eq!(
            normalize_user_link("magnet:?xt=urn:btih:abc"),
            NormalizedSource::Magnet("magnet:?xt=urn:btih:abc".into())
        );
    }

    #[test]
    fn ftp_classified() {
        assert_eq!(
            normalize_user_link("ftp://user:pass@example.com/a.bin"),
            NormalizedSource::Ftp("ftp://user:pass@example.com/a.bin".into())
        );
    }

    #[test]
    fn ed2k_classified() {
        assert_eq!(
            normalize_user_link("ed2k://file|a|1|hash|"),
            NormalizedSource::Ed2k("ed2k://file|a|1|hash|".into())
        );
    }

    #[test]
    fn unknown_is_unsupported() {
        assert_eq!(
            normalize_user_link("sqla://whatever"),
            NormalizedSource::Unsupported("sqla://whatever".into())
        );
    }
}
