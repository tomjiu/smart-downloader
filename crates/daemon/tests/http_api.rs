//! M6: HTTP API（axum）——POST /tasks 添加（重复 → 409 + DuplicateRejected 事件）、
//! GET /tasks/:id 快照（跳号补拉入口）、GET /tasks 列表、pause/resume、
//! GET /providers 运行态快照。WS 升级端点为骨架（协议逻辑在 WsHub 测试覆盖）。

mod common;

use base64::Engine;
use common::{patterned, TestServer};
use smart_dl_daemon::events::SchedulerEvent;
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_daemon::ws::WsHub;
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;

async fn serve() -> (std::net::SocketAddr, Arc<DaemonState>) {
    let engine = HttpEngine::new(reqwest::Client::new());
    // 安全修复（V2）适配：测试的显式 dest 落在系统临时目录（/tmp/m6-test-*），
    // 必须把它注入为白名单根，否则 dest 预检按越界拒绝（400）。
    // bt 构建下注入 BtEngine；非 bt 构建纯 HTTP（双态声明，两个 cfg 均零警告）
    #[cfg(feature = "bt")]
    let state = {
        // 生产契约（config.bt_save_path）：`[bt] save_path` 缺省 = `[download] dest_root`，
        // 即引擎 save_path 必须与 default_dest_root 一致。本文件大量 HTTP 测试的显式
        // dest（/tmp/m6-test-*）依赖 temp_dir 白名单根，default 不能改——故把 BT 引擎
        // save_path 对齐 temp_dir()。测试 magnet 均为假 btih（无 metadata），remove 时
        // save_fastresume 走未就绪分支不落盘，save_path 零残留。
        let bt = smart_dl_daemon::bt::BtEngine::new(
            std::env::temp_dir().as_path(),
            None,
            0,
            0,
            false,
            false,
            false,
        )
        .expect("bt engine");
        DaemonState::new(Arc::new(engine), vec![])
            .with_dest_root(std::env::temp_dir())
            .with_bt(Arc::new(bt))
    };
    #[cfg(not(feature = "bt"))]
    let state = DaemonState::new(Arc::new(engine), vec![]).with_dest_root(std::env::temp_dir());
    let state = Arc::new(state);
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state)
}

/// 添加任务（dest 指向独立临时目录，避免引擎把产物写到测试 CWD）。
/// dest 用 进程号+计数器 保证唯一（并行测试下 nanos 会碰撞——曾致 400）。
async fn add_task(
    client: &reqwest::Client,
    base: &str,
    url: &str,
) -> (reqwest::StatusCode, serde_json::Value) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static DEST_SEQ: AtomicU64 = AtomicU64::new(0);
    let dest = std::env::temp_dir().join(format!(
        "m6-test-{}-{}",
        std::process::id(),
        DEST_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": url, "dest": dest.to_str().unwrap() }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.json().await.unwrap_or(serde_json::Value::Null);
    (status, body)
}

#[tokio::test]
async fn add_task_then_get_snapshot_and_list() {
    let body = patterned(64 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = add_task(&client, &base, &srv.url()).await;
    assert_eq!(resp.0, reqwest::StatusCode::CREATED, "添加任务必须 201");
    let created = resp.1;
    let tid = created["task_id"].as_str().unwrap().to_string();

    // GET /tasks/:id 快照（跳号补拉入口）
    let snap: serde_json::Value = client
        .get(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snap["task_id"], tid);

    // GET /tasks 列表
    let list: serde_json::Value = client
        .get(format!("{base}/tasks"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn duplicate_add_rejected_with_event() {
    let body = patterned(32 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let first = add_task(&client, &base, &srv.url()).await;
    assert_eq!(first.0, reqwest::StatusCode::CREATED);

    let second = add_task(&client, &base, &srv.url()).await;
    assert_eq!(
        second.0,
        reqwest::StatusCode::CONFLICT,
        "重复 canonical 必须拒绝"
    );

    // DuplicateRejected 事件已发布（seq 递增）
    let drained = state.hub().drain();
    let events: Vec<&SchedulerEvent> = drained.iter().map(|e| &e.event).collect();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SchedulerEvent::DuplicateRejected { .. })),
        "重复拒绝必须发 DuplicateRejected 事件"
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, SchedulerEvent::TaskCreated { .. })));
}

#[tokio::test]
async fn pause_resume_via_http() {
    let body = patterned(16 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = add_task(&client, &base, &srv.url()).await;
    assert_eq!(resp.0, reqwest::StatusCode::CREATED);
    let tid = resp.1["task_id"].as_str().unwrap().to_string();

    let p = client
        .post(format!("{base}/tasks/{tid}/pause"))
        .send()
        .await
        .unwrap();
    assert!(p.status().is_success());
    let r = client
        .post(format!("{base}/tasks/{tid}/resume"))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    assert!(state.task_snapshot(&tid).await.is_some(), "任务仍存在");
}

#[tokio::test]
async fn provider_status_snapshot() {
    // Provider 健康/配额/冷却快照（GET /providers）
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .get(format!("{base}/providers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(resp.is_array(), "providers 快照必须是数组");
}

#[tokio::test]
async fn unknown_task_returns_404() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/tasks/ghost"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[test]
fn hub_wired_into_state() {
    // DaemonState 持有 WsHub（事件发布统一入口）
    let engine = HttpEngine::new(reqwest::Client::new());
    let state = DaemonState::new(Arc::new(engine), vec![]);
    let hub: &WsHub = state.hub();
    assert_eq!(hub.last_seq(), 0);
}

#[tokio::test]
async fn same_resource_different_tokens_deduped_d34() {
    // D34：canonical 身份剥离 token 参数 → 同资源不同签名 token 判为同一任务（409）
    let body = patterned(16 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let (s1, _) = add_task(&client, &base, &format!("{}?token=aaa", srv.url())).await;
    assert_eq!(s1, reqwest::StatusCode::CREATED, "首次添加应 201");

    let (s2, b2) = add_task(&client, &base, &format!("{}?token=bbb", srv.url())).await;
    assert_eq!(s2, reqwest::StatusCode::CONFLICT, "token 不同仍应判重复");
    assert!(
        b2["error"].as_str().unwrap().contains("duplicate"),
        "错误信息应含 duplicate: {b2}"
    );
}

#[tokio::test]
async fn distinct_query_params_are_distinct_tasks() {
    // 非 token 参数差异 → 不同 canonical → 允许添加
    let body = patterned(16 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let (s1, _) = add_task(&client, &base, &format!("{}?v=1", srv.url())).await;
    assert_eq!(s1, reqwest::StatusCode::CREATED);
    let (s2, _) = add_task(&client, &base, &format!("{}?v=2", srv.url())).await;
    assert_eq!(s2, reqwest::StatusCode::CREATED, "v=1/v=2 是不同资源");
}

// ---- 迅雷链接家族归一化（thunder:// / qqdl://）----

fn thunder_link(real: &str) -> String {
    let inner = format!("AA{real}ZZ");
    format!(
        "thunder://{}",
        base64::engine::general_purpose::STANDARD.encode(inner.as_bytes())
    )
}

fn qqdl_link(real: &str) -> String {
    format!(
        "qqdl://{}",
        base64::engine::general_purpose::STANDARD.encode(real.as_bytes())
    )
}

#[tokio::test]
async fn thunder_link_decoded_and_added() {
    // thunder:// = base64("AA"+url+"ZZ") → 归一化后走 HTTP 引擎 → 201
    let body = patterned(16 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = add_task(&client, &base, &thunder_link(&srv.url())).await;
    assert_eq!(
        resp.0,
        reqwest::StatusCode::CREATED,
        "thunder:// 应解码并 201"
    );
    let tid = resp.1["task_id"].as_str().unwrap().to_string();

    // 快照 source 是解码后的真实 URL（非 thunder:// 壳）
    let snap: serde_json::Value = client
        .get(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let src = snap["source"].as_str().unwrap();
    assert!(!src.starts_with("thunder://"), "必须已解码: {src}");
    assert!(src.contains(&srv.url()), "含真实 URL: {src}");
}

#[tokio::test]
async fn qqdl_link_decoded_and_added() {
    // qqdl:// = base64(url)（无 AA/ZZ 壳）→ 201
    let body = patterned(16 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = add_task(&client, &base, &qqdl_link(&srv.url())).await;
    assert_eq!(resp.0, reqwest::StatusCode::CREATED, "qqdl:// 应解码并 201");
}

#[cfg(feature = "bt")]
#[tokio::test]
async fn magnet_ed2k_unknown_rejected_with_clear_error() {
    // 归一化分类：magnet→BT；ed2k→不支持；未知→无法识别
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": "magnet:?xt=urn:btih:0d2c9c9d5c2d3e8f9a1b2c3d4e5f6a7b8c9d0e1f&dn=test" }))
        .send()
        .await
        .unwrap();
    let s = resp.status();
    let b = resp
        .json::<serde_json::Value>()
        .await
        .unwrap_or(serde_json::Value::Null);
    assert_eq!(
        s,
        reqwest::StatusCode::CREATED,
        "magnet 应创建 BT 任务: {b}"
    );
    assert!(b["task_id"].as_str().unwrap().starts_with('t'), "{b}");
    // BT 任务必须落到全局 save_path（v1 约束），再删掉避免污染后续测试
    let tid = b["task_id"].as_str().unwrap().to_string();
    let _ = client
        .post(format!("{base}/tasks/{tid}/remove"))
        .send()
        .await;

    let (s, b) = add_task(&client, &base, "ed2k://file|a|1|hash|").await;
    assert_eq!(s, reqwest::StatusCode::BAD_REQUEST);
    assert!(b["error"].as_str().unwrap().contains("ed2k"), "{b}");

    let (s, b) = add_task(&client, &base, "sqla://whatever").await;
    assert_eq!(s, reqwest::StatusCode::BAD_REQUEST);
    assert!(b["error"].as_str().unwrap().contains("无法识别"), "{b}");

    // 畸形 thunder://（坏 base64）同样 400
    let (s, b) = add_task(&client, &base, "thunder://!!!not-base64!!!").await;
    assert_eq!(s, reqwest::StatusCode::BAD_REQUEST);
    assert!(b["error"].as_str().unwrap().contains("thunder"), "{b}");
}

// ---- D37 端点补齐：/config、/tasks/:id/logs、/tasks/:id/fallback ----

#[tokio::test]
async fn config_endpoint_returns_injected_snapshot() {
    // with_config 注入 → GET /config 返回精简快照
    let engine = HttpEngine::new(reqwest::Client::new());
    let state = Arc::new(
        DaemonState::new(Arc::new(engine), vec![])
            .with_config(serde_json::json!({ "dest_root": "/data/dl", "note": "test" })),
    );
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}");
    let resp: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["dest_root"], "/data/dl", "config 应含注入的 dest_root");
    assert_eq!(resp["note"], "test");
}

#[tokio::test]
async fn task_logs_source_is_redacted() {
    // H-1 回归：`GET /tasks/:id/logs` 的 source 快照必须经 redacted_debug()——
    // 源 URL 中的 userinfo 凭据不得明文外溢（state.rs 曾漏改一处裸 format!(\"{:?}\")）。
    let body = patterned(1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let cred_url = format!("http://alice:sup3rs3cret@{}/file", srv.addr);
    let resp = add_task(&client, &base, &cred_url).await;
    assert_eq!(
        resp.0,
        reqwest::StatusCode::CREATED,
        "带凭据的 URL 应可建任务: {:?}",
        resp.1
    );
    let tid = resp.1["task_id"].as_str().unwrap().to_string();

    let logs: serde_json::Value = client
        .get(format!("{base}/tasks/{tid}/logs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let source = logs["source"].as_str().unwrap_or_default();
    assert!(
        !source.contains("sup3rs3cret"),
        "userinfo 密码不得出现在 logs source: {source}"
    );
    assert!(
        source.contains("***@"),
        "source 应为脱敏形态（***@host）: {source}"
    );
}

#[tokio::test]
async fn task_logs_returns_add_event() {
    // add 任务 → GET /tasks/:id/logs → events 含 add 操作
    let body = patterned(8 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = add_task(&client, &base, &srv.url()).await;
    assert_eq!(resp.0, reqwest::StatusCode::CREATED);
    let tid = resp.1["task_id"].as_str().unwrap().to_string();

    let logs: serde_json::Value = client
        .get(format!("{base}/tasks/{tid}/logs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(logs["task_id"], tid);
    assert_eq!(logs["state"], "Queued");
    let events = logs["events"].as_array().unwrap();
    assert!(
        events.iter().any(|e| e["op"] == "add"),
        "logs 必须含 add 事件: {logs}"
    );
}

#[tokio::test]
async fn fallback_on_missing_task_returns_404() {
    // M6 已接线：不存在的任务 → 404（不再 501）
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let resp = reqwest::Client::new()
        .post(format!("{base}/tasks/t1/fallback"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "fallback 不存在任务应 404"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("not found"),
        "{body}"
    );
}

#[tokio::test]
async fn fallback_on_http_task_is_rejected() {
    // M6：兜底仅面向 BT 任务（HTTP 任务直接拒绝）
    let body = patterned(8 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = add_task(&client, &base, &srv.url()).await;
    assert_eq!(resp.0, reqwest::StatusCode::CREATED);
    let tid = resp.1["task_id"].as_str().unwrap().to_string();

    let fr = client
        .post(format!("{base}/tasks/{tid}/fallback"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        fr.status(),
        reqwest::StatusCode::CONFLICT,
        "HTTP 任务兜底应 409"
    );
    let fb: serde_json::Value = fr.json().await.unwrap();
    assert!(
        fb["error"].as_str().unwrap().contains("仅 BT 任务"),
        "错误应说明只支持 BT: {fb}"
    );
}

/// 等任务快照 state（引擎状态映射）到目标值（最长 10s）。
async fn wait_snapshot_state(state: &Arc<DaemonState>, id: &str, want: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if let Some(s) = state.task_snapshot(id).await {
            if s.state == want {
                return;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "60s 内未到 {want}: {id}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// 等 list（记录 state——HTTP 状态推进循环写入）到目标值（最长 10s）。
async fn wait_list_state(client: &reqwest::Client, base: &str, id: &str, want: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let list: Vec<serde_json::Value> = client
            .get(format!("{base}/tasks"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let Some(t) = list.iter().find(|t| t["task_id"] == id) {
            if t["state"] == want {
                return;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "60s 内 list 未到 {want}: {id}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// 等事件中枢出现匹配事件（轮询 drain；最长 10s）。
async fn wait_event(
    state: &Arc<DaemonState>,
    want: impl Fn(&SchedulerEvent) -> bool,
) -> Vec<smart_dl_daemon::events::Envelope> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let drained = state.hub().drain();
        if drained.iter().any(|e| want(&e.event)) {
            return drained;
        }
        assert!(std::time::Instant::now() < deadline, "60s 内未等到目标事件");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// HTTP 终态推进（serve 装配路径）：http_events 循环轮询 → 记录推进 → list 显示
/// Completed + 事件广播；二次轮询无效果（幂等，不重复广播）。
#[tokio::test]
async fn http_task_completed_advances_list_state() {
    let body = patterned(64 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, state) = serve().await;
    // serve 装配：状态推进循环（测试用 100ms 加速轮询）
    let _h = smart_dl_daemon::http_events::spawn_http_events(
        state.clone(),
        std::time::Duration::from_millis(100),
    );
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let (status, b) = add_task(&client, &base, &srv.url()).await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "add 应 201: {b}");
    let tid = b["task_id"].as_str().unwrap().to_string();

    // 引擎先完成（快照实时化）→ 循环把记录推进 Completed → list 与 status 一致
    wait_snapshot_state(&state, &tid, "Completed").await;
    wait_list_state(&client, &base, &tid, "Completed").await;
    // 事件广播（Completed + StateChanged）
    wait_event(
        &state,
        |e| matches!(e, SchedulerEvent::Completed { task_id } if task_id == &tid),
    )
    .await;
    // 幂等：再轮询无新效果（不会重复推进/广播）
    let again = state.poll_http_task_states().await;
    assert!(again.is_empty(), "已终态任务不应重复推进: {again:?}");
}

/// HTTP 失败推进：引擎 Error → 记录 Failed → list 显示 Failed + Failed 事件 + error。
/// 脆弱服务器：首个请求（probe bytes=0-0）206 通过预检 → 后续下载请求 500（运行期失败）。
#[tokio::test]
async fn http_task_failure_marks_failed_in_list() {
    use axum::{
        body::Body,
        http::HeaderMap,
        response::{IntoResponse, Response},
        routing::get,
        Router,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    let hits = Arc::new(AtomicUsize::new(0));
    let frag = hits.clone();
    let app = Router::new().route(
        "/fragile",
        get(move |_h: HeaderMap| async move {
            let n = frag.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Response::builder()
                    .status(206)
                    .header("Content-Range", "bytes 0-0/4096")
                    .header("Accept-Ranges", "bytes")
                    .header("Content-Length", "1")
                    .body(Body::from(vec![0u8; 1]))
                    .unwrap()
                    .into_response()
            } else {
                Response::builder()
                    .status(500)
                    .body(Body::empty())
                    .unwrap()
                    .into_response()
            }
        }),
    );
    let frag_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let frag_addr = frag_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(frag_listener, app).await.unwrap();
    });

    let (addr, state) = serve().await;
    let _h = smart_dl_daemon::http_events::spawn_http_events(
        state.clone(),
        std::time::Duration::from_millis(100),
    );
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let (status, b) = add_task(&client, &base, &format!("http://{frag_addr}/fragile")).await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "add 应 201: {b}");
    let tid = b["task_id"].as_str().unwrap().to_string();

    wait_snapshot_state(&state, &tid, "Failed").await;
    wait_list_state(&client, &base, &tid, "Failed").await;
    // Failed 事件广播
    wait_event(
        &state,
        |e| matches!(e, SchedulerEvent::Failed { task_id, .. } if task_id == &tid),
    )
    .await;
    // 幂等
    let again = state.poll_http_task_states().await;
    assert!(again.is_empty(), "已 Failed 任务不应重复推进: {again:?}");
}

// ===== 安全回归（V1/V13）：API 认证中间件 =====

/// 带 token 的测试 server：`Authorization: Bearer test-token-123` 必须校验。
async fn serve_with_token() -> std::net::SocketAddr {
    let engine = HttpEngine::new(reqwest::Client::new());
    let state =
        DaemonState::new(Arc::new(engine), vec![]).with_http_token(Some("test-token-123".into()));
    let state = Arc::new(state);
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn auth_required_when_token_configured() {
    let addr = serve_with_token().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    // 无 Authorization → 401（快照/列表/配置三个代表性端点）
    for path in ["/tasks", "/config", "/providers"] {
        let r = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(
            r.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "GET {path} 应 401"
        );
    }

    // 错误 token → 401
    let r = client
        .get(format!("{base}/tasks"))
        .header("Authorization", "Bearer wrong-token")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED);

    // 非 Bearer scheme → 401
    let r = client
        .get(format!("{base}/tasks"))
        .header("Authorization", "Basic dXNlcjpwYXNz")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED);

    // 正确 token → 200
    let r = client
        .get(format!("{base}/tasks"))
        .header("Authorization", "Bearer test-token-123")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::OK, "正确 token 应放行");
}

#[tokio::test]
async fn auth_open_when_token_not_configured() {
    // 未配置 token（回环兼容模式）→ 不带 Authorization 也放行
    let (addr, _state) = serve().await;
    let client = reqwest::Client::new();
    let r = client
        .get(format!("http://{addr}/tasks"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        reqwest::StatusCode::OK,
        "未配置 token 时（回环）应保持兼容放行"
    );
}

#[test]
fn verify_http_token_unit() {
    let engine = HttpEngine::new(reqwest::Client::new());
    let bare = DaemonState::new(Arc::new(engine), vec![]);
    assert!(bare.verify_http_token(None));
    assert!(bare.verify_http_token(Some("whatever")));

    let engine2 = HttpEngine::new(reqwest::Client::new());
    let secured = DaemonState::new(Arc::new(engine2), vec![]).with_http_token(Some("t-abc".into()));
    assert!(!secured.verify_http_token(None));
    assert!(!secured.verify_http_token(Some("Bearer wrong")));
    assert!(!secured.verify_http_token(Some("t-abc"))); // 必须带 Bearer 前缀
    assert!(secured.verify_http_token(Some("Bearer t-abc")));
}

/// P2 运维 API：/health 存活探针 + /version 构建信息。
#[tokio::test]
async fn health_and_version_report_build_info() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let health: serde_json::Value = client
        .get(format!("{base}/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["status"], "ok");

    let version: serde_json::Value = client
        .get(format!("{base}/version"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(version["name"], "smart-dl-daemon");
    // 集成测试与 daemon 同包，CARGO_PKG_VERSION 一致
    assert_eq!(version["version"], env!("CARGO_PKG_VERSION"));
    // features 是布尔对象（部署矩阵对齐：构建组合一目了然）
    let feats = version["features"].as_object().expect("features 对象");
    assert!(!feats.is_empty());
    assert!(feats.values().all(|v| v.is_boolean()));
}

/// P2 运维 API：/stats 聚合（初始 0 → 加 1 任务后 total=1 且 by_state/by_engine 有值）。
#[tokio::test]
async fn stats_reflect_task_counts() {
    let body = patterned(64 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    // 初始：total = 0
    let stats: serde_json::Value = client
        .get(format!("{base}/stats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats["total"], 0);
    assert_eq!(stats["down_bytes_s"], 0);

    // 添加 1 个 HTTP 任务 → total=1，by_state/by_engine 各有 1 个键
    let resp = add_task(&client, &base, &srv.url()).await;
    assert_eq!(resp.0, reqwest::StatusCode::CREATED);

    let stats: serde_json::Value = client
        .get(format!("{base}/stats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats["total"], 1);
    let by_state = stats["by_state"].as_object().expect("by_state 对象");
    assert_eq!(
        by_state.values().filter_map(|v| v.as_u64()).sum::<u64>(),
        1,
        "by_state 聚合必须覆盖全部任务"
    );
    let by_engine = stats["by_engine"].as_object().expect("by_engine 对象");
    assert_eq!(by_engine.get("http"), Some(&serde_json::json!(1)));
    // bt 构建下该测试也可能有 BT 引擎注册，但无 BT 任务 → by_engine 无 bt 键
    assert!(by_engine.get("bt").is_none());
}

// ============ 任务级限速（POST /tasks/:id/limit，P1 能力增强）============

#[tokio::test]
async fn task_limit_set_then_merge_and_snapshot_echo() {
    let body = patterned(64 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let (status, created) = add_task(&client, &base, &srv.url()).await;
    assert_eq!(status, reqwest::StatusCode::CREATED);
    let tid = created["task_id"].as_str().unwrap().to_string();

    // 快照初始无 limits 字段（None → 序列化跳过）
    let snap: serde_json::Value = client
        .get(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(snap.get("limits").is_none(), "未设置时快照不出 limits");

    // 首设 down=128
    let resp = client
        .post(format!("{base}/tasks/{tid}/limit"))
        .json(&serde_json::json!({ "down_kb_s": 128 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let snap: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(snap["limits"]["down_kb_s"], 128);
    assert!(snap["limits"].get("up_kb_s").is_none(), "up 未设置不回显");

    // 合并语义：只传 down=0（显式不限）→ down 覆盖、其余保持
    let resp = client
        .post(format!("{base}/tasks/{tid}/limit"))
        .json(&serde_json::json!({ "down_kb_s": 0 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let snap: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(snap["limits"]["down_kb_s"], 0, "0 = 显式不限");

    // 空请求体（两方向都缺省）→ 合并保持既有配置，200
    let resp = client
        .post(format!("{base}/tasks/{tid}/limit"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let snap: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(snap["limits"]["down_kb_s"], 0, "空请求沿用既有值");
}

#[tokio::test]
async fn task_limit_up_direction_rejected_for_http_task() {
    // HTTP 任务无上传方向 → 409（state 层预拒，非 500）
    let body = patterned(16 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let (_status, created) = add_task(&client, &base, &srv.url()).await;
    let tid = created["task_id"].as_str().unwrap().to_string();

    let resp = client
        .post(format!("{base}/tasks/{tid}/limit"))
        .json(&serde_json::json!({ "up_kb_s": 64 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("up_kb_s"),
        "错误信息应指明 up_kb_s 不适用: {body}"
    );
}

#[tokio::test]
async fn task_limit_unknown_task_404() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks/t-nope/limit"))
        .json(&serde_json::json!({ "down_kb_s": 128 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn add_task_with_down_kb_s_applies_limit() {
    // 建任务请求携带 down_kb_s → 创建即生效（快照回显）
    let body = patterned(16 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let dest = std::env::temp_dir().join(format!("m6-limit-{}", std::process::id()));
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": srv.url(),
            "dest": dest.to_str().unwrap(),
            "down_kb_s": 256
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: serde_json::Value = resp.json().await.unwrap();
    let tid = created["task_id"].as_str().unwrap().to_string();

    let snap: serde_json::Value = client
        .get(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snap["limits"]["down_kb_s"], 256, "建任务时限速即生效");
}

#[tokio::test]
async fn http_task_file_priority_conflict() {
    // 子文件优先级仅 BT 任务：HTTP 任务 → 409（双构建通用，不依赖 bt feature）
    let body = patterned(16 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let (_status, created) = add_task(&client, &base, &srv.url()).await;
    let tid = created["task_id"].as_str().unwrap().to_string();

    let resp = client
        .post(format!("{base}/tasks/{tid}/files/priority"))
        .json(&serde_json::json!({ "priorities": [ { "index": 0, "priority": 0 } ] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
}

// ==================== E6 add API 能力对齐（sha256/headers/name+backup） ====================

use sha2::{Digest, Sha256};

/// 轮询快照到终态（Completed/Error，30s 超时）。
async fn poll_terminal(client: &reqwest::Client, base: &str, tid: &str) -> serde_json::Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let snap: serde_json::Value = client
            .get(format!("{base}/tasks/{tid}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let st = snap["state"].as_str().unwrap_or("");
        if st == "Completed" || st == "Failed" || st == "Error" {
            return snap;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "任务 30s 未到终态: {snap}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// E6 主例：API 传入 sha256 → 引擎校验链生效。正确校验和 → Completed 无告警；
/// 错误校验和 → 降级接受仍 Completed + 告警含 sha256（Q-B5 语义经 API 保持）。
#[tokio::test]
async fn add_task_with_sha256_e2e() {
    let body = patterned(64 * 1024);
    let mut hasher = Sha256::new();
    hasher.update(&body);
    let good = format!("{:x}", hasher.finalize());
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    // 正确 sha256 → Completed 无告警
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": srv.url(),
            "dest": std::env::temp_dir().join(format!("e6-sha-ok-{}", std::process::id())),
            "sha256": good,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();
    let snap = poll_terminal(&client, &base, &tid).await;
    assert_eq!(snap["state"], "Completed", "正确 sha256 必须完成: {snap}");
    assert!(snap["error"].is_null(), "正确 sha256 不得告警: {snap}");

    // 错误 sha256 → 降级接受（Completed）+ 告警含 sha256
    // （第二个服务实例：同 URL 二次添加会被 canonical 查重 409）
    let srv2 = TestServer::start(patterned(64 * 1024)).await;
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": srv2.url(),
            "dest": std::env::temp_dir().join(format!("e6-sha-bad-{}", std::process::id())),
            "sha256": "ab".repeat(32),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();
    let snap = poll_terminal(&client, &base, &tid).await;
    assert_eq!(
        snap["state"], "Completed",
        "降级接受语义（Q-B5）经 API 保持: {snap}"
    );
    let err = snap["error"].as_str().unwrap_or_default();
    assert!(err.contains("sha256"), "告警应定性 sha256: {err}");
}

/// E6 headers：API 传入自定义头 → 探测/段下载全链下发（强校验服务端：缺头
/// 即 403）。带正确头 → Completed；不带 → 任务 Failed（探测即拒）。
#[tokio::test]
async fn add_task_with_headers_forwarded_e2e() {
    use axum::extract::Request;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing;
    use axum::Router;

    let app = Router::new().fallback(routing::any(|req: Request| async move {
        let ok = req
            .headers()
            .get("x-test-token")
            .and_then(|v| v.to_str().ok())
            .map(|v| v == "s3cret-token")
            .unwrap_or(false);
        if !ok {
            return StatusCode::FORBIDDEN.into_response();
        }
        let body = vec![0x5Au8; 256 * 1024];
        let total = body.len() as u64;
        // Range 支持（206）：引擎段请求带 bytes=start-end，必须切回 206
        let range = req
            .headers()
            .get("range")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes="))
            .and_then(|v| v.split_once('-'))
            .and_then(|(s, e)| Some((s.parse::<u64>().ok()?, e.parse::<u64>().ok()?)));
        if let Some((s, e)) = range {
            let e = e.min(total - 1);
            let payload = body[s as usize..=(e as usize)].to_vec();
            return axum::response::Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header("content-range", format!("bytes {s}-{e}/{total}"))
                .body(axum::body::Body::from(payload))
                .unwrap()
                .into_response();
        }
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("content-length", body.len())
            .body(axum::body::Body::from(body))
            .unwrap()
            .into_response()
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let url = format!("http://{addr}/guarded.bin");

    let (saddr, _state) = serve().await;
    let base = format!("http://{saddr}");
    let client = reqwest::Client::new();

    // 带正确头 → 完成
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": url,
            "dest": std::env::temp_dir().join(format!("e6-hdr-ok-{}", std::process::id())),
            "headers": { "X-Test-Token": "s3cret-token" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();
    let snap = poll_terminal(&client, &base, &tid).await;
    assert_eq!(snap["state"], "Completed", "带正确头必须完成: {snap}");

    // 不带头 → 探测 403 → 任务失败（第二个服务实例：同 URL 二次添加会 canonical 409）
    let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr2 = listener2.local_addr().unwrap();
    let app2 = Router::new().fallback(routing::any(|req: Request| async move {
        let ok = req
            .headers()
            .get("x-test-token")
            .and_then(|v| v.to_str().ok())
            .map(|v| v == "s3cret-token")
            .unwrap_or(false);
        if !ok {
            return StatusCode::FORBIDDEN.into_response();
        }
        let body = vec![0x5Au8; 4096];
        let total = body.len() as u64;
        let range = req
            .headers()
            .get("range")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes="))
            .and_then(|v| v.split_once('-'))
            .and_then(|(s, e)| Some((s.parse::<u64>().ok()?, e.parse::<u64>().ok()?)));
        if let Some((s, e)) = range {
            let e = e.min(total - 1);
            let payload = body[s as usize..=(e as usize)].to_vec();
            return axum::response::Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header("content-range", format!("bytes {s}-{e}/{total}"))
                .body(axum::body::Body::from(payload))
                .unwrap()
                .into_response();
        }
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("content-length", body.len())
            .body(axum::body::Body::from(body))
            .unwrap()
            .into_response()
    }));
    tokio::spawn(async move {
        axum::serve(listener2, app2).await.unwrap();
    });
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": format!("http://{addr2}/guarded.bin"),
            "dest": std::env::temp_dir().join(format!("e6-hdr-miss-{}", std::process::id())),
        }))
        .send()
        .await
        .unwrap();
    // 引擎 add 探测失败 → add 返回错误（400/500 视错误映射），任务不创建或创建即失败
    let status = resp.status();
    if status == reqwest::StatusCode::CREATED {
        // 若实现为创建后失败，轮询到终态断言 Failed/Error
        let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
            .as_str()
            .unwrap()
            .to_string();
        let snap = poll_terminal(&client, &base, &tid).await;
        assert_ne!(snap["state"], "Completed", "缺头（403）不得完成: {snap}");
    } else {
        assert!(
            status == reqwest::StatusCode::BAD_REQUEST
                || status == reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            "探测失败应拒绝建任务: {status}"
        );
    }
}

/// E6 name + backup_url：主源 404 → 备用源兜底完成（E2 引擎语义经 API）；
/// 显式名落盘（E4 metadata.name 权威）。
#[tokio::test]
async fn add_task_with_name_and_backup_url_e2e() {
    let body = patterned(64 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let dest = std::env::temp_dir().join(format!("e6-backup-{}", std::process::id()));
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            // /missing 路径 TestServer 未注册 → 404 → 主源探测失败 → 备用源兜底
            "url": format!("http://{}/missing", srv.addr),
            "dest": dest,
            "backup_url": srv.url(),
            "name": "renamed-by-api.bin",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();
    let snap = poll_terminal(&client, &base, &tid).await;
    assert_eq!(
        snap["state"], "Completed",
        "主源 404 + 备用源兜底必须完成: {snap}"
    );
    let got = std::fs::read(dest.join("renamed-by-api.bin")).unwrap();
    assert_eq!(got.len(), 64 * 1024, "落盘应为备用源内容（显式名落位）");
}

/// E7 建 n 个不同 canonical 的任务（n 个独立 TestServer 各供一个 URL）。
async fn add_n_tasks(client: &reqwest::Client, base: &str, n: usize, body: &[u8]) -> Vec<String> {
    let mut ids = Vec::new();
    for i in 0..n {
        let srv = TestServer::start(body.to_vec()).await;
        let dest = std::env::temp_dir().join(format!(
            "e7-batch-{}-{i}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let resp = client
            .post(format!("{base}/tasks"))
            .json(&serde_json::json!({ "url": srv.url(), "dest": dest.to_str().unwrap() }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::CREATED, "第 {i} 个任务");
        ids.push(
            resp.json::<serde_json::Value>().await.unwrap()["task_id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    ids
}

/// E7 列表查询：分页 + X-Total-Count + engine 过滤回显 + 非法参数 400。
/// 状态过滤不在此赌真实下载竞态（state 层单测覆盖语义），只验证合法值 200。
#[tokio::test]
async fn list_tasks_query_filter_pagination_e2e() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let ids = add_n_tasks(&client, &base, 3, &patterned(16 * 1024)).await;

    // 兼容不变：无参数 → 全量数组；新字段 engine 恒回显
    let resp = client.get(format!("{base}/tasks")).send().await.unwrap();
    assert!(
        !resp.headers().contains_key("x-total-count"),
        "无分页参数不加 header"
    );
    let list: serde_json::Value = resp.json().await.unwrap();
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert!(
        arr.iter().all(|r| r["engine"] == "http"),
        "engine 标签必须回显: {arr:?}"
    );

    // 分页：limit=2&offset=1 → 第 2、3 个 + X-Total-Count=3（创建序确定性）
    let resp = client
        .get(format!("{base}/tasks?limit=2&offset=1"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.headers()
            .get("x-total-count")
            .and_then(|v| v.to_str().ok()),
        Some("3"),
        "X-Total-Count = 过滤后总数"
    );
    let page: serde_json::Value = resp.json().await.unwrap();
    let got: Vec<&str> = page
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["task_id"].as_str().unwrap())
        .collect();
    assert_eq!(got, &ids[1..3], "分页必须按创建序切片");

    // engine 过滤：http 命中全部；bt 为空
    let list: serde_json::Value = client
        .get(format!("{base}/tasks?engine=http"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 3);
    let list: serde_json::Value = client
        .get(format!("{base}/tasks?engine=bt"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.as_array().unwrap().is_empty());

    // 合法 state 值 200（数量不赌下载竞态）
    let status = client
        .get(format!("{base}/tasks?state=Paused,Completed"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, reqwest::StatusCode::OK);

    // 非法参数逐个 400（错误信息带合法值提示）
    for bad in ["state=Bogus", "engine=Excel", "limit=0", "limit=501"] {
        let resp = client
            .get(format!("{base}/tasks?{bad}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST, "{bad}");
        let text = resp.text().await.unwrap();
        assert!(!text.is_empty(), "{bad} 的 400 必须带错误说明");
    }
}

/// E7 批量 remove e2e + 请求校验：2 存在 + 1 不存在 → 200 逐项结果（部分失败
/// 不影响全局 200）；malformed 请求 400；全删后列表为空。
#[tokio::test]
async fn batch_remove_e2e_and_validation() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let ids = add_n_tasks(&client, &base, 3, &patterned(8 * 1024)).await;

    let resp = client
        .post(format!("{base}/tasks/batch"))
        .json(&serde_json::json!({
            "action": "remove",
            "ids": [ids[0], ids[1], ids[2], "t999"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "单项失败不改变全局 200"
    );
    let out: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(out["succeeded"], 3);
    assert_eq!(out["failed"], 1);
    let results = out["results"].as_array().unwrap();
    assert_eq!(results.len(), 4);
    let bad = results.iter().find(|r| r["id"] == "t999").unwrap();
    assert_eq!(bad["ok"], false);
    assert!(
        bad["error"].as_str().unwrap_or("").contains("not found"),
        "失败项必须带原因: {bad}"
    );

    // 全删后列表为空
    let list: serde_json::Value = client
        .get(format!("{base}/tasks"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.as_array().unwrap().is_empty());

    // malformed：未知 action / 空 ids / 超 100 上限 → 400
    let resp = client
        .post(format!("{base}/tasks/batch"))
        .json(&serde_json::json!({ "action": "explode", "ids": ["t1"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let resp = client
        .post(format!("{base}/tasks/batch"))
        .json(&serde_json::json!({ "action": "pause", "ids": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let many: Vec<String> = (0..101).map(|i| format!("t{i}")).collect();
    let resp = client
        .post(format!("{base}/tasks/batch"))
        .json(&serde_json::json!({ "action": "pause", "ids": many }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

/// E7 批量 pause/resume e2e（幸福路径走一遍 HTTP 线；单项失败语义在 state 层
/// 与 batch_remove e2e 覆盖）。对存在任务 batch pause → succeeded=2。
#[tokio::test]
async fn batch_pause_resume_e2e() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let ids = add_n_tasks(&client, &base, 2, &patterned(8 * 1024)).await;

    let resp = client
        .post(format!("{base}/tasks/batch"))
        .json(&serde_json::json!({ "action": "pause", "ids": ids }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let out: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(out["succeeded"], 2, "两任务均存在 → 全成功: {out}");
    assert_eq!(out["failed"], 0);

    let resp = client
        .post(format!("{base}/tasks/batch"))
        .json(&serde_json::json!({ "action": "resume", "ids": ids }))
        .send()
        .await
        .unwrap();
    let out: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(out["succeeded"], 2, "resume 回来: {out}");
}

/// E7 DELETE ?delete_data=true 透传：引擎侧同步删数据（204）；无参数兼容
/// （同样 204，数据处置语义由 state 层单测断言）。
#[tokio::test]
async fn delete_task_query_delete_data_e2e() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let ids = add_n_tasks(&client, &base, 2, &patterned(8 * 1024)).await;

    let resp = client
        .delete(format!("{base}/tasks/{}?delete_data=true", ids[0]))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    let resp = client
        .delete(format!("{base}/tasks/{}", ids[1]))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT, "无参数兼容");
}

/// E7 任务名透出：E6 显式名 → 列表条目与快照都带 name 字段。
#[tokio::test]
async fn task_name_exposed_in_list_and_snapshot_e2e() {
    let body = patterned(8 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let dest = std::env::temp_dir().join(format!("e7-name-{}", std::process::id()));
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": srv.url(),
            "dest": dest.to_str().unwrap(),
            "name": "named-by-api.bin",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 快照 name
    let snap: serde_json::Value = client
        .get(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        snap["name"], "named-by-api.bin",
        "快照必须透出任务名: {snap}"
    );

    // 列表 name
    let list: serde_json::Value = client
        .get(format!("{base}/tasks"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = list
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["task_id"] == tid.as_str())
        .unwrap();
    assert_eq!(row["name"], "named-by-api.bin", "列表必须透出任务名: {row}");
}

/// E8 任务级代理热改 API：合法 URL 200 + 快照返回；空串/端口越界 400（纯本地
/// 校验不发起连接）；缺省 body = 清除语义 200；不存在任务 404。
#[tokio::test]
async fn set_task_proxy_api_e2e() {
    let body = patterned(8 * 1024);
    let srv = TestServer::start(body).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let dest = std::env::temp_dir().join(format!("e8-proxy-{}", std::process::id()));
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": srv.url(), "dest": dest.to_str().unwrap() }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 设置：合法 URL（不可达没关系——校验是纯本地构建试水）→ 200 + 快照
    let resp = client
        .post(format!("{base}/tasks/{tid}/proxy"))
        .json(&serde_json::json!({ "proxy": "socks5://127.0.0.1:1080" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "合法代理 URL 应 200"
    );
    let snap: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(snap["task_id"], tid.as_str(), "成功响应必须是任务快照");

    // 非法：空串（清除语义由 null 承担）与端口越界 → 400
    for bad in ["", "http://127.0.0.1:70000"] {
        let resp = client
            .post(format!("{base}/tasks/{tid}/proxy"))
            .json(&serde_json::json!({ "proxy": bad }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST, "{bad:?}");
        let text = resp.text().await.unwrap();
        assert!(!text.is_empty(), "{bad:?} 的 400 必须带错误说明");
    }

    // 清除：缺省 body / null → 200
    let resp = client
        .post(format!("{base}/tasks/{tid}/proxy"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "缺省 = 清除语义");
    let resp = client
        .post(format!("{base}/tasks/{tid}/proxy"))
        .json(&serde_json::json!({ "proxy": null }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "null = 清除语义");

    // 不存在的任务 → 404
    let resp = client
        .post(format!("{base}/tasks/t404/proxy"))
        .json(&serde_json::json!({ "proxy": "http://127.0.0.1:1080" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}
