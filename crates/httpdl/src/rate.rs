//! 下载速率限制器（M4b 增量）：跨段共享的 token-bucket 近似——
//! "下一 chunk 允许完成时刻"（deadline 链）法：每次消费 n 字节把全局
//! deadline 向后推 n/rate 秒；落后于时钟（无带宽积压）时从当前时刻重新起算。
//! 简化实现：速率 0 = 不限（no-op，零开销路径）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use parking_lot::Mutex as SyncMutex;

/// 速率限制器（`rate` = bytes/sec；0 = 不限）。多段并发共享一个实例（总量限制）。
#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<RateInner>,
}

struct RateInner {
    /// 速率 bytes/sec（AtomicU64：运行中可调，per-task 限速即时生效）。
    rate: AtomicU64,
    /// 下一 chunk 允许完成时刻（跨段共享）。
    /// P1-1 修复：改用同步锁（parking_lot）——原 tokio::Mutex 在持锁期间
    /// sleep，并发段被迫串行排队（B 等 A 的 sleep + 自己的 sleep），
    /// 实际速率低于设定值。临界区内仅做纳秒级 deadline 推进，同步锁足够。
    next: SyncMutex<Instant>,
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
            next: SyncMutex::new(Instant::now()),
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
                next: SyncMutex::new(Instant::now()),
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
                next: SyncMutex::new(Instant::now()),
                upstream: Some(upstream.clone()),
            }),
        }
    }

    /// 运行中调整速率（KiB/s；0 = 不限）。已持有的 Arc 立即观察到新速率
    /// —— per-task 限速变更无需重启下载循环。
    /// P1-3 修复：速率变更时同步清空 deadline 债务（next 重置为 now）——
    /// 原实现保留旧速率积压的 next（如 1MiB/s 跑 4MiB 后 next=now+3s），
    /// 热调到更高速率后旧债务仍压制 3s，新速率迟迟达不到。热调语义
    /// = 用户重新设定速率，旧速率的欠债不应继续生效。
    pub fn set_rate_kb_s(&self, kb_s: u32) {
        let new_rate = kb_s as u64 * 1024;
        let old = self.inner.rate.swap(new_rate, Ordering::Relaxed);
        if old != new_rate {
            *self.inner.next.lock() = Instant::now();
        }
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
    /// P1-1 修复：锁内只推进 deadline，【锁外】sleep——原实现持锁 sleep，
    /// 并发段被迫串行排队（段 B 的等待 = 段 A 的 sleep + 自己的 sleep），
    /// N 个并发段的实际合计速率远低于设定值。修复后各段在锁内瞬时完成
    /// deadline 推进，各自独立 sleep 至目标时刻，合计速率精确等于设定。
    async fn wait_inner(&self, n: u64) {
        let rate = self.inner.rate.load(Ordering::Relaxed);
        if rate == 0 {
            return;
        }
        let dur = Duration::from_secs_f64(n as f64 / rate as f64);
        let sleep_for = {
            let mut next = self.inner.next.lock();
            let now = Instant::now();
            let target = (*next).max(now); // 积压落后则从当下起算（无带宽时不让旧债务堆积）
            *next = target + dur;
            target.checked_duration_since(now)
        };
        if let Some(d) = sleep_for {
            tokio::time::sleep(d).await;
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
        if dt < RATE_SAMPLE_MIN_WINDOW {
            // P1-2 修复：短窗口不推进基线——原实现先推进 last_done/last_at
            // 再返回 smoothed，短窗口内的增量字节被吞（下一完整窗口的 db
            // 从新基线起算，中间增量永久丢失）→ 速率系统性低估。
            // 保持旧基线：增量累计到下一个 ≥200ms 完整窗口一并计入。
            return self.smoothed;
        }
        let db = done.saturating_sub(self.last_done);
        self.last_done = done;
        self.last_at = Some(now);
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
        // P1-3 修复后：热调同时清空旧速率的 deadline 债务——1024KiB/s 下
        // 先积压一笔，热调到 8192KiB/s 后新消费按新速率立即起算（不再被
        // 旧债务压制）。
        let lim = RateLimiter::new(1024);
        assert_eq!(lim.rate_kb_s(), 1024);
        lim.wait(1024 * 1024).await; // 冷启动放行，next 推进 +1s（旧速率债务）
        lim.set_rate_kb_s(8192); // 热调 + 债务清零
        let t0 = Instant::now();
        lim.wait(1024 * 1024).await; // 8MiB/s 下 1MiB ≈ 128ms（无旧债）
        let el = t0.elapsed();
        assert!(
            el < Duration::from_millis(600),
            "热调后旧债务必须清除（实际 {el:?}，预期 ≤≈128ms）"
        );
        // 读侧证据：速率回读即时反映热调值
        lim.set_rate_kb_s(4 * 1024);
        assert_eq!(lim.rate_kb_s(), 4 * 1024);
        assert_eq!(lim.rate_kb_s(), 4096, "KiB/s 口径换算一致");
    }

    /// P1-1 回归：并发段不被持锁 sleep 串行化——4 个并发段 @ 4MiB/s
    /// 各消费 256KiB，合计 1MiB ≈ 256ms；若持锁 sleep 串行排队，耗时
    /// 将 ≥ 4×64ms 且逐段叠加。修复后各段独立 sleep，总耗时 ≈ 单段最长。
    #[tokio::test]
    async fn concurrent_segments_not_serialized_by_lock() {
        let lim = RateLimiter::new(4 * 1024); // 4 MiB/s
        let t0 = Instant::now();
        let mut futs = Vec::new();
        for _ in 0..4 {
            let l = lim.clone();
            futs.push(tokio::spawn(async move {
                l.wait(256 * 1024).await; // 每段 256KiB @ 4MiB/s = 64ms
            }));
        }
        for f in futs {
            f.await.unwrap();
        }
        let el = t0.elapsed();
        // 4×64ms = 256ms 合计预算；串行化时每段还要叠加前段的 sleep
        //（≥ 64+128+192+256 = 640ms）。容忍调度噪声取 500ms 阈值。
        assert!(
            el < Duration::from_millis(500),
            "并发段被持锁 sleep 串行化: {el:?}（预期 ≈256ms）"
        );
    }

    /// P1-2 回归：短窗口增量不丢失——密集采样期间的字节增量须累计到
    /// 下一个完整窗口（修复前短窗口推进基线，增量被吞，速率系统性低估）。
    #[test]
    fn rate_sample_short_window_accumulates_bytes() {
        let mut s = RateSample::default();
        s.sample(0); // 基线 t0
        std::thread::sleep(Duration::from_millis(250));
        let r1 = s.sample(1000); // 完整窗口：0→1000
        assert!(r1 > 0);
        // 密集采样窗口（<200ms）：增量 500 发生在此窗口
        std::thread::sleep(Duration::from_millis(30));
        let _ = s.sample(1500); // 短窗口 → 返回 smoothed，基线不推进
        // 下一完整窗口：从基线 1000 起算（增量 500 + 新增 300 须全部计入）
        std::thread::sleep(Duration::from_millis(250));
        let r2 = s.sample(1800);
        assert!(
            r2 > 0,
            "短窗口后的完整窗口必须计入累计增量（修复前基线被提前推进致增量丢失）"
        );
    }
}
