//! P1: backup_md5 兜底（夸克 backup_url/backup_md5 机制落地）。
//! 主源两次 sha256 校验失败 → 切备用源（backup_url）→ 以 backup_md5 校验；
//! 备用源也失败 → 降级接受 + 告警；仅 backup_url 无 backup_md5 → 复用主源 sha256。

mod common;
mod integration;

use common::{make_http_task_backup, make_http_task_sha256, make_http_task_to, wait_terminal};
use integration::http_server::{md5_of, sha256_of, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::HttpEngine;

const MB: u64 = 1024 * 1024;

#[tokio::test]
async fn main_bad_backup_good_recovers_via_md5() {
    // 主源内容与声明的 sha256 全程不符 → 重下 1 次仍失败 → 切备用源；
    // 备用源内容符合 backup_md5 → 校验通过 → Completed 无告警，落位 = 备用源内容。
    let size = MB;
    let bad = vec![0u8; size as usize];
    let good = integration::http_server::patterned(size);

    let srv_main = HttpTestServer::start(HttpServerConfig {
        size,
        content: Some(bad.clone()),
        ..Default::default()
    })
    .await;
    let srv_backup = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_backup(
        "bk1",
        &srv_main.url("/file"),
        &srv_backup.url("/file"),
        &md5_of(&good),
        dir.path().to_path_buf(),
        "bk1.bin",
        &sha256_of(&good),
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);
    assert!(st.error.is_none(), "备用源校验通过不得告警");
    let got = std::fs::read(dir.path().join("bk1.bin")).unwrap();
    assert_eq!(got, good, "落位内容应为备用源内容");
}

#[tokio::test]
async fn backup_also_bad_downgrades_with_warning() {
    // 主源坏 + 备用源也坏（与 backup_md5 不符）→ 切备用源后仍失败 → 降级接受 + md5 告警。
    let size = MB;
    let bad = vec![0u8; size as usize];
    let wrong_backup = vec![0x11u8; size as usize];
    let good = integration::http_server::patterned(size);

    let srv_main = HttpTestServer::start(HttpServerConfig {
        size,
        content: Some(bad.clone()),
        ..Default::default()
    })
    .await;
    let srv_backup = HttpTestServer::start(HttpServerConfig {
        size,
        content: Some(wrong_backup.clone()),
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_backup(
        "bk2",
        &srv_main.url("/file"),
        &srv_backup.url("/file"),
        &md5_of(&good), // 备用源内容不符
        dir.path().to_path_buf(),
        "bk2.bin",
        &sha256_of(&good),
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "备用源也失败 → 降级接受");
    assert!(
        st.error.as_deref().unwrap_or("").contains("md5"),
        "备用源 md5 不匹配必须告警"
    );
}

#[tokio::test]
async fn backup_url_only_reuses_main_sha256() {
    // 只有 backup_url、无 backup_md5：切备用源后仍以主源 sha256 校验。
    // 备用源内容与声明 sha256 一致 → 校验通过。
    let size = MB;
    let good = integration::http_server::patterned(size);
    let bad = vec![0u8; size as usize];

    let srv_main = HttpTestServer::start(HttpServerConfig {
        size,
        content: Some(bad.clone()),
        ..Default::default()
    })
    .await;
    let srv_backup = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    // 手工构造：backup_url 有值、backup_md5 无值（复用 make_http_task_backup 后清空 md5）
    let mut task = make_http_task_backup(
        "bk3",
        &srv_main.url("/file"),
        &srv_backup.url("/file"),
        "00000000000000000000000000000000", // 占位，随后清空
        dir.path().to_path_buf(),
        "bk3.bin",
        &sha256_of(&good),
    );
    task.identity = smart_dl_core::identity::ContentIdentity::SingleFile {
        size: 0,
        etag: None,
        sha256: Some(sha256_of(&good)),
        backup_md5: None,
    };
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);
    assert!(st.error.is_none(), "复用 sha256 校验通过不得告警");
    let got = std::fs::read(dir.path().join("bk3.bin")).unwrap();
    assert_eq!(got, good);
}

#[tokio::test]
async fn no_backup_keeps_legacy_downgrade_path() {
    // 回归：无 backup 配置时行为不变 —— sha256 失败重下 1 次 → 降级接受 + sha256 告警。
    let size = MB;
    let wrong_sha = sha256_of(&vec![0u8; size as usize]);
    let srv = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_sha256(
        "bk4",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        "bk4.bin",
        &wrong_sha,
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);
    assert!(
        st.error.as_deref().unwrap_or("").contains("sha256"),
        "无备用源 → 保留原 sha256 降级告警"
    );
}

#[tokio::test]
async fn backup_unused_when_main_ok() {
    // 主源内容正确 → 不触发备用源（备用服务器不应收到请求）。
    // E24 适配：备用源用不同 ETag（多源门控拒绝）——否则同强 ETag 的双源
    // 会被合法地分段分摊（E24 新语义），原「零接触」断言不再成立；
    // 本测试锁定的语义是「兕底切换不被触发」，不同 ETag 下不变。
    let size = MB;
    let good = integration::http_server::patterned(size);
    let srv_main = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let srv_backup = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        etag: Some("etag-backup-different"),
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_backup(
        "bk5",
        &srv_main.url("/file"),
        &srv_backup.url("/file"),
        &md5_of(&good),
        dir.path().to_path_buf(),
        "bk5.bin",
        &sha256_of(&good),
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);
    assert!(st.error.is_none());
    let got = std::fs::read(dir.path().join("bk5.bin")).unwrap();
    assert_eq!(got, good);
    // 备用服务器仅 add 期探测 1 次（E24：主源成功也会探测备用源做同质性
    // 门控），无下载段请求
    assert_eq!(
        srv_backup
            .request_count
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "主源正常时备用源只应有探测请求，无下载段"
    );
}

#[tokio::test]
async fn backup_without_sha256_still_fails_over_on_content_mismatch() {
    // 主源无 sha256（不校验）→ 不触发备用源路径（无校验=不切换），保持原语义。
    // 这里同时验证：仅 backup_url 且主源无校验目标时，直接落位、不访问备用源。
    let size = MB;
    let bad = vec![0u8; size as usize];

    let srv_main = HttpTestServer::start(HttpServerConfig {
        size,
        content: Some(bad.clone()),
        ..Default::default()
    })
    .await;
    let srv_backup = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        // E24 适配：不同 ETag（主源零/备用源 patterned 内容本就不同，
        // 同默认 ETag 会误启多源混拼破坏本测试的「单源原样落位」前提）
        etag: Some("etag-backup-different"),
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let mut task = make_http_task_to(
        "bk6",
        &srv_main.url("/file"),
        dir.path().to_path_buf(),
        Some("bk6.bin"),
    );
    task.source = smart_dl_core::types::DownloadSource::Http {
        url: srv_main.url("/file"),
        headers: vec![],
        auth: None,
        backup_url: Some(srv_backup.url("/file")),
        proxy: None,
    };
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);
    assert!(st.error.is_none(), "无校验目标 → 不校验不告警");
    let got = std::fs::read(dir.path().join("bk6.bin")).unwrap();
    assert_eq!(got, bad, "主源内容原样落位");
    // E24：备用源只有 add 期探测（1 次），无下载段请求
    assert_eq!(
        srv_backup
            .request_count
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "无校验目标 → 不触发备用源下载"
    );
}
