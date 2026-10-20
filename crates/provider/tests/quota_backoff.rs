//! M5: 配额/backoff/认证 —— quota=0 或 backoff 中 → Router 不选该 Provider；
//! quota 耗尽注入；并发上限。

mod common;

use smart_dl_core::ownership::FallbackPolicy;
use smart_dl_core::types::DownloadSource;
use smart_dl_provider::{FallbackCoordinator, MockProvider, ProviderError, RemoteProvider};
use std::sync::Arc;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[tokio::test]
async fn quota_zero_provider_not_selected() {
    let p = MockProvider::new("noquota").with_quota(0);
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], FallbackPolicy::default());
    assert_eq!(coord.select_provider(), None, "quota=0 不得被选中");
}

#[tokio::test]
async fn backoff_provider_not_selected() {
    let p = MockProvider::new("backoff").with_backoff(now_unix() + 3600);
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], FallbackPolicy::default());
    assert_eq!(coord.select_provider(), None, "backoff 中不得被选中");
}

#[tokio::test]
async fn unauthenticated_provider_not_selected() {
    let p = MockProvider::new("noauth").unauthenticated();
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], FallbackPolicy::default());
    assert_eq!(coord.select_provider(), None, "未认证不得被选中");
}

#[tokio::test]
async fn disabled_provider_not_selected() {
    let p = MockProvider::new("off").disabled();
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], FallbackPolicy::default());
    assert_eq!(coord.select_provider(), None, "disabled 不得被选中");
}

#[tokio::test]
async fn healthy_provider_is_selected() {
    let p = MockProvider::new("ok").with_quota(5);
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], FallbackPolicy::default());
    assert_eq!(coord.select_provider().as_deref(), Some("ok"));
}

#[tokio::test]
async fn quota_exhausted_submit_fails() {
    let p = MockProvider::new("exhaust").with_quota(0);
    let r = p.submit(&DownloadSource::Magnet("m".into())).await;
    assert!(
        matches!(r, Err(ProviderError::Quota)),
        "quota=0 submit 必须失败"
    );
}

#[tokio::test]
async fn concurrency_full_blocks_selection() {
    let p = MockProvider::new("busy").with_concurrency(2);
    p.set_busy(2);
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], FallbackPolicy::default());
    assert_eq!(
        coord.select_provider(),
        None,
        "并发满（busy==limit）不得被选中"
    );
}

#[tokio::test]
async fn refresh_auth_restores_authentication() {
    let p = MockProvider::new("reauth").unauthenticated();
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], FallbackPolicy::default());
    assert_eq!(coord.select_provider(), None);
    coord.refresh_auth_all().await;
    assert_eq!(
        coord.select_provider().as_deref(),
        Some("reauth"),
        "refresh_auth 后可选"
    );
}

#[tokio::test]
async fn runtime_default_and_validity_helper() {
    // ProviderRuntime::default 构造 + files_all_valid 语义（覆盖类型层缺测分支）
    let rt: smart_dl_provider::ProviderRuntime = Default::default();
    assert!(rt.enabled && rt.authenticated);
    assert_eq!(rt.concurrency_limit, 2);
    let now = now_unix();
    let good = smart_dl_provider::ResolvedRemoteFile {
        rel_path: "a".into(),
        url: "http://x/a".into(),
        size: 1,
        etag: None,
        expires_at: None,
    };
    let expired_file = smart_dl_provider::ResolvedRemoteFile {
        expires_at: Some(now - 10),
        ..good.clone()
    };
    assert!(smart_dl_provider::coordinator::files_all_valid(
        &[good],
        now
    ));
    assert!(!smart_dl_provider::coordinator::files_all_valid(
        &[expired_file],
        now
    ));
}

/// P1-17 回归：busy 槽全生命周期配对——兜底编排进行中（传输挂起）并发槽
/// 被占用（limit=1 时选择闸门关闭），编排结束（成功/失败路径，RAII Drop）
/// 释放后闸门恢复。原实现 busy 恒 0，「每 provider 并发上限」从未生效。
#[tokio::test]
async fn busy_slot_paired_across_fallback_lifecycle() {
    use std::sync::atomic::{AtomicBool, Ordering};

    /// 阻塞 sink：transfer 挂起直到放行信号（保住 busy 槽制造观察窗口）。
    struct GatedSink {
        release: Arc<AtomicBool>,
        entered: Arc<AtomicBool>,
    }
    #[async_trait::async_trait]
    impl smart_dl_provider::HttpSink for GatedSink {
        async fn transfer(
            &self,
            _id: &str,
            _url: &str,
            _dest: std::path::PathBuf,
            _name: Option<String>,
        ) -> Result<(), smart_dl_provider::SinkError> {
            self.entered.store(true, Ordering::SeqCst);
            while !self.release.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
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

    let release = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(AtomicBool::new(false));
    let sink = GatedSink {
        release: release.clone(),
        entered: entered.clone(),
    };

    // 并发上限 1：单个在途兜底即关闸；files 必须非空（resolve 空表不会
    // 进入 transfer，busy 槽观察窗无从建立）
    let p = MockProvider::new("busygate")
        .with_concurrency(1)
        .with_files(vec![smart_dl_provider::ResolvedRemoteFile {
            rel_path: "a.bin".into(),
            url: "http://127.0.0.1:1/a.bin".into(),
            size: 1,
            etag: None,
            expires_at: None,
        }]);
    let coord = FallbackCoordinator::new(vec![Arc::new(p)], FallbackPolicy::default());
    assert_eq!(coord.select_provider().as_deref(), Some("busygate"));

    let task = smart_dl_core::task::DownloadTask {
        id: "bg1".to_string(),
        canonical_id: smart_dl_core::identity::CanonicalId {
            kind: smart_dl_core::identity::CanonicalKind::Bt,
            identity: "btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            validator: None,
            token_sensitive: false,
        },
        source: DownloadSource::Magnet(
            "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ),
        identity: smart_dl_core::identity::ContentIdentity::SingleFile {
            size: 0,
            etag: None,
            sha256: None,
            sha1: None,
            md5: None,
            backup_md5: None,
        },
        dest_root: std::path::PathBuf::from("/tmp/sd-busy-gate-test"),
        files: vec![],
        acquisitions: vec![],
        aggregate: Default::default(),
        state: smart_dl_core::state_machine::TaskState::Queued,
        retry: Default::default(),
        created_at: std::time::Instant::now(),
        file_priorities: None,
        sequential: false,
        metadata: smart_dl_core::task::TaskMetadata {
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
        queue_priority: 0,
    };

    // 兜底编排转入后台（transfer 挂起持有 busy 槽）
    let coord_arc = std::sync::Arc::new(coord);
    let coord2 = coord_arc.clone();
    let task2 = task.clone(); // 第二兜底尝试用（预期在闸门处被拒）
    let sink2 = GatedSink {
        release: release.clone(),
        entered: entered.clone(),
    };
    let handle = tokio::spawn(async move {
        coord2
            .begin_fallback(&task, 0.0, true, &sink)
            .await
            .expect("mock 全链兜底应成功")
    });

    // 等待 transfer 进入（busy 槽已占）
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !entered.load(Ordering::SeqCst) {
        assert!(std::time::Instant::now() < deadline, "transfer 未进入");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // 在途占用中（coordinator 计数 1/1）：第二个兜底编排不得再打进同一
    // provider——选择后获取并发槽失败 → 试尽 → NoProvider（闸门真正生效；
    // 原实现 busy 恒 0，100 个并发兜底可同时打进同一 provider）。
    let coord3 = coord_arc.clone();
    let second =
        tokio::spawn(async move { coord3.begin_fallback(&task2, 0.0, true, &sink2).await });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match second.is_finished() {
            true => break,
            false if std::time::Instant::now() > deadline => {
                panic!("第二个兜底应在闸门处快速返回 NoProvider，而非挂起")
            }
            false => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
        }
    }
    let second_result = second.await.expect("join");
    assert!(
        matches!(
            second_result,
            Err(smart_dl_provider::ProviderError::NoProvider)
        ),
        "并发满（limit=1 在途）时第二兜底必须 NoProvider，得到 {second_result:?}"
    );

    // 放行 → 编排完成 → 槽释放 → 新兜底可再次获取
    release.store(true, Ordering::SeqCst);
    let outcome = handle.await.expect("join");
    assert_eq!(outcome.provider, "busygate");
    assert_eq!(
        coord_arc.select_provider().as_deref(),
        Some("busygate"),
        "退出编排后并发槽必须释放（RAII Drop）"
    );
}
