//! Core domain types: `DownloadTask`, `Slice`, listener trait, scheduler, and
//! the 7-stage state machine (Quark-style).

pub mod listener;
pub mod scheduler;
pub mod state_machine;
pub mod task;

pub use listener::{DownloadEventListener, NoopListener};
pub use scheduler::TaskScheduler;
pub use state_machine::{StateMachine, Stage, StageTransition};
pub use task::{DownloadTask, Slice, SliceStatus, TaskKind, TaskStatus};
