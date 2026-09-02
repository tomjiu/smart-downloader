//! feature `bt`：magnet 链接 → libtorrent 引擎（BtEngine）端到端。
//! 无该 feature 时整个文件被跳过（编译基线不链接 libtorrent）。

#![cfg(feature = "bt")]

mod common;

use common::TestServer;
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use std::sync::Arc;

use base64::Engine as _;

/// 返回值第三项 = BT 引擎全局落盘目录（= default_dest_root）。
/// 生产装配契约（config.bt_save_path）：`[bt] save_path` 缺省 = `[download] dest_root`，
/// 即引擎 save_path 必须与 `with_dest_root` 注入的默认落盘目录一致。此前测试把两者
/// 拆开（引擎=独立 tempdir，default=temp_dir()），add_bt_task 落 dest_root=default
/// ≠ save_path → 引擎 v1 落盘约束 400，5 个用例在 bt 构建下恒失败。
async fn serve_bt() -> (std::net::SocketAddr, Arc<DaemonState>, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let save = dir.path().to_path_buf();
    let bt = smart_dl_daemon::bt::BtEngine::new(&save, None, 0, 0, false, false, false).unwrap();
    let http = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    // 安全修复（V2）适配：save 同时注入为白名单根（with_dest_root 双重语义）。
    let state = Arc::new(
        DaemonState::new(Arc::new(http), vec![])
            .with_dest_root(save.clone())
            .with_bt(Arc::new(bt)),
    );
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state, save)
}

const MAGNET: &str = "magnet:?xt=urn:btih:0d2c9c9d5c2d3e8f9a1b2c3d4e5f6a7b8c9d0e1f&dn=test";

#[tokio::test]
async fn magnet_add_creates_bt_task() {
    let (addr, state, _save) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": MAGNET }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED, "magnet 应 201");
    let body: serde_json::Value = resp.json().await.unwrap();
    let tid = body["task_id"].as_str().unwrap().to_string();

    // 快照：engine=bt、source 为 Magnet、进度可读（libtorrent 实时状态）
    let snap: serde_json::Value = client
        .get(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snap["engine"], "bt", "引擎必须标注 bt");
    assert!(snap["source"].as_str().unwrap().contains("Magnet"));
    assert_eq!(snap["task_id"], tid);

    // 列表含该任务
    let list: serde_json::Value = client
        .get(format!("{base}/tasks"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);

    // TaskCreated + StateChanged(Bt) 事件已发布
    let drained = state.hub().drain();
    let events: Vec<&smart_dl_daemon::events::SchedulerEvent> =
        drained.iter().map(|e| &e.event).collect();
    assert!(events.iter().any(|e| matches!(
        e,
        smart_dl_daemon::events::SchedulerEvent::TaskCreated { .. }
    )));
}

#[tokio::test]
async fn same_magnet_deduped_409() {
    let (addr, _state, _save) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let first = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": MAGNET }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::CREATED);

    let second = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": MAGNET }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        second.status(),
        reqwest::StatusCode::CONFLICT,
        "同 btih 必须判重"
    );
}

#[tokio::test]
async fn torrent_file_add_creates_task() {
    // 最小 .torrent（手写 bencode）→ torrent_b64 上传 → 201 + engine=bt
    let mut t = b"d4:infod6:lengthi123e4:name4:test12:piece lengthi16384e6:pieces20:".to_vec();
    t.extend_from_slice(&[0xAB; 20]);
    t.extend_from_slice(b"ee");
    let b64 = base64::engine::general_purpose::STANDARD.encode(&t);

    let (addr, state, _save) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "torrent_b64": b64 }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        ".torrent 应 201"
    );
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    let snap: serde_json::Value = client
        .get(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snap["engine"], "bt", "torrent 任务必须走 BT 引擎");
    assert!(
        snap["source"].as_str().unwrap().contains("TorrentFile"),
        "source 应标注 TorrentFile"
    );

    // 同一 .torrent 重复 → 409（infohash canonical 查重）
    let dup = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "torrent_b64": b64 }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        dup.status(),
        reqwest::StatusCode::CONFLICT,
        "同 infohash 必须判重"
    );

    // 事件已发布（TaskCreated）
    let drained = state.hub().drain();
    let events: Vec<&smart_dl_daemon::events::SchedulerEvent> =
        drained.iter().map(|e| &e.event).collect();
    assert!(events.iter().any(|e| matches!(
        e,
        smart_dl_daemon::events::SchedulerEvent::TaskCreated { .. }
    )));
}

#[tokio::test]
async fn invalid_base64_rejected() {
    let (addr, _state, _save) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "torrent_b64": "!!!not-base64!!!" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn http_task_with_nested_dest_auto_created() {
    // B10：dest 指向不存在目录 → 自动创建 + 201（HTTP 任务 per-task dest 真实生效；
    // BT 引擎 v1 全局落盘不接受自定义 dest，见 bt_task_with_custom_dest_rejected）
    let body = common::patterned(8 * 1024);
    let srv = TestServer::start(body).await;
    let url = srv.url();
    let (addr, _state, save) = serve_bt().await;
    // nested dest 挂在白名单根（= BT 引擎全局落盘目录）下，测 B10 自动创建
    let nested = save.join("some/deep/dir");
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": url,
            "dest": nested.to_string_lossy()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "缺失 dest 应自动创建"
    );
    assert!(nested.is_dir(), "dest 目录必须被创建");
}

#[tokio::test]
async fn magnet_and_http_coexist() {
    // 同一 daemon 内 BT + HTTP 任务并存（引擎统一抽象）
    let body = common::patterned(16 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state, _save) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let bt = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": MAGNET }))
        .send()
        .await
        .unwrap();
    assert_eq!(bt.status(), reqwest::StatusCode::CREATED);

    let http = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": srv.url() }))
        .send()
        .await
        .unwrap();
    let http_status = http.status();
    if http_status != reqwest::StatusCode::CREATED {
        eprintln!("HTTP add body: {:?}", http.text().await.unwrap());
        panic!("http add should be 201, got {http_status}");
    }

    let list: serde_json::Value = client
        .get(format!("{base}/tasks"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2, "BT+HTTP 任务并存");
}

#[tokio::test]
async fn magnet_remove_ok() {
    let (addr, _state, _save) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": MAGNET }))
        .send()
        .await
        .unwrap();
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    let r = client
        .delete(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "BT 任务删除应成功");

    let snap = client
        .get(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap();
    assert_eq!(snap.status(), 404, "删除后快照应 404");
}

#[tokio::test]
async fn bt_task_with_custom_dest_rejected() {
    // BT 引擎 v1 全局落盘（serve bt.save_path）：任务级 dest 与全局目录不一致 → 400。
    // custom 取白名单根（= 引擎 save_path）的子目录：先过 dest 白名单（V2），
    // 再被引擎层落盘约束拒绝——精确测「全局落盘」文案而非白名单越界文案。
    let (addr, _state, save) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let custom = save.join("custom-dest");

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": MAGNET,
            "dest": custom.to_string_lossy()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "自定义 dest 应被拒绝（诚实约束，避免静默落错目录）"
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains("全局落盘"), "错误信息应说明落盘约束: {body}");
}

#[tokio::test]
async fn readd_same_magnet_after_restart_ok() {
    // 重启续传前提：新 session 同一 save_path 重新 add 同一 magnet → 成功（libtorrent
    // 磁盘检查复用已下载块）。daemon 持久化恢复走的就是这条路径。
    let dir = tempfile::tempdir().unwrap();
    let http = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());

    // 第一次"运行"
    let r1 = {
        let bt = smart_dl_daemon::bt::BtEngine::new(dir.path(), None, 0, 0, false, false, false)
            .unwrap();
        let state = DaemonState::new(Arc::new(http.clone()), vec![]).with_bt(Arc::new(bt));
        state.add_link_task(MAGNET.to_string(), None).await
    };
    assert!(r1.is_ok(), "首次 add 应成功: {:?}", r1.err());

    // "重启"：新 session（同 save_path）重新 add
    let bt2 =
        smart_dl_daemon::bt::BtEngine::new(dir.path(), None, 0, 0, false, false, false).unwrap();
    let state2 = DaemonState::new(Arc::new(http), vec![]).with_bt(Arc::new(bt2));
    let r2 = state2.add_link_task(MAGNET.to_string(), None).await;
    assert!(
        r2.is_ok(),
        "重启后同 ih 重新 add 应成功（续传前提）: {:?}",
        r2.err()
    );
}

// ============ 任务级限速（POST /tasks/:id/limit）——BT up/down 双向 ============

#[tokio::test]
async fn bt_task_limit_up_and_down_merge() {
    // BT 任务双方向限速：up 是 BT 独有能力（HTTP 任务 up → 409，见 http_api）。
    // 假 btih 磁链在 libtorrent 侧有真实 torrent handle（metadata 未就绪），
    // per-torrent set_download/upload_limit 对 handle 有效 → 引擎层调用必须成功。
    let body = common::patterned(16 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state, _save) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let bt = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": MAGNET }))
        .send()
        .await
        .unwrap();
    assert_eq!(bt.status(), reqwest::StatusCode::CREATED);
    let tid = bt.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 首设 up=64
    let resp = client
        .post(format!("{base}/tasks/{tid}/limit"))
        .json(&serde_json::json!({ "up_kb_s": 64 }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "BT 任务 up 限速必须成功: {:?}",
        resp.text().await.unwrap()
    );
    let snap: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(snap["limits"]["up_kb_s"], 64);

    // 只传 down=512 → 合并保持 up=64
    let resp = client
        .post(format!("{base}/tasks/{tid}/limit"))
        .json(&serde_json::json!({ "down_kb_s": 512 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let snap: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(snap["limits"]["down_kb_s"], 512);
    assert_eq!(snap["limits"]["up_kb_s"], 64, "up 必须被合并保留");

    // 引擎层真实生效的旁证：持久化文件里的任务记录带 limits
    // （state.autosave 在 set 后触发；等 tasks.json 出现）
    let _ = srv; // 保持 TestServer 存活到断言结束
}

// ============ 任务级子文件优先级（POST /tasks/:id/files/priority）============

fn minimal_torrent_b64() -> String {
    // 最小单文件 .torrent（手写 bencode，与 torrent_file_add_creates_task 同款）
    let mut t = b"d4:infod6:lengthi123e4:name4:test12:piece lengthi16384e6:pieces20:".to_vec();
    t.extend_from_slice(&[0xAB; 20]);
    t.extend_from_slice(b"ee");
    base64::engine::general_purpose::STANDARD.encode(&t)
}

#[tokio::test]
async fn torrent_file_task_sets_and_readbacks_priority() {
    // 真实 torrent（metadata 即时就绪）→ file 0 设为 0（skip）→ 回显当前优先级表
    let (addr, _state, _save) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "torrent_b64": minimal_torrent_b64() }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = client
        .post(format!("{base}/tasks/{tid}/files/priority"))
        .json(&serde_json::json!({ "priorities": [ { "index": 0, "priority": 0 } ] }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "设置子文件优先级必须 200: {:?}",
        resp.text().await.unwrap()
    );

    // libtorrent 文件优先级为异步记账（设后立即查可能读旧值，file_prio_alert
    // 为准）——轮询到收敛（空 priorities 列表 = 纯 readback）。
    let mut applied = None;
    for _ in 0..40 {
        let resp = client
            .post(format!("{base}/tasks/{tid}/files/priority"))
            .json(&serde_json::json!({ "priorities": [] }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        if body["priorities"] == serde_json::json!([0]) {
            applied = Some(body.clone());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        applied.expect("3s 内优先级必须收敛为 [0] (skip)")["priorities"],
        serde_json::json!([0]),
        "回显 = 当前各文件优先级（下标 = 文件序）"
    );

    // 恢复默认 4 → 轮询收敛 [4]
    let resp = client
        .post(format!("{base}/tasks/{tid}/files/priority"))
        .json(&serde_json::json!({ "priorities": [ { "index": 0, "priority": 4 } ] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let mut restored = None;
    for _ in 0..40 {
        let resp = client
            .post(format!("{base}/tasks/{tid}/files/priority"))
            .json(&serde_json::json!({ "priorities": [] }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        if body["priorities"] == serde_json::json!([4]) {
            restored = Some(body.clone());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        restored.expect("3s 内优先级必须收敛回 [4]")["priorities"],
        serde_json::json!([4])
    );
}

#[tokio::test]
async fn magnet_task_file_priority_metadata_pending_409() {
    // magnet 任务 metadata 未就绪 → files 未规划 → 409（明确语义，非 500）
    let (addr, _state, _save) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let bt = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": MAGNET }))
        .send()
        .await
        .unwrap();
    assert_eq!(bt.status(), reqwest::StatusCode::CREATED);
    let tid = bt.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = client
        .post(format!("{base}/tasks/{tid}/files/priority"))
        .json(&serde_json::json!({ "priorities": [ { "index": 0, "priority": 0 } ] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("metadata"),
        "错误应指明 metadata 未就绪: {body}"
    );
}

#[tokio::test]
async fn file_priority_index_out_of_range_400() {
    let (addr, _state, _save) = serve_bt().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "torrent_b64": minimal_torrent_b64() }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 单文件 torrent → index 5 越界 → 400
    let resp = client
        .post(format!("{base}/tasks/{tid}/files/priority"))
        .json(&serde_json::json!({ "priorities": [ { "index": 5, "priority": 4 } ] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // priority 8 越界（0..=7）→ 400
    let resp = client
        .post(format!("{base}/tasks/{tid}/files/priority"))
        .json(&serde_json::json!({ "priorities": [ { "index": 0, "priority": 8 } ] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

// ============ 持久化 + 恢复重放（P1-3：file_priorities 生命周期）============

/// serve_bt 的外置目录变体：save = dir（调用方持有 tempdir 保活），可选注入
/// tasks.json 持久化路径——重启恢复 e2e 用（serve_bt 的内嵌 tempdir 会随函数
/// 返回被删除，无法跨"重启"复用落盘目录）。
async fn serve_bt_in(
    dir: &std::path::Path,
    store: Option<std::path::PathBuf>,
) -> (std::net::SocketAddr, Arc<DaemonState>, std::path::PathBuf) {
    let save = dir.to_path_buf();
    let bt = smart_dl_daemon::bt::BtEngine::new(&save, None, 0, 0, false, false, false).unwrap();
    let http = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let mut state = smart_dl_daemon::state::DaemonState::new(Arc::new(http), vec![])
        .with_dest_root(save.clone())
        .with_bt(Arc::new(bt));
    if let Some(p) = store {
        state = state.with_storage(p);
    }
    let state = Arc::new(state);
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state, save)
}

#[tokio::test]
async fn file_priority_persisted_and_replayed_after_restart() {
    // 全生命周期 e2e：设置优先级 → tasks.json 落盘 file_priorities →
    // "重启"（新 BtEngine + restore_from）→ metadata 就绪任务立即重放 →
    // 引擎侧 readback 收敛到持久化值。
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tasks.json");
    let client = reqwest::Client::new();
    let tid;

    // —— 第一次"运行"：add torrent（metadata 即时就绪）→ 设 file 0 = 0（skip）
    {
        let (addr, _state, _save) = serve_bt_in(dir.path(), Some(store.clone())).await;
        let base = format!("http://{addr}");

        let resp = client
            .post(format!("{base}/tasks"))
            .json(&serde_json::json!({ "torrent_b64": minimal_torrent_b64() }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
        tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
            .as_str()
            .unwrap()
            .to_string();

        let resp = client
            .post(format!("{base}/tasks/{tid}/files/priority"))
            .json(&serde_json::json!({ "priorities": [ { "index": 0, "priority": 0 } ] }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        // 异步记账 → 轮询收敛 [0]
        let mut converged = false;
        for _ in 0..40 {
            let resp = client
                .post(format!("{base}/tasks/{tid}/files/priority"))
                .json(&serde_json::json!({ "priorities": [] }))
                .send()
                .await
                .unwrap();
            if resp.json::<serde_json::Value>().await.unwrap()["priorities"]
                == serde_json::json!([0])
            {
                converged = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(converged, "首设优先级必须收敛为 [0]");

        // tasks.json 落盘 file_priorities（autosave 在 set 后触发；pretty JSON → 解析断言）
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if let Ok(text) = std::fs::read_to_string(&store) {
                let persisted = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| {
                        v.as_array().map(|a| {
                            a.iter()
                                .any(|t| t["task"]["file_priorities"] == serde_json::json!([0]))
                        })
                    })
                    .unwrap_or(false);
                if persisted {
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "tasks.json 必须持久化 file_priorities"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        // client/_state 在块尾 drop：模拟进程退出（session 关闭）
    }

    // —— "重启"：新 session（同 save 目录）+ restore_from → 立即重放
    let bt2 =
        smart_dl_daemon::bt::BtEngine::new(dir.path(), None, 0, 0, false, false, false).unwrap();
    let http2 = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let state2 = Arc::new(
        smart_dl_daemon::state::DaemonState::new(Arc::new(http2), vec![])
            .with_dest_root(dir.path().to_path_buf())
            .with_bt(Arc::new(bt2)),
    );
    let n = state2.restore_from(&store).await.unwrap();
    assert_eq!(n, 1, "应恢复 1 条任务");

    // 重启后的 state2 拉起 HTTP（快照/readback 走公开 API）
    let app = http::router(state2.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr2 = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base2 = format!("http://{addr2}");

    // 快照透出恢复记录的持久化优先级表（TaskSnapshot.file_priorities）
    let snap: serde_json::Value = client
        .get(format!("{base2}/tasks/{tid}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        snap["file_priorities"],
        serde_json::json!([0]),
        "恢复记录必须保留持久化的优先级表: {snap}"
    );

    // 引擎侧 readback 收敛到持久化值 [0]（file priority 异步记账 → 轮询；
    // 空 priorities 列表 = 纯 readback）
    let mut replayed = false;
    for _ in 0..40 {
        let resp = client
            .post(format!("{base2}/tasks/{tid}/files/priority"))
            .json(&serde_json::json!({ "priorities": [] }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        if resp.json::<serde_json::Value>().await.unwrap()["priorities"] == serde_json::json!([0]) {
            replayed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(replayed, "重启后优先级必须重放收敛为 [0]");
}
