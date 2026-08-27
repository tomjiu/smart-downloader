//! M4b: 镜像 failover（§14 镜像轮换）。mirror1 中途 404 → mirror2 接管，文件完整。

mod common;
mod integration;

use common::{make_http_task_to, wait_terminal};
use integration::http_server::{patterned, sha256_of, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::HttpEngine;

const MB: u64 = 1024 * 1024;

#[tokio::test]
async fn mirror1_404_then_mirror2_takes_over() {
    // 核心：段在 mirror1 上 404 → mirror2 补上 → 文件完整
    // 32MB → 动态分段 {0,16MB} 两段；第 2 段起点 16MB 在 mirror1 404
    let size = 32 * MB;
    let src = patterned(size);
    let expected = sha256_of(&src);

    // mirror1：第 2 段起点（16MB）404
    let m1 = HttpTestServer::start(HttpServerConfig {
        size,
        fail_ranges: vec![16 * MB],
        patterned_content: true,
        ..Default::default()
    })
    .await;
    // mirror2：全好
    let m2 = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "mir1",
        &m1.url("/file"),
        dir.path().to_path_buf(),
        Some("o.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    engine
        .update_sources(&tid, vec![m1.url("/file"), m2.url("/file")])
        .await
        .unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);
    let got = std::fs::read(dir.path().join("o.bin")).unwrap();
    assert_eq!(sha256_of(&got), expected, "镜像接管后文件必须完整");
}

#[tokio::test]
async fn healthy_mirror1_never_uses_mirror2() {
    let size = MB;
    let m1 = HttpTestServer::start(HttpServerConfig {
        size,
        ..Default::default()
    })
    .await;
    let m2 = HttpTestServer::start(HttpServerConfig {
        size,
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "mir2",
        &m1.url("/file"),
        dir.path().to_path_buf(),
        Some("o2.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    engine
        .update_sources(&tid, vec![m1.url("/file"), m2.url("/file")])
        .await
        .unwrap();
    wait_terminal(&engine, &tid).await;

    let m2_requests = m2.request_count.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(m2_requests, 0, "mirror1 全好时不应触碰 mirror2");
}

#[tokio::test]
async fn weighted_score_prefers_healthy_mirror() {
    // P1 Mirror 加权评分：首轮 m1 段1 失败被罚后，换源重试轮按分数排序优先 m2。
    // 64MB → 4 段（0/16M/32M/48M）；m1 对起点 16M 404，其余健康。
    let size = 64 * MB;
    let src = patterned(size);
    let expected = sha256_of(&src);
    let m1 = HttpTestServer::start(HttpServerConfig {
        size,
        fail_ranges: vec![16 * MB],
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let m2 = HttpTestServer::start(HttpServerConfig {
        size,
        patterned_content: true,
        ..Default::default()
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let urls = vec![m1.url("/file"), m2.url("/file")];
    let task = make_http_task_to("score1", &urls[0], dir.path().to_path_buf(), Some("s1.bin"));
    let tid = engine.add(&task).await.unwrap();
    engine.update_sources(&tid, urls).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "应完成: {:?}", st.error);
    let got = std::fs::read(dir.path().join("s1.bin")).unwrap();
    assert_eq!(sha256_of(&got), expected);

    // 换源重试轮：m1 已被罚（段1 缩小粒度仍失败）→ 排序优先 m2 → m2 服务全部 4 段起点
    // （首轮 mirrors 只有 [m1]，m1 的 16M 起点留痕属正常；断言 m2 全量接管即可证明排序生效）
    let m2_starts = m2.range_starts.lock().unwrap();
    for want in [0u64, 16 * MB, 32 * MB, 48 * MB] {
        assert!(
            m2_starts.contains(&want),
            "m2 应接管全部段（实际 {m2_starts:?}）缺起点 {want:#x}"
        );
    }
}

#[tokio::test]
async fn all_mirrors_dead_reports_error() {
    // 32MB → 动态分段 {0,16MB}；两源对第 2 段起点 16MB 都 404
    // （probe 走起点 0，不受影响）→ 段全源失败 → 整体 Error（不做部分成功利用）
    let size = 32 * MB;
    let m1 = HttpTestServer::start(HttpServerConfig {
        size,
        fail_ranges: vec![16 * MB],
        ..Default::default()
    })
    .await;
    let m2 = HttpTestServer::start(HttpServerConfig {
        size,
        fail_ranges: vec![16 * MB],
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "mir3",
        &m1.url("/file"),
        dir.path().to_path_buf(),
        Some("o3.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    engine
        .update_sources(&tid, vec![m1.url("/file"), m2.url("/file")])
        .await
        .unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Error, "全部 mirror 失败 → Error");
    assert!(st.error.is_some());
}

#[tokio::test]
async fn failed_large_segment_recovers_by_halving() {
    // P1 失败缩小粒度重试：mirror 对"起点 16MB 且长度 >= 8MB"的 Range 404（fail_ranges_min_len），
    // 整段 [16MB,32MB) 失败 → 拆半重试收敛：left2 [16MB,20MB) 放行、right2 [20MB,24MB) 与 right [24MB,32MB) 成功。
    let size = 32 * MB;
    let src = patterned(size);
    let expected = sha256_of(&src);
    let m1 = HttpTestServer::start(HttpServerConfig {
        size,
        fail_ranges: vec![16 * MB],
        fail_ranges_min_len: Some(8 * MB),
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "mir4",
        &m1.url("/file"),
        dir.path().to_path_buf(),
        Some("o4.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(
        st.state,
        EngineState::Completed,
        "缩小粒度重试应完成: {:?}",
        st.error
    );
    let got = std::fs::read(dir.path().join("o4.bin")).unwrap();
    assert_eq!(sha256_of(&got), expected, "缩小粒度重试后文件必须完整");

    // 拆分过程留痕：整段尝试（16MB）、left 再拆（20MB）、right（24MB）都应出现在 Range 起点里
    let starts = m1.range_starts.lock().unwrap();
    for want in [0u64, 16 * MB, 20 * MB, 24 * MB] {
        assert!(
            starts.contains(&want),
            "应观察到 Range 起点 {want:#x}（实际: {starts:?}）"
        );
    }
}
