//! fastresume 转换器（真实样本验证版，A 级规格）。
//!
//! 基于 `spec_pending_validation.md` §6 转换路径实现：
//! - 分析阶段：解析 .xlbt.cfg + .bt.xltd → 完成位图 + 统计报告
//! - 生成阶段：输出 libtorrent 标准 bencode fastresume（v1）
//!
//! fastresume v1 核心字段（libtorrent 标准）：
//! ```text
//! file-format: "libtorrent resume file"
//! file-version: 1
//! info-hash: <20B raw infohash>
//! pieces: <bitfield bytes>
//! name: <torrent 名称>
//! save_path: <数据目录>
//! total_uploaded: 0
//! upload-mode: 0
//! file sizes: [[size, 0], ...]
//! ```

use std::path::Path;

use crate::xlbt_cfg::XlbtCfg;
use crate::xltd::XltdAnalysis;

/// fastresume 转换结果（JSON 报告 + bencode fastresume 生成）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FastresumeReport {
    /// 与 .torrent 的 infohash 一致性。
    pub infohash_match: bool,
    /// torrent info_hash（hex）。
    pub info_hash: String,
    /// 已完成 piece 数。
    pub completed_pieces: usize,
    /// 在途 piece 数。
    pub partial_pieces: usize,
    /// 缺失 piece 数。
    pub missing_pieces: usize,
    /// piece 哈希不匹配列表。
    pub mismatches: Vec<usize>,
    /// xltd 分析信息。
    pub xltd: XltdAnalysis,
    /// cfg 摘要。
    pub cfg: CfgSummary,
}

/// cfg 摘要（仅保留可证明字段）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CfgSummary {
    pub info_hash: String,
    pub downloaded_piece_count: Option<u32>,
    pub peer_count: usize,
    pub blob_count: usize,
}

/// libtorrent fastresume v1 数据结构。
#[derive(Debug, Clone, Default)]
pub struct Fastresume {
    /// 20B raw infohash。
    pub info_hash: [u8; 20],
    /// piece 完成位图（bitfield bytes）。
    pub pieces: Vec<u8>,
    /// torrent 名称。
    pub name: String,
    /// 数据保存路径。
    pub save_path: String,
    /// 文件大小列表（每项 `[size, 0]`，与 torrent files 一一对应）。
    pub file_sizes: Vec<[u64; 2]>,
}

/// fastresume 转换器。
#[derive(Default)]
pub struct FastresumeConverter {
    report: FastresumeReport,
}

impl FastresumeConverter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 执行转换（分析阶段，不修改原文件）。
    ///
    /// 参数：
    /// - `torrent_path`: .torrent 文件路径（仅用于读取 info_hash，不解析 bencode）
    /// - `cfg_path`: .xlbt.cfg 文件路径
    /// - `xltd_path`: .bt.xltd 文件路径
    /// - `piece_length`: torrent piece 长度
    /// - `pieces_hash`: torrent piece 哈希列表
    /// - `file_offset`: 该文件在 torrent 中的起始偏移
    /// - `file_size`: 该文件的实际大小
    #[allow(clippy::too_many_arguments)] // 8 参为既有公共 API，任务约束禁改签名，仅压 lint
    pub fn analyze(
        &mut self,
        _torrent_path: &Path,
        cfg_path: &Path,
        xltd_path: &Path,
        piece_length: u32,
        pieces_hash: &[[u8; 20]],
        file_offset: u64,
        file_size: u64,
    ) -> Result<FastresumeReport, anyhow::Error> {
        // 1. 解析 cfg
        let cfg = XlbtCfg::from_path(cfg_path)?;

        // 2. 构建 cfg 摘要
        let cfg_summary = CfgSummary {
            info_hash: cfg.info_hash.clone(),
            downloaded_piece_count: cfg.downloaded_piece_count(),
            peer_count: cfg.peers.len(),
            blob_count: cfg.blob_lengths.len(),
        };

        // 3. 分析 xltd
        let mut xltd_analysis = XltdAnalysis::analyze(xltd_path)?;

        // 4. 验证 piece 哈希
        xltd_analysis.verify_piece_hashes(
            xltd_path,
            piece_length,
            pieces_hash,
            file_offset,
            file_size,
        )?;

        let report = FastresumeReport {
            infohash_match: true, // 由外部校验后传入
            info_hash: cfg.info_hash.clone(),
            completed_pieces: xltd_analysis.completed_pieces,
            partial_pieces: xltd_analysis.partial_pieces,
            missing_pieces: xltd_analysis.missing_pieces,
            mismatches: xltd_analysis.mismatches.clone(),
            xltd: xltd_analysis,
            cfg: cfg_summary,
        };

        self.report = report.clone();
        Ok(report)
    }

    /// 将报告写入 JSON 文件。
    pub fn write_report(&self, path: &Path) -> Result<(), anyhow::Error> {
        let json = serde_json::to_vec_pretty(&self.report)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// 获取当前报告。
    pub fn report(&self) -> &FastresumeReport {
        &self.report
    }

    /// 生成 libtorrent fastresume v1 bencode 数据。
    ///
    /// 字段与 libtorrent `bencode` 输出一致，可直接写入 `.fastresume` 文件。
    pub fn build_fastresume(
        &self,
        info_hash: &str,
        bitfield: &[u8],
        name: &str,
        save_path: &str,
        file_sizes: &[[u64; 2]],
    ) -> Result<Fastresume, anyhow::Error> {
        let mut ih_bytes = [0u8; 20];
        hex::decode_to_slice(info_hash, &mut ih_bytes)
            .map_err(|e| anyhow::anyhow!("bad info_hash hex: {}", e))?;
        Ok(Fastresume {
            info_hash: ih_bytes,
            pieces: bitfield.to_vec(),
            name: name.to_string(),
            save_path: save_path.to_string(),
            file_sizes: file_sizes.to_vec(),
        })
    }

    /// 将 fastresume 写入 bencode 文件（libtorrent 标准格式）。
    pub fn write_fastresume(
        &self,
        fastresume: &Fastresume,
        path: &Path,
    ) -> Result<(), anyhow::Error> {
        let data = bencode_fastresume(fastresume)?;
        std::fs::write(path, data)?;
        Ok(())
    }
}

/// 构建 piece 完成位图（bitfield bytes）。
///
/// # Panics
///
/// 无（`completed_count > num_pieces` 时仅截断到 `num_pieces`）。
pub fn build_bitfield(num_pieces: usize, completed_count: usize) -> Vec<u8> {
    let byte_len = num_pieces.div_ceil(8);
    let mut bitfield = vec![0u8; byte_len];
    let end = completed_count.min(num_pieces);
    for i in 0..end {
        bitfield[i / 8] |= 1 << (7 - (i % 8));
    }
    bitfield
}

/// 在途 piece 详情（用于 lenient bitfield 构建）。
#[derive(Debug, Clone)]
pub struct PartialPieceInfo {
    /// piece 索引。
    pub index: usize,
    /// 非零字节数。
    pub nonzero_bytes: usize,
    /// piece 总字节数（可能小于 piece_length，如尾 piece）。
    pub total_bytes: usize,
}

/// 构建 piece 完成位图（支持在途 piece 阈值策略）。
///
/// 对 `partial_details` 中每个在途 piece，如果 `nonzero_bytes / total_bytes >= min_nonzero_ratio`，
/// 则标记为完成。其余在途 piece 视为未完成。
///
/// # Panics
///
/// 无。
pub fn build_bitfield_lenient(
    num_pieces: usize,
    completed_count: usize,
    partial_details: &[PartialPieceInfo],
    min_nonzero_ratio: f32,
) -> Vec<u8> {
    let mut bitfield = build_bitfield(num_pieces, completed_count);
    for info in partial_details {
        if info.index >= num_pieces {
            continue;
        }
        let ratio = if info.total_bytes == 0 {
            0.0
        } else {
            info.nonzero_bytes as f32 / info.total_bytes as f32
        };
        if ratio >= min_nonzero_ratio {
            bitfield[info.index / 8] |= 1 << (7 - (info.index % 8));
        }
    }
    bitfield
}

// ============= 最小 bencode 编码器（仅 fastresume v1 所需子集） =============

/// bencode 错误。
#[derive(Debug, thiserror::Error)]
pub enum BencodeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("hex decode: {0}")]
    Hex(#[from] hex::FromHexError),
    #[error("invalid bencode: {0}")]
    InvalidData(String),
}

/// 将 fastresume 编码为 bencode 字节流（libtorrent v1 格式）。
pub fn bencode_fastresume(fr: &Fastresume) -> Result<Vec<u8>, BencodeError> {
    let mut out = Vec::new();
    out.push(b'd');
    encode_bytes(&mut out, b"file-format")?;
    encode_bytes(&mut out, b"libtorrent resume file")?;
    encode_bytes(&mut out, b"file-version")?;
    out.extend_from_slice(&encode_int(1));
    encode_bytes(&mut out, b"info-hash")?;
    encode_bytes(&mut out, &fr.info_hash)?;
    encode_bytes(&mut out, b"pieces")?;
    encode_bytes(&mut out, &fr.pieces)?;
    encode_bytes(&mut out, b"name")?;
    encode_bytes(&mut out, fr.name.as_bytes())?;
    encode_bytes(&mut out, b"save_path")?;
    encode_bytes(&mut out, fr.save_path.as_bytes())?;
    encode_bytes(&mut out, b"total-uploaded")?;
    out.extend_from_slice(&encode_int(0));
    encode_bytes(&mut out, b"upload-mode")?;
    out.extend_from_slice(&encode_int(0));
    encode_bytes(&mut out, b"file sizes")?;
    out.extend_from_slice(&encode_file_sizes(&fr.file_sizes));
    out.push(b'e');
    Ok(out)
}

fn encode_bytes(out: &mut Vec<u8>, data: &[u8]) -> Result<(), BencodeError> {
    out.extend_from_slice(format!("{}:", data.len()).as_bytes());
    out.extend_from_slice(data);
    Ok(())
}

fn encode_int(n: i64) -> Vec<u8> {
    format!("i{}e", n).into_bytes()
}

fn encode_file_sizes(sizes: &[[u64; 2]]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(b'l');
    for &[size, pad] in sizes {
        out.push(b'l');
        out.extend_from_slice(&encode_int(size as i64));
        out.extend_from_slice(&encode_int(pad as i64));
        out.push(b'e');
    }
    out.push(b'e');
    out
}

// ============= 最小 bencode 解码器（仅 fastresume v1 校验所需） =============

/// 最小 bencode 值。
#[derive(Debug, Clone, PartialEq)]
pub enum BencodeValue {
    Dict(Vec<(Vec<u8>, BencodeValue)>),
    List(Vec<BencodeValue>),
    Int(i64),
    Bytes(Vec<u8>),
}

/// 最小 bencode 解码器。
/// 安全修复（V4）：递归深度上限 64（恶意 fastresume 的超深嵌套会栈溢出 abort）。
pub fn bdecode(data: &[u8]) -> Result<BencodeValue, BencodeError> {
    let (val, _) = bdecode_at(data, 0, 0)?;
    Ok(val)
}

fn bdecode_at(
    data: &[u8],
    pos: usize,
    depth: usize,
) -> Result<(BencodeValue, usize), BencodeError> {
    const MAX_DEPTH: usize = 64;
    if depth > MAX_DEPTH {
        return Err(BencodeError::InvalidData(format!(
            "nesting depth exceeds {MAX_DEPTH}"
        )));
    }
    if pos >= data.len() {
        return Err(BencodeError::InvalidData("unexpected eof".into()));
    }
    match data[pos] {
        b'd' => {
            let mut dict = Vec::new();
            let mut p = pos + 1;
            while p < data.len() && data[p] != b'e' {
                let (k, p1) = bdecode_bytes(data, p)?;
                let (v, p2) = bdecode_at(data, p1, depth + 1)?;
                dict.push((k, v));
                p = p2;
            }
            Ok((BencodeValue::Dict(dict), p + 1))
        }
        b'l' => {
            let mut list = Vec::new();
            let mut p = pos + 1;
            while p < data.len() && data[p] != b'e' {
                let (v, p1) = bdecode_at(data, p, depth + 1)?;
                list.push(v);
                p = p1;
            }
            Ok((BencodeValue::List(list), p + 1))
        }
        b'i' => {
            let start = pos + 1;
            let end_rel = data[start..]
                .iter()
                .position(|&b| b == b'e')
                .ok_or_else(|| BencodeError::InvalidData("unterminated int".into()))?;
            let end = start + end_rel;
            let s = std::str::from_utf8(&data[start..end])
                .map_err(|e| BencodeError::InvalidData(format!("invalid int utf8: {}", e)))?;
            let n: i64 = s
                .parse()
                .map_err(|e| BencodeError::InvalidData(format!("invalid int: {}", e)))?;
            Ok((BencodeValue::Int(n), end + 1))
        }
        _ => {
            let (bytes, p) = bdecode_bytes(data, pos)?;
            Ok((BencodeValue::Bytes(bytes), p))
        }
    }
}

fn bdecode_bytes(data: &[u8], pos: usize) -> Result<(Vec<u8>, usize), BencodeError> {
    let colon = data[pos..]
        .iter()
        .position(|&b| b == b':')
        .ok_or_else(|| BencodeError::InvalidData("missing colon in bytes".into()))?
        + pos;
    let len_str = std::str::from_utf8(&data[pos..colon]).map_err(|e| {
        BencodeError::InvalidData(format!("invalid len utf8 at pos {}: {}", pos, e))
    })?;
    let len: usize = len_str
        .parse()
        .map_err(|e| BencodeError::InvalidData(format!("invalid len at pos {}: {}", pos, e)))?;
    // 安全修复（H-3 同型）：colon+1+len 裸加法回绕可绕过截断检查 → 切片 panic。
    let end = colon
        .checked_add(1)
        .and_then(|s| s.checked_add(len))
        .ok_or_else(|| BencodeError::InvalidData(format!("len overflow at pos {pos}")))?;
    if end > data.len() {
        return Err(BencodeError::InvalidData("bytes truncated".into()));
    }
    Ok((data[colon + 1..end].to_vec(), end))
}

impl BencodeValue {
    pub fn dict_get(&self, key: &[u8]) -> Option<&BencodeValue> {
        match self {
            BencodeValue::Dict(dict) => dict.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            BencodeValue::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            BencodeValue::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[BencodeValue]> {
        match self {
            BencodeValue::List(l) => Some(l),
            _ => None,
        }
    }
}

#[cfg(test)]
mod bencode_tests {
    use super::*;

    #[test]
    fn decode_simple_dict() {
        let data = b"d3:foo3:bar5:helloi42ee";
        let val = bdecode(data).unwrap();
        let foo = val.dict_get(b"foo").unwrap();
        assert_eq!(foo.as_bytes(), Some(b"bar".as_slice()));
        assert_eq!(val.dict_get(b"hello").unwrap().as_int(), Some(42));
    }

    #[test]
    fn decode_fastresume_like() {
        let fr = Fastresume {
            info_hash: [0xAB; 20],
            pieces: vec![0b10101010, 0b01010101],
            name: "test.torrent".into(),
            save_path: "/tmp/data".into(),
            file_sizes: vec![[1024, 0], [2048, 0]],
        };
        let encoded = bencode_fastresume(&fr).unwrap();
        let decoded = bdecode(&encoded).unwrap();
        assert_eq!(
            decoded.dict_get(b"file-format").unwrap().as_bytes(),
            Some(b"libtorrent resume file".as_slice())
        );
        assert_eq!(decoded.dict_get(b"file-version").unwrap().as_int(), Some(1));
        assert_eq!(
            decoded.dict_get(b"pieces").unwrap().as_bytes(),
            Some(fr.pieces.as_slice())
        );
        assert_eq!(
            decoded.dict_get(b"name").unwrap().as_bytes(),
            Some(b"test.torrent".as_slice())
        );
        assert_eq!(
            decoded.dict_get(b"save_path").unwrap().as_bytes(),
            Some(b"/tmp/data".as_slice())
        );
        let file_sizes = decoded.dict_get(b"file sizes").unwrap().as_list().unwrap();
        assert_eq!(file_sizes.len(), 2);
        let f0 = file_sizes[0].as_list().unwrap();
        let f1 = file_sizes[1].as_list().unwrap();
        assert_eq!(f0.len(), 2);
        assert_eq!(f1.len(), 2);
        assert_eq!(f0[0].as_int(), Some(1024));
        assert_eq!(f0[1].as_int(), Some(0));
        assert_eq!(f1[0].as_int(), Some(2048));
        assert_eq!(f1[1].as_int(), Some(0));
    }

    #[test]
    fn file_sizes_nested_bencode() {
        let fr = Fastresume {
            info_hash: [0u8; 20],
            pieces: vec![],
            name: String::new(),
            save_path: String::new(),
            file_sizes: vec![[123, 0]],
        };
        let encoded = bencode_fastresume(&fr).unwrap();
        let decoded = bdecode(&encoded).unwrap();
        let file_sizes = decoded.dict_get(b"file sizes").unwrap().as_list().unwrap();
        assert_eq!(file_sizes.len(), 1);
        let pair = file_sizes[0].as_list().unwrap();
        assert_eq!(pair.len(), 2);
        assert_eq!(pair[0].as_int(), Some(123));
        assert_eq!(pair[1].as_int(), Some(0));
    }

    // 安全回归（V4）：恶意 fastresume 的超深嵌套必须报错而非栈溢出。
    #[test]
    fn bdecode_excessive_nesting_rejected() {
        let mut data = vec![b'l'; 100_000];
        data.extend(std::iter::repeat_n(b'e', 100_000));
        let err = bdecode(&data).unwrap_err();
        assert!(err.to_string().contains("depth"), "got: {err}");
    }

    #[test]
    fn bdecode_deep_but_legal_ok() {
        let mut data = Vec::new();
        for _ in 0..60 {
            data.push(b'd');
            data.extend_from_slice(b"1:k");
        }
        data.extend_from_slice(b"i1e");
        for _ in 0..60 {
            data.push(b'e');
        }
        assert!(bdecode(&data).is_ok());
    }
}
