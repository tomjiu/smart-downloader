//! Peer connection lifecycle state machine — Tixati 11-stage model
//! (analysis §7.2).
//!
//! Stages:
//!  0. `PeerDiscovery`
//!  1. `ConnectionInitiation`
//!  2. `TcpUtpConnect`
//!  3. `MsePeHandshake`
//!  4. `BtHandshake`
//!  5. `ExtensionHandshake`
//!  6. `BitfieldExchange`
//!  7. `InterestNegotiation`
//!  8. `DataTransfer`
//!  9. `KeepAlive`
//! 10. `Disconnection`
//! 11. `BanOrRetry`
//!
//! Each transition is guarded by a small set of allowed `(from, to)` pairs
//! (see [`ConnectionState::can_transition_to`]) so the FSM is fully
//! inspectable in logs and unit tests.

use std::fmt;
use std::time::Instant;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::peer::PeerSource;

/// All 12 observable states (analysis §7.2 — note the off-by-one: Tixati's
/// docs label stages 0–11, which is 12 stages).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConnectionState {
    /// Stage 0.
    PeerDiscovery,
    /// Stage 1.
    ConnectionInitiation,
    /// Stage 2 — TCP/uTP connect.
    TcpUtpConnect,
    /// Stage 3 — MSE/PE DH key exchange + cipher negotiation.
    MsePeHandshake,
    /// Stage 4 — `"\x13BitTorrent protocol"` + info_hash + peer_id.
    BtHandshake,
    /// Stage 5 — BEP 10 extended handshake.
    ExtensionHandshake,
    /// Stage 6 — bitfield exchange.
    BitfieldExchange,
    /// Stage 7 — interest negotiation.
    InterestNegotiation,
    /// Stage 8 — request/piece transfer.
    DataTransfer,
    /// Stage 9 — keep-alive monitor.
    KeepAlive,
    /// Stage 10 — disconnection (any trigger).
    Disconnection,
    /// Stage 11 — ban / schedule retry.
    BanOrRetry,
}

impl ConnectionState {
    /// All valid transitions (encoded as a match — O(1)).
    #[must_use]
    pub fn can_transition_to(self, next: ConnectionState) -> bool {
        use ConnectionState::*;
        matches!(
            (self, next),
            (PeerDiscovery, ConnectionInitiation)
                | (ConnectionInitiation, TcpUtpConnect)
                | (ConnectionInitiation, Disconnection)
                | (TcpUtpConnect, MsePeHandshake)
                | (TcpUtpConnect, BtHandshake)        // unencrypted fallback
                | (TcpUtpConnect, Disconnection)
                | (MsePeHandshake, BtHandshake)
                | (MsePeHandshake, Disconnection)
                | (BtHandshake, ExtensionHandshake)
                | (BtHandshake, Disconnection)
                | (ExtensionHandshake, BitfieldExchange)
                | (ExtensionHandshake, Disconnection)
                | (BitfieldExchange, InterestNegotiation)
                | (BitfieldExchange, Disconnection)
                | (InterestNegotiation, DataTransfer)
                | (InterestNegotiation, KeepAlive)
                | (InterestNegotiation, Disconnection)
                | (DataTransfer, KeepAlive)
                | (DataTransfer, Disconnection)
                | (KeepAlive, DataTransfer)
                | (KeepAlive, Disconnection)
                | (Disconnection, BanOrRetry)
                | (Disconnection, PeerDiscovery)         // retry
                | (BanOrRetry, PeerDiscovery)           // ban expires
        )
    }
}

impl fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Transition record (audit log entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionTransition {
    pub from: ConnectionState,
    pub to: ConnectionState,
    pub ts: Instant,
    pub note: Option<String>,
}

/// Per-peer FSM instance.
pub struct ConnectionStateMachine {
    state: Mutex<ConnectionState>,
    history: Mutex<Vec<ConnectionTransition>>,
    source: PeerSource,
}

impl ConnectionStateMachine {
    /// Construct a new FSM starting at `PeerDiscovery`.
    #[must_use]
    pub fn new(source: PeerSource) -> Self {
        Self {
            state: Mutex::new(ConnectionState::PeerDiscovery),
            history: Mutex::new(Vec::new()),
            source,
        }
    }

    /// Current state.
    #[must_use]
    pub fn state(&self) -> ConnectionState {
        *self.state.lock()
    }

    /// Peer discovery source.
    #[must_use]
    pub fn source(&self) -> PeerSource {
        self.source
    }

    /// Attempt to transition; returns `Err` with the previous state on
    /// illegal transitions.
    pub fn transition(
        &self,
        next: ConnectionState,
        note: Option<String>,
    ) -> Result<(), (ConnectionState, ConnectionState)> {
        let mut s = self.state.lock();
        let cur = *s;
        if !cur.can_transition_to(next) {
            return Err((cur, next));
        }
        let t = ConnectionTransition {
            from: cur,
            to: next,
            ts: Instant::now(),
            note: note.clone(),
        };
        tracing::info!(from = ?cur, to = ?next, note = ?note, "conn state transition");
        self.history.lock().push(t);
        *s = next;
        Ok(())
    }

    /// Number of transitions recorded.
    #[must_use]
    pub fn transitions(&self) -> usize {
        self.history.lock().len()
    }

    /// True if currently in a "data-bearing" state.
    #[must_use]
    pub fn is_data_state(&self) -> bool {
        matches!(self.state(), ConnectionState::DataTransfer | ConnectionState::KeepAlive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_progresses_to_data_transfer() {
        let sm = ConnectionStateMachine::new(PeerSource::Dht);
        sm.transition(ConnectionState::ConnectionInitiation, None).unwrap();
        sm.transition(ConnectionState::TcpUtpConnect, None).unwrap();
        sm.transition(ConnectionState::MsePeHandshake, None).unwrap();
        sm.transition(ConnectionState::BtHandshake, None).unwrap();
        sm.transition(ConnectionState::ExtensionHandshake, None).unwrap();
        sm.transition(ConnectionState::BitfieldExchange, None).unwrap();
        sm.transition(ConnectionState::InterestNegotiation, None).unwrap();
        sm.transition(ConnectionState::DataTransfer, None).unwrap();
        assert!(sm.is_data_state());
    }

    #[test]
    fn illegal_transition_rejected() {
        let sm = ConnectionStateMachine::new(PeerSource::Dht);
        let err = sm.transition(ConnectionState::DataTransfer, None).unwrap_err();
        assert_eq!(err.0, ConnectionState::PeerDiscovery);
        assert_eq!(err.1, ConnectionState::DataTransfer);
    }

    #[test]
    fn unencrypted_fallback_allowed() {
        let sm = ConnectionStateMachine::new(PeerSource::Dht);
        sm.transition(ConnectionState::ConnectionInitiation, None).unwrap();
        sm.transition(ConnectionState::TcpUtpConnect, None).unwrap();
        // Direct TcpUtpConnect → BtHandshake (skip MSE) is allowed.
        sm.transition(ConnectionState::BtHandshake, Some("plaintext".into())).unwrap();
        assert_eq!(sm.state(), ConnectionState::BtHandshake);
    }

    #[test]
    fn disconnect_always_allowed_from_data_transfer() {
        let sm = ConnectionStateMachine::new(PeerSource::Dht);
        sm.transition(ConnectionState::ConnectionInitiation, None).unwrap();
        sm.transition(ConnectionState::TcpUtpConnect, None).unwrap();
        sm.transition(ConnectionState::BtHandshake, None).unwrap();
        sm.transition(ConnectionState::ExtensionHandshake, None).unwrap();
        sm.transition(ConnectionState::BitfieldExchange, None).unwrap();
        sm.transition(ConnectionState::InterestNegotiation, None).unwrap();
        sm.transition(ConnectionState::DataTransfer, None).unwrap();
        sm.transition(ConnectionState::Disconnection, Some("peer gone".into())).unwrap();
        sm.transition(ConnectionState::BanOrRetry, None).unwrap();
        assert_eq!(sm.state(), ConnectionState::BanOrRetry);
    }
}
