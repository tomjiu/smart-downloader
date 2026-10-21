//! rss.json 并发落盘写序回归（Issue #121 B-5 修复的判别锚）。
//!
//! 缺陷（P1-9 引入）：rss_save_snapshot 落盘用调用方在 rss_state 锁内
//! 克隆的**旧快照**——并发 add 时 T1{A} 的落盘可能晚于 T2{A,B}，
//! 旧快照重排覆盖 → B 丢失更新（tasks.json 的 P1-1 同款缺陷翻版）。
//!
//! 修复：rss_persist_lock 临界区内**重新快照**（写入序 = 拿锁序，
//! 每次写 = 拿锁时刻最新状态）。
//!
//! 判别策略（REALTEST-PLAN 原则 4 的确定性形态）：实机并发交错窗口
//! 极窄（锁内入列串行 + 落盘毫秒级），集成层 b2_5 守恒断言难以稳定
//! 复现旧缺陷；本文件用**受控交错**直接构造「T2 先落盘、T1 后落盘」
//! 的最坏序列，证明旧实现必丢、新实现必胜。

use super::*;
use std::sync::Arc;

fn feed(id: u64, url: &str) -> crate::rss::RssFeed {
    crate::rss::RssFeed {
        id,
        url: url.to_string(),
        title: format!("t{id}"),
        added_at_unix: 0,
        last_refresh_unix: None,
        interval_override_secs: 0,
        items: vec![],
    }
}

/// 最坏交错（确定性复现旧缺陷的判别锚）：
/// T1 锁内入列 A + 快照 S1 →（未落盘）→ T2 锁内入列 B + 快照 S2 →
/// T2 落盘 S2 → T1 落盘 S1（旧快照后写覆盖）。
/// 旧实现：文件 = {A}（B 丢失）；新实现：T1 落盘时锁内重取 = {A,B}。
#[test]
fn rss_stale_snapshot_last_write_wins_with_lock() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = Arc::new(DaemonState::new(fake, vec![]).with_storage(store.clone()));

    // —— 受控交错（手动展开两个并发 add 的落盘段）——
    // T1：锁内入列 A，拿到旧快照 S1
    let s1 = {
        let mut st = state.rss_state().lock();
        st.feeds.push(feed(1, "http://x/1"));
        st.clone()
    };
    // T2：锁内入列 B，快照 S2（含 A+B），先完成落盘
    let s2 = {
        let mut st = state.rss_state().lock();
        st.feeds.push(feed(2, "http://x/2"));
        st.clone()
    };
    state.rss_save_snapshot(&s2); // T2 落盘（此刻文件 = {A,B}）

    // T1 的旧快照 S1 后到——旧实现直接写 S1 → B 丢失；新实现锁内重取
    state.rss_save_snapshot(&s1); // T1 落盘（最坏序：最后写）

    // 断言（用户可见效果）：重启恢复的数据源 rss.json 必须含全部变更
    let rss_path = dir.path().join("rss.json");
    let text = std::fs::read_to_string(&rss_path).expect("rss.json 必须已落盘");
    let v: serde_json::Value = serde_json::from_str(&text).expect("rss.json 必须可解析");
    let urls: Vec<&str> = v["feeds"]
        .as_array()
        .map(|a| a.iter().filter_map(|f| f["url"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(
        urls,
        vec!["http://x/1", "http://x/2"],
        "旧快照后写覆盖：B 丢失更新（文件 {urls:?}）—— rss_persist_lock 重快照未生效"
    );
}

/// 并发压力形态（补充锚）：多线程同时落盘不同代快照，最终文件必须
/// 反映最后一次拿锁时刻的最新状态（= 全量）。
#[test]
fn rss_concurrent_saves_converge_to_latest_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = Arc::new(DaemonState::new(fake, vec![]).with_storage(store.clone()));

    // 预置全部 feed（锁内一次放好）
    {
        let mut st = state.rss_state().lock();
        for i in 1..=8u64 {
            st.feeds.push(feed(i, &format!("http://x/{i}")));
        }
    }

    // 8 线程各持**不同年代的旧快照**并发落盘（年代越旧越可能后写）
    let snapshots: Vec<_> = (1..=8u64)
        .map(|gen| {
            let st = state.rss_state().lock();
            let mut snap = st.clone();
            snap.feeds.truncate(gen as usize); // 年代 g 的旧快照：只含前 g 个
            snap
        })
        .collect();
    let mut handles = Vec::new();
    for snap in snapshots {
        let s = state.clone();
        handles.push(std::thread::spawn(move || {
            s.rss_save_snapshot(&snap);
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    // 无论线程调度序如何，最终文件必须收敛到最新状态（8 个全量）
    let rss_path = dir.path().join("rss.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&rss_path).unwrap()).unwrap();
    let n = v["feeds"].as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(
        n, 8,
        "并发落盘后 rss.json 收敛失败（{n}/8）—— 某个旧快照成为了最终态"
    );
}
