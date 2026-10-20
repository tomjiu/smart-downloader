//! feature `bt`：UNWIRED #5 回归——web seeds 端到端。
//! `.torrent` 的 `url-list` → daemon 侧提取（bt.rs）→ FFI `set_web_seeds`
//! → libtorrent url seed 拉取。零 tracker（announce 指向死端口）、零 peer
//!（无 DHT/LSd/PEX、无 add_peer）——HTTP 服务器是唯一可能的数据源，
//! 任务完成 + 服务器收到请求 + 内容逐字节一致 = web seed 链路全通。
//!
//! 版本语义：2.0.x libtorrent 全文件 `torrent_info` ctor 原生读 url-list
//!（本用例同时锁定该兜底）；2.1 的 info-section ctor 丢弃顶层 url-list
//! ——daemon 侧 `parse_torrent` 提取是唯一路径（本批修复）。
//! 无 bt feature 时整个文件被跳过（编译基线不链接 libtorrent）。

#![cfg(feature = "bt")]

mod common;

use common::lt_gate::LT_SESSION_GATE;
use smart_dl_core::identity::{CanonicalId, CanonicalKind, ContentIdentity};
use smart_dl_core::state_machine::{EvalPhase, TaskState};
use smart_dl_core::task::{DownloadTask, ProgressAggregate, RetryState, TaskMetadata};
use smart_dl_core::types::{DownloadEngine, DownloadSource, EngineState};
use smart_dl_daemon::bt::BtEngine;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use sha1::{Digest, Sha1};

const FILE_NAME: &str = "m0_test.bin";
const FILE_SIZE: usize = 256 * 1024; // 单 piece（piece length = 256KiB）

/// 确定性内容（xorshift64，固定种子）——torrent pieces 与服务器同源。
fn webseed_content() -> Vec<u8> {
    let mut x: u64 = 0x1234_5678_9abc_def0;
    let mut out = Vec::with_capacity(FILE_SIZE);
    while out.len() < FILE_SIZE {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// 手写带 url-list 的单文件 .torrent（bencode，键序 announce < info < url-list；
/// info 内 length < name < piece length < pieces）。
fn webseed_torrent(base_url: &str) -> Vec<u8> {
    let content = webseed_content();
    let mut h = Sha1::new();
    h.update(&content);
    let pieces = h.finalize();

    let announce = "http://127.0.0.1:1/announce"; // 死端口：排除 tracker 数据路径
    let mut t = Vec::new();
    t.extend_from_slice(b"d");
    t.extend_from_slice(format!("8:announce{}:", announce.len()).as_bytes());
    t.extend_from_slice(announce.as_bytes());
    t.extend_from_slice(b"4:infod");
    t.extend_from_slice(format!("6:lengthi{FILE_SIZE}e").as_bytes());
    t.extend_from_slice(format!("4:name{}:{}", FILE_NAME.len(), FILE_NAME).as_bytes());
    t.extend_from_slice(b"12:piece lengthi262144e");
    t.extend_from_slice(b"6:pieces20:");
    t.extend_from_slice(&pieces);
    t.extend_from_slice(b"e");
    t.extend_from_slice(b"8:url-listl");
    t.extend_from_slice(format!("{}:", base_url.len()).as_bytes());
    t.extend_from_slice(base_url.as_bytes());
    t.extend_from_slice(b"ee");
    t
}

/// web seed HTTP 服务器：支持 Range（libtorrent 按段拉取）+ 请求计数。
async fn spawn_webseed_server(content: Arc<Vec<u8>>) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let (c2, body_src) = (hits.clone(), content);
    let app = axum::Router::new().route(
        &format!("/{FILE_NAME}"),
        axum::routing::get(move |headers: HeaderMap| {
            let content = body_src.clone();
            let hits = c2.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                let total = content.len() as u64;
                // Range 解析：bytes=A-B / bytes=A- / bytes=-N（后缀）
                let range = headers
                    .get(header::RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|r| r.strip_prefix("bytes="))
                    .map(|r| r.to_string());
                let (start, end) = match range.as_deref() {
                    Some(r) if r.starts_with('-') => {
                        let n: u64 = r[1..].parse().unwrap_or(total);
                        (total.saturating_sub(n), total - 1)
                    }
                    Some(r) => {
                        let mut it = r.splitn(2, '-');
                        let s: u64 = it.next().unwrap_or("0").parse().unwrap_or(0);
                        let e = it
                            .next()
                            .filter(|e| !e.is_empty())
                            .and_then(|e| e.parse::<u64>().ok())
                            .map(|e| e.min(total - 1))
                            .unwrap_or(total - 1);
                        (s.min(total - 1), e.max(s))
                    }
                    None => (0, total - 1),
                };
                let body = content[start as usize..=(end as usize)].to_vec();
                let mut builder = Response::builder()
                    .header(header::CONTENT_LENGTH, body.len())
                    .header(header::ACCEPT_RANGES, "bytes");
                if range.is_some() {
                    builder = builder.status(StatusCode::PARTIAL_CONTENT).header(
                        header::CONTENT_RANGE,
                        format!("bytes {start}-{end}/{total}"),
                    );
                }
                builder.body(axum::body::Body::from(body)).unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, hits)
}

fn torrent_task(id: &str, torrent: Vec<u8>) -> DownloadTask {
    DownloadTask {
        id: id.to_string(),
        canonical_id: CanonicalId {
            kind: CanonicalKind::Bt,
            identity: id.to_string(),
            validator: None,
            token_sensitive: false,
        },
        source: DownloadSource::TorrentFile(torrent),
        identity: ContentIdentity::SingleFile {
            size: FILE_SIZE as u64,
            etag: None,
            sha256: None,
            sha1: None,
            md5: None,
            backup_md5: None,
        },
        dest_root: PathBuf::from("."),
        files: vec![],
        acquisitions: vec![],
        aggregate: ProgressAggregate::default(),
        state: TaskState::Evaluating(EvalPhase::MetadataPending),
        retry: RetryState::default(),
        created_at: Instant::now(),
        file_priorities: None,
        sequential: false,
        metadata: TaskMetadata {
            name: None,
            added_at_unix: 0,
            added_at_ms: 0,
            tags: Vec::new(),
            finished_at_unix: 0,
            start_at_unix: 0,
            next_retry_at_unix: 0,
        },
        limits: None,
        max_connections: None,
        queue_priority: 0,
    }
}

#[tokio::test]
async fn torrent_url_list_web_seed_downloads_without_peers() {
    let _lt = LT_SESSION_GATE.lock().await;
    // 纯 web seed 数据源：url-list 是唯一通道（tracker 死端口 + 无 DHT/LSd/PEX
    // + 不 add_peer）。完成即证明 url-list → daemon 提取 → FFI → libtorrent
    // 全链路；服务器请求计数 > 0 排除「数据来自别处」的可能。
    let content = Arc::new(webseed_content());
    let (ws_addr, hits) = spawn_webseed_server(content.clone()).await;
    let torrent = webseed_torrent(&format!("http://{ws_addr}/"));
    // .torrent 可解析性预检（fail-fast：测试自身构造错误不该表现成超时）
    let meta =
        smart_dl_core::torrent_meta::parse_torrent(&torrent).expect("测试 .torrent 必须可解析");
    assert_eq!(
        meta.web_seeds,
        vec![format!("http://{ws_addr}/")],
        "url-list 必须被 daemon 解析层读出（UNWIRED #5 修复点）"
    );

    let save = tempfile::tempdir().unwrap();
    let engine = BtEngine::new(
        save.path(),
        None,
        0,
        0,
        false, // dht off
        false, // lsd off
        false, // upnp off
        false, // pex off
        false, // utp off
        "allow",
        &[],
        0.0,
        0,
    )
    .unwrap();

    let tid = engine
        .add(&torrent_task("ws-e2e", torrent))
        .await
        .expect("add");

    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let st = engine.status(&tid).await.expect("status");
        // BT 完成语义：数据齐后转 Seeding（做种），非 Completed
        if matches!(st.state, EngineState::Completed | EngineState::Seeding)
            && st.total_done == FILE_SIZE as u64
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "120s 未完成（state={:?} done={}）：web seed 链路不通",
            st.state,
            st.total_done
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    assert!(
        hits.load(Ordering::SeqCst) > 0,
        "web seed 服务器必须收到请求（否则数据来源存疑）"
    );
    let got = std::fs::read(save.path().join(FILE_NAME)).expect("落盘文件");
    assert_eq!(got, *content, "web seed 内容必须逐字节一致");
}

#[tokio::test]
async fn magnet_ws_param_reaches_engine_add() {
    // magnet 分支的轻量锚定：ws= 的提取在 core 单测已锁定（magnet.rs），FFI
    // 转发与 .torrent 分支同构；本用例锁定「magnet 字符串原样进内核」——
    // libtorrent parse_magnet_uri 原生解析 ws=（RC_2_0 源码 magnet_uri.cpp
    // "ws" 分支 → p.url_seeds），daemon 不改写磁力串即透传。
    // 无 peer 场景 magnet 拿不到 metadata（web seed 不提供 metadata），
    // 故断言面收缩为：add 成功 + 引擎侧任务存在（不要求进度）。
    let save = tempfile::tempdir().unwrap();
    let engine = BtEngine::new(
        save.path(),
        None,
        0,
        0,
        false,
        false,
        false,
        false,
        false,
        "allow",
        &[],
        0.0,
        0,
    )
    .unwrap();
    // 任意合法 btih（40 hex）；ws= 指向不存在的服务器不影响 add 语义
    let magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=x&ws=http%3A%2F%2F127.0.0.1%3A1%2F";
    // 解析层确认 ws= 可提取（与 bt.rs 同一函数）
    let parsed = smart_dl_core::parse_magnet(magnet).expect("parse_magnet");
    assert_eq!(
        parsed.web_seeds,
        vec!["http://127.0.0.1:1/"],
        "magnet ws= 提取（UNWIRED #5 修复点）"
    );

    let mut task = torrent_task("ws-magnet", Vec::new());
    task.source = DownloadSource::Magnet(magnet.to_string());
    task.canonical_id.identity = magnet.to_string();
    let tid = engine.add(&task).await.expect("add（magnet + ws=）");
    let st = engine.status(&tid).await.expect("status");
    // metadata pending 属预期（无 peer）；任务必须存活而非报错
    assert_ne!(
        st.state,
        EngineState::Error,
        "magnet+ws= add 后不得报错: {:?}",
        st.error
    );
}
