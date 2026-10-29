//! 并发与死锁实机测试（Issue #121 维度 B，REALTEST-PLAN §3）。
//!
//! 与内部 `state_tests`（FakeEngine、单线程场景）的根本区别：本文件打
//! **真实 daemon**（真实 axum + 真实 HttpEngine + 真实 HTTP 下载源），
//! 并发请求在 HTTP 层交错——覆盖 `gate_or_enqueue` → `engine.add` →
//! `attach_reserved_handle` 全链锁路径与轮询回填路径的真实竞争窗口。
//!
//! 原则 3（REALTEST-PLAN §7）：**所有测试用 `tokio::time::timeout` 包裹**
//! ——死锁时测试以「超时失败」暴露，而不是挂死 CI。
//!
//! 原则 2：断言用户可见效果（配额恰好、列表一致、文件不丢），非 HTTP 200。
//!
//! 覆盖矩阵（REALTEST-PLAN B-1/B-2）：
//! - B-1 ABBA 回归（repro/round23 实机版）：并发 add+attach 与轮询交错
//! - B-2 #1 并发 add 超卖：配额 3 × 并发 50 → 恰好 3 进引擎
//! - B-2 #2 并发 remove + add：无 panic、无残留引用
//! - B-2 #3 并发 pause + remove：终态一致，引擎无孤儿
//! - B-2 #4 账本并发写：tasks.json 持续可解析且与内存一致
//! - B-2 #5 RSS 并发修改：rss.json 不丢失更新
//! - B-2 #7 限速器并发：多任务共享全局限速，总速率不超标
//! - B-2 #6（段下载并发/数据完整）已由 httpdl `multi_conn_integrity.rs`
//!   常驻覆盖（four_segments_64mb_sha256_matches_source +
//!   segment_requests_cover_file_without_overlap），本文件不重复。

mod common;

use common::{patterned, SlowTestServer, TestServer};
use smart_dl_daemon::config::QueueCfg;
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

/// 配额简写（0 = 不限位保持默认）。
fn config_quota(max_active_http: u32) -> QueueCfg {
    QueueCfg {
        max_active_bt: 0,
        max_active_http,
        max_active_ftp: 0,
        max_active_total: 0,
    }
}

/// 真实 daemon（HTTP 引擎 + 可选配额/全局限速/持久化注入）。
struct LiveDaemon {
    base: String,
    #[allow(dead_code)] // 持有所有权（serve 任务引用）
    state: Arc<DaemonState>,
}

async fn serve_with(f: impl FnOnce(DaemonState) -> DaemonState) -> LiveDaemon {
    let engine = HttpEngine::new(reqwest::Client::new());
    let state = DaemonState::new(Arc::new(engine), vec![]).with_dest_root(std::env::temp_dir());
    let state = Arc::new(f(state));
    // 生产同构：引擎状态轮询循环（HTTP 任务 attach 后留 Queued，首轮轮询
    // 迁移 Downloading——list 读记录态，无轮询则列表态永不推进）。
    // 300ms 快轮询：状态回填在断言窗口内完成。
    let _poll =
        smart_dl_daemon::http_events::spawn_http_events(state.clone(), Duration::from_millis(300));
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    LiveDaemon {
        base: format!("http://{addr}"),
        state,
    }
}

/// RSS feed host：`/feed/{n}.xml` 各返回不同 title 的最小 RSS（并发添加
/// 需要互不相同的 URL 去重语义，且 add 是「立即拉取一次，失败即 Err」）。
async fn serve_feed_host() -> String {
    async fn handler(axum::extract::Path(n): axum::extract::Path<u16>) -> impl IntoResponse {
        let xml = format!(
            "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel>\
             <title>feed-{n}</title><link>http://example.com/{n}</link>\
             <description>d</description>\
             <item><title>item-{n}</title>\
             <link>http://example.com/{n}/file.bin</link></item>\
             </channel></rss>"
        );
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/rss+xml")],
            xml,
        )
    }
    let app = Router::new().route("/feed/:n", get(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// add（独立临时 dest，`?k=` query 区分 canonical 避免查重 409）。
async fn add(
    client: &reqwest::Client,
    base: &str,
    url: &str,
    k: usize,
) -> (reqwest::StatusCode, String) {
    let dest = std::env::temp_dir().join(format!("conc-{}-{k}", std::process::id()));
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": format!("{url}?k={k}"),
            "dest": dest.display().to_string()
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let val: serde_json::Value = resp.json().await.unwrap_or_default();
    (
        status,
        val["task_id"].as_str().unwrap_or_default().to_string(),
    )
}

async fn list(client: &reqwest::Client, base: &str) -> Vec<serde_json::Value> {
    client
        .get(format!("{base}/tasks"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn delete(client: &reqwest::Client, base: &str, id: &str) -> reqwest::StatusCode {
    client
        .delete(format!("{base}/tasks/{id}?delete_data=true"))
        .send()
        .await
        .unwrap()
        .status()
}

/// 全部测试的超时预算（原则 3）：死锁 → 超时失败而非挂死。
const TIMEOUT: Duration = Duration::from_secs(60);

// ============ B-1：ABBA 死锁回归（repro/round23 实机版） ============

/// PR #114 P0-2 修复的常驻回归：`gate_or_enqueue`（add 路径，锁序
/// tasks → slot_reservations）与 `attach_reserved_handle`（曾为反序，
/// 修复后统一）在 HTTP 层高并发交错。8 worker × 12 add 连发 + 并行
/// 轮询（`poll_engine_states` 路径同持 tasks 锁）+ 并行 pause 递补
/// （`activate_one` 同两把锁）。修复前（slot → tasks 反序）该组合在
/// repro round23 的 8×2000 次压测中稳定死锁；本测试以 30s 超时断言
/// 全部完成——死锁时超时变红（判别验证见 PR 描述：临时对调
/// attach_reserved_handle 锁序复跑，测试必红）。
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn b1_abba_regression_concurrent_add_attach_poll_no_deadlock() {
    tokio::time::timeout(TIMEOUT, async {
        // 慢源：8MB / 80 块 × 50ms ≈ 4s——add 窗口内任务持续在途
        let srv = SlowTestServer::start(patterned(8 * 1024 * 1024), 80, 50).await;
        let d = serve_with(|s| s.with_queue_cfg(config_quota(2))).await;
        let client = reqwest::Client::new();

        let mut handles = Vec::new();
        // 8 个 add worker：每人连发 12 个 add（gate + engine.add + attach 全链）
        for t in 0..8u16 {
            let c = client.clone();
            let base = d.base.clone();
            let url = srv.url();
            handles.push(tokio::spawn(async move {
                for k in 0..12usize {
                    let (st, _id) = add(&c, &base, &url, (t as usize) * 100 + k).await;
                    assert_eq!(st, reqwest::StatusCode::CREATED, "add 不得因锁竞争失败");
                }
            }));
        }
        // 2 个轮询 worker：GET /tasks（触发状态轮询回填路径的 tasks 锁竞争）
        for _ in 0..2 {
            let c = client.clone();
            let base = d.base.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..40 {
                    let l = list(&c, &base).await;
                    // 超卖守恒不变量：任意时刻在途（Downloading）≤ 配额 2
                    let inflight = l.iter().filter(|t| t["state"] == "Downloading").count();
                    assert!(
                        inflight <= 2,
                        "轮询期间超卖：{inflight} 个 Downloading > 配额 2"
                    );
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }));
        }

        let results = futures::future::join_all(handles).await;
        for r in results {
            r.expect("worker 不得 panic（死锁表现为超时而非 panic）");
        }
        // daemon 仍活着（无 panic 拖垮 serve 任务）
        let health = client
            .get(format!("{}/health", d.base))
            .send()
            .await
            .unwrap();
        assert_eq!(health.status(), 200);
    })
    .await
    .expect("超时 —— 疑似死锁（tasks/slot_reservations 锁序回归？）");
}

// ============ B-2 #1：并发 add 超卖 ============

/// 配额 3、并发 50 个 add → **恰好 3 个**进引擎（Downloading），其余 47
/// 落 Queued（用户可见效果：列表分组计数）。超卖 = 状态机/账本损坏。
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn b2_1_concurrent_add_no_oversell() {
    tokio::time::timeout(TIMEOUT, async {
        // 慢源：16MB / 160 块 × 50ms ≈ 8s——断言窗口内 3 个在途任务不会完成
        let srv = SlowTestServer::start(patterned(16 * 1024 * 1024), 160, 50).await;
        let d = serve_with(|s| s.with_queue_cfg(config_quota(3))).await;
        let client = reqwest::Client::new();

        let mut handles = Vec::new();
        for k in 0..50usize {
            let c = client.clone();
            let base = d.base.clone();
            let url = srv.url();
            handles.push(tokio::spawn(async move {
                let (st, id) = add(&c, &base, &url, k).await;
                assert_eq!(st, reqwest::StatusCode::CREATED);
                id
            }));
        }
        let ids: Vec<String> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(ids.len(), 50, "全部 add 必须 201");

        // 状态回填窗口：add 时记录态为 Queued，Downloading 由引擎状态轮询
        // 回填（首轮间隔秒级）。等回填后再断言配额（否则断言空转全 Queued）。
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let l = list(&client, &d.base).await;
        let inflight = l.iter().filter(|t| t["state"] == "Downloading").count();
        let queued = l.iter().filter(|t| t["state"] == "Queued").count();
        assert_eq!(
            inflight, 3,
            "配额 3 → 恰好 3 个进引擎（实际 {inflight}：超卖或漏配）"
        );
        assert_eq!(queued, 47, "其余 47 个排队（实际 {queued}）");

        // 清理（delete_data：临时 dest 全清）
        for id in &ids {
            let st = delete(&client, &d.base, id).await;
            assert_eq!(st, reqwest::StatusCode::NO_CONTENT);
        }
    })
    .await
    .expect("超时 —— 疑似死锁");
}

// ============ B-2 #2：并发 remove + add ============

/// 先建 10 个任务，然后并发交错 remove × add。断言（用户可见效果）：
/// ① daemon 全程存活；② 列表与逐任务 GET 一致（无 zombie 引用/无幽灵
/// 404）；③ 全部结束后再全删，列表清空（无残留记录）。
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn b2_2_concurrent_remove_and_add_no_residue() {
    tokio::time::timeout(TIMEOUT, async {
        let srv = SlowTestServer::start(patterned(8 * 1024 * 1024), 80, 50).await;
        let d = serve_with(|s| s.with_queue_cfg(config_quota(2))).await;
        let client = reqwest::Client::new();

        // 初始 10 个任务（3 进引擎 + 7 排队）
        let mut ids = Vec::new();
        for k in 0..10usize {
            let (st, id) = add(&client, &d.base, &srv.url(), k).await;
            assert_eq!(st, reqwest::StatusCode::CREATED);
            ids.push(id);
        }

        let mut handles = Vec::new();
        // 5 个 remover：删除初始任务（remove_with → engine.remove → 删记录）
        for id in ids.clone() {
            let c = client.clone();
            let base = d.base.clone();
            handles.push(tokio::spawn(async move {
                let st = delete(&c, &base, &id).await;
                assert!(
                    st == reqwest::StatusCode::NO_CONTENT,
                    "remove 竞态窗口内允许 404（已删）但不允许 5xx: {st}"
                );
            }));
        }
        // 5 个 adder：同时加新任务
        let mut new_ids = Vec::new();
        for k in 100..105usize {
            let (st, id) = add(&client, &d.base, &srv.url(), k).await;
            assert_eq!(st, reqwest::StatusCode::CREATED);
            new_ids.push(id);
        }

        let results = futures::future::join_all(handles).await;
        for r in results {
            r.expect("remover 不得 panic");
        }

        // ② 一致性：列表里每个任务 GET /tasks/:id 都 200
        let l = list(&client, &d.base).await;
        let listed: std::collections::HashSet<&str> =
            l.iter().filter_map(|t| t["task_id"].as_str()).collect();
        for tid in &listed {
            let st = client
                .get(format!("{}/tasks/{tid}", d.base))
                .send()
                .await
                .unwrap()
                .status();
            assert_eq!(st, 200, "列表引用的任务必须可取（zombie 引用 = 账本损坏）");
        }

        // ③ 全删 → 清空
        for id in new_ids {
            let st = delete(&client, &d.base, &id).await;
            assert_eq!(st, reqwest::StatusCode::NO_CONTENT);
        }
        for id in ids {
            let _ = delete(&client, &d.base, &id).await; // 可能已删（404 合法）
        }
        let l = list(&client, &d.base).await;
        assert!(
            l.is_empty(),
            "全删后列表必须清空（残留记录 = remove 竞态丢失）: {l:?}"
        );
    })
    .await
    .expect("超时 —— 疑似死锁");
}

// ============ B-2 #3：并发 pause + remove ============

/// 对同一批在途任务并发 pause / remove / resume。断言终态一致：
/// 任务要么彻底消失（GET 404 且不在列表），要么存在且 GET 200——
/// 不允许「在列表但 404」（记录残留）或「404 但仍在下载」（引擎孤儿，
/// C-1 的 .part 断言在 lifecycle_leak.rs 单独覆盖）。
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn b2_3_concurrent_pause_remove_consistent_final_state() {
    tokio::time::timeout(TIMEOUT, async {
        let srv = SlowTestServer::start(patterned(8 * 1024 * 1024), 80, 50).await;
        let d = serve_with(|s| s.with_queue_cfg(config_quota(3))).await;
        let client = reqwest::Client::new();

        let mut ids = Vec::new();
        for k in 0..6usize {
            let (st, id) = add(&client, &d.base, &srv.url(), k).await;
            assert_eq!(st, reqwest::StatusCode::CREATED);
            ids.push(id);
        }

        // 对同一任务并发 pause / remove / resume（三种终态迁移交错）
        let mut handles = Vec::new();
        for id in ids.clone() {
            for op in ["pause", "remove"] {
                let c = client.clone();
                let base = d.base.clone();
                let id = id.clone();
                handles.push(tokio::spawn(async move {
                    match op {
                        "pause" => {
                            let st = c
                                .post(format!("{base}/tasks/{id}/pause"))
                                .send()
                                .await
                                .unwrap()
                                .status();
                            assert!(st == 200 || st == 404, "pause: {st}");
                        }
                        _ => {
                            let st = delete(&c, &base, &id).await;
                            assert!(st == 204 || st == 404, "remove: {st}");
                        }
                    }
                }));
            }
        }
        let results = futures::future::join_all(handles).await;
        for r in results {
            r.expect("不得 panic");
        }

        // 终态一致性（settle 后）
        tokio::time::sleep(Duration::from_millis(300)).await;
        let l = list(&client, &d.base).await;
        let listed: std::collections::HashSet<String> = l
            .iter()
            .filter_map(|t| t["task_id"].as_str().map(String::from))
            .collect();
        for id in &ids {
            let st = client
                .get(format!("{}/tasks/{id}", d.base))
                .send()
                .await
                .unwrap()
                .status();
            let in_list = listed.contains(id);
            assert_eq!(
                in_list,
                st == 200,
                "任务 {id}: 列表={in_list} GET={st} —— 记录与账本脱节"
            );
        }
        // 收尾：清空
        for id in listed {
            let _ = delete(&client, &d.base, &id).await;
        }
    })
    .await
    .expect("超时 —— 疑似死锁");
}

// ============ B-2 #4：账本并发写 ============

/// with_storage + 并发 40 add / 20 remove 交错（每步触发 autosave）+
/// 后台读者持续解析 tasks.json。断言：① 任意时刻文件可解析（原子
/// rename + persist_lock 串行化的用户可见效果：重启不丢任务）；② 最终
/// 文件条数 == 内存列表条数。
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn b2_4_ledger_concurrent_write_no_corruption() {
    tokio::time::timeout(TIMEOUT, async {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("tasks.json");
        let srv = TestServer::start(patterned(64 * 1024)).await;
        let d = serve_with(|s| s.with_storage(storage.clone())).await;
        let client = reqwest::Client::new();

        // 后台读者：持续读+解析（P1-1 原子写 + persist_lock 的实机验证）
        let read_path = storage.clone();
        let reader = tokio::spawn(async move {
            let mut parsed = 0usize;
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            while std::time::Instant::now() < deadline {
                if let Ok(text) = std::fs::read_to_string(&read_path) {
                    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| {
                        panic!("tasks.json 并发写损坏（不可解析）: {e}\n{text}")
                    });
                    assert!(v.is_array(), "tasks.json 顶层必须是数组: {v}");
                    parsed += 1;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            parsed
        });

        // 并发写：40 add + 20 remove 交错（一半任务先建后删）
        let mut handles = Vec::new();
        for k in 0..40usize {
            let c = client.clone();
            let base = d.base.clone();
            let url = srv.url();
            handles.push(tokio::spawn(async move {
                let (st, id) = add(&c, &base, &url, k).await;
                assert_eq!(st, reqwest::StatusCode::CREATED);
                id
            }));
        }
        let added: Vec<String> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        // 交错删除后 20 个（先建的）
        let mut rm = Vec::new();
        for id in added.iter().take(20) {
            let c = client.clone();
            let base = d.base.clone();
            let id = id.clone();
            rm.push(tokio::spawn(async move {
                let st = delete(&c, &base, &id).await;
                assert_eq!(st, reqwest::StatusCode::NO_CONTENT);
            }));
        }
        for r in futures::future::join_all(rm).await {
            r.expect("remover 不得 panic");
        }

        // 最终一致：文件条数 == 列表条数
        let l = list(&client, &d.base).await;
        let text = std::fs::read_to_string(&storage).unwrap();
        let persisted: serde_json::Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("最终 tasks.json 不可解析: {e}"));
        let persisted_n = persisted.as_array().map(|a| a.len()).unwrap_or(0);
        assert_eq!(
            persisted_n,
            l.len(),
            "账本条数 {} != 内存列表条数 {}（持久化丢写）",
            persisted_n,
            l.len()
        );
        assert_eq!(l.len(), 20, "40 add - 20 remove = 20（实际 {}）", l.len());

        // 读者确实读过（否则上面的解析断言是空转）
        let parsed = reader.await.unwrap();
        assert!(parsed > 0, "后台读者必须实际读到过文件（否则断言空转）");
    })
    .await
    .expect("超时 —— 疑似死锁");
}

// ============ B-2 #5：RSS 并发修改 ============

/// 并发添加 10 个不同 feed（每个 add：拉取 → 锁内入列 → 锁外落盘）→
/// rss.json 必须包含全部 10 个（磁盘不丢失更新）。这正是 P1-9 锁外
/// 落盘引入的写序风险点：并发快照交错落盘时，旧快照可能后写覆盖新
/// 快照（tasks.json 的 P1-1 同款缺陷在 RSS 侧的翻版）。
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn b2_5_rss_concurrent_add_no_lost_update() {
    tokio::time::timeout(TIMEOUT, async {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("tasks.json"); // rss.json 派生自同目录
        let host = serve_feed_host().await;
        let d = serve_with(|s| s.with_storage(storage.clone())).await;
        let client = reqwest::Client::new();

        let mut handles = Vec::new();
        for n in 1..=10u16 {
            let c = client.clone();
            let base = d.base.clone();
            let url = format!("{host}/feed/{n}");
            handles.push(tokio::spawn(async move {
                let resp = c
                    .post(format!("{base}/rss/feeds"))
                    .json(&serde_json::json!({ "url": url }))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(
                    resp.status(),
                    201,
                    "feed 添加不得因并发失败: {}",
                    resp.text().await.unwrap_or_default()
                );
            }));
        }
        for r in futures::future::join_all(handles).await {
            r.expect("adder 不得 panic");
        }

        // 用户可见效果 1：列表有全部 10 个
        let feeds: serde_json::Value = client
            .get(format!("{}/rss/feeds", d.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let n_listed = feeds["feeds"].as_array().map(|a| a.len()).unwrap_or(0);
        assert_eq!(n_listed, 10, "内存列表丢 feed: {n_listed}/10");

        // 用户可见效果 2：磁盘不丢失更新（重启恢复的数据源）
        let rss_path = dir.path().join("rss.json");
        let text =
            std::fs::read_to_string(&rss_path).unwrap_or_else(|e| panic!("rss.json 未落盘: {e}"));
        let v: serde_json::Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("rss.json 损坏: {e}"));
        let n_disk = v["feeds"].as_array().map(|a| a.len()).unwrap_or(0);
        assert_eq!(
            n_disk, 10,
            "rss.json 丢失更新（磁盘 {n_disk}/10，内存 10/10）—— P1-9 锁外落盘写序竞态"
        );
    })
    .await
    .expect("超时 —— 疑似死锁");
}

// ============ B-2 #7：限速器并发 ============

/// 全局限速 192KiB/s × 2 个并发任务（每任务服务器能力 ≈1MiB/s）→
/// 3s 稳定窗口内两任务合计吞吐 ≤ 192 × 1.3 KiB/s（多任务共享全局
/// 限速器的用户可见效果：总速率不超标——单任务限速契约 #10 在
/// contract_matrix.rs 已覆盖，本条补并发维度）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn b2_7_global_limiter_concurrent_tasks_total_rate_capped() {
    tokio::time::timeout(TIMEOUT, async {
        // 每任务能力 ≈ 1MiB/s（2MB / 40 × 50ms），不限速时合计 ≈2MiB/s
        let srv = SlowTestServer::start(patterned(4 * 1024 * 1024), 80, 50).await;
        let d = serve_with(|s| s.with_global_limits(192, 0)).await;
        let client = reqwest::Client::new();

        let mut ids = Vec::new();
        for k in 0..2usize {
            let (st, id) = add(&client, &d.base, &srv.url(), k).await;
            assert_eq!(st, reqwest::StatusCode::CREATED);
            ids.push(id);
        }

        // 等两个任务都在途 + 限速生效
        tokio::time::sleep(Duration::from_millis(800)).await;
        let mut before = 0u64;
        for id in &ids {
            let s: serde_json::Value = client
                .get(format!("{}/tasks/{id}", d.base))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            before += s["done"].as_u64().unwrap_or(0);
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
        let mut after = 0u64;
        for id in &ids {
            let s: serde_json::Value = client
                .get(format!("{}/tasks/{id}", d.base))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            after += s["done"].as_u64().unwrap_or(0);
        }

        let total_kbps = after.saturating_sub(before) as f64 / 1024.0 / 3.0;
        assert!(
            total_kbps <= 192.0 * 1.3,
            "全局 192KiB/s 但两任务合计实测 {total_kbps:.1}KiB/s —— 共享限速器并发失效"
        );

        for id in ids {
            let _ = delete(&client, &d.base, &id).await;
        }
    })
    .await
    .expect("超时 —— 疑似死锁");
}
