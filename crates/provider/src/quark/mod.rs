//! 夸克网盘 Provider（Task 5-d/T2，能力吸收：multi_downloader 分析包
//! `05_quark` + Rust 原型 07_rust_proto 的 Quark 模块）。
//!
//! 模块结构：
//! - [`types`]：登录态（Cookie）/ 错误分类（NotLogin/ShareExpired/
//!   QuotaExhausted）/ 端点常量（UA/Referer 对齐 quark_architecture.md §3.3）
//! - [`client`]：drive 网关端点封装（stoken → detail → save → task → download）
//! - [`share`]：分享链接解析 + `QuarkProvider`（RemoteProvider 实现）+ mock 测试
//!
//! 对齐 provider 现有模式：
//! - trait：`crate::RemoteProvider`（submit/status/resolve/refresh_links）
//! - 冷却：失败自动 backoff（Auth 5min / Quota 1h / 其他 1min），同 xunlei
//! - 登录态：文件持久化 + 原子写，同 xunlei::auth（独立实现）
//!
//! **端点形状待真机验证**：分析文档仅覆盖 installer stub，分享 API 按通用
//! 网盘 REST 形状实现；测试用 axum mock 与本实现形状一致。

pub mod client;
pub mod share;
pub mod types;

pub use client::{DownloadLink, SaveTaskState, QuarkClient};
pub use share::{parse_share_link, QuarkProvider, QuarkShareLink};
pub use types::{load_auth, save_auth, QuarkAuth, QuarkError, REFERER, USER_AGENT, BASE};
