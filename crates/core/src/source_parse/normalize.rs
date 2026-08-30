//! 用户输入链接归一化（Daemon add 入口）：
//! 迅雷链接家族（thunder:// / qqdl://）解码为真实 HTTP URL；其余分类透传。
//!
//! - `thunder://` 内容 = base64("AA" + 真实URL + "ZZ")（§7.1 D36）
//! - `qqdl://`  内容 = base64(真实URL)，无 AA/ZZ 壳

use crate::source_parse::ed2k::parse_ed2k;
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
    if link
        .get(..7)
        .map(|p| p.eq_ignore_ascii_case("ed2k://"))
        .unwrap_or(false)
    {
        // ed2k（Task 5-a T3）：结构化解析（名称/大小/MD4）。
        // 完整引擎在远期（BACKLOG C 段）：合法链接分类为 Ed2k（路由层给出
        // 携带 md4/size 元数据的明确错误）；非法链接 → Unsupported（报错可读）。
        return match parse_ed2k(link) {
            Ok(_) => NormalizedSource::Ed2k(link.to_string()),
            Err(e) => NormalizedSource::Unsupported(format!("ed2k 链接解析失败: {e}")),
        };
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
        // Task 5-a T3：合法 ed2k → Ed2k 分类（结构化解析见 ed2k.rs）
        assert_eq!(
            normalize_user_link("ed2k://|file|a.bin|1|0123456789abcdef0123456789abcdef|/"),
            NormalizedSource::Ed2k("ed2k://|file|a.bin|1|0123456789abcdef0123456789abcdef|/".into())
        );
    }

    #[test]
    fn ed2k_case_insensitive_scheme_classified() {
        assert!(matches!(
            normalize_user_link("ED2K://|file|a.bin|1|0123456789abcdef0123456789abcdef|/"),
            NormalizedSource::Ed2k(_)
        ));
    }

    #[test]
    fn ed2k_bad_md4_is_unsupported() {
        // 非法 ed2k（md4 槽位不是 32 位十六进制）→ Unsupported（报错可读）
        match normalize_user_link("ed2k://file|a|1|hash|") {
            NormalizedSource::Unsupported(msg) => {
                assert!(msg.contains("ed2k 链接解析失败"), "msg={msg}");
                assert!(msg.contains("MD4"), "msg={msg}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn unknown_is_unsupported() {
        assert_eq!(
            normalize_user_link("sqla://whatever"),
            NormalizedSource::Unsupported("sqla://whatever".into())
        );
    }
}
