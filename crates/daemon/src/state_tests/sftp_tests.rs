//! C-S1：SFTP 路由（feature `sftp`）单测——add_sftp_task 前缀/user 校验 /
//! auth 提取 / 路由 Sftp 引擎 / canonical 查重（与 ftp:// 键不相撞）。
//! 协议层 e2e 由 httpdl sftp.rs hermetic 服务器覆盖（russh 双端），此处不再
//! 起真实 SSH 服务。
#![cfg(all(test, feature = "sftp"))]

use super::*;

// ---- 单元：add_sftp_task 基础路由 ----

#[tokio::test]
async fn non_sftp_url_is_invalid() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Sftp));
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
    let err = state
        .add_sftp_task("ftp://host/f.bin".into(), None)
        .await
        .expect_err("非 sftp:// 前缀应拒绝");
    assert!(
        matches!(err, DaemonError::InvalidSource(_)),
        "应返回 InvalidSource: {err}"
    );
    assert!(fake.added().is_empty(), "不应路由到引擎");
}

#[tokio::test]
async fn missing_user_is_invalid() {
    // SSH 无匿名惯例：无 user → 明确报错（与 ftp anonymous 回退刻意不同）
    let fake = Arc::new(FakeEngine::new(EngineKind::Sftp));
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
    let err = state
        .add_sftp_task("sftp://host/f.bin".into(), None)
        .await
        .expect_err("缺 user 应拒绝");
    assert!(
        matches!(err, DaemonError::InvalidSource(_)),
        "应返回 InvalidSource: {err}"
    );
    assert!(fake.added().is_empty(), "不应路由到引擎");
}

#[tokio::test]
async fn extracts_auth_and_routes_to_sftp_engine() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Sftp));
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
    let url = "sftp://alice:secret@host:2222/data/a.bin".to_string();
    let tid = state.add_sftp_task(url.clone(), None).await.unwrap();
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    assert_eq!(rec.engine_kind, EngineKind::Sftp, "engine_kind 记 Sftp");
    match &rec.task.source {
        DownloadSource::Sftp { url: u, user, pass } => {
            assert_eq!(u, &url);
            assert_eq!(user, "alice", "parse_sftp_auth 应提取 user");
            assert_eq!(pass, "secret", "parse_sftp_auth 应提取 pass");
        }
        other => panic!("source 应为 DownloadSource::Sftp: {other:?}"),
    }
    assert_eq!(fake.added(), vec![tid], "应路由到 Sftp 引擎 add");
    assert_eq!(
        rec.task.metadata.name.as_deref(),
        Some("a.bin"),
        "单文件任务落盘名取 URL 最后一段"
    );
}

#[tokio::test]
async fn sftp_dup_is_rejected() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Sftp));
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
    let url = "sftp://u:p@host/data.bin".to_string();
    let _ = state.add_sftp_task(url.clone(), None).await.unwrap();
    let err = state
        .add_sftp_task(url.clone(), None)
        .await
        .expect_err("重复 canonical 应拒绝");
    assert!(
        matches!(err, DaemonError::Duplicate(_)),
        "应返回 Duplicate: {err}"
    );
}

/// 需 ftp+sftp 双 feature 同时开启（跨引擎 canonical 区分断言）
#[cfg(feature = "ftp")]
#[tokio::test]
async fn sftp_and_ftp_urls_are_distinct_canonicals() {
    // 同 host/path 的 ftp:// 与 sftp:// 键不相撞（CanonicalKind 区分）
    let ftp_fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
    let sftp_fake = Arc::new(FakeEngine::new(EngineKind::Sftp));
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::new(ftp_fake.clone(), vec![])
        .with_dest_root(dir.path().to_path_buf())
        .with_ftp(ftp_fake.clone())
        .with_sftp(sftp_fake.clone());
    let _ = state
        .add_ftp_task("ftp://u:p@host/data.bin".into(), None)
        .await
        .unwrap();
    let _ = state
        .add_sftp_task("sftp://u:p@host/data.bin".into(), None)
        .await
        .unwrap();
    assert_eq!(ftp_fake.added().len(), 1, "ftp 任务路由 ftp 引擎");
    assert_eq!(sftp_fake.added().len(), 1, "sftp 任务路由 sftp 引擎");
}

#[tokio::test]
async fn sftp_task_serializes_roundtrip() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Sftp));
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
    let tid = state
        .add_sftp_task("sftp://u:p@host/data.bin".into(), None)
        .await
        .unwrap();
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    // 序列化往返：DownloadSource::Sftp + EngineKind::Sftp 持久化兼容
    let json = serde_json::to_vec(&rec.task).expect("task 可序列化");
    let _restored: DownloadTask = serde_json::from_slice(&json).expect("可反序列化");
}
