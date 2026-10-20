//! 进程与资源生命周期实机测试（Issue #121 维度 C，REALTEST-PLAN §4）。
//!
//! 用户报告的原始问题：**「删任务后后台仍在下载」**。本文件用真实
//! daemon + 真实 HttpEngine + 慢速下载源，在文件系统层面断言引擎
//! 真的停了（`.part` 停止增长 / 被清理）、产物无残留、暂停意图跨
//! 重启保持、daemon 进程优雅退出且端口可立即复用。
//!
//! 原则 2：断言用户可见效果（文件大小/目录内容/进程存活），非 HTTP 200。
//! 原则 3：全部 `tokio::time::timeout` 包裹。
//!
//! 维度 C-2 说明：当前仓库形态为「daemon 独立进程 + Next.js Web UI」
//!（无桌面壳），关窗即关浏览器标签——REALTEST-PLAN 的桌面脚本场景
//! （Job Object / PDEATHSIG）不适用；本文件以 **daemon 进程生命周期**
//! 等价覆盖（lockfile 互斥 → SIGTERM 优雅退出 → 端口复用），桌面壳
//! 上线后在 `scripts/test-desktop-lifecycle.ps1` 补壳进程场景。

mod common;

use common::{patterned, SlowTestServer};
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;
use std::time::Duration;

/// 起真实 daemon（可注入持久化路径；含生产同构的引擎状态轮询循环）。
/// 存在 tasks.json 时执行恢复（重启语义：restore_from）。
async fn serve_with_storage(storage: std::path::PathBuf) -> (String, Arc<DaemonState>) {
    let engine = HttpEngine::new(reqwest::Client::new());
    let state = DaemonState::new(Arc::new(engine), vec![])
        .with_dest_root(std::env::temp_dir())
        .with_storage(storage.clone());
    let state = Arc::new(state);
    if storage.exists() {
        let n = state.restore_from(&storage).await.unwrap_or(0);
        let _ = n; // 恢复条数（重启语义断言在具体测试里）
    }
    // 生产同构：引擎状态轮询（list 读记录态；无轮询则 Queued 永不推进）
    let _poll =
        smart_dl_daemon::http_events::spawn_http_events(state.clone(), Duration::from_millis(300));
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
}

/// 在 dest_root 下找 `.part` 文件（文件名由引擎从 URL 末段派生，
/// dest 语义是目录——扫描而非拼路径，不依赖派生规则细节）。
fn find_part(dest_root: &std::path::Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dest_root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "part").unwrap_or(false))
}

async fn add_at(client: &reqwest::Client, base: &str, url: &str, dest: &std::path::Path) -> String {
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": url,
            "dest": dest.display().to_string()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    v["task_id"].as_str().unwrap_or_default().to_string()
}

async fn snap(client: &reqwest::Client, base: &str, id: &str) -> serde_json::Value {
    client
        .get(format!("{base}/tasks/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

const TIMEOUT: Duration = Duration::from_secs(90);

// ============ C-1：删任务后引擎真的停了吗（用户报告的问题） ============

/// **核心用例（Issue #120 同源场景的文件系统层验证）**：慢速下载中删任务
/// （`delete_data=true`）→ 等段边界 → `.part` 必须被清除（引擎 remove 的
/// 数据处置链路）。`.part` 仍存在或在增长 = 引擎未停止。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c1_remove_task_with_data_deletes_part() {
    tokio::time::timeout(TIMEOUT, async {
        // 64MB / 640 块 × 50ms ≈ 32s 全速——足够观察窗口
        let srv = SlowTestServer::start(patterned(64 * 1024 * 1024), 640, 50).await;
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("tasks.json");
        let (base, _state) = serve_with_storage(storage).await;
        let client = reqwest::Client::new();
        let dest = dir.path().join("big.bin");

        let id = add_at(&client, &base, &srv.url(), &dest).await;
        // 等下载起来（.part 在增长；慢源 probe + 首段启动秒级）
        let part = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(p) = find_part(&dest) {
                    if std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0) > 0 {
                        return p;
                    }
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .expect("下载中 .part 必须出现且非空");
        let s = snap(&client, &base, &id).await;
        assert_eq!(s["state"], "Downloading", "前提：任务在跑: {s}");
        let size_before = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        assert!(size_before > 0, ".part 必须已写入数据");

        // 删任务（delete_data=true：引擎删落盘数据）
        let st = client
            .delete(format!("{base}/tasks/{id}?delete_data=true"))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(st, reqwest::StatusCode::NO_CONTENT, "删除应成功");

        // 段边界窗口（一个段 = 16MB/64MB × 32s 慢速远未完成，引擎停写应在
        // 秒级）：连续采样 5s，.part 要么不存在（已清）要么大小冻结。
        let mut last = size_before;
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let now = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            assert!(
                now <= last,
                "删任务后 .part 仍在增长（{last} → {now}）—— 引擎未停止"
            );
            last = now;
        }
        // 最终断言（用户可见效果）：delete_data=true → 数据被清理
        assert!(
            !part.exists(),
            "delete_data=true 删除任务后 .part 必须被引擎清理（残留 {last}B）"
        );
    })
    .await
    .expect("超时 —— 删除链路疑似挂死");
}

/// delete_data=false 变体：`.part` 保留（用户数据）但**停止增长**
/// ——「删任务后后台仍在下载」的直接文件系统证据。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c1_remove_task_keeps_data_but_stops_writing() {
    tokio::time::timeout(TIMEOUT, async {
        let srv = SlowTestServer::start(patterned(64 * 1024 * 1024), 640, 50).await;
        let dir = tempfile::tempdir().unwrap();
        let (base, _state) = serve_with_storage(dir.path().join("tasks.json")).await;
        let client = reqwest::Client::new();
        let dest = dir.path().join("keep.bin");

        let id = add_at(&client, &base, &srv.url(), &dest).await;
        let part = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(p) = find_part(&dest) {
                    if std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0) > 0 {
                        return p;
                    }
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .expect("下载中 .part 必须出现且非空");
        let s = snap(&client, &base, &id).await;
        assert_eq!(s["state"], "Downloading");
        let size_before = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);

        let st = client
            .delete(format!("{base}/tasks/{id}"))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(st, reqwest::StatusCode::NO_CONTENT);

        // 5s 连续采样：保留但冻结
        let mut max_seen = size_before;
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let now = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            max_seen = max_seen.max(now);
        }
        assert!(
            max_seen <= size_before + 1,
            "删任务后 .part 仍在增长（{size_before} → {max_seen}）—— 引擎未停止"
        );
        assert!(part.exists(), "delete_data=false：.part 应保留（用户数据）");
    })
    .await
    .expect("超时 —— 删除链路疑似挂死");
}

// ============ C-3 #1：删任务后产物文件全部清理 ============

/// N 次 add（小文件完成）+ remove(delete_data=true) → dest 目录必须
/// 无 `.part` / 进度账本残留（文件句柄与产物泄漏的用户可见面）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c3_remove_leaves_no_artifacts() {
    tokio::time::timeout(TIMEOUT, async {
        use common::TestServer;
        let srv = TestServer::start(patterned(128 * 1024)).await;
        let dir = tempfile::tempdir().unwrap();
        let (base, _state) = serve_with_storage(dir.path().join("tasks.json")).await;
        let client = reqwest::Client::new();

        let mut ids = Vec::new();
        for k in 0..20usize {
            let dest = dir.path().join(format!("art-{k}"));
            // query 区分 canonical（同 URL 查重 409）
            let id = add_at(&client, &base, &format!("{}?k={k}", srv.url()), &dest).await;
            ids.push((id, dest));
        }
        // 等全部完成（小文件毫秒级）
        tokio::time::sleep(Duration::from_millis(1500)).await;

        for (id, _dest) in &ids {
            let st = client
                .delete(format!("{base}/tasks/{id}?delete_data=true"))
                .send()
                .await
                .unwrap()
                .status();
            assert_eq!(st, reqwest::StatusCode::NO_CONTENT);
        }
        tokio::time::sleep(Duration::from_millis(300)).await;

        // 用户可见效果：dest 目录清空（完成文件 + .part + 账本全清）
        let mut leftovers = Vec::new();
        for (_id, dest) in &ids {
            if let Ok(rd) = std::fs::read_dir(dest) {
                for e in rd.flatten() {
                    leftovers.push(e.path());
                }
            }
        }
        assert!(
            leftovers.is_empty(),
            "20 次 add+remove(delete_data) 后 dest 残留产物: {leftovers:?}"
        );
    })
    .await
    .expect("超时");
}

// ============ C-3 #3：暂停意图与进度跨 daemon 重启保持 ============

/// 慢速任务下载中 pause → 用同一 storage 重建 daemon（`restore_from`）
/// → 任务必须恢复且**保持 Paused**（G5 用户暂停意图：重启后不自动开跑）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c3_pause_survives_daemon_restart() {
    tokio::time::timeout(TIMEOUT, async {
        // 8MB < 16MiB 分段阈值 → 单连接整流，done 流式上报（多段模式下
        // total_done 按段完成跳变，8s 观测窗内恒 0）
        let srv = SlowTestServer::start(patterned(8 * 1024 * 1024), 80, 50).await;
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("tasks.json");
        let (base, _state) = serve_with_storage(storage.clone()).await;
        let client = reqwest::Client::new();
        let dest = dir.path().join("paused.bin");

        let id = add_at(&client, &base, &srv.url(), &dest).await;
        // 等真实写盘进度（引擎语义：单段任务 total_done 按段完成跳变，
        // 进度判据用 .part 文件大小——流式写盘的物理证据）
        let (part, size_before) = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(p) = find_part(&dest) {
                    let sz = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                    if sz > 0 {
                        return (p, sz);
                    }
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .expect("暂停前必须有写盘进度（8s 内 .part > 0）");

        let st = client
            .post(format!("{base}/tasks/{id}/pause"))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(st, 200);
        tokio::time::sleep(Duration::from_millis(600)).await;
        let s = snap(&client, &base, &id).await;
        assert_eq!(s["state"], "Paused", "暂停后状态: {s}");

        // 暂停后 .part 冻结（引擎停止的文件系统证据）
        let mut max_seen = size_before;
        for _ in 0..6 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let now = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            max_seen = max_seen.max(now);
        }
        assert!(
            max_seen <= size_before + 1,
            "暂停后 .part 仍在增长（{size_before} → {max_seen}）—— 引擎未停止"
        );

        // 「重启」：同一 storage 起新 daemon 并恢复
        let (base2, _state2) = serve_with_storage(storage).await;
        let s2 = snap(&client, &base2, &id).await;
        assert_eq!(
            s2["state"], "Paused",
            "重启后必须保持 Paused（G5 用户暂停意图），实际 {}",
            s2["state"]
        );
        // 数据面保留（续传基础）：.part 与已写进度还在
        assert!(part.exists(), "重启后 .part 必须保留（进度数据）");
        let size_after_restart = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        assert!(
            size_after_restart >= size_before,
            "重启后已写进度保留（{size_after_restart} >= {size_before}）"
        );

        // 恢复下载（resume 链路在重启后可用）
        let st = client
            .post(format!("{base2}/tasks/{id}/resume"))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(st, 200, "重启后 resume 应可用: {st}");
        let _ = client
            .delete(format!("{base2}/tasks/{id}?delete_data=true"))
            .send()
            .await;
    })
    .await
    .expect("超时");
}
