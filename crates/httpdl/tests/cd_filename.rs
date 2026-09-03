//! Content-Disposition 文件名识别（E4）回归。
//!
//! 契约：
//! 1. 落盘名派生链：用户显式名（权威，非法即拒）→ 探测响应 CD 文件名
//!    （RFC 6266/5987，剥目录成分）→ URL 末段 → "download.bin"；
//! 2. CD 目录成分（`/` `\`）剥离后落盘于 dest_root 内（穿越载体不得落地）；
//! 3. `filename*`（RFC 5987 percent-decode）优先于普通 `filename`。

mod common;
mod integration;

use common::{make_http_task_to, wait_terminal};
use integration::http_server::{patterned, sha256_of, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::HttpEngine;

const MB: u64 = 1024 * 1024;

async fn server_with_cd(cd: &str) -> HttpTestServer {
    let size = MB;
    HttpTestServer::start(HttpServerConfig {
        size,
        range: true,
        patterned_content: true,
        content_disposition: Some(cd.to_string()),
        ..Default::default()
    })
    .await
}

/// 1. CD 普通引号名 → 落盘为服务端声明名（不再 download.bin）。
#[tokio::test]
async fn cd_plain_name_lands_as_declared() {
    let size = MB;
    let src = patterned(size);
    let srv = server_with_cd("attachment; filename=\"setup-v2.exe\"").await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    // name = None → 走自动派生链；URL 带无意义 query，末段名（file）应被 CD 压制
    let task = make_http_task_to(
        "cd1",
        &format!("{}?download-id=42", srv.url("/file")),
        dir.path().to_path_buf(),
        None,
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);
    let got = std::fs::read(dir.path().join("setup-v2.exe")).unwrap();
    assert_eq!(sha256_of(&got), sha256_of(&src), "落位内容一致");
    assert!(
        !dir.path().join("download.bin").exists(),
        "CD 存在时不得兜底 download.bin"
    );
}

/// 2. filename*（RFC 5987 UTF-8 percent-decode）优先，中文文件名正确落地。
#[tokio::test]
async fn cd_filename_star_utf8_lands_decoded() {
    let size = MB;
    let src = patterned(size);
    let srv = server_with_cd(
        "attachment; filename=\"fallback.bin\"; filename*=UTF-8''%E4%B8%AD%E6%96%87%E8%B5%84%E6%BA%90.bin",
    )
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to("cd2", &srv.url("/file"), dir.path().to_path_buf(), None);
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);
    let got = std::fs::read(dir.path().join("中文资源.bin")).unwrap();
    assert_eq!(sha256_of(&got), sha256_of(&src));
}

/// 3. 用户显式名权威：CD 存在也不覆盖（同时 CD 不参与净化拒杀）。
#[tokio::test]
async fn user_name_overrides_content_disposition() {
    let size = MB;
    let src = patterned(size);
    let srv = server_with_cd("attachment; filename=\"server-name.bin\"").await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "cd3",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("mine.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);
    let got = std::fs::read(dir.path().join("mine.bin")).unwrap();
    assert_eq!(sha256_of(&got), sha256_of(&src));
    assert!(!dir.path().join("server-name.bin").exists());
}

/// 4. CD 目录成分剥离：`../../evil.bin` / `..\\..\\evil.bin` → 落盘为
/// `evil.bin` 于 dest_root 内，穿越路径不落地。
#[tokio::test]
async fn cd_traversal_stripped_to_basename_inside_root() {
    let size = MB;
    let src = patterned(size);

    // 正斜杠形态
    let srv = server_with_cd("attachment; filename=\"../../evil.bin\"").await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to("cd4a", &srv.url("/file"), dir.path().to_path_buf(), None);
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);
    let got = std::fs::read(dir.path().join("evil.bin")).unwrap();
    assert_eq!(sha256_of(&got), sha256_of(&src));
    // 穿越载体不得在 dest_root 外/内以路径形态存在
    assert!(!dir.path().join("..").join("evil.bin").exists());
    assert!(!dir.path().join("download.bin").exists());
    drop(engine);
    drop(srv);

    // Windows 反斜杠形态
    let srv2 = server_with_cd("attachment; filename=\"..\\..\\evil2.bin\"").await;
    let dir2 = tempfile::tempdir().unwrap();
    let engine2 = HttpEngine::new(reqwest::Client::new());
    let task2 = make_http_task_to("cd4b", &srv2.url("/file"), dir2.path().to_path_buf(), None);
    let tid2 = engine2.add(&task2).await.unwrap();
    let st2 = wait_terminal(&engine2, &tid2).await;
    assert_eq!(st2.state, EngineState::Completed);
    assert!(dir2.path().join("evil2.bin").exists());
}

/// 5. 无 CD → URL 末段派生（剥 query/hash；目录型 URL → download.bin 兜底）。
#[tokio::test]
async fn no_cd_falls_back_to_url_basename() {
    let size = MB;
    let src = patterned(size);
    let srv = HttpTestServer::start(HttpServerConfig {
        size,
        range: true,
        patterned_content: true,
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    // 无 CD + name=None → URL 末段派生（query 剥离）：/file?token=x → `file`，
    // 不再兜底 download.bin（与 FTP 引擎“URL 末段”语义对齐）
    let url = format!("{}?token=abc", srv.url("/file"));
    let task = make_http_task_to("cd5", &url, dir.path().to_path_buf(), None);
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed);
    let got = std::fs::read(dir.path().join("file")).unwrap();
    assert_eq!(sha256_of(&got), sha256_of(&src));
    assert!(!dir.path().join("download.bin").exists());
}

/// E9：status().name 透出落盘名决策结果——CD 派生名优先于 URL 末段
/// （daemon 名字回填的数据源契约）。
#[tokio::test]
async fn status_name_reports_cd_derived_name() {
    let srv = server_with_cd("attachment; filename=\"setup-v2.exe\"").await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    // 无显式名 → 派生链（CD 压制 URL 末段）
    let task = make_http_task_to("cd9", &srv.url("/file"), dir.path().to_path_buf(), None);
    let tid = engine.add(&task).await.unwrap();
    let st = engine.status(&tid).await.unwrap();
    assert_eq!(
        st.name.as_deref(),
        Some("setup-v2.exe"),
        "status 必须透出 CD 派生名（回填数据源）"
    );
}

/// E9：显式名回显同口径（决策结果 = 显式名，daemon 侧不覆盖）。
#[tokio::test]
async fn status_name_reports_explicit_name() {
    let srv = server_with_cd("attachment; filename=\"server-name.bin\"").await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "cd10",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("my-name.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    let st = engine.status(&tid).await.unwrap();
    assert_eq!(
        st.name.as_deref(),
        Some("my-name.bin"),
        "显式名回显同口径（权威不被服务端声明压制）"
    );
}
