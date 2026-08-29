//! Atomic file IO — `pwrite`-based append + sparse-file preallocation.
//!
//! Borrows FlashGet's `pwrite` pattern (analysis §4.4) so multiple slice
//! workers can write into the same destination file concurrently without
//! needing locks at the byte-offset level. We do **not** embed any metadata
//! into the file itself (jettisoning FlashGet's `.jc!` header — analysis §9).

use std::io::{Seek, SeekFrom};
use std::path::Path;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::error::{DownloadError, ErrorCategory, Result};

/// Wraps a destination file with offset-based writes and optional
/// preallocation.
pub struct AtomicFile {
    inner: Mutex<tokio::fs::File>,
}

impl AtomicFile {
    /// Open (creating if absent) the destination file with read+write
    /// permissions.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let f = tokio::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)
            .await?;
        Ok(Self {
            inner: Mutex::new(f),
        })
    }

    /// Preallocate `size` bytes via `fallocate` (Linux) or sparse seek+1
    /// (fallback). Avoids fragmentation when the destination FS supports it.
    pub async fn preallocate(&self, size: u64) -> Result<()> {
        let f = self.inner.lock();
        // Try Linux's posix_fallocate via std::os::linux::fs::FileExt; fall
        // back to set_len (sparse) on non-Linux Unix and on Windows.
        #[cfg(target_os = "linux")]
        {
            use std::os::linux::fs::FileExt;
            let std_file = f.try_clone().await?.into_std().await;
            std_file
                .allocate(0, size)
                .or_else(|_| {
                    // Fallback: set_len (sparse file).
                    std_file.set_len(size)
                })
                .map_err(|e| DownloadError::new(0, ErrorCategory::Io, e.to_string()))?;
        }
        #[cfg(not(target_os = "linux"))]
        {
            f.set_len(size).await?;
        }
        Ok(())
    }

    /// Write `bytes` at the given absolute `offset` (concurrent-safe `pwrite`).
    pub async fn pwrite(&self, bytes: &[u8], offset: u64) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            let f = self.inner.lock();
            // Tokio's Mutex gives us a `tokio::fs::File`, but `FileExt::write_at`
            // is on `std::fs::File`. We extract via `try_clone().into_std()`.
            let std_file = f.try_clone().await?.into_std().await;
            std_file
                .write_at(bytes, offset)
                .map_err(|e| DownloadError::new(0, ErrorCategory::Io, e.to_string()))?;
        }
        #[cfg(not(unix))]
        {
            let mut f = self.inner.lock();
            f.seek(SeekFrom::Start(offset)).await?;
            f.write_all(bytes).await?;
        }
        Ok(())
    }

    /// Flush + fsync for durability.
    pub async fn flush(&self) -> Result<()> {
        let mut f = self.inner.lock();
        f.flush().await?;
        f.sync_all().await?;
        Ok(())
    }

    /// Read a range of bytes (for hash verification).
    pub async fn pread(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let mut f = self.inner.lock();
        f.seek(SeekFrom::Start(offset)).await?;
        let mut buf = vec![0u8; len];
        let n = f.read(&mut buf).await?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Atomic rename — useful for "write to .part then rename" workflows
    /// (Quark uses this for installer file replacement; we use it to atomically
    /// commit a completed file).
    pub async fn atomic_rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> Result<()> {
        tokio::fs::rename(from.as_ref(), to.as_ref())
            .await
            .map_err(|e| DownloadError::new(0, ErrorCategory::Io, e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn pwrite_writes_at_offset() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let f = AtomicFile::open(&path).await.unwrap();
        f.preallocate(1024).await.unwrap();
        f.pwrite(b"hello", 0).await.unwrap();
        f.pwrite(b"world", 5).await.unwrap();
        f.flush().await.unwrap();
        drop(f);
        let bytes = tokio::fs::read(&path).await.unwrap();
        assert_eq!(&bytes[..5], b"hello");
        assert_eq!(&bytes[5..10], b"world");
    }

    #[tokio::test]
    async fn pread_returns_written_range() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("g.bin");
        let f = AtomicFile::open(&path).await.unwrap();
        f.preallocate(64).await.unwrap();
        f.pwrite(b"abc", 10).await.unwrap();
        let read = f.pread(10, 3).await.unwrap();
        assert_eq!(read, b"abc");
    }
}
