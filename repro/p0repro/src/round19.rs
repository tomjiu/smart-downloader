//! 第十九轮：审计 batch10 P1-9 的「锁外落盘」引入的丢失更新竞态
//!
//! 修复内容（rss.rs）：
//!   原：持 rss_state 锁调 rss_save（锁内整表 JSON 序列化 + 写盘，毫秒级）
//!   新：锁内 clone() 快照 → 释放锁 → rss_save_snapshot(快照) 锁外写盘
//!
//! 审查点：
//!   锁外写的是一份**快照**，而快照可能已过期。
//!   两个并发修改各自持锁→各自 clone→各自锁外写，
//!   后写的那个用**旧快照**覆盖**新状态** → 丢失更新。

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq)]
struct RssState {
    feeds: Vec<String>,
    next_id: u64,
}

impl RssState {
    fn new() -> Self {
        RssState { feeds: vec![], next_id: 0 }
    }

    /// 模拟「写盘」——把快照内容记录到共享的"磁盘"
    fn save(&self, disk: &Mutex<Vec<String>>) {
        // 模拟序列化 + 磁盘写（毫秒级）；为可控起见这里只做记录
        let mut d = disk.lock().unwrap();
        // 完全覆盖（真实实现是 File::create + write 整表）
        *d = self.feeds.clone();
    }
}

struct Daemon {
    rss_state: Mutex<RssState>,
    disk: Mutex<Vec<String>>,
}

impl Daemon {
    fn new() -> Self {
        Daemon {
            rss_state: Mutex::new(RssState::new()),
            disk: Mutex::new(vec![]),
        }
    }

    /// 复刻 batch10 的 rss_add_feed：锁内改 + clone 快照，锁外写
    /// `delay_between` 用于放大锁外写之前的窗口
    fn add_feed_locked_then_save(&self, name: &str, save_first: bool) {
        // ① 锁内：修改 + 复制快照
        let snapshot = {
            let mut st = self.rss_state.lock().unwrap();
            st.next_id += 1;
            st.feeds.push(name.to_string());
            st.clone()   // ← 快照
        };               // ← 释放锁

        // ② 锁外：写快照（这里可插入调度延迟）
        if save_first {
            // 竞态窗口：让另一个线程先完成它的写
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        snapshot.save(&self.disk);
    }
}

#[cfg(test)]
mod round19 {
    use super::*;
    use std::time::Duration;

    #[test]
    fn lost_update_reproducible() {
        println!("=== P1-9 锁外落盘的丢失更新竞态 ===\n");

        let d = Arc::new(Daemon::new());

        // 时序：
        //   A：锁内加 feed-a → 快照_A（含 a）→ 释放锁 → *等待*
        //   B：锁内加 feed-b → 快照_B（含 a,b）→ 释放锁 → 立即写
        //   A：醒来 → 写快照_A（只含 a）  ← 覆盖掉 b
        let d1 = d.clone();
        let t_a = std::thread::spawn(move || {
            d1.add_feed_locked_then_save("feed-a", true); // save_first=true → 先睡眠再写
        });

        // 确保 A 先进入并释放锁
        std::thread::sleep(Duration::from_millis(5));

        let d2 = d.clone();
        let t_b = std::thread::spawn(move || {
            d2.add_feed_locked_then_save("feed-b", false); // 不睡眠，立即写
        });

        t_a.join().unwrap();
        t_b.join().unwrap();

        let mem = d.rss_state.lock().unwrap().feeds.clone();
        let disk = d.disk.lock().unwrap().clone();

        println!("[时序] A 先锁内加 feed-a，睡眠 20ms 后才写盘");
        println!("       B 随后锁内加 feed-b，立即写盘");
        println!();
        println!("  内存中的 feeds : {:?}", mem);
        println!("  磁盘上的 feeds : {:?}", disk);
        println!();

        if mem != disk {
            println!("  ❌ **丢失更新**：磁盘与内存不一致");
            println!("     B 写入的 feed-b 被 A 的旧快照覆盖掉了。");
        } else {
            println!("  本次调度未触发（取决于时序）");
        }

        // 断言：内存有 2 条
        assert_eq!(mem.len(), 2, "内存应有 2 个 feed");
        // 磁盘 1 条 = 丢失更新已发生
        assert_eq!(
            disk.len(), 1,
            "磁盘应只有 1 条（旧快照覆盖新状态）—— 这就是丢失更新"
        );

        println!("=== 根因 ===\n");
        println!("  rss_save_snapshot 写的是**锁内 clone 的快照**，");
        println!("  但快照在锁释放后可能已过期：");
        println!();
        println!("  {{");
        println!("      let mut st = self.rss_state().lock();");
        println!("      st.feeds.push(...);                    // 改内存");
        println!("      let snapshot = st.clone();             // 快照（时刻 T1）");
        println!("  }}                                          // 释放锁");
        println!("  self.rss_save_snapshot(&snapshot);          // 写盘（时刻 T2）");
        println!();
        println!("  T1 → T2 之间若其他线程修改并写盘，本线程会用 T1 的旧快照覆盖。");

        println!("\n=== 影响 ===\n");
        println!("  触发条件：两个并发 RSS 操作（add/update/refresh/规则匹配）");
        println!("  后果：磁盘上的 RSS 状态**丢失一部分变更**");
        println!("        · 重启后刚添加的订阅消失");
        println!("        · 已读标记回退（重复推送）");
        println!("  概率：中等（RSS 定时刷新 + 用户手动 add 并发常见）");
        println!("  定性：**P2 数据丢失**（非崩溃，但用户可见的状态丢失）");

        println!("\n=== 修法 ===\n");
        println!("  方案 1（推荐）：给落盘加独立串行锁 + 从**当前状态**取快照");
        println!("    fn rss_save_now(&self) {{");
        println!("        let _g = self.rss_persist_lock.lock();   // 串行化写");
        println!("        let snapshot = self.rss_state().lock().clone();  // 持锁取最新");
        println!("        snapshot.save(...);");
        println!("    }}");
        println!("    → 写盘串行 + 快照总是最新，无丢失");
        println!("    → 代价：写盘期间持 rss_state 锁（回到了原问题）");
        println!();
        println!("  方案 2：版本号 + 乐观写入");
        println!("    struct RssState {{ version: u64, ... }}");
        println!("    写盘前比对 version，若已变化则重新取快照");
        println!();
        println!("  方案 3：写盘串行锁 + 锁内只取 version，写前比对");
        println!("    → version 变了就重取，兼顾性能与正确性");
        println!();
        println!("  ⚠️ 当前实现（无锁外串行化 + 无版本校验）= **确定会丢失更新**");
        println!("     batch10 的性能优化牺牲了正确性。");
    }
}
