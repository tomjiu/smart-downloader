//! R5/R8 优先级收敛回归测试：set_task_file_priorities 的 verify-and-reappy。
//! 缺陷背景（2026-10 R5 多文件本地 swarm 实测）：
//!  ① libtorrent 异步记账——设后立即 readback 可能读到旧值，旧实现把旧值
//!    直接持久化 → 恢复重放时反噬用户 skip（数据面缺陷）；
//!  ② API 响应「当前快照」同一竞态，两跑分叉 [4,4,4]/[4,0,4]。
//! 修复语义：响应与持久化均取「请求合并后的期望值」（意图优先），漂移即
//! 全量重设收敛。prio_stale 模拟恒漂移引擎验证该语义。

#![cfg(all(test, feature = "bt"))]

use super::*;
use std::sync::Arc;

/// BT 下载态任务记录（engine_tid=ih）。
fn downloading_rec(id: &str, ih: &str) -> TaskRecord {
    let mut rec = TaskRecord {
        task: DownloadTask {
            id: id.into(),
            canonical_id: CanonicalId {
                kind: CanonicalKind::Bt,
                identity: ih.to_string(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Magnet(format!("magnet:?xt=urn:btih:{ih}")),
            identity: ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                sha1: None,
                md5: None,
                backup_md5: None,
            },
            dest_root: PathBuf::from("."),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Downloading(EngineKind::Bt),
            retry: RetryState {
                retries: 0,
                max_retries: 0,
            },
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential: false,
            metadata: TaskMetadata {
                name: None,
                added_at_unix: 0,
                added_at_ms: 0,
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: 0,
                next_retry_at_unix: 0,
            },
            limits: None,
            max_connections: None,
            max_uploads: None,
            queue_priority: 0,
        },
        engine_tid: Some(ih.to_string()),
        engine_kind: EngineKind::Bt,
        engine_status: None,
        seeding_since: None,
        events: vec![],
    };
    rec.push_event("add", None);
    rec
}

async fn setup() -> (Arc<DaemonState>, Arc<FakeEngine>) {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let state = Arc::new(
        DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![]).with_bt(fake.clone()),
    );
    state
        .tasks
        .lock()
        .insert("tDl".into(), downloading_rec("tDl", "abc"));
    (state, fake)
}

/// 缺陷①回归：引擎恒漂移（readback 永远旧值）时，响应与持久化必须取
/// 请求合并后的期望值，而非旧 readback。
#[tokio::test]
async fn stale_readback_persists_request_intent_not_stale_value() {
    let (state, fake) = setup().await;
    // 恒漂移：set 不更新 readback（模拟 libtorrent 异步记账不收敛的窗口）
    fake.set_prio_stale(true);

    let resp = state
        .set_task_file_priorities("tDl", &[(0, 0)])
        .await
        .expect("set 必须成功");
    // 响应 = 期望值 [Some(0)]，而非漂移的旧值 [Some(4)]
    assert_eq!(
        resp,
        vec![Some(0u32)],
        "响应必须反映请求意图（缺陷①：旧值曾直出）"
    );

    // 持久化记录 = 期望值，非旧 readback（防恢复重放反噬 skip）
    let rec = state.tasks.lock().get("tDl").cloned().expect("记录在");
    assert_eq!(
        rec.task.file_priorities,
        Some(vec![0u32]),
        "持久化必须取请求意图（缺陷①：旧值曾污染记录→重放反噬）"
    );

    // 漂移期间应发生多轮全量重设（verify-and-reappy 真的在干活）
    let calls = fake.prio_calls();
    assert!(
        calls.len() >= 3,
        "恒漂移下应有多轮重设（实际 {} 轮）",
        calls.len()
    );
    // 最后一轮重设必须是全量期望值
    let (_, last) = calls.last().expect("至少一轮重设");
    assert_eq!(last.as_slice(), &[(0, 0)], "重设值 = 期望值全量");
}

/// 正常路径：引擎反映 set（收敛语义 mock）→ 快速收敛，响应/持久化一致。
#[tokio::test]
async fn convergent_engine_fast_path() {
    let (state, fake) = setup().await;

    let resp = state
        .set_task_file_priorities("tDl", &[(0, 7)])
        .await
        .expect("set 必须成功");
    assert_eq!(resp, vec![Some(7u32)]);

    let rec = state.tasks.lock().get("tDl").cloned().expect("记录在");
    assert_eq!(rec.task.file_priorities, Some(vec![7u32]));

    // 收敛引擎：初始 set + 首轮 readback 命中后仍需第二轮确认（连续两轮）
    let calls = fake.prio_calls();
    assert!(
        calls.len() <= 3,
        "收敛引擎不应疯狂重设（实际 {} 轮）",
        calls.len()
    );
}

/// 多文件局部设置：未提及下标保持引擎现值（readback ∪ 请求语义）。
#[tokio::test]
async fn partial_set_merges_with_current() {
    let (state, fake) = setup().await;
    // 引擎现值 [4,4,4]（三文件默认）
    fake.set_prio_readback(Some(Ok(vec![Some(4), Some(4), Some(4)])));

    let resp = state
        .set_task_file_priorities("tDl", &[(1, 0)])
        .await
        .expect("set 必须成功");
    assert_eq!(
        resp,
        vec![Some(4u32), Some(0u32), Some(4u32)],
        "局部设置：未提及下标保持现值"
    );

    let rec = state.tasks.lock().get("tDl").cloned().expect("记录在");
    assert_eq!(rec.task.file_priorities, Some(vec![4, 0, 4]));
}
