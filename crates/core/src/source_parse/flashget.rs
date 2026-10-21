//! `flashget://`（快车链）解码器 —— chore/core-dead-code 批次抢救落位。
//!
//! 唯一来源：死代码 `core::sniffer`（735 行，0 消费者——其规则引擎方案已被
//! `source_parse` 活链取代，qqdl/fs2you/ed2k 均已在链上，唯 flashget 解码
//! 为该模块独有）。本模块只抢救这一独有能力进活链，sniffer 其余删除。
//!
//! 协议形状（公开资料 + sniffer 历史实现，L0 纯解码）：
//!   `flashget://<base64>` 解码后 = `[FLASHGET]<真实URL>[FLASHGET]`
//!   包裹符可能被历史客户端/文本嗅探剥掉——有壳无壳都接受（sniffer 同款容错）。
//!
//! 解码失败一律返回错误（调用方归一化为 Unsupported，不 panic），与
//! thunder:// / qqdl:// 家族容错口径一致（`decode_base64_lenient`）。

use crate::source_parse::thunder::decode_base64_lenient;

/// 解析 `flashget://` 链接为内层真实 URL。
///
/// 接受大小写混合 scheme、`[FLASHGET]` 包裹符存在/缺失两种形态、宽松 base64。
pub fn parse_flashget(link: &str) -> Result<String, String> {
    let rest = link
        .get("flashget://".len()..)
        .ok_or_else(|| "缺少载荷".to_string())?
        .trim();

    let decoded = decode_base64_lenient(rest).map_err(|e| format!("base64 解码失败: {e}"))?;
    let text = String::from_utf8_lossy(&decoded).into_owned();

    // 剥包裹符（有壳无壳都接受；前缀/后缀独立剥，容忍半剥形态）
    let mut inner = text.as_str();
    for pat in ["[FLASHGET]", "FLASHGET"] {
        if let Some(x) = inner.strip_prefix(pat) {
            inner = x;
            break;
        }
    }
    let mut inner = inner.trim();
    for pat in ["[FLASHGET]", "FLASHGET"] {
        if let Some(x) = inner.strip_suffix(pat) {
            inner = x.trim();
            break;
        }
    }

    if inner.is_empty() {
        return Err("解码后为空".to_string());
    }
    Ok(inner.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;

    fn enc(s: &str) -> String {
        B64.encode(s.as_bytes())
    }

    #[test]
    fn decodes_wrapped_form() {
        let link = format!(
            "flashget://{}",
            enc("[FLASHGET]http://example.com/files/a.bin[FLASHGET]")
        );
        assert_eq!(
            parse_flashget(&link).unwrap(),
            "http://example.com/files/a.bin"
        );
    }

    #[test]
    fn decodes_bare_form() {
        // 包裹符被剥掉的形态（sniffer 历史容错：文本嗅探可能剥壳）
        let link = format!("flashget://{}", enc("http://example.com/b.bin"));
        assert_eq!(parse_flashget(&link).unwrap(), "http://example.com/b.bin");
    }

    #[test]
    fn decodes_half_stripped_form() {
        // 前壳在尾壳丢（半剥形态）
        let link = format!("flashget://{}", enc("[FLASHGET]http://example.com/c.bin"));
        assert_eq!(parse_flashget(&link).unwrap(), "http://example.com/c.bin");
    }

    #[test]
    fn scheme_case_insensitive() {
        // 大小写混合 scheme：parse 按长度跳过 scheme 段，载荷保留原大小写
        // （base64 大小写敏感——全链 lowercase 会破坏载荷，正是本用例钉住的口径）
        let link = format!(
            "FLASHGET://{}",
            enc("[FLASHGET]http://x.com/d.bin[FLASHGET]")
        );
        assert_eq!(parse_flashget(&link).unwrap(), "http://x.com/d.bin");
    }

    #[test]
    fn https_inner_passes_through() {
        let link = format!(
            "flashget://{}",
            enc("[FLASHGET]https://sec.example.com/e[FLASHGET]")
        );
        assert_eq!(parse_flashget(&link).unwrap(), "https://sec.example.com/e");
    }

    #[test]
    fn bad_base64_is_err() {
        assert!(parse_flashget("flashget://!!!not-b64!!!").is_err());
    }

    #[test]
    fn empty_inner_is_err() {
        let link = format!("flashget://{}", enc(""));
        assert!(parse_flashget(&link).is_err());
    }

    #[test]
    fn missing_payload_is_err() {
        assert!(parse_flashget("flashget://").is_err());
    }
}
