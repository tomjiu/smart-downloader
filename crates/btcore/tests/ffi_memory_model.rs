//! M1 内存模型测试（D13）：缓冲扩容重试；字符串立即拷贝；
//! 未找到/错误文本路径。直接使用 ffi::Session（safe 包装）验证契约层行为。

#[path = "../../../tests/integration/seed/mod.rs"]
mod seed;

use std::time::{Duration, Instant};

use smart_dl_btcore::ffi::Session;

fn session() -> Session {
    let save = seed::TempDir::new().expect("tempdir");
    Session::new(save.path(), "mem").expect("session")
}

/// 下载到完成（本地 seeder 直连）
fn download_to_complete(s: &Session, ih: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let st = s.status(ih).expect("status");
        if st.progress >= 1.0 && st.state == 1 {
            return;
        }
        assert!(Instant::now() < deadline, "did not finish in 60s");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn string_copies_survive_following_pops() {
    let seeder = seed::TestSeeder::start();
    let s = session();
    s.set_alert_mask(0xFFFF).expect("mask");
    let ih = s.add_magnet(seeder.magnet(), &[]).expect("add_magnet");
    let (ip, port) = seeder.addr();
    s.resume(&ih).expect("resume");
    s.add_peer(&ih, &ip, port).expect("add_peer");
    download_to_complete(&s, &ih);

    // 第一批：抓 metadata / piece / finished 中任一文本
    let first = s.pop_alerts(64).expect("pop1");
    let stash: Vec<(i32, String)> = first.iter().map(|a| (a.kind, fstr(&a.msg))).collect();
    assert!(!stash.is_empty(), "download 应产生 alert");
    // 第二批：内核 ring 被清空/复用，Rust 侧持有拷贝必须不变
    let _second = s.pop_alerts(64).expect("pop2");
    for (i, (kind, msg)) in stash.iter().enumerate() {
        assert_eq!(first[i].kind, *kind);
        assert_eq!(fstr(&first[i].msg), *msg, "pop 后旧值被 C++ 侧复写");
    }
}

#[test]
fn grow_retry_on_take_resume_data() {
    // take_resume_data 从 cap=0 起自动扩容重试（内核 LT_ERR_BUFFER_TOO_SMALL → out_len）
    let seeder = seed::TestSeeder::start();
    let s = session();
    s.set_alert_mask(0xFFFF).expect("mask");
    let ih = s.add_magnet(seeder.magnet(), &[]).expect("add_magnet");
    let (ip, port) = seeder.addr();
    s.resume(&ih).expect("resume");
    s.add_peer(&ih, &ip, port).expect("add_peer");
    download_to_complete(&s, &ih);

    s.request_save_resume(&ih).expect("request");
    let dl = Instant::now() + Duration::from_secs(10);
    let mut data: Option<Vec<u8>> = None;
    while Instant::now() < dl {
        let _ = s.pop_alerts(64).expect("pop");
        match s.take_resume_data(&ih) {
            Ok(d) => {
                data = Some(d);
                break;
            }
            Err(smart_dl_btcore::Error::NotFound(_)) => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => panic!("unexpected: {}", e),
        }
    }
    let data = data.expect("10s 内 resume 未就绪");
    assert!(!data.is_empty(), "resume bencode 非空");
    assert!(
        data.len() > 64,
        "fastresume 应大于 64B，实际 {}",
        data.len()
    );
}

#[test]
fn not_found_reports_err_str() {
    let s = session();
    // 40 位合法 hex 但未添加任何 torrent → NOT_FOUND
    let ghost = "a6453589c479ace6613048fb3c607a77495a3f7c";
    match s.status(ghost) {
        Err(smart_dl_btcore::Error::NotFound(_)) => {}
        other => panic!("期望 NotFound，实际 {:?}", other),
    }
    let msg = s.err_str().expect("err_str");
    assert!(msg.contains("torrent not found"), "err_str={}", msg);
}

fn fstr<const N: usize>(arr: &[std::os::raw::c_char; N]) -> String {
    let bytes: Vec<u8> = arr
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}
