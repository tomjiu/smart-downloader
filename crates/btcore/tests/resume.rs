//! M1 resume 异步流测试（D16）：request_save_resume → RESUME alert(resume_ready) →
//! take_resume_data → 落盘数据非空 → 新 session add_torrent_resume 回灌 ih 一致。

#[path = "../../../tests/integration/seed/mod.rs"]
mod seed;

use std::time::{Duration, Instant};

use smart_dl_btcore::{AlertKind, BtCore, Error, ResumeBytes};

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
fn save_resume_roundtrip_reproduces_ih() {
    let (c, _save) = core("resume-a");
    c.set_alert_mask(0xFFFF).expect("mask");
    let seeder = seed::TestSeeder::start();
    let ih = c.add_magnet(seeder.magnet(), &[]).expect("add_magnet");
    let (ip, port) = seeder.addr();
    c.resume(&ih).expect("resume");
    c.add_peer(&ih, &ip, port).expect("add_peer");
    download_to_complete(&c, &ih);

    c.request_save_resume(&ih).expect("request");

    // 等 RESUME alert（resume_ready=1）→ take
    let dl = Instant::now() + Duration::from_secs(10);
    let mut saved: Option<ResumeBytes> = None;
    while Instant::now() < dl {
        for a in c.pop_alerts(256).expect("pop") {
            if a.kind == AlertKind::Resume && a.is_resume_ready() {
                saved = Some(c.take_resume_data(&ih).expect("take"));
            }
        }
        if saved.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let saved = saved.expect("10s 内未收到 RESUME·SAVED alert");
    assert!(!saved.is_empty(), "resume 数据非空");
    assert!(saved.len() > 64, "fastresume 应 >64B，实际 {}", saved.len());

    // 回灌：新 session + resume 数据 → 同一 infohash（无 metadata 状态可查）
    let (c2, _save2) = core("resume-b");
    let ih2 = c2
        .add_torrent_resume(saved.as_bytes(), &[])
        .expect("add_torrent_resume");
    assert_eq!(ih2, ih, "resume 回灌 infohash 必须一致");
    let st = c2.status(&ih2).expect("status");
    assert!(!st.metadata_received, "回灌后尚无 metadata（无 seeder）");
}

#[test]
fn take_before_request_is_not_found() {
    let (c, _save) = core("resume-c");
    let seeder = seed::TestSeeder::start();
    let ih = c.add_magnet(seeder.magnet(), &[]).expect("add_magnet");
    let (ip, port) = seeder.addr();
    c.resume(&ih).expect("resume");
    c.add_peer(&ih, &ip, port).expect("add_peer");
    download_to_complete(&c, &ih);

    match c.take_resume_data(&ih) {
        Err(Error::NotFound(_)) => {}
        other => panic!(
            "未 request 时 take 应 NotFound，实际 {:?}",
            other.map(|d| d.len())
        ),
    }
}
