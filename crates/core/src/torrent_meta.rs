//! .torrent 元数据摘要解析（B-1：magnet 抓取产物 + torrent 直传共用）。
//!
//! 基于本 crate 的 bencode 解码器（[`crate::bencode`]）做完整解析，
//! 附带 infohash 定位（SHA1(info dict 原始字节)——bencode 重编码不保证
//! 字节一致，故用原始 span 而非 decode 产物）。
//!
//! 产物 [`TorrentSummary`] 面向「任务预览 / 子文件选择 / fastresume 对账」：
//! - name（utf-8 失败按 lossy）
//! - 单文件（`length`）或多文件（`files[]`：path + size）
//! - piece 长度/数量、announce 列表（含 announce-list 扁平化）、web seeds

use crate::bencode::{self, Value};
use sha1::{Digest, Sha1};
use std::fmt;

/// .torrent 解析产物摘要。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TorrentSummary {
    /// v1 infohash（SHA1(info dict 原始字节)，40 hex 小写）。
    pub infohash_v1: String,
    /// `info.name`（多文件为目录名；utf-8 失败按 lossy）。
    pub name: String,
    /// piece 长度（`info."piece length"`）。
    pub piece_len: i64,
    /// piece 数量 = ceil(total / piece_len)。
    pub num_pieces: i64,
    /// 全部文件（单文件 = 1 项；多文件按 `files` 顺序，path 以 `/` 连接）。
    pub files: Vec<TorrentFileMeta>,
    /// 总大小（各文件 size 之和）。
    pub total_size: u64,
    /// announce + announce-list 扁平化（去重保序）。
    pub trackers: Vec<String>,
    /// url-list（BTIP-30 web seed，字符串或字符串列表两种形态均支持）。
    pub web_seeds: Vec<String>,
    /// `comment`（可选）。
    pub comment: Option<String>,
    /// `created by`（可选）。
    pub created_by: Option<String>,
}

/// 单文件元数据。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TorrentFileMeta {
    /// 相对路径（多文件以 `/` 连接 path 列表；单文件 = `info.name`）。
    pub path: String,
    /// 文件字节数。
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TorrentMetaError {
    /// bencode 解码失败。
    Decode(bencode::DecodeError),
    /// 顶层不是 dict / 缺 `info` dict。
    MissingInfo,
    /// info dict 内缺必填字段（name / piece length / pieces / length|files）。
    MissingField(&'static str),
    /// 字段类型不符。
    BadType(&'static str),
    /// path 列表为空（多文件条目）。
    EmptyPath,
}

impl fmt::Display for TorrentMetaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TorrentMetaError::Decode(e) => write!(f, "bencode 解码失败: {e}"),
            TorrentMetaError::MissingInfo => write!(f, "顶层 dict 缺 info 键"),
            TorrentMetaError::MissingField(k) => write!(f, "info dict 缺字段 {k}"),
            TorrentMetaError::BadType(k) => write!(f, "字段 {k} 类型不符"),
            TorrentMetaError::EmptyPath => write!(f, "files 条目 path 列表为空"),
        }
    }
}

impl std::error::Error for TorrentMetaError {}

/// 在原始字节中定位顶层 `info` dict 的 span（含首尾字节），返回 (start, end_inclusive)。
/// 只做最小扫描（dict 内键 `4:info` 后紧随的值），不做完整解析。
fn locate_info_span(b: &[u8]) -> Option<(usize, usize)> {
    // 顶层 dict 起点
    if b.first() != Some(&b'd') {
        return None;
    }
    let mut i = 1usize;
    while i < b.len() {
        if b[i] == b'e' {
            return None; // dict 结束前没遇到 info
        }
        // 读键（bencode 字符串）
        let (key, next) = read_str(b, i)?;
        i = next;
        if key == b"info" {
            // 值的起点就是 i，找配对结束
            let end = dict_span_end(b, i)?;
            return Some((i, end));
        }
        // 跳过该键的值（配对扫描）
        i = skip_value(b, i)?;
    }
    None
}

/// 从 `pos` 读 bencode 字符串，返回 (字节, 下一位移)。
fn read_str(b: &[u8], pos: usize) -> Option<(Vec<u8>, usize)> {
    let colon = b[pos..].iter().position(|&c| c == b':')? + pos;
    let len: usize = std::str::from_utf8(&b[pos..colon]).ok()?.parse().ok()?;
    let start = colon + 1;
    let end = start.checked_add(len)?;
    if end > b.len() {
        return None;
    }
    Some((b[start..end].to_vec(), end))
}

/// 跳过 `pos` 处的一个完整 bencode 值，返回值结束后的位移（不含）。
fn skip_value(b: &[u8], pos: usize) -> Option<usize> {
    match *b.get(pos)? {
        b'i' => {
            let e = b[pos..].iter().position(|&c| c == b'e')? + pos;
            Some(e + 1)
        }
        b'l' | b'd' => {
            // 嵌套容器统一 depth 扫描：容器头 +1、'e' -1；字符串按 len:bytes 整体跳过
            let mut depth = 0i64;
            let mut i = pos;
            while i < b.len() {
                let c = b[i];
                if c == b'l' || c == b'd' {
                    depth += 1;
                    i += 1;
                } else if c == b'e' {
                    depth -= 1;
                    i += 1;
                    if depth == 0 {
                        return Some(i);
                    }
                } else if c == b'i' {
                    let e = b[i..].iter().position(|&x| x == b'e')? + i;
                    i = e + 1;
                } else if c.is_ascii_digit() {
                    // 字符串：len:bytes
                    let (_, next) = read_str(b, i)?;
                    i = next;
                } else {
                    return None;
                }
            }
            None
        }
        c if c.is_ascii_digit() => {
            let (_, next) = read_str(b, pos)?;
            Some(next)
        }
        _ => None,
    }
}

/// dict 从 `pos`（'d'）开始的配对结束字节位（含 'e'）。
/// 注意：dict 内部的字符串键/值都走 read_str/skip_value，'e' 只在此层消费——
/// 但嵌套 dict/list 会递增 depth，故按统一 depth 扫描（与 skip_value 同构）。
fn dict_span_end(b: &[u8], pos: usize) -> Option<usize> {
    if b.get(pos) != Some(&b'd') {
        return None;
    }
    skip_value(b, pos).map(|end| end - 1) // 含 'e'
}

fn bytes_to_string(v: &[u8]) -> String {
    String::from_utf8_lossy(v).into_owned()
}

fn as_int(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i),
        _ => None,
    }
}

fn as_bytes(v: &Value) -> Option<&[u8]> {
    match v {
        Value::Bytes(b) => Some(b),
        _ => None,
    }
}

/// 收集 announce-list（嵌套 list of list of str）+ announce，去重保序。
fn collect_trackers(top: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };
    if let Some(a) = top.dict_get(b"announce").and_then(as_bytes) {
        push(bytes_to_string(a));
    }
    if let Some(Value::List(rows)) = top.dict_get(b"announce-list") {
        for row in rows {
            if let Value::List(tiers) = row {
                for t in tiers {
                    if let Some(u) = as_bytes(t) {
                        push(bytes_to_string(u));
                    }
                }
            }
        }
    }
    out
}

/// 收集 url-list（BTIP-30：单字符串或字符串列表）。
fn collect_web_seeds(top: &Value) -> Vec<String> {
    let mut out = Vec::new();
    match top.dict_get(b"url-list") {
        Some(Value::Bytes(b)) => {
            let s = bytes_to_string(b);
            if !s.is_empty() {
                out.push(s);
            }
        }
        Some(Value::List(items)) => {
            for it in items {
                if let Some(b) = as_bytes(it) {
                    let s = bytes_to_string(b);
                    if !s.is_empty() && !out.contains(&s) {
                        out.push(s);
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// 解析 .torrent 字节 → 摘要。单文件（length）/多文件（files）双形态。
pub fn parse_torrent(bytes: &[u8]) -> Result<TorrentSummary, TorrentMetaError> {
    let top = bencode::decode(bytes).map_err(TorrentMetaError::Decode)?;
    if !matches!(top, Value::Dict(_)) {
        return Err(TorrentMetaError::MissingInfo);
    }

    // infohash = SHA1(info dict 原始字节)
    let (s, e) = locate_info_span(bytes).ok_or(TorrentMetaError::MissingInfo)?;
    let mut hasher = Sha1::new();
    hasher.update(&bytes[s..=e]);
    let infohash_v1: String = hasher.finalize().iter().map(|x| format!("{x:02x}")).collect();

    let info = top.dict_get(b"info").ok_or(TorrentMetaError::MissingInfo)?;
    let Value::Dict(_) = info else {
        return Err(TorrentMetaError::BadType("info"));
    };

    let name_v = info.dict_get(b"name").and_then(as_bytes).ok_or(TorrentMetaError::MissingField("name"))?;
    let name = bytes_to_string(name_v);
    let piece_len = as_int(info.dict_get(b"piece length").ok_or(TorrentMetaError::MissingField("piece length"))?)
        .ok_or(TorrentMetaError::BadType("piece length"))?;
    let pieces = as_bytes(info.dict_get(b"pieces").ok_or(TorrentMetaError::MissingField("pieces"))?)
        .ok_or(TorrentMetaError::BadType("pieces"))?;
    let num_pieces = pieces.len() as i64 / 20;

    let mut files = Vec::new();
    if let Some(len) = info.dict_get(b"length") {
        let size = as_int(len).ok_or(TorrentMetaError::BadType("length"))?;
        files.push(TorrentFileMeta { path: name.clone(), size: size.max(0) as u64 });
    } else if let Some(Value::List(entries)) = info.dict_get(b"files") {
        for entry in entries {
            let Value::Dict(_) = entry else {
                return Err(TorrentMetaError::BadType("files[]"));
            };
            let size = as_int(entry.dict_get(b"length").ok_or(TorrentMetaError::MissingField("files[].length"))?)
                .ok_or(TorrentMetaError::BadType("files[].length"))?;
            let Value::List(parts) =
                entry.dict_get(b"path").ok_or(TorrentMetaError::MissingField("files[].path"))?
            else {
                return Err(TorrentMetaError::BadType("files[].path"));
            };
            if parts.is_empty() {
                return Err(TorrentMetaError::EmptyPath);
            }
            let segs: Vec<String> = parts
                .iter()
                .filter_map(|p| as_bytes(p).map(bytes_to_string))
                .collect();
            files.push(TorrentFileMeta {
                path: segs.join("/"),
                size: size.max(0) as u64,
            });
        }
    } else {
        return Err(TorrentMetaError::MissingField("length|files"));
    }

    let total_size = files.iter().map(|f| f.size).sum();
    Ok(TorrentSummary {
        infohash_v1,
        name,
        piece_len,
        num_pieces,
        files,
        total_size,
        trackers: collect_trackers(&top),
        web_seeds: collect_web_seeds(&top),
        comment: top.dict_get(b"comment").and_then(as_bytes).map(bytes_to_string),
        created_by: top.dict_get(b"created by").and_then(as_bytes).map(bytes_to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bencode::Value;

    /// 手工构造最小单文件 .torrent（name/length/piece length/pieces/announce）。
    fn single_file_torrent() -> Vec<u8> {
        let info = Value::Dict(vec![
            (b"length".to_vec(), Value::Int(2048)),
            (b"name".to_vec(), Value::Bytes(b"test.iso".to_vec())),
            (b"piece length".to_vec(), Value::Int(16384)),
            (b"pieces".to_vec(), Value::Bytes(vec![0xAB; 20])),
        ]);
        Value::Dict(vec![
            (b"announce".to_vec(), Value::Bytes(b"http://tracker/a".to_vec())),
            (b"info".to_vec(), info),
        ])
        .into_bencode()
    }

    /// 多文件 + announce-list + url-list + comment/created by。
    fn multi_file_torrent() -> Vec<u8> {
        let info = Value::Dict(vec![
            (b"files".to_vec(), Value::List(vec![
                Value::Dict(vec![
                    (b"length".to_vec(), Value::Int(100)),
                    (b"path".to_vec(), Value::List(vec![
                        Value::Bytes(b"sub".to_vec()),
                        Value::Bytes(b"a.txt".to_vec()),
                    ])),
                ]),
                Value::Dict(vec![
                    (b"length".to_vec(), Value::Int(250)),
                    (b"path".to_vec(), Value::List(vec![
                        Value::Bytes(b"b.bin".to_vec()),
                    ])),
                ]),
            ])),
            (b"name".to_vec(), Value::Bytes(b"pkg".to_vec())),
            (b"piece length".to_vec(), Value::Int(32768)),
            (b"pieces".to_vec(), Value::Bytes(vec![0xCD; 40])),
        ]);
        Value::Dict(vec![
            (b"announce".to_vec(), Value::Bytes(b"http://tracker/1".to_vec())),
            (b"announce-list".to_vec(), Value::List(vec![
                Value::List(vec![Value::Bytes(b"http://tracker/1".to_vec())]),
                Value::List(vec![Value::Bytes(b"http://tracker/2".to_vec())]),
            ])),
            (b"comment".to_vec(), Value::Bytes(b"hello".to_vec())),
            (b"created by".to_vec(), Value::Bytes(b"smart-dl test".to_vec())),
            (b"info".to_vec(), info),
            (b"url-list".to_vec(), Value::List(vec![
                Value::Bytes(b"https://ws/1".to_vec()),
                Value::Bytes(b"https://ws/2".to_vec()),
            ])),
        ])
        .into_bencode()
    }

    impl Value {
        fn into_bencode(self) -> Vec<u8> {
            crate::bencode::encode(&self)
        }
    }

    #[test]
    fn single_file_summary_ok() {
        let bytes = single_file_torrent();
        let s = parse_torrent(&bytes).unwrap();
        assert_eq!(s.name, "test.iso");
        assert_eq!(s.files, vec![TorrentFileMeta { path: "test.iso".into(), size: 2048 }]);
        assert_eq!(s.total_size, 2048);
        assert_eq!(s.piece_len, 16384);
        assert_eq!(s.num_pieces, 1);
        assert_eq!(s.trackers, vec!["http://tracker/a"]);
        assert!(s.web_seeds.is_empty());
        assert_eq!(s.infohash_v1.len(), 40);
        assert!(s.infohash_v1.bytes().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn multi_file_summary_ok() {
        let bytes = multi_file_torrent();
        let s = parse_torrent(&bytes).unwrap();
        assert_eq!(s.name, "pkg");
        assert_eq!(s.files.len(), 2);
        assert_eq!(s.files[0].path, "sub/a.txt");
        assert_eq!(s.files[0].size, 100);
        assert_eq!(s.files[1].path, "b.bin");
        assert_eq!(s.total_size, 350);
        assert_eq!(s.num_pieces, 2);
        // announce + announce-list 去重保序
        assert_eq!(s.trackers, vec!["http://tracker/1", "http://tracker/2"]);
        assert_eq!(s.web_seeds, vec!["https://ws/1", "https://ws/2"]);
        assert_eq!(s.comment.as_deref(), Some("hello"));
        assert_eq!(s.created_by.as_deref(), Some("smart-dl test"));
    }

    #[test]
    fn url_list_single_string_form_ok() {
        let info = Value::Dict(vec![
            (b"length".to_vec(), Value::Int(1)),
            (b"name".to_vec(), Value::Bytes(b"x".to_vec())),
            (b"piece length".to_vec(), Value::Int(16384)),
            (b"pieces".to_vec(), Value::Bytes(vec![0u8; 20])),
        ]);
        let bytes = Value::Dict(vec![
            (b"info".to_vec(), info),
            (b"url-list".to_vec(), Value::Bytes(b"https://only/one".to_vec())),
        ])
        .into_bencode();
        let s = parse_torrent(&bytes).unwrap();
        assert_eq!(s.web_seeds, vec!["https://only/one"]);
    }

    #[test]
    fn infohash_matches_daemon_locate_algo() {
        // 与 daemon::state::torrent_infohash 同一定义：SHA1(info dict 原始字节)
        let bytes = single_file_torrent();
        let s = parse_torrent(&bytes).unwrap();
        // 用独立实现交叉验证（daemon 侧算法的等价重放）
        let (start, end) = locate_info_span(&bytes).unwrap();
        let mut h = Sha1::new();
        h.update(&bytes[start..=end]);
        let expect: String = h.finalize().iter().map(|x| format!("{x:02x}")).collect();
        assert_eq!(s.infohash_v1, expect);
    }

    #[test]
    fn missing_info_rejected() {
        let bytes = Value::Dict(vec![(b"announce".to_vec(), Value::Bytes(b"x".to_vec()))]).into_bencode();
        assert_eq!(parse_torrent(&bytes).unwrap_err(), TorrentMetaError::MissingInfo);
    }

    #[test]
    fn garbage_rejected() {
        assert!(matches!(parse_torrent(b"not-bencode"), Err(TorrentMetaError::Decode(_))));
        assert_eq!(parse_torrent(b"i5e").unwrap_err(), TorrentMetaError::MissingInfo);
    }

    #[test]
    fn missing_name_rejected() {
        let info = Value::Dict(vec![
            (b"length".to_vec(), Value::Int(1)),
            (b"piece length".to_vec(), Value::Int(16384)),
            (b"pieces".to_vec(), Value::Bytes(vec![0u8; 20])),
        ]);
        let bytes = Value::Dict(vec![(b"info".to_vec(), info)]).into_bencode();
        assert_eq!(
            parse_torrent(&bytes).unwrap_err(),
            TorrentMetaError::MissingField("name")
        );
    }

    #[test]
    fn empty_path_rejected() {
        let info = Value::Dict(vec![
            (b"files".to_vec(), Value::List(vec![Value::Dict(vec![
                (b"length".to_vec(), Value::Int(1)),
                (b"path".to_vec(), Value::List(vec![])),
            ])])),
            (b"name".to_vec(), Value::Bytes(b"d".to_vec())),
            (b"piece length".to_vec(), Value::Int(16384)),
            (b"pieces".to_vec(), Value::Bytes(vec![0u8; 20])),
        ]);
        let bytes = Value::Dict(vec![(b"info".to_vec(), info)]).into_bencode();
        assert_eq!(parse_torrent(&bytes).unwrap_err(), TorrentMetaError::EmptyPath);
    }

    #[test]
    fn locate_span_handles_nested_and_trailing_keys() {
        // info 之前有嵌套 list 值、之后还有别的键——定位须精确
        let bytes = multi_file_torrent();
        let (s, e) = locate_info_span(&bytes).unwrap();
        // span 应以 'd' 开头 'e' 结尾且可独立解码
        assert_eq!(bytes[s], b'd');
        assert_eq!(bytes[e], b'e');
        let inner = crate::bencode::decode(&bytes[s..=e]).unwrap();
        assert!(inner.dict_get(b"name").is_some());
    }
}
