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
    let state = DaemonState::new(Arc::new(engine), vec![])
        .with_dest_root(std::env::temp_dir());
    #[cfg(feature = "bt")]
    {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bt = smart_dl_daemon::bt::BtEngine::new(
            tmp.path(),
            None,
            0,
            0,
            false,
            false,
            false,
        )
        .expect("bt engine");
        state = state.with_bt(Arc::new(bt));
    }
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
    let b = resp.json::<serde_json::Value>().await.unwrap_or(serde_json::Value::Null);
    assert_eq!(s, reqwest::StatusCode::CREATED, "magnet 应创建 BT 任务: {b}");
    assert!(b["task_id"].as_str().unwrap().starts_with('t'), "{b}");
    // BT 任务必须落到全局 save_path（v1 约束），再删掉避免污染后续测试
    let tid = b["task_id"].as_str().unwrap().to_string();
    let _ = client.post(format!("{base}/tasks/{tid}/remove")).send().await;

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
    let state = DaemonState::new(Arc::new(engine), vec![])
        .with_http_token(Some("test-token-123".into()));
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
        assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED, "GET {path} 应 401");
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
    let secured = DaemonState::new(Arc::new(engine2), vec![])
        .with_http_token(Some("t-abc".into()));
    assert!(!secured.verify_http_token(None));
    assert!(!secured.verify_http_token(Some("Bearer wrong")));
    assert!(!secured.verify_http_token(Some("t-abc"))); // 必须带 Bearer 前缀
    assert!(secured.verify_http_token(Some("Bearer t-abc")));
}
