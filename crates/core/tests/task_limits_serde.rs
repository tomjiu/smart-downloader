//! 任务级限速（TaskLimits）serde 兼容：
//! ① 旧 tasks.json（无 limits 键）反序列化 → None（不破坏存量持久化文件）；
//! ② 有 limits 时 roundtrip 保真；
//! ③ limits 为 None 时序列化不出键（快照/持久化零噪声）。

mod common;

use common::make_task;
use smart_dl_core::task::{DownloadTask, TaskLimits};

#[test]
fn old_json_without_limits_field_deserializes_to_none() {
    // make_task 序列化（limits=None → 不出键）再人工补一层"旧版 JSON"等价性：
    // 旧版持久化文件根本没有 limits 字段 —— 反序列化必须容错为 None。
    let task: DownloadTask = make_task("t-old", "legacy");
    let json = serde_json::to_string(&task).unwrap();
    assert!(!json.contains("\"limits\""), "limits=None 时不得序列化出键");
    let back: DownloadTask = serde_json::from_str(&json).unwrap();
    assert!(back.limits.is_none(), "旧格式（无键）→ None");
}

#[test]
fn limits_roundtrip_preserves_both_directions() {
    let mut task: DownloadTask = make_task("t-limit", "limited");
    task.limits = Some(TaskLimits {
        down_kb_s: Some(512),
        up_kb_s: Some(64),
    });
    let json = serde_json::to_string(&task).unwrap();
    let back: DownloadTask = serde_json::from_str(&json).unwrap();
    assert_eq!(
        back.limits,
        Some(TaskLimits {
            down_kb_s: Some(512),
            up_kb_s: Some(64),
        })
    );
}

#[test]
fn explicit_zero_and_unset_directions_survive() {
    // 显式 0（不限）与未设方向混合时字段不丢失
    let limits = TaskLimits {
        down_kb_s: Some(0),
        up_kb_s: None,
    };
    assert!(!limits.is_empty(), "down=Some(0) 是显式配置，不算空");
    let json = serde_json::to_string(&limits).unwrap();
    let back: TaskLimits = serde_json::from_str(&json).unwrap();
    assert_eq!(back.down_kb_s, Some(0));
    assert_eq!(back.up_kb_s, None);
    assert!(TaskLimits::default().is_empty(), "全空才是 empty");
}

#[test]
fn empty_limits_do_not_leak_into_json() {
    // 全 None 的 TaskLimits 序列化成 {}（skip_serializing_if 兜底）
    let limits = TaskLimits::default();
    let json = serde_json::to_string(&limits).unwrap();
    assert_eq!(json, "{}", "空 limits 序列化应为空对象: {json}");
}
