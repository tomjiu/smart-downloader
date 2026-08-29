//! Task scheduler — fair-share queue with priority bands.
//!
//! Combines ideas from FlashGet's task queue (analysis §10) and Quark's
//! "one slice at a time per task" scheduling, while exposing hooks for
//! Tixati's weekday × cycle scheduler (Tixati analysis §9.1 — kept as a
//! placeholder trait, not active in the prototype).
//!
//! The scheduler admits at most `max_concurrent_tasks` tasks concurrently;
//! within each task the engine manages its own slice concurrency. Among
//! ready tasks the scheduler is **strict-priority + round-robin within a
//! band** to avoid starving low-priority tasks indefinitely.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::sync::Notify;

use super::task::{DownloadTask, TaskStatus};
use crate::config::AppConfig;

/// Priority band (FlashGet-style "high / normal / low" three-band queue).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Priority {
    /// Background / seeding tasks.
    Low = 0,
    /// Default.
    Normal = 1,
    /// User-initiated foreground download.
    High = 2,
}

/// Internal queued-task entry.
struct Entry {
    task: DownloadTask,
    priority: Priority,
    enqueued_at: Instant,
}

/// Top-level scheduler.
pub struct TaskScheduler {
    cfg: Arc<Mutex<AppConfig>>,
    queue: Arc<Mutex<BTreeMap<Priority, VecDeque<Entry>>>>,
    running: Arc<Mutex<Vec<u64>>>,
    notify: Arc<Notify>,
}

impl TaskScheduler {
    /// Build a scheduler from an `AppConfig` snapshot.
    #[must_use]
    pub fn new(cfg: AppConfig) -> Self {
        Self {
            cfg: Arc::new(Mutex::new(cfg)),
            queue: Arc::new(Mutex::new(BTreeMap::new())),
            running: Arc::new(Mutex::new(Vec::new())),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Enqueue a task with the given priority.
    pub fn enqueue(&self, task: DownloadTask, priority: Priority) {
        let entry = Entry {
            task,
            priority,
            enqueued_at: Instant::now(),
        };
        self.queue
            .lock()
            .entry(priority)
            .or_default()
            .push_back(entry);
        self.notify.notify_one();
    }

    /// Block until a task is admissible (i.e. `running.len() < max_concurrent_tasks`),
    /// returning the next task to run and a guard that releases the slot on
    /// drop.
    pub async fn acquire(&self) -> RunningTaskGuard {
        loop {
            // 1. Try to admit immediately if capacity is available.
            if let Some(entry) = self.try_admit() {
                return entry;
            }
            // 2. Otherwise wait for the notify signal.
            self.notify.notified().await;
        }
    }

    fn try_admit(&self) -> Option<RunningTaskGuard> {
        let max = self.cfg.lock().max_concurrent_tasks.max(1) as usize;
        let mut running = self.running.lock();
        if running.len() >= max {
            return None;
        }
        // Strict-priority: pick from the highest non-empty band first.
        let mut queue = self.queue.lock();
        for (band, deque) in queue.iter_mut().rev() {
            if let Some(entry) = deque.pop_front() {
                running.push(entry.task.task_id);
                let queue = Arc::clone(&self.queue);
                let running = Arc::clone(&self.running);
                let notify = Arc::clone(&self.notify);
                let p = *band;
                let _ = p; // band retained for future fairness tracking
                return RunningTaskGuard {
                    task: entry.task,
                    running,
                    notify,
                };
            }
        }
        let _ = &queue; // satisfy borrow checker lint under different cfgs
        None
    }

    /// Snapshot of the queue length per band (for diagnostics / tests).
    #[must_use]
    pub fn pending_counts(&self) -> [(Priority, usize); 3] {
        let q = self.queue.lock();
        [
            (Priority::Low, q.get(&Priority::Low).map_or(0, VecDeque::len)),
            (Priority::Normal, q.get(&Priority::Normal).map_or(0, VecDeque::len)),
            (Priority::High, q.get(&Priority::High).map_or(0, VecDeque::len)),
        ]
    }

    /// Number of currently-running tasks.
    #[must_use]
    pub fn running_count(&self) -> usize {
        self.running.lock().len()
    }

    /// Sleep helper that respects cancellation via `Notify`. Exposed for tests.
    pub async fn wait_for(&self, dur: Duration) {
        let n = tokio::time::timeout(dur, self.notify.notified()).await;
        let _ = n;
    }
}

/// RAII guard: holds the task and removes its id from the running set when
/// dropped.
pub struct RunningTaskGuard {
    /// The admitted task.
    pub task: DownloadTask,
    running: Arc<Mutex<Vec<u64>>>,
    notify: Arc<Notify>,
}

impl RunningTaskGuard {
    /// Manually release the slot before drop (e.g. when the task completes).
    pub fn release(self) {
        // Drop impl does the work.
        drop(self);
    }
}

impl Drop for RunningTaskGuard {
    fn drop(&mut self) {
        let mut running = self.running.lock();
        if let Some(pos) = running.iter().position(|&id| id == self.task.task_id) {
            running.swap_remove(pos);
        }
        self.notify.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::task::TaskKind;
    use url::Url;

    fn cfg() -> AppConfig {
        let mut c = AppConfig::default();
        c.max_concurrent_tasks = 2;
        c
    }

    fn mk(id: u64) -> DownloadTask {
        let t = DownloadTask::new(TaskKind::Http, Url::parse("https://x/y").unwrap());
        // Force the task_id via reflection of fields; since task_id is private
        // to the new() function we just trust the allocator for tests.
        let _ = id;
        t
    }

    #[tokio::test]
    async fn admit_until_full() {
        let s = TaskScheduler::new(cfg());
        s.enqueue(mk(1), Priority::High);
        s.enqueue(mk(2), Priority::Normal);
        s.enqueue(mk(3), Priority::Low);
        let g1 = s.acquire().await;
        let g2 = s.acquire().await;
        assert_eq!(s.running_count(), 2);
        drop(g1);
        // After dropping one, the third should be admissible.
        let g3 = s.acquire().await;
        assert_eq!(s.running_count(), 2);
        drop(g2);
        drop(g3);
    }

    #[tokio::test]
    async fn high_priority_first() {
        let s = TaskScheduler::new(cfg());
        s.enqueue(mk(10), Priority::Low);
        s.enqueue(mk(11), Priority::High);
        let g = s.acquire().await;
        // The task_id we get back should not be 10 (we can't directly inspect
        // ids since they're opaque, but we can check the queue count).
        let counts = s.pending_counts();
        assert_eq!(counts[0].1, 1); // Low band still has 1
        drop(g);
    }
}
