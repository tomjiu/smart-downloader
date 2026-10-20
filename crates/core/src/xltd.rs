//! 迅雷 .bt.xltd 下载进度文件（零/非零块启发式扫描）。
//!
//! ⚠️ 状态说明（架构审查 §6.5.1）：本模块是**早期研究雏形，当前未接线**
//!（生产路径使用 `xunlei-convert::xltd` 的 piece 级 SHA1 密码学校验，
//! 0 消费者）。保留作为启发式扫描参照；若未来接线须先补 e2e 验证。
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// block_size 上限（防外部输入的巨大分配：`vec![0u8; block_size as usize]`
/// 在 block_size = u64::MAX 时直接 OOM/panic）。16 MiB 与 torrent piece
/// 上限同口径，实际迅雷 .bt.xltd 块粒度为 4 KiB 系。
const MAX_BLOCK_SIZE: u64 = 16 * 1024 * 1024;

/// 一块的扫描结果。
#[derive(Debug, Clone, PartialEq)]
pub struct BlockStatus {
    /// 块在文件中的起始偏移。
    pub offset: u64,
    /// 块大小（字节）。
    pub len: u64,
    /// 是否含非零数据（true = 已下载）。
    pub has_data: bool,
}

/// xltd 文件的分析结果。
#[derive(Debug, Clone, PartialEq)]
pub struct XltdProgress {
    /// 目标文件总大小（= xltd 文件大小）。
    pub file_size: u64,
    /// 已下载（非零）字节总数。
    pub downloaded: u64,
    /// 进度 0.0 ~ 1.0。
    pub progress: f64,
    /// 连续的非零区间（合并相邻块）。
    pub data_ranges: Vec<(u64, u64)>, // (offset, len)
}

/// 分析一个 xltd 文件，按 block_size 字节分块扫描非零。
/// block_size 为 0 或超过 16 MiB → InvalidInput（防巨大分配）。
pub fn analyze(path: &Path, block_size: u64) -> std::io::Result<XltdProgress> {
    if block_size == 0 || block_size > MAX_BLOCK_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("block_size 须在 1..={MAX_BLOCK_SIZE}（得到 {block_size}）"),
        ));
    }
    let metadata = std::fs::metadata(path)?;
    let file_size = metadata.len();
    if file_size == 0 {
        return Ok(XltdProgress {
            file_size: 0,
            downloaded: 0,
            progress: 0.0,
            data_ranges: vec![],
        });
    }

    let mut downloaded = 0u64;
    let mut data_ranges = vec![];
    let mut cur_range_start: Option<u64> = None;

    let mut f = std::fs::File::open(path)?;
    let mut offset = 0u64;
    let mut buf = vec![0u8; block_size as usize];

    while offset < file_size {
        let remaining = file_size - offset;
        let to_read = std::cmp::min(remaining, block_size);
        let slice = &mut buf[..to_read as usize];

        f.seek(SeekFrom::Start(offset))?;
        let n = f.read(slice)?;
        if n == 0 {
            break;
        }

        let has_data = slice[..n].iter().any(|&b| b != 0);
        if has_data {
            downloaded += to_read;
            if cur_range_start.is_none() {
                cur_range_start = Some(offset);
            }
        } else {
            if let Some(start) = cur_range_start {
                data_ranges.push((start, offset - start));
                cur_range_start = None;
            }
        }

        offset += to_read;
    }

    if let Some(start) = cur_range_start {
        data_ranges.push((start, file_size - start));
    }

    let progress = if file_size == 0 {
        0.0
    } else {
        downloaded as f64 / file_size as f64
    };

    Ok(XltdProgress {
        file_size,
        downloaded,
        progress,
        data_ranges,
    })
}

/// 从 xltd 恢复成目标文件：非零块按原偏移写入，空洞补零。
/// 缺陷修复（架构审查 §6.5.1）：末尾不再 set_len 截断（尾部零区是合法数据）。
/// block_size 为 0 或超过 16 MiB → InvalidInput（防巨大分配）。
pub fn recover(src: &Path, dst: &Path, block_size: u64) -> std::io::Result<XltdProgress> {
    if block_size == 0 || block_size > MAX_BLOCK_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("block_size 须在 1..={MAX_BLOCK_SIZE}（得到 {block_size}）"),
        ));
    }
    let metadata = std::fs::metadata(src)?;
    let file_size = metadata.len();

    let mut f = std::fs::File::open(src)?;
    let mut out = std::fs::File::create(dst)?;
    out.set_len(file_size)?;

    let mut downloaded = 0u64;
    let mut data_ranges = vec![];
    let mut cur_range_start: Option<u64> = None;
    let mut last_data_end = 0u64;

    let mut buf = vec![0u8; block_size as usize];
    let mut offset = 0u64;

    while offset < file_size {
        let remaining = file_size - offset;
        let to_read = std::cmp::min(remaining, block_size);
        let slice = &mut buf[..to_read as usize];

        f.seek(SeekFrom::Start(offset))?;
        let n = f.read(slice)?;
        if n == 0 {
            break;
        }

        let has_data = slice[..n].iter().any(|&b| b != 0);
        if has_data {
            out.seek(SeekFrom::Start(offset))?;
            out.write_all(slice)?;
            downloaded += to_read;
            last_data_end = offset + to_read;
            if cur_range_start.is_none() {
                cur_range_start = Some(offset);
            }
        } else {
            if let Some(start) = cur_range_start {
                data_ranges.push((start, offset - start));
                cur_range_start = None;
            }
        }

        offset += to_read;
    }

    if let Some(start) = cur_range_start {
        data_ranges.push((start, file_size - start));
    }

    // 缺陷修复：不再截断到 last_data_end——尾部零区是合法数据（稀疏语义），
    // 恢复产物保持与 xltd 等长（开头 set_len(file_size) 已保证）。
    let _ = last_data_end; // 仅供调试观测（截断逻辑已移除）

    let progress = if file_size == 0 {
        0.0
    } else {
        downloaded as f64 / file_size as f64
    };

    Ok(XltdProgress {
        file_size,
        downloaded,
        progress,
        data_ranges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_xltd(path: &std::path::Path, size: u64, data: &[(u64, u8)]) {
        let mut f = std::fs::File::create(path).unwrap();
        f.set_len(size).unwrap();
        for &(off, b) in data {
            use std::io::Seek;
            f.seek(std::io::SeekFrom::Start(off)).unwrap();
            f.write_all(&[b]).unwrap();
        }
    }

    #[test]
    fn empty_file_zero_progress() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.xltd");
        make_xltd(&p, 0, &[]);
        let r = analyze(&p, 64 * 1024).unwrap();
        assert_eq!(r.file_size, 0);
        assert_eq!(r.downloaded, 0);
        assert_eq!(r.progress, 0.0);
        assert!(r.data_ranges.is_empty());
    }

    #[test]
    fn all_zero_file_full_holes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.xltd");
        make_xltd(&p, 128 * 1024, &[]); // 128KB 全零
        let r = analyze(&p, 64 * 1024).unwrap();
        assert_eq!(r.file_size, 128 * 1024);
        assert_eq!(r.downloaded, 0);
        assert_eq!(r.progress, 0.0);
        assert!(r.data_ranges.is_empty());
    }

    #[test]
    fn partially_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.xltd");
        // 128KB 文件，第一块(0-64KB)有数据，第二块(64-128KB)全零
        make_xltd(&p, 128 * 1024, &[(0, 0xAB), (10, 0xCD)]);
        let r = analyze(&p, 64 * 1024).unwrap();
        assert_eq!(r.file_size, 128 * 1024);
        // 第一块非零 → downloaded = 64KB（整块算已下载）
        assert_eq!(r.downloaded, 64 * 1024);
        assert!((r.progress - 0.5).abs() < 1e-9);
        assert_eq!(r.data_ranges, vec![(0, 64 * 1024)]);
    }

    #[test]
    fn recover_writes_data_at_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("t.xltd");
        let dst = dir.path().join("out.bin");
        // 64KB 文件，offset 100 处有非零字节
        make_xltd(&src, 64 * 1024, &[(100, 0xEE)]);
        let r = recover(&src, &dst, 64 * 1024).unwrap();
        assert_eq!(r.file_size, 64 * 1024);
        // 恢复后的文件：offset 100 处是 0xEE，其他是 0
        let data = std::fs::read(&dst).unwrap();
        assert_eq!(data.len(), 64 * 1024);
        assert_eq!(data[100], 0xEE);
        assert_eq!(data[0], 0x00);
        assert_eq!(data[99], 0x00);
        assert_eq!(data[101], 0x00);
    }

    #[test]
    fn trailing_partial_block_counted() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.xltd");
        // 100KB 文件（不是 64KB 整数倍），最后一块 36KB 有数据
        make_xltd(&p, 100 * 1024, &[(64 * 1024, 0x01)]);
        let r = analyze(&p, 64 * 1024).unwrap();
        assert_eq!(r.file_size, 100 * 1024);
        // 第一块(0-64KB)全零，最后一块(64-100KB)非零 → downloaded = 36KB
        assert_eq!(r.downloaded, 36 * 1024);
        assert_eq!(r.data_ranges, vec![(64 * 1024, 36 * 1024)]);
    }

    /// 回归：recover 不得截断尾部零区——目标文件以零结尾是合法数据
    /// （旧实现 set_len(last_data_end) 会把「头部数据 + 尾部合法零」的文件
    /// 截成只剩头部，静默丢失数据）。
    #[test]
    fn recover_keeps_trailing_legal_zeros() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("t.xltd");
        let dst = dir.path().join("out.bin");
        // 128KB：首块有数据，尾部 64KB 全零（可能是合法内容）
        make_xltd(&src, 128 * 1024, &[(0, 0xAA)]);
        let r = recover(&src, &dst, 64 * 1024).unwrap();
        assert_eq!(r.file_size, 128 * 1024);
        let data = std::fs::read(&dst).unwrap();
        assert_eq!(
            data.len(),
            128 * 1024,
            "恢复产物必须与 xltd 等长——尾部零区是合法数据不得截断"
        );
        assert_eq!(data[0], 0xAA);
        assert_eq!(data[64 * 1024], 0x00);
    }

    /// 回归：block_size 越界（0 / 超 16MiB）拒绝，防巨大分配。
    #[test]
    fn block_size_bounds_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.xltd");
        make_xltd(&p, 4096, &[(0, 1)]);
        assert!(analyze(&p, 0).is_err(), "block_size=0 须拒绝");
        assert!(analyze(&p, 16 * 1024 * 1024 + 1).is_err(), "超上限须拒绝");
        assert!(analyze(&p, 4096).is_ok(), "正常值不受影响");
        assert!(recover(&p, &p.with_extension("x"), 0).is_err());
        assert!(recover(&p, &p.with_extension("x"), u32::MAX as u64).is_err());
    }
}
