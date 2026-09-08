//! `.xlbt.cfg` 解析器（真实样本验证版，A 级规格）。
//!
//! 真实格式（`spec_pending_validation.md` V1-V8 全绿，2026-08-17）：
//! - magic = `XDLCTX\x00\x00`（非旧推测的 `XLBTCFG`）
//! - 0x08-0x17：16B 任务随机区（opaque）
//! - 0x18-0x37：8 个 u32 头部字段（部分语义已解码，部分仍为 B/C 级未知）
//! - 0x38-0x3B：infohash 字符串长度（固定 40）
//! - 0x3C-0x63：40B ASCII infohash（= torrent v1 info_hash）
//! - 0x64 起：TLV 记录
//!   - tag-02：`02 00 <key:le16> <val:le32>`，8B/entry；key=1 = 已下载 piece 数
//!   - tag-04：`04 00 <len:le32> <data>`，blob 记录（语义未完全解码）
//!   - 内嵌文件大小 u64 记录
//!   - `bt://ip:port` peer 缓存字符串
//!
//! 关键否定结论（A 级）：
//! - **无 piece 哈希表**（32KB 物理装不下 45KB 哈希，231 个 20B blob 零匹配）
//! - **无 bitfield/完成位图**（状态由 .bt.xltd 零区 + SHA1 推导）
//! - **无 section 数组**（旧反汇编推断 40B 头 + 20B/entry section 已被推翻）
//!
//! 遗留 B/C 级：头部 0x08-0x3B 字段语义；tag-02 key 2..2200；64KB 块记录；
//! tag-04 blob 语义。不影响 BT 接续转换。
//!
//! 真实样本观测值（audio-books-cjk / C5AA...，analysis.json）：
//! - 0x18=30025, 0x1C=7, 0x20=0, 0x24=4, 0x28=0, 0x2C=28584, 0x30=4, 0x34=262145
//! - 0x38 固定为 infohash 长度 40
//! - tag-02 key=1=已下载 piece 数；key 100/19700/42192/22490 等非零值语义未知
//! - tag-04：231 个 20B blob 无一匹配 piece 哈希；另有 8B "Reserved" 标签 x4
//! - 64KB 块记录偏移 0x4968 起（`65536×n+2` 序列），解码未完成

use std::fmt;
use std::path::Path;

/// 解析错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XlbtCfgError {
    /// magic 不匹配（期望 `XDLCTX\x00\x00`）。
    BadMagic,
    /// 文件过小，无法包含基本头部（< 0x64）。
    TooSmall,
    /// infohash 长度字段不是 40。
    BadInfoHashLen,
    /// infohash 区域包含非 ASCII 字符。
    BadInfoHash,
    /// tag-02 int 记录截断。
    TruncatedTag02,
    /// tag-04 blob 记录截断。
    TruncatedTag04,
}

impl fmt::Display for XlbtCfgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => write!(f, "bad magic (expected XDLCTX\\x00\\x00)"),
            Self::TooSmall => write!(f, "file too small for header"),
            Self::BadInfoHashLen => write!(f, "infohash length field is not 40"),
            Self::BadInfoHash => write!(f, "infohash contains non-ASCII bytes"),
            Self::TruncatedTag02 => write!(f, "truncated tag-02 record"),
            Self::TruncatedTag04 => write!(f, "truncated tag-04 blob record"),
        }
    }
}

impl std::error::Error for XlbtCfgError {}

/// `.xlbt.cfg` 解析结果（保守：只输出可证明的字段）。
///
/// 真实布局（A 级，真实样本 32KB）：
/// - 0x00-0x07：magic `XDLCTX\x00\x00`
/// - 0x08-0x17：16B 任务随机区
/// - 0x18-0x37：8 个 u32 头部字段
/// - 0x38-0x3B：infohash 长度（40）
/// - 0x3C-0x63：40B ASCII infohash
/// - 0x64 起：TLV 记录（tag-02 / tag-04）+ peer 缓存字符串
#[derive(Debug, Clone, Default, PartialEq)]
pub struct XlbtCfg {
    /// 0x08-0x17：16B 任务随机区（opaque，疑似 task uuid）。
    pub task_random: [u8; 16],
    /// 头部 u32 字段（按偏移索引）。
    ///
    /// 真实样本固定值（C5AA... 样本，e2e 合成样本复用了同一组常量）：
    /// - 0x18 = 30025
    /// - 0x1C = 7
    /// - 0x20 = 0
    /// - 0x24 = 4
    /// - 0x28 = 0
    /// - 0x2C = 28584
    /// - 0x30 = 4
    /// - 0x34 = 262145
    /// - 0x38 = 40（infohash 字符串长度，唯一已确认语义）
    ///
    /// 其余字段是否随版本/任务变化，以及是否编码 piece/file 元信息，当前为 B/C 级未知。
    pub header_u32s: Vec<(u32, u32)>, // (offset, value)
    /// 0x3C 起的 40B ASCII infohash（= torrent v1 info_hash）。
    pub info_hash: String,
    /// tag-02 int 记录表（`02 00 <key:le16> <val:le32>`），自 0x64 起。
    /// key=1 = 已下载 piece 数；key 2..2200 语义未解码。
    pub int_records: Vec<(u16, u32)>,
    /// tag-04 blob 记录长度表（`04 00 <len:le32> <data>`），扫描全文件。
    ///
    /// 真实样本统计（C5AA...）：
    /// - 共 231 个 20B blob，无一匹配 torrent piece 哈希
    /// - 另有 4 个 8B "Reserved" 标签（0x700C 起）
    /// - 0x4968 起存在 `65536×n+2` 序列的 64KB 块记录，解码未完成
    ///
    /// 当前策略：只记录长度，不硬编码语义；不影响 BT 接续转换。
    pub blob_lengths: Vec<u32>,
    /// peer 缓存地址（`bt://ip:port` 字符串）。
    pub peers: Vec<String>,
    /// 原始文件大小。
    pub file_size: usize,
}

impl XlbtCfg {
    /// 从文件路径解析 `.xlbt.cfg`。
    pub fn from_path(path: &Path) -> Result<Self, XlbtCfgError> {
        let data = std::fs::read(path).map_err(|_| XlbtCfgError::TooSmall)?;
        Self::parse(&data)
    }

    /// 从字节数组解析 `.xlbt.cfg`。
    pub fn parse(data: &[u8]) -> Result<Self, XlbtCfgError> {
        let size = data.len();
        if size < 0x64 {
            return Err(XlbtCfgError::TooSmall);
        }

        // 1. magic
        let magic = &data[0x00..0x08];
        if magic != b"XDLCTX\x00\x00" {
            return Err(XlbtCfgError::BadMagic);
        }

        // 2. 随机区
        let mut task_random = [0u8; 16];
        task_random.copy_from_slice(&data[0x08..0x18]);

        // 3. 头部 u32 字段（已知偏移）
        let mut header_u32s = Vec::new();
        for off in [0x18u32, 0x1C, 0x24, 0x2C, 0x30, 0x34, 0x38] {
            let off = off as usize;
            if off + 4 <= size {
                let val =
                    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
                header_u32s.push((off as u32, val));
            }
        }

        // 4. infohash
        let ih_len_offset = 0x38;
        let ih_len = u32::from_le_bytes([
            data[ih_len_offset],
            data[ih_len_offset + 1],
            data[ih_len_offset + 2],
            data[ih_len_offset + 3],
        ]) as usize;
        if ih_len != 40 {
            return Err(XlbtCfgError::BadInfoHashLen);
        }
        let ih_offset = 0x3C;
        let ih_bytes = &data[ih_offset..ih_offset + 40];
        if !ih_bytes
            .iter()
            .all(|&b| b.is_ascii() && (b.is_ascii_hexdigit() || b == 0))
        {
            return Err(XlbtCfgError::BadInfoHash);
        }
        let info_hash = String::from_utf8_lossy(ih_bytes).to_uppercase();

        // 5. tag-02 int 记录表（0x64 起）
        let mut int_records = Vec::new();
        let mut i = 0x64usize;
        while i + 8 <= size && data[i..i + 2] == [0x02, 0x00] {
            let key = u16::from_le_bytes([data[i + 2], data[i + 3]]);
            let val = u32::from_le_bytes([data[i + 4], data[i + 5], data[i + 6], data[i + 7]]);
            int_records.push((key, val));
            i += 8;
        }

        // 6. tag-04 blob 记录（扫描整个文件）
        let mut blob_lengths = Vec::new();
        let mut j = 0usize;
        while j + 6 <= size && data[j..j + 2] == [0x04, 0x00] {
            let len =
                u32::from_le_bytes([data[j + 2], data[j + 3], data[j + 4], data[j + 5]]) as usize;
            blob_lengths.push(len as u32);
            j += 6 + len;
        }

        // 7. peer 缓存（bt://ip:port 字符串）
        let peers = find_peer_strings(data);

        Ok(Self {
            task_random,
            header_u32s,
            info_hash,
            int_records,
            blob_lengths,
            peers,
            file_size: size,
        })
    }

    /// 获取已下载 piece 数（key=1 的 tag-02 记录）。
    pub fn downloaded_piece_count(&self) -> Option<u32> {
        self.int_records
            .iter()
            .find(|(k, _)| *k == 1)
            .map(|(_, v)| *v)
    }
}

/// 在 cfg 数据中查找 `bt://ip:port` 字符串。
///
/// 当前实现只提取地址字符串，不解析内部端口/保留字段语义；
/// 这些 peer 记录仅作为转换后的可选 peer 提示，不参与必须性校验。
fn find_peer_strings(data: &[u8]) -> Vec<String> {
    let mut peers = Vec::new();
    let prefix = b"bt://";
    let mut start = 0usize;
    while let Some(pos) = data[start..]
        .windows(prefix.len())
        .position(|w| w == prefix)
    {
        let abs_pos = start + pos;
        let rest = &data[abs_pos + prefix.len()..];
        let mut end = 0usize;
        while end < rest.len()
            && (rest[end].is_ascii_digit() || rest[end] == b'.' || rest[end] == b':')
        {
            end += 1;
        }
        if end > 0 {
            if let Ok(s) = std::str::from_utf8(&rest[..end]) {
                peers.push(format!("bt://{}", s));
            }
            start = abs_pos + prefix.len() + end;
            continue;
        }
        start = abs_pos + prefix.len();
    }
    peers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_header() {
        // 构造最小合法 .xlbt.cfg（仅头部 + 1 个 tag-02）
        let mut data = Vec::new();
        data.extend_from_slice(b"XDLCTX\x00\x00"); // magic
        data.extend_from_slice(&[0u8; 16]); // 随机区
        data.extend_from_slice(&30025u32.to_le_bytes()); // 0x18
        data.extend_from_slice(&7u32.to_le_bytes()); // 0x1C
        data.extend_from_slice(&0u32.to_le_bytes()); // 0x20
        data.extend_from_slice(&4u32.to_le_bytes()); // 0x24
        data.extend_from_slice(&0u32.to_le_bytes()); // 0x28
        data.extend_from_slice(&28584u32.to_le_bytes()); // 0x2C
        data.extend_from_slice(&4u32.to_le_bytes()); // 0x30
        data.extend_from_slice(&262145u32.to_le_bytes()); // 0x34
        data.extend_from_slice(&40u32.to_le_bytes()); // 0x38 infohash len
        data.extend_from_slice(b"ABCDEF0123456789abcdef0123456789ABCDEF01"); // 0x3C infohash
                                                                             // tag-02: key=1, val=1868
        data.extend_from_slice(&[0x02, 0x00]);
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&1868u32.to_le_bytes());

        let cfg = XlbtCfg::parse(&data).unwrap();
        assert_eq!(cfg.info_hash, "ABCDEF0123456789ABCDEF0123456789ABCDEF01");
        assert_eq!(cfg.downloaded_piece_count(), Some(1868));
        assert_eq!(cfg.int_records.len(), 1);
    }

    #[test]
    fn reject_bad_magic() {
        let mut data = vec![0u8; 0x64];
        data[0..8].copy_from_slice(b"BADMAGIC");
        assert!(matches!(XlbtCfg::parse(&data), Err(XlbtCfgError::BadMagic)));
    }

    #[test]
    fn reject_too_small() {
        assert!(matches!(
            XlbtCfg::parse(&[0u8; 10]),
            Err(XlbtCfgError::TooSmall)
        ));
    }

    #[test]
    fn reject_bad_infohash_len() {
        let mut data = Vec::new();
        data.extend_from_slice(b"XDLCTX\x00\x00");
        data.extend_from_slice(&[0u8; 16]);
        for _ in 0..7 {
            data.extend_from_slice(&0u32.to_le_bytes());
        }
        data.extend_from_slice(&0u32.to_le_bytes()); // 0x38 = 0
        data.extend_from_slice(&[0u8; 40]);
        // 填充到 0x64 以上，确保进入 infohash 长度检查
        while data.len() < 0x64 {
            data.push(0);
        }

        assert!(matches!(
            XlbtCfg::parse(&data),
            Err(XlbtCfgError::BadInfoHashLen)
        ));
    }
}
