//! C-DASH e2e：`.mpd` URL 经 HttpEngine::add 分流 DASH static VOD 下载——
//! 1. SegmentTemplate duration 定址：init + 4 媒体段（$Number%03d$ 展开）
//!    → Completed + 落盘内容逐字节一致 + 落盘名派生 `<清单名>.mp4`；
//! 2. SegmentTimeline 定址段账本续传：预置前 3 项凭据 → add 后仅拉取剩余段
//!    （请求计数断言）；
//! 3. dynamic（live）清单 → 任务 Error（拒绝面在下载循环内落错）；
//! 4. pause → resume 往返 → Completed。

mod common;

use common::make_http_task_to;
use smart_dl_core::task::DownloadTask;
use smart_dl_core::types::{DownloadEngine, DownloadSource, EngineState};
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;
use std::time::Duration;

use axum::routing::get;

const INIT: &[u8] = b"INIT-VIDEO-FMP4-";
const CH1: &[u8] = b"CHUNK-1-";
const CH2: &[u8] = b"CHUNK-2---";
const CH3: &[u8] = b"CHUNK-3-PAYLOAD";
const CH4: &[u8] = b"CHUNK-4-PAYLOAD-LONGER";

const MANIFEST_MPD: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT8S">
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <SegmentTemplate timescale="1" duration="2" initialization="video/init.mp4" media="video/chunk-$Number%03d$.m4s"/>
      <Representation id="1080p" bandwidth="4000000"/>
      <Representation id="720p" bandwidth="2000000"/>
    </AdaptationSet>
  </Period>
</MPD>"#;

const TIMELINE_MPD: &str = r#"<MPD type="static">
  <Period>
    <AdaptationSet contentType="video">
      <SegmentTemplate timescale="1" initialization="tl/init.mp4" media="tl/seg-$Time$.m4s">
        <SegmentTimeline><S t="0" d="1" r="3"/></SegmentTimeline>
      </SegmentTemplate>
      <Representation id="v" bandwidth="1000000"/>
    </AdaptationSet>
  </Period>
</MPD>"#;

/// 测试源站：duration 定址清单（4 段）+ timeline 定址清单（4 段）+ 请求计数。
async fn serve_dash() -> (
    String,
    Arc<std::sync::Mutex<std::collections::HashMap<String, usize>>>,
) {
    let counts: Arc<std::sync::Mutex<std::collections::HashMap<String, usize>>> =
        Arc::new(Default::default());

    async fn counting(
        counts: Arc<std::sync::Mutex<std::collections::HashMap<String, usize>>>,
        name: &'static str,
        body: Vec<u8>,
    ) -> Vec<u8> {
        *counts.lock().unwrap().entry(name.to_string()).or_default() += 1;
        body
    }

    let c_init = counts.clone();
    let c_c1 = counts.clone();
    let c_c2 = counts.clone();
    let c_c3 = counts.clone();
    let c_c4 = counts.clone();
    let c_tinit = counts.clone();
    let c_t0 = counts.clone();
    let c_t1 = counts.clone();
    let c_t2 = counts.clone();
    let c_t3 = counts.clone();
    let app = axum::Router::new()
        .route("/manifest.mpd", get(|| async { MANIFEST_MPD }))
        .route(
            "/video/init.mp4",
            get(move || counting(c_init, "init", INIT.to_vec())),
        )
        .route(
            "/video/chunk-001.m4s",
            get(move || counting(c_c1, "chunk-001", CH1.to_vec())),
        )
        .route(
            "/video/chunk-002.m4s",
            get(move || counting(c_c2, "chunk-002", CH2.to_vec())),
        )
        .route(
            "/video/chunk-003.m4s",
            get(move || counting(c_c3, "chunk-003", CH3.to_vec())),
        )
        .route(
            "/video/chunk-004.m4s",
            get(move || counting(c_c4, "chunk-004", CH4.to_vec())),
        )
        .route("/tl.mpd", get(|| async { TIMELINE_MPD }))
        .route(
            "/tl/init.mp4",
            get(move || counting(c_tinit, "tl-init", INIT.to_vec())),
        )
        .route(
            "/tl/seg-0.m4s",
            get(move || counting(c_t0, "tl-seg-0", CH1.to_vec())),
        )
        .route(
            "/tl/seg-1.m4s",
            get(move || counting(c_t1, "tl-seg-1", CH2.to_vec())),
        )
        .route(
            "/tl/seg-2.m4s",
            get(move || counting(c_t2, "tl-seg-2", CH3.to_vec())),
        )
        .route(
            "/tl/seg-3.m4s",
            get(move || counting(c_t3, "tl-seg-3", CH4.to_vec())),
        )
        .route(
            "/live.mpd",
            get(|| async {
                r#"<MPD type="dynamic" availabilityStartTime="2026-01-01T00:00:00Z">
  <Period><AdaptationSet contentType="video">
    <SegmentTemplate duration="1" media="s$Number$.m4s"/>
    <Representation id="a" bandwidth="1"/>
  </AdaptationSet></Period>
</MPD>"#
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), counts)
}

fn make_task(id: &str, url: &str, dest_root: &std::path::Path) -> DownloadTask {
    let mut t = make_http_task_to(id, url, dest_root.to_path_buf(), None);
    t.source = DownloadSource::Http {
        url: url.to_string(),
        headers: vec![],
        auth: None,
        backup_url: None,
        proxy: None,
    };
    t
}

async fn wait_terminal(engine: &HttpEngine, tid: &String) -> smart_dl_core::types::EngineStatus {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let snap = engine.status(tid).await.unwrap();
        if matches!(snap.state, EngineState::Completed | EngineState::Error) {
            return snap;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "DASH 30s 未达终态: {snap:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn dash_static_vod_downloads_init_and_segments() {
    let (base, _counts) = serve_dash().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_task("t1", &format!("{base}/manifest.mpd"), dir.path());
    let tid = engine.add(&task).await.unwrap();
    let snap = wait_terminal(&engine, &tid).await;
    assert_eq!(
        snap.state,
        EngineState::Completed,
        "DASH 下载应 Completed: {snap:?}"
    );
    // 落盘名派生：manifest.mpd → manifest.mp4
    assert_eq!(snap.name.as_deref(), Some("manifest.mp4"));

    let got = std::fs::read(dir.path().join("manifest.mp4")).unwrap();
    let expect: Vec<u8> = [INIT, CH1, CH2, CH3, CH4].concat();
    assert_eq!(got, expect, "init + 4 段顺序拼接逐字节一致");
}

#[tokio::test]
async fn dash_ledger_resume_skips_done_segments() {
    let (base, counts) = serve_dash().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());

    // 预置续传凭据：init + 前 2 段已完成（.part + 段账本）
    let mpd_url = format!("{base}/tl.mpd");
    let mpd_text = reqwest::get(&mpd_url).await.unwrap().text().await.unwrap();
    let fingerprint = smart_dl_httpdl::dash::dash_fingerprint(&mpd_url, &mpd_text);
    let prefix: Vec<u8> = [INIT, CH1, CH2].concat();
    let part = dir.path().join("tl.mp4.part");
    std::fs::write(&part, &prefix).unwrap();
    let ledger = smart_dl_httpdl::dash::DashLedger {
        version: smart_dl_httpdl::dash::DASH_LEDGER_VERSION,
        manifest_fingerprint: fingerprint,
        segments_done: 3,
        bytes_done: prefix.len() as u64,
    };
    std::fs::write(
        dir.path().join("tl.mp4.part.dash-ledger"),
        serde_json::to_vec(&ledger).unwrap(),
    )
    .unwrap();

    let task = make_task("t2", &mpd_url, dir.path());
    let tid = engine.add(&task).await.unwrap();
    let snap = wait_terminal(&engine, &tid).await;
    assert_eq!(snap.state, EngineState::Completed, "续传任务应 Completed");
    let got = std::fs::read(dir.path().join("tl.mp4")).unwrap();
    let expect: Vec<u8> = [INIT, CH1, CH2, CH3, CH4].concat();
    assert_eq!(got, expect, "续传 = 前 3 项落盘 + 后 2 段新拉");

    // 请求计数：init/seg-0/seg-1 零请求（账本命中），seg-2/seg-3 各 1
    let c = counts.lock().unwrap();
    assert_eq!(c.get("tl-init"), None, "已完成项不应重复拉取");
    assert_eq!(c.get("tl-seg-0"), None);
    assert_eq!(c.get("tl-seg-1"), None);
    assert_eq!(c.get("tl-seg-2"), Some(&1));
    assert_eq!(c.get("tl-seg-3"), Some(&1));
}

#[tokio::test]
async fn dash_dynamic_rejected_in_loop() {
    let (base, _counts) = serve_dash().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_task("t3", &format!("{base}/live.mpd"), dir.path());
    // add 同步返回 Ok（任务创建），dynamic 拒绝发生在下载循环内 → 任务 Error
    let tid = engine.add(&task).await.unwrap();
    let snap = wait_terminal(&engine, &tid).await;
    assert_eq!(snap.state, EngineState::Error, "dynamic 清单应任务失败");
    assert!(
        snap.error.as_deref().unwrap_or("").contains("dynamic"),
        "错误信息应含 dynamic 语义: {:?}",
        snap.error
    );
}

#[tokio::test]
async fn dash_pause_resume_roundtrip() {
    let (base, counts) = serve_dash().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_task("t4", &format!("{base}/manifest.mpd"), dir.path());
    let tid = engine.add(&task).await.unwrap();
    // 立即暂停 → 等待部分段 → 恢复 → Completed
    engine.pause(&tid).await.unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;
    engine.resume(&tid).await.unwrap();
    let snap = wait_terminal(&engine, &tid).await;
    assert_eq!(snap.state, EngineState::Completed, "pause→resume 后应完成");
    let got = std::fs::read(dir.path().join("manifest.mp4")).unwrap();
    let expect: Vec<u8> = [INIT, CH1, CH2, CH3, CH4].concat();
    assert_eq!(got, expect);
    // 暂停重试可能重复拉段：init + 4 段请求总数 ≥ 5
    let c = counts.lock().unwrap();
    let pulls: usize = ["init", "chunk-001", "chunk-002", "chunk-003", "chunk-004"]
        .iter()
        .map(|s| c.get(*s).copied().unwrap_or(0))
        .sum();
    assert!(pulls >= 5, "段请求总数应覆盖 init + 4 段（含重复）: {c:?}");
}
