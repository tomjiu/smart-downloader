//! HTTP API（axum，M6）：任务 CRUD + 快照 + Provider 运行态 + WS 升级端点（M7）。

#[cfg(feature = "nas")]
use crate::nas;
use crate::state::{DaemonError, DaemonState};
use crate::ws::Throttler;
#[cfg(feature = "nas")]
use axum::response::Response;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::{header, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
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
    /// 任务级下载限速 KiB/s（0 = 不限；缺省 = 走全局）。任务创建成功后应用。
    #[serde(default)]
    pub down_kb_s: Option<u32>,
    /// 任务级上传限速 KiB/s（仅 BT 任务；0 = 不限）。
    #[serde(default)]
    pub up_kb_s: Option<u32>,
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

/// 任务级限速（`POST /tasks/:id/limit`，P1 能力增强）。
/// 字段语义：`None` = 不调整该方向；`0` = 不限速；`n` = 上限 n KiB/s。
/// 合并口径见 `DaemonState::set_task_limits`（快照返回合并后全量配置）。
#[derive(Deserialize)]
pub struct LimitReq {
    #[serde(default)]
    pub down_kb_s: Option<u32>,
    #[serde(default)]
    pub up_kb_s: Option<u32>,
}

/// `POST /tasks/:id/limit`：设置/调整任务级限速，返回合并后的快照。
async fn task_limit(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(req): Json<LimitReq>,
) -> impl IntoResponse {
    match state.set_task_limits(&id, req.down_kb_s, req.up_kb_s).await {
        Ok(_merged) => match state.task_snapshot(&id).await {
            Some(snap) => Json(snap).into_response(),
            None => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "not found" })),
            )
                .into_response(),
        },
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

/// `POST /bt/metadata`（B-1）：magnet → .torrent 元数据抓取（预览/预取 sidecar，
/// 不建任务、不进 registry）。单并发（进行中 → 409）。
#[derive(Deserialize)]
pub struct MagnetMetaReq {
    pub magnet: String,
    /// 抓取总超时（秒）；缺省 60，clamp 5..=600。
    #[serde(default)]
    pub timeout_s: Option<u64>,
    /// DHT 开关；缺省 true（内网/直连场景可关）。
    #[serde(default)]
    pub dht: Option<bool>,
    /// 追加 tracker（magnet 自带 tr 之外）。
    #[serde(default)]
    pub trackers: Vec<String>,
    /// 已知 peer 引导（`ip:port`；本地 seeder / 手动注入）。
    #[serde(default)]
    pub peers: Vec<String>,
    /// 可选：成功后将 .torrent 落盘到「下载根目录」（`[download] dest_root`）下
    /// 的该相对路径（V15：拒绝绝对路径/`..`/盘符前缀；父目录须已存在）。
    #[serde(default)]
    pub save_to: Option<String>,
}

#[cfg(feature = "bt")]
async fn bt_magnet_metadata(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<MagnetMetaReq>,
) -> impl IntoResponse {
    use tokio::sync::Semaphore;

    // V16（CWE-667）：单并发门禁改为 RAII 信号量——`try_acquire` 拿到的 permit
    // 在 drop 时自动归还。修复前用裸 `AtomicBool` + `compare_exchange`：客户端在
    // 抓取 `await` 期间断连 → handler future 被 drop → `BUSY.store(false)` 永不
    // 执行 → 端点此后对所有请求永久 409。permit 是 handler future 的局部值，
    // 正常返回与取消（断连）两条路径都必然 drop，门禁不可能锁死。
    static METADATA_GATE: Semaphore = Semaphore::const_new(1);

    // 参数面校验（同步、快）：magnet / peers / save_to / timeout
    let peers: Vec<std::net::SocketAddr> = match req
        .peers
        .iter()
        .map(|s| s.parse::<std::net::SocketAddr>())
        .collect()
    {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("peers 解析失败: {e}") })),
            )
                .into_response();
        }
    };
    // V15（CWE-22/23）：save_to 前置校验——必须是相对下载根目录的相对路径，
    // 越界/穿越在抓取（5-600s）开始前即 400 快速失败（不占门禁）。
    let save_to = match &req.save_to {
        Some(s) => match validate_save_dest(&state.default_dest_root(), s) {
            Ok(p) => Some(p),
            Err(msg) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": msg })),
                )
                    .into_response();
            }
        },
        None => None,
    };
    let timeout = std::time::Duration::from_secs(req.timeout_s.unwrap_or(60).clamp(5, 600));
    let opts = smart_dl_btcore::magnet::FetchOpts {
        timeout,
        extra_trackers: req.trackers.clone(),
        bootstrap_peers: peers,
        enable_dht: req.dht.unwrap_or(true),
        ..Default::default()
    };
    let magnet = req.magnet.clone();

    let _permit = match METADATA_GATE.try_acquire() {
        Ok(p) => p,
        Err(_) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({ "error": "已有 metadata 抓取进行中（单并发）" })),
            )
                .into_response();
        }
    };
    // 持有 _permit 直至 handler 结束（含取消路径）→ 无需手动释放。
    let result = run_magnet_fetch(&magnet, &opts, save_to.as_deref()).await;

    match result {
        Ok(body) => Json(body).into_response(),
        Err((code, msg)) => (code, Json(serde_json::json!({ "error": msg }))).into_response(),
    }
}

/// 安全修复（V15，CWE-22/23）：`POST /bt/metadata` 的 `save_to` 落盘路径校验。
/// 修复前 `std::fs::write(dest)` 直写用户输入 → 已认证客户端可写任意路径
/// （绝对路径 `/etc/crontab`、`..` 穿越、Windows 目标上的盘符前缀）。
/// 修复后语义：
/// - 仅接受相对路径，逐分量白名单（Normal/CurDir），拒绝 `..`、绝对前缀、
///   盘符（与 fix/security-p0（PR #5）`sanitize_rel` 同构，合并后两处一致）；
/// - 最终落盘点 = `<default_dest_root>/save_to`，不再接受任意绝对目标；
/// - 父目录须已存在（保留原契约），且 canonicalize 后必须仍在根目录内——
///   拦截已存在的 symlink 指向根外（写穿逃逸）。
///
/// 纯路径校验（仅 std，无 btcore 依赖）：不挂 cfg(bt)——无 bt 构建的测试
/// 同样覆盖，亦可供其他落盘端点复用。
pub fn validate_save_dest(root: &std::path::Path, raw: &str) -> Result<std::path::PathBuf, String> {
    let rel = std::path::PathBuf::from(raw);
    if rel.as_os_str().is_empty() {
        return Err("save_to 为空".into());
    }
    if rel.is_absolute() {
        return Err(format!("save_to 必须是相对下载根目录的相对路径: {raw}"));
    }
    for comp in rel.components() {
        match comp {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            // ParentDir(..) / RootDir(/) / Prefix(C:) / 其他 → 一律拒绝
            _ => {
                return Err(format!(
                    "save_to 含非法路径分量（拒绝 .. 穿越、绝对/盘符前缀）: {raw}"
                ))
            }
        }
    }
    let dest = root.join(&rel);
    let parent = dest
        .parent()
        .ok_or_else(|| format!("save_to 无父目录: {raw}"))?
        .to_path_buf();
    let parent_canon = parent
        .canonicalize()
        .map_err(|e| format!("save_to 父目录须已存在且可达: {}: {e}", parent.display()))?;
    let root_canon = root
        .canonicalize()
        .map_err(|e| format!("下载根目录不可达: {}: {e}", root.display()))?;
    if !parent_canon.starts_with(&root_canon) {
        return Err(format!("save_to 越界（解析后不在下载根目录内）: {raw}"));
    }
    Ok(dest)
}

/// S2（第五轮复审非阻断项收尾）：magnet 抓取 scratch 目录遗留清理。
/// `run_magnet_fetch` 的 best-effort 清理覆盖正常返回与客户端断连（`spawn_blocking`
/// 任务不随 handler future 取消，尾部清理必然执行）；唯进程级死亡（kill -9 /
/// 断电）会留下 `smart-dl-magnet-fetch-{pid}-{nanos}` 残骸。daemon 启动时
/// （serve.rs）与每次抓取前各清扫一次：仅删除「PID ≠ 本进程 且 目录 mtime
/// 距今超过阈值」的遗留——阈值（30 分钟）大于单次抓取上限（600s）的两倍
/// 裕量，任何活跃抓取的 scratch 都不可能命中。
/// 并发安全假设：生产单实例部署（lockfile 保证）；测试并行时同进程 PID 相同
/// 天然跳过，跨进程的活跃 scratch 因 mtime 新鲜被阈值保护。
pub fn cleanup_stale_magnet_scratch() {
    cleanup_stale_magnet_scratch_with(std::time::Duration::from_secs(30 * 60));
}

/// `cleanup_stale_magnet_scratch` 的可调阈值版本（供测试注入 max_age=0 /
/// 大阈值分别验证删除与保护两条分支）。
pub fn cleanup_stale_magnet_scratch_with(max_age: std::time::Duration) {
    const PREFIX: &str = "smart-dl-magnet-fetch-";
    let now = std::time::SystemTime::now();
    let current_pid = std::process::id();
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(PREFIX)) else {
            continue;
        };
        // 名字格式 {pid}-{nanos}：只处理可归属的条目——解析失败视为非本程序
        // 产物（或未来格式变更），一律不动。
        let Some(pid) = rest.split('-').next().and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        if pid == current_pid {
            continue; // 本进程的 scratch（活跃抓取）永不清理
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        // 时钟回拨（duration_since 失败）→ 视为新鲜，不动
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age < max_age {
            continue;
        }
        // best-effort：目录用 remove_dir_all；同前缀的散文件（异常残留）兜底
        // remove_file——remove_dir_all 对普通文件会 ENOTDIR 失败。
        let p = entry.path();
        if p.is_dir() {
            let _ = std::fs::remove_dir_all(&p);
        } else {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// 抓取执行体（spawn_blocking 包装阻塞流程 + 临时 scratch 目录管理）。
#[cfg(feature = "bt")]
async fn run_magnet_fetch(
    magnet: &str,
    opts: &smart_dl_btcore::magnet::FetchOpts,
    save_to: Option<&std::path::Path>,
) -> Result<serde_json::Value, (StatusCode, String)> {
    use smart_dl_btcore::magnet::{fetch_metadata, FetchError};

    // S2：先清扫历史遗留（进程死亡残骸），再建本进程 scratch
    cleanup_stale_magnet_scratch();
    let scratch = std::env::temp_dir().join(format!(
        "smart-dl-magnet-fetch-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    if let Err(e) = std::fs::create_dir_all(&scratch) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("scratch 目录创建失败: {e}"),
        ));
    }

    let m = magnet.to_string();
    let o = opts.clone();
    let scratch_for_task = scratch.clone();
    let res = tokio::task::spawn_blocking(move || fetch_metadata(&m, &scratch_for_task, &o))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("抓取任务 join 失败: {e}"),
            )
        })
        .and_then(|r| {
            r.map_err(|e| match e {
                FetchError::Magnet(_) | FetchError::Summary(_) => {
                    (StatusCode::BAD_REQUEST, e.to_string())
                }
                FetchError::Timeout { .. } => (StatusCode::REQUEST_TIMEOUT, e.to_string()),
                FetchError::Ffi(_) | FetchError::Other(_) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
                }
            })
        });

    // scratch 目录 best-effort 清理（含抓取中途的部分文件）
    let _ = std::fs::remove_dir_all(&scratch);
    let fetched = res?;

    // 可选落盘（V15：save_to 已在 handler 前置校验=根内相对路径+父目录存在；
    // 此处写失败 → 500）
    let mut saved_to: Option<String> = None;
    if let Some(dest) = save_to {
        std::fs::write(dest, &fetched.torrent).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(".torrent 落盘失败 {}: {e}", dest.display()),
            )
        })?;
        saved_to = Some(dest.display().to_string());
    }

    let s = &fetched.summary;
    Ok(serde_json::json!({
        "infohash": fetched.infohash,
        "name": s.name,
        "total_size": s.total_size,
        "piece_len": s.piece_len,
        "num_pieces": s.num_pieces,
        "files": s.files.iter().map(|f| serde_json::json!({
            "path": f.path,
            "size": f.size,
        })).collect::<Vec<_>>(),
        "trackers": s.trackers,
        "web_seeds": s.web_seeds,
        "comment": s.comment,
        "created_by": s.created_by,
        "torrent_b64": base64::engine::general_purpose::STANDARD.encode(&fetched.torrent),
        "saved_to": saved_to,
    }))
}

#[cfg(not(feature = "bt"))]
async fn bt_magnet_metadata(
    State(_state): State<Arc<DaemonState>>,
    Json(req): Json<MagnetMetaReq>,
) -> impl IntoResponse {
    let _ = req; // 参数不校验：无 BT 引擎的构建里该端点恒不可用
    (
        StatusCode::BAD_REQUEST,
        Json(
            serde_json::json!({ "error": "metadata 抓取需 BT 引擎（编译时启用 --features daemon/bt）" }),
        ),
    )
}

/// `GET /config`：生效配置快照（serve 注入；未注入时提示）。
async fn config_endpoint(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    Json(state.config_snapshot())
}

/// 全局统计（任务按状态/引擎聚合 + 聚合速率，速率口径 1s 快照）。
async fn stats_endpoint(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    Json(state.stats())
}

/// 版本与编译特性（对齐部署矩阵：二进制是哪个 feature 组合的构建）。
async fn version_endpoint() -> impl IntoResponse {
    Json(serde_json::json!({
        "name": "smart-dl-daemon",
        "version": env!("CARGO_PKG_VERSION"),
        "features": {
            "bt": cfg!(feature = "bt"),
            "ftp": cfg!(feature = "ftp"),
            "nas": cfg!(feature = "nas"),
            "xunlei": cfg!(feature = "xunlei"),
        },
    }))
}

/// 存活探针（liveness）：200 = 进程可达且路由栈就绪。引擎故障属任务级
/// 状态（见 /tasks 与 /stats），不在此反映。与全端点一致受 auth_mw 保护
/// —— 配置 token 时探针需携带 Bearer（V1/V13 fail-closed 姿态，不设例外）。
async fn health_endpoint() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
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
        Ok(task_id) => {
            // 建任务时可选任务级限速：复用 set_task_limits 全链（合并/持久化/重放）。
            // 任务已存在 → 限速失败不回滚任务，返回错误体提示单独重试 limit 调用。
            let limits_err = if req.down_kb_s.is_some() || req.up_kb_s.is_some() {
                state
                    .set_task_limits(&task_id, req.down_kb_s, req.up_kb_s)
                    .await
                    .err()
            } else {
                None
            };
            match limits_err {
                None => (
                    StatusCode::CREATED,
                    Json(serde_json::json!({ "task_id": task_id })),
                ),
                Some(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "task_id": task_id,
                        "error": format!("任务已创建但限速设置失败: {e}（可单独重试 POST /tasks/:id/limit）")
                    })),
                ),
            }
        }
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
        $app.route("/tasks", get(list_tasks).post(add_task))
            .route("/tasks/:id", get(task_snapshot).delete(remove_task))
            .route("/tasks/:id/pause", post(pause_task))
            .route("/tasks/:id/resume", post(resume_task))
            .route("/tasks/:id/logs", get(task_logs))
            .route("/tasks/:id/fallback", post(task_fallback))
            .route("/tasks/:id/limit", post(task_limit))
            .route("/tasks/:id/webseeds", post(task_webseeds))
            .route("/bt/metadata", post(bt_magnet_metadata))
            .route("/config", get(config_endpoint))
            .route("/stats", get(stats_endpoint))
            .route("/version", get(version_endpoint))
            .route("/health", get(health_endpoint))
            .route("/providers", get(providers))
            .route("/ws", get(ws_handler))
    };
}

/// 安全修复（V1/V13，CWE-306）：HTTP API 认证中间件——覆盖全部路由（含 /ws
/// 升级握手与 NAS 代理）。未配置 token 时放行（serve 启动检查已保证该模式
/// 仅回环可达）；已配置时非 Bearer 匹配一律 401。
async fn auth_mw(
    State(state): State<Arc<DaemonState>>,
    req: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let authorization = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if state.verify_http_token(authorization) {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(
                serde_json::json!({ "error": "unauthorized: 需要 Authorization: Bearer <token>" }),
            ),
        )
            .into_response()
    }
}

#[cfg(feature = "nas")]
macro_rules! router_nas {
    ($app:expr) => {
        $app.route("/nas/install", post(nas_install))
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
        .layer(middleware::from_fn_with_state(state.clone(), auth_mw))
        .with_state(state)
}

#[cfg(not(feature = "xunlei-import"))]
pub fn router(state: Arc<DaemonState>) -> Router {
    router_nas!(router_base!(Router::new()))
        .layer(middleware::from_fn_with_state(state.clone(), auth_mw))
        .with_state(state)
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
    // 安全修复（V8，CWE-759/UB）：API 请求不得写入进程环境变量——多线程
    // set_var 是 UB，且无认证调用方可借它把 SPK 解包源指向任意路径（联动 V5）。
    // SPK 路径只能经启动期环境变量 SD_NAS_SPK 配置；请求值与生效配置不一致 → 409。
    if let Some(Json(r)) = req {
        if let Some(p) = r.spk_path {
            let configured = std::env::var("SD_NAS_SPK").unwrap_or_default();
            if configured != p {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "ok": false,
                        "error": "spk_path 仅支持启动期配置（环境变量 SD_NAS_SPK），API 运行时变更已禁用（安全修复 V8）"
                    })),
                )
                    .into_response();
            }
        }
    }
    let mgr = nas::manager();
    match mgr.install().await {
        Ok(info) => Json(serde_json::json!({
            "ok": true,
            "version": info.version,
            "dest": info.dest,
            "engine": info.engine,
        }))
        .into_response(),
        Err(NasError::Install(e))
        | Err(NasError::Io(e))
        | Err(NasError::Start(e))
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
        Err(NasError::Install(e))
        | Err(NasError::Io(e))
        | Err(NasError::Start(e))
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
async fn nas_token(State(_): State<Arc<DaemonState>>, Json(req): Json<NasTokenReq>) -> Response {
    use nas::NasError;
    match nas::put_auth_token(nas::manager(), &req.token_json).await {
        Ok(p) => Json(serde_json::json!({ "ok": true, "path": p })).into_response(),
        Err(NasError::Install(e))
        | Err(NasError::Io(e))
        | Err(NasError::Start(e))
        | Err(NasError::Token(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "ok": false, "error": e })),
        )
            .into_response(),
    }
}
