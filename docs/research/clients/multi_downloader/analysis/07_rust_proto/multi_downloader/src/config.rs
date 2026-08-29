//! Persistent configuration system.
//!
//! Borrows the SQLite-WAL persistence idea from FileCentipede (analysis §2.1,
//! table row "Configuration persistence") and Quark's local JSON config, while
//! **explicitly rejecting** Quark's CMS remote-pull channel (analysis §7.2) —
//! remote config push is a privacy / supply-chain attack vector that we will
//! not replicate.
//!
//! The SQLite database lives at `<config_dir>/mdc.db` and uses WAL mode for
//! crash-safe concurrent reads during downloads.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::{DownloadError, ErrorCategory, Result};

/// Default per-slice size for HTTP downloads (4 MiB — matches Quark's
/// inferred slice size from `analysis/05_quark/quark_architecture.md` §4.1).
pub const DEFAULT_SLICE_SIZE: u64 = 4 * 1024 * 1024;

/// Default number of concurrent HTTP slices per task (matches FlashGet 1.x
/// "5 connections" default; see `analysis/03_flashget` §4.5).
pub const DEFAULT_CONCURRENCY: u32 = 5;

/// Default mirror cooldown (seconds) — matches FlashGet "ban 30s".
pub const DEFAULT_MIRROR_COOLDOWN_SECS: u64 = 30;

/// Application-level configuration. Lives in memory; persisted to SQLite on
/// every mutation via [`AppConfig::save`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Directory under which all downloads are placed by default.
    pub download_dir: PathBuf,
    /// Maximum number of concurrent tasks the scheduler will admit.
    pub max_concurrent_tasks: u32,
    /// Default number of slices per task.
    pub default_concurrency: u32,
    /// Per-slice size for HTTP range downloads.
    pub default_slice_size: u64,
    /// Maximum number of retries per slice (Quark MAX_RETRY).
    pub max_retry_per_slice: u32,
    /// Per-task max bytes/sec (0 = unlimited).
    pub task_speed_limit_bps: u64,
    /// Global max bytes/sec across all tasks (0 = unlimited).
    pub global_speed_limit_bps: u64,
    /// Enable mirror discovery (FlashGet P2SP-like). Off by default —
    /// mirrors must be explicitly opted-in for privacy.
    pub enable_mirror_discovery: bool,
    /// Mirror cooldown in seconds after a failed probe.
    pub mirror_cooldown_secs: u64,
    /// User-Agent string sent on every HTTP request.
    pub user_agent: String,
    /// Whether BT-related code paths are wired (false in this prototype; the
    /// trait exists but the implementation is a placeholder).
    pub bt_enabled: bool,
    /// AutoThrottle target RTT in milliseconds (Tixati §6.2).
    pub autothrottle_target_rtt_ms: u32,
    /// AutoThrottle minimum rate floor in bytes/sec.
    pub autothrottle_min_bps: u64,
    /// AutoThrottle maximum rate ceiling in bytes/sec.
    pub autothrottle_max_bps: u64,
    /// Default proxy URL (None = direct).
    pub proxy: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            download_dir: dirs_or_tmp(),
            max_concurrent_tasks: 3,
            default_concurrency: DEFAULT_CONCURRENCY,
            default_slice_size: DEFAULT_SLICE_SIZE,
            max_retry_per_slice: crate::error::MAX_RETRY,
            task_speed_limit_bps: 0,
            global_speed_limit_bps: 0,
            enable_mirror_discovery: false,
            mirror_cooldown_secs: DEFAULT_MIRROR_COOLDOWN_SECS,
            user_agent: concat!("multi-downloader/", env!("CARGO_PKG_VERSION"))
                .to_string(),
            bt_enabled: false,
            autothrottle_target_rtt_ms: 100,
            autothrottle_min_bps: 64 * 1024,
            autothrottle_max_bps: 100 * 1024 * 1024,
            proxy: None,
        }
    }
}

fn dirs_or_tmp() -> PathBuf {
    std::env::var("MDC_DOWNLOAD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("mdc_downloads"))
}

/// A wrapper holding both the in-memory config and a live SQLite handle.
///
/// Reads are cheap (RwLock read); writes serialize on a write-lock and commit
/// to SQLite via a single `UPDATE` of a single-row JSON blob. This trades a
/// little write throughput for simplicity — config changes are rare.
#[derive(Clone)]
pub struct ConfigStore {
    inner: Arc<RwLock<AppConfig>>,
    db_path: PathBuf,
}

impl ConfigStore {
    /// Open (or create) the config store backed by `<db_dir>/mdc.db`.
    ///
    /// On first run an empty schema is created and the row is populated with
    /// `AppConfig::default()`.
    pub fn open(db_dir: impl AsRef<Path>) -> Result<Self> {
        std::fs::create_dir_all(db_dir.as_ref())?;
        let db_path = db_dir.as_ref().join("mdc.db");
        let conn = Connection::open(&db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS config (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
            [],
        )?;
        let cfg: AppConfig = conn
            .query_row(
                "SELECT value FROM config WHERE key = 'app'",
                [],
                |row| {
                    let s: String = row.get(0)?;
                    Ok(serde_json::from_str(&s).unwrap_or_default())
                },
            )
            .optional()?
            .unwrap_or_default();
        if conn
            .query_row("SELECT 1 FROM config WHERE key = 'app'", [], |_| Ok(()))
            .optional()?
            .is_none()
        {
            let s = serde_json::to_string(&cfg).map_err(|e| {
                DownloadError::new(0, ErrorCategory::Protocol, e.to_string())
            })?;
            conn.execute(
                "INSERT INTO config (key, value) VALUES ('app', ?)",
                params![s],
            )?;
        }
        Ok(Self {
            inner: Arc::new(RwLock::new(cfg)),
            db_path,
        })
    }

    /// Read a snapshot of the current config.
    #[must_use]
    pub fn snapshot(&self) -> AppConfig {
        self.inner.read().clone()
    }

    /// Replace the config and persist to SQLite atomically.
    pub fn save(&self, new_cfg: AppConfig) -> Result<()> {
        let conn = Connection::open(&self.db_path)?;
        let s = serde_json::to_string(&new_cfg).map_err(|e| {
            DownloadError::new(0, ErrorCategory::Protocol, e.to_string())
        })?;
        conn.execute(
            "INSERT OR REPLACE INTO config (key, value) VALUES ('app', ?)",
            params![s],
        )?;
        *self.inner.write() = new_cfg;
        Ok(())
    }

    /// Path of the backing SQLite database (exposed for tests / debugging).
    #[must_use]
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn default_config_is_sane() {
        let c = AppConfig::default();
        assert!(!c.enable_mirror_discovery, "mirror discovery must be off by default");
        assert_eq!(c.default_concurrency, DEFAULT_CONCURRENCY);
        assert_eq!(c.default_slice_size, DEFAULT_SLICE_SIZE);
        assert!(c.user_agent.starts_with("multi-downloader/"));
    }

    #[test]
    fn open_creates_schema_and_round_trips() {
        let dir = tempdir().unwrap();
        let store = ConfigStore::open(dir.path()).unwrap();
        let mut cfg = store.snapshot();
        cfg.default_concurrency = 7;
        cfg.enable_mirror_discovery = true;
        store.save(cfg.clone()).unwrap();
        drop(store);
        let store2 = ConfigStore::open(dir.path()).unwrap();
        let reloaded = store2.snapshot();
        assert_eq!(reloaded.default_concurrency, 7);
        assert!(reloaded.enable_mirror_discovery);
    }
}
