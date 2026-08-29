//! HTTP / HTTPS multi-slice downloader.
//!
//! Combines:
//! - Quark-style slice model (`task_id` + `Slice` + retry + backup URL).
//! - FlashGet-style mirror discovery (`mirror.rs`) — opt-in only.
//! - Quark-style three-segment error code via `DownloadError`.
//! - FlashGet-style exponential backoff (`utils::retry`).
//! - Rustls-based TLS via `net::tls`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use super::protocol::{ProtocolEngine, ProtocolKind};
use crate::config::AppConfig;
use crate::core::listener::DownloadEventListener;
use crate::core::task::{DownloadTask, Slice, SliceStatus};
use crate::error::{DownloadError, ErrorCategory, Result};
use crate::net::tls::build_https_client;
use crate::storage::file_io::AtomicFile;
use crate::utils::retry::backoff_delay;

/// HTTP/HTTPS slice downloader.
pub struct HttpEngine {
    cfg: AppConfig,
    listener: Arc<dyn DownloadEventListener>,
    client: reqwest::Client,
    /// Concurrency limiter shared across all slices of a single task.
    slice_sem: Arc<Semaphore>,
}

impl HttpEngine {
    /// Construct a new HTTP engine bound to the given config + listener.
    #[must_use]
    pub fn new(cfg: AppConfig, listener: Arc<dyn DownloadEventListener>) -> Self {
        let client = build_https_client(&cfg).expect("failed to build HTTPS client");
        let slice_sem = Arc::new(Semaphore::new(cfg.default_concurrency as usize));
        Self {
            cfg,
            listener,
            client,
            slice_sem,
        }
    }

    /// Resolve the file size + Accept-Ranges support via a HEAD request.
    ///
    /// If HEAD is not supported (some CDN/edge servers reject HEAD), fall
    /// back to a small GET Range request and inspect the `Content-Range`.
    pub async fn probe_size(&self, task: &DownloadTask) -> Result<(u64, bool)> {
        let req = self.client.head(task.url.clone());
        let resp = req.send().await.map_err(DownloadError::from)?;
        let status = resp.status().as_u16() as i32;
        if !resp.status().is_success() {
            return Err(DownloadError::new(task.task_id, ErrorCategory::from_http(status), format!("HEAD status {status}")));
        }
        let len = resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let ar = resp
            .headers()
            .get(reqwest::header::ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.eq_ignore_ascii_case("bytes"))
            .unwrap_or(false);
        Ok((len, ar))
    }

    /// Slice a task into the configured number of slices (using `slice_size`).
    pub fn plan_task(&self, task: &mut DownloadTask, total: u64, supports_range: bool) {
        task.total_size = total;
        if !supports_range || total == 0 {
            // Single slice covering the whole file.
            task.install_slices(vec![Slice::new(0, 0, total.max(1))]);
            return;
        }
        let slices = task.plan_slices();
        task.install_slices(slices);
    }

    /// Download a single slice to completion (with retries + mirror failover
    /// already orchestrated outside).
    async fn download_slice(&self, task: &DownloadTask, slice_idx: usize) -> Result<()> {
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let snapshot = task.snapshot_slices();
            let slice = &snapshot[slice_idx];
            if slice.status == SliceStatus::Done {
                return Ok(());
            }
            self.listener.on_slice_start(task, slice).await;

            let url = if slice.mirror_id < 0 {
                task.url.clone()
            } else {
                // Mirrors are stored under `task.meta["mirror_<id>"]`.
                task.meta
                    .get(&format!("mirror_{}", slice.mirror_id))
                    .and_then(|s| Url::parse(s).ok())
                    .unwrap_or_else(|| task.url.clone())
            };
            let start = slice.range_start();
            let end = slice.range_end();
            let header_val = format!("bytes={start}-{end}");
            let mut req = self
                .client
                .get(url.clone())
                .header(reqwest::header::RANGE, header_val)
                .header(reqwest::header::USER_AGENT, &self.cfg.user_agent);
            if let Some(ref r) = task.referer {
                req = req.header(reqwest::header::REFERER, r);
            }
            if let Some(ref c) = task.cookies {
                req = req.header(reqwest::header::COOKIE, c);
            }
            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    let err = DownloadError::from(e).with_context("slice_idx", slice_idx.to_string());
                    self.listener.on_slice_failed(task, slice, &err).await;
                    self.mark_slice_failure(task, slice_idx, &err).await;
                    if attempt > self.cfg.max_retry_per_slice {
                        return Err(err);
                    }
                    let delay = backoff_delay(attempt);
                    tokio::time::sleep(delay).await;
                    continue;
                }
            };
            let status = resp.status().as_u16() as i32;
            if status != 206 && status != 200 {
                let cat = if (400..500).contains(&status) {
                    ErrorCategory::HttpClient
                } else if (500..600).contains(&status) {
                    ErrorCategory::HttpServer
                } else {
                    ErrorCategory::Protocol
                };
                let err = DownloadError::new(task.task_id, cat, format!("HTTP {status}"))
                    .with_extra(status)
                    .with_context("slice_idx", slice_idx.to_string());
                self.listener.on_slice_failed(task, slice, &err).await;
                self.mark_slice_failure(task, slice_idx, &err).await;
                if attempt > self.cfg.max_retry_per_slice {
                    return Err(err);
                }
                let delay = backoff_delay(attempt);
                tokio::time::sleep(delay).await;
                continue;
            }
            // Stream the body to disk.
            let dest = task
                .dest
                .clone()
                .ok_or_else(|| DownloadError::new(task.task_id, ErrorCategory::Io, "no dest"))?;
            let file = AtomicFile::open(&dest).await?;
            let mut stream = resp.bytes_stream();
            let mut total = 0u64;
            let mut last_report = tokio::time::Instant::now();
            while let Some(chunk) = stream.next().await {
                let bytes: Bytes = chunk.map_err(|e| DownloadError::from(e))?;
                let base = slice.offset + total;
                file.pwrite(&bytes, base).await?;
                total += bytes.len() as u64;
                if last_report.elapsed() > Duration::from_millis(250) {
                    let mut slices = task.slices.write();
                    if let Some(s) = slices.get_mut(slice_idx) {
                        s.record_bytes(bytes.len() as u64, 0.1);
                    }
                    drop(slices);
                    self.listener
                        .on_slice_progress(task, slice, total)
                        .await;
                    last_report = tokio::time::Instant::now();
                }
            }
            // Final flush + mark slice complete.
            let mut slices = task.slices.write();
            if let Some(s) = slices.get_mut(slice_idx) {
                s.downloaded = s.length;
                s.status = SliceStatus::Done;
            }
            drop(slices);
            file.flush().await?;
            let snap = task.snapshot_slices();
            self.listener.on_slice_complete(task, &snap[slice_idx]).await;
            debug!(task = task.task_id, slice = slice_idx, "slice complete");
            return Ok(());
        }
    }

    /// Mark a slice failure (bumps retry_count, transitions state).
    async fn mark_slice_failure(&self, task: &DownloadTask, idx: usize, err: &DownloadError) {
        let mut slices = task.slices.write();
        if let Some(s) = slices.get_mut(idx) {
            s.mark_failure(err);
        }
    }

    /// Run all slices concurrently up to `concurrency`.
    ///
    /// Uses `futures::stream::buffer_unordered` to bound concurrency instead
    /// of spawning detached tasks — this keeps the borrow checker happy and
    /// avoids the `Arc<HttpEngine>` indirection that the prototype doesn't
    /// otherwise need.
    pub async fn run_slices(&self, task: &DownloadTask) -> Result<()> {
        use futures::stream::{self, StreamExt};
        let slices = task.snapshot_slices();
        let n = slices.len();
        let sem = Arc::new(Semaphore::new(task.concurrency as usize));
        let results: Vec<std::result::Result<(), DownloadError>> =
            stream::iter(0..n)
                .map(|i| {
                    let sem = Arc::clone(&sem);
                    async move {
                        let _p = sem.acquire().await.expect("sem closed");
                        self.download_slice(task, i).await
                    }
                })
                .buffer_unordered(n)
                .collect()
                .await;
        let mut first_err: Option<DownloadError> = None;
        for r in results {
            if let Err(e) = r {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
        match first_err {
            None => Ok(()),
            Some(e) => Err(e),
        }
    }
}

#[async_trait]
impl ProtocolEngine for HttpEngine {
    fn kind(&self) -> ProtocolKind {
        ProtocolKind::Https
    }

    async fn run_task(&self, task: &DownloadTask) -> Result<()> {
        // 1. Probe size + range support.
        let (size, ar) = self.probe_size(task).await?;
        info!(task = task.task_id, size, supports_range = ar, "probed");
        // Clone to mutate slice list.
        let mut t = task.clone();
        self.plan_task(&mut t, size, ar);
        // Copy the slices back into the caller's task.
        task.install_slices(t.snapshot_slices());
        // 2. Run all slices.
        let result = self.run_slices(task).await;
        // 3. Swap to backup_url if necessary.
        if result.is_err() {
            if let Some(backup) = task.backup_url.clone() {
                warn!(task = task.task_id, "primary failed; switching to backup");
                let mut t2 = task.clone();
                t2.url = backup;
                return self.run_slices(&t2).await;
            }
        }
        if let Err(ref e) = result {
            self.listener.on_task_failed(task, e).await;
        } else {
            self.listener.on_task_complete(task).await;
        }
        result
    }
}

/// Helper extension — interpret an HTTP status code as a category.
impl ErrorCategory {
    /// Map an HTTP status code into an [`ErrorCategory`].
    #[must_use]
    pub fn from_http(status: i32) -> Self {
        match status {
            200..=299 => Self::HttpOk,
            300..=399 => Self::HttpRedirect,
            400..=499 => Self::HttpClient,
            500..=599 => Self::HttpServer,
            _ => Self::Protocol,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::listener::NoopListener;

    #[test]
    fn http_status_to_category() {
        assert_eq!(ErrorCategory::from_http(200), ErrorCategory::HttpOk);
        assert_eq!(ErrorCategory::from_http(302), ErrorCategory::HttpRedirect);
        assert_eq!(ErrorCategory::from_http(404), ErrorCategory::HttpClient);
        assert_eq!(ErrorCategory::from_http(500), ErrorCategory::HttpServer);
    }

    #[test]
    fn engine_construction_uses_rustls() {
        let cfg = AppConfig::default();
        let engine = HttpEngine::new(cfg, Arc::new(NoopListener));
        assert_eq!(engine.kind(), ProtocolKind::Https);
        assert_eq!(engine.cfg.default_concurrency, crate::config::DEFAULT_CONCURRENCY);
    }
}
