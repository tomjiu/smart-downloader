//! Exponential-backoff retry helper, designed around Quark's three-segment
//! error code (analysis §4.1 / §8).
//!
//! Quark uses `2 ** retry_count` seconds between retries, capped at 60s. We
//! replicate that with a configurable cap and an optional jitter.

use std::time::Duration;

use crate::error::{DownloadError, ErrorCategory, MAX_RETRY};

/// Default cap (Quark uses 60 s; we use 30 s to be slightly less aggressive).
pub const DEFAULT_BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Compute the backoff delay for a given attempt number (1-indexed).
///
/// `delay = min(cap, 2^attempt secs)`.
#[must_use]
pub fn backoff_delay(attempt: u32) -> Duration {
    backoff_delay_capped(attempt, DEFAULT_BACKOFF_CAP)
}

/// Compute backoff with a custom cap.
#[must_use]
pub fn backoff_delay_capped(attempt: u32, cap: Duration) -> Duration {
    if attempt == 0 {
        return Duration::ZERO;
    }
    // 2^attempt — but cap the shift to avoid overflow on huge attempt counts.
    let shift = (attempt - 1).min(20) as u32;
    let secs = 1u64 << shift;
    let dur = Duration::from_secs(secs);
    if dur > cap {
        cap
    } else {
        dur
    }
}

/// Add ±10 % uniform jitter to a duration (helps avoid thundering-herd
/// retries against the same mirror).
#[must_use]
pub fn jittered(d: Duration) -> Duration {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let ms = d.as_millis() as i64;
    let delta = (ms / 10).max(1);
    let jit = rng.gen_range(-delta..=delta);
    let new_ms = (ms + jit).max(0) as u64;
    Duration::from_millis(new_ms)
}

/// Drive a fallible async operation through `MAX_RETRY` retries with
/// exponential backoff. Returns `Ok` on the first success, `Err` on
/// permanent failure.
///
/// `op` receives the attempt number (1-indexed) so it can record metadata.
pub async fn with_retry<F, Fut, T>(mut op: F) -> Result<T, DownloadError>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = Result<T, DownloadError>>,
{
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match op(attempt).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if !e.is_retryable() || attempt >= MAX_RETRY {
                    return Err(e.inc_retry());
                }
                let delay = jittered(backoff_delay(attempt));
                tracing::warn!(
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    err = %e,
                    "retrying after error"
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_growth_is_exponential() {
        assert_eq!(backoff_delay(1), Duration::from_secs(1));
        assert_eq!(backoff_delay(2), Duration::from_secs(2));
        assert_eq!(backoff_delay(3), Duration::from_secs(4));
        assert_eq!(backoff_delay(4), Duration::from_secs(8));
    }

    #[test]
    fn backoff_caps_at_default_cap() {
        // 2^5 = 32s > 30s cap.
        assert_eq!(backoff_delay(5), DEFAULT_BACKOFF_CAP);
        // Even with huge attempt, still capped.
        assert_eq!(backoff_delay(99), DEFAULT_BACKOFF_CAP);
    }

    #[test]
    fn backoff_zero_is_zero() {
        assert_eq!(backoff_delay(0), Duration::ZERO);
    }

    #[tokio::test]
    async fn with_retry_succeeds_on_second_attempt() {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let c = counter.clone();
        let res: Result<u32, DownloadError> = with_retry(|_n| {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n == 0 {
                    Err(DownloadError::new(0, ErrorCategory::Network, "first attempt fails").with_extra(10054))
                } else {
                    Ok(n)
                }
            }
        })
        .await;
        assert!(res.is_ok());
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn with_retry_gives_up_after_max() {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let c = counter.clone();
        let res: Result<(), DownloadError> = with_retry(|_| {
            let c = c.clone();
            async move {
                c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err(DownloadError::new(0, ErrorCategory::Network, "always fails").with_extra(10054))
            }
        })
        .await;
        assert!(res.is_err());
        // Attempted at least MAX_RETRY times.
        let n = counter.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(n, MAX_RETRY);
    }
}
