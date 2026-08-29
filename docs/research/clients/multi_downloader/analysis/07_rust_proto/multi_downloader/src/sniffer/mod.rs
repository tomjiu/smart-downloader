//! Sniffer framework — FileCentipede-style (analysis §4).
//!
//! The sniffer extracts download-able URLs from raw HTML / JSON / text
//! payloads. We deliberately **do not** ship a browser extension; the sniffer
//! is purely an in-process utility for the engine to consume captured page
//! bodies (e.g. via a future filec:// URI handler).

pub mod rule_engine;
pub mod url_extractor;

pub use rule_engine::{RuleLayer, SnifferRule, SnifferRuleEngine};
pub use url_extractor::{UrlExtractor, UrlKind};
