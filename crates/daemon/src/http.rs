//! HTTP API（axum，M6）：任务 CRUD + 快照 + Provider 运行态 + WS 升级端点（M7）。

use crate::state::{DaemonError, DaemonState};
#[cfg(feature = "nas")]
use crate::nas;
use crate::ws::Throttler;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
#[cfg(feature = "nas")]
use axum::response::Response;
use base64::Engine as _;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
pub struct AddTaskReq {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub dest: Option<String>,
    /// .torrent 文件内容（标准 base64）。与 `url` 二选一，优先 torrent。
    #[serde(default)]
    pub torrent_b64: Option<String>,
}

#[cfg(feature = "xunlei-import")]
#[derive(Deserialize)]
pub struct AddXunleiImportReq {
    /// .torrent 文件内容（标准 base64）。
    pub torrent_b64: String,
    /// .xlbt.cfg 文件内容（标准 base64）。
    pub cfg_b64: String,
    /// .bt.xltd 文件内容数组（标准 base64）；单文件为 1 项，多文件按 torrent files 顺序。
    pub xltd_b64s: Vec<String>,
    #[serde(default)]
    pub dest: Option<String>,
}

/// F5 P2SP：给 BT 任务注入云盘直链 web seed（`POST /tasks/:id/webseeds`）。
/// URL 必须原样（带 `at=` 防篡改签名，禁改 query）。
#[derive(Deserialize)]
pub struct WebseedReq {
    pub urls: Vec<String>,
}

async fn task_webseeds(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(req): Json<WebseedReq>,
) -> impl IntoResponse {
    match state.add_webseeds(&id, &req.urls).await {
        Ok(n) => Json(serde_json::json!({ "added": n })).into_response(),
        Err(e) => {
            let body = Json(serde_json::json!({ "error": e.to_string() }));
            let status = match e {
                DaemonError::NotFound(_) => StatusCode::NOT_FOUND,
                DaemonError::UnsupportedOp(_) => StatusCode::CONFLICT,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, body).into_response()
        }
    }
}

/// `GET /tasks/:id/logs`：任务操作日志（快照 + 事件序列）。
async fn task_logs(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.task_logs(&id) {
        Ok(v) => Json(v).into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "not found" })),
        )
            .into_response(),
    }
}

/// `POST /tasks/:id/fallback`：Q-B9 手动兜底（M6 已接线）——BT 任务暂停且进度 <50%
/// → 云 Provider 直链 → HTTP 引擎传输 → 任务置 Completed。
async fn task_fallback(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.fallback(&id).await {
        Ok(outcome) => Json(serde_json::json!({
            "status": "completed",
            "provider": outcome.provider,
            "provider_task": outcome.provider_task,
            "transferred": outcome.transferred,
        }))
        .into_response(),
        Err(e) => {
            let body = Json(serde_json::json!({ "error": e.to_string() }));
            let status = match e {
                DaemonError::NotFound(_) => StatusCode::NOT_FOUND,
                DaemonError::Fallback(_) => StatusCode::CONFLICT,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, body).into_response()
        }
    }
}

/// `GET /config`：生效配置快照（serve 注入；未注入时提示）。
async fn config_endpoint(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    Json(state.config_snapshot())
}

/// 删除任务（引擎 remove + 目录记录移除；delete_data=false 保留已下载文件）。
async fn remove_task(
    Path(id): Path<String>,
    State(state): State<Arc<DaemonState>>,
) -> impl IntoResponse {
    match state.remove(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

/// WS 升级端点（D36 socket 端点，M7）：连接即推全量（重连重同步），随后
/// 轮询增量 + 1s 快照节流（Progress/Speed 合并）。单消费者语义（D22 本机单
/// 用户）；多连接时后来的连接会 drain 走队列，属可接受偏差。
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<DaemonState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_session(socket, state))
}

async fn ws_session(mut socket: WebSocket, state: Arc<DaemonState>) {
    let hub = state.hub();

    // 1) 连接即推送全量（掉队客户端重连重同步入口）
    let mut last_seq = 0u64;
    for env in hub.drain() {
        if send_text(&mut socket, &env).await.is_none() {
            return;
        }
        last_seq = env.seq;
    }

    // 2) 增量轮询 + 1s 节流
    let mut throttler = Throttler::new();
    let mut last_flush = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None => return,
                Some(Ok(Message::Ping(p))) => {
                    if socket.send(Message::Pong(p)).await.is_err() { return; }
                }
                Some(Ok(_)) => {} // 本端点以推送为主，忽略上行文本
                Some(Err(_)) => return,
            },
            _ = tick.tick() => {
                for env in hub.snapshot_upto(last_seq) {
                    last_seq = env.seq;
                    if Throttler::is_throttlable(&env.event) {
                        throttler.upsert(env); // 合并，等 1s flush
                    } else if send_text(&mut socket, &env).await.is_none() {
                        return;
                    }
                }
                if !throttler.is_empty() && last_flush.elapsed() >= Duration::from_secs(1) {
                    for env in throttler.drain_pending() {
                        if send_text(&mut socket, &env).await.is_none() {
                            return;
                        }
                    }
                    last_flush = Instant::now();
                }
            }
        }
    }
}

/// 序列化 Envelope 为 WS 文本帧；失败（连接断开）返回 None。
async fn send_text(socket: &mut WebSocket, env: &crate::events::Envelope) -> Option<()> {
    let text = serde_json::to_string(env).ok()?;
    socket.send(Message::Text(text)).await.ok()
}

async fn add_task(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<AddTaskReq>,
) -> impl IntoResponse {
    let result = if let Some(b64) = req.torrent_b64 {
        match base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()) {
            Ok(bytes) => {
                #[cfg(feature = "bt")]
                {
                    state.add_torrent_task(bytes, req.dest).await
                }
                #[cfg(not(feature = "bt"))]
                {
                    let _ = bytes;
                    Err(crate::state::DaemonError::InvalidSource(
                        ".torrent 需 BT 引擎（编译时启用 --features daemon/bt）".into(),
                    ))
                }
            }
            Err(_) => Err(crate::state::DaemonError::InvalidSource(
                "torrent_b64 不是合法 base64".into(),
            )),
        }
    } else {
        let Some(url) = req.url else {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "需要 url 或 torrent_b64" })),
            );
        };
        state.add_link_task(url, req.dest).await
    };
    match result {
        Ok(task_id) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "task_id": task_id })),
        ),
        Err(e) => match e {
            crate::state::DaemonError::Duplicate(existing) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({ "error": format!("duplicate (existing: {existing})") })),
            ),
            other => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": other.to_string() })),
            ),
        },
    }
}

#[cfg(feature = "xunlei-import")]
async fn add_xunlei_import(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<AddXunleiImportReq>,
) -> impl IntoResponse {
    let decode = |b64: &str| base64::engine::general_purpose::STANDARD.decode(b64.as_bytes());
    let torrent = match decode(&req.torrent_b64) {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "torrent_b64 不是合法 base64" })),
            )
                .into_response();
        }
    };
    let cfg = match decode(&req.cfg_b64) {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "cfg_b64 不是合法 base64" })),
            )
                .into_response();
        }
    };
    let mut xltds = Vec::with_capacity(req.xltd_b64s.len());
    for b64 in &req.xltd_b64s {
        match decode(b64) {
            Ok(b) => xltds.push(b),
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "xltd_b64s 含非法 base64" })),
                )
                    .into_response();
            }
        }
    }

    match state
        .add_xunlei_import_task(torrent, cfg, xltds, req.dest)
        .await
    {
        Ok(task_id) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "task_id": task_id })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn task_snapshot(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.task_snapshot(&id).await {
        Some(snap) => Json(snap).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "not found" })),
        )
            .into_response(),
    }
}

async fn list_tasks(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    Json(state.list())
}

async fn pause_task(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.pause(&id).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn resume_task(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.resume(&id).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn providers(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let rows: Vec<serde_json::Value> = state
        .provider_status()
        .iter()
        .map(|(name, rt)| {
            serde_json::json!({
                "provider": name,
                "enabled": rt.enabled,
                "authenticated": rt.authenticated,
                "quota_remaining": rt.quota_remaining,
                "busy": rt.busy,
                "backoff_until": rt.backoff_until,
                "last_error": rt.last_error,
            })
        })
        .collect();
    Json(rows)
}

macro_rules! router_base {
    ($app:expr) => {
        $app
            .route("/tasks", get(list_tasks).post(add_task))
            .route("/tasks/:id", get(task_snapshot).delete(remove_task))
            .route("/tasks/:id/pause", post(pause_task))
            .route("/tasks/:id/resume", post(resume_task))
            .route("/tasks/:id/logs", get(task_logs))
            .route("/tasks/:id/fallback", post(task_fallback))
            .route("/tasks/:id/webseeds", post(task_webseeds))
            .route("/config", get(config_endpoint))
            .route("/providers", get(providers))
            .route("/ws", get(ws_handler))
    };
}

#[cfg(feature = "nas")]
macro_rules! router_nas {
    ($app:expr) => {
        $app
            .route(
                "/nas/install",
                post(nas_install),
            )
            .route("/nas/start", post(nas_start))
            .route("/nas/stop", post(nas_stop))
            .route("/nas/status", get(nas_status))
            .route("/nas/token", post(nas_token))
            .fallback(nas::nas_proxy)
    };
}
#[cfg(not(feature = "nas"))]
macro_rules! router_nas {
    ($app:expr) => {
        $app
    };
}

#[cfg(feature = "xunlei-import")]
pub fn router(state: Arc<DaemonState>) -> Router {
    router_nas!(router_base!(Router::new()))
        .route("/tasks/xunlei-import", post(add_xunlei_import))
        .with_state(state)
}

#[cfg(not(feature = "xunlei-import"))]
pub fn router(state: Arc<DaemonState>) -> Router {
    router_nas!(router_base!(Router::new())).with_state(state)
}

/// ===== NAS 引擎管理端点（feature nas）=====
#[cfg(feature = "nas")]
#[derive(serde::Deserialize, Default)]
pub struct NasInstallReq {
    /// SPK 文件路径（daemon 可达）。缺省用 manager 现有配置。
    #[serde(default)]
    pub spk_path: Option<String>,
}

#[cfg(feature = "nas")]
async fn nas_install(
    State(_): State<Arc<DaemonState>>,
    req: Option<Json<NasInstallReq>>,
) -> Response {
    use nas::NasError;
    let mgr = nas::manager();
    if let Some(Json(r)) = req {
        if let Some(p) = r.spk_path {
            std::env::set_var("SD_NAS_SPK", p);
            // manager 已初始化则此赋值不生效；首次调用前设置生效。此处仅透传提示。
        }
    }
    match mgr.install().await {
        Ok(info) => Json(serde_json::json!({
            "ok": true,
            "version": info.version,
            "dest": info.dest,
            "engine": info.engine,
        }))
        .into_response(),
        Err(NasError::Install(e)) | Err(NasError::Io(e)) | Err(NasError::Start(e))
        | Err(NasError::Token(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "ok": false, "error": e })),
        )
            .into_response(),
    }
}

#[cfg(feature = "nas")]
async fn nas_start(State(_): State<Arc<DaemonState>>) -> Response {
    use nas::NasError;
    match nas::manager().start().await {
        Ok(pid) => Json(serde_json::json!({ "ok": true, "pid": pid })).into_response(),
        Err(NasError::Install(e)) | Err(NasError::Io(e)) | Err(NasError::Start(e))
        | Err(NasError::Token(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "ok": false, "error": e })),
        )
            .into_response(),
    }
}

#[cfg(feature = "nas")]
async fn nas_stop(State(_): State<Arc<DaemonState>>) -> Response {
    let _ = nas::manager().stop().await;
    Json(serde_json::json!({ "ok": true })).into_response()
}

#[cfg(feature = "nas")]
async fn nas_status(State(_): State<Arc<DaemonState>>) -> Response {
    Json(nas::manager().status().await).into_response()
}

#[cfg(feature = "nas")]
#[derive(serde::Deserialize)]
pub struct NasTokenReq {
    /// xluser OAuth token JSON（L1 云登录产物；格式校准=假设区实测项）。
    pub token_json: String,
}

#[cfg(feature = "nas")]
async fn nas_token(
    State(_): State<Arc<DaemonState>>,
    Json(req): Json<NasTokenReq>,
) -> Response {
    use nas::NasError;
    match nas::put_auth_token(nas::manager(), &req.token_json).await {
        Ok(p) => Json(serde_json::json!({ "ok": true, "path": p })).into_response(),
        Err(NasError::Install(e)) | Err(NasError::Io(e)) | Err(NasError::Start(e))
        | Err(NasError::Token(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "ok": false, "error": e })),
        )
            .into_response(),
    }
}
