//! 第十八轮：审计 daemon/state 的 ABBA 死锁风险
//!
//! 发现：
//!   两把锁 tasks / slot_reservations（均为 parking_lot::Mutex，不可重入）
//!   在两条路径上以**相反顺序**获取：
//!
//!   路径 A（gate_or_enqueue / active_slot_counts）：
//!       tasks → slot_reservations
//!   路径 B（attach_reserved_handle / rollback_slot_reservation）：
//!       slot_reservations → tasks
//!
//!   并发 A + B = 经典 ABBA 死锁。
//!
//! 历史：
//!   · ABBA 在 batch10 **之前就存在**（main 的 active_slot_counts 已经是
//!     tasks 先于 slot_reservations）
//!   · 但 batch10 把 gate_or_enqueue 的「计数 + 插入 + 登记」合并进同一
//!     tasks 临界区（修复超卖），**持锁时间变长，死锁窗口增大**
//!
//! 本文件用最小模型实证死锁可复现。

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// 复刻 DaemonState 的两把锁
struct State {
    tasks: Mutex<Vec<String>>,
    slot_reservations: Mutex<Vec<String>>,
}

impl State {
    fn new() -> Self {
        State {
            tasks: Mutex::new(Vec::new()),
            slot_reservations: Mutex::new(Vec::new()),
        }
    }

    /// 路径 A：gate_or_enqueue（tasks → slot_reservations）
    fn path_a(&self, id: &str) {
        let mut tasks = self.tasks.lock().unwrap();
        // 模拟「计数检查 + 插入」在 tasks 临界区内
        let res = self.slot_reservations.lock().unwrap(); // slot_reservations（保持持有）
        tasks.push(id.to_string());
        drop(res);
    }

    /// 路径 B：attach_reserved_handle（slot_reservations → tasks）
    fn path_b(&self, id: &str) {
        self.slot_reservations.lock().unwrap().push(id.to_string());
        let mut tasks = self.tasks.lock().unwrap();
        tasks.push(format!("attached-{id}"));
    }
}

#[cfg(test)]
mod round18 {
    use super::*;

    #[test]
    fn abba_deadlock_reproducible() {
        println!("=== ABBA 死锁实证 ===\n");

        println!("[锁序]");
        println!("  路径 A gate_or_enqueue      : tasks → slot_reservations");
        println!("  路径 B attach_reserved_handle: slot_reservations → tasks");
        println!("  → 相反顺序\n");

        let state = Arc::new(State::new());
        let a_started = Arc::new(AtomicBool::new(false));
        let b_started = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicUsize::new(0));

        // 用带超时的线程 + 检测完成数来判定死锁
        let s1 = state.clone();
        let a1 = a_started.clone();
        let b1 = b_started.clone();
        let d1 = done.clone();
        let _t_a = thread::spawn(move || {
            // 先占 tasks（模拟 A 拿到第一把锁）
            let tasks = s1.tasks.lock().unwrap();
            a1.store(true, Ordering::SeqCst);
            // 等 B 占住 slot_reservations
            while !b1.load(Ordering::SeqCst) {
                thread::yield_now();
            }
            thread::sleep(Duration::from_millis(20)); // 确保 B 已持锁
            // 现在尝试取 slot_reservations（会被 B 挡住）
            let _guard = s1.slot_reservations.lock().unwrap();
            d1.fetch_add(1, Ordering::SeqCst);
            drop(_guard);
            drop(tasks);
        });

        let s2 = state.clone();
        let a2 = a_started.clone();
        let b2 = b_started.clone();
        let d2 = done.clone();
        let _t_b = thread::spawn(move || {
            // 等 A 先占住 tasks
            while !a2.load(Ordering::SeqCst) {
                thread::yield_now();
            }
            // 占 slot_reservations
            let res = s2.slot_reservations.lock().unwrap();
            b2.store(true, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(20));
            // 现在尝试取 tasks（会被 A 挡住）→ 死锁
            let _guard = s2.tasks.lock().unwrap();
            d2.fetch_add(1, Ordering::SeqCst);
            drop(_guard);
            drop(res);
        });

        // 等待 2 秒看是否完成
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && done.load(Ordering::SeqCst) < 2 {
            thread::sleep(Duration::from_millis(10));
        }

        let completed = done.load(Ordering::SeqCst);
        println!("  2 秒后完成数: {completed}/2");

        if completed < 2 {
            println!("\n  ❌ **死锁**：两个线程互相等待对方持有的锁");
            println!("     线程 A 持 tasks，等 slot_reservations");
            println!("     线程 B 持 slot_reservations，等 tasks");
            println!("     parking_lot::Mutex 无超时 → 永久阻塞");
        } else {
            println!("\n  ✅ 未死锁（本次调度侥幸）");
        }

        // 不放 join（会永久挂起），直接泄漏线程
        // 这是刻意的：证明死锁后线程永不返回

        println!("\n=== 历史与 batch10 的影响 ===\n");
        println!("  batch10 之前（main）:");
        println!("    active_slot_counts() {{");
        println!("        let tasks = self.tasks.lock();");
        println!("        let reservations = self.slot_reservations.lock();   // ← 同样 ABBA");
        println!("    }}");
        println!("    → ABBA **在 batch10 之前就存在**");
        println!();
        println!("  batch10 的改动：");
        println!("    把「计数检查 + tasks.insert + slot_reservations.insert」");
        println!("    合并进同一 tasks 临界区（修复超卖 TOCTOU）");
        println!("    → 正确性提升 ✅");
        println!("    → 但持锁时间变长 → **死锁窗口增大** ⚠️");

        println!("\n=== 触发条件 ===\n");
        println!("  线程 1: add 任务 → gate_or_enqueue 持 tasks 锁");
        println!("  线程 2: 另一任务的 engine.add 完成 → attach_reserved_handle");
        println!("          持 slot_reservations 锁，等 tasks");
        println!("  → 两者同时发生即死锁");
        println!();
        println!("  概率：并发 add + 引擎接入完成重叠时（批量添加任务场景）");
        println!("  后果：**daemon 永久卡死**（无超时、无恢复）");

        println!("\n=== 修法 ===\n");
        println!("  统一锁序为 tasks → slot_reservations（两处都改）：");
        println!();
        println!("  // attach_reserved_handle");
        println!("  let (attached, _) = {{");
        println!("      let mut tasks = self.tasks.lock();          // ← 先 tasks");
        println!("      let mut reservations = self.slot_reservations.lock();");
        println!("      let removed = reservations.remove(task_id);");
        println!("      // ... 在 tasks 里更新 rec");
        println!("      (attached, removed)");
        println!("  }};");
        println!();
        println!("  // rollback_slot_reservation 同理");
        println!();
        println!("  并在文档（docs/LOCK_MODEL.md）里明确规定全局锁序。");
        println!("  LOCK_MODEL.md 目前只讲「至多持一把锁」，");
        println!("  但这两把锁实际经常同时持有 → 文档已过时。");
    }
}
