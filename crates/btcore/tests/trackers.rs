//! E29：tracker 运行时增删查（真实 libtorrent；magnet 任务 metadata 前即可操作）。
//! 契约：add_tracker 追加 → list_trackers 两段式列举 → remove_tracker URL
//! 精确删除（无匹配 → NotFound 定性错误）。
//! 依赖：libtorrent 链接环境（与 m0/magnet_metadata 同前置）。
//! 注：tracker 增删查属 handle 级配置操作，无需真实 seeder/网络——
//! 静态合法 magnet 即可（不触碰下载链路），故不引入 seed harness。

use smart_dl_btcore::BtCore;

const MAGNET: &str = "magnet:?xt=urn:btih:0d2c9c9d5c2d3e8f9a1b2c3d4e5f6a7b8c9d0e1f";
const T1: &str = "http://tracker.example/announce";
const T2: &str = "udp://tracker2.example:1337/announce";

#[test]
fn tracker_add_list_remove_roundtrip() {
    let save = std::env::temp_dir().join(format!("e29-trackers-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&save);
    std::fs::create_dir_all(&save).expect("save dir");

    let core = BtCore::new(&save, "trackers-e29").expect("session");
    let ih = core.add_magnet(MAGNET, &[]).expect("add_magnet");
    core.resume(&ih).expect("resume");

    // 种子 magnet 无 tr 参数 → 初始 tracker 表为空
    let before = core.list_trackers(&ih).expect("list_trackers");
    assert!(before.is_empty(), "初始表应为空: {before:?}");

    // 追加两条（metadata 未就绪也可设——announce 表属 handle 级配置）
    core.add_tracker(&ih, T1).expect("add T1");
    core.add_tracker(&ih, T2).expect("add T2");
    let listed = core.list_trackers(&ih).expect("list after add");
    assert_eq!(listed.len(), 2, "追加后应有两条: {listed:?}");
    let urls: Vec<&str> = listed.iter().map(|t| t.url.as_str()).collect();
    assert!(urls.contains(&T1) && urls.contains(&T2));
    assert!(
        listed.iter().all(|t| t.tier == 0),
        "默认 tier 应为 0: {listed:?}"
    );

    // 精确删除 T1 → 表中仅剩 T2
    core.remove_tracker(&ih, T1).expect("remove T1");
    let after = core.list_trackers(&ih).expect("list after remove");
    assert_eq!(after.len(), 1, "删除后应剩一条: {after:?}");
    assert_eq!(after[0].url, T2);

    // 删不存在的 tracker → NotFound 定性（daemon 映射 404）
    let err = core.remove_tracker(&ih, T1).expect_err("重复删除应报错");
    assert!(
        matches!(err, smart_dl_btcore::Error::NotFound(_)),
        "重复删除应定性 NotFound: {err:?}"
    );

    // 不存在的任务 → NotFound
    let fake = "0123456789abcdef0123456789abcdef01234567";
    let err2 = core.list_trackers(fake).expect_err("未知任务应报错");
    assert!(
        matches!(err2, smart_dl_btcore::Error::NotFound(_)),
        "未知任务应定性 NotFound: {err2:?}"
    );

    let _ = std::fs::remove_dir_all(&save);
}
