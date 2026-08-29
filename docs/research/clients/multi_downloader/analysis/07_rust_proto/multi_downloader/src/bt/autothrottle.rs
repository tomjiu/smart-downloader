//! AutoThrottle — Tixati's RTT-driven bandwidth control (analysis §6.2).
//!
//! Borrowed from LEDBAT (RFC 6817) but with Tixati's twist: actively probe RTT
//! to peers, keep a baseline (min over N minutes), and adjust the global
//! outgoing limit so that `queueing_delay = current_rtt - baseline_rtt` stays
//! near `target_rtt`.
//!
//! Algorithm (Tixati §6.2):
//!
//! ```text
//! current_rtt   := measure()                     # via keep-alive ping
//! baseline_rtt  := min(rtt_history.last(N minutes))
//! queueing      := current_rtt - baseline_rtt
//! if queueing > target_rtt * 0.8:
//!     new_rate = current_rate * 0.9
//! elif queueing < target_rtt * 0.2:
//!     new_rate = min(current_rate * 1.05, max_rate)
//! else:
//!     new_rate = current_rate
//! ```

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// Default target RTT (Tixati = 100 ms).
pub const DEFAULT_TARGET_RTT: Duration = Duration::from_millis(100);

/// Window over which baseline RTT is computed (Tixati = 5 minutes).
pub const BASELINE_WINDOW: Duration = Duration::from_secs(5 * 60);

/// Multiplier when reducing the rate (analysis §6.2).
pub const DECREASE_FACTOR: f64 = 0.9;
/// Multiplier when increasing the rate.
pub const INCREASE_FACTOR: f64 = 1.05;
/// Fraction of `target_rtt` above which we throttle.
pub const QUEUE_HIGH_THRESHOLD: f64 = 0.8;
/// Fraction of `target_rtt` below which we accelerate.
pub const QUEUE_LOW_THRESHOLD: f64 = 0.2;

/// A single RTT sample.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RttSample {
    /// When the sample was taken.
    pub at: Instant,
    /// The measured RTT.
    pub rtt: Duration,
}

/// Tunable parameters.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AutoThrottleConfig {
    pub target_rtt: Duration,
    pub min_bps: u64,
    pub max_bps: u64,
    pub baseline_window: Duration,
}

impl Default for AutoThrottleConfig {
    fn default() -> Self {
        Self {
            target_rtt: DEFAULT_TARGET_RTT,
            min_bps: 64 * 1024,
            max_bps: 100 * 1024 * 1024,
            baseline_window: BASELINE_WINDOW,
        }
    }
}

/// The AutoThrottle state machine.
pub struct AutoThrottle {
    cfg: AutoThrottleConfig,
    samples: Mutex<VecDeque<RttSample>>,
    current_rate: Mutex<u64>,
}

impl AutoThrottle {
    /// Build a new throttle from a config + initial rate.
    #[must_use]
    pub fn new(cfg: AutoThrottleConfig, initial_rate_bps: u64) -> Self {
        Self {
            cfg,
            samples: Mutex::new(VecDeque::with_capacity(256)),
            current_rate: Mutex::new(initial_rate_bps.clamp(cfg.min_bps, cfg.max_bps)),
        }
    }

    /// Current rate (bytes/sec).
    #[must_use]
    pub fn current_rate(&self) -> u64 {
        *self.current_rate.lock()
    }

    /// Push a freshly-measured RTT sample.
    pub fn record_sample(&self, rtt: Duration) {
        let now = Instant::now();
        let mut s = self.samples.lock();
        s.push_back(RttSample { at: now, rtt });
        // Trim to baseline window.
        let cutoff = now - self.cfg.baseline_window;
        while let Some(front) = s.front() {
            if front.at < cutoff {
                s.pop_front();
            } else {
                break;
            }
        }
    }

    /// Compute the baseline (min) RTT over the configured window.
    #[must_use]
    pub fn baseline_rtt(&self) -> Option<Duration> {
        let s = self.samples.lock();
        s.iter().map(|x| x.rtt).min()
    }

    /// Compute the most recent RTT sample.
    #[must_use]
    pub fn current_rtt(&self) -> Option<Duration> {
        let s = self.samples.lock();
        s.back().map(|x| x.rtt)
    }

    /// One control-loop step. Returns the new rate.
    ///
    /// Caller is expected to invoke this every 100 ms or so (Tixati default).
    #[must_use]
    pub fn step(&self) -> u64 {
        let (Some(current), Some(baseline)) = (self.current_rtt(), self.baseline_rtt()) else {
            return self.current_rate();
        };
        let target_ms = self.cfg.target_rtt.as_secs_f64() * 1000.0;
        let current_ms = current.as_secs_f64() * 1000.0;
        let baseline_ms = baseline.as_secs_f64() * 1000.0;
        let queueing = current_ms - baseline_ms;
        let mut rate = self.current_rate.lock();
        let new = if queueing > target_ms * QUEUE_HIGH_THRESHOLD {
            (*rate as f64 * DECREASE_FACTOR) as u64
        } else if queueing < target_ms * QUEUE_LOW_THRESHOLD {
            (((*rate) as f64) * INCREASE_FACTOR) as u64
        } else {
            *rate
        };
        *rate = new.clamp(self.cfg.min_bps, self.cfg.max_bps);
        tracing::debug!(
            rtt_current_ms = current_ms,
            rtt_baseline_ms = baseline_ms,
            queueing_ms = queueing,
            rate_bps = *rate,
            "autothrottle step"
        );
        *rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_is_minimum_over_window() {
        let at = AutoThrottle::new(AutoThrottleConfig::default(), 1_000_000);
        at.record_sample(Duration::from_millis(120));
        at.record_sample(Duration::from_millis(80));
        at.record_sample(Duration::from_millis(110));
        assert_eq!(at.baseline_rtt().unwrap(), Duration::from_millis(80));
        assert_eq!(at.current_rtt().unwrap(), Duration::from_millis(110));
    }

    #[test]
    fn high_queueing_decreases_rate() {
        let at = AutoThrottle::new(AutoThrottleConfig::default(), 1_000_000);
        // Baseline 30 ms, current 200 ms → queueing 170 ms > 80 ms.
        at.record_sample(Duration::from_millis(30));
        at.record_sample(Duration::from_millis(200));
        let new_rate = at.step();
        assert!(new_rate < 1_000_000, "expected decrease, got {new_rate}");
    }

    #[test]
    fn low_queueing_increases_rate() {
        let at = AutoThrottle::new(AutoThrottleConfig::default(), 1_000_000);
        at.record_sample(Duration::from_millis(100));
        at.record_sample(Duration::from_millis(101)); // queueing = 1 ms, < 20 ms
        let new_rate = at.step();
        assert!(new_rate > 1_000_000, "expected increase, got {new_rate}");
    }

    #[test]
    fn steady_queue_holds_rate() {
        let at = AutoThrottle::new(AutoThrottleConfig::default(), 1_000_000);
        at.record_sample(Duration::from_millis(100));
        at.record_sample(Duration::from_millis(110)); // queueing = 10 ms, in [20, 80]
        let new_rate = at.step();
        assert_eq!(new_rate, 1_000_000);
    }

    #[test]
    fn rate_clamped_to_min_max() {
        let cfg = AutoThrottleConfig {
            target_rtt: Duration::from_millis(100),
            min_bps: 500_000,
            max_bps: 2_000_000,
            baseline_window: Duration::from_secs(60),
        };
        let at = AutoThrottle::new(cfg, 500_000); // floor
        at.record_sample(Duration::from_millis(30));
        at.record_sample(Duration::from_millis(200)); // high queueing
        let new_rate = at.step();
        assert_eq!(new_rate, 500_000, "should not fall below floor");
    }
}
