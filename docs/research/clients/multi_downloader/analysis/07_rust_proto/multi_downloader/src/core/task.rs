//! Task / Slice data structures — the in-memory mirror of Quark's
//! `DownloadTask` + `Slice` model
//! (see `analysis/05_quark/quark_architecture.md` §4.1).
//!
//! Key design choices inherited from Quark:
//!
//! - Every task carries a globally-unique `task_id: u64`.
//! - Each task contains a list of `Slice`s, each carrying its own
//!   `status` / `error_code` / `extra_error_code` / `retry_count` triple.
//! - A task may carry a `backup_url` + `backup_md5` that the engine swaps in
//!   when the primary URL exhausts its retries (Quark `use_backup` flow).
//!
//! Key differences from Quark:
//!
//! - We use SHA-256 (not MD5) for slice-level integrity by default.
//! - Slice state is persisted to SQLite (`storage::resume_db`), **not** into
//!   the download file — jettisoning FlashGet's `.jc!`-style header embedding.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::DownloadError;

/// Monotonic task-id generator (process-local; persisted task-ids live in
/// `storage::resume_db`).
static TASK_ID_SEQ: AtomicU64 = AtomicU64::new(1);

/// Allocate the next task id.
#[must_use]
pub fn next_task_id() -> u64 {
    TASK_ID_SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Top-level classification of a download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskKind {
    /// Plain HTTP(S) range download.
    Http,
    /// FTP / FTPS (placeholder — same shape as Http).
    Ftp,
    /// BitTorrent via magnet URI.
    Magnet,
    /// BitTorrent via .torrent file.
    Torrent,
    /// HTTP(S) HLS .m3u8 stream (FileCentipede-style, placeholder).
    Hls,
}

/// Status of an entire task (mirrors the Quark 7-stage state machine, but
/// flattened to its observable high-level state for the `DownloadTask` view;
/// see `state_machine.rs` for the full stage progression).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskStatus {
    /// Task created, not yet scheduled.
    Pending,
    /// FetchVersion / KillExistProcess stage.
    Initializing,
    /// Active download.
    Downloading,
    /// Installing / finalising (used by installer-style flow; for ordinary
    /// downloads this maps to "flushing + checksum").
    Installing,
    /// Setup / post-download hooks (e.g. write desktop entry, set perms).
    Setup,
    /// Completed successfully.
    Completed,
    /// Failed permanently.
    Failed,
    /// User-cancelled.
    Cancelled,
}

/// Per-slice status (FlashGet-style 6-state machine, see analysis §4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(i64)]
pub enum SliceStatus {
    /// Initial state.
    Pending = 0,
    /// Worker has been assigned.
    Downloading = 1,
    /// All bytes received, hash checked.
    Done = 2,
    /// Error encountered, retry in flight.
    Retrying = 3,
    /// Mirror died, awaiting reselection.
    MirrorFail = 4,
    /// Permanent failure; user intervention required.
    Corrupt = 5,
}

impl SliceStatus {
    /// True if the slice is in a terminal state.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Corrupt)
    }
}

/// A single byte-range slice of a [`DownloadTask`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Slice {
    /// Index within the parent task (0-based).
    pub index: u32,
    /// Absolute byte offset within the destination file.
    pub offset: u64,
    /// Slice length in bytes.
    pub length: u64,
    /// Number of bytes already downloaded into the destination file.
    pub downloaded: u64,
    /// Current state.
    pub status: SliceStatus,
    /// Active mirror id (`-1` = primary URL; FlashGet semantics).
    pub mirror_id: i32,
    /// Slice-level retry counter.
    pub retry_count: u32,
    /// Last error code (HTTP status or category code).
    pub error_code: i32,
    /// Last OS/TLS-level error code.
    pub extra_error_code: i32,
    /// Exponentially-moving-average speed in bytes/sec for this slice.
    pub speed_ema_bps: f64,
}

impl Slice {
    /// Create a fresh pending slice.
    #[must_use]
    pub fn new(index: u32, offset: u64, length: u64) -> Self {
        Self {
            index,
            offset,
            length,
            downloaded: 0,
            status: SliceStatus::Pending,
            mirror_id: -1,
            retry_count: 0,
            error_code: 0,
            extra_error_code: 0,
            speed_ema_bps: 0.0,
        }
    }

    /// Number of remaining bytes for this slice.
    #[must_use]
    pub fn remaining(&self) -> u64 {
        self.length.saturating_sub(self.downloaded)
    }

    /// Range start byte for the next HTTP Range request.
    #[must_use]
    pub fn range_start(&self) -> u64 {
        self.offset + self.downloaded
    }

    /// Range end byte (inclusive) for the next HTTP Range request.
    #[must_use]
    pub fn range_end(&self) -> u64 {
        self.offset + self.length - 1
    }

    /// Record that `n` more bytes were delivered for this slice.
    pub fn record_bytes(&mut self, n: u64, now_secs: f64) {
        self.downloaded = self.downloaded.saturating_add(n);
        if self.downloaded >= self.length {
            self.downloaded = self.length;
            self.status = SliceStatus::Done;
        }
        // EMA with alpha = 0.3.
        if now_secs > 0.0 {
            let inst = n as f64 / now_secs;
            self.speed_ema_bps = 0.7 * self.speed_ema_bps + 0.3 * inst;
        }
    }

    /// Mark this slice as failed with a Quark-style three-segment code.
    pub fn mark_failure(&mut self, err: &DownloadError) {
        self.error_code = err.error_code;
        self.extra_error_code = err.extra_error_code;
        self.retry_count = self.retry_count.saturating_add(1);
        self.status = if self.retry_count >= crate::error::MAX_RETRY {
            SliceStatus::Corrupt
        } else if err.category == crate::error::ErrorCategory::HttpServer {
            SliceStatus::MirrorFail
        } else {
            SliceStatus::Retrying
        };
    }
}

/// A complete download task — HTTP, FTP, or BT-flavoured. BT-specific fields
/// are kept as `Option`s so that HTTP tasks don't pay for them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadTask {
    /// Globally unique task id (Quark `task_id`).
    pub task_id: u64,
    /// Kind of task (determines engine dispatch).
    pub kind: TaskKind,
    /// Primary URL.
    pub url: Url,
    /// Optional backup URL (Quark `backup_url`).
    pub backup_url: Option<Url>,
    /// Optional backup MD5 hex (legacy Quark compatibility; we prefer SHA-256).
    pub backup_md5: Option<String>,
    /// Expected SHA-256 hex (32 bytes). If set, the engine verifies the final
    /// file post-download.
    pub expected_sha256: Option<String>,
    /// Destination path on disk.
    pub dest: Option<PathBuf>,
    /// Total file size in bytes (0 = unknown until first HEAD).
    pub total_size: u64,
    /// Default slice size used when splitting.
    pub slice_size: u64,
    /// Maximum parallel slices.
    pub concurrency: u32,
    /// Maximum retries per slice.
    pub max_retry_per_slice: u32,
    /// Optional referer header (sniffer-set).
    pub referer: Option<String>,
    /// Optional cookies (sniffer-set).
    pub cookies: Option<String>,
    /// Free-form metadata bag (mirror list, peer hints, …).
    pub meta: HashMap<String, String>,
    /// Current status.
    pub status: TaskStatus,
    /// Slices (kept under a lock because workers mutate them concurrently).
    #[serde(skip)]
    pub slices: Arc<RwLock<Vec<Slice>>>,
}

impl DownloadTask {
    /// Construct a new task with default config; call `with_*` to specialize.
    #[must_use]
    pub fn new(kind: TaskKind, url: Url) -> Self {
        Self {
            task_id: next_task_id(),
            kind,
            url,
            backup_url: None,
            backup_md5: None,
            expected_sha256: None,
            dest: None,
            total_size: 0,
            slice_size: crate::config::DEFAULT_SLICE_SIZE,
            concurrency: crate::config::DEFAULT_CONCURRENCY,
            max_retry_per_slice: crate::error::MAX_RETRY,
            referer: None,
            cookies: None,
            meta: HashMap::new(),
            status: TaskStatus::Pending,
            slices: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Builder-style: set concurrency.
    #[must_use]
    pub fn with_concurrency(mut self, c: u32) -> Self {
        self.concurrency = c.max(1);
        self
    }

    /// Builder-style: set backup URL.
    #[must_use]
    pub fn with_backup_url(mut self, u: Url) -> Self {
        self.backup_url = Some(u);
        self
    }

    /// Builder-style: set expected SHA-256 hex.
    #[must_use]
    pub fn with_expected_sha256(mut self, h: String) -> Self {
        self.expected_sha256 = Some(h);
        self
    }

    /// Builder-style: set destination path.
    #[must_use]
    pub fn with_dest(mut self, p: PathBuf) -> Self {
        self.dest = Some(p);
        self
    }

    /// Builder-style: set slice size.
    #[must_use]
    pub fn with_slice_size(mut self, s: u64) -> Self {
        self.slice_size = s.max(1);
        self
    }

    /// Builder-style: set referer.
    #[must_use]
    pub fn with_referer(mut self, r: String) -> Self {
        self.referer = Some(r);
        self
    }

    /// Compute the basename for the destination file based on the URL path.
    #[must_use]
    pub fn basename(&self) -> String {
        let path = self.url.path();
        let last = path.rsplit('/').next().filter(|s| !s.is_empty());
        last.map(str::to_string)
            .unwrap_or_else(|| format!("mdc_task_{}", self.task_id))
    }

    /// Number of slices already completed.
    #[must_use]
    pub fn slices_done(&self) -> usize {
        self.slices
            .read()
            .iter()
            .filter(|s| s.status == SliceStatus::Done)
            .count()
    }

    /// Bytes downloaded across all slices.
    #[must_use]
    pub fn bytes_done(&self) -> u64 {
        self.slices.read().iter().map(|s| s.downloaded).sum()
    }

    /// Slice the file into `Vec<Slice>` of length `slice_size` (final slice
    /// shorter if `total_size % slice_size != 0`).
    pub fn plan_slices(&self) -> Vec<Slice> {
        let total = self.total_size;
        if total == 0 {
            return Vec::new();
        }
        let ss = self.slice_size.max(1);
        let mut out = Vec::with_capacity((total / ss + 1) as usize);
        let mut offset = 0u64;
        let mut idx = 0u32;
        while offset < total {
            let len = ss.min(total - offset);
            out.push(Slice::new(idx, offset, len));
            offset += len;
            idx += 1;
        }
        out
    }

    /// Replace the slice list atomically.
    pub fn install_slices(&self, slices: Vec<Slice>) {
        *self.slices.write() = slices;
    }

    /// Snapshot the slices (cloned; cheap for typical slice counts).
    #[must_use]
    pub fn snapshot_slices(&self) -> Vec<Slice> {
        self.slices.read().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_split_correctly() {
        let t = DownloadTask::new(TaskKind::Http, Url::parse("https://x/y").unwrap())
            .with_slice_size(1_000_000);
        let t = DownloadTask {
            total_size: 2_500_000,
            ..t
        };
        let s = t.plan_slices();
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].length, 1_000_000);
        assert_eq!(s[1].length, 1_000_000);
        assert_eq!(s[2].length, 500_000);
        assert_eq!(s[2].offset, 2_000_000);
    }

    #[test]
    fn basename_falls_back_when_url_has_no_path() {
        let t = DownloadTask::new(TaskKind::Http, Url::parse("https://example.com").unwrap());
        assert_eq!(t.basename(), format!("mdc_task_{}", t.task_id));
        let t2 = DownloadTask::new(TaskKind::Http, Url::parse("https://x/file.zip").unwrap());
        assert_eq!(t2.basename(), "file.zip");
    }

    #[test]
    fn slice_records_bytes_and_completes() {
        let mut s = Slice::new(0, 0, 100);
        s.record_bytes(60, 1.0);
        assert_eq!(s.downloaded, 60);
        assert_eq!(s.status, SliceStatus::Downloading);
        s.record_bytes(40, 1.0);
        assert_eq!(s.status, SliceStatus::Done);
    }

    #[test]
    fn mark_failure_escalates_to_corrupt() {
        let mut s = Slice::new(0, 0, 10);
        let err = DownloadError::new(0, crate::error::ErrorCategory::Network, "reset")
            .with_extra(10054);
        for _ in 0..(crate::error::MAX_RETRY + 1) {
            s.mark_failure(&err);
        }
        assert_eq!(s.status, SliceStatus::Corrupt);
    }
}
