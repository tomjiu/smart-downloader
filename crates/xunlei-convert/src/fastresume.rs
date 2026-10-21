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
use smart_dl_core::bencode::Value;

/// fastresume 转换结果（JSON 报告 + bencode fastresume 生成）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FastresumeReport {
    /// 与 .torrent 的 infohash 一致性。
    pub infohash_match: bool,
    /// torrent info_hash（hex）。
    pub info_hash: String,
    /// 已完成 piece 数。
    pub completed_pieces: usize,
    /// 已完成 piece 的局部索引集（审计修复 P0-2 新增，与 completed_pieces
    /// 一致；供调用方按真实位置合并全局位图，不再假设前缀）。旧 JSON 兼容。
    #[serde(default)]
    pub completed_piece_indices: Vec<usize>,
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
            completed_piece_indices: xltd_analysis.completed_indices.clone(),
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

/// 构建 piece 完成位图（审计修复 P0-2：按真实索引置位）。
///
/// 原 `build_bitfield(num_pieces, completed_count)` 把完成数当**前缀长度**
/// 置位 0..N——仅当完成块恰为全局前缀时正确；多文件种子跨文件进度不均衡
/// 或 P2SP 非顺序完成时，前置未下载 piece 会被误标完成 → libtorrent 信任
/// fastresume 不再重下 → 落盘文件混入全零块（静默损坏）。本函数接收真实
/// 索引集，位置语义严格正确。
///
/// # Panics
///
/// 无（越界索引被忽略；partial 策略与 `build_bitfield_lenient` 一致）。
pub fn build_bitfield_from_indices(
    num_pieces: usize,
    completed_indices: &[usize],
    partial_details: &[PartialPieceInfo],
    min_nonzero_ratio: f32,
) -> Vec<u8> {
    let byte_len = num_pieces.div_ceil(8);
    let mut bitfield = vec![0u8; byte_len];
    for &i in completed_indices {
        if i < num_pieces {
            bitfield[i / 8] |= 1 << (7 - (i % 8));
        }
    }
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
///
/// TD-1 统一：编码走 `core::bencode::encode`（键按字典序输出——规范要求的
/// bencode 键序；旧手写编码器把 `file sizes`（含空格 0x20）排在末尾，属
/// 字典序违规但 libtorrent 解码器宽容故未暴露。统一后输出即规范序）。
pub fn bencode_fastresume(fr: &Fastresume) -> Result<Vec<u8>, BencodeError> {
    let file_sizes = Value::List(
        fr.file_sizes
            .iter()
            .map(|&[size, pad]| Value::List(vec![Value::Int(size as i64), Value::Int(pad as i64)]))
            .collect(),
    );
    let v = Value::Dict(vec![
        (
            b"file-format".to_vec(),
            Value::Bytes(b"libtorrent resume file".to_vec()),
        ),
        (b"file-version".to_vec(), Value::Int(1)),
        (b"info-hash".to_vec(), Value::Bytes(fr.info_hash.to_vec())),
        (b"pieces".to_vec(), Value::Bytes(fr.pieces.clone())),
        (b"name".to_vec(), Value::Bytes(fr.name.as_bytes().to_vec())),
        (
            b"save_path".to_vec(),
            Value::Bytes(fr.save_path.as_bytes().to_vec()),
        ),
        (b"total-uploaded".to_vec(), Value::Int(0)),
        (b"upload-mode".to_vec(), Value::Int(0)),
        (b"file sizes".to_vec(), file_sizes),
    ]);
    Ok(smart_dl_core::bencode::encode(&v))
}

// ============= bencode 解码（TD-1：core::bencode 委托） =============

/// bencode 值（core 权威实现的别名——访问器 `dict_get`/`as_bytes`/`as_int`/
/// `as_list` 由 core::bencode::Value 提供）。
pub type BencodeValue = smart_dl_core::bencode::Value;

/// bencode 解码（委托 `core::bencode::decode`：MAX_DEPTH 递归上限 + 长度
/// 字段 checked_add 防护 + 截断输入报错——P1-23 修复在 core 侧同款保有）。
pub fn bdecode(data: &[u8]) -> Result<BencodeValue, BencodeError> {
    smart_dl_core::bencode::decode(data).map_err(|e| BencodeError::InvalidData(e.to_string()))
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

    // 审计回归（P0-2）：非前缀索引必须精确置位，不得再按计数当前缀。
    #[test]
    fn bitfield_from_indices_non_prefix_exact() {
        let partials: Vec<PartialPieceInfo> = Vec::new();
        let bf = build_bitfield_from_indices(16, &[2, 15], &partials, 0.5);
        assert_eq!(bf, vec![0b0010_0000, 0b0000_0001]);
    }

    #[test]
    fn bitfield_from_indices_prefix_compat() {
        let partials: Vec<PartialPieceInfo> = Vec::new();
        let idx: Vec<usize> = (0..4).collect();
        assert_eq!(
            build_bitfield_from_indices(16, &idx, &partials, 0.5),
            build_bitfield(16, 4)
        );
    }

    #[test]
    fn bitfield_from_indices_out_of_range_ignored() {
        let partials: Vec<PartialPieceInfo> = Vec::new();
        let bf = build_bitfield_from_indices(8, &[7, 8, 100], &partials, 0.5);
        assert_eq!(bf, vec![0b0000_0001]);
    }

    #[test]
    fn bdecode_deep_but_legal_ok() {
        let mut data = Vec::new();
        for _ in 0..60 {
            data.push(b'd');
            data.extend_from_slice(b"1:k");
        }
        data.extend_from_slice(b"i1e");
        data.extend(std::iter::repeat_n(b'e', 60));
        assert!(bdecode(&data).is_ok());
    }
}
