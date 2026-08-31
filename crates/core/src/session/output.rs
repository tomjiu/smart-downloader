//! 输出层（§12）：HTTP/FTP/云经 `.part` 落位，完成 rename；跨卷 rename 失败 → copy fallback + 删源。
//! 磁盘预检（D36 分段公式）：required = max(total×1.1, total + min(500MB, total))。

use std::fs;
use std::path::{Path, PathBuf};

const MB: u64 = 1024 * 1024;

/// 安全修复（V3，CWE-22/23）：统一相对路径净化——拒绝绝对路径、`..` 分量、
/// Windows 盘符/设备前缀与空路径。所有 `dest_root.join(rel)` 落盘点必须先过
/// 本函数；torrent 元数据（name / files[].path）同样以它校验，非法即拒任务。
pub fn sanitize_rel(rel: &str) -> Result<PathBuf, OutputError> {
    let pb = PathBuf::from(rel);
    if pb.as_os_str().is_empty() {
        return Err(OutputError::UnsafePath(rel.into()));
    }
    if pb.is_absolute() {
        return Err(OutputError::UnsafePath(rel.into()));
    }
    for comp in pb.components() {
        match comp {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            // ParentDir(..) / RootDir(/) / Prefix(C:) / 其他 → 一律拒绝
            _ => return Err(OutputError::UnsafePath(rel.into())),
        }
    }
    Ok(pb)
}

/// 防符号链接逃逸（V3 修复方案 3）：`dest` 相对 `root` 的每一级已存在中间目录
/// 与最终文件本身均不得为 symlink——否则 rename/copy 会写穿到 root 之外。
/// root 自身为 symlink 不在此检查（root 由配置指定，属运维意图）。
fn ensure_no_symlink_escape(root: &Path, dest: &Path) -> Result<(), OutputError> {
    let Ok(root_canon) = root.canonicalize() else {
        // root 不存在：落盘前的 create_dir_all 由调用链负责；此处跳过检查
        return Ok(());
    };
    // 从 root 下一级开始逐级检查到 dest 全路径
    let rel = match dest.strip_prefix(root) {
        Ok(r) => r,
        Err(_) => return Err(OutputError::UnsafePath(dest.display().to_string())),
    };
    let mut cur = root_canon.clone();
    for comp in rel.components() {
        cur.push(comp);
        if let Ok(md) = fs::symlink_metadata(&cur) {
            if md.file_type().is_symlink() {
                return Err(OutputError::UnsafePath(cur.display().to_string()));
            }
        }
    }
    Ok(())
}

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
    /// 安全修复（V3）：rel 先经 sanitize_rel 净化，非法路径直接报错。
    pub fn part_path(&self, rel: &str) -> Result<PathBuf, OutputError> {
        let rel_pb = sanitize_rel(rel)?;
        let mut s = self.dest_root.join(rel_pb).as_os_str().to_os_string();
        s.push(".part");
        Ok(PathBuf::from(s))
    }

    /// 完成落位：校验大小 → rename；rename 失败（典型：跨卷）→ copy fallback + 删源。
    /// 安全修复（V3）：rel 净化 + symlink 逃逸防护。
    pub fn finalize(&self, rel: &str, expected_size: u64) -> Result<(), OutputError> {
        let part = self.part_path(rel)?;
        let dest = self.dest_root.join(sanitize_rel(rel)?);
        ensure_no_symlink_escape(&self.dest_root, &dest)?;
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
    #[error("非法落盘路径（拒绝穿越/symlink 逃逸）: {0}")]
    UnsafePath(String),
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

#[cfg(test)]
mod sanitize_tests {
    use super::*;

    #[test]
    fn sanitize_accepts_normal_rel() {
        assert_eq!(sanitize_rel("a/b.bin").unwrap(), PathBuf::from("a/b.bin"));
        assert_eq!(sanitize_rel("./x.bin").unwrap(), PathBuf::from("./x.bin"));
        assert_eq!(sanitize_rel("单文件 名.bin").unwrap(), PathBuf::from("单文件 名.bin"));
    }

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize_rel("../etc/passwd").is_err());
        assert!(sanitize_rel("a/../../x").is_err());
        assert!(sanitize_rel("..").is_err());
        assert!(sanitize_rel("").is_err());
    }

    #[test]
    fn sanitize_rejects_absolute_and_prefix() {
        assert!(sanitize_rel("/etc/passwd").is_err());
        #[cfg(windows)]
        assert!(sanitize_rel(r"C:\Windows\x").is_err());
    }

    #[test]
    fn finalize_rejects_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("dl");
        fs::create_dir_all(&root).unwrap();
        // outside 目录 + root 内 symlink 指向它
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
            // 通过 symlink 目录落盘 → 必须拒绝
            let om = OutputManager::new(root.clone());
            let part = root.join("p.part");
            fs::write(&part, b"12345").unwrap();
            // 先把 .part 放到 link 下（模拟穿越写法不走 part_path：直接 finalize 路径检查）
            let r = om.finalize("link/evil.bin", 5);
            assert!(matches!(r, Err(OutputError::UnsafePath(_))), "got {r:?}");
            let _ = fs::remove_file(&part);
        }
    }
}
