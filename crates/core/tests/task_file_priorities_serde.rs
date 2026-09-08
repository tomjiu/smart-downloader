//! BT 子文件优先级持久化字段（DownloadTask.file_priorities）serde 兼容：
//! ① 旧 tasks.json（无 file_priorities 键）反序列化 → None（不破坏存量文件）；
//! ② Some 全量表 roundtrip 保真（下标 = 文件序）；
//! ③ None 时序列化不出键（持久化零噪声）。

mod common;

use common::make_task;
use smart_dl_core::task::DownloadTask;

#[test]
fn old_json_without_file_priorities_field_deserializes_to_none() {
    let task: DownloadTask = make_task("t-old", "legacy");
    let json = serde_json::to_string(&task).unwrap();
    assert!(
        !json.contains("\"file_priorities\""),
        "file_priorities=None 时不得序列化出键: {json}"
    );
    let back: DownloadTask = serde_json::from_str(&json).unwrap();
    assert!(back.file_priorities.is_none(), "旧格式（无键）→ None");
}

#[test]
fn file_priorities_roundtrip_preserves_full_table() {
    // 全量优先级表（下标 = 文件序，0..=7）：roundtrip 必须逐位保真
    let mut task: DownloadTask = make_task("t-prio", "multi-file");
    task.file_priorities = Some(vec![0, 1, 4, 7, 4]);
    let json = serde_json::to_string(&task).unwrap();
    assert!(json.contains("\"file_priorities\":[0,1,4,7,4]"), "{json}");
    let back: DownloadTask = serde_json::from_str(&json).unwrap();
    assert_eq!(back.file_priorities, Some(vec![0, 1, 4, 7, 4]));
}

#[test]
fn set_then_none_again_keeps_json_clean() {
    // 曾设置（Some）→ 序列化出键；清回 None → 键消失（覆盖重放/清理路径的序列化面）
    let mut task: DownloadTask = make_task("t-prio2", "toggle");
    task.file_priorities = Some(vec![0]);
    let json = serde_json::to_string(&task).unwrap();
    assert!(json.contains("\"file_priorities\""), "{json}");
    task.file_priorities = None;
    let json = serde_json::to_string(&task).unwrap();
    assert!(!json.contains("\"file_priorities\""), "{json}");
}
