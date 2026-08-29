//! Network primitives.
//!
//! - `tls` — rustls configuration (replaces Quark's static OpenSSL).
//! - `socket_pool` — keep-alive connection pool (FlashGet-style).
//! - `proxy` — HTTP / SOCKS5 proxy support.

pub mod proxy;
pub mod socket_pool;
pub mod tls;

pub use proxy::ProxyConfig;
pub use socket_pool::SocketPool;
pub use tls::build_https_client;
