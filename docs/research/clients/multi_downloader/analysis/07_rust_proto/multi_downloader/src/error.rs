//! Three-segment error code, lifted from the Quark Cloud Drive installer.
//!
//! Quark's installer (see `analysis/05_quark/quark_architecture.md` §4.2) emits
//! a log line of the form
//!
//! ```text
//! [tid %lu][quark_installer][error] download slice failed. task_id: %d,
//! error_code: %d, extra_error_code: %d, retry_count: %d
//! ```
//!
//! The two-code split (`error_code` for business layer + `extra_error_code` for
//! OS / TLS layer) makes it possible to tell a 502 from the upstream HTTP
//! gateway apart from a `WSAECONNRESET` from the local TCP stack. We borrow
//! the design verbatim but rehome it into `thiserror` / `anyhow` idioms.

use std::collections::HashMap;
use std::fmt;

use thiserror::Error;

/// Crate-level `Result` alias.
pub type Result<T> = std::result::Result<T, DownloadError>;

/// High-level classification of an error, used for the `error_code` field.
///
/// Numbers are kept stable so logs from old builds remain greppable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ErrorCategory {
    /// HTTP 2xx → no error. (Used internally to indicate success categories.)
    HttpOk = 200,
    /// HTTP 3xx redirect.
    HttpRedirect = 300,
    /// HTTP 4xx client error.
    HttpClient = 400,
    /// HTTP 5xx server error.
    HttpServer = 500,
    /// Network-layer error (DNS / TCP / TLS handshake).
    Network = -1,
    /// TLS handshake / certificate validation failure.
    Tls = -2,
    /// JSON / Bencode / message format error.
    Protocol = -3,
    /// Local file IO error.
    Io = -4,
    /// Hash / integrity verification failed.
    Integrity = -5,
    /// Task was cancelled by the user / scheduler.
    Cancelled = -6,
    /// Feature is not implemented in this prototype.
    Unimplemented = -99,
}

impl ErrorCategory {
    /// Convert into the `error_code` slot of `DownloadError`.
    #[must_use]
    pub fn code(self) -> i32 {
        self as i32
    }
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HttpOk => write!(f, "HTTP_OK"),
            Self::HttpRedirect => write!(f, "HTTP_REDIRECT"),
            Self::HttpClient => write!(f, "HTTP_CLIENT"),
            Self::HttpServer => write!(f, "HTTP_SERVER"),
            Self::Network => write!(f, "NETWORK"),
            Self::Tls => write!(f, "TLS"),
            Self::Protocol => write!(f, "PROTOCOL"),
            Self::Io => write!(f, "IO"),
            Self::Integrity => write!(f, "INTEGRITY"),
            Self::Cancelled => write!(f, "CANCELLED"),
            Self::Unimplemented => write!(f, "UNIMPLEMENTED"),
        }
    }
}

/// Quark-style three-segment error.
///
/// Fields map 1:1 to Quark's `task_id` / `error_code` / `extra_error_code` /
/// `retry_count` tuple, with an extra `context` map for ad-hoc structured
/// fields (URL, mirror name, peer id, …).
#[derive(Debug, Clone, Error)]
#[error("task_id={} category={} error_code={} extra_error_code={} retry_count={} msg={}",
        task_id, category, error_code, extra_error_code, retry_count)]
pub struct DownloadError {
    /// Owning task (0 = unscoped / library-level error).
    pub task_id: u64,
    /// High-level category (drives `error_code`).
    pub category: ErrorCategory,
    /// Primary code — HTTP status code, business code, or category code.
    pub error_code: i32,
    /// Secondary code — OS errno / TLS alert / WSA errno equivalent.
    pub extra_error_code: i32,
    /// How many retries have already been attempted for this error.
    pub retry_count: u32,
    /// Human-readable message.
    pub message: String,
    /// Free-form structured context (URL, mirror, peer_id, …).
    pub context: HashMap<String, String>,
}

impl DownloadError {
    /// Build a new error, deriving `error_code` from `category`.
    #[must_use]
    pub fn new(task_id: u64, category: ErrorCategory, message: impl Into<String>) -> Self {
        Self {
            task_id,
            category,
            error_code: category.code(),
            extra_error_code: 0,
            retry_count: 0,
            message: message.into(),
            context: HashMap::new(),
        }
    }

    /// Attach an OS-level / TLS-level secondary code.
    #[must_use]
    pub fn with_extra(mut self, extra: i32) -> Self {
        self.extra_error_code = extra;
        self
    }

    /// Attach a structured key-value pair (chainable).
    #[must_use]
    pub fn with_context(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.context.insert(k.into(), v.into());
        self
    }

    /// Bump retry counter (chainable).
    #[must_use]
    pub fn inc_retry(mut self) -> Self {
        self.retry_count = self.retry_count.saturating_add(1);
        self
    }

    /// True if the error is worth retrying (transient / network / 5xx).
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.category,
            ErrorCategory::Network
                | ErrorCategory::Tls
                | ErrorCategory::HttpServer
                | ErrorCategory::Integrity
        ) && self.retry_count < MAX_RETRY
    }
}

/// Maximum retry attempts before the task is abandoned (matches Quark's
/// observed MAX_RETRY = 5, see analysis §4.1).
pub const MAX_RETRY: u32 = 5;

impl From<std::io::Error> for DownloadError {
    fn from(e: std::io::Error) -> Self {
        Self::new(0, ErrorCategory::Io, e.to_string())
            .with_extra(e.raw_os_error().unwrap_or(0))
    }
}

impl From<rusqlite::Error> for DownloadError {
    fn from(e: rusqlite::Error) -> Self {
        Self::new(0, ErrorCategory::Io, e.to_string())
            .with_extra(e.extended_code())
    }
}

impl From<reqwest::Error> for DownloadError {
    fn from(e: reqwest::Error) -> Self {
        let cat = if e.is_connect() || e.is_timeout() {
            ErrorCategory::Network
        } else if e.is_body() || e.is_decode() {
            ErrorCategory::Protocol
        } else {
            ErrorCategory::Network
        };
        Self::new(0, cat, e.to_string())
    }
}

impl From<url::ParseError> for DownloadError {
    fn from(e: url::ParseError) -> Self {
        Self::new(0, ErrorCategory::Protocol, e.to_string())
    }
}

/// Convert any `anyhow::Error` into a `DownloadError` (used at boundaries).
impl From<anyhow::Error> for DownloadError {
    fn from(e: anyhow::Error) -> Self {
        Self::new(0, ErrorCategory::Unimplemented, e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_codes_are_stable() {
        assert_eq!(ErrorCategory::Network.code(), -1);
        assert_eq!(ErrorCategory::Tls.code(), -2);
        assert_eq!(ErrorCategory::HttpServer.code(), 500);
        assert_eq!(ErrorCategory::Unimplemented.code(), -99);
    }

    #[test]
    fn retryable_logic_is_correct() {
        let mut e = DownloadError::new(1, ErrorCategory::Network, "conn reset");
        assert!(e.is_retryable());
        for _ in 0..MAX_RETRY {
            e = e.inc_retry();
        }
        assert!(!e.is_retryable());
    }

    #[test]
    fn context_is_chainable() {
        let e = DownloadError::new(2, ErrorCategory::HttpServer, "502")
            .with_extra(10054)
            .with_context("mirror", "example.com");
        assert_eq!(e.context.get("mirror").map(String::as_str), Some("example.com"));
        assert_eq!(e.extra_error_code, 10054);
    }

    #[test]
    fn io_error_round_trip() {
        let io = std::io::Error::from_raw_os_error(13); // EACCES
        let e: DownloadError = io.into();
        assert_eq!(e.category, ErrorCategory::Io);
        assert_eq!(e.extra_error_code, 13);
    }
}
