//! smart-dl-btcore：libtorrent 薄核的 Rust safe 门面（M1）。
//! 分层：`ffi`（bindgen + unsafe 包装，unsafe 只在这层）→ `engine::BtCore`（safe API）。
//! 接口契约：`btcore::{BtCore, TorrentStatus, PeerInfo, Alert, ResumeBytes}`。
//! 内存规则（D13）：Rust 预分配缓冲 + cap；LT_ERR_BUFFER_TOO_SMALL → 扩容重试。

// bindgen 产物为 C 风格命名，绑定层内部约定放行
#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

pub mod alerts;
pub mod bare;
pub mod engine;
pub mod ffi;
pub mod resume;
pub mod xudt;
#[cfg(feature = "xunlei")]
pub mod xunlei_engine;

pub use alerts::{Alert, AlertKind, StateSubKind};
pub use bare::Bare;
pub use engine::{peer_flags, BtCore, PeerInfo, TorrentStatus};
pub use ffi::{Error, Result};
pub use resume::ResumeBytes;
#[cfg(feature = "xunlei")]
pub use xunlei_engine::XunleiBtEngine;
