//! M1 alert 扁平化测试（D31）：→ Rust 结构；kind/ih/子类型解析；溢出计数触发快照路径。

#[path = "../../../tests/integration/seed/mod.rs"]
mod seed;

use std::time::{Duration, Instant};

use smart_dl_btcore::{Alert, AlertKind, BtCore, StateSubKind};

fn core() -> (BtCore, seed::TempDir) {
    let save = seed::TempDir::new().expect("tempdir");
    let c = BtCore::new(save.path(), "alerts").expect("session");
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
fn download_emits_metadata_piece_and_finished_state() {
    let (c, _save) = core();
    c.set_alert_mask(0xFFFF).expect("mask");
    let seeder = seed::TestSeeder::start();
    let ih = c.add_magnet(seeder.magnet(), &[]).expect("add_magnet");
    let (ip, port) = seeder.addr();
    c.resume(&ih).expect("resume");
    c.add_peer(&ih, &ip, port).expect("add_peer");
    download_to_complete(&c, &ih);

    let all = drain_all(&c, Duration::from_secs(5));
    let kinds: Vec<AlertKind> = all.iter().map(|a| a.kind).collect();
    let ihs_ok = all.iter().filter(|a| !a.ih.is_empty()).all(|a| a.ih == ih);
    assert!(ihs_ok, "torrent 类 alert 的 ih 应等于下载 ih");

    assert!(
        kinds.contains(&AlertKind::Metadata),
        "缺少 metadata alert: {:?}",
        kinds
    );
    assert!(
        kinds.contains(&AlertKind::Piece),
        "缺少 piece finished alert: {:?}",
        kinds
    );
    // STATE·FINISHED：msg 前缀约定（§8.5），子类型解析
    let finished = all
        .iter()
        .find(|a| a.kind == AlertKind::State && a.state_subkind() == StateSubKind::Finished);
    assert!(finished.is_some(), "缺少 STATE·FINISHED: {:?}", all);
}

#[test]
fn kind_mapping_covers_all_buckets() {
    // 7 桶扁平化映射（§8.4 预算）+ STATE 子类型解析（纯单元，无需 seeder）
    use smart_dl_btcore::AlertKind;
    let cases = [
        (1, AlertKind::Tracker),
        (2, AlertKind::Peer),
        (4, AlertKind::Error),
        (8, AlertKind::Metadata),
        (16, AlertKind::State),
        (32, AlertKind::Resume),
        (64, AlertKind::Piece),
        (999, AlertKind::Other(999)),
    ];
    for (raw, expect) in cases {
        assert_eq!(AlertKind::from(raw), expect, "raw={}", raw);
    }
    let state = smart_dl_btcore::Alert {
        kind: AlertKind::State,
        ih: String::new(),
        msg: "torrent finished downloading".into(),
        at: 0,
        resume_ready: false,
    };
    assert_eq!(state.state_subkind(), StateSubKind::Finished);
    let paused = smart_dl_btcore::Alert {
        kind: AlertKind::State,
        ih: String::new(),
        msg: "torrent paused".into(),
        at: 0,
        resume_ready: false,
    };
    assert_eq!(paused.state_subkind(), StateSubKind::Paused);
    assert!(!paused.is_resume_ready());
    let resume_ready = smart_dl_btcore::Alert {
        kind: AlertKind::Resume,
        ih: String::new(),
        msg: String::new(),
        at: 0,
        resume_ready: true,
    };
    assert!(resume_ready.is_resume_ready());
}

#[test]
fn dropped_counter_counts_unmasked_alerts() {
    let (c, _save) = core();
    c.set_alert_mask(0xFFFF).expect("mask");
    let seeder = seed::TestSeeder::start();
    let ih = c.add_magnet(seeder.magnet(), &[]).expect("add_magnet");
    let (ip, port) = seeder.addr();
    c.resume(&ih).expect("resume");
    c.add_peer(&ih, &ip, port).expect("add_peer");
    download_to_complete(&c, &ih);

    // 全屏蔽：后续产生的 alert 全部丢弃并计数（快照补拉路径的数据源）
    c.set_alert_mask(0).expect("mask off");
    std::thread::sleep(Duration::from_millis(1500));
    let _ = c.pop_alerts(64).expect("pop");
    let dropped = c.alerts_dropped().expect("dropped");
    assert!(dropped > 0, "屏蔽后应有丢弃计数，实际 {}", dropped);
}

/// 反复 pop 直到无新 alert（带上限）
fn drain_all(c: &BtCore, timeout: Duration) -> Vec<Alert> {
    let mut out = Vec::new();
    let dl = Instant::now() + timeout;
    loop {
        let batch = c.pop_alerts(256).expect("pop");
        if batch.is_empty() {
            if Instant::now() >= dl {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        for b in batch {
            out.push(b);
        }
    }
    out
}
