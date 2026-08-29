//! Proxy configuration (HTTP / SOCKS5).
//!
//! Borrowed from FileCentipede's per-task proxy override (analysis §11).
//! Default is "direct" — no proxy is applied unless the user explicitly
//! configures one via `AppConfig::proxy`.

use serde::{Deserialize, Serialize};

use crate::error::{DownloadError, ErrorCategory, Result};

/// Supported proxy schemes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProxyKind {
    /// `http://` proxy (HTTP CONNECT for HTTPS targets).
    Http,
    /// `https://` proxy (TLS to the proxy itself).
    Https,
    /// `socks5://` proxy (SOCKS5h — remote DNS resolution).
    Socks5,
}

/// A parsed proxy URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl ProxyConfig {
    /// Parse a proxy URL string like `socks5://user:pass@host:1080`.
    ///
    /// # Errors
    /// Returns a `DownloadError` with category `Protocol` if the URL is
    /// malformed or uses an unsupported scheme.
    pub fn parse(s: &str) -> Result<Self> {
        let url = url::Url::parse(s).map_err(|e| {
            DownloadError::new(0, ErrorCategory::Protocol, e.to_string())
        })?;
        let kind = match url.scheme() {
            "http" => ProxyKind::Http,
            "https" => ProxyKind::Https,
            "socks5" | "socks5h" => ProxyKind::Socks5,
            other => {
                return Err(DownloadError::new(
                    0,
                    ErrorCategory::Protocol,
                    format!("unsupported proxy scheme: {other}"),
                ));
            }
        };
        let host = url
            .host_str()
            .ok_or_else(|| DownloadError::new(0, ErrorCategory::Protocol, "missing proxy host"))?
            .to_string();
        let port = url.port_or_known_default().unwrap_or(match kind {
            ProxyKind::Http | ProxyKind::Https => 8080,
            ProxyKind::Socks5 => 1080,
        });
        let username = url.username().split(':').next().filter(|s| !s.is_empty()).map(str::to_string);
        let password = url.password().map(str::to_string);
        Ok(Self {
            kind,
            host,
            port,
            username,
            password,
        })
    }

    /// Convert into a `reqwest::Proxy` suitable for the `reqwest::ClientBuilder`.
    #[must_use]
    pub fn into_reqwest(self) -> reqwest::Proxy {
        let scheme = match self.kind {
            ProxyKind::Http => "http://",
            ProxyKind::Https => "https://",
            ProxyKind::Socks5 => "socks5h://",
        };
        let user_part = match (&self.username, &self.password) {
            (Some(u), Some(p)) => format!("{u}:{p}@"),
            (Some(u), None) => format!("{u}@"),
            _ => String::new(),
        };
        let url = format!("{scheme}{user_part}{}:{}", self.host, self.port);
        match reqwest::Proxy::all(&url) {
            Ok(p) => p,
            Err(_) => reqwest::Proxy::all("http://127.0.0.1:0").expect("dummy proxy"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_proxy() {
        let p = ProxyConfig::parse("http://proxy.example.com:3128").unwrap();
        assert_eq!(p.kind, ProxyKind::Http);
        assert_eq!(p.host, "proxy.example.com");
        assert_eq!(p.port, 3128);
    }

    #[test]
    fn parses_socks5_with_auth() {
        let p = ProxyConfig::parse("socks5://u:p@host:1080").unwrap();
        assert_eq!(p.kind, ProxyKind::Socks5);
        assert_eq!(p.username.as_deref(), Some("u"));
        assert_eq!(p.password.as_deref(), Some("p"));
        assert_eq!(p.port, 1080);
    }

    #[test]
    fn rejects_unsupported_scheme() {
        let p = ProxyConfig::parse("ftp://x");
        assert!(p.is_err());
        assert_eq!(p.unwrap_err().category, ErrorCategory::Protocol);
    }
}
