//! `DownloadEventListener` trait — Quark's `DownloadEventListener` design
//! (see `analysis/05_quark/quark_architecture.md` §11.1).
//!
//! Quark ships a single interface with start/progress/complete/failed hooks
//! for both slices and tasks. We copy that shape verbatim, with two changes:
//!
//! 1. Hooks are `async` (Quark's are sync C++ virtuals).
//! 2. Hooks take shared references — listeners must not mutate task state
//!    directly; they should instead push side effects to their own channels.

use async_trait::async_trait;

use super::task::{DownloadTask, Slice};
use crate::error::DownloadError;

/// Listener invoked by the engine on task / slice lifecycle events.
///
/// All hooks have a default no-op implementation, so implementors may
/// override only what they care about.
#[async_trait]
pub trait DownloadEventListener: Send + Sync {
    /// A slice is about to start downloading (or retry after failure).
    async fn on_slice_start(&self, _task: &DownloadTask, _slice: &Slice) {}

    /// Progress tick for an active slice (called at most every ~250ms per
    /// slice to avoid log spam).
    async fn on_slice_progress(
        &self,
        _task: &DownloadTask,
        _slice: &Slice,
        _bytes_done: u64,
    ) {
    }

    /// Slice finished successfully (hash already verified).
    async fn on_slice_complete(&self, _task: &DownloadTask, _slice: &Slice) {}

    /// Slice failed (may still be retried — check `slice.status`).
    async fn on_slice_failed(
        &self,
        _task: &DownloadTask,
        _slice: &Slice,
        _err: &DownloadError,
    ) {
    }

    /// Task as a whole completed successfully.
    async fn on_task_complete(&self, _task: &DownloadTask) {}

    /// Task failed permanently (exhausted retries on at least one slice).
    async fn on_task_failed(&self, _task: &DownloadTask, _err: &DownloadError) {}

    /// Stage transition within the 7-stage state machine (Quark-style).
    async fn on_stage_transition(&self, _task: &DownloadTask, _stage: &str) {}
}

/// A no-op listener that swallows every event. Useful for tests and for
/// library users that don't need callbacks.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopListener;

#[async_trait]
impl DownloadEventListener for NoopListener {}

/// A counting listener that records every event into an internal counter map.
/// Useful for tests asserting that hooks fire.
#[derive(Default)]
pub struct CountingListener {
    /// Atomic counters keyed by event name.
    counts: std::sync::Mutex<std::collections::HashMap<&'static str, u64>>,
}

impl CountingListener {
    /// Construct a fresh counting listener.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn bump(&self, name: &'static str) {
        *self.counts.lock().unwrap().entry(name).or_insert(0) += 1;
    }

    /// Read a counter (0 if unknown).
    #[must_use]
    pub fn get(&self, name: &str) -> u64 {
        self.counts.lock().unwrap().get(name).copied().unwrap_or(0)
    }
}

#[async_trait]
impl DownloadEventListener for CountingListener {
    async fn on_slice_start(&self, _: &DownloadTask, _: &Slice) {
        self.bump("slice_start");
    }
    async fn on_slice_progress(&self, _: &DownloadTask, _: &Slice, _: u64) {
        self.bump("slice_progress");
    }
    async fn on_slice_complete(&self, _: &DownloadTask, _: &Slice) {
        self.bump("slice_complete");
    }
    async fn on_slice_failed(&self, _: &DownloadTask, _: &Slice, _: &DownloadError) {
        self.bump("slice_failed");
    }
    async fn on_task_complete(&self, _: &DownloadTask) {
        self.bump("task_complete");
    }
    async fn on_task_failed(&self, _: &DownloadTask, _: &DownloadError) {
        self.bump("task_failed");
    }
    async fn on_stage_transition(&self, _: &DownloadTask, stage: &str) {
        self.bump(stage.leak_static());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::task::TaskKind;
    use url::Url;

    #[tokio::test]
    async fn noop_listener_compiles_and_runs() {
        let l = NoopListener;
        let t = DownloadTask::new(TaskKind::Http, Url::parse("https://x/y").unwrap());
        let s = crate::core::task::Slice::new(0, 0, 10);
        l.on_slice_start(&t, &s).await;
        l.on_slice_complete(&t, &s).await;
        l.on_task_complete(&t).await;
    }

    #[tokio::test]
    async fn counting_listener_tracks_counts() {
        let l = CountingListener::new();
        let t = DownloadTask::new(TaskKind::Http, Url::parse("https://x/y").unwrap());
        let s = crate::core::task::Slice::new(0, 0, 10);
        l.on_slice_start(&t, &s).await;
        l.on_slice_start(&t, &s).await;
        l.on_slice_complete(&t, &s).await;
        assert_eq!(l.get("slice_start"), 2);
        assert_eq!(l.get("slice_complete"), 1);
        assert_eq!(l.get("slice_progress"), 0);
    }
}
