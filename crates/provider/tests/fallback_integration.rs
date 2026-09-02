//! M5 关键集成：BT stall & <50% → pause BT 后启动 Provider → 完成；
//! **BT 半成品文件保留（硬性验收，F3 回归）**；allow_parallel_disk=false →
//! Provider 必须等 pause；BT ≥50% → 拒绝自动兜底。

mod common;

use common::{patterned, TestServer};
use smart_dl_core::ownership::{decide_auto_fallback, FallbackDecision, FallbackPolicy};
use smart_dl_core::state_machine::TaskState;
use smart_dl_core::types::DownloadSource;
use smart_dl_provider::{FallbackCoordinator, MockProvider, ProviderError};
use std::sync::Arc;

fn bt_partial_task(dest_root: std::path::PathBuf) -> smart_dl_core::task::DownloadTask {
    smart_dl_core::task::DownloadTask {
        id: "fb1".to_string(),
        canonical_id: smart_dl_core::identity::CanonicalId {
            kind: smart_dl_core::identity::CanonicalKind::Bt,
            identity: "btih:feedfacefeedfacefeedfacefeedfacefeedface".to_string(),
            validator: None,
            token_sensitive: false,
        },
        source: DownloadSource::Magnet(
            "magnet:?xt=urn:btih:feedfacefeedfacefeedfacefeedfacefeedface".to_string(),
        ),
        identity: smart_dl_core::identity::ContentIdentity::SingleFile {
            size: 0,
            etag: None,
            sha256: None,
            backup_md5: None,
        },
        dest_root,
        files: vec![],
        acquisitions: vec![],
        aggregate: Default::default(),
        state: TaskState::Queued,
        retry: Default::default(),
        created_at: std::time::Instant::now(),
        file_priorities: None,
        metadata: smart_dl_core::task::TaskMetadata {
            name: None,
            added_at_unix: 0,
        },
        limits: None,
    }
}

#[tokio::test]
async fn stall_bt_keeps_partial_file_after_provider_completes() {
    // 硬性验收（F3）：BT stall 且 <50% → 兜底完成 → BT 半成品文件仍在（未被删）
    let size = 64 * 1024;
    let body = patterned(size);
    let srv = TestServer::start(body.clone(), None).await;

    let dir = tempfile::tempdir().unwrap();
    // BT 半成品：dest_root 下已有 20KB 部分文件
    let bt_partial = dir.path().join("movie.bin.part");
    std::fs::write(&bt_partial, vec![0xBBu8; 20 * 1024]).unwrap();

    let files = vec![smart_dl_provider::ResolvedRemoteFile {
        rel_path: "movie.bin".to_string(),
        url: srv.url(),
        size,
        etag: None,
        expires_at: None,
    }];
    let p = MockProvider::new("cloud").with_files(files);
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], FallbackPolicy::default());

    // BT 进度 0.1 < 0.5 → 允许兜底（RequiresPauseFirst，测试先 pause）
    assert_eq!(coord.decide(0.1), FallbackDecision::RequiresPauseFirst);
    let outcome = coord
        .begin_fallback(
            &bt_partial_task(dir.path().to_path_buf()),
            0.1,
            true,
            &sink_noop(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.transferred.len(), 1);
    assert!(
        bt_partial.exists(),
        "BT 半成品文件必须保留（禁删，F3 硬性验收）"
    );
    // 兜底产物落位由 mock_lifecycle::full_flow_transfers_two_files_to_disk（真 HttpEngine）覆盖；
    // 此处 sink 为空实现，仅验证编排与 BT 半成品保留。
}

/// 空 sink：fallback_integration 只验证编排（transfer 直接 Ok）。
struct SinkNoop;

#[async_trait::async_trait]
impl smart_dl_provider::HttpSink for SinkNoop {
    async fn transfer(
        &self,
        _id: &str,
        _url: &str,
        _dest: std::path::PathBuf,
        _name: Option<String>,
    ) -> Result<(), smart_dl_provider::SinkError> {
        Ok(())
    }
    async fn update_sources(
        &self,
        _id: &str,
        _urls: Vec<String>,
    ) -> Result<(), smart_dl_provider::SinkError> {
        Ok(())
    }
}

fn sink_noop() -> SinkNoop {
    SinkNoop
}

#[tokio::test]
async fn parallel_disk_off_requires_pause_first() {
    // allow_parallel_disk=false → 决策 RequiresPauseFirst → 未 pause → 拒绝启动
    let p = MockProvider::new("serial");
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], FallbackPolicy::default());
    assert_eq!(
        coord.decide(0.3),
        FallbackDecision::RequiresPauseFirst,
        "默认策略禁双份占盘"
    );
    let r = coord
        .begin_fallback(
            &bt_partial_task(std::path::PathBuf::from(".")),
            0.3,
            false,
            &sink_noop(),
        )
        .await;
    assert!(
        matches!(r, Err(ProviderError::RequiresPause)),
        "未 pause 不得启动 Provider"
    );
}

#[tokio::test]
async fn over_half_progress_rejects_auto_fallback() {
    // BT ≥50% → 拒绝自动兜底（仅手动）
    let p = MockProvider::new("reject");
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], FallbackPolicy::default());
    assert_eq!(coord.decide(0.51), FallbackDecision::ManualOnly);
    let r = coord
        .begin_fallback(
            &bt_partial_task(std::path::PathBuf::from(".")),
            0.51,
            true,
            &sink_noop(),
        )
        .await;
    assert!(
        matches!(r, Err(ProviderError::ManualOnly)),
        "≥50% 拒绝自动兜底"
    );
}

#[tokio::test]
async fn policy_progress_boundary_is_frozen() {
    // 与 M2 冻结的决策函数一致（0.5 阈值 / 禁双份默认）
    let policy = FallbackPolicy::default();
    assert_eq!(
        decide_auto_fallback(0.499, &policy),
        FallbackDecision::RequiresPauseFirst
    );
    assert_eq!(
        decide_auto_fallback(0.5, &policy),
        FallbackDecision::ManualOnly
    );
}

#[tokio::test]
async fn no_provider_available_reports_error() {
    let coord = FallbackCoordinator::new(vec![], FallbackPolicy::default());
    assert_eq!(coord.select_provider(), None);
    let r = coord
        .begin_fallback(
            &bt_partial_task(std::path::PathBuf::from(".")),
            0.1,
            true,
            &sink_noop(),
        )
        .await;
    assert!(
        matches!(r, Err(ProviderError::NoProvider)),
        "无可用 Provider → 报错"
    );
}

#[tokio::test]
async fn provider_task_failed_reports_error() {
    // poll_ready 收到 Failed → begin_fallback 报错（ProviderStatus::Failed 分支）
    let p = MockProvider::new("boom");
    p.fail_next_submits();
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], FallbackPolicy::default());
    let r = coord
        .begin_fallback(
            &bt_partial_task(std::path::PathBuf::from(".")),
            0.1,
            true,
            &sink_noop(),
        )
        .await;
    assert!(
        matches!(r, Err(ProviderError::Other(_))),
        "任务 Failed → 兜底失败"
    );
}
