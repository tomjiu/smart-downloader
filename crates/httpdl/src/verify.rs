//! ContentIdentity 校验（§14 Q-B5）：sha256/sha1/md5 仅用户/源提供时启用
//! （E25 三算法主源校验，互斥）；
//! 失败重下 1 次后降级接受（告警）。ETag+Content-Length 为准（校验在段层完成）。
//! 备用源切换后以 backup_md5（MD5）校验（夸克 backup_md5 机制）。

use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::io;
use std::path::Path;

/// 计算文件 SHA256（hex）。
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(format!("{:x}", h.finalize()))
}

/// 校验文件内容与期望 SHA256 是否一致。文件缺失 → Err。
pub fn verify_file(path: &Path, expected_hex: &str) -> io::Result<bool> {
    Ok(sha256_file(path)? == expected_hex)
}

/// 计算文件 SHA1（hex，E25 主源校验算法之一）。
pub fn sha1_file(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    let mut h = Sha1::new();
    h.update(&bytes);
    Ok(format!("{:x}", h.finalize()))
}

/// 校验文件内容与期望 SHA1 是否一致。文件缺失 → Err。
pub fn verify_file_sha1(path: &Path, expected_hex: &str) -> io::Result<bool> {
    Ok(sha1_file(path)? == expected_hex)
}

/// 计算文件 MD5（hex）。
pub fn md5_file(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    let mut h = Md5::new();
    h.update(&bytes);
    Ok(format!("{:x}", h.finalize()))
}

/// 校验文件内容与期望 MD5 是否一致。文件缺失 → Err。
pub fn verify_file_md5(path: &Path, expected_hex: &str) -> io::Result<bool> {
    Ok(md5_file(path)? == expected_hex)
}
