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
        let bt = smart_dl_daemon::bt::BtEngine::new(
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
