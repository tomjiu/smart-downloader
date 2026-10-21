//! 落盘与输出层（原 `core::session::output`，TD-3 I/O 下沉）。
//!
//! 从 core 拆出的纯 fs I/O 模块，使 core 回归纯逻辑职责（架构审查 §6 建议 6）：
//! - `sanitize_rel`：相对路径净化（拒绝穿越/绝对路径/Windows 保留名/symlink 逃逸面）
//! - `OutputManager`：`.part` → rename 落位 + 跨卷 copy fallback
//! - `evaluate_disk` / `required_disk`：磁盘预检（D36 分段公式）
//!
//! 历史注记：同目录原 `manager.rs`（SessionManager，state.json/resume.bencode）
//! 为生产死代码——daemon 使用自己的 `state/persistence.rs`（tasks.json 原子写 +
//! autosave），与该实现平行且从未接线，随本次下沉一并删除（git 历史可恢复）。

pub mod output;

pub use output::{
    evaluate_disk, required_disk, sanitize_rel, DiskCheck, OutputError, OutputManager,
};
