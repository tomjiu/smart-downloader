//! Xunlei DownloadSDK FFI 封装（Windows-only）。
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
//! - `query`    — 状态查询（QueryTaskInfo）

#![cfg_attr(not(windows), allow(dead_code))]
#[cfg(not(windows))]
compile_error!("xunlei-ffi only supports Windows (requires DownloadSDKProxy.dll)");

pub mod bindings;
pub mod error;
pub mod loader;
pub mod handle;
pub mod task;
pub mod peer;
pub mod tracker;
pub mod dcdn;
pub mod identity;
pub mod query;

pub use error::{XunleiError, Result};
pub use handle::XunleiHandle;
