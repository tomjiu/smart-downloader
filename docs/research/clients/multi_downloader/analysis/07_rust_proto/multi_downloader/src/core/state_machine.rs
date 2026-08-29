//! Quark-style 7-stage state machine.
//!
//! Quark's mini_install.dll exposes a state-machine that walks every download
//! through (see `analysis/05_quark/quark_architecture.md` §5.1):
//!
//! 1. `FetchVersion` — pull latest manifest / remote config
//! 2. `KillExistProcess` — terminate any process holding the destination
//! 3. `Download` — slice-based concurrent download
//! 4. `Install` — extract / move into place
//! 5. `Setup` — post-install hooks (registry entries / desktop shortcuts)
//!
//! Each stage has `start_*` and `end_*` hooks. Failures at the Download stage
//! trigger a `Retry` branch with exponential backoff.
//!
//! We borrow the shape verbatim but:
//! - Replace Windows-specific `KillExistProcess` semantics with a generic
//!   "release destination lock" stage.
//! - Replace the InnoSetup registry / shortcut hooks with a configurable
//!   trait (`SetupHook`) — by default a no-op.

use std::fmt;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::listener::DownloadEventListener;
use super::task::{DownloadTask, TaskStatus};
use crate::error::{DownloadError, ErrorCategory, Result};

/// All 7 observable stages (the Quark "mini_*" names, sans the Windows-isms).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Stage {
    /// Initial state.
    Init,
    /// Fetch manifest / version metadata.
    FetchVersion,
    /// Release any lingering lock on the destination file.
    KillExistProcess,
    /// Concurrent slice download.
    Download,
    /// Optional retry branch (off main flow).
    DownloadRetry,
    /// Extract / move into place + hash verification.
    Install,
    /// Post-install setup hooks (Unix perms / symlinks / launcher entries).
    Setup,
    /// Terminal success.
    Done,
    /// Terminal failure.
    Failed,
}

impl Stage {
    /// All valid transitions from `self` to `next` (graph encoded as matches).
    #[must_use]
    pub fn can_transition_to(self, next: Stage) -> bool {
        use Stage::*;
        matches!(
            (self, next),
            (Init, FetchVersion)
                | (FetchVersion, KillExistProcess)
                | (FetchVersion, Failed)
                | (KillExistProcess, Download)
                | (KillExistProcess, Failed)
                | (Download, Install)
                | (Download, DownloadRetry)
                | (Download, Failed)
                | (DownloadRetry, Download)
                | (DownloadRetry, Failed)
                | (Install, Setup)
                | (Install, Failed)
                | (Setup, Done)
                | (Setup, Failed)
        )
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Record of a single stage transition (used for audit log).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageTransition {
    pub from: Stage,
    pub to: Stage,
    pub task_id: u64,
    pub ts_unix: u64,
    pub note: Option<String>,
}

/// The state machine itself.
pub struct StateMachine {
    current: Mutex<Stage>,
    transitions: Mutex<Vec<StageTransition>>,
    task_id: u64,
    listener: Arc<dyn DownloadEventListener>,
    /// Reference to the task (for listener dispatch; not mutated here).
    task: DownloadTask,
}

impl StateMachine {
    /// Construct a new state machine for `task`, starting in `Stage::Init`.
    #[must_use]
    pub fn new(task: DownloadTask, listener: Arc<dyn DownloadEventListener>) -> Self {
        let task_id = task.task_id;
        Self {
            current: Mutex::new(Stage::Init),
            transitions: Mutex::new(Vec::new()),
            task_id,
            listener,
            task,
        }
    }

    /// Current stage (cheap snapshot).
    #[must_use]
    pub fn current(&self) -> Stage {
        *self.current.lock()
    }

    /// Transition to `next`, recording the transition and firing the listener.
    ///
    /// Returns an error if the transition is not in the allowed graph.
    pub async fn transition(&self, next: Stage, note: Option<String>) -> Result<()> {
        let cur = *self.current.lock();
        if !cur.can_transition_to(next) {
            return Err(DownloadError::new(
                self.task_id,
                ErrorCategory::Protocol,
                format!("illegal stage transition {cur:?} -> {next:?}"),
            ));
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let t = StageTransition {
            from: cur,
            to: next,
            task_id: self.task_id,
            ts_unix: ts,
            note: note.clone(),
        };
        tracing::info!(
            task = self.task_id,
            from = ?cur,
            to = ?next,
            note = ?note,
            "stage transition"
        );
        self.transitions.lock().push(t);
        *self.current.lock() = next;
        // Mirror the high-level task status.
        let new_status = match next {
            Stage::Init => TaskStatus::Pending,
            Stage::FetchVersion | Stage::KillExistProcess => TaskStatus::Initializing,
            Stage::Download | Stage::DownloadRetry => TaskStatus::Downloading,
            Stage::Install => TaskStatus::Installing,
            Stage::Setup => TaskStatus::Setup,
            Stage::Done => TaskStatus::Completed,
            Stage::Failed => TaskStatus::Failed,
        };
        self.task.status = new_status;
        self.listener.on_stage_transition(&self.task, &next.to_string()).await;
        Ok(())
    }

    /// Snapshot all transitions recorded so far.
    #[must_use]
    pub fn history(&self) -> Vec<StageTransition> {
        self.transitions.lock().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::listener::NoopListener;
    use crate::core::task::TaskKind;
    use url::Url;

    fn mk() -> StateMachine {
        let t = DownloadTask::new(TaskKind::Http, Url::parse("https://x/y").unwrap());
        StateMachine::new(t, Arc::new(NoopListener))
    }

    #[tokio::test]
    async fn happy_path_transitions() {
        let sm = mk();
        sm.transition(Stage::FetchVersion, None).await.unwrap();
        sm.transition(Stage::KillExistProcess, None).await.unwrap();
        sm.transition(Stage::Download, None).await.unwrap();
        sm.transition(Stage::Install, None).await.unwrap();
        sm.transition(Stage::Setup, None).await.unwrap();
        sm.transition(Stage::Done, None).await.unwrap();
        assert_eq!(sm.current(), Stage::Done);
        assert_eq!(sm.history().len(), 6);
    }

    #[tokio::test]
    async fn illegal_transition_rejected() {
        let sm = mk();
        let err = sm.transition(Stage::Download, None).await.unwrap_err();
        assert_eq!(err.category, ErrorCategory::Protocol);
    }

    #[tokio::test]
    async fn retry_branch_works() {
        let sm = mk();
        sm.transition(Stage::FetchVersion, None).await.unwrap();
        sm.transition(Stage::KillExistProcess, None).await.unwrap();
        sm.transition(Stage::Download, None).await.unwrap();
        sm.transition(Stage::DownloadRetry, Some("5xx".into())).await.unwrap();
        sm.transition(Stage::Download, None).await.unwrap();
        sm.transition(Stage::Install, None).await.unwrap();
        sm.transition(Stage::Setup, None).await.unwrap();
        sm.transition(Stage::Done, None).await.unwrap();
        assert_eq!(sm.history().len(), 8);
    }
}
