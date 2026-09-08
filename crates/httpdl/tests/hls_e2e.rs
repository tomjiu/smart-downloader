//! C-HLS e2e：`.m3u8` URL 经 HttpEngine::add 分流 HLS VOD 下载——
//! 1. master playlist 最高带宽变体展开 → 4 段（2 明文 + 2 AES-128-CBC，
//!    IV 显式/缺省两形态）→ Completed + 落盘内容逐字节一致；
//! 2. 段账本续传：预置前 2 段凭据 → add 后仅拉取剩余段（请求计数断言）。

mod common;

use common::make_http_task_to;
use smart_dl_core::task::DownloadTask;
use smart_dl_core::types::{DownloadEngine, DownloadSource, EngineState};
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;
use std::time::Duration;

use axum::routing::get;
use cbc::cipher::{BlockEncryptMut, KeyIvInit};

const KEY: [u8; 16] = [0x42; 16];
const IV2: [u8; 16] = [0x11; 16];
const PLAIN0: &[u8] = b"PLAIN-0-";
const PLAIN1: &[u8] = b"PLAIN-1-----";
const PLAIN2: &[u8] = b"ENC-2-PAYLOAD";
const PLAIN3: &[u8] = b"ENC-3-PAYLOAD-LONGER";

fn aes_enc(plain: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
    type Enc = cbc::Encryptor<aes::Aes128>;
    let mut buf = vec![0u8; plain.len() + 16];
    buf[..plain.len()].copy_from_slice(plain);
    Enc::new(key.into(), iv.into())
        .encrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut buf, plain.len())
        .unwrap()
        .to_vec()
}

/// 测试源站：master/media 清单 + key + 4 段（seg2/seg3 密文）+ 请求计数。
async fn serve_hls() -> (
    String,
    Arc<std::sync::Mutex<std::collections::HashMap<String, usize>>>,
) {
    let counts: Arc<std::sync::Mutex<std::collections::HashMap<String, usize>>> =
        Arc::new(Default::default());
    let seg0 = PLAIN0.to_vec();
    let seg1 = PLAIN1.to_vec();
    let seg2 = aes_enc(PLAIN2, &KEY, &IV2);
    let iv3 = smart_dl_httpdl::hls::iv_from_sequence(3);
    let seg3 = aes_enc(PLAIN3, &KEY, &iv3);
    let key = KEY.to_vec();

    async fn counting(
        counts: Arc<std::sync::Mutex<std::collections::HashMap<String, usize>>>,
        name: &'static str,
        body: Vec<u8>,
    ) -> Vec<u8> {
        *counts.lock().unwrap().entry(name.to_string()).or_default() += 1;
        body
    }

    let counts_key = counts.clone();
    let counts_s0 = counts.clone();
    let counts_s1 = counts.clone();
    let counts_s2 = counts.clone();
    let counts_s3 = counts.clone();
    let app = axum::Router::new()
        .route(
            "/master.m3u8",
            get(|| async {
                "#EXTM3U\n\
                 #EXT-X-STREAM-INF:BANDWIDTH=500000\nlow.m3u8\n\
                 #EXT-X-STREAM-INF:BANDWIDTH=2000000\nmedia.m3u8\n"
            }),
        )
        .route(
            "/low.m3u8",
            get(|| async { "#EXTM3U\n#EXTINF:1,\nlowseg.ts\n#EXT-X-ENDLIST\n" }),
        )
        .route(
            "/media.m3u8",
            get(|| async {
                "#EXTM3U\n\
                 #EXT-X-TARGETDURATION:10\n\
                 #EXT-X-MEDIA-SEQUENCE:0\n\
                 #EXTINF:9.0,\nv/seg0.ts\n\
                 #EXTINF:9.0,\nv/seg1.ts\n\
                 #EXT-X-KEY:METHOD=AES-128,URI=\"key\",IV=0x11111111111111111111111111111111\n\
                 #EXTINF:9.0,\nv/seg2.ts\n\
                 #EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n\
                 #EXTINF:9.0,\nv/seg3.ts\n\
                 #EXT-X-ENDLIST\n"
            }),
        )
        .route(
            "/key",
            get(move || counting(counts_key.clone(), "key", key.clone())),
        )
        .route(
            "/v/seg0.ts",
            get(move || counting(counts_s0.clone(), "seg0", seg0.clone())),
        )
        .route(
            "/v/seg1.ts",
            get(move || counting(counts_s1.clone(), "seg1", seg1.clone())),
        )
        .route(
            "/v/seg2.ts",
            get(move || counting(counts_s2.clone(), "seg2", seg2.clone())),
        )
        .route(
            "/v/seg3.ts",
            get(move || counting(counts_s3.clone(), "seg3", seg3.clone())),
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

#[tokio::test]
async fn hls_master_expands_and_decrypts_segments() {
    let (base, _counts) = serve_hls().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_task("t1", &format!("{base}/master.m3u8"), dir.path());
    let tid = engine.add(&task).await.unwrap();

    // 轮询到终态
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut snap = engine.status(&tid).await.unwrap();
    while snap.state == EngineState::Downloading || snap.state == EngineState::MetadataPending {
        assert!(
            tokio::time::Instant::now() < deadline,
            "HLS 30s 未完成: {snap:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        snap = engine.status(&tid).await.unwrap();
    }
    assert_eq!(
        snap.state,
        EngineState::Completed,
        "HLS 下载应 Completed: {snap:?}"
    );
    // E9 落盘名回显：v1 基于入口 URL（master.m3u8 → master.ts；变体在运行时
    // 展开，media.m3u8 内容合流进同一交付文件）
    assert_eq!(snap.name.as_deref(), Some("master.ts"));

    let got = std::fs::read(dir.path().join("master.ts")).unwrap();
    let expect: Vec<u8> = [PLAIN0, PLAIN1, PLAIN2, PLAIN3].concat();
    assert_eq!(got, expect, "4 段顺序拼接 + 2 段 AES-128 解密逐字节一致");
}

#[tokio::test]
async fn hls_ledger_resume_skips_done_segments() {
    let (base, counts) = serve_hls().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());

    // 预置续传凭据：前 2 段已完成（.part = PLAIN0+PLAIN1 + 段账本）
    let media_url = format!("{base}/media.m3u8");
    let media_text = reqwest::get(&media_url)
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let fingerprint = smart_dl_httpdl::hls::hls_fingerprint(&media_url, &media_text);
    let dest = dir.path().join("media.ts");
    let part = dir.path().join("media.ts.part");
    std::fs::write(&part, [PLAIN0, PLAIN1].concat()).unwrap();
    let ledger = smart_dl_httpdl::hls::HlsLedger {
        version: smart_dl_httpdl::hls::HLS_LEDGER_VERSION,
        playlist_fingerprint: fingerprint,
        segments_done: 2,
        bytes_done: (PLAIN0.len() + PLAIN1.len()) as u64,
    };
    std::fs::write(
        dir.path().join("media.ts.part.hls-ledger"),
        serde_json::to_vec(&ledger).unwrap(),
    )
    .unwrap();

    let task = make_task("t2", &media_url, dir.path());
    let tid = engine.add(&task).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let snap = engine.status(&tid).await.unwrap();
        if snap.state == EngineState::Completed || snap.state == EngineState::Error {
            assert_eq!(snap.state, EngineState::Completed, "续传任务应 Completed");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "HLS 续传 30s 未完成"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let got = std::fs::read(&dest).unwrap();
    let expect: Vec<u8> = [PLAIN0, PLAIN1, PLAIN2, PLAIN3].concat();
    assert_eq!(got, expect, "续传 = 前 2 段落盘 + 后 2 段新拉");

    // 请求计数：seg0/seg1 零请求（账本命中），seg2/seg3 各 1，key 1
    let c = counts.lock().unwrap();
    assert_eq!(c.get("seg0"), None, "已完成段不应重复拉取");
    assert_eq!(c.get("seg1"), None, "已完成段不应重复拉取");
    assert_eq!(c.get("seg2"), Some(&1));
    assert_eq!(c.get("seg3"), Some(&1));
    assert_eq!(c.get("key"), Some(&1));
}

#[tokio::test]
async fn hls_live_playlist_rejected_at_add() {
    // live 清单（无 ENDLIST）→ add 即失败（任务拒绝，引擎层 Err）
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/live.m3u8",
        get(|| async { "#EXTM3U\n#EXTINF:10,\nlive-seg.ts\n" }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_task("t3", &format!("http://{addr}/live.m3u8"), dir.path());
    // add 同步返回 Ok（任务创建），live 拒绝发生在下载循环内 → 任务 Error
    let tid = engine.add(&task).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let snap = engine.status(&tid).await.unwrap();
        if snap.state == EngineState::Error || snap.state == EngineState::Completed {
            assert_eq!(snap.state, EngineState::Error, "live 清单应任务失败");
            assert!(
                snap.error.as_deref().unwrap_or("").contains("live"),
                "错误信息应含 live 语义: {:?}",
                snap.error
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "live 拒绝 30s 未达终态"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn hls_pause_resume_roundtrip() {
    let (base, counts) = serve_hls().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let task = make_task("t4", &format!("{base}/media.m3u8"), dir.path());
    let tid = engine.add(&task).await.unwrap();
    // 立即暂停 → 等待部分段 → 恢复 → Completed
    engine.pause(&tid).await.unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;
    engine.resume(&tid).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let snap = engine.status(&tid).await.unwrap();
        if snap.state == EngineState::Completed || snap.state == EngineState::Error {
            assert_eq!(snap.state, EngineState::Completed, "pause→resume 后应完成");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "pause/resume 30s 未完成"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let got = std::fs::read(dir.path().join("media.ts")).unwrap();
    let expect: Vec<u8> = [PLAIN0, PLAIN1, PLAIN2, PLAIN3].concat();
    assert_eq!(got, expect);
    // 暂停时可能已有段完成：全流程后 key 至少 1 次；段请求总数 ≥ 4（含重复）
    let c = counts.lock().unwrap();
    assert!(c.get("key").copied().unwrap_or(0) >= 1);
    let seg_pulls: usize = ["seg0", "seg1", "seg2", "seg3"]
        .iter()
        .map(|s| c.get(*s).copied().unwrap_or(0))
        .sum();
    assert!(seg_pulls >= 4, "段请求总数应覆盖 4 段（暂停重试可能重复）");
}
