//! P0 最小复现：真实键序多文件 .torrent → parse_file_list 除零 panic
//!
//! 代码提取自 crates/daemon/src/state/bt_alerts.rs：
//!   L5-161   (TorrentMeta/FileMeta + parse)
//!   L163-254 (parse_file_list + parse_path_list)
//!   L449-538 (bencode 工具族)
//! 逻辑未改动，仅替换错误类型名 DaemonError → Err2。

#![allow(dead_code)]

pub mod round2;
pub mod round3;

pub mod plength;

#[derive(Debug, Clone)]
pub struct Err2 {
    pub msg: String,
}
impl Err2 {
    pub fn InvalidSource(s: String) -> Self { Err2 { msg: s } }
}
impl std::fmt::Display for Err2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.msg) }
}
impl std::error::Error for Err2 {}

/// sanitize_rel 最小替身（与本 P0 无关，原实现见 crates/core/src/session/output.rs）
fn sanitize_rel(name: &str) -> Result<std::path::PathBuf, ()> {
    if name.is_empty() || name.contains("..") || name.starts_with('/') {
        return Err(());
    }
    Ok(std::path::PathBuf::from(name))
}/// 单文件 .torrent 元数据（迅雷导入用）。
#[derive(Debug, Clone)]
pub struct TorrentMeta {
    pub info_hash: String,
    pub piece_length: u32,
    pub pieces_hash: Vec<[u8; 20]>,
    pub name: String,
    /// 单文件大小（仅单文件 torrent 使用）。
    pub file_size: u64,
    /// 多文件列表（仅多文件 torrent 使用）。
    pub files: Vec<FileMeta>,
}

/// 单文件元数据。
#[derive(Debug, Clone)]
pub struct FileMeta {
    /// 相对路径（多文件）或文件名（单文件）。
    pub path: String,
    /// 文件大小（字节）。
    pub size: u64,
    /// 该文件在 torrent 中的起始 piece 索引。
    pub piece_offset: usize,
    /// 该文件占用的 piece 数量。
    pub piece_count: usize,
}


impl TorrentMeta {
    /// 从 .torrent 字节解析元数据（单文件/多文件）。
    pub fn parse(b: &[u8]) -> Result<Self, Err2> {
        use sha1::Digest;
        let (info_start, info_end) = locate_info(b).ok_or_else(|| {
            Err2::InvalidSource(".torrent 解析失败：无法定位 info dict".into())
        })?;

        let info_hash = {
            let digest = sha1::Sha1::digest(&b[info_start..=info_end]);
            digest
                .iter()
                .map(|x| format!("{x:02x}"))
                .collect::<String>()
        };

        let mut piece_length = 0u32;
        let mut pieces_hash = Vec::new();
        let mut name = String::new();
        let mut file_size = 0u64;
        let mut has_length = false;
        let mut files = Vec::new();
        let mut has_files = false;

        let mut i = info_start + 1; // skip 'd'
        let end = info_end;
        while i < end {
            let (key, after_key) = be_str(b, i)
                .ok_or_else(|| Err2::InvalidSource(".torrent info dict 解析失败".into()))?;
            i = after_key;

            match key {
                b"piece length" => {
                    piece_length = be_int(b, i)
                        .ok_or_else(|| Err2::InvalidSource("piece length 解析失败".into()))?
                        as u32;
                    i = value_skip(b, i, 0).ok_or_else(|| {
                        Err2::InvalidSource(".torrent info dict 解析失败".into())
                    })?;
                }
                b"pieces" => {
                    let pieces_data = be_str(b, i)
                        .ok_or_else(|| Err2::InvalidSource("pieces 解析失败".into()))?;
                    if pieces_data.0.len() % 20 != 0 {
                        return Err(Err2::InvalidSource(
                            "pieces 长度不是 20 的倍数".into(),
                        ));
                    }
                    pieces_hash = pieces_data.0.as_chunks::<20>().0.to_vec();
                    i = value_skip(b, i, 0).ok_or_else(|| {
                        Err2::InvalidSource(".torrent info dict 解析失败".into())
                    })?;
                }
                b"name" => {
                    name = String::from_utf8_lossy(
                        be_str(b, i)
                            .ok_or_else(|| Err2::InvalidSource("name 解析失败".into()))?
                            .0,
                    )
                    .into_owned();
                    // 安全修复（V3）：torrent 根名直通 dest_root.join，恶意 name
                    // （../、绝对路径）即任意文件写——parse 层即拒任务。
                    sanitize_rel(&name).map_err(|_| {
                        Err2::InvalidSource(format!(
                            ".torrent name 含非法路径分量已拒绝: {name}"
                        ))
                    })?;
                    i = value_skip(b, i, 0).ok_or_else(|| {
                        Err2::InvalidSource(".torrent info dict 解析失败".into())
                    })?;
                }
                b"length" => {
                    file_size = be_int(b, i)
                        .ok_or_else(|| Err2::InvalidSource("length 解析失败".into()))?
                        as u64;
                    has_length = true;
                    i = value_skip(b, i, 0).ok_or_else(|| {
                        Err2::InvalidSource(".torrent info dict 解析失败".into())
                    })?;
                }
                b"files" => {
                    has_files = true;
                    // files value 是 list（l...e）
                    let list_end = list_skip(b, i, 0)
                        .ok_or_else(|| Err2::InvalidSource("files 解析失败".into()))?;
                    files = parse_file_list(&b[i + 1..list_end], piece_length)?;
                    i = list_end + 1; // 跳过 list 的闭合 'e'
                }
                _ => {
                    i = value_skip(b, i, 0).ok_or_else(|| {
                        Err2::InvalidSource(".torrent info dict 解析失败".into())
                    })?;
                }
            }
        }

        if piece_length == 0 || pieces_hash.is_empty() {
            return Err(Err2::InvalidSource(
                ".torrent 缺少必要字段 (piece length/pieces)".into(),
            ));
        }

        if has_files {
            // 多文件 torrent：files 数组已解析
            Ok(Self {
                info_hash,
                piece_length,
                pieces_hash,
                name,
                file_size: 0,
                files,
            })
        } else {
            // 单文件 torrent
            if !has_length {
                return Err(Err2::InvalidSource(
                    ".torrent 缺少 length 字段".into(),
                ));
            }
            Ok(Self {
                info_hash,
                piece_length,
                pieces_hash,
                name,
                file_size,
                files: vec![],
            })
        }
    }
}

/// 解析多文件 torrent 的 files 列表内容（bencode，`l`/`e` 已剥离）。

fn parse_file_list(data: &[u8], piece_length: u32) -> Result<Vec<FileMeta>, Err2> {
    let mut files = Vec::new();
    let mut pos = 0usize;
    let plen = piece_length as u64;

    while pos < data.len() {
        if data.get(pos) != Some(&b'd') {
            pos = value_skip(data, pos, 0)
                .ok_or_else(|| Err2::InvalidSource("files 列表解析失败".into()))?;
            continue;
        }
        let dict_end = dict_skip(data, pos, 0)
            .ok_or_else(|| Err2::InvalidSource("files dict 解析失败".into()))?;
        let file_dict = &data[pos..=dict_end];

        let mut path = String::new();
        let mut length = 0u64;
        let mut j = 1;
        while j < file_dict.len() - 1 {
            let (key, after_key) = be_str(file_dict, j)
                .ok_or_else(|| Err2::InvalidSource("files dict key 解析失败".into()))?;
            j = after_key;
            match key {
                b"length" => {
                    length = be_int(file_dict, j)
                        .ok_or_else(|| Err2::InvalidSource("files length 解析失败".into()))?
                        as u64;
                    j = value_skip(file_dict, j, 0).ok_or_else(|| {
                        Err2::InvalidSource("files dict value 解析失败".into())
                    })?;
                }
                b"path" => {
                    // path value 是 list（l...e）
                    let path_list_end = list_skip(file_dict, j, 0).ok_or_else(|| {
                        Err2::InvalidSource("files path list 解析失败".into())
                    })?;
                    path = parse_path_list(&file_dict[j + 1..path_list_end])?;
                    j = path_list_end + 1;
                }
                _ => {
                    j = value_skip(file_dict, j, 0).ok_or_else(|| {
                        Err2::InvalidSource("files dict value 解析失败".into())
                    })?;
                }
            }
        }

        if length > 0 && !path.is_empty() {
            // 计算 piece 偏移和数量（按文件在 torrent 中的累计字节偏移）
            let total_size: u64 = files.iter().map(|f: &FileMeta| f.size).sum();
            let piece_offset = (total_size / plen) as usize;
            let piece_count = length.div_ceil(plen) as usize;
            files.push(FileMeta {
                path,
                size: length,
                piece_offset,
                piece_count,
            });
        }

        pos = dict_end + 1;
    }

    Ok(files)
}

/// 解析 path list 内容（bencode，`l`/`e` 已剥离）为路径字符串。
/// 安全修复（V3）：逐段净化——拒 `..` / 绝对路径段，恶意种子不得写出 dest_root。

fn parse_path_list(data: &[u8]) -> Result<String, Err2> {
    let mut parts = Vec::new();
    let mut p = 0usize;
    while p < data.len() {
        let (seg, after) = be_str(data, p)
            .ok_or_else(|| Err2::InvalidSource("path segment 解析失败".into()))?;
        let seg_str = String::from_utf8_lossy(seg).into_owned();
        if seg_str == ".." || seg_str.contains('/') || seg_str.contains('\\') || seg_str.is_empty()
        {
            return Err(Err2::InvalidSource(format!(
                "files path 含非法段已拒绝: {seg_str}"
            )));
        }
        parts.push(seg_str);
        p = after;
    }
    if parts.is_empty() {
        return Err(Err2::InvalidSource("files path 为空".into()));
    }
    Ok(parts.join(std::path::MAIN_SEPARATOR_STR))
}

/// 定位 info dict：返回 (info 值起始 'd' 下标, info dict 闭合 'e' 下标)。

fn locate_info(b: &[u8]) -> Option<(usize, usize)> {
    if b.first() != Some(&b'd') {
        return None;
    }
    let mut i = 1; // 顶层 dict 键值对扫描
    while i < b.len() {
        let (key, after_key) = be_str(b, i)?;
        i = after_key;
        if key == b"info" {
            if b.get(i) != Some(&b'd') {
                return None; // info 必须是 dict
            }
            let end = dict_skip(b, i, 0)?;
            return Some((i, end));
        }
        // 跳过值（结构感知），继续找 `info`
        i = value_skip(b, i, 0)?;
    }
    None
}

/// bencode 字符串 `len:data` → (data, 内容后下标)。

fn be_str(b: &[u8], at: usize) -> Option<(&[u8], usize)> {
    let colon = b[at..].iter().position(|&c| c == b':')? + at;
    let len: usize = std::str::from_utf8(&b[at..colon]).ok()?.parse().ok()?;
    let start = colon + 1;
    // 安全修复（H-3 同型）：start+len 裸加法——恶意 fastresume/torrent 的超大
    // 长度字段在 release 下回绕或直接越界 → 切片 panic。checked_add + 界检查。
    let end = start.checked_add(len)?;
    if end > b.len() {
        return None;
    }
    Some((&b[start..end], end))
}

/// bencode 整数 `i<digits>e` → 值。

fn be_int(b: &[u8], at: usize) -> Option<i64> {
    if b.get(at) != Some(&b'i') {
        return None;
    }
    let e = b[at..].iter().position(|&c| c == b'e')? + at;
    let s = std::str::from_utf8(&b[at + 1..e]).ok()?;
    s.parse().ok()
}

/// dict 结束下标：从 `start`（'d'）按 键(字符串)→值 结构推进到闭合 'e'。
/// 键位置固定为字符串（len: 数字开头），值可为任意类型——值内的数据字节
/// （如 pieces 的 20 字节）不会被误当 len: 解析。
/// 安全修复（V4）：带深度参数，超限返回 None（恶意种子不再能栈溢出 abort）。

fn dict_skip(b: &[u8], start: usize, depth: usize) -> Option<usize> {
    const MAX_DEPTH: usize = 64;
    if depth > MAX_DEPTH {
        return None;
    }
    let mut i = start + 1;
    while b.get(i) != Some(&b'e') {
        let (_, after) = be_str(b, i)?; // 键：字符串
        i = value_skip(b, after, depth + 1)?; // 值：任意类型
    }
    Some(i)
}

/// list 结束下标：从 `start`（'l'）按 值* 推进到闭合 'e'。

fn list_skip(b: &[u8], start: usize, depth: usize) -> Option<usize> {
    let mut i = start + 1;
    while b.get(i) != Some(&b'e') {
        i = value_skip(b, i, depth + 1)?;
    }
    Some(i)
}

/// 跳过任意 bencode 值（dict/list/int/str），返回其后的下标。

fn value_skip(b: &[u8], i: usize, depth: usize) -> Option<usize> {
    match b.get(i)? {
        b'd' => dict_skip(b, i, depth).map(|e| e + 1),
        b'l' => list_skip(b, i, depth).map(|e| e + 1),
        b'i' => {
            let e = b[i..].iter().position(|&c| c == b'e')? + i;
            Some(e + 1)
        }
        _ => be_str(b, i).map(|(_, after)| after),
    }
}
// ══════════════ 复现 ══════════════
#[cfg(test)]
mod p0 {
    use super::*;

    /// 真实生成器键序（bencode 字典序）：files < name < piece length < pieces
    fn real_order() -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(b"d4:infod");
        t.extend_from_slice(b"5:filesl");
        t.extend_from_slice(b"d6:lengthi10e4:pathl1:aee");
        t.extend_from_slice(b"d6:lengthi20e4:pathl1:bee");
        t.extend_from_slice(b"e");
        t.extend_from_slice(b"4:name4:mult");
        t.extend_from_slice(b"12:piece lengthi16384e");
        t.extend_from_slice(b"6:pieces20:");
        t.extend_from_slice(&[0u8; 20]);
        t.extend_from_slice(b"ee");
        t
    }

    #[test]
    fn p0_divide_by_zero() {
        let torrent = real_order();
        println!("=== 复现 P0: 真实键序多文件 torrent ===");
        println!("字节: {}", String::from_utf8_lossy(&torrent));
        println!("→ TorrentMeta::parse()  [piece_length 此时仍为 0]");
        let meta = TorrentMeta::parse(&torrent).expect("parse 失败");
        println!("解析成功: files={}", meta.files.len());
        for f in &meta.files {
            println!("  path={} size={} piece_offset={} piece_count={}",
                f.path, f.size, f.piece_offset, f.piece_count);
        }
        assert!(meta.files.iter().any(|f| f.piece_count > 0),
            "P0-B: piece_count 全 0 → 完成位图归零（静默损坏）");
    }

    #[test]
    fn control_illegal_order() {
        let mut t = Vec::new();
        t.extend_from_slice(b"d4:infod");
        t.extend_from_slice(b"12:piece lengthi16384e");
        t.extend_from_slice(b"6:pieces20:");
        t.extend_from_slice(&[0u8; 20]);
        t.extend_from_slice(b"4:name4:mult");
        t.extend_from_slice(b"5:filesl");
        t.extend_from_slice(b"d6:lengthi10e4:pathl1:aee");
        t.extend_from_slice(b"d6:lengthi20e4:pathl1:bee");
        t.extend_from_slice(b"e");
        t.extend_from_slice(b"ee");
        println!("=== 对照组: 非法键序（现有测试口径）===");
        let meta = TorrentMeta::parse(&t).expect("对照组解析失败");
        println!("成功: files={}", meta.files.len());
        for f in &meta.files {
            println!("  path={} piece_offset={} piece_count={}", f.path, f.piece_offset, f.piece_count);
        }
    }
}




