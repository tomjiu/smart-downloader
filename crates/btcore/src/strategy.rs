//! BitComet 策略建议器门面（Task 5-d/T4）。
//!
//! 实现本体在 [`smart_dl_core::strategy`]（Linux 可 check 的纯模块；
//! btcore 在 Linux 因 bindgen(libclang) + Windows 专用 `lt_kernel.lib`
//! 无法编译，按 Task 5-d 的可编译性决策回退到 core，此处仅转发，
//! 供 Windows 构建下 `smart_dl_btcore::strategy::*` 使用）。
//!
//! 接入点（libtorrent settings_pack / btcore 现有钩子）说明见
//! `smart_dl_core::strategy` 模块尾部注释。

pub use smart_dl_core::strategy::{AntiLeechAdvice, CacheProfile, DiskCacheAdvice, LeechProfile};
