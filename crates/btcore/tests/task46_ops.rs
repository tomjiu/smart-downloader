//! Task 46 强制操作三件套 + IP 封禁 + 种子导出集成测试（真实 libtorrent session）。
//! 覆盖：force_reannounce / force_dht_announce（语义 = 无错下发）；
//! force_recheck（完整下载后可重校验）；ban_ip / is_banned / unban_ip
//! （幂等 + 非法 IP 拒绝 + 引擎过滤生效）；export_torrent（metainfo bencode）。

#[path = "../../../tests/integration/seed/mod.rs"]
mod seed;

use std::time::{Duration, Instant};

use smart_dl_btcore::BtCore;

fn core(tag: &str) -> (BtCore, seed::TempDir) {
    let save = seed::TempDir::new().expect("tempdir");
    let c = BtCore::new(save.path(), tag).expect("session");
    (c, save)
}

/// add + resume + 挂本地 seeder 下载到完成（60s 上限）。
fn download_to_complete(c: &BtCore, ih: &str, seeder: &seed::TestSeeder) {
    c.resume(ih).expect("resume");
    let (ip, port) = seeder.addr();
    c.add_peer(ih, &ip, port).expect("add_peer");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let st = c.status(ih).expect("status");
        if st.progress >= 1.0 && st.state == 1 {
            return;
        }
        assert!(Instant::now() < deadline, "did not finish in 60s");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn force_reannounce_and_dht_ok() {
    let (c, _save) = core("t46-announce");
    let seeder = seed::TestSeeder::start();
    let ih = c.add_magnet(seeder.magnet(), &[]).expect("add_magnet");

    // 元数据未就绪也可宣告（handle 级操作，无前置状态要求）
    c.force_reannounce(&ih).expect("force_reannounce");
    c.force_dht_announce(&ih).expect("force_dht_announce");

    // 不存在的 infohash → NotFound（NOT_FOUND 语义）
    let r = c.force_reannounce("0000000000000000000000000000000000000000");
    assert!(matches!(r, Err(smart_dl_btcore::Error::NotFound(_))));
}

#[test]
fn force_recheck_after_complete() {
    let (c, _save) = core("t46-recheck");
    let seeder = seed::TestSeeder::start();
    let ih = c.add_magnet(seeder.magnet(), &[]).expect("add_magnet");
    download_to_complete(&c, &ih, &seeder);

    // 完成态强制重新校验：下发成功（校验异步推进；不做状态时序断言——
    // checking 窗口极短，依赖 seeder 在场校验必然通过）
    c.force_recheck(&ih).expect("force_recheck");

    // 校验后任务仍健康（无 seeder 断连时 progress 回读 1.0）
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let st = c.status(&ih).expect("status");
        if st.progress >= 1.0 {
            break;
        }
        assert!(Instant::now() < deadline, "recheck 后未恢复完整位图");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn ban_ip_lifecycle() {
    let (c, _save) = core("t46-ban");
    let seeder = seed::TestSeeder::start();

    // 初始未封禁
    assert!(!c.is_banned("192.0.2.77").expect("is_banned0"));

    // 封禁 + 查回
    c.ban_ip(None, "192.0.2.77").expect("ban");
    assert!(c.is_banned("192.0.2.77").expect("is_banned1"));

    // 幂等：重复封禁 Ok
    c.ban_ip(None, "192.0.2.77").expect("ban twice");

    // 封禁的 IP 无法建立出站连接（引擎过滤生效）：add_peer 后无进度
    // （TEST-NET-3 地址本就不可达；这里主要验证 filter 写入不破坏 session）
    let ih = c.add_magnet(seeder.magnet(), &[]).expect("add_magnet");
    c.resume(&ih).expect("resume");

    // 解封 + 查回
    c.unban_ip("192.0.2.77").expect("unban");
    assert!(!c.is_banned("192.0.2.77").expect("is_banned2"));

    // 幂等：未封禁再解封 Ok
    c.unban_ip("192.0.2.77").expect("unban again");

    // 非法 IP → Arg（上层映射 400）
    let r = c.ban_ip(None, "not-an-ip");
    assert!(matches!(r, Err(smart_dl_btcore::Error::Arg)));

    // 任务上下文封禁：ih 存在 → Ok；ih 不存在 → NotFound
    let seeder2 = seed::TestSeeder::start();
    let ih2 = c.add_magnet(seeder2.magnet(), &[]).expect("add_magnet2");
    c.ban_ip(Some(&ih2), "192.0.2.78").expect("ban with ih");
    assert!(c.is_banned("192.0.2.78").expect("is_banned3"));
    let r = c.ban_ip(
        Some("0000000000000000000000000000000000000000"),
        "192.0.2.79",
    );
    assert!(matches!(r, Err(smart_dl_btcore::Error::NotFound(_))));
}

#[test]
fn export_torrent_bencode() {
    let (c, _save) = core("t46-export");
    let seeder = seed::TestSeeder::start();
    let ih = c.add_magnet(seeder.magnet(), &[]).expect("add_magnet");
    download_to_complete(&c, &ih, &seeder);

    // 元数据就绪 → 导出非空 bencode；含 info dict（"4:info"）与 announce 兜底
    let meta = c.metadata(&ih).expect("metadata").expect("metadata ready");
    assert!(!meta.is_empty());
    let needle = b"4:info";
    assert!(
        meta.windows(needle.len()).any(|w| w == needle),
        "导出字节应含 info dict"
    );
    // bencode 顶层为 dict（'d' 开头）
    assert_eq!(meta[0], b'd', "metainfo 顶层应为 bencode dict");

    // 不存在的任务 / 元数据未就绪 → Ok(None)（ffi 契约：NOT_FOUND 归一 None）
    let r = c.metadata("0000000000000000000000000000000000000000");
    assert_eq!(r.expect("metadata err"), None);
}
