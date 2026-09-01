//! 下载速率限制器（M4b 增量）：跨段共享的 token-bucket 近似——
//! "下一 chunk 允许完成时刻"（deadline 链）法：每次消费 n 字节把全局
//! deadline 向后推 n/rate 秒；落后于时钟（无带宽积压）时从当前时刻重新起算。
//! 简化实现：速率 0 = 不限（no-op，零开销路径）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// 速率限制器（`rate` = bytes/sec；0 = 不限）。多段并发共享一个实例（总量限制）。
#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<RateInner>,
}

struct RateInner {
    /// 速率 bytes/sec（AtomicU64：运行中可调，per-task 限速即时生效）。
    rate: AtomicU64,
    /// 下一 chunk 允许完成时刻（跨段共享）。
    next: Mutex<Instant>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        RateLimiter::new(0)
    }
}

impl Default for RateInner {
    fn default() -> Self {
        RateInner {
            rate: AtomicU64::new(0),
            next: Mutex::new(Instant::now()),
        }
    }
}

impl RateLimiter {
    /// `kb_s` = KiB/s；0 = 不限。
    pub fn new(kb_s: u32) -> Self {
        RateLimiter {
            inner: Arc::new(RateInner {
                rate: AtomicU64::new(kb_s as u64 * 1024),
                next: Mutex::new(Instant::now()),
            }),
        }
    }

    /// 运行中调整速率（KiB/s；0 = 不限）。已持有的 Arc 立即观察到新速率
    /// —— per-task 限速变更无需重启下载循环。
    pub fn set_rate_kb_s(&self, kb_s: u32) {
        self.inner.rate.store(kb_s as u64 * 1024, Ordering::Relaxed);
    }

    /// 当前速率（KiB/s；0 = 不限）。
    pub fn rate_kb_s(&self) -> u32 {
        (self.inner.rate.load(Ordering::Relaxed) / 1024) as u32
    }

    /// 消费 `n` 字节的"时间预算"：若 deadline 在将来则 sleep 至 deadline。
    /// 速率 0 → 立即返回（不限速）。
    pub async fn wait(&self, n: u64) {
        let rate = self.inner.rate.load(Ordering::Relaxed);
        if rate == 0 || n == 0 {
            return;
        }
        let dur = Duration::from_secs_f64(n as f64 / rate as f64);
        let mut next = self.inner.next.lock().await;
        let now = Instant::now();
        let target = (*next).max(now); // 积压落后则从当下起算（无带宽时不让旧债务堆积）
        *next = target + dur;
        if target > now {
            tokio::time::sleep(target - now).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn unlimited_is_noop() {
        let lim = RateLimiter::new(0);
        let t0 = Instant::now();
        lim.wait(1 << 20).await; // 1MiB 不限速应立即返回
        assert!(t0.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn cold_start_releases_first_chunk() {
        // deadline 法语义：无积压（next 落后于 now）时首个 chunk 立即放行（突发恢复不欠债）
        let lim = RateLimiter::new(512); // 512KiB/s
        let t0 = Instant::now();
        lim.wait(1024 * 1024).await; // 1MiB
        assert!(t0.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn limited_rates_second_call_waits() {
        // 连续消费累积：第二次调用必被限 —— 1MiB/s 下两个 1MiB ≈ 1s
        let lim = RateLimiter::new(1024);
        let t0 = Instant::now();
        lim.wait(1024 * 1024).await; // 冷启动放行，next 推进 +1s
        lim.wait(1024 * 1024).await; // 受 next 约束 → sleep ≈1s
        let el = t0.elapsed();
        assert!(el >= Duration::from_millis(900), "elapsed {el:?}");
        assert!(el < Duration::from_secs(3), "不应超长 {el:?}");
    }

    #[tokio::test]
    async fn shared_across_tasks_accumulates() {
        // 共享实例跨任务累积：3×1MiB @ 1MiB/s → 后两段受限 ≈ 2s
        let lim = RateLimiter::new(1024);
        let t0 = Instant::now();
        lim.wait(1024 * 1024).await;
        lim.wait(1024 * 1024).await;
        lim.wait(1024 * 1024).await;
        let el = t0.elapsed();
        assert!(el >= Duration::from_millis(1800), "elapsed {el:?}");
        assert!(el < Duration::from_secs(4), "不应超长 {el:?}");
    }

    #[tokio::test]
    async fn set_rate_hot_adjust_observed_by_holders() {
        // per-task 限速核心语义：已持有的 Arc 热调速率 → 后续 wait 按新速率节流。
        // 注意 deadline 链的债务（next 绝对时刻）跨速率携带 —— 非零速率的
        // 时长断言会受先前累积债务干扰；故用 0（早退路径，不受债务影响）
        // 做" holders 观察到热调"的确定性行为断言，非零值用 getter 回读。
        let lim = RateLimiter::new(1024);
        assert_eq!(lim.rate_kb_s(), 1024);
        // 1MiB/s 下 4MiB 需 ≈3s；热调 0 = 不限 → 立即放行（已持有实例生效）
        lim.set_rate_kb_s(0);
        let t0 = Instant::now();
        lim.wait(4 * 1024 * 1024).await;
        assert!(
            t0.elapsed() < Duration::from_millis(50),
            "热调 0 后必须立即放行（不限速）"
        );
        // 读侧证据：速率回读即时反映热调值
        lim.set_rate_kb_s(4 * 1024);
        assert_eq!(lim.rate_kb_s(), 4 * 1024);
        assert_eq!(lim.rate_kb_s(), 4096, "KiB/s 口径换算一致");
    }
}
