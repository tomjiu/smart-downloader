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
    /// 上游串联限速器（E16 全局总阀门）：Some = wait 先消费上游预算再消费
    /// 自身——任务级限速器链到引擎全局 limiter，保证「总阀门控制所有任务
    /// 合计不超速」对任务级限速任务同样成立（串联两层各自维护 deadline 链，
    /// 有效速率 ≈ min(任务级, 全局)）。仅允许一层（构造入口不暴露嵌套口）。
    upstream: Option<RateLimiter>,
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
            upstream: None,
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
                upstream: None,
            }),
        }
    }

    /// 任务级限速器构造（E16）：`kb_s` 同 `new`，另串联引擎全局 limiter 作
    /// 上游——每次 wait 先消费上游（全局）预算再消费自身，总阀门对任务级
    /// 限速任务同样生效。`upstream` 自身速率为 0（不限）时透传零开销。
    pub fn new_chained(kb_s: u32, upstream: &RateLimiter) -> Self {
        RateLimiter {
            inner: Arc::new(RateInner {
                rate: AtomicU64::new(kb_s as u64 * 1024),
                next: Mutex::new(Instant::now()),
                upstream: Some(upstream.clone()),
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
    /// 串联上游（E16）时先消费上游预算（上游自身速率为 0 → 透传零开销）。
    /// 速率 0 → 立即返回（不限速；上游已先行消费）。
    pub async fn wait(&self, n: u64) {
        if n == 0 {
            return;
        }
        if let Some(up) = self.inner.upstream.as_ref() {
            // 上游恒为单层（new_chained 构造口不暴露嵌套）→ 非递归内部路径，
            // 免 async fn 递归所需的 Box::pin 装箱（wait 每 chunk 一次）。
            up.wait_inner(n).await;
        }
        self.wait_inner(n).await;
    }

    /// 自身预算消费（不含上游）。`upstream: None` 恒成立时与 wait 等价。
    async fn wait_inner(&self, n: u64) {
        let rate = self.inner.rate.load(Ordering::Relaxed);
        if rate == 0 {
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

/// 速率采样窗口下限：低于此窗口视为密集采样噪声，沿用上次平滑值
/// （daemon 周期轮询 + 快照按需查询共用同一采样点，双消费者窗口可能被切碎）。
const RATE_SAMPLE_MIN_WINDOW: Duration = Duration::from_millis(200);

/// 速率采样器（E11）：`status()` 读取时以「done 增量 ÷ 距上次采样时长」估计
/// 瞬时速率（B/s）。无需后台计数——下载循环对 `done` 的单调累加即输入，
/// 引擎侧零改动接线。done 回退（换源重置/重下）以饱和减法兜底取 0，一轮自愈。
#[derive(Clone, Default)]
pub(crate) struct RateSample {
    last_done: u64,
    last_at: Option<Instant>,
    /// 窗口过短或首次采样时沿用的平滑值（上次完整窗口结果）。
    smoothed: u64,
}

impl RateSample {
    /// 以当前累计 `done` 采样一次，返回估计速率（B/s）。
    pub(crate) fn sample(&mut self, done: u64) -> u64 {
        let now = Instant::now();
        let Some(last) = self.last_at else {
            // 首采样无时间窗：登记基线，速率报 0（下一窗口起有值）
            self.last_done = done;
            self.last_at = Some(now);
            return 0;
        };
        let dt = now.duration_since(last);
        let db = done.saturating_sub(self.last_done);
        self.last_done = done;
        self.last_at = Some(now);
        if dt < RATE_SAMPLE_MIN_WINDOW {
            return self.smoothed;
        }
        let r = (db as f64 / dt.as_secs_f64()).round() as u64;
        self.smoothed = r;
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn rate_sample_first_read_is_zero() {
        let mut s = RateSample::default();
        assert_eq!(s.sample(0), 0, "首采样无时间窗 → 0");
        assert_eq!(s.sample(0), 0, "done 未动 → 0");
    }

    #[test]
    fn rate_sample_windowed_delta_over_elapsed() {
        let mut s = RateSample::default();
        s.sample(0);
        std::thread::sleep(Duration::from_millis(250));
        let r = s.sample(1000); // 1000B / ≥250ms ≈ ≤4000 B/s
        assert!(r > 0 && r <= 4500, "窗口速率应在合理区间: {r}");
    }

    #[test]
    fn rate_sample_short_window_keeps_smoothed() {
        let mut s = RateSample::default();
        s.sample(0);
        std::thread::sleep(Duration::from_millis(250));
        let r = s.sample(1000);
        assert!(r > 0);
        // 密集采样（窗口 <200ms）：沿用平滑值，不产生噪声
        assert_eq!(s.sample(1500), r, "短窗口应沿用平滑值");
    }

    #[test]
    fn rate_sample_done_regression_bounded_at_zero() {
        let mut s = RateSample::default();
        s.sample(0);
        std::thread::sleep(Duration::from_millis(250));
        assert!(s.sample(1000) > 0);
        // done 回退（换源重置）：饱和减法 → 一轮 0，随后自愈
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(s.sample(0), 0, "done 回退窗口速率为 0（饱和）");
        std::thread::sleep(Duration::from_millis(250));
        assert!(s.sample(500) > 0, "回退后恢复正常采样");
    }

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
    async fn chained_zero_task_rate_still_consumes_upstream() {
        // E16 语义核心：任务级限速 0（不限）串联全局后仍受全局约束——
        // 任务级不限速 ≠ 绕过总阀门。2×1MiB @ 全局 1MiB/s → 第二笔 ≈1s。
        let global = RateLimiter::new(1024);
        let task = RateLimiter::new_chained(0, &global);
        let t0 = Instant::now();
        task.wait(1024 * 1024).await; // 冷启动放行，全局 next 推进 +1s
        task.wait(1024 * 1024).await; // 受全局 next 约束
        let el = t0.elapsed();
        assert!(el >= Duration::from_millis(900), "elapsed {el:?}");
        assert!(el < Duration::from_secs(3), "不应超长 {el:?}");
    }

    #[tokio::test]
    async fn chained_upstream_zero_is_passthrough_noop() {
        // 全局不限（0）时上游透传零开销：任务级限速 1024KiB/s 独自生效，
        // 行为与未串联的 new(1024) 一致（第二次调用受限 ≈1s）。
        let global = RateLimiter::new(0);
        let task = RateLimiter::new_chained(1024, &global);
        let t0 = Instant::now();
        task.wait(1024 * 1024).await;
        task.wait(1024 * 1024).await;
        let el = t0.elapsed();
        assert!(el >= Duration::from_millis(900), "elapsed {el:?}");
        assert!(el < Duration::from_secs(3), "不应超长 {el:?}");
    }

    #[tokio::test]
    async fn chained_task_rate_hot_adjust_keeps_upstream() {
        // 任务级热调速率为 0 后仍受上游约束（上游链不随热调丢失）。
        let global = RateLimiter::new(1024);
        let task = RateLimiter::new_chained(0, &global);
        task.set_rate_kb_s(0); // 语义同初始（不限），验证热调不破坏上游链
        let t0 = Instant::now();
        task.wait(1024 * 1024).await;
        task.wait(1024 * 1024).await;
        let el = t0.elapsed();
        assert!(
            el >= Duration::from_millis(900),
            "热调后上游约束丢失: {el:?}"
        );
        assert!(el < Duration::from_secs(3), "不应超长 {el:?}");
    }

    #[tokio::test]
    async fn chained_upstream_hot_adjust_observed_through_task_limiter() {
        // 全局总阀门热调：引擎全局 limiter set_rate 后，经由任务级链式限速器
        // 立即可见（全局 Arc 内共享）——全局 0 → 4MiB 放行；非零值回读验证。
        let global = RateLimiter::new(0);
        let task = RateLimiter::new_chained(0, &global);
        let t0 = Instant::now();
        task.wait(4 * 1024 * 1024).await;
        assert!(
            t0.elapsed() < Duration::from_millis(50),
            "全局不限应立即放行"
        );
        global.set_rate_kb_s(4 * 1024);
        assert_eq!(global.rate_kb_s(), 4 * 1024, "全局热调回读");
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
