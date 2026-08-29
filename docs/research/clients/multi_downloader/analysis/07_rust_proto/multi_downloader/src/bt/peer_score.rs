//! Peer scoring algorithm — Tixati analysis §5.3.
//!
//! Reimplements (with documentation) the inferred Tixati formula:
//!
//! ```text
//! score  := 0
//! score += bps_in              * SPEED_WEIGHT        //  ~40%
//! score += min(ratio, 4.0)     * RATIO_WEIGHT        //  ~15%
//! score += progress            * PROGRESS_WEIGHT     //  ~15%
//! score += UTP_BONUS  if uTP / I2P_BONUS if I2P
//! score *= source_trust_mult
//! score += CLIENT_BONUS if good client / -PENALTY if bad
//! score += GEO_BONUS if same region (used by Charity mode)
//! ```
//!
//! Constants below are tuned to match the relative importances given in
//! `tixati_architecture.md` §5.3; they are exposed as `pub const` so a real
//! backend (librqbit integration) can override them per-torrent.

use super::peer::{ConnProtocol, PeerMetrics, PeerSource};
use serde::{Deserialize, Serialize};

/// Weight applied to the per-second download speed component.
pub const SPEED_WEIGHT: f64 = 1.0;
/// Weight applied to the upload-fairness ratio component.
pub const RATIO_WEIGHT: f64 = 250.0;
/// Weight applied to the peer's piece-completion fraction.
pub const PROGRESS_WEIGHT: f64 = 100.0;
/// Bonus added when the peer is on uTP (BEP 29) — preferred for its
/// congestion-control friendliness.
pub const UTP_BONUS: i64 = 30;
/// Bonus added when the peer is on I2P — preferred in anonymity mode.
pub const I2P_BONUS: i64 = 50;
/// Bonus added when the peer has the same geoip region as the local node
/// (used by the Charity unchoke algorithm to favour nearby weak peers).
pub const GEO_BONUS: i64 = 5;
/// Bonus added when the peer identifies as a "known-good" client.
pub const CLIENT_GOOD_BONUS: i64 = 10;
/// Penalty subtracted when the peer identifies as a "known-buggy" client.
pub const CLIENT_BAD_PENALTY: i64 = 50;
/// Cap on the upload-fairness ratio component.
pub const RATIO_CAP: f64 = 4.0;

/// Source-trust multiplier applied multiplicatively.
#[must_use]
pub fn source_trust(s: PeerSource) -> f64 {
    match s {
        // Inbound already passed NAT — highest trust.
        PeerSource::Incoming => 1.5,
        // Local LAN — usually low-latency, give a small boost.
        PeerSource::Lsd => 1.3,
        // Peer Exchange — recommended by already-connected peer.
        PeerSource::Pex => 1.2,
        // DHT — neutral.
        PeerSource::Dht => 1.0,
        // Tracker — neutral.
        PeerSource::Tracker => 1.0,
        // Manually added — give a small trust boost.
        PeerSource::Manual => 1.1,
    }
}

/// Known-good client substrings (Tixati's `GOOD_CLIENTS`).
pub const GOOD_CLIENTS: &[&str] = &["libtorrent", "qBittorrent", "Transmission", "Tixati", "rqbit"];
/// Known-buggy client substrings (Tixati's `BAD_CLIENTS`).
pub const BAD_CLIENTS: &[&str] = &["BitLord", "XBT", "Old rasterbar"];

/// Client compatibility modifier.
///
/// Returns `+CLIENT_GOOD_BONUS` / `-CLIENT_BAD_PENALTY` / `0`.
#[must_use]
pub fn client_compat(client: &str) -> i64 {
    let lower = client.to_ascii_lowercase();
    if BAD_CLIENTS.iter().any(|b| lower.contains(&b.to_ascii_lowercase())) {
        return -CLIENT_BAD_PENALTY;
    }
    if GOOD_CLIENTS.iter().any(|g| lower.contains(&g.to_ascii_lowercase())) {
        return CLIENT_GOOD_BONUS;
    }
    0
}

/// Local-geoip context passed into [`peer_score`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LocalGeo {
    /// Local ISO country code, if known.
    pub country: Option<String>,
}

/// Compute the Tixati peer priority score.
///
/// Returns an `i64` so callers can compare directly without floating-point
/// surprises; underflows saturate to `i64::MIN`.
#[must_use]
pub fn peer_score(m: &PeerMetrics, local: &LocalGeo) -> i64 {
    let mut score: f64 = 0.0;

    // 1. Download speed (≈40%).
    score += (m.bps_in as f64) * SPEED_WEIGHT;

    // 2. Upload fairness ratio (≈15%).
    if m.bytes_out > 0 {
        let ratio = m.bytes_in as f64 / m.bytes_out as f64;
        score += ratio.min(RATIO_CAP) * RATIO_WEIGHT;
    }

    // 3. Progress (≈15%).
    score += m.progress.clamp(0.0, 1.0) * PROGRESS_WEIGHT;

    // 4. Protocol bonus (uTP > TCP; I2P highest in anonymity mode).
    score += match m.conn_protocol {
        ConnProtocol::UtpV4 | ConnProtocol::UtpV6 => UTP_BONUS as f64,
        ConnProtocol::I2p => I2P_BONUS as f64,
        _ => 0.0,
    } as f64;

    // 5. Source trust (multiplicative).
    score *= source_trust(m.source);

    // 6. Client compatibility (additive).
    score += client_compat(&m.client) as f64;

    // 7. Geographic bonus (for Charity mode).
    if let (Some(local_geo), Some(peer_geo)) = (&local.country, &m.geoip) {
        if local_geo.eq_ignore_ascii_case(peer_geo) {
            score += GEO_BONUS as f64;
        }
    }

    if score >= 0.0 {
        score.min(i64::MAX as f64) as i64
    } else {
        score.max(i64::MIN as f64) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bt::peer::{PeerFlags, PeerStatus};
    use std::net::{Ipv4Addr, SocketAddr};

    fn mk_peer(bps_in: u64, bytes_in: u64, bytes_out: u64, progress: f64) -> PeerMetrics {
        PeerMetrics {
            conn_protocol: ConnProtocol::UtpV4,
            is_incoming: false,
            addr: SocketAddr::from((Ipv4Addr::new(1, 2, 3, 4), 6881)),
            client: "Tixati/3.44".into(),
            flags: PeerFlags::default(),
            geoip: Some("US".into()),
            source: PeerSource::Dht,
            bytes_in,
            bytes_out,
            progress,
            status: PeerStatus::Fresh,
            priority: 0,
            bps_in,
            bps_out: 0,
        }
    }

    #[test]
    fn faster_peer_scores_higher() {
        let local = LocalGeo { country: Some("US".into()) };
        let slow = mk_peer(10_000, 100_000, 100_000, 0.5);
        let fast = mk_peer(1_000_000, 100_000, 100_000, 0.5);
        assert!(peer_score(&fast, &local) > peer_score(&slow, &local));
    }

    #[test]
    fn incoming_beats_dht_at_same_speed() {
        let local = LocalGeo::default();
        let mut dht = mk_peer(100, 100, 100, 0.5);
        dht.source = PeerSource::Dht;
        let mut inc = dht.clone();
        inc.source = PeerSource::Incoming;
        assert!(peer_score(&inc, &local) > peer_score(&dht, &local));
    }

    #[test]
    fn bad_client_penalised() {
        let local = LocalGeo::default();
        let mut p = mk_peer(1000, 100, 100, 0.5);
        p.client = "BitLord 1.0".into();
        let bad = peer_score(&p, &local);
        p.client = "qBittorrent 4.5".into();
        let good = peer_score(&p, &local);
        assert!(good > bad, "good={good}, bad={bad}");
    }

    #[test]
    fn ratio_caps_at_4() {
        let local = LocalGeo::default();
        let low_ratio = mk_peer(0, 100, 100, 0.0);
        let high_ratio = mk_peer(0, 10_000, 100, 0.0);
        let capped = mk_peer(0, 400, 100, 0.0); // ratio=4 (cap)
        assert!(peer_score(&high_ratio, &local) > peer_score(&low_ratio, &local));
        // Beyond the cap, the score stops growing.
        assert_eq!(peer_score(&high_ratio, &local), peer_score(&capped, &local));
    }

    #[test]
    fn geo_bonus_applied() {
        let mut p = mk_peer(0, 0, 0, 0.0);
        p.geoip = Some("US".into());
        let local = LocalGeo { country: Some("US".into()) };
        let local_diff = LocalGeo { country: Some("DE".into()) };
        assert!(peer_score(&p, &local) > peer_score(&p, &local_diff));
    }

    #[test]
    fn utp_beats_tcp_at_same_speed() {
        let local = LocalGeo::default();
        let mut tcp = mk_peer(1000, 100, 100, 0.5);
        tcp.conn_protocol = ConnProtocol::TcpV4;
        let mut utp = tcp.clone();
        utp.conn_protocol = ConnProtocol::UtpV4;
        assert!(peer_score(&utp, &local) > peer_score(&tcp, &local));
    }
}
