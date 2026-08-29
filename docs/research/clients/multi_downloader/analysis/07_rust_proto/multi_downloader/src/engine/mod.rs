//! Protocol engines — the FileCentipede-style protocol abstraction.
//!
//! Each protocol family (HTTP/HTTPS, FTP, BT/Magnet) implements the
//! [`ProtocolEngine`] trait so the scheduler can dispatch on URL scheme
//! without coupling to engine internals.

pub mod bt_engine;
pub mod http_engine;
pub mod mirror;
pub mod protocol;

pub use bt_engine::{BtEngine, BtEngineImpl};
pub use http_engine::HttpEngine;
pub use mirror::{Mirror, MirrorDiscovery, MirrorProbe, MirrorScore};
pub use protocol::{ProtocolEngine, ProtocolKind};
