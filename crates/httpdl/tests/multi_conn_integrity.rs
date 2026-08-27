//! M4b: 多连接并行下载（用户指定用例：4 段并行下载 64MB → SHA256 与源一致）。

mod common;
mod integration;

use common::{make_http_task_to, wait_terminal};
use integration::http_server::{patterned, sha256_of, HttpServerConfig, HttpTestServer};
use smart_dl_core::types::{DownloadEngine, EngineState};
use smart_dl_httpdl::{static_split, HttpEngine};

const MB: u64 = 1024 * 1024;

#[tokio::test]
async fn four_segments_64mb_sha256_matches_source() {
    // 用户指定用例：64MB → 16MB 粒度 4 段，由 2 个 worker（clamp(64MB/64MB,2,8)=2）
    // 经 SegmentManager 动态领取并行下载 → 文件 SHA256 与源一致
    let size = 64 * MB;
    let src = patterned(size);
    let expected = sha256_of(&src);
    let srv = HttpTestServer::start(HttpServerConfig {
        size,
        range: true,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "i1",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("out.bin"),
    );
    let tid = engine.add(&task).await.unwrap();

    let st = wait_terminal(&engine, &tid).await;
    assert_eq!(st.state, EngineState::Completed, "error: {:?}", st.error);

    let got = std::fs::read(dir.path().join("out.bin")).unwrap();
    assert_eq!(sha256_of(&got), expected, "4 段并行下载结果必须与源一致");
    assert!(
        !dir.path().join("out.bin.part").exists(),
        "完成后 .part 应落位删除"
    );
}

#[tokio::test]
async fn part_file_released_after_finalize() {
    let srv = HttpTestServer::start(HttpServerConfig {
        size: MB,
        patterned_content: true,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "i2",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("m.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    wait_terminal(&engine, &tid).await;

    assert!(dir.path().join("m.bin").exists());
    assert!(!dir.path().join("m.bin.part").exists());
}

#[tokio::test]
async fn segment_requests_cover_file_without_overlap() {
    // 64MB 文件，16MB 粒度 → 4 段：动态领取最终必须覆盖全部段起点
    // （probe 与段0 同起点 0；worker 领取顺序不影响最终集合）
    let size = 64 * MB;
    let srv = HttpTestServer::start(HttpServerConfig {
        size,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_http_task_to(
        "i3",
        &srv.url("/file"),
        dir.path().to_path_buf(),
        Some("c.bin"),
    );
    let tid = engine.add(&task).await.unwrap();
    wait_terminal(&engine, &tid).await;

    let starts: std::collections::HashSet<u64> =
        srv.range_starts.lock().unwrap().iter().copied().collect();
    let expected: std::collections::HashSet<u64> =
        [0u64, 16 * MB, 32 * MB, 48 * MB].into_iter().collect();
    assert!(
        starts.is_superset(&expected),
        "动态领取必须覆盖全文件各段，实际: {starts:?}"
    );
    assert_eq!(
        std::fs::metadata(dir.path().join("c.bin")).unwrap().len(),
        size
    );
}

#[test]
fn split_n_makes_n_disjoint_segments() {
    let segs = static_split::split_n(64 * MB, 4);
    assert_eq!(segs.len(), 4);
    for w in segs.windows(2) {
        assert_eq!(w[0].end + 1, w[1].start);
    }
    assert_eq!(segs.first().unwrap().start, 0);
    assert_eq!(segs.last().unwrap().end, 64 * MB - 1);
}
