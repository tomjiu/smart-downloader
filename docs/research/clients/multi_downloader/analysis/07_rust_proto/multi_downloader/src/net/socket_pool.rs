//! Keep-Alive connection pool (FlashGet-style).
//!
//! FlashGet's worker loop (analysis §4.4) reuses the same TCP socket across
//! multiple slice GETs on the same mirror — this saves a TLS handshake per
//! slice. We replicate that with a small per-host idle pool.
//!
//! This module is intentionally a thin abstraction over
//! `reqwest::Client`'s built-in pool (which already does keep-alive); it
//! exists primarily to expose metrics (idle count, reuse ratio) that the
//! listener trait can report.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use url::Url;

/// Metrics for one host's pool.
#[derive(Debug, Default, Clone, Copy)]
pub struct PoolMetrics {
    /// Connections currently checked out to workers.
    pub active: u32,
    /// Idle connections waiting for reuse.
    pub idle: u32,
    /// Total connections ever created.
    pub created_total: u64,
    /// Total reuses (avoided re-handshakes).
    pub reused_total: u64,
}

/// Per-host connection pool.
pub struct SocketPool {
    inner: Mutex<HashMap<String, PoolEntry>>,
    /// Maximum idle connections per host (FlashGet-style — 8).
    max_idle_per_host: u32,
    /// Idle timeout (matches `reqwest::Client::pool_idle_timeout`).
    idle_timeout: Duration,
}

#[derive(Debug, Default, Clone)]
struct PoolEntry {
    metrics: PoolMetrics,
    last_check_in: Option<Instant>,
}

impl SocketPool {
    /// Construct a new pool with sensible defaults.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            max_idle_per_host: 8,
            idle_timeout: Duration::from_secs(60),
        }
    }

    /// Configure max idle per host.
    #[must_use]
    pub fn with_max_idle(mut self, n: u32) -> Self {
        self.max_idle_per_host = n;
        self
    }

    /// Record a check-out (active connection acquired).
    pub fn check_out(&self, host: &Url) {
        let key = host.host_str().unwrap_or_default().to_string();
        let mut m = self.inner.lock();
        let e = m.entry(key).or_default();
        e.metrics.active = e.metrics.active.saturating_add(1);
        e.metrics.idle = e.metrics.idle.saturating_sub(1);
        if e.metrics.created_total == 0 || e.metrics.idle == 0 {
            e.metrics.created_total = e.metrics.created_total.saturating_add(1);
        } else {
            e.metrics.reused_total = e.metrics.reused_total.saturating_add(1);
        }
    }

    /// Record a check-in (connection returned to the pool).
    pub fn check_in(&self, host: &Url) {
        let key = host.host_str().unwrap_or_default().to_string();
        let mut m = self.inner.lock();
        let e = m.entry(key).or_default();
        e.metrics.active = e.metrics.active.saturating_sub(1);
        if e.metrics.idle < self.max_idle_per_host {
            e.metrics.idle = e.metrics.idle.saturating_add(1);
            e.last_check_in = Some(Instant::now());
        }
    }

    /// Drop expired idle entries (call periodically).
    pub fn evict_expired(&self) {
        let now = Instant::now();
        let mut m = self.inner.lock();
        for e in m.values_mut() {
            if let Some(t) = e.last_check_in {
                if now.duration_since(t) > self.idle_timeout {
                    e.metrics.idle = 0;
                }
            }
        }
    }

    /// Snapshot metrics for all hosts.
    #[must_use]
    pub fn metrics(&self) -> HashMap<String, PoolMetrics> {
        self.inner
            .lock()
            .iter()
            .map(|(k, v)| (k.clone(), v.metrics))
            .collect()
    }
}

impl Default for SocketPool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_out_creates_first_conn() {
        let p = SocketPool::new();
        let u = Url::parse("https://example.com/x").unwrap();
        p.check_out(&u);
        let m = p.metrics();
        let e = m.get("example.com").unwrap();
        assert_eq!(e.created_total, 1);
        assert_eq!(e.reused_total, 0);
        assert_eq!(e.active, 1);
    }

    #[test]
    fn check_in_then_out_counts_as_reuse() {
        let p = SocketPool::new();
        let u = Url::parse("https://example.com/x").unwrap();
        p.check_out(&u);
        p.check_in(&u);
        p.check_out(&u);
        let m = p.metrics();
        let e = m.get("example.com").unwrap();
        assert_eq!(e.created_total, 1);
        assert_eq!(e.reused_total, 1);
    }
}
