//! 单实例锁（守护进程互斥）：`fs2` 排他文件锁。
//! 重复启动时 `try_lock_exclusive` 失败 → `AlreadyRunning`（附锁内 pid，供诊断）。
//! 锁文件内容 = 当前 pid（文本，供运维排查）；Drop 时解锁并删除锁文件。

use fs2::FileExt as _;
use std::fs::{self, File, OpenOptions};
#[cfg(test)]
use std::io::Read;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("daemon 已在运行 (锁持有者 pid={pid})")]
    AlreadyRunning { pid: u32 },
    #[error("锁文件 io 错误: {0}")]
    Io(#[from] io::Error),
}

/// 已持有的单实例锁（未 Drop 前保持排他）。
#[derive(Debug)]
pub struct InstanceLock {
    file: File,
    path: PathBuf,
}

impl InstanceLock {
    /// 获取排他锁；已被其他实例持有 → `AlreadyRunning`。
    /// 锁文件缺失时自动创建（含父目录不存在 → Io 错误，调用方决定是否建目录）。
    pub fn acquire(path: &Path) -> Result<Self, LockError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                // 记录当前 pid（截断重写；失败不阻断锁本身）
                let _ = file.set_len(0);
                let _ = writeln!(&file, "{}", std::process::id());
                Ok(InstanceLock {
                    file,
                    path: path.to_path_buf(),
                })
            }
            Err(_) => {
                // Windows 排他锁会拒绝其他句柄读取 → pid 仅尽力（常见 0）
                let pid = fs::read_to_string(path)
                    .ok()
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(0);
                Err(LockError::AlreadyRunning { pid })
            }
        }
    }
}

/// 锁文件 pid 读取（诊断用；仅测试路径调用，生产冲突分支直接 `read_to_string`）。
#[cfg(test)]
fn read_pid(f: &File) -> Option<u32> {
    let mut s = String::new();
    let mut borrowed = f;
    borrowed.read_to_string(&mut s).ok()?;
    s.trim().parse().ok()
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // 审查修复（batch10-F，经典 unlink 竞态）：不再删除锁文件——
        // 原实现「unlock → remove_file」两步间，B 进程可打开同一 inode
        // 并加锁成功；A 随后删掉路径；C 启动时 create(true) 新建 inode
        // 并加锁成功 → B 与 C 同时运行（单实例语义失效）。
        // 锁文件残留无害：判据是 try_lock_exclusive（同 inode 才互斥），
        // 删除反而制造 inode 竞争窗口。只 unlock，不 unlink。
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_rejects_second_instance() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");
        let first = InstanceLock::acquire(&path).unwrap();

        // 同进程二次获取 → AlreadyRunning（Windows 上锁冲突句柄读不到 pid → 仅断言类型）
        match InstanceLock::acquire(&path) {
            Err(LockError::AlreadyRunning { .. }) => {}
            other => panic!("期望 AlreadyRunning，得到 {other:?}"),
        }
        drop(first);
    }

    #[test]
    fn drop_releases_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");
        {
            let _first = InstanceLock::acquire(&path).unwrap();
        }
        // 释放后可重新获取
        let second = InstanceLock::acquire(&path).unwrap();
        assert!(second.file.metadata().is_ok());
    }

    #[test]
    fn lock_file_survives_drop_but_unlocks() {
        // 审查修复（batch10-F）语义更新：Drop 只 unlock 不删文件——
        // 删除会制造「B 锁旧 inode / C 建新 inode」双实例窗口（经典
        // unlink 竞态）。锁文件残留无害（判据是 flock 互斥）。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");
        {
            let _lock = InstanceLock::acquire(&path).unwrap();
            assert!(path.exists(), "持有期间锁文件存在");
        }
        assert!(path.exists(), "Drop 后锁文件保留（防 inode 竞态）");
        // unlock 生效：释放后可重新加锁
        let _second = InstanceLock::acquire(&path).unwrap();
    }

    #[test]
    fn missing_parent_dir_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no/such/dir/daemon.lock");
        assert!(matches!(
            InstanceLock::acquire(&path),
            Err(LockError::Io(_))
        ));
    }

    #[test]
    fn stale_pid_readable_without_lock() {
        // 无锁持有时可读旧 pid（Windows 锁冲突时读不到 → 此路径为跨平台诊断基础）
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");
        let mut f = File::create(&path).unwrap();
        f.write_all(b"424242\n").unwrap();
        drop(f);
        let f2 = File::open(&path).unwrap();
        assert_eq!(read_pid(&f2), Some(424242));

        // acquire 成功 → 覆盖为新 pid（写同一句柄自己锁的区域允许）
        let lock = InstanceLock::acquire(&path).unwrap();
        drop(lock);
    }
}
