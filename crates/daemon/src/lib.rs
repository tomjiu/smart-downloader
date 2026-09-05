//! 守护进程（M6 交付）：事件协议（D36）/ WS 背压 / CLI（D26）/ 健康（§11）/
//! HTTP API + 任务状态（M2–M5 集成）。

pub mod baidu_resolve;
#[cfg(feature = "bt")]
pub mod bt;
#[cfg(feature = "bt")]
pub mod bt_events;
pub mod cli;
pub mod client;
pub mod config;
pub mod events;
pub mod health;
pub mod http;
pub mod http_events;
pub mod lockfile;
/// Metalink4（RFC 5854）解析与 add 链路展开（B1）。
pub mod metalink;
/// NAS 版迅雷引擎托管（xllite/pan-cli，Linux-only）。附录 E-2026-08-30。
#[cfg(feature = "nas")]
pub mod nas;
#[cfg(feature = "nas")]
pub mod nas_remote;
pub mod serve;
pub mod state;
pub mod ws;
pub mod xunlei_login;

pub use cli::{Cli, CliCommand, CliError};
pub use events::SchedulerEvent;
pub use ws::WsHub;
