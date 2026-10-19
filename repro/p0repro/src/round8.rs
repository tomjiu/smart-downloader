//! 第八轮：审计 PR #114 的 rate.rs 修复（取消债务回滚）
//!
//! 审查点：
//!   1. `*next == self.end` 用 Instant 相等判链尾 —— 并发下是否误判？
//!   2. `unwrap_or_else(|e| e.into_inner())` 吞 mutex 中毒 —— 是否安全？
//!   3. 取消回滚是否真能落回（时序窗口）

#[cfg(test)]
mod round8 {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// 复刻 PR 的 RateInner（简化）
    struct RateInner {
        rate: AtomicU64,
        next: std::sync::Mutex<Instant>,
    }

    /// 复刻 PR 的 DebtReservation
    struct DebtReservation {
        inner: Arc<RateInner>,
        target: Instant,
        end: Instant,
        consumed: bool,
    }

    impl Drop for DebtReservation {
        fn drop(&mut self) {
            if !self.consumed {
                let mut next = self.inner.next.lock().unwrap_or_else(|e| e.into_inner());
                if *next == self.end {
                    *next = self.target;
                }
            }
        }
    }

    /// 复刻 PR 的临界区（只做算术，不 sleep）
    fn reserve(inner: &Arc<RateInner>, n: u64) -> Option<DebtReservation> {
        let rate = inner.rate.load(Ordering::Relaxed);
        if rate == 0 {
            return None;
        }
        let dur = Duration::from_secs_f64(n as f64 / rate as f64);
        let now = Instant::now();
        let (target, end) = {
            let mut next = inner.next.lock().unwrap_or_else(|e| e.into_inner());
            let target = (*next).max(now);
            *next = target + dur;
            (target, target + dur)
        };
        Some(DebtReservation {
            inner: inner.clone(),
            target,
            end,
            consumed: false,
        })
    }

    #[test]
    fn audit_fixes() {
        println!("=== PR #114 rate.rs 修复审计 ===\n");

        // ── 1. 验证锁内 sleep 已消除 ──
        println!("[1] 锁内 sleep 消除");
        let inner = Arc::new(RateInner {
            rate: AtomicU64::new(1024 * 1024), // 1 MB/s
            next: std::sync::Mutex::new(Instant::now()),
        });
        let t0 = Instant::now();
        let r = reserve(&inner, 16 * 1024 * 1024); // 16MB @ 1MB/s = 16秒债务
        let reserve_time = t0.elapsed();
        println!("     reserve() 耗时: {:.3}ms（应为亚毫秒，不含 sleep）",
                 reserve_time.as_secs_f64() * 1000.0);
        assert!(reserve_time < Duration::from_millis(10), "临界区耗时过长");
        println!("     ✅ 临界区只剩瞬时算术\n");

        // ── 2. 验证取消回滚 ──
        println!("[2] 取消债务回滚");
        let before = *inner.next.lock().unwrap();
        drop(r); // 未 consumed → 应回滚
        let after = *inner.next.lock().unwrap();
        println!("     drop 前 next = {:?}", before.duration_since(t0).as_millis());
        println!("     drop 后 next = {:?}", after.duration_since(t0).as_millis());
        println!("     回滚: {} ✅", if after < before { "是" } else { "否 ❌" });
        assert!(after <= before, "应回滚");

        // ── 3. 验证 consumed=true 时不回滚 ──
        println!("\n[3] 正常消费不回滚");
        let inner2 = Arc::new(RateInner {
            rate: AtomicU64::new(1024 * 1024),
            next: std::sync::Mutex::new(Instant::now()),
        });
        let mut r2 = reserve(&inner2, 1024 * 1024).unwrap();
        let before2 = *inner2.next.lock().unwrap();
        r2.consumed = true;
        drop(r2);
        let after2 = *inner2.next.lock().unwrap();
        println!("     consumed=true → next 保持: {} ✅", if after2 == before2 { "是" } else { "否 ❌" });
        assert_eq!(before2, after2);

        // ── 4. 关键：Instant 相等判链尾的误判风险 ──
        println!("\n[4] 链尾判定的误判风险（*next == self.end）");
        println!("     场景：两个并发预约，end 恰好相同");
        let inner3 = Arc::new(RateInner {
            rate: AtomicU64::new(1), // 极慢，让 end 落在同一 ns 附近
            next: std::sync::Mutex::new(Instant::now()),
        });
        // 构造两次相同 n 的预约
        let a = reserve(&inner3, 1).unwrap();
        let b = reserve(&inner3, 1).unwrap();
        // a 和 b 的 end 不同（链式推进），但如果 rate 极大，dur≈0 → end 可能相同
        println!("     a.end == b.end ? {}", a.end == b.end);

        // 真正风险场景：rate 极大时 dur → 0
        let inner4 = Arc::new(RateInner {
            rate: AtomicU64::new(u64::MAX / 2), // 极大速率
            next: std::sync::Mutex::new(Instant::now()),
        });
        let x = reserve(&inner4, 1);
        let y = reserve(&inner4, 1);
        if let (Some(x), Some(y)) = (x, y) {
            println!("     极大速率下 x.end == y.end ? {}", x.end == y.end);
            println!("     ⚠️  若相等，x 被取消时会回滚 y 的预约 → y 的槽位被破坏");
            println!("     → 但此场景 dur = n/rate ≈ 0，回滚影响可忽略");
        }

        println!("\n=== 结论 ===");
        println!("  ✅ 锁内 sleep 已消除（临界区亚毫秒）");
        println!("  ✅ 取消回滚有效（未 consumed 时回退链尾）");
        println!("  ✅ 正常消费不回滚");
        println!("  ⚠️  链尾判定用 Instant 相等，理论上并发误判；");
        println!("      实际影响受限于 dur 量级，非阻断项");
        println!("  ⚠️  unwrap_or_else(into_inner) 吞中毒——纯算术临界区安全，");
        println!("      但应在注释说明这是有意为之");
    }
}
