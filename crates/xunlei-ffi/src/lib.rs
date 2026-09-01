//! Xunlei DownloadSDK FFI 封装（Windows-only 运行时，全平台可编译）。
//!
//! 两层架构：
//! - 第一层：标准 BT/P2P（BT swarm、Tracker、DHT）
//! - 第二层：迅雷 P2SP 加速网络（PHub + SHub + FreeDCDN）—— 免登录，UserID=0 可用
//!
//! 模块拆分（ABI/错误号变了只改对应模块）：
//! - `bindings` — FFI 类型/函数签名（struct size、extern 声明）
//! - `loader`   — DLL 加载（DownloadSDKProxy.dll）
//! - `error`    — 错误码映射（XunleiError）
//! - `handle`   — 生命周期管理（XL_Init / XL_UnInit）
//! - `task`     — 任务创建/启停（Magnet/BT/P2SP）
//! - `peer`     — Peer 管理（AddPeer/DiscardPeer）
//! - `tracker`  — Tracker 管理（BatchAddBTTracker）
//! - `dcdn`     — FreeDCDN 加速（免登录）
//! - `identity` — 用户身份/加速凭证注入面（设置 token 模式、AppGuid、加速证书；非登录）
//! - `query`    — 状态查询（QueryTaskInfo / QueryTaskFlow）
//!
//! # 跨平台编译策略（2026-08-30 Task 5-a）
//!
//! 本 crate **在所有平台都可编译**（类型/函数签名/API 面保持不变），
//! 但运行时只在 Windows 可用：
//! - Windows：真实 FFI 实现（加载 DownloadSDKProxy.dll）。
//! - 非 Windows：`loader::ensure_dlls_loaded` 直接返回
//!   `Err("仅支持 Windows")`，所有经由 `XunleiHandle::new` 的路径都会安全失败，
//!   不产生任何 FFI 调用（上层 API 不变，调用方拿到可读错误）。
//!
//! 这样 btcore（xunlei feature）/ xunlei-convert 等下游 crate 在 Linux
//! 可正常编译与做纯逻辑测试，只有真正触碰 SDK 运行时才报错。

#![cfg_attr(not(windows), allow(dead_code))]

pub mod bindings;
pub mod dcdn;
pub mod error;
pub mod handle;
pub mod identity;
pub mod loader;
pub mod peer;
pub mod query;
pub mod task;
pub mod tracker;

pub use error::{Result, XunleiError};
pub use handle::XunleiHandle;
