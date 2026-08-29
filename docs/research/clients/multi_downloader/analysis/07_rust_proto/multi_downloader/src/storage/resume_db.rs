//! SQLite-WAL-backed resume database.
//!
//! Replaces FlashGet's `.jc!`-style header embedding (analysis §9 of FlashGet
//! doc, called out as a design flaw) and Quark's local JSON config (analysis
//! §11.1, "Configuration persistence" row).
//!
//! Schema:
//!
//! - `tasks` (task_id, kind, url, dest, status, total_size, created_at)
//! - `slices` (task_id, idx, offset, length, downloaded, status, retry_count,
//!   error_code, extra_error_code)
//! - `config` (key, value) — shared with `crate::config`.
//!
//! All writes use SQLite WAL mode for crash-safe concurrent reads while
//! downloads are in progress.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::core::task::{Slice, SliceStatus, TaskKind, TaskStatus};
use crate::error::{DownloadError, ErrorCategory, Result};

/// Persistent snapshot of a task that can be saved / restored across restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedTask {
    pub task_id: u64,
    pub kind: TaskKind,
    pub url: String,
    pub dest: Option<PathBuf>,
    pub status: TaskStatus,
    pub total_size: u64,
    pub slice_size: u64,
    pub concurrency: u32,
    pub created_at_unix: u64,
    pub slices: Vec<Slice>,
}

/// Wrapper holding a SQLite connection (one per process is plenty).
pub struct ResumeDb {
    conn: Connection,
}

impl ResumeDb {
    /// Open (or create) the database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                task_id       INTEGER PRIMARY KEY,
                kind          TEXT NOT NULL,
                url           TEXT NOT NULL,
                dest          TEXT,
                status        TEXT NOT NULL,
                total_size    INTEGER NOT NULL,
                slice_size    INTEGER NOT NULL,
                concurrency   INTEGER NOT NULL,
                created_at    INTEGER NOT NULL,
                payload       TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS slices (
                task_id        INTEGER NOT NULL,
                idx            INTEGER NOT NULL,
                offset         INTEGER NOT NULL,
                length         INTEGER NOT NULL,
                downloaded     INTEGER NOT NULL,
                status         INTEGER NOT NULL,
                retry_count    INTEGER NOT NULL,
                error_code     INTEGER NOT NULL,
                extra_error_code INTEGER NOT NULL,
                PRIMARY KEY (task_id, idx),
                FOREIGN KEY (task_id) REFERENCES tasks(task_id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS slices_task_idx ON slices(task_id);
            CREATE INDEX IF NOT EXISTS tasks_status ON tasks(status);",
        )?;
        Ok(Self { conn })
    }

    /// Persist a task snapshot (replaces any existing record with same id).
    pub fn save_task(&self, task: &PersistedTask) -> Result<()> {
        let payload = serde_json::to_string(task).map_err(|e| {
            DownloadError::new(task.task_id, ErrorCategory::Protocol, e.to_string())
        })?;
        let kind = format!("{:?}", task.kind);
        let status = format!("{:?}", task.status);
        self.conn.execute(
            "INSERT OR REPLACE INTO tasks
                (task_id, kind, url, dest, status, total_size, slice_size, concurrency,
                 created_at, payload)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                task.task_id as i64,
                kind,
                task.url,
                task.dest.as_ref().map(|p| p.to_string_lossy().to_string()),
                status,
                task.total_size as i64,
                task.slice_size as i64,
                task.concurrency,
                task.created_at_unix as i64,
                payload,
            ],
        )?;
        // Replace slices.
        self.conn.execute(
            "DELETE FROM slices WHERE task_id = ?",
            params![task.task_id as i64],
        )?;
        let mut stmt = self.conn.prepare(
            "INSERT INTO slices
                (task_id, idx, offset, length, downloaded, status, retry_count,
                 error_code, extra_error_code)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )?;
        for s in &task.slices {
            stmt.execute(params![
                task.task_id as i64,
                s.index as i64,
                s.offset as i64,
                s.length as i64,
                s.downloaded as i64,
                s.status as i64,
                s.retry_count as i64,
                s.error_code,
                s.extra_error_code,
            ])?;
        }
        Ok(())
    }

    /// Load a single task by id.
    pub fn load_task(&self, task_id: u64) -> Result<Option<PersistedTask>> {
        let payload: Option<String> = self
            .conn
            .query_row(
                "SELECT payload FROM tasks WHERE task_id = ?",
                params![task_id as i64],
                |row| row.get(0),
            )
            .optional()?;
        match payload {
            Some(s) => {
                let t = serde_json::from_str(&s).map_err(|e| {
                    DownloadError::new(task_id, ErrorCategory::Protocol, e.to_string())
                })?;
                Ok(Some(t))
            }
            None => Ok(None),
        }
    }

    /// List all tasks (without slices — call `load_task` to get slices).
    pub fn list_tasks(&self) -> Result<Vec<PersistedTask>> {
        let mut stmt = self.conn.prepare("SELECT payload FROM tasks")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            let s = r?;
            let t = serde_json::from_str(&s).map_err(|e| {
                DownloadError::new(0, ErrorCategory::Protocol, e.to_string())
            })?;
            out.push(t);
        }
        Ok(out)
    }

    /// Delete a task (cascades to its slices).
    pub fn delete_task(&self, task_id: u64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM tasks WHERE task_id = ?",
            params![task_id as i64],
        )?;
        Ok(())
    }

    /// Count of pending slices (any non-Done / non-Corrupt status).
    pub fn pending_slice_count(&self) -> Result<i64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM slices WHERE status != ? AND status != ?",
            params![SliceStatus::Done as i64, SliceStatus::Corrupt as i64],
            |row| row.get(0),
        )?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::task::{DownloadTask, Slice};
    use tempfile::tempdir;

    fn mk_task() -> PersistedTask {
        let t = DownloadTask::new(TaskKind::Http, url::Url::parse("https://x/y").unwrap());
        let slices = vec![
            Slice::new(0, 0, 1000),
            Slice::new(1, 1000, 1000),
            Slice::new(2, 2000, 500),
        ];
        PersistedTask {
            task_id: t.task_id,
            kind: TaskKind::Http,
            url: "https://x/y".into(),
            dest: Some(PathBuf::from("/tmp/y")),
            status: TaskStatus::Downloading,
            total_size: 2500,
            slice_size: 1000,
            concurrency: 5,
            created_at_unix: 1_700_000_000,
            slices,
        }
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let db = ResumeDb::open(dir.path().join("r.db")).unwrap();
        let t = mk_task();
        db.save_task(&t).unwrap();
        let loaded = db.load_task(t.task_id).unwrap().unwrap();
        assert_eq!(loaded.task_id, t.task_id);
        assert_eq!(loaded.url, t.url);
        assert_eq!(loaded.slices.len(), t.slices.len());
    }

    #[test]
    fn list_tasks_returns_all() {
        let dir = tempdir().unwrap();
        let db = ResumeDb::open(dir.path().join("r.db")).unwrap();
        db.save_task(&mk_task()).unwrap();
        let t2 = mk_task();
        let mut t2 = t2;
        t2.task_id = 999;
        db.save_task(&t2).unwrap();
        let all = db.list_tasks().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn delete_removes_task_and_slices() {
        let dir = tempdir().unwrap();
        let db = ResumeDb::open(dir.path().join("r.db")).unwrap();
        let t = mk_task();
        db.save_task(&t).unwrap();
        db.delete_task(t.task_id).unwrap();
        assert!(db.load_task(t.task_id).unwrap().is_none());
        assert_eq!(db.pending_slice_count().unwrap(), 0);
    }
}
