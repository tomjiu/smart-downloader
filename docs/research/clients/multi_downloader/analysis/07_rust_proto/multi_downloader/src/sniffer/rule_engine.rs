//! Three-layer sniffer rule engine — FileCentipede analysis §4.3.
//!
//! FileCentipede matches each captured URL against three independent layers
//! (analysis §4.3 / §4.6):
//!
//! 1. **Extension** — match by file suffix (`.mp4`, `.zip`, `.iso`, ...).
//! 2. **MIME type** — match by `Content-Type` header (`video/mp4`, ...).
//! 3. **Regex** — match by URL pattern (e.g. `\\.m3u8$`, `cdn\\.example\\.com`).
//!
//! A URL is considered "sniffable" if **any** layer matches (OR semantics),
//! unless a deny-rule explicitly suppresses it.

use regex::Regex;
use serde::{Deserialize, Serialize};
use url::Url;

/// Which layer produced a match (used for diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RuleLayer {
    /// File-extension match.
    Extension,
    /// MIME-type match.
    MimeType,
    /// Regex match against the URL.
    Regex,
}

/// A single sniffer rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnifferRule {
    /// Rule name (for diagnostics / logging).
    pub name: String,
    /// Layer this rule operates on.
    pub layer: RuleLayer,
    /// Pattern (file extension without leading dot for `Extension`; MIME
    /// string for `MimeType`; regex source for `Regex`).
    pub pattern: String,
    /// Allow (true) or deny (false). Deny rules suppress matches from
    /// other layers.
    pub allow: bool,
    /// Optional protocol hint (http / magnet / etc.).
    pub protocol_hint: Option<String>,
}

impl SnifferRule {
    /// Compile the rule into a matcher (compiles regex for Regex layer).
    ///
    /// # Errors
    /// Returns the regex compilation error if the pattern doesn't compile.
    pub fn compile(&self) -> Result<CompiledRule, regex::Error> {
        let re = if matches!(self.layer, RuleLayer::Regex) {
            Some(Regex::new(&self.pattern)?)
        } else {
            None
        };
        Ok(CompiledRule {
            rule: self.clone(),
            re,
        })
    }
}

/// A pre-compiled rule (regex compiled once at engine construction time).
pub struct CompiledRule {
    pub rule: SnifferRule,
    re: Option<Regex>,
}

impl CompiledRule {
    /// Test whether the rule matches the given (url, mime, suffix) tuple.
    #[must_use]
    pub fn matches(&self, url: &Url, mime: &str, suffix: &str) -> bool {
        match self.rule.layer {
            RuleLayer::Extension => {
                let pat = self.rule.pattern.trim_start_matches('.');
                !pat.is_empty() && suffix.eq_ignore_ascii_case(pat)
            }
            RuleLayer::MimeType => mime.eq_ignore_ascii_case(&self.rule.pattern),
            RuleLayer::Regex => self
                .re
                .as_ref()
                .map_or(false, |r| r.is_match(url.as_str())),
        }
    }
}

/// The rule engine.
pub struct SnifferRuleEngine {
    rules: Vec<CompiledRule>,
}

impl SnifferRuleEngine {
    /// Construct a new engine with the default rule set (a subset of
    /// FileCentipede's defaults, analysis §4.6).
    #[must_use]
    pub fn with_defaults() -> Self {
        let mut e = Self::empty();
        // Default allow-rules (FileCentipede analysis §4.6).
        for (ext, hint) in &[
            ("exe", "http"),
            ("zip", "http"),
            ("rar", "http"),
            ("7z", "http"),
            ("tar", "http"),
            ("gz", "http"),
            ("iso", "http"),
            ("mp4", "http"),
            ("mkv", "http"),
            ("avi", "http"),
            ("mov", "http"),
            ("m3u8", "http"),
            ("mpd", "http"),
            ("torrent", "torrent"),
            ("magnet", "magnet"),
        ] {
            e.add(SnifferRule {
                name: format!("ext-{ext}"),
                layer: RuleLayer::Extension,
                pattern: (*ext).into(),
                allow: true,
                protocol_hint: Some((*hint).into()),
            })
            .expect("default rule compiles");
        }
        // MIME allow-rules.
        for mime in &[
            "application/octet-stream",
            "application/zip",
            "application/x-tar",
            "application/x-rar-compressed",
            "video/mp4",
            "video/x-matroska",
            "application/x-bittorrent",
        ] {
            e.add(SnifferRule {
                name: format!("mime-{mime}"),
                layer: RuleLayer::MimeType,
                pattern: (*mime).into(),
                allow: true,
                protocol_hint: None,
            })
            .expect("default rule compiles");
        }
        // Regex deny-rule example: ignore query-style URLs that look like ads.
        e.add(SnifferRule {
            name: "deny-ads".into(),
            layer: RuleLayer::Regex,
            pattern: r"(?i)doubleclick\.net|googlesyndication\.com".into(),
            allow: false,
            protocol_hint: None,
        })
        .expect("default rule compiles");
        e
    }

    /// Construct an empty engine.
    #[must_use]
    pub fn empty() -> Self {
        Self { rules: Vec::new() }
    }

    /// Add a rule (compiles its regex if applicable).
    ///
    /// # Errors
    /// Returns `regex::Error` if the regex doesn't compile.
    pub fn add(&mut self, rule: SnifferRule) -> Result<(), regex::Error> {
        self.rules.push(rule.compile()?);
        Ok(())
    }

    /// Evaluate whether a URL should be sniffed.
    ///
    /// Returns `Some((layer, hint))` if allowed, `None` if no allow-rule
    /// matched or a deny-rule suppressed it.
    #[must_use]
    pub fn evaluate(&self, url: &Url, mime: &str) -> Option<(RuleLayer, Option<String>)> {
        let suffix = url
            .path()
            .rsplit('.')
            .next()
            .filter(|s| !s.is_empty() && !s.contains('/'))
            .unwrap_or("");
        let mut allowed: Option<(RuleLayer, Option<String>)> = None;
        let mut denied = false;
        for r in &self.rules {
            if !r.matches(url, mime, suffix) {
                continue;
            }
            if r.rule.allow {
                if allowed.is_none() {
                    allowed = Some((r.rule.layer, r.rule.protocol_hint.clone()));
                }
            } else {
                denied = true;
                break;
            }
        }
        if denied {
            return None;
        }
        allowed
    }

    /// Number of rules loaded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// True if the engine holds no rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_match() {
        let e = SnifferRuleEngine::with_defaults();
        let u = Url::parse("https://x/y.mp4").unwrap();
        let r = e.evaluate(&u, "").unwrap();
        assert_eq!(r.0, RuleLayer::Extension);
        assert_eq!(r.1.as_deref(), Some("http"));
    }

    #[test]
    fn mime_match() {
        let e = SnifferRuleEngine::with_defaults();
        let u = Url::parse("https://x/some-path").unwrap();
        let r = e.evaluate(&u, "application/zip").unwrap();
        assert_eq!(r.0, RuleLayer::MimeType);
    }

    #[test]
    fn deny_overrides_allow() {
        let e = SnifferRuleEngine::with_defaults();
        let u = Url::parse("https://doubleclick.net/ad.mp4").unwrap();
        assert!(e.evaluate(&u, "").is_none());
    }

    #[test]
    fn unknown_extension_not_matched() {
        let e = SnifferRuleEngine::with_defaults();
        let u = Url::parse("https://x/y.weirdext").unwrap();
        assert!(e.evaluate(&u, "").is_none());
    }

    #[test]
    fn torrent_extension_returns_torrent_hint() {
        let e = SnifferRuleEngine::with_defaults();
        let u = Url::parse("https://x/y.torrent").unwrap();
        let r = e.evaluate(&u, "").unwrap();
        assert_eq!(r.1.as_deref(), Some("torrent"));
    }
}
