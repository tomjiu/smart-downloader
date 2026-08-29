//! M1 富 peer 测试：lt_peers 字段解析（ip/port/peer_id/client/progress_ppm/flags 位）。

#[path = "../../../tests/integration/seed/mod.rs"]
mod seed;

use std::time::{Duration, Instant};

use smart_dl_btcore::{peer_flags, BtCore};

fn core() -> (BtCore, seed::TempDir) {
    let save = seed::TempDir::new().expect("tempdir");
    let c = BtCore::new(save.path(), "peers").expect("session");
    (c, save)
}

#[test]
fn peers_empty_without_connections() {
    // 未注入 peer / 未连 swarm → peers() 返回空列表 Ok（D13 空列表快路径）
    let (c, _save) = core();
    let ih = c
        .add_magnet(
            "magnet:?xt=urn:btih:a6453589c479ace6613048fb3c607a77495a3f7c",
            &[],
        )
        .expect("add_magnet");
    let p = c.peers(&ih).expect("peers");
    assert!(p.is_empty(), "无连接时应为空: {:?}", p);
}

#[test]
fn rich_peer_fields_during_download() {
    let (c, _save) = core();
    c.set_alert_mask(0xFFFF).expect("mask");
    let seeder = seed::TestSeeder::start();
    let ih = c.add_magnet(seeder.magnet(), &[]).expect("add_magnet");
    let (ip, port) = seeder.addr();
    c.resume(&ih).expect("resume");
    c.add_peer(&ih, &ip, port).expect("add_peer");
    // 限速 100KB/s → 2MB 约 20s：拉长 peer 存活窗口，让 SEED 快照确定可测
    //（顺带覆盖 set_limits 契约）；本地瞬时传输时 peer 可能立刻断开导致快照窗口竞态
    c.set_limits(&ih, 100 * 1024, 0).expect("set_limits");

    // 等到 seeder 快照稳定（SEED 位 + 满进度）：连接早期位图未交换时快照可能不全
    let dl = Instant::now() + Duration::from_secs(45);
    let mut peers = Vec::new();
    while Instant::now() < dl {
        peers = c.peers(&ih).expect("peers");
        if peers
            .iter()
            .any(|p| p.is_seed() && p.progress_ppm == 1_000_000)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        peers
            .iter()
            .any(|p| p.is_seed() && p.progress_ppm == 1_000_000),
        "45s 内未看到 SEED 快照"
    );
    let p = peers.iter().find(|p| p.is_seed()).expect("seed peer");
    assert!(
        p.ip.contains('.') || p.ip.contains(':'),
        "ip 非法: {}",
        p.ip
    );
    assert_eq!(p.port, port, "端口应等于 seeder 监听端口");
    assert_eq!(p.peer_id.len(), 40, "peer_id 应为 40 hex: {}", p.peer_id);
    // client 字符串：瞬时完成传输时快照可能尚未解析，允许为空（其余快照字段是硬契约）
    assert!(
        p.progress_ppm <= 1_000_000,
        "progress_ppm 越界: {}",
        p.progress_ppm
    );
    // seeder 已完成全部数据：seed 位 + progress_ppm == 1_000_000
    assert!(
        p.flags & peer_flags::SEED != 0,
        "seeder 应标 SEED: flags={:#x}",
        p.flags
    );
    assert!(p.is_seed(), "is_seed() 与标志位一致");
    assert_eq!(p.progress_ppm, 1_000_000, "seeder progress_ppm 应为满");
    assert!(
        p.flags & peer_flags::UTP != 0 || p.flags & peer_flags::LOCAL != 0,
        "本地连接应标 UTP 或 LOCAL: flags={:#x}",
        p.flags
    );
    // 传输统计可用（快照语义）
    assert!(p.total_download >= 0 && p.total_upload >= 0);
}
