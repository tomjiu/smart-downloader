//! BitTorrent sub-system.
//!
//! While the BT protocol itself is **not implemented** in this prototype (see
//! `engine::bt_engine` for the placeholder), the policy / algorithm layer is
//! fully fleshed out — these are the lessons learned from Tixati
//! (`analysis/04_tixati/tixati_architecture.md`):
//!
//! - `peer` — 14-field `PeerMetrics` data structure (analysis §5.1).
//! - `peer_score` — Tixati scoring algorithm (analysis §5.3).
//! - `unchoke` — 3-mode unchoke: Forced / Random / Charity (analysis §5.4).
//! - `bandwidth` — 5-layer bandwidth model (analysis §6).
//! - `autothrottle` — RTT-based LEDBAT-like throttling (analysis §6.2).
//! - `connection` — 11-stage connection lifecycle state machine (analysis §7).

pub mod autothrottle;
pub mod bandwidth;
pub mod connection;
pub mod peer;
pub mod peer_score;
pub mod unchoke;

pub use autothrottle::AutoThrottle;
pub use bandwidth::{BandwidthQuota, BandwidthTier, FiveLayerAllocator};
pub use connection::{ConnectionState, ConnectionStateMachine};
pub use peer::{PeerMetrics, PeerSource};
pub use peer_score::peer_score;
pub use unchoke::{UnchokeDecision, UnchokeMode, UnchokeSelector};
