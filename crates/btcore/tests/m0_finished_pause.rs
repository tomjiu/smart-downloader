//! M0 spike 附带验证项：做种停止路径 torrent_finished → lt_pause（§10.1 / D19 / D32）。
//! 验证点：客户端本地 seeder 下载至完成后，主动 lt_pause；
//!   断言 (1) 收到 torrent_finished（STATE 桶，msg 含 "torrent finished"），
//!   (2) 之后收到 torrent_paused（STATE 桶，msg 含 "torrent paused"），且 finished 先于 paused；
//!   (3) pause 后状态不再推进（progress 保持 1.0）。
//! 结论写回设计文档 §10.1。

#[path = "../../../tests/integration/seed/mod.rs"]
mod seed;

use std::time::{Duration, Instant};

use smart_dl_btcore::Bare;

#[test]
fn torrent_finished_then_pause_ordering() {
    let seeder = seed::TestSeeder::start();
    let save = seed::TempDir::new().expect("tempdir");
    let session = Bare::new(save.path(), "m0-pause").expect("session");
    session.diag_set_mask(0xFFFF).expect("set_mask");

    let ih = session
        .add_magnet(seeder.magnet(), &[])
        .expect("add_magnet");
    let (ip, port) = seeder.addr();
    session.resume(&ih).expect("resume");
    session.add_peer(&ih, &ip, port).expect("add_peer");

    // 下载直到完成（progress 1.0）
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (p, state) = session.status(&ih).expect("status");
        if p >= 1.0 && state == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "did not finish in 60s (p={} state={})",
            p,
            state
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    // 完成 → 主动 pause
    session.pause(&ih).expect("pause");

    // 收集直到出现 torrent_paused（最多 10s）
    let mut saw_finished_idx: Option<usize> = None;
    let mut saw_paused_idx: Option<usize> = None;
    let mut collected: Vec<String> = Vec::new();
    let dl = Instant::now() + Duration::from_secs(10);
    while saw_paused_idx.is_none() && Instant::now() < dl {
        for a in session.diag_pop_alerts().expect("pop") {
            collected.push(a.clone());
            let idx = collected.len() - 1;
            if a.contains("torrent finished") && saw_finished_idx.is_none() {
                saw_finished_idx = Some(idx);
            }
            if a.contains("torrent paused") && saw_paused_idx.is_none() {
                saw_paused_idx = Some(idx);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let fin = saw_finished_idx.expect("torrent_finished alert 未出现");
    let paused_msg = format!(
        "torrent_paused alert 未出现（10s 内）; 状态={:?}; 收集={:?}",
        session.status(&ih).unwrap_or((-1.0, -1)),
        collected
    );
    let pau = saw_paused_idx.expect(&paused_msg);
    assert!(
        fin < pau,
        "torrent_finished(idx={}) 必须先于 torrent_paused(idx={})  全部: {:?}",
        fin,
        pau,
        collected
    );

    // 暂停后不再有数据传输（同步点 = torrent_paused alert，D19/D32）。
    // 注意：ABI100 无 flags_t，torrent_status::state 不反映暂停（状态停在完成），
    // 暂停状态由引擎层以 torrent_paused alert 维护（结论写回 §10.1）。
    let (p, _state) = session.status(&ih).expect("status");
    assert_eq!(p, 1.0, "pause 后 progress 保持 1.0");

    println!(
        "OK: finished@{} < paused@{}（{} 条 alert）",
        fin,
        pau,
        collected.len()
    );
}
