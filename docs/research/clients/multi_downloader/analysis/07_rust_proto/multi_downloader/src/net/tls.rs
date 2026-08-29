//! rustls-based HTTPS client factory.
//!
//! Borrows Quark's TLS posture (analysis §6):
//!
//! - TLS 1.3 cipher suites (`TLS_AES_256_GCM_SHA384`,
//!   `TLS_CHACHA20_POLY1305_SHA256`, `TLS_AES_128_GCM_SHA256`).
//! - Perfect forward secrecy via ECDHE key exchange.
//! - Single static client config (no per-task handshakes).
//!
//! Critical departures from Quark:
//!
//! - **Cross-platform root store**: we use `webpki-roots` (Mozilla bundle)
//!   rather than the OS cert store. Quark reads the Windows `ROOT` / `CA`
//!   store, which is platform-specific and a vector for enterprise-root
//!   injection attacks.
//! - **No RC4**: Tixati's MSE/PE uses RC4 (analysis §7.4); we reject it.
//!   AEAD cipher suites only.

use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::HandshakeSignatureValid;
use rustls::crypto::{ring as ring_provider, CryptoProvider};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use tracing::debug;

use crate::config::AppConfig;
use crate::error::{DownloadError, ErrorCategory, Result};

/// Build a `reqwest::Client` with the project's TLS posture baked in.
///
/// # Errors
/// Returns a `DownloadError` of category `Tls` if the rustls client config
/// cannot be assembled (e.g. webpki-roots unavailable at build time).
pub fn build_https_client(cfg: &AppConfig) -> Result<reqwest::Client> {
    let tls_cfg = build_rustls_config()?;

    let mut builder = reqwest::Client::builder()
        .use_preconfigured_tls(tls_cfg)
        .user_agent(&cfg.user_agent)
        .pool_idle_timeout(Some(Duration::from_secs(60)))
        .pool_max_idle_per_host(8)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(0)) // no global timeout (slices handle their own)
        .redirect(reqwest::redirect::Policy::limited(10));

    if let Some(p) = &cfg.proxy {
        builder = match super::proxy::ProxyConfig::parse(p) {
            Ok(parsed) => builder.proxy(parsed.into_reqwest()),
            Err(e) => return Err(e),
        };
    }

    builder
        .build()
        .map_err(|e| DownloadError::new(0, ErrorCategory::Tls, e.to_string()))
}

/// Build the rustls `ClientConfig` — exposed for callers that want to use
/// rustls directly without reqwest (e.g. raw BT/uTP paths in the future).
///
/// # Errors
/// Only fails if the `aws_lc_rs` default provider can't be installed or the
/// root store can't be loaded.
pub fn build_rustls_config() -> Result<ClientConfig> {
    // Use aws_lc_rs as the crypto provider (FIPS-eligible, recommended by
    // rustls 0.23).
    if CryptoProvider::get_default().is_none() {
        aws_lc_rs::default_provider()
            .install_default()
            .map_err(|e| {
                DownloadError::new(0, ErrorCategory::Tls, format!("crypto install: {e}"))
            })?;
    }

    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    debug!(n_roots = roots.len(), "loaded webpki-roots");

    let cfg = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(cfg)
}

/// A trivial in-memory cert verifier used only by tests that want to disable
/// verification. **Never use this in production code.**
#[derive(Debug)]
pub struct NoVerifyVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifyVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ED25519,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_https_client() {
        let cfg = AppConfig::default();
        let c = build_https_client(&cfg);
        assert!(c.is_ok(), "{:?}", c.err());
    }

    #[test]
    fn builds_rustls_config_with_webpki_roots() {
        let c = build_rustls_config();
        assert!(c.is_ok());
        let _ = c.unwrap();
    }
}
