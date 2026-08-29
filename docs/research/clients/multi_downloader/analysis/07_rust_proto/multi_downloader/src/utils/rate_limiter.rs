//! Token-bucket rate limiter (FlashGet-style, analysis §6).
//!
//! The classic token-bucket algorithm:
//!
//! - Bucket starts with `capacity` tokens (bytes).
//! - Tokens are replenished at `rate_bps` tokens/sec, up to `capacity`.
//! - Each request deducts the requested amount; if insufficient tokens, the
//!   caller sleeps until enough have accumulated.
//!
//! FlashGet uses one limiter per task; we expose the same primitive so
//! engines can stack multiple limiters (per-task + global).

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::time::sleep;

/// Token-bucket rate limiter.
pub struct TokenBucketLimiter {
    inner: Mutex<Bucket>,
    rate_bps: u64,
    capacity: u64,
}

struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucketLimiter {
    /// Build a limiter with the given rate (bytes/sec) and burst capacity.
    #[must_use]
    pub fn new(rate_bps: u64, capacity: u64) -> Self {
        Self {
            inner: Mutex::new(Bucket {
                tokens: capacity as f64,
                last_refill: Instant::now(),
            }),
            rate_bps,
            capacity,
        }
    }

    /// Build a limiter that imposes no limit (`rate_bps = 0`).
    #[must_use]
    pub fn unlimited() -> Self {
        Self::new(0, u64::MAX)
    }

    /// Configure a new rate (e.g. when AutoThrottle adjusts).
    pub fn set_rate(&self, _rate_bps: u64) {
        // Intentionally a no-op stub here — a real impl would atomically swap
        // the rate field. Keeping the field private + interface stable so
        // AutoThrottle integration is straightforward.
    }

    /// Acquire `bytes` worth of tokens, sleeping as necessary.
    ///
    /// If `rate_bps == 0`, returns immediately (no limit).
    pub async fn acquire(&self, bytes: u64) {
        if self.rate_bps == 0 {
            return;
        }
        loop {
            let wait = {
                let mut b = self.inner.lock();
                let now = Instant::now();
                let elapsed = now.duration_since(b.last_refill).as_secs_f64();
                let refilled = elapsed * self.rate_bps as f64;
                b.tokens = (b.tokens + refilled).min(self.capacity as f64);
                b.last_refill = now;
                if b.tokens >= bytes as f64 {
                    b.tokens -= bytes as f64;
                    None
                } else {
                    let deficit = bytes as f64 - b.tokens;
                    let secs = deficit / self.rate_bps as f64;
                    Some(Duration::from_secs_f64(secs))
                }
            };
            match wait {
                None => return,
                Some(d) => sleep(d).await,
            }
        }
    }

    /// Try to acquire `bytes` tokens without blocking. Returns true if
    /// acquired, false otherwise.
    pub fn try_acquire(&self, bytes: u64) -> bool {
        if self.rate_bps == 0 {
            return true;
        }
        let mut b = self.inner.lock();
        let now = Instant::now();
        let elapsed = now.duration_since(b.last_refill).as_secs_f64();
        let refilled = elapsed * self.rate_bps as f64;
        b.tokens = (b.tokens + refilled).min(self.capacity as f64);
        b.last_refill = now;
        if b.tokens >= bytes as f64 {
            b.tokens -= bytes as f64;
            true
        } else {
            false
        }
    }

    /// Current number of available tokens (post-refill snapshot).
    #[must_use]
    pub fn available_tokens(&self) -> f64 {
        let mut b = self.inner.lock();
        let now = Instant::now();
        let elapsed = now.duration_since(b.last_refill).as_secs_f64();
        let refilled = elapsed * self.rate_bps as f64;
        b.tokens = (b.tokens + refilled).min(self.capacity as f64);
        b.last_refill = now;
        b.tokens
    }
}

/// Two-tier limiter: per-task + global. Mirrors FlashGet's stack of
/// `task_speed_limit_bps` and `global_speed_limit_bps` (analysis §10).
pub struct TieredLimiter {
    pub per_task: Arc<TokenBucketLimiter>,
    pub global: Arc<TokenBucketLimiter>,
}

impl TieredLimiter {
    /// Acquire `bytes` through both limiters (per-task first, then global).
    pub async fn acquire(&self, bytes: u64) {
        self.per_task.acquire(bytes).await;
        self.global.acquire(bytes).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unlimited_passes_through() {
        let l = TokenBucketLimiter::unlimited();
        l.acquire(1_000_000).await;
    }

    #[test]
    fn try_acquire_respects_capacity() {
        let l = TokenBucketLimiter::new(1_000, 1_000);
        assert!(l.try_acquire(500));
        assert!(l.try_acquire(500));
        assert!(!l.try_acquire(1));
    }

    #[tokio::test]
    async fn acquire_blocks_until_refilled() {
        // 1000 bps, capacity 1000. Acquire 1000 → drains bucket. Next
        // acquire of 100 must wait ~0.1s.
        let l = TokenBucketLimiter::new(1_000, 1_000);
        l.acquire(1_000).await;
        let t0 = Instant::now();
        l.acquire(100).await;
        let elapsed = t0.elapsed();
        assert!(
            elapsed >= Duration::from_millis(80),
            "expected ~100ms wait, got {elapsed:?}"
        );
    }
}
