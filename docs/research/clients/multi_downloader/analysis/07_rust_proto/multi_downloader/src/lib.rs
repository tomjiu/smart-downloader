//! # multi_downloader
//!
//! A Rust prototype multi-protocol downloader that borrows design ideas from
//! five existing clients analyzed in `/home/z/my-project/analysis/`:
//!
//! 1. **qBittorrent** — libtorrent wrapping philosophy (BT engine trait ready
//!    for `librqbit` / `libtorrent-rs` integration).
//! 2. **FileCentipede** — protocol abstraction trait + three-layer sniffer rule
//!    engine.
//! 3. **FlashGet** — multi-thread HTTP range downloads + mirror discovery with
//!    weighted scoring (`speed*0.6 + 1/latency*0.3 + reliability*0.1`).
//! 4. **Tixati** — Charity unchoke, Trading Allocation, AutoThrottle (RTT).
//! 5. **Quark Cloud Drive** — slice-based downloads, three-segment error code,
//!    7-stage state machine, `DownloadEventListener` trait.
//!
//! ## Module map
//!
//! | Module | Responsibility | Borrowed from |
//! |---|---|---|
//! | `core` | Task / listener / state machine / scheduler | Quark + FlashGet |
//! | `engine` | HTTP / BT engine / mirror / protocol trait | Quark + FileCentipede + FlashGet |
//! | `bt` | Peer / score / unchoke / bandwidth / RTT / conn FSM | Tixati |
//! | `net` | rustls config + socket pool + proxy | Quark + FlashGet |
//! | `storage` | piece store + SQLite WAL resume + atomic file IO | FlashGet (jettison `.jc!`) |
//! | `sniffer` | URL extractor + rule engine | FileCentipede |
//! | `utils` | Rate limiter + retry / backoff | FlashGet + Quark |
//! | `error` | Three-segment error code | Quark |
//! | `config` | SQLite-persisted config | Quark (jettisoning CMS remote push) |
//!
//! ## Design rules explicitly enforced
//!
//! - No metadata embedded in the downloaded file (rejects FlashGet `.jc!` style).
//! - Mirror discovery / P2SP is **off by default** (must be enabled by user).
//! - Modern AEAD ciphers (AES-GCM / ChaCha20-Poly1305) replace Tixati's RC4 MSE.
//! - No telemetry / report channels (rejects Quark's 4-track reporting).
//! - Single executable binary (no InnoSetup wrapper).
//! - `webpki-roots` instead of OS cert store (cross-platform, no Windows dep).

#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(rust_2021_compatibility)]
#![warn(missing_docs, clippy::all, clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::needless_pass_by_value)]

pub mod bt;
pub mod config;
pub mod core;
pub mod engine;
pub mod error;
pub mod net;
pub mod sniffer;
pub mod storage;
pub mod utils;

/// Crate-wide re-exports of the most commonly used types.
pub mod prelude {
    pub use crate::config::AppConfig;
    pub use crate::core::listener::{DownloadEventListener, NoopListener};
    pub use crate::core::scheduler::TaskScheduler;
    pub use crate::core::state_machine::{StateMachine, Stage};
    pub use crate::core::task::{DownloadTask, Slice, SliceStatus, TaskStatus};
    pub use crate::engine::http_engine::HttpEngine;
    pub use crate::engine::protocol::{ProtocolEngine, ProtocolKind};
    pub use crate::error::{DownloadError, ErrorCategory, Result};
}

/// Library version string, populated at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Initialize the global tracing subscriber. Call once at process start.
///
/// Uses `tracing_subscriber` with `RUST_LOG` env-filter; structured JSON output
/// can be enabled by setting `MDC_LOG_FMT=json`.
pub fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let env = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_json = std::env::var("MDC_LOG_FMT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    if fmt_json {
        tracing_subscriber::registry()
            .with(env)
            .with(fmt::layer().json())
            .init();
    } else {
        tracing_subscriber::registry()
            .with(env)
            .with(fmt::layer().with_target(true))
            .init();
    }
}
