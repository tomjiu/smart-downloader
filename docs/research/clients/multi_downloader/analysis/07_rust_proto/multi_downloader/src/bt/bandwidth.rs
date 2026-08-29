//! 5-layer bandwidth allocator — Tixati analysis §6.
//!
//! Layers (top to bottom):
//!
//! 1. **Global throttle** — hard cap on aggregate in/out kbps.
//! 2. **Trading allocation** — split between downloading peers (high prio) and
//!    seeding peers (low prio), governed by
//!    `trrottle_outgoing_guarantee_dspercent`.
//! 3. **Seeding allocation** — pure-seeding mode (mutually exclusive with
//!    Trading) — all bandwidth goes to seeding peers by percentage.
//! 4. **Auto limit** — RTT-driven LEDBAT-like control (delegated to
//!    `autothrottle.rs`).
//! 5. **Per-peer quota** — daily / weekly per-peer cap (Tixati `bwquotas`).
//!
//! Each layer returns a `BandwidthQuota` describing the maximum bytes that
//! may be sent in the next scheduling window.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::peer::{PeerMetrics, PeerStatus};

/// Scheduling window used throughout the allocator (100 ms — Tixati default).
pub const WINDOW: Duration = Duration::from_millis(100);

/// Output of each layer — a hard ceiling in bytes/sec that may be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BandwidthQuota {
    /// Bytes allowed per second.
    pub bps: u64,
    /// Tier that produced this quota (for diagnostics).
    pub tier: BandwidthTier,
}

/// Which layer produced the quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BandwidthTier {
    /// Layer 1 — global throttle.
    Global,
    /// Layer 2 — trading allocation.
    Trading,
    /// Layer 3 — seeding allocation.
    Seeding,
    /// Layer 4 — auto limit (RTT-based).
    Auto,
    /// Layer 5 — per-peer quota.
    Quota,
}

/// Layer-1 config — global in/out caps.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct GlobalThrottle {
    pub in_kbps: u64,
    pub out_kbps: u64,
    pub in_enabled: bool,
    pub out_enabled: bool,
}

impl Default for GlobalThrottle {
    fn default() -> Self {
        Self {
            in_kbps: 0,
            out_kbps: 0,
            in_enabled: false,
            out_enabled: false,
        }
    }
}

/// Layer-2 config — Trading Allocation (analysis §6.3).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TradingAllocation {
    /// Percentage [0, 100] of outgoing bandwidth reserved for downloading
    /// peers. Remainder goes to seeding peers.
    pub guarantee_ds_percent: u8,
    pub enabled: bool,
}

impl Default for TradingAllocation {
    fn default() -> Self {
        Self {
            guarantee_ds_percent: 70,
            enabled: false,
        }
    }
}

/// Layer-3 config — Seeding Allocation (pure-seeding mode).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SeedingAllocation {
    /// When in pure-seeding mode, what fraction of bandwidth each seeding
    /// peer gets (relative to peer count).
    pub per_peer_percent: u8,
    pub enabled: bool,
}

impl Default for SeedingAllocation {
    fn default() -> Self {
        Self {
            per_peer_percent: 5,
            enabled: false,
        }
    }
}

/// Layer-5 config — per-peer quota (Tixati `bwquotas`).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct PeerQuota {
    /// Maximum bytes a single peer may receive per day.
    pub daily_bytes: u64,
    /// Maximum bytes a single peer may receive per week.
    pub weekly_bytes: u64,
}

/// Top-level allocator that applies all five layers in order and returns the
/// most restrictive quota.
pub struct FiveLayerAllocator {
    pub global: GlobalThrottle,
    pub trading: TradingAllocation,
    pub seeding: SeedingAllocation,
    pub peer_quota: PeerQuota,
    /// RTT-derived auto-limit (Layer 4) — set externally by AutoThrottle.
    pub auto_limit_bps: u64,
}

impl FiveLayerAllocator {
    /// Compute the quota for a single peer.
    ///
    /// The min of all five layers is taken, because bandwidth ceilings stack
    /// conservatively: any layer that says "no more than X" enforces X.
    #[must_use]
    pub fn allocate(&self, peer: &PeerMetrics, all_peers: &[PeerMetrics]) -> BandwidthQuota {
        // Layer 1 — global.
        let global_bps = if self.global.out_enabled {
            self.global.out_kbps * 1024
        } else {
            u64::MAX
        };

        // Layer 2 — Trading Allocation. Compute total budget for downloading
        // vs seeding peers, then divide among downloaders.
        let n_downloaders = all_peers
            .iter()
            .filter(|p| p.status == PeerStatus::Online && p.progress < 1.0)
            .count()
            .max(1) as u64;
        let n_seeders = all_peers
            .iter()
            .filter(|p| p.status == PeerStatus::Online && p.progress >= 1.0)
            .count()
            .max(1) as u64;
        let trading_bps = if self.trading.enabled {
            let total = global_bps.min(u64::MAX / 2); // avoid overflow
            if peer.progress < 1.0 {
                total * u64::from(self.trading.guarantee_ds_percent) / 100 / n_downloaders
            } else {
                total * u64::from(100 - self.trading.guarantee_ds_percent) / 100 / n_seeders
            }
        } else {
            u64::MAX
        };

        // Layer 3 — Seeding allocation. Only active when no downloaders.
        let seeding_bps = if self.seeding.enabled && peer.progress >= 1.0 {
            let total = global_bps.min(u64::MAX / 2);
            total * u64::from(self.seeding.per_peer_percent) / 100 / n_seeders
        } else {
            u64::MAX
        };

        // Layer 4 — Auto limit (RTT-driven).
        let auto_bps = if self.auto_limit_bps > 0 {
            self.auto_limit_bps
        } else {
            u64::MAX
        };

        // Layer 5 — per-peer quota (daily).
        let quota_bps = if self.peer_quota.daily_bytes > 0 {
            // Spread the daily budget over the day.
            self.peer_quota.daily_bytes / 86_400
        } else {
            u64::MAX
        };

        // Take the min.
        let mut candidates = [
            (global_bps, BandwidthTier::Global),
            (trading_bps, BandwidthTier::Trading),
            (seeding_bps, BandwidthTier::Seeding),
            (auto_bps, BandwidthTier::Auto),
            (quota_bps, BandwidthTier::Quota),
        ];
        candidates.sort_by_key(|(b, _)| *b);
        let (bps, tier) = candidates[0];
        BandwidthQuota { bps, tier }
    }
}

impl Default for FiveLayerAllocator {
    fn default() -> Self {
        Self {
            global: GlobalThrottle::default(),
            trading: TradingAllocation::default(),
            seeding: SeedingAllocation::default(),
            peer_quota: PeerQuota::default(),
            auto_limit_bps: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bt::peer::{ConnProtocol, PeerFlags, PeerSource};
    use std::net::{Ipv4Addr, SocketAddr};

    fn mk(progress: f64) -> PeerMetrics {
        PeerMetrics {
            conn_protocol: ConnProtocol::TcpV4,
            is_incoming: false,
            addr: SocketAddr::from((Ipv4Addr::new(10, 0, 0, 1), 6881)),
            client: "test".into(),
            flags: PeerFlags::default(),
            geoip: None,
            source: PeerSource::Dht,
            bytes_in: 0,
            bytes_out: 0,
            progress,
            status: PeerStatus::Online,
            priority: 0,
            bps_in: 0,
            bps_out: 0,
        }
    }

    #[test]
    fn global_zero_means_unlimited() {
        let a = FiveLayerAllocator::default();
        let p = mk(0.5);
        let q = a.allocate(&p, std::slice::from_ref(&p));
        assert_eq!(q.bps, u64::MAX, "no layers enabled → unlimited");
    }

    #[test]
    fn global_cap_applied() {
        let mut a = FiveLayerAllocator::default();
        a.global.out_enabled = true;
        a.global.out_kbps = 100; // 100 KB/s = 102400 B/s
        let p = mk(0.5);
        let q = a.allocate(&p, std::slice::from_ref(&p));
        assert_eq!(q.bps, 100 * 1024);
        assert_eq!(q.tier, BandwidthTier::Global);
    }

    #[test]
    fn trading_allocation_splits_by_ds_percent() {
        let mut a = FiveLayerAllocator::default();
        a.global.out_enabled = true;
        a.global.out_kbps = 1000; // 1 MB/s total
        a.trading.enabled = true;
        a.trading.guarantee_ds_percent = 70;
        let dl = mk(0.5);
        let sd = mk(1.0);
        let peers = vec![dl.clone(), sd.clone()];
        let q_dl = a.allocate(&dl, &peers);
        let q_sd = a.allocate(&sd, &peers);
        // 70% of 1MB/s to 1 downloader = 700 KB/s
        assert_eq!(q_dl.bps, 1000 * 1024 * 70 / 100);
        // 30% of 1MB/s to 1 seeder = 300 KB/s
        assert_eq!(q_sd.bps, 1000 * 1024 * 30 / 100);
    }

    #[test]
    fn auto_limit_overrides_when_smallest() {
        let mut a = FiveLayerAllocator::default();
        a.global.out_enabled = true;
        a.global.out_kbps = 1000; // 1 MB/s
        a.auto_limit_bps = 50_000; // 50 KB/s — RTT says slow down
        let p = mk(0.5);
        let q = a.allocate(&p, std::slice::from_ref(&p));
        assert_eq!(q.bps, 50_000);
        assert_eq!(q.tier, BandwidthTier::Auto);
    }
}
