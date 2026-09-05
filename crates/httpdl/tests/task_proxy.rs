//! E5 任务级代理回归：任务专用 client 仅装该代理（覆盖全局），探测/下载
//! 全链生效；代理凭据以 Proxy-Authorization: Basic 下发；None 回退共享 client。
//!
//! 测试策略：mini 正向代理（axum）返回 patterned 内容；任务目标指向
//! 不可达域名（DNS 必失败）——内容只能经代理获得，Completed + sha256 一致
//! 即证明请求确实走了代理。对照组：无代理 → 同目标必 Failed 且代理零触达。

mod common;
mod integration;

use integration::http_server::{patterned, sha256_of};
use smart_dl_core::identity::ContentIdentity;
use smart_dl_core::types::{DownloadEngine, DownloadSource, EngineState};
use smart_dl_httpdl::HttpEngine;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use common::{make_http_task_to, wait_terminal};

const SIZE: u64 = 1024 * 1024; // 1MB = 4 段（256KB）
/// base64("user:pass") —— proxy basic_auth 对 http 目标以 Proxy-Authorization 下发。
const EXPECT_PROXY_AUTH: &str = "Basic dXNlcjpwYXNz";

/// 不可达目标域名（.invalid TLD 由 RFC 2606 保留，DNS 必失败；走代理时
/// reqwest 不解析目标 DNS，由代理应答——内容只能来自代理）。
const UNREACHABLE_URL: &str = "http://proxy-e2e.invalid/f.bin";

/// 解析 `Range: bytes=start-end`（单区间形态，引擎只发这种）。
fn parse_range(v: &str) -> Option<(u64, u64)> {
    let rest = v.strip_prefix("bytes=")?;
    let (s, e) = rest.split_once('-')?;
    Some((s.parse().ok()?, e.parse().ok()?))
}

/// mini 正向代理：任何方法/路径（fallback）→ 按 Range 回 206 切片或 200 全量，
/// 内容恒为 `body`。`require_proxy_auth = true` 时校验 Proxy-Authorization 头，
/// 缺头/错值 → 407。返回（代理 URL，命中计数）。
async fn start_mini_proxy(body: Vec<u8>, require_proxy_auth: bool) -> (String, Arc<AtomicUsize>) {
    use axum::extract::Request;
    use axum::http::{header, HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing;

    let hits = Arc::new(AtomicUsize::new(0));
    let hits_c = hits.clone();
    let body = Arc::new(body);

    let app = axum::Router::new().fallback(routing::any(move |req: Request| {
        let hits = hits_c.clone();
        let body = body.clone();
        async move {
            let headers: HeaderMap = req.headers().clone();
            hits.fetch_add(1, Ordering::SeqCst);
            if require_proxy_auth {
                let ok = headers
                    .get("proxy-authorization")
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v == EXPECT_PROXY_AUTH)
                    .unwrap_or(false);
                if !ok {
                    return StatusCode::PROXY_AUTHENTICATION_REQUIRED.into_response();
                }
            }
            let total = body.len() as u64;
            match headers
                .get("range")
                .and_then(|v| v.to_str().ok())
                .and_then(parse_range)
            {
                Some((s, e)) if s < total => {
                    let e = e.min(total - 1);
                    let slice = &body[s as usize..=(e as usize)];
                    (
                        StatusCode::PARTIAL_CONTENT,
                        [(header::CONTENT_RANGE, format!("bytes {s}-{e}/{total}"))],
                        slice.to_vec(),
                    )
                        .into_response()
                }
                // 无 Range / 起点越界：全量 200
                _ => (StatusCode::OK, body.as_ref().clone()).into_response(),
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), hits)
}

/// 构造带 proxy 的任务（目标 = 不可达域名；dest_root 指向 tempdir）。
fn make_proxied_task(
    id: &str,
    proxy: Option<String>,
    dest: std::path::PathBuf,
    name: &str,
    sha: &str,
) -> smart_dl_core::task::DownloadTask {
    let mut t = make_http_task_to(id, UNREACHABLE_URL, dest, Some(name));
    t.source = DownloadSource::Http {
        url: UNREACHABLE_URL.into(),
        headers: vec![],
        auth: None,
        backup_url: None,
        proxy,
    };
    t.identity = ContentIdentity::SingleFile {
        size: 0,
        etag: None,
        sha256: Some(sha.to_string()),
        sha1: None,
        md5: None,
        backup_md5: None,
    };
    t
}

/// E5 主例：任务级代理生效——不可达目标仅经代理可达，任务 Completed 且
/// 落位内容 = 代理返回的 patterned 内容（探测/分段下载全链走任务 client）。
#[tokio::test]
async fn proxied_task_completes_via_proxy() {
    let content = patterned(SIZE);
    let sha = sha256_of(&content);
    let (proxy_url, hits) = start_mini_proxy(content.clone(), false).await;

    let engine = HttpEngine::new(reqwest::Client::new());
    let dir = tempfile::tempdir().unwrap();
    let task = make_proxied_task(
        "t-proxy",
        Some(proxy_url),
        dir.path().to_path_buf(),
        "via-proxy.bin",
        &sha,
    );
    let tid = engine.add(&task).await.unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(
        st.state,
        EngineState::Completed,
        "带代理任务应完成: err={:?}",
        st.error
    );
    assert!(hits.load(Ordering::SeqCst) > 0, "代理必须被触达");
    let got = std::fs::read(dir.path().join("via-proxy.bin")).unwrap();
    assert_eq!(sha256_of(&got), sha, "落位应为代理返回的 patterned 内容");
}

/// E5 认证：`http://user:pass@host` 形态的代理凭据以 Proxy-Authorization:
/// Basic 下发；错凭据 → 代理 407 → add 探测即失败。
#[tokio::test]
async fn proxy_basic_auth_forwarded() {
    let content = patterned(SIZE);
    let sha = sha256_of(&content);
    let (proxy_url, hits) = start_mini_proxy(content.clone(), true).await;
    let host_port = proxy_url.trim_start_matches("http://");

    // 带正确凭据 → 407 不出现 → Completed
    let engine = HttpEngine::new(reqwest::Client::new());
    let dir = tempfile::tempdir().unwrap();
    let task = make_proxied_task(
        "t-auth",
        Some(format!("http://user:pass@{host_port}")),
        dir.path().to_path_buf(),
        "auth-ok.bin",
        &sha,
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(
        st.state,
        EngineState::Completed,
        "带正确凭据应完成: err={:?}",
        st.error
    );
    assert!(hits.load(Ordering::SeqCst) > 0, "代理必须被触达");

    // 错凭据 → 代理 407 → add 探测即失败（主源探测失败）
    let engine2 = HttpEngine::new(reqwest::Client::new());
    let task2 = make_proxied_task(
        "t-auth-bad",
        Some(format!("http://wrong:creds@{host_port}")),
        dir.path().to_path_buf(),
        "auth-bad.bin",
        &sha,
    );
    let add_err = engine2.add(&task2).await;
    assert!(
        add_err.is_err(),
        "错误凭据应在 add 探测阶段即失败（代理 407）"
    );
}

/// E5 对照：无代理 + 不可达目标 → add 探测即失败（直连 DNS 解析失败），
/// 且代理零触达（证明主例内容确实来自代理而非其他路径）。
#[tokio::test]
async fn unproxied_task_fails_on_unreachable_target() {
    let content = patterned(SIZE);
    let sha = sha256_of(&content);
    let (_proxy_url, hits) = start_mini_proxy(content, false).await;

    let engine = HttpEngine::new(reqwest::Client::new());
    let dir = tempfile::tempdir().unwrap();
    let task = make_proxied_task(
        "t-noproxy",
        None,
        dir.path().to_path_buf(),
        "no-proxy.bin",
        &sha,
    );
    let add_err = engine.add(&task).await;
    assert!(
        add_err.is_err(),
        "不可达目标无代理必须失败（直连 DNS 解析失败）"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "对照组不得触达代理（证明主例内容确实来自代理）"
    );
}

/// E8 热改测试基建口径：32MB（2 段 × 16MB min_split，segment_count 最小 2 并发）
/// + 全局限速 4MiB/s（总时长 ~8s）+ 固定 300ms 后热改——此时两段均在飞
///   （每段 ~4s），"下载中"由总时长保证，不依赖进度轮询（进度按段完成上报，
///   16MB 段粒度下中途恒为 0）。
const E8_SIZE: u64 = 32 * 1024 * 1024;
const E8_RATE_KB_S: u32 = 4096;

/// E8：下载中任务热切代理——新循环（epoch+1 重入）用新 client 领剩余段，
/// 新代理必须被触达；终态 Completed 且内容一致（旧循环检查点自杀不 finalize，
/// 并发覆盖写同内容字节幂等，P4 既有收敛语义）。
#[tokio::test]
async fn set_task_proxy_hot_switch_mid_download() {
    let content = patterned(E8_SIZE);
    let sha = sha256_of(&content);
    let (proxy_a, hits_a) = start_mini_proxy(content.clone(), false).await;
    let (proxy_b, hits_b) = start_mini_proxy(content.clone(), false).await;

    let engine = HttpEngine::new_limited(reqwest::Client::new(), E8_RATE_KB_S);
    let dir = tempfile::tempdir().unwrap();
    let task = make_proxied_task(
        "t-hotswitch",
        Some(proxy_a),
        dir.path().to_path_buf(),
        "hotswitch.bin",
        &sha,
    );
    let tid = engine.add(&task).await.unwrap();

    // 等下载进入中途（总时长 ~8s，300ms 时两段均在飞）
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let st = engine.status(&tid).await.unwrap();
    assert_eq!(st.state, EngineState::Downloading, "300ms 时应仍在下载");
    assert!(
        hits_a.load(Ordering::SeqCst) > 0,
        "切换前代理 A 必须已被触达"
    );

    // 热切 proxy B：立即返回（不等待下载），新循环用 B 重入
    engine
        .set_task_proxy(&tid, Some(proxy_b.clone()))
        .await
        .unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(
        st.state,
        EngineState::Completed,
        "热切代理后应经新代理完成: err={:?}",
        st.error
    );
    assert!(
        hits_b.load(Ordering::SeqCst) > 0,
        "切换后新代理 B 必须被触达（新 client 生效证据）"
    );
    let got = std::fs::read(dir.path().join("hotswitch.bin")).unwrap();
    assert_eq!(sha256_of(&got), sha, "落位内容必须与 patterned 一致");
}

/// E8：下载中任务清除代理（None）——新循环直连不可达目标（DNS 必失败），
/// 终态非 Completed；旧循环即使把剩余段领完也不 finalize（epoch 门控），
/// 落位文件不得存在。
#[tokio::test]
async fn set_task_proxy_clear_falls_back_direct() {
    let content = patterned(E8_SIZE);
    let sha = sha256_of(&content);
    let (proxy_a, _hits_a) = start_mini_proxy(content.clone(), false).await;

    let engine = HttpEngine::new_limited(reqwest::Client::new(), E8_RATE_KB_S);
    let dir = tempfile::tempdir().unwrap();
    let task = make_proxied_task(
        "t-clear",
        Some(proxy_a),
        dir.path().to_path_buf(),
        "cleared.bin",
        &sha,
    );
    let tid = engine.add(&task).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let st = engine.status(&tid).await.unwrap();
    assert_eq!(st.state, EngineState::Downloading, "300ms 时应仍在下载");

    // 清除代理：新循环直连 .invalid → DNS 必失败 → 段全失败 → Error
    engine.set_task_proxy(&tid, None).await.unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_ne!(
        st.state,
        EngineState::Completed,
        "清除代理后直连不可达目标不得完成"
    );
    assert!(
        !dir.path().join("cleared.bin").exists(),
        "未完成任务不得落位（旧循环 epoch 门控不 finalize）"
    );
}
