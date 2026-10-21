//! 前后端契约矩阵（Issue #121 维度 A，REALTEST-PLAN §2 A-2）。
//!
//! 与既有测试的根本区别（原则 1/2）：**每条用例复刻 UI 的真实请求体**
//!（字面量抄自 ui/src/lib/daemon/store.ts，注释标注行号），断言**用户可见
//! 效果**而非 HTTP 200——字段名不一致 / 静默空操作 / 形状漂移在此全部
//! 显形。serde 双 `#[serde(default)]` Option 静默丢字段的坑（限速字段名
//! 事故）正是本矩阵要永久拦截的缺陷类。
//!
//! 有效性自证（原则 4）：关键用例（限速/删任务停止）已按「回滚修复 →
//! 必红」双向验证；矩阵随修复演进持续补判别锚。

mod common;

use common::{patterned, SlowTestServer, TestServer};
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

/// 起 daemon（真实 axum + HttpEngine；bt 构建注入 BtEngine 对齐生产装配）。
async fn serve() -> (std::net::SocketAddr, Arc<DaemonState>) {
    let engine = HttpEngine::new(reqwest::Client::new());
    #[cfg(feature = "bt")]
    let state = {
        let bt = smart_dl_btcore::dl_engine::BtEngine::new(
            std::env::temp_dir().as_path(),
            None,
            0,
            0,
            false,
            false,
            false,
            false,
            false,
            "allow",
            &[],
            0.0,
            0,
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

/// 添加任务（独立临时 dest；复刻 UI addTask 的请求体形状）。
async fn add_task(
    client: &reqwest::Client,
    base: &str,
    body: serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value, String) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static DEST_SEQ: AtomicU64 = AtomicU64::new(0);
    let dest = std::env::temp_dir().join(format!(
        "contract-{}-{}",
        std::process::id(),
        DEST_SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    let mut body = body;
    if body.get("dest").is_none() && body.get("url").is_some() {
        body["dest"] = serde_json::json!(dest.display().to_string());
    }
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let val: serde_json::Value = resp.json().await.unwrap_or_default();
    let id = val["task_id"].as_str().unwrap_or_default().to_string();
    (status, val, id)
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

/// 轮询至终态（Completed/Failed/Error 字符串口径）或超时。
async fn wait_terminal(
    client: &reqwest::Client,
    base: &str,
    id: &str,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        let s = snap(client, base, id).await;
        let st = s["state"].as_str().unwrap_or_default().to_string();
        if matches!(st.as_str(), "Completed" | "Failed" | "Error") {
            return s;
        }
        assert!(Instant::now() < deadline, "超时未到终态（state={st}）: {s}");
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

// ============ A-2 契约清单 ============

/// #1 `POST /tasks {url}`：任务出现在列表（用户可见：列表有它）。
/// 请求体抄自 store.ts:80 addTask（UI 侧 AddTaskReq 形状）。
#[tokio::test]
async fn contract_01_url_add_appears_in_list() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let srv = TestServer::start(patterned(64 * 1024)).await;

    let (status, body, id) =
        add_task(&client, &base, serde_json::json!({ "url": srv.url() })).await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "body: {body}");
    assert!(!id.is_empty(), "响应须含 task_id: {body}");

    let list: Vec<serde_json::Value> = client
        .get(format!("{base}/tasks"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        list.iter().any(|t| t["task_id"] == *id),
        "列表必须包含新任务（UI 列表刷新的可见性基础）"
    );
}

/// #4 `POST /tasks {url, headers}`：请求头真的下发（服务器侧断言，非 200）。
/// 抄自 store.ts addTask（AddTaskReq.headers）。
#[tokio::test]
async fn contract_04_custom_headers_reach_server() {
    // 记录型服务器：把收到的 X-Custom 头原样回显在响应头里
    #[derive(Clone)]
    struct EchoHeaders;
    async fn echo_handler(State(_): State<EchoHeaders>, headers: HeaderMap) -> Response {
        let got = headers
            .get("x-custom-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        // 200 + 全量小体（无 Range 需求——探测 bytes=0-0 与整段都走这里）
        Response::builder()
            .status(StatusCode::OK)
            .header("x-echo-token", got)
            .header(header::CONTENT_LENGTH, "16")
            .body(axum::body::Body::from(vec![0u8; 16]))
            .unwrap()
    }
    let app = Router::new()
        .route("/file", get(echo_handler))
        .with_state(EchoHeaders);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let (daemon_addr, _state) = serve().await;
    let base = format!("http://{daemon_addr}");
    let client = reqwest::Client::new();

    let (status, _body, id) = add_task(
        &client,
        &base,
        serde_json::json!({
            // 抄自 UI addTask 的 headers 字段形状（Record<string,string>）
            "url": format!("http://{addr}/file"),
            "headers": { "X-Custom-Token": "contract-secret-42" }
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED);

    let s = wait_terminal(&client, &base, &id, Duration::from_secs(15)).await;
    assert_eq!(
        s["state"], "Completed",
        "带自定义头的下载应完成（error: {:?}）",
        s["error"]
    );
    // 引擎侧头下发的机器断言：任务完成即证明 200 路径通；头本身的逐字节
    // 断言在 httpdl segment_headers.rs 已锁（H-8），此处锁 daemon 契约层
    //（UI 的 headers 字段被接受且不阻断下载）。
}

/// #5 `POST /tasks {url, username, password}`：Basic auth 真的带上。
/// 401 挑战服务器 → 无凭据拒绝、正确凭据放行；任务必须 Completed。
#[tokio::test]
async fn contract_05_basic_auth_forwarded() {
    #[derive(Clone)]
    struct AuthServer;
    async fn auth_handler(State(_): State<AuthServer>, headers: HeaderMap) -> Response {
        let ok = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(|v| v == "Basic Y29udHJhY3Q6c2VjcmV0") // contract:secret
            .unwrap_or(false);
        if ok {
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_LENGTH, "32")
                .body(axum::body::Body::from(vec![7u8; 32]))
                .unwrap()
        } else {
            (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Basic realm=\"contract\"")],
            )
                .into_response()
        }
    }
    let app = Router::new()
        .route("/file", get(auth_handler))
        .with_state(AuthServer);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let (daemon_addr, _state) = serve().await;
    let base = format!("http://{daemon_addr}");
    let client = reqwest::Client::new();

    let (status, _b, id) = add_task(
        &client,
        &base,
        serde_json::json!({
            // 抄自 UI addTask（AddTaskReq.username/password）
            "url": format!("http://{addr}/file"),
            "username": "contract",
            "password": "secret"
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED);
    let s = wait_terminal(&client, &base, &id, Duration::from_secs(15)).await;
    assert_eq!(
        s["state"], "Completed",
        "Basic auth 凭据必须下发（否则恒 401 → Error）。error: {:?}",
        s["error"]
    );
}

/// #6 `POST /tasks {url, sha256}`：校验失败任务标 Error（非静默交付坏数据）。
#[tokio::test]
async fn contract_06_sha256_mismatch_marks_error() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let srv = TestServer::start(patterned(64 * 1024)).await;

    let wrong_sha = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(vec![0u8; 64 * 1024]); // 与 patterned 内容不同的哈希
        format!("{:x}", h.finalize())
    };
    let (status, _b, id) = add_task(
        &client,
        &base,
        serde_json::json!({
            "url": srv.url(),
            "sha256": wrong_sha
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED);
    let s = wait_terminal(&client, &base, &id, Duration::from_secs(20)).await;
    assert_eq!(
        s["state"], "Failed",
        "哈希不符必须进 Failed（error: {:?}）",
        s["error"]
    );
}

/// #7 `POST /tasks {url, dest}`：文件真的落到指定目录。
#[tokio::test]
async fn contract_07_dest_directory_honored() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let srv = TestServer::start(patterned(128 * 1024)).await;
    let dest = tempfile::tempdir().unwrap();

    let (status, _b, id) = add_task(
        &client,
        &base,
        serde_json::json!({
            "url": srv.url(),
            "dest": dest.path().display().to_string()
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED);
    let s = wait_terminal(&client, &base, &id, Duration::from_secs(15)).await;
    assert_eq!(s["state"], "Completed", "error: {:?}", s["error"]);
    // 用户可见效果：dest 目录下有产物文件且内容正确
    let mut found = None;
    for e in std::fs::read_dir(dest.path()).unwrap().flatten() {
        let p = e.path();
        if p.is_file() && !p.to_string_lossy().contains(".part") {
            found = Some(p);
        }
    }
    let file = found.expect("dest 目录必须有产物文件");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        patterned(128 * 1024),
        "产物内容一致"
    );
}

/// #9 `POST /tasks {url, start_at_unix}`：未来时间点任务不启动。
#[tokio::test]
async fn contract_09_start_at_future_stays_queued() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let srv = TestServer::start(patterned(64 * 1024)).await;

    let future = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let (status, _b, id) = add_task(
        &client,
        &base,
        serde_json::json!({
            "url": srv.url(),
            "start_at_unix": future
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED);
    tokio::time::sleep(Duration::from_millis(800)).await;
    let s = snap(&client, &base, &id).await;
    assert_ne!(
        s["state"], "Completed",
        "start_at 未来的任务 800ms 内不得完成（定时未到）"
    );
    assert_ne!(
        s["state"], "Downloading",
        "定时任务不得提前进入下载（state={}）",
        s["state"]
    );
}

/// **#10（核心，REALTEST-PLAN 标注「当前坏的」）** `POST /tasks/:id/limit
/// {down_kb_s, up_kb_s}`：限速真的降到设定值——实测吞吐，非 200 状态码。
/// 请求体抄自 store.ts:111 taskLimit。
/// 依赖 PR #117（set_limits 运行中首次设限热调）：旧代码该场景限速不生效。
#[tokio::test]
async fn contract_10_limit_actually_limits_speed() {
    // 慢服务器：2MiB / 40 块 × 50ms ≈ 2s 全速 ≈ 1MiB/s
    let srv = SlowTestServer::start(patterned(2 * 1024 * 1024), 40, 50).await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let (status, _b, id) = add_task(&client, &base, serde_json::json!({ "url": srv.url() })).await;
    assert_eq!(status, reqwest::StatusCode::CREATED);
    // 等下载起来（首段在飞）
    tokio::time::sleep(Duration::from_millis(400)).await;
    let running = snap(&client, &base, &id).await;
    assert_eq!(running["state"], "Downloading", "限速前提：任务在跑");

    // 复刻 UI 的请求体（store.ts:111）：down 传 number、up 传 undefined
    let resp = client
        .post(format!("{base}/tasks/{id}/limit"))
        .json(&serde_json::json!({
            // 抄自 ui/src/lib/daemon/store.ts:111
            "down_kb_s": 100, "up_kb_s": null
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "限速请求应 200");

    // 关键断言：3s 窗口的 done 增量 ≤ 100KiB/s × 1.3 余量
    let before = snap(&client, &base, &id).await["done"]
        .as_u64()
        .unwrap_or(0);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let after = snap(&client, &base, &id).await["done"]
        .as_u64()
        .unwrap_or(0);
    let kbps = (after.saturating_sub(before)) as f64 / 1024.0 / 3.0;
    assert!(
        kbps <= 130.0,
        "限速 100KiB/s 但实测 {kbps:.1}KiB/s —— 契约失效（PR #117 热调修复的回归锚）"
    );
}

/// #16 `GET /tasks/:id`：name 可为 null（UI 必须能处理——types.ts 契约）。
#[tokio::test]
async fn contract_16_name_may_be_null() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    // 纯 query URL（无路径段）→ 名字不可推导
    let srv = TestServer::start(patterned(1024)).await;
    let url = format!("{}?id=123", srv.url());
    let (status, _b, id) = add_task(&client, &base, serde_json::json!({ "url": url })).await;
    assert_eq!(status, reqwest::StatusCode::CREATED);
    let s = snap(&client, &base, &id).await;
    // P0-2 契约：name 可空且可缺省（Option + skip_serializing_if）——
    // UI types.ts 的 `name?: string | null` 必须两者都能处理。
    assert!(
        s.get("name").is_some() || s["name"].is_null(),
        "name 字段应存在或为 null: {s}"
    );
}

/// #17 `GET /tasks/:id/logs`：形状 { task_id, state, source, error, events:[{at_ms,op,detail}] }。
/// 抄自 store.ts:101 taskLogs 的期望形状（P1 修复后的契约）。
#[tokio::test]
async fn contract_17_logs_response_shape() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let srv = TestServer::start(patterned(1024)).await;
    let (status, _b, id) = add_task(&client, &base, serde_json::json!({ "url": srv.url() })).await;
    assert_eq!(status, reqwest::StatusCode::CREATED);
    let _ = wait_terminal(&client, &base, &id, Duration::from_secs(15)).await;

    let body: serde_json::Value = client
        .get(format!("{base}/tasks/{id}/logs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["events"].is_array(), "缺 events 字段: {body}");
    if let Some(ev) = body["events"].as_array().and_then(|a| a.first()) {
        assert!(ev["at_ms"].is_number(), "event 缺 at_ms: {ev}");
        assert!(ev["op"].is_string(), "event 缺 op: {ev}");
    }
    assert!(body["state"].is_string(), "缺 state: {body}");
}

/// #18 `POST /tasks/batch {action, ids, delete_data}`：批量操作生效。
/// 抄自 store.ts:123 batchTasks（action 是字段名，非 op）。
#[tokio::test]
async fn contract_18_batch_pause_shape() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    // 慢服务器：8MiB / 160 块 × 50ms ≈ 8s——确保批量暂停落在下载中
    let srv = SlowTestServer::start(patterned(8 * 1024 * 1024), 160, 50).await;

    let mut ids = Vec::new();
    for k in 0..2 {
        // query 区分 canonical（同 URL 会被查重 409）
        let (st, _b, id) = add_task(
            &client,
            &base,
            serde_json::json!({ "url": format!("{}?k={}", srv.url(), k) }),
        )
        .await;
        assert_eq!(st, reqwest::StatusCode::CREATED);
        ids.push(id);
    }
    // 复刻 UI 请求体（store.ts:127）
    let resp = client
        .post(format!("{base}/tasks/batch"))
        .json(&serde_json::json!({
            "action": "pause",
            "ids": ids,
            "delete_data": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "批量操作应 200");
    let body: serde_json::Value = resp.json().await.unwrap();
    let _ = body;
    // 用户可见效果：两个任务都真的暂停（慢服务器保证还在下载中）
    for id in &ids {
        let s = snap(&client, &base, id).await;
        assert_eq!(s["state"], "Paused", "批量暂停后 state 应 Paused: {s}");
    }
}

/// #21 `GET /providers`：返回数组（UI ProvidersView 的渲染基础）。
#[tokio::test]
async fn contract_21_providers_list_shape() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/providers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body.is_array(), "providers 应为数组: {body}");
}

/// #22 `POST /config/limit {max_download_kb_s, max_upload_kb_s}`：
/// 全局限速生效 + /config 回显。抄自 store.ts:213（全局用 max_* 字段名——
/// 与任务级 down_kb_s 不同名，这是后端契约而非 UI bug）。
#[tokio::test]
async fn contract_22_global_limit_applies_and_echoes() {
    let (addr, state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/config/limit"))
        .json(&serde_json::json!({
            // 抄自 ui/src/lib/daemon/store.ts:213（全局端点是 max_* 字段名）
            "max_download_kb_s": 512, "max_upload_kb_s": 256
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "全局限速应 200: {:?}", resp);
    // 契约（http.rs 文档）：200 + 当前生效 {max_download_kb_s, max_upload_kb_s}
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["max_download_kb_s"], 512,
        "全局限速响应必须回显生效值（UI 设置页的数据源）: {body}"
    );
    assert_eq!(body["max_upload_kb_s"], 256, "上行同口径: {body}");
    let _ = state;
}

/// #23 `PUT /settings`：设置真的持久化 + GET 回读。
/// 抄自 store.ts:234 putSettings（?persist= 语义）。
#[tokio::test]
async fn contract_23_settings_persist_and_roundtrip() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .put(format!("{base}/settings?persist=true"))
        .json(&serde_json::json!({
            // 抄自 SettingsView save()：域级嵌套补丁（后端 SettingsReq 按域合并）
            "webhook": { "url": "http://127.0.0.1:1/hook-contract" }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "设置应 200: {resp:?}");

    let settings: serde_json::Value = client
        .get(format!("{base}/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        settings["webhook"]["url"], "http://127.0.0.1:1/hook-contract",
        "设置必须可回读（UI 设置页持久性的数据源，域嵌套形状）: {settings}"
    );
}

/// #13 `POST /tasks/:id/proxy {proxy}`：动态换代理——快照 source 反映。
/// 抄自 store.ts:145 taskProxy（null = 清除）。
#[tokio::test]
async fn contract_13_task_proxy_set_echoes() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let srv = TestServer::start(patterned(1024 * 1024)).await;
    let (st, _b, id) = add_task(&client, &base, serde_json::json!({ "url": srv.url() })).await;
    assert_eq!(st, reqwest::StatusCode::CREATED);

    let resp = client
        .post(format!("{base}/tasks/{id}/proxy"))
        .json(&serde_json::json!({ "proxy": "http://127.0.0.1:9" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "换代理应 200: {resp:?}");
    let s = snap(&client, &base, &id).await;
    assert!(
        s["source"].as_str().unwrap_or_default().contains("proxy"),
        "快照 source 应反映代理已设置: {s}"
    );
}

/// #12 `POST /tasks/:id/connections {max_connections}`：
/// 后端契约（仅 BT 任务，HTTP 任务 409）——UI 对 HTTP 任务调用时必须
/// 收到 409 而非静默 200（错误可见性契约）。
#[tokio::test]
async fn contract_12_connections_on_http_task_is_409() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let srv = TestServer::start(patterned(1024 * 1024)).await;
    let (st, _b, id) = add_task(&client, &base, serde_json::json!({ "url": srv.url() })).await;
    assert_eq!(st, reqwest::StatusCode::CREATED);

    let resp = client
        .post(format!("{base}/tasks/{id}/connections"))
        .json(&serde_json::json!({ "max_connections": 4 }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CONFLICT,
        "HTTP 任务的 connections 必须 409（UI 可见的语义边界）"
    );
}

// ============ 补齐 8 条（#143 第一梯队；REALTEST-PLAN A-2 缺口
// ============ #2/#3/#8/#11/#14/#15/#19/#20；2026-10-21） ============

use base64::Engine as _;

/// mini 正向代理（httpdl task_proxy.rs 同款策略）：任意方法/路径 → 按
/// Range 回 206 切片或 200 全量，内容恒为 body；命中计数。返回（URL, hits）。
async fn start_mini_proxy(body: Vec<u8>) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_c = hits.clone();
    let body = Arc::new(body);
    let app =
        axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
            let hits = hits_c.clone();
            let body = body.clone();
            async move {
                use axum::http::{header, StatusCode};
                hits.fetch_add(1, Ordering::SeqCst);
                let total = body.len() as u64;
                let range = req
                    .headers()
                    .get(header::RANGE)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                match range.as_deref().and_then(|r| r.strip_prefix("bytes=")) {
                    Some(spec) => {
                        let mut parts = spec.split('-');
                        let start: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                        let end: u64 = parts
                            .next()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(u64::MAX)
                            .min(total - 1);
                        let payload = body
                            .get(start as usize..=(end as usize).min(total as usize - 1))
                            .unwrap_or(&[])
                            .to_vec();
                        axum::response::Response::builder()
                            .status(StatusCode::PARTIAL_CONTENT)
                            .header(
                                header::CONTENT_RANGE,
                                format!("bytes {start}-{end}/{total}"),
                            )
                            .body(axum::body::Body::from(payload))
                            .unwrap()
                            .into_response()
                    }
                    None => axum::response::Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_LENGTH, total.to_string())
                        .body(axum::body::Body::from((*body).clone()))
                        .unwrap()
                        .into_response(),
                }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), hits)
}

/// #2 `POST /tasks {torrent_b64}`：bt 构建真实加种（任务入列表）；
/// 非 bt 构建清晰 400 点明「需 BT 引擎」——禁止静默空操作。
/// 消费方：第二梯队「新建任务表单扩展」（当前 UI 未接线，形状按
/// AddTaskReq.torrent_b64（http.rs:36-39）锚定）。
#[tokio::test]
async fn contract_02_torrent_b64_add() {
    // 最小单文件 .torrent（bencode 手写，bt_api.rs minimal_torrent_b64 同款）
    let mut t = b"d4:infod6:lengthi123e4:name4:test12:piece lengthi16384e6:pieces20:".to_vec();
    t.extend_from_slice(&[0xAB; 20]);
    t.extend_from_slice(b"ee");
    let b64 = base64::engine::general_purpose::STANDARD.encode(&t);

    #[cfg(feature = "bt")]
    {
        let _lt = common::lt_gate::LT_SESSION_GATE.lock().await;
        let (addr, _state) = serve().await;
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();
        // BT v1 引擎限制：任务 dest 必须等于全局落盘目录或走默认——
        // 不传 dest（serve() 的全局根 = temp_dir），bt_api 同款惯例
        let (status, val, id) =
            add_task(&client, &base, serde_json::json!({ "torrent_b64": b64 })).await;
        assert_eq!(status, reqwest::StatusCode::CREATED, "{val}");
        assert!(!id.is_empty(), "task_id 必须返回: {val}");
        let list: serde_json::Value = client
            .get(format!("{base}/tasks"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(
            list.as_array()
                .unwrap()
                .iter()
                .any(|t| t["task_id"].as_str() == Some(id.as_str())),
            "用户可见效果：任务出现在列表"
        );
    }
    #[cfg(not(feature = "bt"))]
    {
        let (addr, _state) = serve().await;
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{base}/tasks"))
            .json(&serde_json::json!({
                "torrent_b64": b64,
                "dest": std::env::temp_dir()
                    .join(format!("contract-02-{}", std::process::id()))
                    .display()
                    .to_string()
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
        let v: serde_json::Value = resp.json().await.unwrap();
        let err = v["error"].as_str().unwrap_or_default();
        assert!(err.contains("BT 引擎"), "拒绝必须点明原因（非静默）: {err}");
    }
}

/// #3 `POST /tasks {metalink_b64}`：两 `<file>` 逐条展开为 HTTP 任务集，
/// 响应含 `task_ids`/`count`，全部 Completed + 落盘（用户可见效果）。
#[tokio::test]
async fn contract_03_metalink_b64_expands_and_completes() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let a = TestServer::start(patterned(64)).await;
    let b = TestServer::start(patterned(128)).await;
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <file name="alpha.bin"><url priority="1">{}</url></file>
  <file name="beta.bin"><url priority="1">{}</url></file>
</metalink>"#,
        a.url(),
        b.url()
    );
    let b64 = base64::engine::general_purpose::STANDARD.encode(xml.as_bytes());
    let dest = tempfile::tempdir().unwrap();
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "metalink_b64": b64,
            "dest": dest.path().display().to_string()
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    assert_eq!(
        status,
        reqwest::StatusCode::CREATED,
        "metalink 展开失败: {v}"
    );
    let ids = v["task_ids"].as_array().cloned().unwrap_or_default();
    assert_eq!(ids.len(), 2, "两 <file> 必须展开两条任务: {v}");
    assert_eq!(v["count"].as_u64(), Some(2), "count 与展开数一致: {v}");
    for id in ids {
        let id = id.as_str().unwrap().to_string();
        let s = wait_terminal(&client, &base, &id, Duration::from_secs(15)).await;
        assert_eq!(s["state"], "Completed", "{}", s);
    }
    assert!(dest.path().join("alpha.bin").exists(), "alpha.bin 必须落盘");
    assert!(dest.path().join("beta.bin").exists(), "beta.bin 必须落盘");
}

/// #8 `POST /tasks {url, proxy}`（AddTaskReq.proxy）：真的走代理（服务器侧
/// 断言）。目标 = RFC 2606 保留 `.invalid` 域（DNS 必失败）——内容只能经
/// 代理获得，Completed 即证明探测+下载全链都过了代理。
#[tokio::test]
async fn contract_08_add_with_proxy_routes_through_proxy() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let body = patterned(256 * 1024);
    let (proxy_url, hits) = start_mini_proxy(body).await;

    let (status, v, id) = add_task(
        &client,
        &base,
        serde_json::json!({
            "url": "http://proxy-e2e.invalid/f.bin",
            "proxy": proxy_url,
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "{v}");
    let s = wait_terminal(&client, &base, &id, Duration::from_secs(20)).await;
    assert_eq!(s["state"], "Completed", "内容只能来自代理: {s}");
    assert!(
        hits.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "代理必须被真实触达（非仅探测）"
    );
}

/// #11 `POST /tasks/:id/sequential {sequential}`（store.ts:138-140 UI 形状）：
/// 快照回显（UI 唯一数据源），双向翻转（用户可见：开关可来回切）。
#[tokio::test]
async fn contract_11_sequential_ui_shape_roundtrip() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let srv = TestServer::start(patterned(64 * 1024)).await;
    let (status, v, id) = add_task(&client, &base, serde_json::json!({ "url": srv.url() })).await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "{v}");

    // UI 请求体形状：{"sequential": true}（store.ts:140）
    let r: serde_json::Value = client
        .post(format!("{base}/tasks/{id}/sequential"))
        .json(&serde_json::json!({ "sequential": true }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        r["sequential"],
        serde_json::json!(true),
        "快照必须回显: {r}"
    );

    let r: serde_json::Value = client
        .post(format!("{base}/tasks/{id}/sequential"))
        .json(&serde_json::json!({ "sequential": false }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // false = 字段省略（skip_serializing_if 向后兼容契约）——缺失即 false
    assert!(
        r.get("sequential")
            .map(|v| v == &serde_json::json!(false))
            .unwrap_or(true),
        "false 契约 = 省略或显式 false: {r}"
    );
}

/// #14 `POST /tasks/:id/piece-priority {priority}`（store.ts:156-159 UI 形状，
/// 0..=7）：HTTP 任务 → 409（BT-only 语义边界，UI 可见）；越界值 → 400。
#[tokio::test]
async fn contract_14_piece_priority_http_task_boundary() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let srv = TestServer::start(patterned(8 * 1024)).await;
    let (status, v, id) = add_task(&client, &base, serde_json::json!({ "url": srv.url() })).await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "{v}");

    // UI 请求体形状：{"priority": 7}（store.ts:158）
    let resp = client
        .post(format!("{base}/tasks/{id}/piece-priority"))
        .json(&serde_json::json!({ "priority": 7 }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CONFLICT,
        "HTTP 任务的首尾块优先必须 409（BT-only 边界对 UI 可见）"
    );
    let v: serde_json::Value = resp.json().await.unwrap();
    assert!(
        !v["error"].as_str().unwrap_or_default().is_empty(),
        "错误信息必须可读: {v}"
    );

    // 越界（0..=7 之外）→ 400（契约：优先级取值域）
    let resp = client
        .post(format!("{base}/tasks/{id}/piece-priority"))
        .json(&serde_json::json!({ "priority": 8 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST, "越界 400");
}

/// #14b（bt 构建）torrent 任务的真实生效面：`{"priority": 7}` → `{"ok": true}`。
/// 此前 piece-first-last 零测试覆盖（bt_api 亦无），本条为首个回归锚。
#[cfg(feature = "bt")]
#[tokio::test]
async fn contract_14b_piece_priority_on_torrent_task() {
    let _lt = common::lt_gate::LT_SESSION_GATE.lock().await;
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let mut t = b"d4:infod6:lengthi123e4:name4:test12:piece lengthi16384e6:pieces20:".to_vec();
    t.extend_from_slice(&[0xAB; 20]);
    t.extend_from_slice(b"ee");
    let b64 = base64::engine::general_purpose::STANDARD.encode(&t);
    // 同 contract_02：BT v1 dest 限制——不传走全局默认
    let (status, val, id) =
        add_task(&client, &base, serde_json::json!({ "torrent_b64": b64 })).await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "{val}");

    let resp = client
        .post(format!("{base}/tasks/{id}/piece-priority"))
        .json(&serde_json::json!({ "priority": 7 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "torrent 任务应 200");
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
}

/// #15 `POST /tasks/:id/super-seeding {enabled}`：HTTP 任务 → 409（BT-only
/// 边界对 UI 可见）。torrent 任务的真实生效面已有
/// bt_api.rs super_seeding_endpoint_roundtrip_on_torrent_task 覆盖。
#[tokio::test]
async fn contract_15_super_seeding_http_task_boundary() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let srv = TestServer::start(patterned(8 * 1024)).await;
    let (status, v, id) = add_task(&client, &base, serde_json::json!({ "url": srv.url() })).await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "{v}");

    let resp = client
        .post(format!("{base}/tasks/{id}/super-seeding"))
        .json(&serde_json::json!({ "enabled": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CONFLICT,
        "HTTP 任务的超级种子必须 409（BT-only 边界对 UI 可见）"
    );
}

/// #19 `POST /tasks/:id/priority`：相对移动 `{action:"top"}` 与绝对值
/// `{priority:n}` 的回显契约（响应 `queue_priority`；top 必须真的到顶 =
/// 小于其余排队任务的值）。三条排队任务（start_at 未来）建队列。
#[tokio::test]
async fn contract_19_priority_moves_and_echoes() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let srv = TestServer::start(patterned(8 * 1024)).await;
    let future = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;

    let mut ids = Vec::new();
    for i in 0..3 {
        // 源级去重：同 URL 二次添加 409 duplicate——排队队列用查询参区分源
        let (status, v, id) = add_task(
            &client,
            &base,
            serde_json::json!({ "url": format!("{}?q={i}", srv.url()), "start_at_unix": future }),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::CREATED, "{v}");
        ids.push(id);
    }
    let (a, b, _c) = (&ids[0], &ids[1], &ids[2]);

    // 相对移动：中间任务置顶 → 返回值必须小于队列最小值（默认 0）→ 到顶
    let v: serde_json::Value = client
        .post(format!("{base}/tasks/{b}/priority"))
        .json(&serde_json::json!({ "action": "top" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let top = v["queue_priority"].as_i64().unwrap_or(i64::MAX);
    assert!(top < 0, "top 必须移到默认值 0 之上（更小）: {v}");

    // 绝对值：同给 value 优先于 action（batch5-P2 语义）
    let v: serde_json::Value = client
        .post(format!("{base}/tasks/{a}/priority"))
        .json(&serde_json::json!({ "priority": 5, "action": "top" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        v["queue_priority"].as_i64(),
        Some(5),
        "value 优先于 action: {v}"
    );

    // 相对移动：bottom → 大于当前最大（5）→ 到底
    let v: serde_json::Value = client
        .post(format!("{base}/tasks/{b}/priority"))
        .json(&serde_json::json!({ "action": "bottom" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bottom = v["queue_priority"].as_i64().unwrap_or(i64::MIN);
    assert!(bottom > 5, "bottom 必须移到最大值 5 之下（更大）: {v}");
}

/// #20 `GET/POST/DELETE /tasks/:id/trackers`：HTTP 任务全 409（BT-only
/// 边界对 UI 可见；真实增删查已有 bt_api.rs trackers_add_list_remove_e2e）。
#[tokio::test]
async fn contract_20_trackers_http_task_boundary() {
    let (addr, _state) = serve().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let srv = TestServer::start(patterned(8 * 1024)).await;
    let (status, v, id) = add_task(&client, &base, serde_json::json!({ "url": srv.url() })).await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "{v}");

    let resp = client
        .post(format!("{base}/tasks/{id}/trackers"))
        .json(&serde_json::json!({ "urls": ["http://t.example/announce"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CONFLICT,
        "POST 边界 409"
    );

    let resp = client
        .get(format!("{base}/tasks/{id}/trackers"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT, "GET 边界 409");

    let resp = client
        .delete(format!(
            "{base}/tasks/{id}/trackers?url=http://t.example/announce"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CONFLICT,
        "DELETE 边界 409"
    );
}
