//! B-1 E2E（TDD 验收）：magnet → .torrent 元数据抓取全链路。
//! 依赖：自研 seed_main（本地 2MB 确定性种子）+ libtorrent 链接环境（与 m0 e2e 同前置）。
//! seed_main 直连注入（bootstrap peer），无需 tracker / DHT——确定性语义。
//!
//! 验收点：
//! 1. `fetch_metadata` 产出可解析的 .torrent bencode（infohash 交叉校验一致）
//! 2. `BtCore::metadata` API 层等价（metadata_received → Some(bytes)）

#[path = "../../../tests/integration/seed/mod.rs"]
mod seed;

use std::time::{Duration, Instant};

use smart_dl_btcore::{fetch_metadata, BtCore, FetchOpts};

#[test]
fn fetch_metadata_from_local_seeder_yields_torrent() {
    let seeder = seed::TestSeeder::start();
    let scratch = seed::TempDir::new().expect("tempdir");
    let (ip, port) = seeder.addr();
    let peer: std::net::SocketAddr = format!("{ip}:{port}").parse().expect("sockaddr");

    let opts = FetchOpts {
        timeout: Duration::from_secs(60),
        bootstrap_peers: vec![peer],
        enable_dht: false,
        ..Default::default()
    };
    let out = fetch_metadata(seeder.magnet(), scratch.path(), &opts).expect("fetch_metadata");

    // infohash 交叉校验（fetch 内部已断言，这里再核对外层语义）
    assert_eq!(
        out.summary.infohash_v1, out.infohash,
        "引擎/摘要 infohash 一致"
    );

    // 摘要面：seed_main 产 2MB 确定性单文件
    assert!(out.summary.total_size > 0, "total_size > 0");
    assert_eq!(out.summary.files.len(), 1, "单文件种子");
    assert!(!out.summary.name.is_empty());

    // .torrent 字节：非空、可独立解析（顶层 dict 含 info）
    assert!(!out.torrent.is_empty());
    let top = smart_dl_core::bencode::decode(&out.torrent).expect("bencode");
    assert!(top.dict_get(b"info").is_some(), "导出含 info dict");

    // 完成后任务已清理：同 infohash 的 status 应 NotFound（session 内无残留）
    // —— fetch 内部 remove(delete_data)；session 已 Drop，scratch 目录无残留 payload
    let entries = std::fs::read_dir(scratch.path()).expect("read scratch");
    let leftovers: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    assert!(
        leftovers.is_empty(),
        "scratch 不应残留下载目录: {leftovers:?}"
    );
}

#[test]
fn btcore_metadata_api_roundtrip() {
    let seeder = seed::TestSeeder::start();
    let save = seed::TempDir::new().expect("tempdir");
    let core = BtCore::new(save.path(), "meta-api").expect("session");
    let _ = core.set_alert_mask(0xFFFF);
    core.apply_discovery(false, false, false)
        .expect("discovery");

    let ih = core.add_magnet(seeder.magnet(), &[]).expect("add_magnet");
    core.resume(&ih).expect("resume");
    let (ip, port) = seeder.addr();
    core.add_peer(&ih, &ip, port).expect("add_peer");

    // metadata 未就绪前 → Ok(None)；就绪后 → Some(bytes)
    let deadline = Instant::now() + Duration::from_secs(60);
    let bytes = loop {
        let st = core.status(&ih).expect("status");
        if st.metadata_received {
            break core
                .metadata(&ih)
                .expect("metadata call")
                .expect("metadata bytes");
        }
        assert!(Instant::now() < deadline, "60s 内未收到 metadata");
        std::thread::sleep(Duration::from_millis(500));
    };

    let summary = smart_dl_core::torrent_meta::parse_torrent(&bytes).expect("parse");
    assert_eq!(summary.infohash_v1, ih, "导出 infohash 与任务一致");
    assert!(summary.total_size > 0);

    // 未注册任务 → Ok(None)（metadata 未就绪语义）
    let fake = "0123456789abcdef0123456789abcdef01234567";
    assert!(
        matches!(core.metadata(fake), Ok(None)),
        "未注册 ih → Ok(None)"
    );
}
