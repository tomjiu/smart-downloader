//! E24 多源并行下载：add 期双源身份门控（双强 ETag 相等 + Range/总长一致）
//! → mirrors 双源起步；worker 轮转分摊段（真并行而非单源挤兑）；
//! 门控不过（ETag 不一致/弱 ETag/备用源不可达）→ 单源语义零变化。
//! 内容安全核心：跨源混拼仅在服务器内容指纹一致证据下启用（严于 aria2）。

mod common;
mod integration;

use common::{make_http_task_to, wait_terminal};
use integration::http_server::{patterned, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::HttpEngine;

const MB: u64 = 1024 * 1024;

/// 32MB = 2 段（默认段粒度 16MB），2 worker：慢主源拖住一个 worker 时，
/// 另一个 worker 领取下一段必然落在快备用源 → 分摊可观测（range_starts）。
const SIZE: u64 = 32 * MB;

#[tokio::test]
async fn dual_source_activates_on_matching_strong_etag() {
    let primary = HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        patterned_content: true,
        etag: Some("\"same-v1\""),
        delay_ms: 250, // 慢源：拖住 w0 的段，w1 必然落到备用源
        ..Default::default()
    })
    .await;
    let backup = HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        patterned_content: true,
        etag: Some("\"same-v1\""),
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let mut task = make_http_task_to(
        "e24a",
        &primary.url("/file"),
        dir.path().to_path_buf(),
        Some("dual.bin"),
    );
    // 注入备用源（与主源同内容同 ETag）
    task.source = smart_dl_core::types::DownloadSource::Http {
        url: primary.url("/file"),
        headers: vec![],
        auth: None,
        backup_url: Some(backup.url("/file")),
        proxy: None,
    };

    engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, "e24a").await;
    assert!(
        matches!(st.state, EngineState::Completed),
        "双源任务应完成: {:?} {:?}",
        st.state,
        st.error
    );

    // 分摊证据：备用源除 add 期探测（bytes=0-0 → 起点 0）外，至少领到 1 个段
    //（慢主源拖住 w0；w1 的段起点 > 0）。若门控未生效（单源），备用源恒只有 1 次探测。
    let backup_starts = backup.range_starts.lock().clone();
    assert!(
        backup_starts.len() >= 2,
        "备用源应分摊到下载段（探测 + 段），实际: {backup_starts:?}"
    );
    assert!(
        backup_starts.iter().any(|&s| s > 0),
        "备用源应收到非探测段请求: {backup_starts:?}"
    );

    // 内容完整性：混拼结果 == 单源内容（跨源混拼安全性的端到端证明）
    let out = dir.path().join("dual.bin");
    assert_eq!(
        std::fs::read(&out).unwrap(),
        patterned(SIZE),
        "双源混拼结果必须与单源内容逐字节一致"
    );
}

#[tokio::test]
async fn gate_rejects_on_etag_mismatch() {
    let primary = HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        patterned_content: true,
        etag: Some("\"primary-v1\""),
        delay_ms: 0,
        ..Default::default()
    })
    .await;
    let backup = HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        patterned_content: true,
        etag: Some("\"different-content\""),
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let mut task = make_http_task_to(
        "e24b",
        &primary.url("/file"),
        dir.path().to_path_buf(),
        Some("mismatch.bin"),
    );
    task.source = smart_dl_core::types::DownloadSource::Http {
        url: primary.url("/file"),
        headers: vec![],
        auth: None,
        backup_url: Some(backup.url("/file")),
        proxy: None,
    };

    engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, "e24b").await;
    assert!(matches!(st.state, EngineState::Completed));

    // 门控拒绝：备用源只有 add 期探测（1 次），不参与下载
    let backup_starts = backup.range_starts.lock().clone();
    assert_eq!(
        backup_starts.len(),
        1,
        "ETag 不一致时备用源不得分摊段: {backup_starts:?}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("mismatch.bin")).unwrap(),
        patterned(SIZE)
    );
}

#[tokio::test]
async fn gate_rejects_on_weak_etag() {
    // 弱 ETag（W/ 前缀）：同一资源不同表示可同值，不可作混拼证据
    let primary = HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        patterned_content: true,
        etag: Some("W/\"weak-1\""),
        ..Default::default()
    })
    .await;
    let backup = HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        patterned_content: true,
        etag: Some("W/\"weak-1\""),
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let mut task = make_http_task_to(
        "e24c",
        &primary.url("/file"),
        dir.path().to_path_buf(),
        Some("weak.bin"),
    );
    task.source = smart_dl_core::types::DownloadSource::Http {
        url: primary.url("/file"),
        headers: vec![],
        auth: None,
        backup_url: Some(backup.url("/file")),
        proxy: None,
    };

    engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, "e24c").await;
    assert!(matches!(st.state, EngineState::Completed));
    let backup_starts = backup.range_starts.lock().clone();
    assert_eq!(
        backup_starts.len(),
        1,
        "弱 ETag 时备用源不得分摊段: {backup_starts:?}"
    );
}

#[tokio::test]
async fn gate_rejects_on_backup_unreachable() {
    let primary = HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        patterned_content: true,
        etag: Some("\"v1\""),
        ..Default::default()
    })
    .await;
    // 备用源地址：占一个端口后立即释放（connect refused）
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = listener.local_addr().unwrap();
    drop(listener);

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let mut task = make_http_task_to(
        "e24d",
        &primary.url("/file"),
        dir.path().to_path_buf(),
        Some("deadbk.bin"),
    );
    task.source = smart_dl_core::types::DownloadSource::Http {
        url: primary.url("/file"),
        headers: vec![],
        auth: None,
        backup_url: Some(format!("http://{dead}/file")),
        proxy: None,
    };

    // 备用源探测失败不阻断 add（单源语义，与兑底路径的宽容一致）
    engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, "e24d").await;
    assert!(matches!(st.state, EngineState::Completed));
    assert_eq!(
        std::fs::read(dir.path().join("deadbk.bin")).unwrap(),
        patterned(SIZE)
    );
}

#[tokio::test]
async fn single_source_task_without_backup_unchanged() {
    // 回归锁：无 backup_url → 无备用探测，主源独自完成
    let primary = HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        patterned_content: true,
        etag: Some("\"v1\""),
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "e24e",
        &primary.url("/file"),
        dir.path().to_path_buf(),
        Some("solo.bin"),
    );
    engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, "e24e").await;
    assert!(matches!(st.state, EngineState::Completed));
    assert_eq!(
        std::fs::read(dir.path().join("solo.bin")).unwrap(),
        patterned(SIZE)
    );
}

/// 404 直链过期场景下的多源韧性：主源中途失效（第 2 段起 404）+ 双源同质
/// → 段级逐源回退仍完成（download_dynamic 既有语义在多源表下的回归锁）。
#[tokio::test]
async fn primary_dies_midway_backup_picks_up_segments() {
    let primary = HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        patterned_content: true,
        etag: Some("\"same-v1\""),
        fail_ranges: vec![16 * MB], // 第 2 段起点 → 404（模拟直链过期）
        ..Default::default()
    })
    .await;
    let backup = HttpTestServer::start(HttpServerConfig {
        size: SIZE,
        patterned_content: true,
        etag: Some("\"same-v1\""),
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let mut task = make_http_task_to(
        "e24f",
        &primary.url("/file"),
        dir.path().to_path_buf(),
        Some("failover.bin"),
    );
    task.source = smart_dl_core::types::DownloadSource::Http {
        url: primary.url("/file"),
        headers: vec![],
        auth: None,
        backup_url: Some(backup.url("/file")),
        proxy: None,
    };

    engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, "e24f").await;
    assert!(
        matches!(st.state, EngineState::Completed),
        "主源中途失效应被段级回退吸收: {:?} {:?}",
        st.state,
        st.error
    );
    // 失效段落在备用源完成（起点 16MB 的段请求出现在备用源）
    let backup_starts = backup.range_starts.lock().clone();
    assert!(
        backup_starts.contains(&(16 * MB)),
        "失效段应由备用源接管: {backup_starts:?}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("failover.bin")).unwrap(),
        patterned(SIZE)
    );
}
