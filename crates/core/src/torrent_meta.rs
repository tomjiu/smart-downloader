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
    /// piece length 取值非法（≤ 0 或 > 16 MiB）。
    /// P0-12 修复：原实现只校验类型不校验取值，0/-5 均被接受——
    /// core 的 parse_torrent 是公开 API（任务预览/子文件选择门面），
    /// 非法值会传染给下游除法/偏移计算。上限与 libtorrent 一致（1<<24）。
    InvalidPieceLength(i64),
    /// pieces 长度不是 20 的倍数（余数不得静默丢弃——与 daemon 侧口径统一）。
    BadPiecesLen(usize),
    /// 各文件 size 求和溢出 u64（P0-11：回绕后预检被绕过）。
    TotalSizeOverflow,
}

impl fmt::Display for TorrentMetaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TorrentMetaError::Decode(e) => write!(f, "bencode 解码失败: {e}"),
            TorrentMetaError::MissingInfo => write!(f, "顶层 dict 缺 info 键"),
            TorrentMetaError::MissingField(k) => write!(f, "info dict 缺字段 {k}"),
            TorrentMetaError::BadType(k) => write!(f, "字段 {k} 类型不符"),
            TorrentMetaError::EmptyPath => write!(f, "files 条目 path 列表为空"),
            TorrentMetaError::InvalidPieceLength(v) => {
                write!(f, "piece length 取值非法（须 1..={}，得到 {v}）", 1 << 24)
            }
            TorrentMetaError::BadPiecesLen(n) => {
                write!(f, "pieces 长度 {n} 不是 20 的倍数")
            }
            TorrentMetaError::TotalSizeOverflow => {
                write!(f, "各文件 size 求和溢出 u64")
            }
        }
    }
}

impl std::error::Error for TorrentMetaError {}

fn bytes_to_string(v: &[u8]) -> String {
    String::from_utf8_lossy(v).into_owned()
}

/// 收集 announce-list（嵌套 list of list of str）+ announce，去重保序。
fn collect_trackers(top: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };
    if let Some(a) = top.dict_get(b"announce").and_then(Value::as_bytes) {
        push(bytes_to_string(a));
    }
    if let Some(Value::List(rows)) = top.dict_get(b"announce-list") {
        for row in rows {
            if let Value::List(tiers) = row {
                for t in tiers {
                    if let Some(u) = t.as_bytes() {
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
                if let Some(b) = it.as_bytes() {
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

/// .torrent 的 v1 infohash（40 hex 小写）= SHA1(info dict 原始字节)。
///
/// TD-4 建议 8 随迁：原 daemon `state::torrent_infohash` 的核心逻辑归位
/// core（btcore 引擎适配器与 daemon 快照面共用同一实现）。
/// 解析失败（无法定位 info / 畸形）→ `None`。
pub fn infohash_v1(bytes: &[u8]) -> Option<String> {
    let (s, e) = bencode::locate_dict_value(bytes, b"info")?;
    if bytes.get(s) != Some(&b'd') {
        return None; // info 必须是 dict
    }
    let mut hasher = Sha1::new();
    hasher.update(&bytes[s..=e]);
    Some(
        hasher
            .finalize()
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect(),
    )
}

/// 解析 .torrent 字节 → 摘要。单文件（length）/多文件（files）双形态。
pub fn parse_torrent(bytes: &[u8]) -> Result<TorrentSummary, TorrentMetaError> {
    let top = bencode::decode(bytes).map_err(TorrentMetaError::Decode)?;
    if !matches!(top, Value::Dict(_)) {
        return Err(TorrentMetaError::MissingInfo);
    }

    // infohash = SHA1(info dict 原始字节)
    let infohash_v1 = infohash_v1(bytes).ok_or(TorrentMetaError::MissingInfo)?;

    let info = top.dict_get(b"info").ok_or(TorrentMetaError::MissingInfo)?;
    let Value::Dict(_) = info else {
        return Err(TorrentMetaError::BadType("info"));
    };

    let name_v = info
        .dict_get(b"name")
        .and_then(Value::as_bytes)
        .ok_or(TorrentMetaError::MissingField("name"))?;
    let name = bytes_to_string(name_v);
    let piece_len = info
        .dict_get(b"piece length")
        .ok_or(TorrentMetaError::MissingField("piece length"))?
        .as_int()
        .ok_or(TorrentMetaError::BadType("piece length"))?;
    // P0-12 修复：取值校验（原实现仅校验类型，0/-5 被静默接受）。
    // 上限 16 MiB 与 libtorrent 一致；越界值会在下游偏移计算中溢出回绕。
    const MAX_PIECE_LEN: i64 = 1 << 24;
    if !(1..=MAX_PIECE_LEN).contains(&piece_len) {
        return Err(TorrentMetaError::InvalidPieceLength(piece_len));
    }
    let pieces = info
        .dict_get(b"pieces")
        .ok_or(TorrentMetaError::MissingField("pieces"))?
        .as_bytes()
        .ok_or(TorrentMetaError::BadType("pieces"))?;
    // 与 daemon 侧（bt_alerts.rs）口径统一：余数不得静默丢弃。
    if pieces.len() % 20 != 0 {
        return Err(TorrentMetaError::BadPiecesLen(pieces.len()));
    }
    let num_pieces = pieces.len() as i64 / 20;

    let mut files = Vec::new();
    if let Some(len) = info.dict_get(b"length") {
        let size = len.as_int().ok_or(TorrentMetaError::BadType("length"))?;
        files.push(TorrentFileMeta {
            path: name.clone(),
            size: size.max(0) as u64,
        });
    } else if let Some(Value::List(entries)) = info.dict_get(b"files") {
        for entry in entries {
            let Value::Dict(_) = entry else {
                return Err(TorrentMetaError::BadType("files[]"));
            };
            let size = entry
                .dict_get(b"length")
                .ok_or(TorrentMetaError::MissingField("files[].length"))?
                .as_int()
                .ok_or(TorrentMetaError::BadType("files[].length"))?;
            let Value::List(parts) = entry
                .dict_get(b"path")
                .ok_or(TorrentMetaError::MissingField("files[].path"))?
            else {
                return Err(TorrentMetaError::BadType("files[].path"));
            };
            if parts.is_empty() {
                return Err(TorrentMetaError::EmptyPath);
            }
            let segs: Vec<String> = parts
                .iter()
                .filter_map(|p| p.as_bytes().map(bytes_to_string))
                .collect();
            files.push(TorrentFileMeta {
                path: segs.join("/"),
                size: size.max(0) as u64,
            });
        }
    } else {
        return Err(TorrentMetaError::MissingField("length|files"));
    }

    // P0-11 修复：checked 求和——原 `sum()` 在 [u64::MAX, 1] 场景回绕为 0，
    // 声称总大小 0 字节 → 磁盘预检必然放行 → 写爆磁盘（D36 整数层绕过）。
    let total_size = files
        .iter()
        .try_fold(0u64, |acc, f| acc.checked_add(f.size))
        .ok_or(TorrentMetaError::TotalSizeOverflow)?;
    Ok(TorrentSummary {
        infohash_v1,
        name,
        piece_len,
        num_pieces,
        files,
        total_size,
        trackers: collect_trackers(&top),
        web_seeds: collect_web_seeds(&top),
        comment: top
            .dict_get(b"comment")
            .and_then(Value::as_bytes)
            .map(bytes_to_string),
        created_by: top
            .dict_get(b"created by")
            .and_then(Value::as_bytes)
            .map(bytes_to_string),
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
            (
                b"announce".to_vec(),
                Value::Bytes(b"http://tracker/a".to_vec()),
            ),
            (b"info".to_vec(), info),
        ])
        .into_bencode()
    }

    /// 多文件 + announce-list + url-list + comment/created by。
    fn multi_file_torrent() -> Vec<u8> {
        let info = Value::Dict(vec![
            (
                b"files".to_vec(),
                Value::List(vec![
                    Value::Dict(vec![
                        (b"length".to_vec(), Value::Int(100)),
                        (
                            b"path".to_vec(),
                            Value::List(vec![
                                Value::Bytes(b"sub".to_vec()),
                                Value::Bytes(b"a.txt".to_vec()),
                            ]),
                        ),
                    ]),
                    Value::Dict(vec![
                        (b"length".to_vec(), Value::Int(250)),
                        (
                            b"path".to_vec(),
                            Value::List(vec![Value::Bytes(b"b.bin".to_vec())]),
                        ),
                    ]),
                ]),
            ),
            (b"name".to_vec(), Value::Bytes(b"pkg".to_vec())),
            (b"piece length".to_vec(), Value::Int(32768)),
            (b"pieces".to_vec(), Value::Bytes(vec![0xCD; 40])),
        ]);
        Value::Dict(vec![
            (
                b"announce".to_vec(),
                Value::Bytes(b"http://tracker/1".to_vec()),
            ),
            (
                b"announce-list".to_vec(),
                Value::List(vec![
                    Value::List(vec![Value::Bytes(b"http://tracker/1".to_vec())]),
                    Value::List(vec![Value::Bytes(b"http://tracker/2".to_vec())]),
                ]),
            ),
            (b"comment".to_vec(), Value::Bytes(b"hello".to_vec())),
            (
                b"created by".to_vec(),
                Value::Bytes(b"smart-dl test".to_vec()),
            ),
            (b"info".to_vec(), info),
            (
                b"url-list".to_vec(),
                Value::List(vec![
                    Value::Bytes(b"https://ws/1".to_vec()),
                    Value::Bytes(b"https://ws/2".to_vec()),
                ]),
            ),
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
        assert_eq!(
            s.files,
            vec![TorrentFileMeta {
                path: "test.iso".into(),
                size: 2048
            }]
        );
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
            (
                b"url-list".to_vec(),
                Value::Bytes(b"https://only/one".to_vec()),
            ),
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
        let (start, end) = bencode::locate_dict_value(&bytes, b"info").unwrap();
        let mut h = Sha1::new();
        h.update(&bytes[start..=end]);
        let expect: String = h.finalize().iter().map(|x| format!("{x:02x}")).collect();
        assert_eq!(s.infohash_v1, expect);
    }

    #[test]
    fn missing_info_rejected() {
        let bytes =
            Value::Dict(vec![(b"announce".to_vec(), Value::Bytes(b"x".to_vec()))]).into_bencode();
        assert_eq!(
            parse_torrent(&bytes).unwrap_err(),
            TorrentMetaError::MissingInfo
        );
    }

    #[test]
    fn garbage_rejected() {
        assert!(matches!(
            parse_torrent(b"not-bencode"),
            Err(TorrentMetaError::Decode(_))
        ));
        assert_eq!(
            parse_torrent(b"i5e").unwrap_err(),
            TorrentMetaError::MissingInfo
        );
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
            (
                b"files".to_vec(),
                Value::List(vec![Value::Dict(vec![
                    (b"length".to_vec(), Value::Int(1)),
                    (b"path".to_vec(), Value::List(vec![])),
                ])]),
            ),
            (b"name".to_vec(), Value::Bytes(b"d".to_vec())),
            (b"piece length".to_vec(), Value::Int(16384)),
            (b"pieces".to_vec(), Value::Bytes(vec![0u8; 20])),
        ]);
        let bytes = Value::Dict(vec![(b"info".to_vec(), info)]).into_bencode();
        assert_eq!(
            parse_torrent(&bytes).unwrap_err(),
            TorrentMetaError::EmptyPath
        );
    }

    #[test]
    fn locate_span_handles_nested_and_trailing_keys() {
        // info 之前有嵌套 list 值、之后还有别的键——定位须精确
        let bytes = multi_file_torrent();
        let (s, e) = bencode::locate_dict_value(&bytes, b"info").unwrap();
        // span 应以 'd' 开头 'e' 结尾且可独立解码
        assert_eq!(bytes[s], b'd');
        assert_eq!(bytes[e], b'e');
        let inner = crate::bencode::decode(&bytes[s..=e]).unwrap();
        assert!(inner.dict_get(b"name").is_some());
    }

    /// P0-12 回归：piece length = 0 / 负值 / 超 16MiB 必须被拒绝。
    /// 修复前：只校验类型不校验取值，实测 0 与 -5 均被 ACCEPTED。
    #[test]
    fn invalid_piece_length_rejected() {
        for bad in [0i64, -5, -1, (1 << 24) + 1, i64::MAX] {
            let info = Value::Dict(vec![
                (b"length".to_vec(), Value::Int(1)),
                (b"name".to_vec(), Value::Bytes(b"x".to_vec())),
                (b"piece length".to_vec(), Value::Int(bad)),
                (b"pieces".to_vec(), Value::Bytes(vec![0u8; 20])),
            ]);
            let bytes = Value::Dict(vec![(b"info".to_vec(), info)]).into_bencode();
            assert_eq!(
                parse_torrent(&bytes).unwrap_err(),
                TorrentMetaError::InvalidPieceLength(bad),
                "piece length = {bad} 必须被拒绝"
            );
        }
    }

    /// P0-12 附带：pieces 长度非 20 倍数必须报错（原实现静默丢弃余数，
    /// 21 字节 → num_pieces = 1，与 daemon 侧口径不一致）。
    #[test]
    fn pieces_len_not_multiple_of_20_rejected() {
        let info = Value::Dict(vec![
            (b"length".to_vec(), Value::Int(1)),
            (b"name".to_vec(), Value::Bytes(b"x".to_vec())),
            (b"piece length".to_vec(), Value::Int(16384)),
            (b"pieces".to_vec(), Value::Bytes(vec![0u8; 21])),
        ]);
        let bytes = Value::Dict(vec![(b"info".to_vec(), info)]).into_bencode();
        assert_eq!(
            parse_torrent(&bytes).unwrap_err(),
            TorrentMetaError::BadPiecesLen(21)
        );
    }

    /// P0-11 回归：各文件 size 求和溢出 u64 必须报错。
    /// 修复前：`sum()` 回绕——[u64::MAX, 1] 声称总大小 0 字节，
    /// 磁盘预检必然放行 → 写爆磁盘（已实证：repro/p0repro round2）。
    #[test]
    fn total_size_overflow_rejected() {
        let info = Value::Dict(vec![
            (
                b"files".to_vec(),
                Value::List(vec![
                    Value::Dict(vec![
                        (b"length".to_vec(), Value::Int(i64::MAX)),
                        (
                            b"path".to_vec(),
                            Value::List(vec![Value::Bytes(b"a".to_vec())]),
                        ),
                    ]),
                    Value::Dict(vec![
                        (b"length".to_vec(), Value::Int(i64::MAX)),
                        (
                            b"path".to_vec(),
                            Value::List(vec![Value::Bytes(b"b".to_vec())]),
                        ),
                    ]),
                    Value::Dict(vec![
                        (b"length".to_vec(), Value::Int(i64::MAX)),
                        (
                            b"path".to_vec(),
                            Value::List(vec![Value::Bytes(b"c".to_vec())]),
                        ),
                    ]),
                ]),
            ),
            (b"name".to_vec(), Value::Bytes(b"pkg".to_vec())),
            (b"piece length".to_vec(), Value::Int(16384)),
            (b"pieces".to_vec(), Value::Bytes(vec![0u8; 20])),
        ]);
        let bytes = Value::Dict(vec![(b"info".to_vec(), info)]).into_bencode();
        assert_eq!(
            parse_torrent(&bytes).unwrap_err(),
            TorrentMetaError::TotalSizeOverflow,
            "3 × i64::MAX 的求和溢出必须报错而非回绕"
        );
    }
}
