//! M0 E2E 验收测试（TDD 计划 M0 Step 1）：真实磁力 60s 内 progress>0。
//! 依赖：自研 seed_main（本地 2MB 确定性种子，端口监听）；种子经 lt_add_peer 直连注入，无需 tracker。
//! 前置：02_build.ps1 已产出 seed_main.exe；Bare 已链接 lt_kernel（M0 出口前）。
//! TDD 红态说明：Bare 未链接成功前本测试无法运行（链接失败即预期）。

#[path = "../../../tests/integration/seed/mod.rs"]
mod seed;

use std::time::{Duration, Instant};

use smart_dl_btcore::Bare;

#[test]
fn real_magnet_makes_progress_within_60s() {
    let seeder = seed::TestSeeder::start();
    let save = seed::TempDir::new().expect("tempdir");
    let session = Bare::new(save.path(), "m0").expect("session");

    let ih = session
        .add_magnet(seeder.magnet(), &[])
        .expect("add_magnet");
    let (ip, port) = seeder.addr();
    session.resume(&ih).expect("resume");
    session.add_peer(&ih, &ip, port).expect("add_peer(seeder)");

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (p, _state) = session.status(&ih).expect("status");
        if p > 0.0 {
            assert!(p > 0.0, "progress must exceed 0");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "progress stayed 0 for 60s (seeder addr {}:{})",
            ip,
            port
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}
