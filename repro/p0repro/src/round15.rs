//! 第十五轮：审计 batch10 rate.rs 的 set_rate_kb_s 热调竞态
//!
//! 背景（batch10 P1-3 修复）：
//!   set_rate_kb_s 会在速率变化时把 `next` 重置为 now，清空旧速率债务。
//!
//! 审查点：
//!   `wait_inner` 先在**无锁**状态读 rate 并算出 dur，然后才拿锁推进 next。
//!   若 set_rate_kb_s 在这个窗口内重置了 next，
//!   则 wait_inner 会把**旧速率对应的长 dur** 叠加到刚重置的 next 上，
//!   新速率被旧债务立刻污染 —— 修复意图落空。
//!
//!   时序：
//!     T1 wait_inner: 读 rate=1MB/s → dur=4s（锁外，未持锁）
//!     T2 set_rate_kb_s(100MB/s): next = now（清债务 ✓）
//!     T1 wait_inner: 拿锁 → next = max(next,now) + 4s = now+4s   ← ❌ 旧债务复活

#![allow(dead_code)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct RateInner {
    rate: AtomicU64,
    next: Mutex<Instant>,
}

struct RateLimiter {
    inner: Arc<RateInner>,
}

impl RateLimiter {
    fn new(bytes_per_sec: u64) -> Self {
        RateLimiter {
            inner: Arc::new(RateInner {
                rate: AtomicU64::new(bytes_per_sec),
                next: Mutex::new(Instant::now()),
            }),
        }
    }

    /// 复刻 batch10 的 set_rate_kb_s
    fn set_rate(&self, bytes_per_sec: u64) {
        let old = self.inner.rate.swap(bytes_per_sec, Ordering::Relaxed);
        if old != bytes_per_sec {
            *self.inner.next.lock().unwrap() = Instant::now();
        }
    }

    /// 复刻 batch10 的 wait_inner 的**临界区部分**（不含实际 sleep）
    /// 返回 (target, end) —— 模拟"读到 dur 后拿锁推进"
    fn reserve(&self, n: u64) -> (Instant, Instant) {
        // ① 锁外读 rate 并算 dur
        let rate = self.inner.rate.load(Ordering::Relaxed);
        if rate == 0 {
            let now = Instant::now();
            return (now, now);
        }
        let dur = Duration::from_secs_f64(n as f64 / rate as f64);

        // ② 拿锁推进
        let mut next = self.inner.next.lock().unwrap();
        let now = Instant::now();
        let target = (*next).max(now);
        *next = target + dur;
        (target, target + dur)
    }

    fn next_snapshot(&self) -> Instant {
        *self.inner.next.lock().unwrap()
    }
}

#[cfg(test)]
mod round15 {
    use super::*;

    #[test]
    fn set_rate_race_analysis() {
        println!("=== set_rate_kb_s 热调竞态分析 ===\n");

        // ── 顺序执行：修复生效 ──
        println!("[场景 A] 顺序执行（无并发）—— 修复生效\n");
        let l = RateLimiter::new(1_000_000); // 1 MB/s
        // 制造债务：预留 4MB → dur = 4s
        let (t1, e1) = l.reserve(4_000_000);
        println!("  1MB/s 预留 4MB → target={:.0}ms end={:.0}ms",
                 t1.elapsed().as_millis(), e1.elapsed().as_millis());
        let debt = l.next_snapshot().saturating_duration_since(Instant::now());
        println!("  当前债务: {:.0}ms", debt.as_millis());

        // 热调到 100MB/s
        l.set_rate(100_000_000);
        let after_reset = l.next_snapshot().saturating_duration_since(Instant::now());
        println!("  热调到 100MB/s 后债务: {:.0}ms  ✅ 已清空", after_reset.as_millis());
        assert!(after_reset < Duration::from_millis(100), "债务应被清空");

        // ── 并发：读到旧 rate，中间被 set_rate 打断 ──
        println!("\n[场景 B] 竞态：读到旧 rate 后、拿锁前被 set_rate 打断\n");

        // 手工模拟时序（确定性复现，不依赖线程调度）
        let inner = Arc::new(RateInner {
            rate: AtomicU64::new(1_000_000), // 1 MB/s
            next: Mutex::new(Instant::now()),
        });

        // T1: 锁外读 rate（旧值）+ 算 dur
        let rate_read = inner.rate.load(Ordering::Relaxed);
        let dur_t1 = Duration::from_secs_f64(4_000_000f64 / rate_read as f64);
        println!("  T1 读 rate = {} B/s（1MB/s）→ dur = {:.1}s", rate_read, dur_t1.as_secs_f64());

        // T2: set_rate 到 100MB/s，重置 next
        {
            let old = inner.rate.swap(100_000_000, Ordering::Relaxed);
            if old != 100_000_000 {
                *inner.next.lock().unwrap() = Instant::now();
            }
        }
        println!("  T2 set_rate(100MB/s) → next 已重置为 now");

        // T1 继续：拿锁推进（用的是旧 dur）
        {
            let mut next = inner.next.lock().unwrap();
            let now = Instant::now();
            let target = (*next).max(now);
            *next = target + dur_t1;
        }
        let polluted = inner.next.lock().unwrap().saturating_duration_since(Instant::now());
        println!("  T1 推进 next = now + {:.1}s（旧速率 dur）", dur_t1.as_secs_f64());
        println!("  → 最终债务: {:.0}ms", polluted.as_millis());

        assert!(
            polluted > Duration::from_secs(3),
            "竞态导致旧债务复活"
        );
        println!("\n  ❌ 新速率(100MB/s)的第一块仍被旧债务压制 {:.1}s", polluted.as_secs_f64());
        println!("     P1-3 修复意图（热调清债务）在这个窗口内失效。");

        println!("\n=== 根因 ===\n");
        println!("  wait_inner 的 `rate` 读取与 `next` 推进**不在同一个临界区**：");
        println!("    let rate = ...load(...);          // ← 锁外");
        println!("    let dur = n / rate;               // ← 锁外算");
        println!("    let mut next = ...lock().unwrap();         // ← 锁内");
        println!("    *next = target + dur;             // ← 用旧 dur");
        println!();
        println!("  而 set_rate_kb_s 只重置 next，无法撤销已经算好的 dur。");

        println!("\n=== 影响评估 ===\n");
        println!("  · 触发条件：热调速率与正在进行的分段限速调用并发");
        println!("  · 后果：热调后**第一块**仍按旧速率等待（最多旧的 dur 长度）");
        println!("  · 后续块正常（下次 wait_inner 会读新 rate）");
        println!("  · 严重性：**P3 体验级** —— 不是数据错误，只是热调生效有一次性延迟");
        println!("  · 实务影响小：热调本来就不是高频操作");

        println!("\n=== 修法（若要严丝合缝）===\n");
        println!("  把 rate 读取移入临界区：");
        println!("    let sleep_for = {{");
        println!("        let mut next = self.inner.next.lock().unwrap();");
        println!("        let rate = self.inner.rate.load(Ordering::Relaxed);   // ← 锁内读");
        println!("        if rate == 0 {{ return; }}");
        println!("        let dur = Duration::from_secs_f64(n as f64 / rate as f64);");
        println!("        let now = Instant::now();");
        println!("        let target = (*next).max(now);");
        println!("        *next = target + dur;");
        println!("        target.checked_duration_since(now)");
        println!("    }};");
        println!("  → 与 set_rate_kb_s 串行化，热调即时生效");
        println!("  → 代价：临界区多一次 atomic load（纳秒级，可忽略）");

        println!("\n  ⚠️ 定性：P3，非阻断。batch10 的三处修复方向都正确，");
        println!("     这是修复后在极端时序下的残余窗口。");
    }
}

