//! URL extractor — FileCentipede analysis §4.4 / §4.5.
//!
//! The extractor takes raw HTML / JSON / text payloads and pulls out URLs
//! that point to downloadable resources. FileCentipede uses a multi-strategy
//! approach (analysis §4.4):
//!
//! 1. `<a href>`, `<video src>`, `<source src>`, `<embed>` tags.
//! 2. Inline JSON containing URL-like strings (e.g. API responses).
//! 3. `data:` URI payloads (e.g. base64-encoded m3u8).
//! 4. `<iframe>` sources (recursive — we surface them but don't follow here).
//!
//! Each extracted URL is classified as `Http` / `Magnet` / `Data` etc.

use regex::Regex;
use serde::{Deserialize, Serialize};
use url::Url;

use super::rule_engine::SnifferRuleEngine;

/// What kind of URL was extracted?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UrlKind {
    /// Plain HTTP(S).
    Http,
    /// `magnet:?xt=urn:btih:...`.
    Magnet,
    /// `data:` URI.
    Data,
    /// `ed2k://` or other custom protocol.
    Other,
}

/// An extracted URL along with its classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedUrl {
    pub url: Url,
    pub kind: UrlKind,
    /// Layer / source that produced this URL.
    pub via: ExtractorSource,
    /// MIME hint captured from the page (if any).
    pub mime_hint: Option<String>,
}

/// Which extraction strategy produced the URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExtractorSource {
    /// `<a href>`.
    Anchor,
    /// `<video>/<audio>/<source>/<embed>`.
    Media,
    /// `<iframe>`.
    Iframe,
    /// JSON or plain text body.
    Text,
    /// Regex scan of the whole document.
    Regex,
}

/// The extractor.
pub struct UrlExtractor {
    /// URL-ish regex (matches http(s)://..., magnet:?, data:).
    url_re: Regex,
    /// Optional rule engine for filtering.
    rules: SnifferRuleEngine,
}

impl UrlExtractor {
    /// Build a new extractor with default rules.
    #[must_use]
    pub fn new() -> Self {
        Self {
            url_re: Regex::new(
                r#"(?i)\b((?:https?|ftp|file|data|magnet|ed2k)://[^\s"'<>\)]+)"#,
            )
            .expect("url regex"),
            rules: SnifferRuleEngine::with_defaults(),
        }
    }

    /// Build an extractor that uses a custom rule engine.
    #[must_use]
    pub fn with_rules(rules: SnifferRuleEngine) -> Self {
        Self {
            url_re: Regex::new(r#"(?i)\b((?:https?|ftp|file|data|magnet|ed2k)://[^\s"'<>\)]+)"#)
                .expect("url regex"),
            rules,
        }
    }

    /// Extract all sniffable URLs from a raw HTML / text body.
    ///
    /// `base_url` is used to resolve relative URLs found in attribute values.
    #[must_use]
    pub fn extract(&self, body: &str, base_url: &Url) -> Vec<ExtractedUrl> {
        let mut out = Vec::new();
        // 1. Anchor / media / iframe via attribute parsing.
        let tag_specs: &[(ExtractorSource, &str)] = &[
            (ExtractorSource::Anchor, "a"),
            (ExtractorSource::Media, "video"),
            (ExtractorSource::Media, "audio"),
            (ExtractorSource::Media, "source"),
            (ExtractorSource::Media, "embed"),
            (ExtractorSource::Iframe, "iframe"),
        ];
        for &(via, tag_attr) in tag_specs {
            let srcs = extract_attrs(body, tag_attr, "src");
            let hrefs = extract_attrs(body, tag_attr, "href");
            for raw in srcs.into_iter().chain(hrefs) {
                if let Ok(u) = base_url.join(&raw) {
                    self.maybe_push(&mut out, u, via, None);
                }
            }
        }
        // 2. Regex sweep of the entire body.
        for cap in self.url_re.captures_iter(body) {
            let s = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            if let Ok(u) = Url::parse(s) {
                self.maybe_push(&mut out, u, ExtractorSource::Regex, None);
            } else if let Ok(u) = base_url.join(s) {
                self.maybe_push(&mut out, u, ExtractorSource::Regex, None);
            }
        }
        out
    }

    fn maybe_push(
        &self,
        out: &mut Vec<ExtractedUrl>,
        url: Url,
        via: ExtractorSource,
        mime_hint: Option<String>,
    ) {
        let kind = classify(&url);
        // Apply rule engine — drop URLs that aren't sniffable.
        if let Some((_layer, hint)) = self.rules.evaluate(&url, mime_hint.as_deref().unwrap_or("")) {
            let mut eu = ExtractedUrl {
                url,
                kind,
                via,
                mime_hint: mime_hint.clone(),
            };
            if let Some(h) = hint {
                if h.eq_ignore_ascii_case("magnet") {
                    eu.kind = UrlKind::Magnet;
                }
            }
            out.push(eu);
        } else {
            tracing::trace!(url = %url.as_str(), "skipped (not sniffable)");
        }
    }

    /// Number of rules in the underlying engine.
    #[must_use]
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }
}

impl Default for UrlExtractor {
    fn default() -> Self {
        Self::new()
    }
}

/// Classify a URL by its scheme.
fn classify(u: &Url) -> UrlKind {
    match u.scheme() {
        "http" | "https" | "ftp" | "file" => UrlKind::Http,
        "magnet" => UrlKind::Magnet,
        "data" => UrlKind::Data,
        _ => UrlKind::Other,
    }
}

/// Minimal HTML attribute extractor — pulls `attr` values from `<tag ...>`
/// occurrences. Not a full HTML parser; deliberately small for the prototype.
fn extract_attrs(html: &str, tag: &str, attr: &str) -> Vec<String> {
    let mut out = Vec::new();
    let pat = format!(
        r#"(?i)<{tag}\b[^>]*\b{attr}\s*=\s*["']([^"']+)["']"#
    );
    if let Ok(re) = Regex::new(&pat) {
        for c in re.captures_iter(html) {
            if let Some(m) = c.get(1) {
                out.push(m.as_str().to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_anchor_href() {
        let ex = UrlExtractor::new();
        let base = Url::parse("https://example.com/page").unwrap();
        let html = r#"<html><body><a href="https://x.com/file.zip">d</a></body></html>"#;
        let urls = ex.extract(html, &base);
        assert!(urls.iter().any(|u| u.url.as_str().ends_with("file.zip")));
    }

    #[test]
    fn extracts_video_src() {
        let ex = UrlExtractor::new();
        let base = Url::parse("https://example.com/page").unwrap();
        let html = r#"<video src="https://cdn.example.com/v.mp4"></video>"#;
        let urls = ex.extract(html, &base);
        assert!(urls.iter().any(|u| u.url.as_str().ends_with("v.mp4")));
    }

    #[test]
    fn extracts_magnet_from_text() {
        let ex = UrlExtractor::new();
        let base = Url::parse("https://example.com/page").unwrap();
        let body = "Check this out: magnet:?xt=urn:btih:abcd1234&dn=test";
        let urls = ex.extract(body, &base);
        assert!(urls.iter().any(|u| u.kind == UrlKind::Magnet));
    }

    #[test]
    fn denies_ads() {
        let ex = UrlExtractor::new();
        let base = Url::parse("https://example.com/page").unwrap();
        let html = r#"<a href="https://doubleclick.net/ad.mp4">ad</a>"#;
        let urls = ex.extract(html, &base);
        assert!(urls.iter().filter(|u| u.url.as_str().contains("doubleclick")).count() == 0);
    }

    #[test]
    fn skips_unmatched_extension() {
        let ex = UrlExtractor::new();
        let base = Url::parse("https://example.com/page").unwrap();
        let html = r#"<a href="https://x.com/y.unknownext">x</a>"#;
        let urls = ex.extract(html, &base);
        assert!(urls.is_empty());
    }
}
