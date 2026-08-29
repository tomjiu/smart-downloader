//! Peer data structure with 14 fields — mirrors Tixati's `col_peers_*` UI
//! columns verbatim (analysis §5.1).

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// How a peer was discovered (analysis §5.1 / col_peers_src).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PeerSource {
    /// Inbound connection (already passed NAT).
    Incoming,
    /// Local Service Discovery (BEP 14).
    Lsd,
    /// Peer Exchange (BEP 11 ut_pex).
    Pex,
    /// Distributed Hash Table (BEP 5).
    Dht,
    /// Tracker announce.
    Tracker,
    /// Manual add.
    Manual,
}

/// Network protocol used for the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConnProtocol {
    /// IPv4 TCP.
    TcpV4,
    /// IPv6 TCP.
    TcpV6,
    /// uTP over IPv4 (BEP 29).
    UtpV4,
    /// uTP over IPv6.
    UtpV6,
    /// I2P transport (Tixati's anonymity mode).
    I2p,
}

/// Peer status (analysis §5.2 — 7-state machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PeerStatus {
    /// Newly discovered.
    Fresh,
    /// TCP/uTP handshake in progress.
    Connecting,
    /// Online, exchanging data.
    Online,
    /// Online + bitfield all 1's.
    OnlineComplete,
    /// Offline (timeout / disconnect).
    Offline,
    /// Offline + was complete.
    OfflineComplete,
    /// Banned / manually blocked.
    Ignored,
}

/// The 14-field Peer data structure.
///
/// Field names mirror Tixati's `col_peers_*` UI labels one-for-one so the
/// analysis is greppable from source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerMetrics {
    /// `col_peers_conn` — connection type (incoming/outgoing + protocol).
    pub conn_protocol: ConnProtocol,
    /// True if this is an inbound connection.
    pub is_incoming: bool,
    /// Remote address.
    pub addr: SocketAddr,
    /// `col_peers_protocol` — client identification string.
    pub client: String,
    /// `col_peers_flag` — D/S/U/E/K bitset (download/seed/upload/encryption/keepalive).
    pub flags: PeerFlags,
    /// `col_peers_location` — ISO country code (for Charity geographic pref).
    pub geoip: Option<String>,
    /// `col_peers_src` — discovery source.
    pub source: PeerSource,
    /// `col_peers_bytesin` — total bytes received from peer.
    pub bytes_in: u64,
    /// `col_peers_bytesout` — total bytes sent to peer.
    pub bytes_out: u64,
    /// `col_peers_progress` — peer's piece-completion fraction in `[0.0, 1.0]`.
    pub progress: f64,
    /// `col_peers_status` — status-machine state.
    pub status: PeerStatus,
    /// `col_peers_priority` — last computed priority score.
    pub priority: i64,
    /// `col_peers_bpsin` — current download rate (bytes/sec, EMA).
    pub bps_in: u64,
    /// `col_peers_bpsout` — current upload rate (bytes/sec, EMA).
    pub bps_out: u64,
}

/// Bit-flag set (Tixati `col_peers_flag`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerFlags(pub u8);

impl PeerFlags {
    /// Download flag — peer is sending data.
    pub const D: u8 = 1 << 0;
    /// Seed flag — peer has all pieces.
    pub const S: u8 = 1 << 1;
    /// Upload flag — we are sending data.
    pub const U: u8 = 1 << 2;
    /// Encryption flag — MSE/PE encryption active.
    pub const E: u8 = 1 << 3;
    /// Keep-alive flag — recent keep-alive received.
    pub const K: u8 = 1 << 4;
    /// Has any of the given mask bits set?
    #[must_use]
    pub fn has(self, mask: u8) -> bool {
        (self.0 & mask) != 0
    }
    /// Set a flag.
    pub fn set(&mut self, mask: u8) {
        self.0 |= mask;
    }
    /// Clear a flag.
    pub fn clear(&mut self, mask: u8) {
        self.0 &= !mask;
    }
}

impl Default for PeerStatus {
    fn default() -> Self {
        Self::Fresh
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn mk_peer() -> PeerMetrics {
        PeerMetrics {
            conn_protocol: ConnProtocol::UtpV4,
            is_incoming: false,
            addr: SocketAddr::from((Ipv4Addr::new(1, 2, 3, 4), 6881)),
            client: "Tixati/3.44".into(),
            flags: PeerFlags::default(),
            geoip: Some("US".into()),
            source: PeerSource::Dht,
            bytes_in: 0,
            bytes_out: 0,
            progress: 0.5,
            status: PeerStatus::Fresh,
            priority: 0,
            bps_in: 0,
            bps_out: 0,
        }
    }

    #[test]
    fn peer_has_14_fields() {
        let p = mk_peer();
        // Trivial sanity check that we can access all 14 fields by name.
        let _ = (
            p.conn_protocol, p.is_incoming, p.addr, p.client, p.flags, p.geoip,
            p.source, p.bytes_in, p.bytes_out, p.progress, p.status, p.priority,
            p.bps_in, p.bps_out,
        );
    }

    #[test]
    fn peer_flags_set_clear() {
        let mut f = PeerFlags::default();
        assert!(!f.has(PeerFlags::D));
        f.set(PeerFlags::D);
        assert!(f.has(PeerFlags::D));
        f.clear(PeerFlags::D);
        assert!(!f.has(PeerFlags::D));
    }
}
