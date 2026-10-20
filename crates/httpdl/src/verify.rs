//! ContentIdentity 校验（§14 Q-B5）：sha256/sha1/md5 仅用户/源提供时启用
//! （E25 三算法主源校验，互斥）；
//! 失败重下 1 次后降级接受（告警）。ETag+Content-Length 为准（校验在段层完成）。
//! 备用源切换后以 backup_md5（MD5）校验（夸克 backup_md5 机制）。

use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::io::{self, Read};
use std::path::Path;

/// batch3-P2：流式哈希缓冲（64KB）——旧实现 `std::fs::read` 整文件进内存，
/// 多 GB 任务 finalize 校验时内存峰值翻倍（小内存主机 OOM → 任务 Error 作废）。
const HASH_BUF: usize = 64 * 1024;

/// 流式读取文件，逐块喂给闭包。
fn for_each_chunk(path: &Path, mut f: impl FnMut(&[u8]) -> io::Result<()>) -> io::Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; HASH_BUF];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        f(&buf[..n])?;
    }
    Ok(())
}

/// 计算文件 SHA256（hex）。
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut h = Sha256::new();
    for_each_chunk(path, |b| {
        h.update(b);
        Ok(())
    })?;
    Ok(format!("{:x}", h.finalize()))
}

/// 校验文件内容与期望 SHA256 是否一致。文件缺失 → Err。
pub fn verify_file(path: &Path, expected_hex: &str) -> io::Result<bool> {
    Ok(sha256_file(path)? == expected_hex)
}

/// P0-4（防御纵深层）：无摘要任务的空洞文件兜底校验——完全稀疏
/// （`blocks == 0` 且 `size > 0`）= `set_len` 预分配后从未写入任何字节。
/// 写入操作总是分配块（即使写全零内容），故 blocks == 0 是「零写入」的
/// 铁证，不会误伤合法全零内容文件；长度检查因预分配天然相等而失效的
/// 场景（磁盘满时 set_len 稀疏成功 + 崩溃后账本续传跳过）由此捕获。
/// 空文件（size == 0）合法放行；块信息不可得的平台（非 Unix）保守放行
/// （无证据不定罪，段级 fsync 顺序保证仍是主防线）。
pub fn verify_nonempty_blocks(path: &Path) -> io::Result<bool> {
    let md = std::fs::metadata(path)?;
    if md.len() == 0 {
        return Ok(true); // 空文件：下载空内容合法
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if md.blocks() == 0 {
            tracing::warn!(
                "verify: {path:?} 完全稀疏（{} 字节零块分配）——疑似预分配空洞文件",
                md.len()
            );
            return Ok(false);
        }
    }
    Ok(true)
}

/// 计算文件 SHA1（hex，E25 主源校验算法之一）。
pub fn sha1_file(path: &Path) -> io::Result<String> {
    let mut h = Sha1::new();
    for_each_chunk(path, |b| {
        h.update(b);
        Ok(())
    })?;
    Ok(format!("{:x}", h.finalize()))
}

/// 校验文件内容与期望 SHA1 是否一致。文件缺失 → Err。
pub fn verify_file_sha1(path: &Path, expected_hex: &str) -> io::Result<bool> {
    Ok(sha1_file(path)? == expected_hex)
}

/// 计算文件 MD5（hex）。
pub fn md5_file(path: &Path) -> io::Result<String> {
    let mut h = Md5::new();
    for_each_chunk(path, |b| {
        h.update(b);
        Ok(())
    })?;
    Ok(format!("{:x}", h.finalize()))
}

/// 校验文件内容与期望 MD5 是否一致。文件缺失 → Err。
pub fn verify_file_md5(path: &Path, expected_hex: &str) -> io::Result<bool> {
    Ok(md5_file(path)? == expected_hex)
}
