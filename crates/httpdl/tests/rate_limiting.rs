//! 引擎级限速集成：`HttpEngine::new_limited` 真实节流下载路径（跨段共享 RateLimiter），
//! 且不破坏完整性。冷启动语义：无积压时首个 chunk 立即放行（见 rate.rs 单测），
//! 故 2MiB @ 1MiB/s 的总耗时 ≈ 2s（节流对总量生效，而不是完全等速）。

mod common;
mod integration;

use common::{make_http_task_to, wait_terminal};
use integration::http_server::{patterned, sha256_of, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::HttpEngine;
use std::time::{Duration, Instant};

const MB: u64 = 1024 * 1024;

#[tokio::test]
async fn new_limited_throttles_download_path() {
    // 2MiB @ 1MiB/s：限速接线生效 → 总耗时 ≈ 2s；回归（未接线）→ 本机 mock <200ms。
    let srv = HttpTestServer::start(HttpServerConfig {
        size: 2 * MB,
        range: true,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new_limited(reqwest::Client::new(), 1024); // 1MiB/s
    let task = make_http_task_to(
        "lim1",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("out.bin"),
    );
    let tid = engine.add(&task).await.unwrap();

    let t0 = Instant::now();
    let st = wait_terminal(&engine, &tid).await;
    let el = t0.elapsed();
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    assert!(
        el >= Duration::from_millis(900),
        "限速未生效（总量 2MiB @ 1MiB/s 应 ≈2s，实际 {el:?}）"
    );
    assert!(el < Duration::from_secs(8), "异常超长: {el:?}");

    let got = std::fs::read(dir.path().join("out.bin")).unwrap();
    assert_eq!(
        sha256_of(&got),
        sha256_of(&patterned(2 * MB)),
        "限速不得破坏完整性"
    );
}

#[tokio::test]
async fn unlimited_is_fast_by_contrast() {
    // 对照组：不限速引擎下载同尺寸应远快于限速用例（区分"限速生效"与"环境慢"）
    let srv = HttpTestServer::start(HttpServerConfig {
        size: 2 * MB,
        range: true,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "lim2",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("out.bin"),
    );
    let tid = engine.add(&task).await.unwrap();

    let t0 = Instant::now();
    let st = wait_terminal(&engine, &tid).await;
    let el = t0.elapsed();
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    assert!(
        el < Duration::from_millis(900),
        "不限速引擎 2MiB 本机下载应 <900ms（实际 {el:?}）；若环境过慢需放宽限速用例阈值"
    );
}

/// E16 全局限速热改（trait 扩展）：new_limited(0) 不限速引擎经
/// `set_global_limits(Some(1024), None)` 热改总阀门 → 2MiB 下载节流到 ≈2s。
#[tokio::test]
async fn set_global_limits_hot_adjust_caps_download() {
    let srv = HttpTestServer::start(HttpServerConfig {
        size: 2 * MB,
        range: true,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new_limited(reqwest::Client::new(), 0); // 全局不限
    let task = make_http_task_to(
        "gl1",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("out.bin"),
    );
    let tid = engine.add(&task).await.unwrap();

    // 总阀门热改：0 → 1MiB/s（运行中立即生效）
    engine.set_global_limits(Some(1024), None).await.unwrap();

    let t0 = Instant::now();
    let st = wait_terminal(&engine, &tid).await;
    let el = t0.elapsed();
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    assert!(
        el >= Duration::from_millis(900),
        "总阀门热改未生效（2MiB @ 1MiB/s 应 ≈2s，实际 {el:?}）"
    );
    let got = std::fs::read(dir.path().join("out.bin")).unwrap();
    assert_eq!(
        sha256_of(&got),
        sha256_of(&patterned(2 * MB)),
        "总阀门热改不得破坏完整性"
    );
}

/// E16 链式语义：任务级限速任务同样受全局总阀门约束——任务级 set_limits(0)
/// （不限）+ 全局 1MiB/s → 2MiB 下载仍 ≈2s（回归锁：旧实现任务级 limiter
/// 绕过全局，本用例在旧行为下 <900ms 完成）。
#[tokio::test]
async fn global_valve_caps_task_limited_task() {
    let srv = HttpTestServer::start(HttpServerConfig {
        size: 2 * MB,
        range: true,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new_limited(reqwest::Client::new(), 1024); // 全局 1MiB/s
    let task = make_http_task_to(
        "gl2",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("out.bin"),
    );
    let tid = engine.add(&task).await.unwrap();

    // 任务级设 0（不限）：链式 limiter 上游仍是全局 → 总阀门不因任务级不限被绕过
    engine.set_limits(&tid, Some(0), None).await.unwrap();

    let t0 = Instant::now();
    let st = wait_terminal(&engine, &tid).await;
    let el = t0.elapsed();
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    assert!(
        el >= Duration::from_millis(900),
        "任务级不限不得绕过总阀门（2MiB @ 全局 1MiB/s 应 ≈2s，实际 {el:?}）"
    );
}

/// E16 up 方向拒绝：HTTP 引擎 set_global_limits(up=Some) → Other（调用方
/// 定性为入参错误），down 方向不受影响。
#[tokio::test]
async fn set_global_limits_rejects_up_direction() {
    let engine = HttpEngine::new(reqwest::Client::new());
    let err = engine.set_global_limits(None, Some(512)).await.unwrap_err();
    assert!(
        matches!(err, smart_dl_core::types::EngineError::Other(_)),
        "up 方向应报 Other，实际 {err:?}"
    );
    // down=None + up=Some 同样拒绝（前置校验，不产生任何副作用）
    let err2 = engine
        .set_global_limits(Some(1024), Some(512))
        .await
        .unwrap_err();
    assert!(matches!(err2, smart_dl_core::types::EngineError::Other(_)));
}
