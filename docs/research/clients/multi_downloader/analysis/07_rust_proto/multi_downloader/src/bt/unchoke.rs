//! Unchoke selector — Tixati 3-mode algorithm (analysis §5.4).
//!
//! Three modes:
//!
//! 1. **Forced** — user-pinned unchoke (overrides scoring).
//! 2. **Random** — standard BEP 3 optimistic unchoking (rotation every 30 s).
//! 3. **Charity** — Tixati's signature mode: give low-score peers a chance to
//!    download pieces they need, useful when seeding.
//!
//! Implementation note: the selector is **stateless across calls** except for
//! a `Rng` seeded once at construction time; the rotation is the caller's
//! responsibility (typically a 30 s tick).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

use super::peer::{PeerMetrics, PeerSource, PeerStatus};
use super::peer_score::{peer_score, LocalGeo};

/// Mode of unchoking for a single selection round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnchokeMode {
    /// User-pinned peers (highest priority; bypass scoring).
    Forced,
    /// Standard BEP 3 optimistic unchoking.
    Random,
    /// Tixati Charity: give low-score / slow peers a chance.
    Charity,
}

impl UnchokeMode {
    /// All variants (useful for round-robin iteration).
    #[must_use]
    pub fn all() -> &'static [UnchokeMode] {
        &[UnchokeMode::Forced, UnchokeMode::Random, UnchokeMode::Charity]
    }
}

/// Decision emitted by [`UnchokeSelector::select`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnchokeDecision {
    /// Peer id (we use the peer's `addr` stringified as a stable key).
    pub peer_key: String,
    /// Mode under which this peer was unchoked.
    pub mode: UnchokeMode,
}

/// Maximum peers to unchoke per round (FlashGet/Tixati default = 4).
pub const MAX_UNCHOKE: usize = 4;

/// Interval at which the optimistic slot rotates (Tixati = 30 s).
pub const OPTIMISTIC_INTERVAL: Duration = Duration::from_secs(30);

/// Selects which peers to unchoke.
pub struct UnchokeSelector {
    rng: Mutex<ChaCha8Rng>,
    geo: LocalGeo,
    /// Peers the user has forced unchoked.
    forced: Mutex<HashSet<String>>,
    /// Last optimistic-rotation time.
    last_optimistic: Mutex<Instant>,
}

impl UnchokeSelector {
    /// Build a new selector with the given seed.
    #[must_use]
    pub fn with_seed(seed: u64, geo: LocalGeo) -> Self {
        Self {
            rng: Mutex::new(ChaCha8Rng::seed_from_u64(seed)),
            geo,
            forced: Mutex::new(HashSet::new()),
            last_optimistic: Mutex::new(Instant::now() - OPTIMISTIC_INTERVAL),
        }
    }

    /// Build a new selector with a random seed (using system entropy).
    #[must_use]
    pub fn new(geo: LocalGeo) -> Self {
        let seed = rand::random::<u64>();
        Self::with_seed(seed, geo)
    }

    /// Mark a peer as forced-unchoked (will always be picked first).
    pub fn force_unchoke(&self, peer_key: impl Into<String>) {
        self.forced.lock().insert(peer_key.into());
    }

    /// Remove the forced-unchoke flag.
    pub fn unforce(&self, peer_key: &str) {
        self.forced.lock().remove(peer_key);
    }

    /// Run one unchoke round, returning the chosen peer keys + mode.
    ///
    /// `peers` is the snapshot of currently-online peers. The function
    /// returns at most `MAX_UNCHOKE` decisions, distributed across the three
    /// modes as follows:
    ///
    /// - All forced peers (up to MAX_UNCHOKE).
    /// - Remaining slots filled by Trading Allocation (top by score).
    /// - One slot (if remaining) goes to the Charity candidate.
    /// - Otherwise, one slot goes to Random optimistic.
    pub fn select(&self, peers: &[PeerMetrics]) -> Vec<UnchokeDecision> {
        let mut out: Vec<UnchokeDecision> = Vec::with_capacity(MAX_UNCHOKE);
        let forced = self.forced.lock().clone();
        // 1. Forced first.
        for p in peers.iter().filter(|p| forced.contains(&p.addr.to_string())) {
            if out.len() >= MAX_UNCHOKE {
                break;
            }
            out.push(UnchokeDecision {
                peer_key: p.addr.to_string(),
                mode: UnchokeMode::Forced,
            });
        }
        if out.len() >= MAX_UNCHOKE {
            return out;
        }

        // 2. Trading allocation — top-scoring online peers.
        let mut scored: Vec<(&PeerMetrics, i64)> = peers
            .iter()
            .filter(|p| p.status == PeerStatus::Online)
            .map(|p| (p, peer_score(p, &self.geo)))
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        let top_take = MAX_UNCHOKE.saturating_sub(out.len() + 1).max(0);
        for (p, _) in scored.iter().take(top_take) {
            let key = p.addr.to_string();
            if out.iter().any(|d| d.peer_key == key) {
                continue;
            }
            out.push(UnchokeDecision {
                peer_key: key,
                mode: UnchokeMode::Random,
            });
        }

        // 3. Optimistic / charity slot.
        if out.len() < MAX_UNCHOKE {
            // Charity candidates: low-score but interested (0 < progress < 0.99).
            let mut charity: Vec<&PeerMetrics> = peers
                .iter()
                .filter(|p| {
                    p.status == PeerStatus::Online
                        && (0.0..0.99).contains(&p.progress)
                        && p.bps_in < 50_000
                })
                .collect();
            charity.sort_by_key(|p| p.bps_in);
            let chosen = charity
                .first()
                .or_else(|| {
                    // Fallback: optimistic random.
                    let mut rng = self.rng.lock();
                    let interested: Vec<&PeerMetrics> = peers
                        .iter()
                        .filter(|p| p.status == PeerStatus::Online)
                        .collect();
                    interested.choose(&mut *rng).copied()
                });
            if let Some(p) = chosen {
                let key = p.addr.to_string();
                if !out.iter().any(|d| d.peer_key == key) {
                    let mode = if p.bps_in < 50_000 {
                        UnchokeMode::Charity
                    } else {
                        UnchokeMode::Random
                    };
                    out.push(UnchokeDecision { peer_key: key, mode });
                }
            }
        }
        out
    }

    /// Whether the optimistic slot should rotate (every 30 s).
    #[must_use]
    pub fn should_rotate_optimistic(&self) -> bool {
        let mut last = self.last_optimistic.lock();
        if last.elapsed() >= OPTIMISTIC_INTERVAL {
            *last = Instant::now();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bt::peer::{ConnProtocol, PeerFlags, PeerStatus};
    use std::net::{Ipv4Addr, SocketAddr};

    fn mk(bps_in: u64, progress: f64, addr_oct: u8) -> PeerMetrics {
        PeerMetrics {
            conn_protocol: ConnProtocol::TcpV4,
            is_incoming: false,
            addr: SocketAddr::from((Ipv4Addr::new(10, 0, 0, addr_oct), 6881)),
            client: "Tixati".into(),
            flags: PeerFlags::default(),
            geoip: Some("US".into()),
            source: PeerSource::Dht,
            bytes_in: 0,
            bytes_out: 0,
            progress,
            status: PeerStatus::Online,
            priority: 0,
            bps_in,
            bps_out: 0,
        }
    }

    #[test]
    fn forced_peers_always_picked_first() {
        let sel = UnchokeSelector::with_seed(42, LocalGeo::default());
        let p = mk(0, 0.5, 1);
        sel.force_unchoke(p.addr.to_string());
        let d = sel.select(&[p]);
        assert!(d.iter().any(|d| d.mode == UnchokeMode::Forced));
    }

    #[test]
    fn top_scored_peers_win_random_slots() {
        let sel = UnchokeSelector::with_seed(42, LocalGeo::default());
        let slow = mk(1_000, 0.5, 1);
        let fast = mk(1_000_000, 0.5, 2);
        let fast2 = mk(2_000_000, 0.5, 3);
        let fast3 = mk(3_000_000, 0.5, 4);
        let d = sel.select(&[slow, fast, fast2, fast3, mk(500, 0.5, 5)]);
        assert!(d.iter().any(|d| d.peer_key == fast3.addr.to_string()));
    }

    #[test]
    fn charity_picks_low_bps_peer() {
        let sel = UnchokeSelector::with_seed(42, LocalGeo::default());
        let top = mk(1_000_000, 0.5, 1);
        let top2 = mk(2_000_000, 0.5, 2);
        let top3 = mk(3_000_000, 0.5, 3);
        let weak = mk(100, 0.3, 4); // low bps_in → charity candidate
        let d = sel.select(&[top, top2, top3, weak]);
        assert!(d.iter().any(|d| d.mode == UnchokeMode::Charity && d.peer_key == weak.addr.to_string()));
    }

    #[test]
    fn rotates_every_30s() {
        let sel = UnchokeSelector::with_seed(42, LocalGeo::default());
        // Pre-seed last_optimistic to "now" so it shouldn't rotate.
        *sel.last_optimistic.lock() = Instant::now();
        assert!(!sel.should_rotate_optimistic());
        // Force the timestamp into the past.
        *sel.last_optimistic.lock() = Instant::now() - OPTIMISTIC_INTERVAL - Duration::from_secs(1);
        assert!(sel.should_rotate_optimistic());
    }
}
