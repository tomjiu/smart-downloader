//! `.bt.xltd` 验证与分析（真实样本验证版，A 级规格）。
//!
//! 真实格式（`spec_pending_validation.md` V1-V8 全绿，2026-08-17）：
//! - **无文件头 magic**：首字节即 piece 数据
//! - 大小 = `ceil(file_size / 4096) * 4096`（4096 对齐，样本双文件精确命中）
//! - 整文件预分配，未下载区域零填充（**非 NTFS sparse**；fsutil 显示全量分配）
//! - **核心模型：文件的位置镜像**（byte x of xltd ≡ byte x of target file）
//!
//! piece 数据物理偏移公式（V4，SHA1 验证 1866/1882 = 99.1%）：
//! ```text
//! xltd_offset = piece_index * piece_length - file_offset
//! ```
//! （仅 piece 完全落在文件内时适用；边界 piece 跨多文件，设计内排除）
//!
//! 完成状态判定：
//! - SHA1 一致 → piece 完成
//! - 窗口全零 → 未下载
//! - 部分非零但哈希不一致 → 在途（转换时视为未完成）
//!
//! 真实样本观测值（audio-books-cjk / C5AA...）：
//! - cover.jpg.xltd = 741376B（= ceil(740642/4096)*4096）
//! - SHA1 命中 1866/2263 pieces（~83% 下载进度）
//!
//! 已推翻的旧假设：
//! - ~~sparse hole 表达未下载~~ → 实际是全量分配 + 零填充
//! - ~~可能有文件头~~ → 无头，尺寸公式铁证
//! - ~~纯 piece 数据 sparse file~~ → 实际是文件位置镜像

use sha1::Digest;
use std::path::Path;

/// xltd 验证错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XltdError {
    /// 文件大小不是 4096 对齐。
    NotPageAligned,
    /// 读取文件失败。
    IoError,
    /// piece 哈希验证失败。
    PieceHashMismatch {
        piece_index: usize,
        expected: [u8; 20],
        actual: [u8; 20],
    },
    /// torrent 信息不足（缺少 piece_length 或 pieces_hash）。
    InsufficientTorrentInfo,
}

impl std::fmt::Display for XltdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPageAligned => write!(f, "xltd size is not 4096-byte aligned"),
            Self::IoError => write!(f, "I/O error reading xltd"),
            Self::PieceHashMismatch {
                piece_index,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "piece {} hash mismatch: expected {:x?}, actual {:x?}",
                    piece_index, expected, actual
                )
            }
            Self::InsufficientTorrentInfo => {
                write!(f, "torrent info missing piece_length or pieces_hash")
            }
        }
    }
}

impl std::error::Error for XltdError {}

/// xltd 文件分析结果。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct XltdAnalysis {
    /// 文件大小。
    pub file_size: u64,
    /// 是否是 4096 对齐。
    pub page_aligned: bool,
    /// 已下载 piece 数（通过 SHA1 验证）。
    pub completed_pieces: usize,
    /// 已完成 piece 的局部索引集（审计修复 P0-2 新增：原实现只累计计数，
    /// 位置信息丢失，多文件/非顺序进度被误当全局前缀置位 → 迁移静默数据
    /// 损坏）。`#[serde(default)]` 保证旧 JSON 报告可反序列化。
    #[serde(default)]
    pub completed_indices: Vec<usize>,
    /// 在途 piece 数（部分非零但哈希不匹配）。
    pub partial_pieces: usize,
    /// 未下载 piece 数（全零）。
    pub missing_pieces: usize,
    /// piece 哈希验证错误详情。
    pub mismatches: Vec<usize>,
    /// 在途 piece 详情：(piece_index, nonzero_bytes, total_bytes)。
    pub partial_details: Vec<(usize, usize, usize)>,
}

impl XltdAnalysis {
    /// 分析 xltd 文件（不验证 piece 哈希，只检查结构）。
    pub fn analyze(path: &Path) -> Result<Self, XltdError> {
        let metadata = std::fs::metadata(path).map_err(|_| XltdError::IoError)?;
        let file_size = metadata.len();
        let page_aligned = file_size % 4096 == 0;

        Ok(Self {
            file_size,
            page_aligned,
            ..Default::default()
        })
    }

    /// 用 torrent 信息验证 piece 哈希（逐 piece 扫描 xltd）。
    ///
    /// `piece_length` - torrent piece 长度
    /// `pieces_hash` - torrent piece 哈希列表（每个 20 字节）
    /// `file_offset` - 该 xltd 对应的文件在 torrent 中的起始偏移
    /// `file_size` - 该文件的实际大小
    pub fn verify_piece_hashes(
        &mut self,
        path: &Path,
        piece_length: u32,
        pieces_hash: &[[u8; 20]],
        file_offset: u64,
        file_size: u64,
    ) -> Result<(), XltdError> {
        if piece_length == 0 || pieces_hash.is_empty() {
            return Err(XltdError::InsufficientTorrentInfo);
        }

        let xltd_size = self.file_size;
        let expected_size = file_size.div_ceil(4096) * 4096;
        if xltd_size != expected_size {
            return Err(XltdError::NotPageAligned);
        }

        let mut f = std::fs::File::open(path).map_err(|_| XltdError::IoError)?;
        let piece_length = piece_length as usize;
        let mut completed = 0usize;
        let mut completed_indices = Vec::new();
        let mut partial = 0usize;
        let mut missing = 0usize;

        // batch3-P1：file_offset 语义修正——传入的 pieces_hash 为全局 piece 序列
        // （idx 按全 torrent 编号），本文件占据 [file_offset, file_offset+file_size)。
        // 旧实现把 piece_start 与「本文件局部 file_size」比较、offset 直接
        // saturating_sub：多文件 torrent 第 2+ 文件会把前一文件的 piece 哈希
        // 拿来对比本文件头部数据（假 partial），而本文件真正的 piece 因
        // `piece_start >= file_size` 全部被跳过（完成位图漏标 → 已下载数据重下）。
        let file_end = file_offset + file_size;
        for (idx, &expected_hash) in pieces_hash.iter().enumerate() {
            let piece_start = (idx as u64) * (piece_length as u64);
            let piece_end = piece_start + piece_length as u64;

            // 与本文件区间 [file_offset, file_end) 无交集的 piece 跳过
            if piece_end <= file_offset || piece_start >= file_end {
                continue;
            }
            // piece 与本文件相交的有效字节区间（相对本文件起点）
            let valid_start = piece_start.max(file_offset) - file_offset;
            let valid_end = piece_end.min(file_end) - file_offset;
            let valid_len = (valid_end - valid_start) as usize;

            // piece 在 xltd 中的偏移（xltd 是文件的位置镜像）
            let xltd_offset = valid_start;

            // 超出 xltd 范围（边界 piece）
            if xltd_offset + valid_len as u64 > xltd_size {
                continue;
            }

            let mut buf = vec![0u8; valid_len];
            use std::io::Read;
            use std::io::Seek;
            if f.seek(std::io::SeekFrom::Start(xltd_offset)).is_err()
                || f.read_exact(&mut buf).is_err()
            {
                missing += 1;
                continue;
            }

            let actual_hash = sha1::Sha1::digest(&buf);
            let actual_array: [u8; 20] = actual_hash.into();

            if actual_array == expected_hash {
                completed += 1;
                completed_indices.push(idx);
            } else if buf.iter().any(|&b| b != 0) {
                partial += 1;
                let nonzero = buf.iter().filter(|&&b| b != 0).count();
                self.partial_details.push((idx, nonzero, valid_len));
                self.mismatches.push(idx);
            } else {
                missing += 1;
            }
        }

        self.completed_pieces = completed;
        self.completed_indices = completed_indices;
        self.partial_pieces = partial;
        self.missing_pieces = missing;
        Ok(())
    }
}
