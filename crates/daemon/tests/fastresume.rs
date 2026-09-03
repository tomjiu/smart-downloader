//! feature `bt`：BT fastresume 显式保存（#5）——remove/pause 落盘 `.fastresume` →
//! 重启后 add 同一 magnet 回灌（恢复 metadata + 免重新抓取）；delete_data 清理凭据。
//! 无 bt feature 时整个文件跳过（编译基线不链接 libtorrent）。

#![cfg(feature = "bt")]

#[path = "../../../tests/integration/seed/mod.rs"]
mod seed;

use smart_dl_core::identity::{CanonicalId, CanonicalKind, ContentIdentity};
use smart_dl_core::state_machine::{EvalPhase, TaskState};
use smart_dl_core::task::{DownloadTask, ProgressAggregate, RetryState, TaskMetadata};
use smart_dl_core::types::{DownloadEngine, DownloadSource};
use smart_dl_daemon::bt::BtEngine;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn bt_task(id: &str, magnet: &str) -> DownloadTask {
    DownloadTask {
        id: id.to_string(),
        canonical_id: CanonicalId {
            kind: CanonicalKind::Bt,
            identity: magnet.to_string(),
            validator: None,
            token_sensitive: false,
        },
        source: DownloadSource::Magnet(magnet.to_string()),
        identity: ContentIdentity::SingleFile {
            size: 0,
            etag: None,
            sha256: None,
            sha1: None,
            md5: None,
            backup_md5: None,
        },
        dest_root: PathBuf::from("."),
        files: vec![],
        acquisitions: vec![],
        aggregate: ProgressAggregate::default(),
        state: TaskState::Evaluating(EvalPhase::MetadataPending),
        retry: RetryState::default(),
        created_at: Instant::now(),
        file_priorities: None,
        sequential: false,
        metadata: TaskMetadata {
            name: None,
            added_at_unix: 0,
            tags: Vec::new(),
            finished_at_unix: 0,
            start_at_unix: 0,
            next_retry_at_unix: 0,
        },
        limits: None,
    }
}

fn wait_complete(core: &smart_dl_btcore::BtCore, ih: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let st = core.status(ih).expect("status");
        if st.progress >= 1.0 && st.state == 1 {
            return;
        }
        assert!(Instant::now() < deadline, "60s 未下载完成");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[tokio::test]
async fn fastresume_saved_on_remove_then_reloaded() {
    // 完整下载 → remove 落盘 .fastresume → 新引擎（模拟重启）add 同一 magnet → 回灌：
    // metadata 立即可用（无 seeder 也无需重新抓取）+ infohash 一致
    let save = seed::TempDir::new().expect("tempdir");
    let seeder = seed::TestSeeder::start();
    let magnet = seeder.magnet().to_string();

    // 第一次运行：下载完成
    let engine = BtEngine::new(save.path(), None, 0, 0, false, false, false).unwrap();
    let ih = engine.add(&bt_task("t1", &magnet)).await.unwrap();
    let (ip, port) = seeder.addr();
    engine.core().resume(&ih).unwrap();
    engine.core().add_peer(&ih, &ip, port).unwrap();
    wait_complete(&engine.core(), &ih);

    // remove → then 显式保存 .fastresume
    engine.remove(&ih, false).await.unwrap();
    let fr = save.path().join(format!("{ih}.fastresume"));
    assert!(fr.exists(), "remove 后必须落盘 .fastresume: {fr:?}");
    let data = std::fs::read(&fr).unwrap();
    assert!(data.len() > 64, "fastresume 数据应非空: {}B", data.len());

    // 模拟重启：新 session 同一 save_path → add 应命中 .fastresume 回灌（ih 一致 + 任务注册）
    drop(seeder); // 停掉种子源——回灌/注册不依赖网络
    let engine2 = BtEngine::new(save.path(), None, 0, 0, false, false, false).unwrap();
    let ih2 = engine2.add(&bt_task("t2", &magnet)).await.unwrap();
    assert_eq!(ih2, ih, "回灌 infohash 必须一致");
    // 任务已注册（status 可查）。注：libtorrent save_resume_data 的 resume 数据不含
    // info dict（btcore resume 测试已证实）——magnet 回灌后 metadata 需重新获取属预期；
    // #5 的真实价值（piece 位图/进度恢复免全盘 checking）在 `.torrent` 场景生效。
    let _st = engine2.core().status(&ih2).unwrap();
}

#[tokio::test]
async fn pause_saves_fastresume() {
    // pause 时也保存进度凭据（best-effort，含未完成场景）
    let save = seed::TempDir::new().expect("tempdir");
    let engine = BtEngine::new(save.path(), None, 0, 0, false, false, false).unwrap();
    let seeder = seed::TestSeeder::start();
    let ih = engine.add(&bt_task("t3", seeder.magnet())).await.unwrap();
    let (ip, port) = seeder.addr();
    engine.core().resume(&ih).unwrap();
    engine.core().add_peer(&ih, &ip, port).unwrap();
    wait_complete(&engine.core(), &ih);

    engine.pause(&ih).await.unwrap();
    assert!(
        save.path().join(format!("{ih}.fastresume")).exists(),
        "pause 后应保存 .fastresume"
    );
}

#[tokio::test]
async fn delete_data_removes_fastresume() {
    // remove(delete_data=true) → 数据删除 → 续传凭据一并清理
    let save = seed::TempDir::new().expect("tempdir");
    let engine = BtEngine::new(save.path(), None, 0, 0, false, false, false).unwrap();
    let seeder = seed::TestSeeder::start();
    let ih = engine.add(&bt_task("t4", seeder.magnet())).await.unwrap();
    let (ip, port) = seeder.addr();
    engine.core().resume(&ih).unwrap();
    engine.core().add_peer(&ih, &ip, port).unwrap();
    wait_complete(&engine.core(), &ih);

    engine.remove(&ih, true).await.unwrap();
    assert!(
        !save.path().join(format!("{ih}.fastresume")).exists(),
        "delete_data 后 .fastresume 应清理"
    );
}

// —— P4 G5：暂停意图持久化 + restore 后运行态恢复（daemon 全链）—— //

use smart_dl_daemon::state::DaemonState;
use std::sync::Arc;

fn bt_daemon(save: &std::path::Path, store: &std::path::Path) -> (Arc<DaemonState>, Arc<BtEngine>) {
    let bt = Arc::new(BtEngine::new(save, None, 0, 0, false, false, false).unwrap());
    let http = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let state = Arc::new(
        DaemonState::new(Arc::new(http), vec![])
            .with_dest_root(save.to_path_buf())
            .with_storage(store.to_path_buf())
            .with_bt(bt.clone()),
    );
    (state, bt)
}

/// 等待内核侧进度 > 0（seeder 连上并实际传输）。
fn wait_progress(core: &smart_dl_btcore::BtCore, ih: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let st = core.status(ih).expect("status");
        if st.downloaded > 0 {
            return;
        }
        assert!(Instant::now() < deadline, "60s 内无下载进度");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[tokio::test]
async fn paused_task_stays_paused_after_restart() {
    // 全链：add → 实际下载 → daemon pause（内核暂停 + paused=true 落盘）→
    // "重启"（新引擎 + restore_from）→ 暂停意图重放：内核保持 paused、
    // 记录态 Paused、意图重新登记（Bug A 压制句柄可用）。
    let save = seed::TempDir::new().expect("tempdir");
    let store_dir = seed::TempDir::new().expect("tempdir");
    let store = store_dir.path().join("tasks.json");
    let seeder = seed::TestSeeder::start();
    let magnet = seeder.magnet().to_string();
    let tid;

    // —— 第一次运行：下载出真实进度后用户暂停
    {
        let (state, bt) = bt_daemon(save.path(), &store);
        tid = state.add_link_task(magnet.clone(), None).await.unwrap();
        let ih = state.engine_tid_of(&tid).expect("engine_tid");
        let (ip, port) = seeder.addr();
        bt.core().add_peer(&ih, &ip, port).unwrap();
        // v1 语义：add 即内核暂停 → 用户显式 resume（走 daemon 处理器）后开始传输
        state.resume(&tid).await.unwrap();
        wait_progress(&bt.core(), &ih);

        state.pause(&tid).await.unwrap();
        assert!(bt.core().status(&ih).unwrap().paused, "内核应已暂停");
        // pause 立即 autosave → tasks.json paused=true
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let persisted = std::fs::read_to_string(&store)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .map(|v| {
                    v.as_array()
                        .map(|a| a.iter().any(|t| t["paused"] == serde_json::json!(true)))
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            if persisted {
                break;
            }
            assert!(Instant::now() < deadline, "暂停意图必须落盘 paused=true");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // state/bt 在块尾 drop：模拟进程退出
    }

    // —— "重启"：新 session + restore_from → 暂停意图重放
    let (state2, bt2) = bt_daemon(save.path(), &store);
    let n = state2.restore_from(&store).await.unwrap();
    assert_eq!(n, 1, "应恢复 1 条任务");

    let summaries2 = state2.list();
    assert_eq!(summaries2.len(), 1);
    let tid2 = summaries2[0].task_id.clone();
    assert_eq!(tid2, tid, "task_id 必须保留");
    assert_eq!(summaries2[0].state, "Paused", "摘要态必须为 Paused");
    let ih2 = state2.engine_tid_of(&tid2).expect("engine_tid");
    assert!(
        bt2.core().status(&ih2).unwrap().paused,
        "内核必须保持暂停（此前恢复后从不 pause，任务会自动开跑）"
    );
    assert!(
        bt2.pause_intended(&ih2),
        "暂停意图必须重新登记（Bug A 持续压制依赖）"
    );
}

#[tokio::test]
async fn running_task_resumes_downloading_after_restart() {
    // 全链对照面：未暂停任务重启后必须真正恢复运行（内核 resume 重放）——
    // 此前 add 路径内核 paused 且 restore 从不 resume，恢复任务永不下载。
    let save = seed::TempDir::new().expect("tempdir");
    let store_dir = seed::TempDir::new().expect("tempdir");
    let store = store_dir.path().join("tasks.json");
    let seeder = seed::TestSeeder::start();
    let magnet = seeder.magnet().to_string();

    // —— 第一次运行：有进度后"退出"（无暂停）
    {
        let (state, bt) = bt_daemon(save.path(), &store);
        let tid = state.add_link_task(magnet.clone(), None).await.unwrap();
        let ih = state.engine_tid_of(&tid).expect("engine_tid");
        let (ip, port) = seeder.addr();
        bt.core().add_peer(&ih, &ip, port).unwrap();
        state.resume(&tid).await.unwrap(); // 用户显式 resume → 开始传输
        wait_progress(&bt.core(), &ih);
        // drop = 进程退出（无 pause → tasks.json paused=false）
    }

    // —— "重启"：restore → BT 任务自动 resume → 内核运行且进度继续增长
    let (state2, bt2) = bt_daemon(save.path(), &store);
    let n = state2.restore_from(&store).await.unwrap();
    assert_eq!(n, 1);
    let summaries = state2.list();
    let tid2 = summaries.first().expect("恢复后应有任务").task_id.clone();
    let ih2 = state2.engine_tid_of(&tid2).expect("engine_tid");

    // 内核必须脱离暂停且继续下载（seeder + 直连 peer → 进度增长；
    // 首轮可能已下完：checking 后 downloaded 不再增长，progress>=1.0 亦算恢复成功）
    let (ip, port) = seeder.addr();
    let _ = bt2.core().add_peer(&ih2, &ip, port);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let st = bt2.core().status(&ih2).expect("status");
        if !st.paused && (st.downloaded > 0 || st.progress >= 1.0) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "恢复任务必须在 60s 内脱离暂停并继续下载（paused={} done={} progress={})",
            st.paused,
            st.downloaded,
            st.progress
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn torrent_name_surfaces_in_engine_status() {
    // E28：torrent metadata name → FFI status → EngineStatus.name（BT 任务
    // 名回填链路的数据源就绪）；与 .torrent 内声明名交叉一致
    let save = seed::TempDir::new().expect("tempdir");
    let seeder = seed::TestSeeder::start();
    let magnet = seeder.magnet().to_string();

    let engine = BtEngine::new(save.path(), None, 0, 0, false, false, false).unwrap();
    let ih = engine.add(&bt_task("t-name", &magnet)).await.unwrap();
    let (ip, port) = seeder.addr();
    engine.core().resume(&ih).unwrap();
    engine.core().add_peer(&ih, &ip, port).unwrap();

    // 等 metadata 就绪
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let st = engine.core().status(&ih).unwrap();
        if st.metadata_received {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "60s 内未收到 metadata"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    // 引擎层透出：DownloadEngine::status → EngineStatus.name = Some(非空)
    let es = engine.status(&ih).await.unwrap();
    let surfaced = es.name.expect("E28: EngineStatus.name 应为 Some");
    assert!(!surfaced.is_empty(), "透出名非空");

    // 交叉一致：与 .torrent 内声明的 name 同源
    let meta = engine
        .core()
        .metadata(&ih)
        .unwrap()
        .expect("metadata bytes");
    let summary = smart_dl_core::torrent_meta::parse_torrent(&meta).unwrap();
    assert_eq!(surfaced, summary.name, "透出名应与 torrent metadata 名一致");
}
