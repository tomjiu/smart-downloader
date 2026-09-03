//! E10: 事件历史缓冲（WsHub 非破坏读）——read_after 游标分页（多消费者
//! 安全，drain 破坏语义并存且互不影响）、read_filtered 过滤分页（limit =
//! 过滤后条数 + has_more）、gap_after 缺口检测（头部冲掉/恰好续上/空缓冲
//! 三情形统一公式）、type_label 与 known_event_type_labels 防漂移锁定。

use smart_dl_daemon::events::{known_event_type_labels, Envelope, SchedulerEvent};
use smart_dl_daemon::ws::WsHub;
use std::collections::HashSet;

fn progress(task: &str, done: u64) -> SchedulerEvent {
    SchedulerEvent::Progress {
        task_id: task.into(),
        done,
        total: 1000,
    }
}

fn created(task: &str) -> SchedulerEvent {
    SchedulerEvent::TaskCreated {
        task_id: task.into(),
    }
}

fn seqs(envs: &[Envelope]) -> Vec<u64> {
    envs.iter().map(|e| e.seq).collect()
}

#[test]
fn read_after_pages_without_consuming() {
    let hub = WsHub::new();
    for i in 1..=5 {
        hub.publish(progress("t", i));
    }
    // 分页读取：页 1 [1,2]、页 2 [3,4]、页 3 [5]
    assert_eq!(seqs(&hub.read_after(0, 2)), vec![1, 2]);
    assert_eq!(seqs(&hub.read_after(2, 2)), vec![3, 4]);
    assert_eq!(seqs(&hub.read_after(4, 2)), vec![5]);
    // 非破坏：缓冲原样保留（与 drain 的破坏语义相反）
    assert_eq!(hub.len(), 5);
    assert_eq!(hub.oldest_seq(), Some(1));
    // after 越过全部事件 → 空页
    assert!(hub.read_after(5, 10).is_empty());
    // drain（破坏）与非破坏读互不影响：drain 清空后 read_after 为空
    assert_eq!(hub.drain().len(), 5);
    assert!(hub.read_after(0, 10).is_empty());
}

#[test]
fn read_filtered_limits_by_matched_count_and_reports_has_more() {
    let hub = WsHub::new();
    // 6 条全匹配 Progress + 2 条不匹配 TaskCreated（交错）
    for i in 1..=6 {
        hub.publish(progress("t", i));
        if i <= 2 {
            hub.publish(created("other"));
        }
    }
    assert_eq!(hub.len(), 8);
    let all = hub.read_filtered(0, 100, |e| {
        matches!(e.event, SchedulerEvent::Progress { .. })
    });
    assert_eq!(all.0.len(), 6);
    assert!(!all.1, "命中数未超 limit → 无下一页");
    // limit=4 → 页内 4 条 + has_more（过滤在分页前生效：limit 语义 = 过滤后条数）
    let (page, has_more) =
        hub.read_filtered(0, 4, |e| matches!(e.event, SchedulerEvent::Progress { .. }));
    // publish 序：i=1..=2 各跟一条 created → Progress 落在 seq 1,3,5,6,7,8
    assert_eq!(seqs(&page), vec![1, 3, 5, 6]);
    assert!(has_more);
    // 游标续拉：after = 页内最大 seq → 下一页拿到剩余 2 条
    let cursor = page.last().unwrap().seq;
    let (rest, more) = hub.read_filtered(cursor, 4, |e| {
        matches!(e.event, SchedulerEvent::Progress { .. })
    });
    assert_eq!(rest.len(), 2);
    assert!(!more);
    // task_id 谓词：ProviderStatus 类无 task_id 的事件被自然排除（此处用
    // 交错的两任务模拟）
    let (t1_only, _) = hub.read_filtered(0, 100, |e| e.event.task_id() == Some("other"));
    assert_eq!(t1_only.len(), 2);
}

#[test]
fn gap_after_detects_evicted_head_exact_resume_and_empty_buffer() {
    // 头部冲掉：cap=4 全非关键 → publish 6 后缓冲 [3..6]
    let hub = WsHub::with_capacity(4);
    for i in 1..=6 {
        hub.publish(progress("t", i));
    }
    assert_eq!(hub.oldest_seq(), Some(3));
    assert!(hub.gap_after(1), "客户端在 seq1：seq2 已被冲掉 → 缺口");
    assert!(
        !hub.gap_after(2),
        "客户端在 seq2：缓冲恰好从 3 续上 → 无缺口"
    );
    assert!(!hub.gap_after(4), "客户端在缓冲内部 → 无缺口");
    // 空缓冲：front 取 last_seq+1 → after < last_seq 即缺口
    let empty = WsHub::new();
    assert!(!empty.gap_after(0), "从未发布过任何事件 → 无缺口");
    empty.publish(created("t"));
    assert_eq!(empty.last_seq(), 1);
    empty.drain(); // 清空缓冲，last_seq 仍为 1
    assert!(empty.gap_after(0), "缓冲空 + 客户端在 0：seq1 已丢 → 缺口");
    assert!(!empty.gap_after(1), "客户端已见全部 → 无缺口");
}

#[test]
fn type_labels_cover_all_variants_and_match_serde_tag() {
    let known = known_event_type_labels();
    // 全变体实例 → type_label 必须落在 known 集合内（漏变体在 match 无通配臂
    // 下由编译期拦截；此处锁定运行时映射与全集一致 + known 无重复）
    let all_variants = vec![
        created("t"),
        SchedulerEvent::StateChanged {
            task_id: "t".into(),
            from: smart_dl_core::state_machine::TaskState::Queued,
            to: smart_dl_core::state_machine::TaskState::Paused,
        },
        progress("t", 1),
        SchedulerEvent::Speed {
            task_id: "t".into(),
            down_rate: 1,
            up_rate: 0,
        },
        SchedulerEvent::Error {
            task_id: "t".into(),
            message: "e".into(),
        },
        SchedulerEvent::Completed {
            task_id: "t".into(),
        },
        SchedulerEvent::Failed {
            task_id: "t".into(),
            reason: "r".into(),
        },
        SchedulerEvent::DuplicateRejected {
            task_id: "t".into(),
            existing: "x".into(),
        },
    ];
    let mut seen = HashSet::new();
    for ev in &all_variants {
        let label = ev.type_label();
        assert!(
            known.iter().any(|k| k == label),
            "type_label {label} 未收录进 known_event_type_labels"
        );
        assert!(seen.insert(label), "known 内标签重复: {label}");
    }
    // HealthEvent/ProviderStatus 需额外构造（类型来自其他 crate，单独覆盖）
    let extra = vec![
        SchedulerEvent::HealthEvent {
            task_id: "t".into(),
            kind: smart_dl_daemon::health::HealthEventKind::LeechDetected,
        },
        SchedulerEvent::ProviderStatus {
            provider: "quark".into(),
            runtime: smart_dl_provider::ProviderRuntime {
                enabled: false,
                authenticated: false,
                quota_remaining: 0,
                concurrency_limit: 2,
                busy: 0,
                backoff_until: None,
                last_error: None,
            },
        },
        // E16：daemon 级事件（无 task_id）
        SchedulerEvent::GlobalLimitsChanged {
            max_download_kb_s: 1024,
            max_upload_kb_s: 512,
        },
        // E23：定时任务到点激活
        SchedulerEvent::TaskActivated {
            task_id: "t".into(),
        },
    ];
    for ev in &extra {
        let label = ev.type_label();
        assert!(
            known.iter().any(|k| k == label),
            "type_label {label} 未收录进 known_event_type_labels"
        );
        assert!(seen.insert(label), "known 内标签重复: {label}");
    }
    // 全集长度 = 8 常规 + 4 特殊构造（E16 GlobalLimitsChanged + E23 TaskActivated）
    assert_eq!(known.len(), 12);
}
