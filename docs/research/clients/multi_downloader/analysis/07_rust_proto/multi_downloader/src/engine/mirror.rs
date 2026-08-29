//! Mirror discovery + speed test — the FlashGet algorithm
//! (analysis §5). Borrows the four mirror sources, the HEAD-probe +
//! 64 KB GET speed test, and the weighted scoring formula
//! `speed*0.6 + 1/latency*0.3 + reliability*0.1`.
//!
//! **Crucially**: mirror discovery is off by default (`AppConfig::enable_mirror_discovery`).
//! This is a deliberate rejection of FlashGet 3.x's P2SP opt-out behavior —
//! the privacy / IP-leakage concerns are real and well documented in
//! analysis §7.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use url::Url;

/// A candidate mirror. Carries everything the score function needs at eval
/// time, plus mutable state (`reliability`, `last_fail_at`) for cooling
/// down after failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mirror {
    /// Numeric id (assigned by discovery).
    pub id: i32,
    /// Source URL.
    pub url: Url,
    /// How this mirror was discovered (FlashGet §5.1).
    pub source: MirrorSource,
    /// Historical reliability in `[0.0, 1.0]` (1.0 = never failed).
    pub reliability: f64,
    /// Optional user-agent override for this mirror.
    pub user_agent: Option<String>,
    /// Optional referer for this mirror.
    pub referer: Option<String>,
    /// Last failure time (Unix secs; `None` = never failed).
    pub last_fail_at: Option<u64>,
    /// Number of consecutive failures.
    pub consecutive_failures: u32,
}

/// The four FlashGet mirror sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MirrorSource {
    /// User manually added.
    User,
    /// Captured during 301/302 redirect chain.
    Redirect,
    /// From a global known-mirror list (FlashGet `mirrors.xml`).
    KnownList,
    /// From CMS / dynamic config (Quark-style `open-cms-api`).
    Cms,
}

/// Result of probing a single mirror.
#[derive(Debug, Clone)]
pub struct MirrorProbe {
    pub mirror_id: i32,
    pub alive: bool,
    pub reason: Option<String>,
    pub latency: Duration,
    pub supports_range: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub speed_bytes_per_sec: f64,
    pub content_length: u64,
}

/// Weighted score for a mirror — the FlashGet formula.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MirrorScore(pub f64);

impl std::ops::Deref for MirrorScore {
    type Target = f64;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Weights (FlashGet analysis §5.5).
pub const W_SPEED: f64 = 0.6;
pub const W_LATENCY: f64 = 0.3;
pub const W_RELIABILITY: f64 = 0.1;

/// Compute the FlashGet weighted score for a probe.
///
/// Formula (analysis §5.5):
/// ```text
/// score = speed * 0.6 + (1 / latency_secs) * 100 * 0.3 + reliability * 1000 * 0.1
/// ```
///
/// Latency is multiplied by 100 (so a 1 s RTT yields 100 score-points) and
/// reliability by 1000 so the three terms sit in comparable magnitudes.
#[must_use]
pub fn score_mirror(p: &MirrorProbe, m: &Mirror) -> MirrorScore {
    if !p.alive || !p.supports_range {
        return MirrorScore(0.0);
    }
    let speed = p.speed_bytes_per_sec;
    let lat_s = p.latency.as_secs_f64().max(0.001);
    let lat_score = 1.0 / lat_s * 100.0;
    let rel_score = m.reliability.clamp(0.0, 1.0) * 1000.0;
    MirrorScore(speed * W_SPEED + lat_score * W_LATENCY + rel_score * W_RELIABILITY)
}

/// Discovery service — collects mirrors, probes them, returns ranked list.
pub struct MirrorDiscovery {
    client: reqwest::Client,
    /// All known mirrors, by id.
    mirrors: Arc<RwLock<HashMap<i32, Mirror>>>,
    /// Cooldown duration for failed mirrors (FlashGet = 30 s).
    cooldown: Duration,
    /// Number of bytes downloaded during speed test (FlashGet = 64 KB).
    speed_test_bytes: u64,
    /// Speed-test timeout.
    speed_test_timeout: Duration,
    next_id: parking_lot::Mutex<i32>,
}

impl MirrorDiscovery {
    /// Build a discovery service with FlashGet defaults.
    #[must_use]
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            mirrors: Arc::new(RwLock::new(HashMap::new())),
            cooldown: Duration::from_secs(30),
            speed_test_bytes: 64 * 1024,
            speed_test_timeout: Duration::from_secs(15),
            next_id: parking_lot::Mutex::new(0),
        }
    }

    /// Add a mirror programmatically (returns its id).
    pub fn add(&self, url: Url, source: MirrorSource) -> i32 {
        let mut next = self.next_id.lock();
        *next += 1;
        let id = *next;
        drop(next);
        let m = Mirror {
            id,
            url,
            source,
            reliability: 0.5,
            user_agent: None,
            referer: None,
            last_fail_at: None,
            consecutive_failures: 0,
        };
        self.mirrors.write().insert(id, m);
        id
    }

    /// Discover mirrors from a 301/302 redirect chain (FlashGet §5.2).
    ///
    /// Performs a HEAD request that does **not** auto-follow redirects,
    /// captures each `Location` header, and recurses up to `max_hops` times.
    pub async fn from_redirect_chain(&self, start: &Url, max_hops: usize) -> Vec<i32> {
        let mut ids = Vec::new();
        let mut current = start.clone();
        for _ in 0..max_hops {
            let req = self.client.head(current.clone());
            let resp = match req.send().await {
                Ok(r) => r,
                Err(_) => break,
            };
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            match loc.and_then(|s| current.join(&s).ok()) {
                Some(next) if next != current => {
                    let id = self.add(next.clone(), MirrorSource::Redirect);
                    ids.push(id);
                    current = next;
                }
                _ => break,
            }
        }
        ids
    }

    /// Probe a single mirror with HEAD + 64 KB GET speed test.
    pub async fn probe(&self, mirror_id: i32, expected_size: u64) -> MirrorProbe {
        let mirror = {
            let map = self.mirrors.read();
            map.get(&mirror_id).cloned()
        };
        let Some(mirror) = mirror else {
            return MirrorProbe {
                mirror_id,
                alive: false,
                reason: Some("unknown mirror".into()),
                latency: Duration::ZERO,
                supports_range: false,
                etag: None,
                last_modified: None,
                speed_bytes_per_sec: 0.0,
                content_length: 0,
            };
        };
        // 1. HEAD probe.
        let t0 = Instant::now();
        let resp = match self.client.head(mirror.url.clone()).send().await {
            Ok(r) => r,
            Err(e) => {
                return self.mark_dead(mirror_id, format!("HEAD err: {e}"));
            }
        };
        let latency = t0.elapsed();
        if !resp.status().is_success() {
            return self.mark_dead(mirror_id, format!("HEAD status {}", resp.status()));
        }
        let content_length = resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        if expected_size != 0 && content_length != expected_size {
            return self.mark_dead(mirror_id, format!("size mismatch {content_length}/{expected_size}"));
        }
        let supports_range = resp
            .headers()
            .get(reqwest::header::ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.eq_ignore_ascii_case("bytes"))
            .unwrap_or(false);
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let last_modified = resp
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        // 2. Speed test — 64 KB GET.
        let end_byte = self.speed_test_bytes.saturating_sub(1).max(0);
        let range = format!("bytes=0-{end_byte}");
        let t1 = Instant::now();
        let mut bytes_recv = 0u64;
        let get_result = self
            .client
            .get(mirror.url.clone())
            .header(reqwest::header::RANGE, range)
            .timeout(self.speed_test_timeout)
            .send()
            .await;
        if let Ok(resp) = get_result {
            if resp.status().as_u16() == 206 || resp.status().is_success() {
                if let Ok(body) = resp.bytes().await {
                    bytes_recv = body.len() as u64;
                }
            }
        }
        let elapsed = t1.elapsed();
        let speed = if elapsed.as_secs_f64() > 0.0 {
            bytes_recv as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };

        MirrorProbe {
            mirror_id,
            alive: true,
            reason: None,
            latency,
            supports_range,
            etag,
            last_modified,
            speed_bytes_per_sec: speed,
            content_length,
        }
    }

    fn mark_dead(&self, mirror_id: i32, reason: String) -> MirrorProbe {
        warn!(mirror = mirror_id, %reason, "mirror dead");
        let mut map = self.mirrors.write();
        if let Some(m) = map.get_mut(&mirror_id) {
            m.consecutive_failures = m.consecutive_failures.saturating_add(1);
            m.last_fail_at = Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            );
            m.reliability = (m.reliability - 0.1).max(0.0);
        }
        MirrorProbe {
            mirror_id,
            alive: false,
            reason: Some(reason),
            latency: Duration::ZERO,
            supports_range: false,
            etag: None,
            last_modified: None,
            speed_bytes_per_sec: 0.0,
            content_length: 0,
        }
    }

    /// Rank all mirrors by score.
    pub async fn rank(&self, expected_size: u64) -> Vec<(MirrorScore, Mirror)> {
        let ids: Vec<i32> = self.mirrors.read().keys().copied().collect();
        let mut scored = Vec::with_capacity(ids.len());
        for id in ids {
            let mirror = match self.mirrors.read().get(&id).cloned() {
                Some(m) => m,
                None => continue,
            };
            // Skip cooled-down mirrors.
            if let Some(t) = mirror.last_fail_at {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if now.saturating_sub(t) < self.cooldown.as_secs() {
                    continue;
                }
            }
            let probe = self.probe(id, expected_size).await;
            scored.push((score_mirror(&probe, &mirror), mirror));
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        info!(n = scored.len(), "mirrors ranked");
        scored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_mirror(id: i32, reliability: f64) -> Mirror {
        Mirror {
            id,
            url: Url::parse("https://x/y").unwrap(),
            source: MirrorSource::User,
            reliability,
            user_agent: None,
            referer: None,
            last_fail_at: None,
            consecutive_failures: 0,
        }
    }

    fn mk_probe(latency_ms: u64, speed_bps: f64, range: bool) -> MirrorProbe {
        MirrorProbe {
            mirror_id: 0,
            alive: true,
            reason: None,
            latency: Duration::from_millis(latency_ms),
            supports_range: range,
            etag: None,
            last_modified: None,
            speed_bytes_per_sec: speed_bps,
            content_length: 0,
        }
    }

    #[test]
    fn score_zero_when_unsupported_range() {
        let m = mk_mirror(1, 1.0);
        let p = mk_probe(50, 1_000_000.0, false);
        assert_eq!(score_mirror(&p, &m).0, 0.0);
    }

    #[test]
    fn score_weighted_correctly() {
        // speed = 1_000_000 bps, latency = 100 ms → 1/0.1*100=1000, rel=1.0*1000=1000.
        let m = mk_mirror(1, 1.0);
        let p = mk_probe(100, 1_000_000.0, true);
        let s = score_mirror(&p, &m).0;
        let expected = 1_000_000.0 * 0.6 + 1000.0 * 0.3 + 1000.0 * 0.1;
        assert!((s - expected).abs() < 0.001, "got {s}, expected {expected}");
    }

    #[test]
    fn faster_mirror_scores_higher() {
        let m1 = mk_mirror(1, 1.0);
        let m2 = mk_mirror(2, 1.0);
        let slow = mk_probe(100, 100_000.0, true);
        let fast = mk_probe(100, 1_000_000.0, true);
        assert!(score_mirror(&fast, &m2) > score_mirror(&slow, &m1));
    }
}
