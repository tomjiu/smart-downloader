//! ed2k:// 链接解析（eD2k 文件链接，Task 5-a T3）。
//!
//! 支持格式（eD2k URI 规范）：
//! - 标准文件链接：`ed2k://|file|<名称>|<字节数>|<MD4 十六进制>|/`
//! - 带分段哈希变体：`ed2k://|file|<名称>|<字节数>|<MD4>|h=<SEGMENT-HASH>|/`
//!   （`h=` 可重复出现——多分段文件的 hash set；其余 `key=value` 字段
//!   如 `s=`（HTTP 源）/`p=`（根哈希）宽容忽略）
//!
//! 解析容错：
//! - scheme/`file`/`h` 键名大小写不敏感；MD4 十六进制大小写均可（统一存小写）
//! - 首尾空白容忍；名称做百分号解码（支持中文文件名 URL 编码）
//! - 尾部 `/` 可省略；多余的空字段跳过
//!
//! 路由决策（BACKLOG C 段远期）：ed2k **已识别但暂不支持下载**——
//! 解析结果仅作为元数据（名称/大小/MD4）用于可读报错，完整 eMule/eDonkey
//! 引擎为远期专项，不在此实现。

/// ed2k 文件链接解析结果（结构化元数据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ed2kLink {
    /// 文件名（百分号解码后，非空）。
    pub name: String,
    /// 文件大小（字节，十进制）。
    pub size: u64,
    /// MD4 哈希（32 位十六进制，统一小写；仅存字符串，不做计算）。
    pub md4: String,
    /// `h=` 分段哈希列表（按出现顺序；多分段文件可有多个）。
    pub segment_hashes: Vec<String>,
}

/// ed2k 链接解析错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Ed2kError {
    #[error("缺少 ed2k:// 前缀")]
    MissingPrefix,
    #[error("仅支持 ed2k 文件链接（|file|），不支持该类型: {0}")]
    UnsupportedKind(String),
    #[error("字段不足（需要 |file|名称|大小|md4）")]
    MissingFields,
    #[error("文件大小非法（需十进制字节数）: {0}")]
    InvalidSize(String),
    #[error("MD4 非法（需 32 位十六进制，实际 {1} 位）: {0}")]
    InvalidMd4(String, usize),
    #[error("无法识别的字段: {0}")]
    UnknownField(String),
}

/// 解析 ed2k:// 文件链接 → [`Ed2kLink`]。
///
/// # 示例
/// ```
/// use smart_dl_core::source_parse::ed2k::parse_ed2k;
/// let l = parse_ed2k("ed2k://|file|a.bin|1024|0123456789ABCDEF0123456789ABCDEF|/").unwrap();
/// assert_eq!(l.size, 1024);
/// assert_eq!(l.md4, "0123456789abcdef0123456789abcdef");
/// ```
pub fn parse_ed2k(link: &str) -> Result<Ed2kLink, Ed2kError> {
    let link = link.trim();
    let scheme = link.get(..7).ok_or(Ed2kError::MissingPrefix)?;
    if !scheme.eq_ignore_ascii_case("ed2k://") {
        return Err(Ed2kError::MissingPrefix);
    }
    let rest = &link[7..];

    let mut segs = rest.split('|');
    // 容忍缺省的首个 '|'（ed2k://file|... 与 ed2k://|file|... 等价）
    let mut kind_seg = segs.next().unwrap_or("");
    if kind_seg.is_empty() {
        kind_seg = segs.next().unwrap_or("");
    }
    let kind_seg = kind_seg.trim();
    if kind_seg.is_empty() {
        return Err(Ed2kError::MissingFields);
    }
    if !kind_seg.eq_ignore_ascii_case("file") {
        return Err(Ed2kError::UnsupportedKind(kind_seg.to_string()));
    }

    let name_raw = segs.next().unwrap_or("").trim();
    if name_raw.is_empty() {
        return Err(Ed2kError::MissingFields);
    }
    let size_raw = segs.next().unwrap_or("").trim();
    if size_raw.is_empty() {
        return Err(Ed2kError::MissingFields);
    }
    let size: u64 = size_raw
        .parse()
        .map_err(|_| Ed2kError::InvalidSize(size_raw.to_string()))?;
    let md4_raw = segs.next().unwrap_or("").trim();
    if md4_raw.is_empty() {
        return Err(Ed2kError::MissingFields);
    }
    if !is_md4(md4_raw) {
        return Err(Ed2kError::InvalidMd4(
            md4_raw.to_string(),
            md4_raw.chars().count(),
        ));
    }
    let md4 = md4_raw.to_ascii_lowercase();
    let name = percent_decode(name_raw);

    // 尾部字段：h= 分段哈希收集；其他 key=value（s=/p= 等）宽容忽略；`/` 终止符跳过
    let mut segment_hashes = Vec::new();
    for seg in segs {
        let seg = seg.trim();
        if seg.is_empty() || seg == "/" {
            continue;
        }
        match seg.split_once('=') {
            Some((k, v)) if k.eq_ignore_ascii_case("h") => {
                if v.trim().is_empty() {
                    return Err(Ed2kError::UnknownField(seg.to_string()));
                }
                segment_hashes.push(v.trim().to_string());
            }
            Some((_other_key, _)) => {} // s=/p=/k= 等扩展字段：宽容忽略
            None => return Err(Ed2kError::UnknownField(seg.to_string())),
        }
    }

    Ok(Ed2kLink {
        name,
        size,
        md4,
        segment_hashes,
    })
}

/// MD4 合法性：恰好 32 位十六进制字符。
fn is_md4(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 百分号解码（%XX → 字节；非 UTF-8 序列 lossy 处理；`+` 不转空格——URI 路径语义）。
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h = hex_val(bytes[i + 1]);
            let l = hex_val(bytes[i + 2]);
            if let (Some(h), Some(l)) = (h, l) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD4: &str = "0123456789abcdef0123456789abcdef";
    const MD4_UP: &str = "0123456789ABCDEF0123456789ABCDEF";

    // ---------- 合法链接 ----------

    #[test]
    fn standard_link_parses() {
        let l = parse_ed2k(&format!("ed2k://|file|a.bin|1024|{MD4}|/")).unwrap();
        assert_eq!(l.name, "a.bin");
        assert_eq!(l.size, 1024);
        assert_eq!(l.md4, MD4);
        assert!(l.segment_hashes.is_empty());
    }

    #[test]
    fn scheme_and_md4_case_insensitive() {
        // scheme 大写 + md4 大写 → 统一解析，md4 存小写
        let l = parse_ed2k(&format!("ED2K://|FILE|a.bin|1|{MD4_UP}|/")).unwrap();
        assert_eq!(l.name, "a.bin");
        assert_eq!(l.md4, MD4);
    }

    #[test]
    fn surrounding_whitespace_tolerated() {
        let l = parse_ed2k(&format!("  ed2k://|file|a.bin|1|{MD4}|/  ")).unwrap();
        assert_eq!(l.size, 1);
    }

    #[test]
    fn trailing_slash_optional() {
        let l = parse_ed2k(&format!("ed2k://|file|a.bin|1|{MD4}")).unwrap();
        assert_eq!(l.md4, MD4);
    }

    #[test]
    fn h_param_segment_hash_collected() {
        let l = parse_ed2k(&format!(
            "ed2k://|file|a.bin|1|{MD4}|h=PHNXXWUXGKZYXTCSSX2YCVAN5BW6PFTJ|/"
        ))
        .unwrap();
        assert_eq!(l.segment_hashes, vec!["PHNXXWUXGKZYXTCSSX2YCVAN5BW6PFTJ"]);
    }

    #[test]
    fn multiple_h_params_collected_in_order() {
        let l = parse_ed2k(&format!(
            "ed2k://|file|a.bin|1|{MD4}|h=AAAAA23456789012345678901234567|h=BBBBB23456789012345678901234567|/"
        ))
        .unwrap();
        assert_eq!(l.segment_hashes.len(), 2);
        assert_eq!(l.segment_hashes[0], "AAAAA23456789012345678901234567");
    }

    #[test]
    fn chinese_name_percent_decoded() {
        // "电影.mkv" 的 UTF-8 百分号编码
        let l = parse_ed2k(&format!(
            "ed2k://|file|%E7%94%B5%E5%BD%B1.mkv|2048|{MD4}|/"
        ))
        .unwrap();
        assert_eq!(l.name, "电影.mkv");
        assert_eq!(l.size, 2048);
    }

    #[test]
    fn name_with_spaces_and_plus_preserved() {
        // 名称含 %20 空格；'+' 属于名字本身（URI 路径语义，不转空格）
        let l = parse_ed2k(&format!("ed2k://|file|my%20file+2.bin|1|{MD4}|/")).unwrap();
        assert_eq!(l.name, "my file+2.bin");
    }

    #[test]
    fn extra_source_field_ignored() {
        // s=（HTTP 源）等扩展字段宽容忽略
        let l =
            parse_ed2k(&format!("ed2k://|file|a.bin|1|{MD4}|s=http://x/a.bin|/")).unwrap();
        assert_eq!(l.md4, MD4);
        assert!(l.segment_hashes.is_empty());
    }

    #[test]
    fn leading_pipe_optional() {
        // 宽容：ed2k://file|... （缺首个 |）
        let l = parse_ed2k(&format!("ed2k://file|a.bin|1|{MD4}|/")).unwrap();
        assert_eq!(l.name, "a.bin");
    }

    // ---------- 非法链接 ----------

    #[test]
    fn missing_prefix_rejected() {
        assert_eq!(parse_ed2k("http://x"), Err(Ed2kError::MissingPrefix));
        assert_eq!(parse_ed2k(""), Err(Ed2kError::MissingPrefix));
    }

    #[test]
    fn non_file_kind_rejected() {
        assert_eq!(
            parse_ed2k("ed2k://|server|1.2.3.4|4661|/"),
            Err(Ed2kError::UnsupportedKind("server".into()))
        );
    }

    #[test]
    fn missing_fields_rejected() {
        assert_eq!(parse_ed2k("ed2k://|file|"), Err(Ed2kError::MissingFields));
        assert_eq!(parse_ed2k("ed2k://|file|a.bin|"), Err(Ed2kError::MissingFields));
        assert_eq!(
            parse_ed2k(&format!("ed2k://|file|a.bin|1|")),
            Err(Ed2kError::MissingFields)
        );
    }

    #[test]
    fn bad_size_rejected() {
        assert_eq!(
            parse_ed2k(&format!("ed2k://|file|a.bin|abc|{MD4}|/")),
            Err(Ed2kError::InvalidSize("abc".into()))
        );
        // 负数/溢出同样拒绝
        assert!(matches!(
            parse_ed2k(&format!("ed2k://|file|a.bin|-5|{MD4}|/")),
            Err(Ed2kError::InvalidSize(_))
        ));
        assert!(matches!(
            parse_ed2k(&format!("ed2k://|file|a.bin|99999999999999999999999|{MD4}|/")),
            Err(Ed2kError::InvalidSize(_))
        ));
    }

    #[test]
    fn bad_md4_length_rejected() {
        // 31 位
        assert_eq!(
            parse_ed2k(&format!("ed2k://|file|a.bin|1|{}|/", &MD4[..31])),
            Err(Ed2kError::InvalidMd4(MD4[..31].to_string(), 31))
        );
        // 33 位
        assert_eq!(
            parse_ed2k(&format!("ed2k://|file|a.bin|1|{MD4}a|/")),
            Err(Ed2kError::InvalidMd4(format!("{MD4}a"), 33))
        );
        // 空串
        assert_eq!(
            parse_ed2k("ed2k://|file|a.bin|1||/"),
            Err(Ed2kError::MissingFields)
        );
    }

    #[test]
    fn non_hex_md4_rejected() {
        // 长度对但含非十六进制字符（h= 出现在 md4 槽位，历史测试样例）
        assert!(matches!(
            parse_ed2k("ed2k://|file|x|1|h=abc|/"),
            Err(Ed2kError::InvalidMd4(_, 5))
        ));
    }

    #[test]
    fn unknown_field_rejected() {
        assert!(matches!(
            parse_ed2k(&format!("ed2k://|file|a.bin|1|{MD4}|garbage|/")),
            Err(Ed2kError::UnknownField(_))
        ));
    }

    #[test]
    fn empty_h_value_rejected() {
        assert!(matches!(
            parse_ed2k(&format!("ed2k://|file|a.bin|1|{MD4}|h=|/")),
            Err(Ed2kError::UnknownField(_))
        ));
    }
}
