//! 第十七轮：审计 batch10 P0-3 的 poll_ring 修复
//!
//! 修复内容（bt.rs save_fastresume_impl）：
//!   原：`let poll_ring = !loop_active.load(...)` 【循环外一次性采样】
//!   新：每轮重新采样 + pop 到的 alerts 先过 dispatch_resume_alerts
//!
//! 审查点：
//!   dispatch_resume_alerts **只处理 Resume 类**，其它 alert（State/Finished、
//!   State/Error）在 poll_ring 模式下仍被丢弃。
//!   即：修复缩小了竞态窗口，但没有消除「非 Resume alert 被吞」的根因。

#![allow(dead_code)]

#[derive(Debug, Clone, PartialEq)]
enum AlertKind {
    Resume,
    State,
}

#[derive(Debug, Clone, PartialEq)]
enum StateSubKind {
    Finished,
    Error,
    Other,
}

#[derive(Debug, Clone)]
struct Alert {
    kind: AlertKind,
    ih: String,
    sub: StateSubKind,
}

/// 复刻 dispatch_resume_alerts（只处理 Resume）
fn dispatch_resume_alerts(waiters: &mut Vec<String>, alerts: &[Alert]) -> usize {
    if alerts.is_empty() {
        return 0;
    }
    if waiters.is_empty() {
        return 0;
    }
    let mut dispatched = 0;
    for a in alerts {
        if a.kind != AlertKind::Resume || a.ih.is_empty() {
            continue; // ← State/Finished、State/Error 在此被跳过
        }
        if waiters.contains(&a.ih) {
            dispatched += 1;
        }
    }
    dispatched
}

/// 模拟 save_fastresume_impl 的一轮：poll_ring=true 时消费整个 ring
/// 返回：(被消费的 alert 总数, 被有效分发的数, 被丢弃的 State 类数)
fn one_round_poll_ring(alerts: Vec<Alert>, waiters: &mut Vec<String>) -> (usize, usize, usize) {
    let total = alerts.len();
    let dispatched = dispatch_resume_alerts(waiters, &alerts);

    // 修复后的循环体：只处理本任务的 Resume + take_resume_data
    let mut handled = 0;
    for a in &alerts {
        if a.kind == AlertKind::Resume && a.ih == "my-task" {
            handled += 1;
        }
    }

    // 被丢弃的 State 类 alert（既没分发、也不是本任务的 Resume）
    let state_dropped = alerts
        .iter()
        .filter(|a| a.kind == AlertKind::State)
        .count();

    let _ = handled;
    (total, dispatched, state_dropped)
}

#[cfg(test)]
mod round17 {
    use super::*;

    fn mk(kind: AlertKind, ih: &str, sub: StateSubKind) -> Alert {
        Alert { kind, ih: ih.to_string(), sub }
    }

    #[test]
    fn poll_ring_still_drops_state_alerts() {
        println!("=== P0-3 poll_ring 修复的残留缺陷 ===\n");

        println!("[场景] poll_ring = true（常驻循环尚未就位）");
        println!("       save_fastresume_impl 消费整个 ring\n");

        // ring 里有：本任务的 Resume + 其它任务的 Finished/Error
        let ring = vec![
            mk(AlertKind::Resume, "my-task", StateSubKind::Other),
            mk(AlertKind::State, "other-task-1", StateSubKind::Finished),
            mk(AlertKind::State, "other-task-2", StateSubKind::Error),
            mk(AlertKind::Resume, "other-task-3", StateSubKind::Other),
        ];

        let mut waiters = vec!["my-task".to_string(), "other-task-3".to_string()];
        let (total, dispatched, state_dropped) = one_round_poll_ring(ring.clone(), &mut waiters);

        println!("  ring 内容:");
        for a in &ring {
            println!("    {:?} ih={} sub={:?}", a.kind, a.ih, a.sub);
        }
        println!();
        println!("  pop_alerts 消费了: {total} 条");
        println!("  dispatch_resume_alerts 分发: {dispatched} 条（仅 Resume 类）");
        println!("  ❌ 被丢弃的 State 类: {state_dropped} 条");

        assert_eq!(total, 4);
        assert_eq!(state_dropped, 2, "State 类 alert 被丢弃"); 

        println!();
        println!("  → other-task-1 的 Finished 被丢 → 该任务永不标 Completed");
        println!("  → other-task-2 的 Error 被丢   → 该任务永不标 Failed");
        println!("  → 两者**永久卡在 Downloading**");

        println!("\n=== batch10 修复的实际效果 ===\n");
        println!("  修复前：poll_ring 循环外采样一次 → 整个 3s 窗口内都自 poll");
        println!("          → 3 秒内到达的所有非 Resume alert 全被吞");
        println!("  修复后：每轮重采样 → 常驻循环一置位就立刻让位");
        println!("          → 窗口缩短为「常驻循环启动前」");
        println!();
        println!("  ✅ 窗口显著缩小（从固定 3s → 启动延迟，通常毫秒级）");
        println!("  ❌ 但根因未消除：poll_ring 模式下仍只分发 Resume");

        println!("\n=== 根因 ===\n");
        println!("  save_fastresume_impl 在 poll_ring 模式下**消费整个 ring**，");
        println!("  但只有两类 alert 被有效处理：");
        println!("    1. 本任务的 Resume（take_resume_data）");
        println!("    2. 所有任务的 Resume（dispatch 给 waiters）");
        println!("  其余（State/Finished、State/Error、Tracker 等）直接丢弃。");

        println!("\n=== 影响评估 ===\n");
        println!("  触发条件：");
        println!("    · 常驻 alert 循环尚未启动（daemon 刚启动 / 引擎刚创建）");
        println!("    · 此刻恰好有 pause/remove 调用 save_fastresume_impl");
        println!("    · 且此时 ring 里有其它任务的终态 alert");
        println!();
        println!("  概率：低（需要时序巧合），但后果严重（任务永久卡死）。");
        println!("  缓解：batch10 已把窗口从 3s 缩到启动延迟。");
        println!("  定性：**P2 残留缺陷**（非 batch10 引入，是 P0-3 修复未覆盖全）。");

        println!("\n=== 建议修法 ===\n");
        println!("  poll_ring 模式下，pop 到的非 Resume alert 不应丢弃，而应：");
        println!("    a) 回填到等待常驻循环的队列（最干净，但需新增结构）");
        println!("    b) 或在 poll_ring 模式下只 pop 到本任务相关的 alert");
        println!("       （需要 BtCore 提供按 ih 过滤的 pop）");
        println!("    c) 或最简：poll_ring 模式下把 State/Finished 与 State/Error");
        println!("       也投递到统一的 task-state 通道（与 bt_events 共用）");
    }
}
