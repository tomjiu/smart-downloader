//! M4a: 重试退避（§14 D25：429/5xx 退避 1/2/4/8s×4）。不真等，纯函数断言延迟序列。

use smart_dl_httpdl::retry::{should_retry, Backoff};
use std::time::Duration;

#[test]
fn delay_sequence_is_1_2_4_8() {
    let b = Backoff::default(); // base 1s, max 8s
    assert_eq!(b.next_delay(1), Duration::from_secs(1));
    assert_eq!(b.next_delay(2), Duration::from_secs(2));
    assert_eq!(b.next_delay(3), Duration::from_secs(4));
    assert_eq!(b.next_delay(4), Duration::from_secs(8));
}

#[test]
fn delay_capped_at_max() {
    let b = Backoff::default();
    assert_eq!(b.next_delay(10), Duration::from_secs(8), "指数退避封顶 8s");
}

#[test]
fn retryable_statuses_are_429_and_5xx() {
    for code in [408, 429, 500, 502, 503, 504] {
        assert!(should_retry(code, 1, 4), "{code} 应可重试");
    }
}

#[test]
fn terminal_statuses_are_not_retryable() {
    for code in [200, 400, 401, 403, 404, 416] {
        assert!(!should_retry(code, 1, 4), "{code} 不应重试");
    }
}

#[test]
fn attempt_limit_stops_retry() {
    // 429×4 用尽（D25 ×4）→ 不再重试
    assert!(!should_retry(429, 4, 4));
    assert!(!should_retry(429, 5, 4));
}

#[test]
fn custom_backoff_respects_base_and_max() {
    let b = Backoff {
        base: Duration::from_millis(500),
        max: Duration::from_secs(2),
    };
    assert_eq!(b.next_delay(1), Duration::from_millis(500));
    assert_eq!(
        b.next_delay(3),
        Duration::from_secs(2),
        "2^2*0.5s=2s 达 max"
    );
}

// ============ UNWIRED #1 接线 e2e：段级退避（非纯函数） ============

mod common;
mod integration;

use common::{make_http_task_to, wait_terminal};
use integration::http_server::{patterned, sha256_of, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::HttpEngine;
use std::time::Instant;

/// 段请求 429 → Retry-After 退避 → 同段重试成功（不再拆半/失败）。
/// 锚定设计：1MiB 文件 < DEFAULT_MIN_SPLIT(16MiB) → 单段；探测豁免
///（bytes=0-0）→ 429 只落在唯一的段请求上。旧实现（无退避、拆半路径）：
/// 1MiB/2 < MIN_RETRY_GRANULARITY(1MiB) 不可拆 → 段失败 → mirror 耗尽 →
/// 任务 Error。新实现：退避 1s（服务器 Retry-After）+ jitter → 重试 206。
#[tokio::test]
async fn segment_429_backoff_retries_same_segment_and_succeeds() {
    let srv = HttpTestServer::start(HttpServerConfig {
        size: 1024 * 1024,
        range: true,
        patterned_content: true,
        retry_429_segments: 1, // 首个段请求 429（Retry-After: 1）
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "b429-e2e",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("out.bin"),
    );
    let tid = engine.add(&task).await.unwrap();

    let t0 = Instant::now();
    let st = wait_terminal(&engine, &tid).await;
    let el = t0.elapsed();
    assert_eq!(
        st.state,
        EngineState::Completed,
        "429 后应退避重试至完成（error: {:?}）",
        st.error
    );
    assert!(
        el >= Duration::from_millis(900),
        "退避必须真实发生（Retry-After 1s + jitter，实际 {el:?}）"
    );
    assert!(el < Duration::from_secs(10), "异常超长: {el:?}");
    // 请求计数：探测(1) + 段429(1) + 段重试(1) = 3——无拆半放大
    assert_eq!(
        srv.request_count.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "探测 + 429 + 重试共 3 请求（拆半/多余重试 = 放大）"
    );
    let got = std::fs::read(dir.path().join("out.bin")).unwrap();
    assert_eq!(sha256_of(&got), sha256_of(&patterned(1024 * 1024)));
}

/// 429 持续到耗尽（4 次）→ 不拆半、mirror 失败 → 任务 Error（请求量有界：
/// 1 + 4 = 5 次段请求，无 2^n 放大）。旧实现此处会先拆半（1MiB 边界下
/// 不可拆，行为等价 Error；本用例锁定耗尽语义与请求上界）。
#[tokio::test]
async fn persistent_429_exhausts_without_amplification() {
    let srv = HttpTestServer::start(HttpServerConfig {
        size: 1024 * 1024,
        range: true,
        patterned_content: true,
        retry_429_segments: 100, // 所有段请求恒 429
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "b429-exh",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("out.bin"),
    );
    let tid = engine.add(&task).await.unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(
        st.state,
        EngineState::Error,
        "持续 429 → 耗尽 → 任务 Error（实际 {:?}）",
        st.state
    );
    // 请求上界：探测 1 + 段 4 次（1/2/4s 退避）= 5——正反馈放大已消除
    let reqs = srv.request_count.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(reqs, 5, "请求量必须精确有界（探测+4 次尝试），实际 {reqs}");
}
