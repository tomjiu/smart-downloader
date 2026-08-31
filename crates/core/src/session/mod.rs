//! 会话与输出层（M3）：state.json 持久化 / resume 流程 / .part 管理 / 磁盘预检 / 单实例锁。
//! 对应设计文档 §12（会话目录、恢复、输出、磁盘预检 D36、单实例锁 D24）。

pub mod manager;
pub mod output;
pub mod single_instance;

pub use manager::{
    should_save, LoadOutcome, ResumeOutcome, SaveReason, SessionError, SessionManager,
};
pub use output::{
    evaluate_disk, required_disk, sanitize_rel, DiskCheck, OutputError, OutputManager,
};
pub use single_instance::{InstanceLock, LockStatus};
