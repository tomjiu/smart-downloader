//! HTTP API（axum，M6）：任务 CRUD + 快照 + Provider 运行态 + WS 升级端点（M7）。

use crate::events::{known_event_type_labels, Envelope};
#[cfg(feature = "nas")]
use crate::nas;
use crate::state::{DaemonError, DaemonState};
use crate::ws::Throttler;
// E10：list_events 返回 Response；原仅 nas 使用，现两 feature 均用 → 提出 cfg。
use axum::response::Response;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{sse::Event as SseEvent, IntoResponse, Sse},
    routing::{get, post},
    Json, Router,
};
use base64::Engine as _;
use serde::Deserialize;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::state::{known_engine_labels, known_state_labels, BatchAction, ListQuery};

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
    /// 顺序下载（边下边播）。HTTP = 在飞段窗口收紧；BT = sequential_download
    /// flag；缺省 = 默认并行策略。
    #[serde(default)]
    pub sequential: bool,
    /// 任务级代理 URL（E5，仅 HTTP 任务生效）：`http(s)://` / `socks5://` /
    /// `socks4://`，可带 `user:pass@`；缺省 = 走全局 `[download] proxy`（若有）。
    /// 非法代理 URL → 400；BT/FTP 任务携带此字段被忽略。
    #[serde(default)]
    pub proxy: Option<String>,
    /// 任务级自定义请求头（E6，仅 HTTP 任务生效）：随探测与全部段请求下发
    /// （H-8 链路）；典型用途 Referer/Cookie 反防盗链。BT/FTP 任务忽略。
    #[serde(default)]
    pub headers: Option<std::collections::BTreeMap<String, String>>,
    /// HTTP Basic 认证用户名（E6，仅 HTTP 任务生效）。与 `password` 组合成
    /// Basic 凭据；`password` 缺省 = 空串。BT/FTP 任务忽略。
    #[serde(default)]
    pub username: Option<String>,
    /// HTTP Basic 认证密码（E6，仅 HTTP 任务生效；可空串）。
    #[serde(default)]
    pub password: Option<String>,
    /// 主源内容校验目标（E6，仅 HTTP 任务生效）：64 位十六进制 sha256。
    /// 传入后内容校验失败走既有处置链（重下 → 备用源 → 隔离试错 → 降级）。
    #[serde(default)]
    pub sha256: Option<String>,
    /// 备用源 URL（E6，仅 HTTP 任务生效）：主源探测/校验失败自动兜底。
    #[serde(default)]
    pub backup_url: Option<String>,
    /// 备用源 md5（E6，32 位十六进制）：必须与 `backup_url` 成对提供，
    /// 备用源内容校验目标。
    #[serde(default)]
    pub backup_md5: Option<String>,
    /// 用户显式落盘名（E6，仅 HTTP 任务生效）：非法路径 → 400；
    /// 缺省 = 引擎派生链（Content-Disposition → URL 末段 → download.bin）。
    #[serde(default)]
    pub name: Option<String>,
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

/// 任务级顺序下载（`POST /tasks/:id/sequential`，边下边播）。
/// HTTP = 在飞段窗口收紧（运行中任务下轮拾取）；BT = sequential_download flag
/// （即时生效）；FTP = 不支持（409）。
#[derive(Deserialize)]
pub struct SequentialReq {
    pub sequential: bool,
}

async fn task_sequential(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(req): Json<SequentialReq>,
) -> impl IntoResponse {
    match state.set_task_sequential(&id, req.sequential).await {
        Ok(()) => match state.task_snapshot(&id).await {
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

/// 任务级代理热改（`POST /tasks/:id/proxy`，E8）：`{"proxy": "http(s)://..."}`
/// = 设置任务专用 client（覆盖全局）；`{"proxy": null}` 或 `{}` = 清除回共享
/// client。仅 HTTP 任务（其余 409）；非法 URL 400（不发起连接，纯本地构建
/// 试水）；下载中任务立即生效（旧循环检查点退出，进度凭段账本恢复）。
#[derive(Deserialize)]
pub struct SetProxyReq {
    /// None（缺省/null）= 清除；空串拒绝（与 add 口径一致）。
    #[serde(default)]
    pub proxy: Option<String>,
}

async fn task_set_proxy(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(req): Json<SetProxyReq>,
) -> impl IntoResponse {
    match state.set_task_proxy(&id, req.proxy).await {
        Ok(()) => match state.task_snapshot(&id).await {
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
                DaemonError::InvalidSource(_) => StatusCode::BAD_REQUEST,
                DaemonError::UnsupportedOp(_) => StatusCode::CONFLICT,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, body).into_response()
        }
    }
}

/// 任务重命名（`POST /tasks/:id/name`，E15）：`{"name": "..."}` = 设置
/// （V3 校验同 add，非法路径分量 400）；`{"name": null}` 或 `{}` = 清除
/// 显式名（回退派生链）。显示层改名——落盘路径 add 时已定，不迁移文件。
#[derive(Deserialize)]
pub struct RenameTaskReq {
    /// None（缺省/null）= 清除；空白拒绝（与 add 口径一致）。
    #[serde(default)]
    pub name: Option<String>,
}

/// 任务标签设置（E18，`POST /tasks/:id/tags`）：**替换式**全量覆盖。
/// `tags` 缺省/null 或空数组 = 清除全部；逐项 trim/去重，单项 1..=64 字符、
/// 最多 16 个（超限 400）。
#[derive(Deserialize)]
pub struct TagsReq {
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// 任务标签设置（`POST /tasks/:id/tags`，E18）：返回归一化后的标签全集。
async fn task_set_tags(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(req): Json<TagsReq>,
) -> impl IntoResponse {
    match state.set_task_tags(&id, req.tags) {
        Ok(tags) => Json(serde_json::json!({ "tags": tags })).into_response(),
        Err(e) => {
            let body = Json(serde_json::json!({ "error": e.to_string() }));
            let status = match e {
                DaemonError::NotFound(_) => StatusCode::NOT_FOUND,
                DaemonError::InvalidSource(_) => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, body).into_response()
        }
    }
}

async fn task_rename(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(req): Json<RenameTaskReq>,
) -> impl IntoResponse {
    match state.set_task_name(&id, req.name) {
        Ok(()) => match state.task_snapshot(&id).await {
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
                DaemonError::InvalidSource(_) => StatusCode::BAD_REQUEST,
                DaemonError::UnsupportedOp(_) => StatusCode::CONFLICT,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, body).into_response()
        }
    }
}

/// 任务级子文件优先级（`POST /tasks/:id/files/priority`，P1 能力增强，仅 BT）。
/// priority 语义同 libtorrent：0=不下载 / 1=低 / 4=默认 / 7=最高。
#[derive(Deserialize)]
pub struct FilePriorityReq {
    pub priorities: Vec<FilePriorityEntry>,
}

#[derive(Deserialize)]
pub struct FilePriorityEntry {
    pub index: usize,
    pub priority: u32,
}

/// `POST /tasks/:id/files/priority`：批量设置子文件优先级，响应 = 当前各文件
/// 优先级快照（下标 = 文件序，与快照 files 对齐）。
async fn task_file_priority(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(req): Json<FilePriorityReq>,
) -> impl IntoResponse {
    let prio: Vec<(usize, u32)> = req
        .priorities
        .into_iter()
        .map(|e| (e.index, e.priority))
        .collect();
    match state.set_task_file_priorities(&id, &prio).await {
        Ok(prios) => {
            let list: Vec<serde_json::Value> = prios
                .into_iter()
                .map(|p| match p {
                    Some(v) => serde_json::json!(v),
                    None => serde_json::Value::Null,
                })
                .collect();
            Json(serde_json::json!({ "priorities": list })).into_response()
        }
        Err(e) => {
            let body = Json(serde_json::json!({ "error": e.to_string() }));
            let status = match e {
                DaemonError::NotFound(_) => StatusCode::NOT_FOUND,
                DaemonError::UnsupportedOp(_) => StatusCode::CONFLICT,
                DaemonError::InvalidSource(_) => StatusCode::BAD_REQUEST,
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
///   拦截已存在的 symlink 指向根外（写穿逃逸）；
/// - 末段 symlink 设防（H-2，CWE-59）：parent canonicalize 只覆盖中间分量，
///   dest 本身若为 symlink，`fs::write` 将写穿到链接目标（可越出根目录）。
///   `symlink_metadata` 不跟随末段链接 → 恰好检测链接本体；末段为 symlink
///   一律拒绝（含指向根内的链接——语义简单不留旁门）。
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
    // 末段 symlink 设防（H-2，CWE-59）：parent canonicalize 只解析中间分量；
    // dest 本身是 symlink 时（如 save_to="evil.torrent" 且 root/evil.torrent
    // → /etc/crontab），上面全部检查照过，后续 fs::write 却写穿到根外。
    // symlink_metadata 读链接本体不读目标 → 恰好识别末段链接；
    // 目标不存在（首次写入）→ Err 分支跳过，原语义不变。
    if let Ok(md) = std::fs::symlink_metadata(&dest) {
        if md.file_type().is_symlink() {
            return Err(format!(
                "save_to 目标已存在且为符号链接（拒绝写穿逃逸）: {raw}"
            ));
        }
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

/// 全局限速总阀门热改（`POST /config/limit`，E16）。
///
/// 请求体（两字段均可选，`Option` 缺省语义）：
/// - `max_download_kb_s`：所有引擎合计下行上限（KiB/s；0 = 不限）
/// - `max_upload_kb_s`：BT 合计上行上限（KiB/s；0 = 不限；HTTP/FTP 无上传）
/// - 字段缺省/null = 该方向不调整；双缺省 = 纯查询（返回当前值）
///
/// 成功：200 + 当前生效 `{max_download_kb_s, max_upload_kb_s}`；
/// 下发失败（如 BT settings_pack 错误）：500。不落盘——重启回到配置文件口径；
/// `GET /config` 快照的两键随生效值同步刷新，`global_limits_changed` 事件广播。
#[derive(Deserialize)]
pub struct GlobalLimitReq {
    #[serde(default)]
    pub max_download_kb_s: Option<u32>,
    #[serde(default)]
    pub max_upload_kb_s: Option<u32>,
}

async fn config_set_limit(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<GlobalLimitReq>,
) -> impl IntoResponse {
    match state
        .apply_global_limits(req.max_download_kb_s, req.max_upload_kb_s)
        .await
    {
        Ok(g) => Json(g).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
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

/// 删除任务（引擎 remove + 目录记录移除；`?delete_data=true` 同步删除已下载
/// 数据（E7：BT 删种子数据 / HTTP 删落盘文件），缺省 false 保留文件）。
#[derive(Deserialize, Default)]
struct RemoveTaskQuery {
    #[serde(default)]
    delete_data: bool,
}

async fn remove_task(
    Path(id): Path<String>,
    Query(q): Query<RemoveTaskQuery>,
    State(state): State<Arc<DaemonState>>,
) -> impl IntoResponse {
    match state.remove_with(&id, q.delete_data).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

/// WS 升级端点（D36 socket 端点，M7）：连接即推全量（重连重同步），随后
/// 轮询增量 + 1s 快照节流（Progress/Speed 合并）。E10：首推/轮询均改非破坏
/// 读（read_after/snapshot_upto）——多连接不再互踩（原 drain 单消费者语义
/// 升级）；事件缓冲非清空，历史可事后经 GET /events 查询。
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<DaemonState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_session(socket, state))
}

async fn ws_session(mut socket: WebSocket, state: Arc<DaemonState>) {
    let hub = state.hub();

    // 1) 连接即推送全量（掉队客户端重连重同步入口；E10 非破坏读——
    //    多连接各自拉到同一份历史，不再互踩）
    let mut last_seq = 0u64;
    for env in hub.read_after(0, usize::MAX) {
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
                    state
                        .add_torrent_task_opts(bytes, req.dest, req.sequential)
                        .await
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
        state
            .add_link_task_opts(
                url,
                req.dest,
                crate::state::AddHttpOpts {
                    sequential: req.sequential,
                    proxy: req.proxy,
                    headers: req
                        .headers
                        .map(|m| m.into_iter().collect())
                        .unwrap_or_default(),
                    basic_auth: req.username.map(|u| (u, req.password.unwrap_or_default())),
                    sha256: req.sha256,
                    backup_url: req.backup_url,
                    backup_md5: req.backup_md5,
                    name: req.name,
                },
            )
            .await
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

/// `GET /tasks` 查询参数（E7，全部可选；无参数时行为/形状与 E7 之前一致）。
/// `state`/`engine` 逗号分隔多值（大小写不敏感）；`limit`/`offset` 分页，
/// 提供分页参数时响应附 `X-Total-Count`（过滤后总数）。
#[derive(Deserialize, Default)]
struct ListTasksQuery {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    /// E14：关键字子串搜索（匹配任务名/来源 URL，大小写不敏感；空白 = 不过滤）。
    #[serde(default)]
    search: Option<String>,
    /// E18：标签 any-of 过滤（逗号分隔多值；大小写不敏感；空白段丢弃）。
    #[serde(default)]
    tag: Option<String>,
}

/// `limit` 上限（防一次性拉全表打爆内存；UI 每页 50 量级，500 留足余量）。
const LIST_LIMIT_CAP: usize = 500;

/// 逗号分隔标签解析 + 合法性校验（大小写不敏感；空段/空白段忽略）。
/// 非法值 → Err(错误信息，含合法值全集)。
fn parse_label_list(
    raw: &Option<String>,
    known: &[String],
    dim: &str,
) -> Result<Vec<String>, String> {
    let Some(raw) = raw else {
        return Ok(vec![]);
    };
    let labels: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    for l in &labels {
        if !known.iter().any(|k| k.eq_ignore_ascii_case(l)) {
            return Err(format!(
                "未知 {dim} 值 {l:?}（合法值: {}）",
                known.join(", ")
            ));
        }
    }
    Ok(labels)
}

/// 校验查询参数 → ListQuery（400 语义：state/engine 值非法或 limit 越界）。
fn validate_list_query(q: &ListTasksQuery) -> Result<ListQuery, String> {
    let states = parse_label_list(&q.state, &known_state_labels(), "state")?;
    let engines = parse_label_list(&q.engine, &known_engine_labels(), "engine")?;
    if let Some(l) = q.limit {
        if l == 0 || l > LIST_LIMIT_CAP {
            return Err(format!("limit 须在 1..={LIST_LIMIT_CAP}"));
        }
    }
    Ok(ListQuery {
        states,
        engines,
        limit: q.limit,
        offset: q.offset.unwrap_or(0),
        // E14：trim 后为空视为未提供（宽容处理，与“空 = 不过滤”语义一致）
        search: q
            .search
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        // E18：逗号分隔 → trim → 丢空白段（标签为用户自定义任意串，
        // 不走 known-label 校验；全空 = 不过滤）
        tags: q
            .tag
            .as_deref()
            .map(|s| {
                s.split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// 任务列表（E7 过滤/分页；E14 增 `?search=` 关键字过滤）：无参数 → 全量数组
/// （兼容不变）；有分页参数 → 附 `X-Total-Count`（过滤后总数，含 search 过滤）。
/// 排序恒为创建序（task_id 数值后缀）。
async fn list_tasks(
    Query(q): Query<ListTasksQuery>,
    State(state): State<Arc<DaemonState>>,
) -> impl IntoResponse {
    match validate_list_query(&q) {
        Ok(lq) => {
            let paged = q.limit.is_some() || q.offset.is_some();
            let (items, total) = state.list_filtered(&lq);
            let mut resp = Json(items).into_response();
            if paged {
                if let Ok(v) = HeaderValue::from_str(&total.to_string()) {
                    resp.headers_mut().insert("x-total-count", v);
                }
            }
            resp
        }
        Err(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
    }
}

/// 批量操作请求（E7）：action ∈ {pause, resume, remove}；`delete_data` 仅
/// remove 生效；ids 上限 100、非空；逐项执行单项失败不短路（恒 200 + 逐项结果）。
#[derive(Deserialize)]
struct BatchTaskReq {
    action: String,
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    delete_data: bool,
    /// E19：条件选择（与 `ids` 二选一：同时提供或均缺失 → 400）。命中集
    /// 全量执行 action；仅支持 pause/resume（remove 走显式 ids，非破坏性原则）。
    #[serde(default)]
    select: Option<BatchSelect>,
}

/// 条件选择器（E19）：与 `GET /tasks` 过滤参数同口径——`state`/`engine`
/// 走合法标签校验（逗号分隔多值，大小写不敏感）；`tag` 逗号分隔任意串；
/// `search` 子串。至少提供一个条件（全空 → 400，防 `{}` 误匹配全量任务）。
#[derive(Deserialize, Default)]
struct BatchSelect {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    search: Option<String>,
}

fn parse_batch_select(s: &BatchSelect) -> Result<ListQuery, String> {
    let empty = s.state.is_none() && s.engine.is_none() && s.tag.is_none() && s.search.is_none();
    if empty {
        return Err("选择条件不能全空（state/engine/tag/search 至少一项）".into());
    }
    Ok(ListQuery {
        states: parse_label_list(&s.state, &known_state_labels(), "state")?,
        engines: parse_label_list(&s.engine, &known_engine_labels(), "engine")?,
        limit: None,
        offset: 0,
        search: s
            .search
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        tags: s
            .tag
            .as_deref()
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// 批量 id 上限（批量语义是便利入口不是全表操作入口；防误传全量 id 集合）。
const BATCH_IDS_CAP: usize = 100;

/// `GET /events` 查询参数（E10）：`after` seq 游标（默认 0 = 全量重同步）、
/// `limit` 页长（默认 100）、`task_id`/`type` 可选过滤（`type` 逗号分隔多值，
/// 大小写不敏感，对齐 E7 风格）。
#[derive(Deserialize, Default)]
struct ListEventsQuery {
    #[serde(default)]
    after: Option<u64>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default, rename = "type")]
    event_type: Option<String>,
}

/// `GET /events` 页长上限（E10：UI 每页 50–100 量级；4096 全量可分页拉取）。
const EVENTS_LIMIT_CAP: usize = 1000;
/// `GET /events` 缺省页长（不传 limit 也限页，防一次性拉全缓冲）。
const EVENTS_LIMIT_DEFAULT: usize = 100;

/// 事件历史查询（E10）：非破坏读 WsHub 环形缓冲；seq 游标分页 + task_id/
/// type 过滤 + `truncated` 缺口报警（缓冲已冲掉目标区间 → 客户端应放弃
/// 增量补拉改走全量重同步）。过滤在分页前生效（limit = 过滤后条数）。
async fn list_events(
    Query(q): Query<ListEventsQuery>,
    State(state): State<Arc<DaemonState>>,
) -> Response {
    let types = match parse_label_list(&q.event_type, &known_event_type_labels(), "type") {
        Ok(t) => t,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    if let Some(l) = q.limit {
        if l == 0 || l > EVENTS_LIMIT_CAP {
            return (
                StatusCode::BAD_REQUEST,
                format!("limit 须在 1..={EVENTS_LIMIT_CAP}"),
            )
                .into_response();
        }
    }
    let limit = q.limit.unwrap_or(EVENTS_LIMIT_DEFAULT);
    let after = q.after.unwrap_or(0);
    let hub = state.hub();
    let pred = |env: &Envelope| {
        let task_ok = q
            .task_id
            .as_deref()
            .is_none_or(|want| env.event.task_id() == Some(want));
        let type_ok = types.is_empty()
            || types
                .iter()
                .any(|t| env.event.type_label().eq_ignore_ascii_case(t));
        task_ok && type_ok
    };
    let (events, has_more) = hub.read_filtered(after, limit, pred);
    let next_after = events.last().map(|e| e.seq).unwrap_or(after);
    let truncated = hub.gap_after(after);
    Json(serde_json::json!({
        "events": events,
        "next_after": next_after,
        "has_more": has_more,
        "oldest_seq": hub.oldest_seq(),
        "truncated": truncated,
    }))
    .into_response()
}

/// `GET /events/stream` 查询参数：`after` 游标（显式覆盖 Last-Event-ID 头，
/// 调试友好）/ `task_id` / `type` 过滤（语义同 GET /events）。无 after 时读
/// `Last-Event-ID` 请求头（EventSource 断线重连自动携带），都无 = 0（全量
/// 历史重放）；头解析失败视为未携带（EventSource 只发合法值）。
#[derive(Deserialize, Default)]
struct EventsStreamQuery {
    #[serde(default)]
    after: Option<u64>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default, rename = "type")]
    event_type: Option<String>,
}

/// SSE 轮询周期（与 WS 会话同口径：200ms 增量轮询 + 1s 快照节流）。
const SSE_POLL_INTERVAL: Duration = Duration::from_millis(200);
/// SSE 每轮批量拉取上限（200ms × 256 = 1280 ev/s 稳态，远超背压丢弃口径）。
const SSE_BATCH: usize = 256;

/// Envelope → SSE 帧：`id: <seq>`（EventSource 断线自动回传 Last-Event-ID）、
/// `event: <type_label>`（EventSource addEventListener 按类型路由）、
/// `data: <envelope JSON>`（与 WS 帧、GET /events 条目同形——单套解析复用）。
/// 序列化失败 → None（跳帧，不毒化流）。
fn envelope_to_sse(env: &Envelope) -> Option<SseEvent> {
    let data = serde_json::to_string(env).ok()?;
    Some(
        SseEvent::default()
            .id(env.seq.to_string())
            .event(env.event.type_label())
            .data(data),
    )
}

/// SSE 流状态机（unfold）：pending 先排空（重放/当前批），空则等下一轮
/// 轮询增量；客户端断开 = 响应体 dropped → unfold future 被 poll 中止，
/// 状态自然释放（无泄漏后台任务）。
struct SseStreamState {
    state: Arc<DaemonState>,
    cursor: u64,
    pred: Box<dyn Fn(&Envelope) -> bool + Send>,
    pending: VecDeque<SseEvent>,
    throttler: Throttler,
    last_flush: Instant,
    tick: tokio::time::Interval,
}

/// SSE 事件流端点（E12）：`text/event-stream` 长连接——连接即重放历史
/// （非破坏读，与 WS 首推同口径），随后 200ms 轮询增量 + 1s 节流
/// （Progress/Speed 合并，与 WS 同口径；重放不节流）。缺口（gap_after）→
/// 注释行 `: gap` 报警 + 从缓冲最旧重放（客户端可按首帧 seq 回退或注释行
/// 判定增量补拉失效，同 REST `truncated` 的判定输入）。与 WS 的差异：
/// 单向推送（无上行）、HTTP/1.1 友好（代理穿透/浏览器 EventSource 原生）、
/// keep-alive 注释保活。路由在 auth_mw 之下，与全端点同保护。
async fn events_stream(
    Query(q): Query<EventsStreamQuery>,
    headers: HeaderMap,
    State(state): State<Arc<DaemonState>>,
) -> Response {
    let types = match parse_label_list(&q.event_type, &known_event_type_labels(), "type") {
        Ok(t) => t,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    let last_event_id = headers
        .get(header::HeaderName::from_static("last-event-id"))
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    let requested = q.after.or(last_event_id).unwrap_or(0);
    let hub = state.hub();
    let pred = move |env: &Envelope| {
        let task_ok = q
            .task_id
            .as_deref()
            .is_none_or(|want| env.event.task_id() == Some(want));
        let type_ok = types.is_empty()
            || types
                .iter()
                .any(|t| env.event.type_label().eq_ignore_ascii_case(t));
        task_ok && type_ok
    };

    // 缺口检测（连接时点快照口径）：目标区间已被冲掉 → 从缓冲最旧重放，
    // 并以注释行报警（客户端亦可按首帧 seq 回退自行判定）。
    let (cursor, gap) = if hub.gap_after(requested) {
        (
            hub.oldest_seq()
                .unwrap_or_else(|| hub.last_seq() + 1)
                .saturating_sub(1),
            true,
        )
    } else {
        (requested, false)
    };

    // 连接即重放历史（非破坏读；不节流，与 WS 首推同口径）。
    let mut pending = VecDeque::new();
    if gap {
        pending.push_back(SseEvent::default().comment(format!(
            "gap: events after {requested} were evicted, replaying from oldest buffered seq"
        )));
    }
    let mut cursor = cursor;
    let (replay, _) = hub.read_filtered(cursor, usize::MAX, &pred);
    for env in replay {
        cursor = env.seq;
        if let Some(ev) = envelope_to_sse(&env) {
            pending.push_back(ev);
        }
    }

    let stream = futures::stream::unfold(
        SseStreamState {
            state,
            cursor,
            pred: Box::new(pred),
            pending,
            throttler: Throttler::new(),
            last_flush: Instant::now(),
            tick: tokio::time::interval(SSE_POLL_INTERVAL),
        },
        |mut st| async move {
            loop {
                if let Some(ev) = st.pending.pop_front() {
                    return Some((Ok::<_, Infallible>(ev), st));
                }
                st.tick.tick().await;
                let (batch, _) = st.state.hub().read_filtered(st.cursor, SSE_BATCH, &st.pred);
                for env in batch {
                    st.cursor = env.seq;
                    if Throttler::is_throttlable(&env.event) {
                        st.throttler.upsert(env); // 合并，等 1s flush（与 WS 同口径）
                    } else if let Some(ev) = envelope_to_sse(&env) {
                        st.pending.push_back(ev);
                    }
                }
                if !st.throttler.is_empty() && st.last_flush.elapsed() >= Duration::from_secs(1) {
                    for env in st.throttler.drain_pending() {
                        if let Some(ev) = envelope_to_sse(&env) {
                            st.pending.push_back(ev);
                        }
                    }
                    st.last_flush = Instant::now();
                }
            }
        },
    );
    Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response()
}

async fn batch_tasks(
    State(state): State<Arc<DaemonState>>,
    axum::Json(body): axum::Json<BatchTaskReq>,
) -> impl IntoResponse {
    let action = match body.action.to_ascii_lowercase().as_str() {
        "pause" => BatchAction::Pause,
        "resume" => BatchAction::Resume,
        "remove" => BatchAction::Remove {
            delete_data: body.delete_data,
        },
        _ => return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("未知 action {:?}（合法值: pause, resume, remove）", body.action)
            })),
        )
            .into_response(),
    };
    // E19：ids 与 select 二选一
    if let Some(select) = body.select.as_ref() {
        if !body.ids.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "ids 与 select 只能二选一" })),
            )
                .into_response();
        }
        let q = match parse_batch_select(select) {
            Ok(q) => q,
            Err(msg) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": msg })),
                )
                    .into_response()
            }
        };
        return match state.batch_select(&q, action).await {
            Ok(outcome) => Json(outcome).into_response(),
            Err(e) => {
                let status = match e {
                    DaemonError::InvalidSource(_) => StatusCode::BAD_REQUEST,
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                };
                (status, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
            }
        };
    }
    if body.ids.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "ids 不能为空" })),
        )
            .into_response();
    }
    if body.ids.len() > BATCH_IDS_CAP {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("ids 数量超上限（{} > {BATCH_IDS_CAP}）", body.ids.len())
            })),
        )
            .into_response();
    }
    Json(state.batch(&body.ids, action).await).into_response()
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
            .route("/tasks/batch", post(batch_tasks))
            .route("/tasks/:id", get(task_snapshot).delete(remove_task))
            .route("/tasks/:id/pause", post(pause_task))
            .route("/tasks/:id/resume", post(resume_task))
            .route("/tasks/:id/logs", get(task_logs))
            .route("/tasks/:id/fallback", post(task_fallback))
            .route("/tasks/:id/limit", post(task_limit))
            .route("/tasks/:id/sequential", post(task_sequential))
            .route("/tasks/:id/proxy", post(task_set_proxy))
            .route("/tasks/:id/name", post(task_rename))
            .route("/tasks/:id/tags", post(task_set_tags))
            .route("/tasks/:id/files/priority", post(task_file_priority))
            .route("/tasks/:id/webseeds", post(task_webseeds))
            .route("/bt/metadata", post(bt_magnet_metadata))
            .route("/config", get(config_endpoint))
            .route("/config/limit", post(config_set_limit))
            .route("/stats", get(stats_endpoint))
            .route("/version", get(version_endpoint))
            .route("/health", get(health_endpoint))
            .route("/providers", get(providers))
            .route("/events", get(list_events))
            .route("/events/stream", get(events_stream))
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
