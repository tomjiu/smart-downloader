//! magnet URI 解析（B-1：magnet → torrent 元数据抓取的输入面）。
//!
//! 契约（v1）：
//! - 仅支持 v1 infohash（`xt=urn:btih:` + 40 hex）；v2-only（`urn:btmh:` / 32 位
//!   base32）显式报错 [`MagnetError::UnsupportedV2`]——主线 libtorrent 面为 v1。
//! - 多个 `xt` 时取第一个合法 v1（其余忽略；混合 v1+v2 的 hybrid magnet 取 v1）。
//! - `dn`（display name）/ `tr`（tracker，多值）/ `ws`（web seed，多值）均
//!   percent-decode（`+` 不转空格——BT 惯例与 RFC 3986 一致，query 空格应编码
//!   为 `%20`；宽容处理：`+` 仅在 dn 中按空格解，tracker/web seed 不动）。
//! - 校验失败（非 magnet 前缀 / 无 xt / btih 非法）→ [`MagnetError`]，不静默。

use std::fmt;

/// magnet 解析产物（B-1 抓取流程输入）。
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MagnetInfo {
    /// v1 infohash（40 hex 小写）。
    pub infohash: String,
    /// `dn=` display name（未提供 → None）。
    pub display_name: Option<String>,
    /// `tr=` tracker announce URL 列表（重复去重、保序）。
    pub trackers: Vec<String>,
    /// `ws=` web seed（HTTP(S) 直链）列表（重复去重、保序）。
    pub web_seeds: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MagnetError {
    /// 不是 magnet URI（缺 `magnet:?` 前缀）。
    NotMagnet,
    /// 无 `xt=urn:btih:` 参数。
    MissingXt,
    /// xt 存在但 btih 非 40 hex（含 32 位 base32 v1 表示——统一要求 40 hex）。
    BadInfohash(String),
    /// v2-only（btmh multihash，无 v1 hex xt）。
    UnsupportedV2,
    /// percent-decode 失败（尾随 `%` 或非 hex 转义）。
    BadPercentEncoding(String),
}

impl fmt::Display for MagnetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MagnetError::NotMagnet => write!(f, "不是 magnet URI（缺 magnet:? 前缀）"),
            MagnetError::MissingXt => write!(f, "magnet 缺 xt=urn:btih: 参数"),
            MagnetError::BadInfohash(v) => {
                write!(f, "btih infohash 非法（须 40 hex）: {v}")
            }
            MagnetError::UnsupportedV2 => {
                write!(f, "v2-only magnet（urn:btmh:）暂不支持：主线为 v1")
            }
            MagnetError::BadPercentEncoding(v) => write!(f, "percent-decode 失败: {v}"),
        }
    }
}

impl std::error::Error for MagnetError {}

/// percent-decode（RFC 3986）。`plus_as_space=true` 仅用于 `dn=`（表单习惯宽容）。
fn pct_decode(s: &str, plus_as_space: bool) -> Result<String, MagnetError> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                // 须有两个 hex 位跟随；缺失/非 hex → 显式报错
                let h1 = *b
                    .get(i + 1)
                    .ok_or_else(|| MagnetError::BadPercentEncoding(s.to_string()))?;
                let h2 = *b
                    .get(i + 2)
                    .ok_or_else(|| MagnetError::BadPercentEncoding(s.to_string()))?;
                let hi =
                    hex_val(h1).ok_or_else(|| MagnetError::BadPercentEncoding(s.to_string()))?;
                let lo =
                    hex_val(h2).ok_or_else(|| MagnetError::BadPercentEncoding(s.to_string()))?;
                out.push(hi * 16 + lo);
                i += 3;
            }
            b'+' if plus_as_space => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| MagnetError::BadPercentEncoding(s.to_string()))
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn is_hex40(v: &str) -> bool {
    v.len() == 40 && v.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 解析 magnet URI。多个 xt 取第一个合法 v1；v2-only 显式报错。
pub fn parse_magnet(uri: &str) -> Result<MagnetInfo, MagnetError> {
    let rest = uri.strip_prefix("magnet:?").ok_or(MagnetError::NotMagnet)?;
    if rest.is_empty() {
        return Err(MagnetError::MissingXt);
    }

    let mut info = MagnetInfo::default();
    let mut seen_btih = false;
    let mut seen_btmh = false;
    let mut seen_dn = false;
    let mut seen_tr = std::collections::HashSet::new();
    let mut seen_ws = std::collections::HashSet::new();

    for pair in rest.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            // 无值参数（如裸 `tr`）：按空值跳过，不致整体失败
            None => continue,
        };
        match k {
            "xt" => {
                if let Some(btih) = v.strip_prefix("urn:btih:") {
                    if is_hex40(btih) {
                        if !seen_btih {
                            // 多 xt 取第一个合法 v1（hybrid magnet 的 v1 优先语义）
                            info.infohash = btih.to_ascii_lowercase();
                            seen_btih = true;
                        }
                    } else {
                        return Err(MagnetError::BadInfohash(v.to_string()));
                    }
                } else if v.starts_with("urn:btmh:") {
                    seen_btmh = true;
                }
                // 其他 URN 命名空间（ed2k 等）：忽略
            }
            "dn" if !seen_dn => {
                info.display_name = Some(pct_decode(v, true)?);
                seen_dn = true;
            }
            "tr" => {
                let dec = pct_decode(v, false)?;
                if !dec.is_empty() && seen_tr.insert(dec.clone()) {
                    info.trackers.push(dec);
                }
            }
            "ws" => {
                let dec = pct_decode(v, false)?;
                if !dec.is_empty() && seen_ws.insert(dec.clone()) {
                    info.web_seeds.push(dec);
                }
            }
            _ => {} // xl/xt 其他命名空间/未知参数：忽略（向前兼容）
        }
    }

    if !seen_btih && seen_btmh {
        return Err(MagnetError::UnsupportedV2);
    }
    if !seen_btih {
        return Err(MagnetError::MissingXt);
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IH: &str = "0d2c9c9d5c2d3e8f9a1b2c3d4e5f6a7b8c9d0e1f";

    #[test]
    fn minimal_v1_ok() {
        let m = parse_magnet(&format!("magnet:?xt=urn:btih:{IH}")).unwrap();
        assert_eq!(m.infohash, IH);
        assert_eq!(m.display_name, None);
        assert!(m.trackers.is_empty());
        assert!(m.web_seeds.is_empty());
    }

    #[test]
    fn full_fields_ok() {
        let uri = format!(
            "magnet:?xt=urn:btih:{}&dn=Ubuntu%2022.04&tr=http%3A%2F%2Ftracker.example%2Fannounce&tr=http%3A%2F%2Ft2%2Fa&ws=https%3A%2F%2Fseed.example%2Ff.iso",
            IH.to_uppercase()
        );
        let m = parse_magnet(&uri).unwrap();
        assert_eq!(m.infohash, IH, "大写 infohash 归一为小写");
        assert_eq!(m.display_name.as_deref(), Some("Ubuntu 22.04"));
        assert_eq!(m.trackers.len(), 2);
        assert_eq!(m.trackers[0], "http://tracker.example/announce");
        assert_eq!(m.web_seeds, vec!["https://seed.example/f.iso"]);
    }

    #[test]
    fn hybrid_v1_v2_takes_v1() {
        // BitTorrent v2 hybrid magnet：v1 hex xt 在前，v2 multihash 在后
        let uri = format!(
            "magnet:?xt=urn:btih:{IH}&xt=urn:btmh:1220abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        );
        let m = parse_magnet(&uri).unwrap();
        assert_eq!(m.infohash, IH);
    }

    #[test]
    fn v2_only_rejected() {
        let uri = "magnet:?xt=urn:btmh:1220abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        assert_eq!(parse_magnet(uri).unwrap_err(), MagnetError::UnsupportedV2);
    }

    #[test]
    fn base32_infohash_rejected() {
        // 32 位 base32（另一 v1 表示）：主线统一 40 hex，显式拒绝
        let uri = "magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        assert!(matches!(
            parse_magnet(uri).unwrap_err(),
            MagnetError::BadInfohash(_)
        ));
    }

    #[test]
    fn short_hash_rejected() {
        let uri = "magnet:?xt=urn:btih:abc123";
        assert!(matches!(
            parse_magnet(uri).unwrap_err(),
            MagnetError::BadInfohash(_)
        ));
    }

    #[test]
    fn no_xt_rejected() {
        assert_eq!(
            parse_magnet("magnet:?dn=only-name").unwrap_err(),
            MagnetError::MissingXt
        );
    }

    #[test]
    fn not_magnet_rejected() {
        assert_eq!(
            parse_magnet("https://example.com/f.iso").unwrap_err(),
            MagnetError::NotMagnet
        );
        assert_eq!(parse_magnet("").unwrap_err(), MagnetError::NotMagnet);
    }

    #[test]
    fn bad_percent_encoding_rejected() {
        let uri = format!("magnet:?xt=urn:btih:{IH}&dn=%ZZ");
        assert!(matches!(
            parse_magnet(&uri).unwrap_err(),
            MagnetError::BadPercentEncoding(_)
        ));
    }

    #[test]
    fn duplicate_trackers_deduped_and_empty_ignored() {
        let uri = format!(
            "magnet:?xt=urn:btih:{IH}&tr=http%3A%2F%2Ft%2Fa&tr=http%3A%2F%2Ft%2Fa&tr=&tr=http%3A%2F%2Ft%2Fb"
        );
        let m = parse_magnet(&uri).unwrap();
        assert_eq!(m.trackers, vec!["http://t/a", "http://t/b"]);
    }

    #[test]
    fn valueless_params_skipped() {
        // 无值参数（bare `tr` / 未知键）不致整体失败
        let uri = format!("magnet:?xt=urn:btih:{IH}&tr&foo");
        let m = parse_magnet(&uri).unwrap();
        assert_eq!(m.infohash, IH);
        assert!(m.trackers.is_empty());
    }

    #[test]
    fn plus_in_dn_decodes_as_space() {
        let uri = format!("magnet:?xt=urn:btih:{IH}&dn=a+b%20c");
        let m = parse_magnet(&uri).unwrap();
        assert_eq!(m.display_name.as_deref(), Some("a b c"));
    }

    #[test]
    fn plus_in_tracker_not_space() {
        let uri = format!("magnet:?xt=urn:btih:{IH}&tr=http%3A%2F%2Ft%2Fa%2Bb");
        let m = parse_magnet(&uri).unwrap();
        assert_eq!(m.trackers, vec!["http://t/a+b"]);
    }

    #[test]
    fn utf8_dn_ok() {
        let uri = format!("magnet:?xt=urn:btih:{IH}&dn=%E5%8B%87%E6%B0%94");
        let m = parse_magnet(&uri).unwrap();
        assert_eq!(m.display_name.as_deref(), Some("勇气"));
    }

    #[test]
    fn multiple_v1_xt_takes_first() {
        let ih2 = "1111111111111111111111111111111111111111";
        let uri = format!("magnet:?xt=urn:btih:{IH}&xt=urn:btih:{ih2}");
        let m = parse_magnet(&uri).unwrap();
        assert_eq!(m.infohash, IH);
    }
}
