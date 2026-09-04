//! M1 状态/进度/控制测试：status 单调、piece_count/bitfield、file_progress、
//! limits/tracker/sequential/read_piece、暂停-恢复-移除流、无 seeder 元数据状态。

#[path = "../../../tests/integration/seed/mod.rs"]
mod seed;

use std::time::{Duration, Instant};

use smart_dl_btcore::BtCore;

const FILE_SIZE: i64 = 2 * 1024 * 1024; // seed_main 确定性 2MB
const PIECE_LEN: i64 = 16384;
const PIECES: usize = (FILE_SIZE / PIECE_LEN) as usize; // 128

fn core(tag: &str) -> (BtCore, seed::TempDir) {
    let save = seed::TempDir::new().expect("tempdir");
    let c = BtCore::new(save.path(), tag).expect("session");
    (c, save)
}

fn download_to_complete(c: &BtCore, ih: &str) {
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
fn status_metrics_and_geometry() {
    let (c, _save) = core("geo");
    let seeder = seed::TestSeeder::start();
    let ih = c.add_magnet(seeder.magnet(), &[]).expect("add_magnet");
    let (ip, port) = seeder.addr();

    // 无 seeder 连接前：元数据未获取 → state 4
    let st0 = c.status(&ih).expect("status0");
    assert_eq!(st0.state, 4, "未连 peers 时应为 downloading_metadata(4)");
    assert!(!st0.metadata_received);
    assert_eq!(c.piece_count(&ih).expect("piece0"), 0);
    assert!(
        c.bitfield(&ih).expect("bitfield0").is_empty(),
        "元数据前 bitfield 空"
    );

    c.resume(&ih).expect("resume");
    c.add_peer(&ih, &ip, port).expect("add_peer");
    // 下载中：metadata 已收，peers ≥1，progress 单调上升
    let dl = Instant::now() + Duration::from_secs(60);
    let mut last = 0.0f32;
    loop {
        let st = c.status(&ih).expect("status");
        assert!(
            st.metadata_received || st.progress == 0.0,
            "metadata 后才能有进度"
        );
        assert!(
            st.progress >= last - 1e-6,
            "progress 应单调: {} -> {}",
            last,
            st.progress
        );
        last = st.progress;
        assert!((0.0..=1.0).contains(&st.progress));
        assert!(st.downloaded <= st.total);
        if st.progress >= 1.0 && st.state == 1 {
            break;
        }
        if st.progress > 0.0 {
            assert!(st.num_peers >= 1, "有进度时应有已连 peer");
        }
        assert!(Instant::now() < dl, "30s 未完成");
        std::thread::sleep(Duration::from_millis(200));
    }

    // 形状：128 pieces；bitfield 16 字节全 1
    assert_eq!(c.piece_count(&ih).expect("pieces"), PIECES as i32);
    let bf = c.bitfield(&ih).expect("bitfield");
    assert_eq!(bf.len(), PIECES.div_ceil(8), "bitfield 字节数");
    assert!(
        bf.iter().all(|&b| b == 0xFF),
        "完成后 bitfield 全 1: {:?}",
        bf
    );

    // 文件：单文件 2MB；进度=大小
    assert_eq!(c.file_count(&ih).expect("files"), 1);
    let fp = c.file_progress(&ih).expect("file_progress");
    assert_eq!(fp.len(), 1);
    assert_eq!(fp[0].1, FILE_SIZE, "文件大小 2MB");
    assert_eq!(fp[0].0, FILE_SIZE, "完成后已下载=大小");
    let st = c.status(&ih).expect("status_final");
    assert_eq!(st.total, FILE_SIZE);
    assert_eq!(st.downloaded, FILE_SIZE);
}

#[test]
fn control_ops_and_read_piece() {
    let (c, _save) = core("ctrl");
    let seeder = seed::TestSeeder::start();
    let ih = c.add_magnet(seeder.magnet(), &[]).expect("add_magnet");
    let (ip, port) = seeder.addr();
    c.resume(&ih).expect("resume");
    c.add_peer(&ih, &ip, port).expect("add_peer");
    download_to_complete(&c, &ih);

    // 控制面
    c.set_limits(&ih, 0, 0).expect("limits unrestricted");
    c.set_alert_mask(0xFFFF).expect("mask");
    c.set_limits(&ih, 1 << 20, 1 << 20).expect("limits 1MB");
    c.set_sequential(&ih, true).expect("sequential on");
    c.add_tracker(&ih, "http://127.0.0.1:9/announce")
        .expect("add_tracker");
    c.add_url_seed(&ih, "http://127.0.0.1:9/seed")
        .expect("add_url_seed");

    // finished 状态下 read_piece 可能不生成 alert；先 resume 进入 seeding 再读
    c.resume(&ih).expect("resume before read_piece");
    let dl = Instant::now() + Duration::from_secs(30);
    let mut data: Option<Vec<u8>> = None;
    while Instant::now() < dl {
        match c.read_piece(&ih, 0) {
            Ok(Some(d)) => {
                eprintln!("read_piece OK len={}", d.len());
                data = Some(d);
                break;
            }
            Ok(None) => {
                let _ = c.pop_alerts(64);
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => panic!("read_piece 错误: {}", e),
        }
    }
    let data = data.expect("30s 内 piece0 未就绪");
    assert_eq!(data.len(), PIECE_LEN as usize, "piece 0 长度");

    // 移除（不删数据）→ 状态 NotFound
    c.remove(&ih, false).expect("remove");
    match c.status(&ih) {
        Err(smart_dl_btcore::Error::NotFound(_)) => {}
        other => panic!("移除后 status 应 NotFound，实际 {:?}", other),
    }
}

#[test]
fn pause_resume_flow() {
    // pause → torrent_paused alert；resume 后状态可查（ABI100：状态停在暂停前值，§10.1）
    let (c, _save) = core("pr");
    c.set_alert_mask(0xFFFF).expect("mask");
    let seeder = seed::TestSeeder::start();
    let ih = c.add_magnet(seeder.magnet(), &[]).expect("add_magnet");
    let (ip, port) = seeder.addr();
    c.resume(&ih).expect("resume");
    c.add_peer(&ih, &ip, port).expect("add_peer");
    download_to_complete(&c, &ih);

    c.resume(&ih).expect("resume");
    c.pause(&ih).expect("pause");
    // paused 标志由 lt_torrent_status::paused 提供，作为暂停同步点（比 alert 更即时）
    let st = c.status(&ih).expect("status_paused");
    assert!(st.paused, "pause 后 status.paused 应为 true");
    assert_eq!(st.progress, 1.0, "暂停后 progress 保持 1.0");

    // 辅助校验：pause 后应收到 torrent_paused alert（libtorrent 异步投递，并行下可能延迟）
    let dl = Instant::now() + Duration::from_secs(20);
    let mut got_alert = false;
    while Instant::now() < dl {
        let alerts = c.pop_alerts(256).expect("pop");
        if alerts.iter().any(|a| {
            a.kind == smart_dl_btcore::AlertKind::State
                && a.state_subkind() == smart_dl_btcore::StateSubKind::Paused
        }) {
            got_alert = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(got_alert, "pause 后应在 20s 内收到 torrent_paused alert");

    c.resume(&ih).expect("resume");
    let st = c.status(&ih).expect("status_resumed");
    assert_eq!(st.progress, 1.0);
    assert!(!st.paused, "resume 后 status.paused 应为 false");
}

/// E33：全生命周期累计上/下行透出（lt_torrent_status::all_time_*）。
/// 注意冲账时机：libtorrent 的 all_time 计数器（m_total_downloaded +=
/// m_stat.last_payload_downloaded()）只在 session second_tick（≈1s 节拍）
/// 落账——progress 到 1.0 后立即读可能仍是 0（本次 2MB 环回下载实测如
/// 此）。累计统计的展示语义本就是秒级（qBittorrent 同款轮询口径），
/// 快照读数容忍一个 tick 的滞后属可接受设计，此处轮询等待冲账。
#[test]
fn all_time_totals_exposed() {
    let (c, _save) = core("totals");
    let seeder = seed::TestSeeder::start();
    let ih = c.add_magnet(seeder.magnet(), &[]).expect("add_magnet");
    let (ip, port) = seeder.addr();
    c.resume(&ih).expect("resume");
    c.add_peer(&ih, &ip, port).expect("add_peer");
    download_to_complete(&c, &ih);

    let deadline = Instant::now() + Duration::from_secs(15);
    let st = loop {
        let st = c.status(&ih).expect("status");
        if st.all_time_download > 0 {
            break st;
        }
        assert!(
            Instant::now() < deadline,
            "15s 内 all_time_download 未冲账为非零（tick 未落账）: {st:?}"
        );
        std::thread::sleep(Duration::from_millis(500));
    };
    assert!(
        st.all_time_download >= st.downloaded,
        "累计下行应 >= 本次 done（含 hashfail/重复收块历史口径）: {} < {}",
        st.all_time_download,
        st.downloaded
    );
    assert!(st.all_time_upload >= 0);
}
