//! HTTP/FTP 引擎（M4 交付）：reqwest 传输 + 自研调度层（分块/续传/重试/换源/镜像/校验）。
//! M4a 骨架：Range 探测 / 静态分块规划 / .part 续传决策 / 重试退避 / 引擎生命周期。
//! M4b：多连接并行 / 镜像 / update_sources 换源 / ContentIdentity 校验。
//! M4c：FTP 子集（feature=`ftp`）：PASV / REST 续传 / 421 退避。

// 测试以 `httpdl::` 引用（包名为 smart-dl-httpdl）。
extern crate self as httpdl;

pub mod download;
pub mod engine;
pub mod ledger;
pub mod range;
pub mod rate;
pub mod retry;
pub mod segment_manager;
pub mod static_split;
pub mod verify;

#[cfg(feature = "ftp")]
pub mod protocol;

pub use engine::{build_proxied_client, proxy_auth_of, HttpEngine};

#[cfg(feature = "ftp")]
pub use protocol::ftp::FtpEngine;
