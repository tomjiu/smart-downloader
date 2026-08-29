//! 输出层（§12）：HTTP/FTP/云经 `.part` 落位，完成 rename；跨卷 rename 失败 → copy fallback + 删源。
//! 磁盘预检（D36 分段公式）：required = max(total×1.1, total + min(500MB, total))。

use std::fs;
use std::path::{Path, PathBuf};

const MB: u64 = 1024 * 1024;

/// 输出管理器：`dest_root/<rel>.part` → 完成时 rename 为 `dest_root/<rel>`。
#[derive(Clone, Debug)]
pub struct OutputManager {
    dest_root: PathBuf,
}

impl OutputManager {
    pub fn new(dest_root: PathBuf) -> Self {
        OutputManager { dest_root }
    }

    /// `.part` 路径：`dest_root/<rel>.part`（追加后缀，不替换已有扩展名）。
    pub fn part_path(&self, rel: &str) -> PathBuf {
        let mut s = self.dest_root.join(rel).as_os_str().to_os_string();
        s.push(".part");
        PathBuf::from(s)
    }

    /// 完成落位：校验大小 → rename；rename 失败（典型：跨卷）→ copy fallback + 删源。
    pub fn finalize(&self, rel: &str, expected_size: u64) -> Result<(), OutputError> {
        let part = self.part_path(rel);
        let dest = self.dest_root.join(rel);
        self.finalize_to(&part, &dest, expected_size)
    }

    /// 显式路径版（测试与 daemon 注入用）。
    ///
    /// Bug C 修复：目标已存在且大小一致时，不再直接 Ok 短路——先清理可能残留的
    /// `.part` 文件，避免 BT Seeder 锁文件场景下落位不完整。
    pub fn finalize_to(
        &self,
        part: &Path,
        dest: &Path,
        expected_size: u64,
    ) -> Result<(), OutputError> {
        // 幂等：目标已落位且大小一致 → 清理 .part 后 Ok（完成信号可能重复投递）
        if dest.exists() {
            let dl = fs::metadata(dest).map_err(OutputError::Io)?.len();
            if dl == expected_size {
                let _ = fs::remove_file(part);
                return Ok(());
            }
        }
        if !part.exists() {
            return Err(OutputError::PartMissing);
        }
        let pl = fs::metadata(part).map_err(OutputError::Io)?.len();
        if pl != expected_size {
            return Err(OutputError::SizeMismatch {
                expected: expected_size,
                actual: pl,
            });
        }
        match fs::rename(part, dest) {
            Ok(()) => Ok(()),
            // Windows 跨卷 rename 返回错误 → copy + 校验 + 删源（§12 跨盘）
            Err(_) => self.copy_fallback(part, dest, expected_size),
        }
    }

    /// 跨盘 fallback：copy → 校验长度 → 删除源 `.part`。失败时保留源。
    pub fn copy_fallback(
        &self,
        part: &Path,
        dest: &Path,
        expected_size: u64,
    ) -> Result<(), OutputError> {
        let pl = fs::metadata(part).map_err(OutputError::Io)?.len();
        if pl != expected_size {
            return Err(OutputError::SizeMismatch {
                expected: expected_size,
                actual: pl,
            });
        }
        fs::copy(part, dest).map_err(OutputError::Io)?;
        let dl = fs::metadata(dest).map_err(OutputError::Io)?.len();
        if dl != expected_size {
            return Err(OutputError::SizeMismatch {
                expected: expected_size,
                actual: dl,
            });
        }
        fs::remove_file(part).map_err(OutputError::Io)?;
        Ok(())
    }
}

#[derive(thiserror::Error, Debug)]
pub enum OutputError {
    #[error(".part 文件缺失")]
    PartMissing,
    #[error("大小不符: 期望 {expected}, 实际 {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// 磁盘预检所需空间（D36）：`max(total×1.1, total + min(500MB, total))`。
pub fn required_disk(total: u64) -> u64 {
    let ten_pct = total * 11 / 10;
    let plus_min = total + total.min(500 * MB);
    ten_pct.max(plus_min)
}

/// 磁盘预检结果。
#[derive(Debug, PartialEq, Eq)]
pub enum DiskCheck {
    Ok,
    Insufficient { required: u64, available: u64 },
}

/// 预检：剩余空间不足 required → 拒绝入队（由调用方在入队前调用）。
pub fn evaluate_disk(available: u64, total: u64) -> DiskCheck {
    let required = required_disk(total);
    if available >= required {
        DiskCheck::Ok
    } else {
        DiskCheck::Insufficient {
            required,
            available,
        }
    }
}
