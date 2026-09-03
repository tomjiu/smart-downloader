//! DaemonState（M6 集成层）：任务目录 + HttpEngine + FallbackPolicy + WsHub；
//! add/pause/resume/remove/snapshot/list/provider 快照；重复 canonical → 409 事件。

use parking_lot::Mutex;
use smart_dl_core::identity::{CanonicalId, CanonicalKind, ContentIdentity};
use smart_dl_core::source_parse::normalize::{normalize_user_link, NormalizedSource};
use smart_dl_core::state_machine::TaskState;
use smart_dl_core::task::{DownloadTask, TaskId, TaskMetadata};
#[cfg(any(feature = "ftp", feature = "xunlei-import"))]
use smart_dl_core::task::{FileState, TaskFile};
use smart_dl_core::types::{
    Auth, DownloadEngine, DownloadSource, EngineKind, EngineState, EngineStatus, EngineTaskId,
    FileProgress,
};
use smart_dl_provider::{
    FallbackCoordinator, FallbackOutcome, HttpSink, ProviderError, ProviderRuntime, RemoteProvider,
    SinkError,
};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::events::SchedulerEvent;
use crate::ws::WsHub;

/// 任务记录（引擎句柄 + 引擎运行态缓存）。
#[derive(Clone)]
pub struct TaskRecord {
    pub task: DownloadTask,
    pub engine_tid: Option<EngineTaskId>,
    pub engine_kind: EngineKind,
    /// 引擎快照缓存（E11 起真实写入）：轮询器每轮对活跃任务整体写入
    /// `engine.status()` 结果——速率供 `/stats` 聚合、error 供 `task_logs`；
    /// 运行态字段不落盘（持久化排除），写缓存不触发 autosave。
    /// 非活跃（暂停/终态）时轮询不再光顾，速率由 pause/终态迁移清零防陈旧。
    pub engine_status: Option<EngineStatus>,
    /// 运行态操作日志（add/pause/resume/remove/restored；引擎状态变更不记——见快照）。
    events: Vec<TaskEvent>,
}

/// 任务操作日志条目（`GET /tasks/:id/logs` 返回）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct TaskEvent {
    /// Unix 毫秒时间戳。
    pub at_ms: u64,
    /// 操作名：add / pause / resume / remove / restored。
    pub op: String,
    pub detail: Option<String>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl TaskRecord {
    fn push_event(&mut self, op: &str, detail: Option<String>) {
        self.events.push(TaskEvent {
            at_ms: now_ms(),
            op: op.to_string(),
            detail,
        });
    }
}

/// 任务快照（GET /tasks/:id，跳号补拉入口）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct TaskSnapshot {
    pub task_id: String,
    /// 状态字符串（`Downloading(Http)` → `"Downloading"`；API 消费者无需解析枚举负载）。
    pub state: String,
    pub source: String,
    pub dest_root: PathBuf,
    pub engine: Option<String>,
    pub done: u64,
    pub total: u64,
    pub error: Option<String>,
    /// 文件级进度（实时读引擎 status().files；单文件/无文件引擎为空数组）。
    /// FTP 目录任务与 BT 多文件任务在此处行为一致（都从引擎状态链透出）。
    pub files: Vec<FileProgress>,
    /// 任务级限速配置（KiB/s；None = 未设置走全局）。set 语义见
    /// `DaemonState::set_task_limits`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<smart_dl_core::task::TaskLimits>,
    /// 任务名（E7 透出：E6 显式名 / FTP URL 派生 / xunlei import；None = 引擎
    /// 派生链未回填，序列化时省略）。与列表 `TaskSummary::name` 同口径。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// BT 子文件优先级表（None = 未设置走 libtorrent 默认 4；Some = 持久化
    /// 全量快照，下标 = 文件序）。set 语义见 `DaemonState::set_task_file_priorities`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_priorities: Option<Vec<u32>>,
    /// 顺序下载（边下边播）。set 语义见 `DaemonState::set_task_sequential`；
    /// false = 默认并行策略（不序列化，快照向后兼容）。
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub sequential: bool,
}

/// 列表条目。
#[derive(Clone, Debug, serde::Serialize)]
pub struct TaskSummary {
    pub task_id: String,
    /// 状态字符串（同上）。
    pub state: String,
    pub source: String,
    /// 引擎种类标签（E7：`http`/`bt`/`ftp`/`provider`/`xunlei-nas`）。建任务时即定，
    /// 恒有值——列表侧栏分组与 `?engine=` 过滤的回显依据。
    pub engine: &'static str,
    /// 任务名（E6 显式名 / FTP 单文件 URL 派生 / xunlei import；None = 引擎派生链
    /// （E4 CD → URL 末段）尚未回填，序列化时省略）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 列表过滤/分页查询（E7）。`states`/`engines` 空 = 不过滤；匹配均大小写不敏感。
/// `limit`/`offset` 由 HTTP 层校验（limit 1..=500，offset ≥ 0 由类型保证）后下推。
#[derive(Clone, Debug, Default)]
pub struct ListQuery {
    pub states: Vec<String>,
    pub engines: Vec<String>,
    pub limit: Option<usize>,
    pub offset: usize,
}

/// 合法状态标签全集（E7 `?state=` 校验依据；与 `state_label` 输出同步——
/// 显式列举全部 TaskState 变体，新增变体时编译期漏项由测试锁定）。
pub fn known_state_labels() -> Vec<String> {
    use smart_dl_core::state_machine::{EvalPhase, TaskState};
    use smart_dl_core::types::EngineKind;
    [
        TaskState::Queued,
        TaskState::Evaluating(EvalPhase::MetadataPending),
        TaskState::Evaluating(EvalPhase::PeerDiscovery),
        TaskState::Evaluating(EvalPhase::HeatEvaluating),
        TaskState::Downloading(EngineKind::Http),
        TaskState::Downloading(EngineKind::Bt),
        TaskState::Downloading(EngineKind::Ftp),
        TaskState::Downloading(EngineKind::Provider),
        TaskState::Downloading(EngineKind::XunleiNas),
        TaskState::Paused,
        TaskState::FallbackProvider,
        TaskState::Transferring,
        TaskState::Completed,
        TaskState::Stopped,
        TaskState::Seeding,
        TaskState::Failed,
    ]
    .iter()
    .map(state_label)
    .collect()
}

/// 合法引擎标签全集（E7 `?engine=` 校验依据；与 `kind_label` 输出同步）。
pub fn known_engine_labels() -> Vec<String> {
    [
        EngineKind::Bt,
        EngineKind::Http,
        EngineKind::Ftp,
        EngineKind::Provider,
        EngineKind::XunleiNas,
    ]
    .iter()
    .map(|k| kind_label(k).to_string())
    .collect()
}

/// 批量操作语义（E7 `POST /tasks/batch`）：逐任务独立执行，单项失败不短路。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchAction {
    Pause,
    Resume,
    /// 删除任务；`delete_data = true` 时引擎侧同步删除已下载数据。
    Remove {
        delete_data: bool,
    },
}

/// 批量操作单项结果（ok = false 时 error 带原因，如 `not found: t9`）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct BatchItemResult {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 批量操作汇总：`succeeded + failed == results.len()`（去重后）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct BatchOutcome {
    pub results: Vec<BatchItemResult>,
    pub succeeded: usize,
    pub failed: usize,
}

/// 全局统计（`GET /stats`）：任务按状态/引擎聚合 + 聚合速率。
/// 速率来自引擎快照缓存（`engine_status`，serve 装配 2s 轮询口径），非实时值；
/// 覆盖 HTTP/FTP（引擎侧增量采样）与 BT（FFI 实时值）的活跃任务
/// （Downloading/Seeding/Queued），暂停/终态速率清零。
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct DaemonStats {
    /// 任务总数。
    pub total: usize,
    /// 按状态聚合（键同 `TaskSummary.state` 口径，如 `Downloading`/`Paused`）。
    pub by_state: std::collections::BTreeMap<String, usize>,
    /// 按引擎种类聚合（bt/http/ftp/provider/xunlei-nas）。
    pub by_engine: std::collections::BTreeMap<&'static str, usize>,
    /// 聚合下行速率（B/s）。
    pub down_bytes_s: u64,
    /// 聚合上行速率（B/s；仅 BT 等双向引擎非零）。
    pub up_bytes_s: u64,
}

/// 引擎种类 → 统计标签（`/stats` by_engine 键；与引擎 `id()` 不同，
/// 这里是稳定的分类口径，不随引擎实例变化）。
fn kind_label(k: &EngineKind) -> &'static str {
    match k {
        EngineKind::Bt => "bt",
        EngineKind::Http => "http",
        EngineKind::Ftp => "ftp",
        EngineKind::Provider => "provider",
        EngineKind::XunleiNas => "xunlei-nas",
    }
}

/// 快照用状态标签：取枚举 Debug 的变体名部分。
pub fn state_label(s: &TaskState) -> String {
    let d = format!("{s:?}");
    d.split('(').next().unwrap_or(&d).to_string()
}

/// BT alert 应用结果（task_id + 状态迁移 + 消息），供事件广播使用。
#[cfg(feature = "bt")]
#[derive(Clone, Debug)]
pub struct BtAlertEffect {
    pub task_id: String,
    pub from: TaskState,
    pub to: TaskState,
    pub message: String,
}

/// HTTP 轮询推进结果（task_id + 状态迁移 + 消息），供事件广播使用。
#[derive(Clone, Debug)]
pub struct HttpPollEffect {
    pub task_id: String,
    pub from: TaskState,
    pub to: TaskState,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("duplicate task (existing: {0})")]
    Duplicate(String),
    #[error("task not found: {0}")]
    NotFound(String),
    #[error("engine error: {0}")]
    Engine(String),
    #[error("invalid source: {0}")]
    InvalidSource(String),
    /// 运行态操作与任务引擎种类不匹配（如给非 BT 任务注入 web seed）→ HTTP 409。
    #[error("不支持的操作: {0}")]
    UnsupportedOp(String),
    #[error("持久化: {0}")]
    Persist(String),
    #[error("云兜底: {0}")]
    Fallback(String),
}

#[cfg(feature = "xunlei-import")]
impl From<anyhow::Error> for DaemonError {
    fn from(value: anyhow::Error) -> Self {
        DaemonError::Engine(value.to_string())
    }
}

/// HTTP 任务创建参数（E6）：daemon API → 引擎能力的对齐收口。
/// 散参签名在 sequential/proxy 之后已到极限（E5 时 4 参），headers/auth/
/// 校验目标/备用源/显式名继续散参不可维护 → 收敛为结构体。
/// `add_link_task_opts` 复用本结构：magnet/ftp 分支仅取 `sequential`
/// （其余字段对非 HTTP 任务无语义，与 AddTaskReq 一字段多引擎口径一致）。
#[derive(Clone, Debug, Default)]
pub struct AddHttpOpts {
    /// 顺序下载（HTTP = 在飞段窗口收紧；BT = sequential flag）。
    pub sequential: bool,
    /// 任务级代理 URL（E5：Some = 任务专用 client 覆盖全局；非法 add 即拒）。
    pub proxy: Option<String>,
    /// 任务级自定义请求头（H-8 全链透传：探测 + 段下载）。
    pub headers: Vec<(String, String)>,
    /// HTTP Basic 认证（username 必填，password 可空串）。
    pub basic_auth: Option<(String, String)>,
    /// 主源内容校验目标（64 位十六进制 sha256）。传入后校验失败走既有处置链
    /// （重下 1 次 → 备用源 → 隔离试错轮换 → 降级，E3）。
    pub sha256: Option<String>,
    /// 备用源 URL（主源探测/校验失败兕底，E2/E3）。http(s):// 前缀校验同主源。
    pub backup_url: Option<String>,
    /// 备用源 md5 校验目标（32 位十六进制）。必须与 backup_url 成对（单独给 md5
    /// 无处安放）；主源校验失败切备用源时由引擎既有身份切换逻辑接管。
    pub backup_md5: Option<String>,
    /// 用户显式落盘名（V3 语义：非法即拒；None = 引擎派生链 E4：CD → URL 末段 → 兕底）。
    pub name: Option<String>,
}

/// 校验和归一：小写化（引擎端 sha256/md5 摘要格式化为小写 hex，入参大写需归一后参与比较）。
fn normalize_digest(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

fn is_hex_digest(s: &str, len: usize) -> bool {
    s.len() == len && s.chars().all(|c| c.is_ascii_hexdigit())
}

impl AddHttpOpts {
    /// 入参校验（add 入队前，错误定性 InvalidSource → 400）：
    /// 校验和格式 / 备用源前缀 / header 名值合法性 / 显式名 V3 终审。
    fn validate(&self) -> Result<(), String> {
        if let Some(s) = &self.sha256 {
            let s = normalize_digest(s);
            if !is_hex_digest(&s, 64) {
                return Err(format!("sha256 必须是 64 位十六进制: {s:?}"));
            }
        }
        if let Some(m) = &self.backup_md5 {
            if self.backup_url.is_none() {
                return Err("backup_md5 必须与 backup_url 成对提供".into());
            }
            let m = normalize_digest(m);
            if !is_hex_digest(&m, 32) {
                return Err(format!("backup_md5 必须是 32 位十六进制: {m:?}"));
            }
        }
        if let Some(u) = &self.backup_url {
            if !u.starts_with("http://") && !u.starts_with("https://") {
                return Err(format!("backup_url 仅支持 http(s)://: {u:?}"));
            }
        }
        for (k, v) in &self.headers {
            if k.is_empty() || k.contains(':') || k.contains('\r') || k.contains('\n') {
                return Err(format!("header 名非法: {k:?}"));
            }
            if v.contains('\r') || v.contains('\n') {
                return Err(format!("header 值不得含换行: {k:?}"));
            }
        }
        if let Some(n) = &self.name {
            // V3 终审提前：引擎同函数拒，这里先拒避免错误信息隔着 Engine 包装
            smart_dl_core::session::output::sanitize_rel(n)
                .map_err(|e| format!("name 非法: {e}"))?;
        }
        Ok(())
    }
}

/// 守护进程状态：任务 + 引擎表 + 事件中枢。
pub struct DaemonState {
    engines: HashMap<EngineKind, Arc<dyn DownloadEngine>>,
    hub: WsHub,
    tasks: Mutex<HashMap<TaskId, TaskRecord>>,
    providers: Vec<Arc<dyn RemoteProvider>>,
    next_id: AtomicU64,
    /// 任务持久化文件（Some 时 add/remove/状态变更后自动落盘）。
    persist_path: Option<PathBuf>,
    /// HTTP 任务默认落盘目录（dest 未指定时用；serve 从配置 `[download] dest_root` 注入；
    /// Mutex 支持 #6 TOML 热重载动态更新）。
    default_dest_root: Mutex<PathBuf>,
    /// 安全修复（V2）：dest 白名单根目录。空 = 兜底用 default_dest_root
    /// （保持未注入时的测试/默认行为）；serve 注入 [dest_root]，热重载跟随更新。
    allowed_roots: Mutex<Vec<PathBuf>>,
    /// 安全修复（V1/V13）：HTTP API Bearer token。None/空 = 未配置（serve 保证
    /// 非回环监听时拒绝启动，回环监听放行兼容本机 CLI）；Some = 全端点强制校验。
    http_token: Option<String>,
    /// 安全修复（V10-2）：磁盘预检严格模式（true = 空间不可探测时拒绝入队）。
    /// 启动时由 `[download] disk_precheck_strict` 注入，不参与热重载。
    disk_precheck_strict: bool,
    /// 生效配置快照（`GET /config` 返回；serve 注入精简字段；热重载后刷新）。
    config_snapshot: Mutex<Option<serde_json::Value>>,
    /// 子文件优先级待重放集合（task_id）。恢复时 metadata 未就绪（magnet）
    /// 挂入；就绪后由 replay 循环下发并移除；任务移除/引擎不支持时清理。
    pending_file_prio: Mutex<HashSet<TaskId>>,
}

/// 持久化任务记录：`task`（含 source 原文：url/magnet/torrent 字节）+ 引擎种类。
/// 运行态字段（engine_tid/engine_status）不落盘——恢复时重新向引擎 add。
/// `paused`（P4 G5）：用户暂停意图——重启后保持暂停而非重新入队自动开跑。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PersistedTask {
    pub task: DownloadTask,
    pub engine_kind: EngineKind,
    #[serde(default)]
    pub paused: bool,
}

/// 原子写任务文件（tmp + rename，防半写）。
/// 安全修复（V12，CWE-312/732）：PersistedTask 含完整 source（可能带凭据的 URL/headers），
/// 落盘必须 0600（rename 保留权限位）；存量宽松权限文件在下次写入时被收紧。
pub fn write_tasks_atomic(path: &Path, tasks: &[PersistedTask]) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(tasks).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(md) = std::fs::metadata(path) {
            let mode = md.permissions().mode() & 0o777;
            if mode != 0o600 {
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
    Ok(())
}

/// 单文件 .torrent 元数据（迅雷导入用）。
#[derive(Debug, Clone)]
pub struct TorrentMeta {
    pub info_hash: String,
    pub piece_length: u32,
    pub pieces_hash: Vec<[u8; 20]>,
    pub name: String,
    /// 单文件大小（仅单文件 torrent 使用）。
    pub file_size: u64,
    /// 多文件列表（仅多文件 torrent 使用）。
    pub files: Vec<FileMeta>,
}

/// 单文件元数据。
#[derive(Debug, Clone)]
pub struct FileMeta {
    /// 相对路径（多文件）或文件名（单文件）。
    pub path: String,
    /// 文件大小（字节）。
    pub size: u64,
    /// 该文件在 torrent 中的起始 piece 索引。
    pub piece_offset: usize,
    /// 该文件占用的 piece 数量。
    pub piece_count: usize,
}

#[cfg(feature = "bt")]
impl TorrentMeta {
    /// 从 .torrent 字节解析元数据（单文件/多文件）。
    pub fn parse(b: &[u8]) -> Result<Self, DaemonError> {
        use sha1::Digest;
        let (info_start, info_end) = locate_info(b).ok_or_else(|| {
            DaemonError::InvalidSource(".torrent 解析失败：无法定位 info dict".into())
        })?;

        let info_hash = {
            let digest = sha1::Sha1::digest(&b[info_start..=info_end]);
            digest
                .iter()
                .map(|x| format!("{x:02x}"))
                .collect::<String>()
        };

        let mut piece_length = 0u32;
        let mut pieces_hash = Vec::new();
        let mut name = String::new();
        let mut file_size = 0u64;
        let mut has_length = false;
        let mut files = Vec::new();
        let mut has_files = false;

        let mut i = info_start + 1; // skip 'd'
        let end = info_end;
        while i < end {
            let (key, after_key) = be_str(b, i)
                .ok_or_else(|| DaemonError::InvalidSource(".torrent info dict 解析失败".into()))?;
            i = after_key;

            match key {
                b"piece length" => {
                    piece_length = be_int(b, i)
                        .ok_or_else(|| DaemonError::InvalidSource("piece length 解析失败".into()))?
                        as u32;
                    i = value_skip(b, i, 0).ok_or_else(|| {
                        DaemonError::InvalidSource(".torrent info dict 解析失败".into())
                    })?;
                }
                b"pieces" => {
                    let pieces_data = be_str(b, i)
                        .ok_or_else(|| DaemonError::InvalidSource("pieces 解析失败".into()))?;
                    if pieces_data.0.len() % 20 != 0 {
                        return Err(DaemonError::InvalidSource(
                            "pieces 长度不是 20 的倍数".into(),
                        ));
                    }
                    pieces_hash = pieces_data.0.as_chunks::<20>().0.to_vec();
                    i = value_skip(b, i, 0).ok_or_else(|| {
                        DaemonError::InvalidSource(".torrent info dict 解析失败".into())
                    })?;
                }
                b"name" => {
                    name = String::from_utf8_lossy(
                        be_str(b, i)
                            .ok_or_else(|| DaemonError::InvalidSource("name 解析失败".into()))?
                            .0,
                    )
                    .into_owned();
                    // 安全修复（V3）：torrent 根名直通 dest_root.join，恶意 name
                    // （../、绝对路径）即任意文件写——parse 层即拒任务。
                    smart_dl_core::session::output::sanitize_rel(&name).map_err(|_| {
                        DaemonError::InvalidSource(format!(
                            ".torrent name 含非法路径分量已拒绝: {name}"
                        ))
                    })?;
                    i = value_skip(b, i, 0).ok_or_else(|| {
                        DaemonError::InvalidSource(".torrent info dict 解析失败".into())
                    })?;
                }
                b"length" => {
                    file_size = be_int(b, i)
                        .ok_or_else(|| DaemonError::InvalidSource("length 解析失败".into()))?
                        as u64;
                    has_length = true;
                    i = value_skip(b, i, 0).ok_or_else(|| {
                        DaemonError::InvalidSource(".torrent info dict 解析失败".into())
                    })?;
                }
                b"files" => {
                    has_files = true;
                    // files value 是 list（l...e）
                    let list_end = list_skip(b, i, 0)
                        .ok_or_else(|| DaemonError::InvalidSource("files 解析失败".into()))?;
                    files = parse_file_list(&b[i + 1..list_end], piece_length)?;
                    i = list_end + 1; // 跳过 list 的闭合 'e'
                }
                _ => {
                    i = value_skip(b, i, 0).ok_or_else(|| {
                        DaemonError::InvalidSource(".torrent info dict 解析失败".into())
                    })?;
                }
            }
        }

        if piece_length == 0 || pieces_hash.is_empty() {
            return Err(DaemonError::InvalidSource(
                ".torrent 缺少必要字段 (piece length/pieces)".into(),
            ));
        }

        if has_files {
            // 多文件 torrent：files 数组已解析
            Ok(Self {
                info_hash,
                piece_length,
                pieces_hash,
                name,
                file_size: 0,
                files,
            })
        } else {
            // 单文件 torrent
            if !has_length {
                return Err(DaemonError::InvalidSource(
                    ".torrent 缺少 length 字段".into(),
                ));
            }
            Ok(Self {
                info_hash,
                piece_length,
                pieces_hash,
                name,
                file_size,
                files: vec![],
            })
        }
    }
}

/// 解析多文件 torrent 的 files 列表内容（bencode，`l`/`e` 已剥离）。
#[cfg(feature = "bt")]
fn parse_file_list(data: &[u8], piece_length: u32) -> Result<Vec<FileMeta>, DaemonError> {
    let mut files = Vec::new();
    let mut pos = 0usize;
    let plen = piece_length as u64;

    while pos < data.len() {
        if data.get(pos) != Some(&b'd') {
            pos = value_skip(data, pos, 0)
                .ok_or_else(|| DaemonError::InvalidSource("files 列表解析失败".into()))?;
            continue;
        }
        let dict_end = dict_skip(data, pos, 0)
            .ok_or_else(|| DaemonError::InvalidSource("files dict 解析失败".into()))?;
        let file_dict = &data[pos..=dict_end];

        let mut path = String::new();
        let mut length = 0u64;
        let mut j = 1;
        while j < file_dict.len() - 1 {
            let (key, after_key) = be_str(file_dict, j)
                .ok_or_else(|| DaemonError::InvalidSource("files dict key 解析失败".into()))?;
            j = after_key;
            match key {
                b"length" => {
                    length = be_int(file_dict, j)
                        .ok_or_else(|| DaemonError::InvalidSource("files length 解析失败".into()))?
                        as u64;
                    j = value_skip(file_dict, j, 0).ok_or_else(|| {
                        DaemonError::InvalidSource("files dict value 解析失败".into())
                    })?;
                }
                b"path" => {
                    // path value 是 list（l...e）
                    let path_list_end = list_skip(file_dict, j, 0).ok_or_else(|| {
                        DaemonError::InvalidSource("files path list 解析失败".into())
                    })?;
                    path = parse_path_list(&file_dict[j + 1..path_list_end])?;
                    j = path_list_end + 1;
                }
                _ => {
                    j = value_skip(file_dict, j, 0).ok_or_else(|| {
                        DaemonError::InvalidSource("files dict value 解析失败".into())
                    })?;
                }
            }
        }

        if length > 0 && !path.is_empty() {
            // 计算 piece 偏移和数量（按文件在 torrent 中的累计字节偏移）
            let total_size: u64 = files.iter().map(|f: &FileMeta| f.size).sum();
            let piece_offset = (total_size / plen) as usize;
            let piece_count = length.div_ceil(plen) as usize;
            files.push(FileMeta {
                path,
                size: length,
                piece_offset,
                piece_count,
            });
        }

        pos = dict_end + 1;
    }

    Ok(files)
}

/// 解析 path list 内容（bencode，`l`/`e` 已剥离）为路径字符串。
/// 安全修复（V3）：逐段净化——拒 `..` / 绝对路径段，恶意种子不得写出 dest_root。
#[cfg(feature = "bt")]
fn parse_path_list(data: &[u8]) -> Result<String, DaemonError> {
    let mut parts = Vec::new();
    let mut p = 0usize;
    while p < data.len() {
        let (seg, after) = be_str(data, p)
            .ok_or_else(|| DaemonError::InvalidSource("path segment 解析失败".into()))?;
        let seg_str = String::from_utf8_lossy(seg).into_owned();
        if seg_str == ".." || seg_str.contains('/') || seg_str.contains('\\') || seg_str.is_empty()
        {
            return Err(DaemonError::InvalidSource(format!(
                "files path 含非法段已拒绝: {seg_str}"
            )));
        }
        parts.push(seg_str);
        p = after;
    }
    if parts.is_empty() {
        return Err(DaemonError::InvalidSource("files path 为空".into()));
    }
    Ok(parts.join(std::path::MAIN_SEPARATOR_STR))
}

impl DaemonState {
    /// 单引擎构造（HTTP）；BT 引擎用 `with_bt` 追加（feature `bt`）。
    pub fn new(engine: Arc<dyn DownloadEngine>, providers: Vec<Arc<dyn RemoteProvider>>) -> Self {
        let mut engines = HashMap::new();
        engines.insert(engine.kind(), engine);
        DaemonState {
            engines,
            hub: WsHub::new(),
            tasks: Mutex::new(HashMap::new()),
            providers,
            next_id: AtomicU64::new(1),
            persist_path: None,
            default_dest_root: Mutex::new(PathBuf::from(".")),
            allowed_roots: Mutex::new(Vec::new()),
            http_token: None,
            disk_precheck_strict: false,
            config_snapshot: Mutex::new(None),
            pending_file_prio: Mutex::new(HashSet::new()),
        }
    }

    /// 注入 HTTP 任务默认落盘目录（dest 未指定时使用；serve 从 `[download] dest_root` 传入）。
    /// 同时把该目录加入 dest 白名单（V2）——默认白名单 = [dest_root]。
    pub fn with_dest_root(self, default_dest_root: PathBuf) -> Self {
        *self.default_dest_root.lock() = default_dest_root.clone();
        let mut roots = self.allowed_roots.lock();
        if !roots.contains(&default_dest_root) {
            roots.push(default_dest_root);
        }
        drop(roots);
        self
    }

    /// 读取 HTTP 任务默认落盘目录（V15：`POST /bt/metadata` 的 `save_to`
    /// 落盘白名单根——save_to 必须落在该目录内）。
    pub fn default_dest_root(&self) -> PathBuf {
        self.default_dest_root.lock().clone()
    }

    /// 注入 HTTP API Bearer token（V1/V13）：Some = 全端点（含 /ws 握手）强制
    /// `Authorization: Bearer <token>`；None = 未配置（serve 已保证非回环监听拒绝启动）。
    pub fn with_http_token(mut self, token: Option<String>) -> Self {
        self.http_token = token.filter(|t| !t.is_empty());
        self
    }

    /// 注入磁盘预检严格模式（V10-2）：true = 空间不可探测时拒绝入队。
    pub fn with_disk_precheck_strict(mut self, strict: bool) -> Self {
        self.disk_precheck_strict = strict;
        self
    }

    /// 生效的 dest 白名单（V2）：未显式注入时兜底 default_dest_root。
    fn dest_roots(&self) -> Vec<PathBuf> {
        let g = self.allowed_roots.lock();
        if g.is_empty() {
            vec![self.default_dest_root.lock().clone()]
        } else {
            g.clone()
        }
    }

    /// 校验 HTTP 请求 Bearer token（安全修复 V1/V13）：
    /// - 未配置 token（None）→ 放行（serve 已保证该模式仅回环监听可达）；
    /// - 已配置 → `Authorization: Bearer <token>` 必须精确匹配，否则 false
    ///   （比较走 `ct_eq` 常量时间路径，第六轮 9.3.4）。
    ///
    /// 覆盖全部路由含 /ws 升级握手（同一 Router layer）。
    pub fn verify_http_token(&self, authorization: Option<&str>) -> bool {
        match self.http_token.as_deref() {
            None | Some("") => true,
            Some(expect) => authorization
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(|t| ct_eq(t, expect))
                .unwrap_or(false),
        }
    }

    /// 注入生效配置快照（`GET /config` 返回；serve 组装精简字段）。
    pub fn with_config(self, snapshot: serde_json::Value) -> Self {
        *self.config_snapshot.lock() = Some(snapshot);
        self
    }

    /// 启用任务持久化（每次变更自动写 JSON 到 `path`）。
    pub fn with_storage(mut self, path: PathBuf) -> Self {
        self.persist_path = Some(path);
        self
    }

    /// 追加 BT 引擎（feature `bt`；无该引擎时 magnet 路由 → InvalidSource）。
    #[cfg(feature = "bt")]
    pub fn with_bt(mut self, bt: Arc<dyn DownloadEngine>) -> Self {
        self.engines.insert(EngineKind::Bt, bt);
        self
    }

    /// 追加 FTP 引擎（feature `ftp`；FTP 链接路由到该引擎）。
    /// 独立占用 `EngineKind::Ftp` 槽位——不覆盖 Http 槽，保证 HTTP 任务仍走 HttpEngine。
    #[cfg(feature = "ftp")]
    pub fn with_ftp(mut self, ftp: Arc<dyn DownloadEngine>) -> Self {
        self.engines.insert(EngineKind::Ftp, ftp);
        self
    }

    /// 序列化当前任务目录（持久化用）。`paused` 取自任务缓存态
    /// （pause/resume 处理器同步改写并 autosave，落盘时态准确）。
    fn persisted_tasks(&self) -> Vec<PersistedTask> {
        self.tasks
            .lock()
            .values()
            .map(|r| PersistedTask {
                task: r.task.clone(),
                engine_kind: r.engine_kind,
                paused: matches!(r.task.state, TaskState::Paused),
            })
            .collect()
    }

    /// 自动落盘（启用 storage 时）。同步原子写：任务变更低频（add/remove/状态迁移），
    /// 必须保证顺序（异步并发写会竞态覆盖旧快照）；JSON 规模小，阻塞代价可忽略。
    fn autosave(&self) {
        let Some(path) = self.persist_path.clone() else {
            return;
        };
        let data = self.persisted_tasks();
        if let Err(e) = write_tasks_atomic(&path, &data) {
            tracing::warn!("任务持久化失败 {path:?}: {e}");
        }
    }

    /// 从持久化文件恢复任务：逐条重新 add 到引擎（保留原 task_id，
    /// next_id 推进），add 失败的任务标 Failed 保留记录。返回恢复条数。
    pub async fn restore_from(&self, path: &Path) -> Result<usize, DaemonError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| DaemonError::Persist(format!("读取 {path:?} 失败: {e}")))?;
        let pts: Vec<PersistedTask> = serde_json::from_str(&text)
            .map_err(|e| DaemonError::Persist(format!("解析 {path:?} 失败: {e}")))?;
        let mut restored = 0usize;
        let mut failed = 0usize;
        for pt in pts {
            let mut t = pt.task.clone();
            let was_paused = pt.paused; // 用户暂停意图（P4 G5，旧文件无此字段 = false）
            t.state = TaskState::Queued; // 重启后重新入队
            let engine = match self.engine_for(pt.engine_kind) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("恢复任务 {} 引擎不可用: {e}", t.id);
                    continue;
                }
            };
            match engine.add(&t).await {
                Ok(tid) => {
                    // 恢复期重放（best-effort）：持久化的任务级配置在恢复后原样
                    // 下发引擎，单项失败仅记事件不阻断恢复（任务可用性优先）。
                    let mut replay_details: Vec<String> = Vec::new();
                    // ① 限速重放：原样传合并配置（BT 引擎 None 方向=不限的
                    // 全量快照语义；HTTP 引擎 None up=no-op 不触发方向预拒）。
                    if let Some(l) = t.limits.clone().filter(|l| !l.is_empty()) {
                        if let Err(e) = engine.set_limits(&tid, l.down_kb_s, l.up_kb_s).await {
                            replay_details.push(format!("限速重放失败: {e}"));
                        }
                    }
                    // ② 子文件优先级重放（仅 BT 任务；非 BT 引擎 Unsupported →
                    // 记事件）。magnet 恢复时 metadata 未就绪（引擎 NotFound）→
                    // 挂 pending 集合，由重放循环在就绪后收敛；.torrent 任务
                    // add 时 metadata 已就绪，此处直接成功。
                    if pt.engine_kind == EngineKind::Bt {
                        if let Some(prios) = t.file_priorities.clone().filter(|p| !p.is_empty()) {
                            let pairs: Vec<(usize, u32)> =
                                prios.iter().enumerate().map(|(i, p)| (i, *p)).collect();
                            match engine.set_file_priorities(&tid, &pairs).await {
                                Ok(()) => {}
                                Err(smart_dl_core::types::EngineError::NotFound) => {
                                    self.pending_file_prio.lock().insert(t.id.clone());
                                    replay_details
                                        .push("子文件优先级待 metadata 就绪后重放".into());
                                }
                                Err(e) => {
                                    replay_details.push(format!("子文件优先级重放失败: {e}"));
                                }
                            }
                        }
                    }
                    // ③ 顺序下载重放：sequential=true 原样下发（BT=handle 级
                    // flag 即时；HTTP=字段改写，下一重下轮拾取；不支持引擎记
                    // 事件不阻断恢复）。flag 幂等，与 add 时下发叠加无副作用。
                    if t.sequential {
                        if let Err(e) = engine.set_sequential(&tid, true).await {
                            replay_details.push(format!("顺序下载重放失败: {e}"));
                        }
                    }
                    // ④ 暂停意图重放 + 运行态恢复（P4 G5）：
                    // - was_paused → engine.pause：BT（内核暂停 + 意图登记持续压制
                    //   + fastresume）；HTTP（暂停标志置位，循环段边界退出）。
                    //   记录态同步回写 Paused（否则缓存显示 Queued 与内核错位）。
                    // - 非 paused 且 BT → engine.resume：所有 add 路径内核侧强制
                    //   paused（lt_kernel 统一语义），不 resume 则恢复任务永不下载。
                    //   HTTP add 已自启下载循环（epoch 语义），不得重复 resume。
                    if was_paused {
                        if let Err(e) = engine.pause(&tid).await {
                            replay_details.push(format!("暂停意图重放失败: {e}"));
                        }
                    } else if pt.engine_kind == EngineKind::Bt {
                        if let Err(e) = engine.resume(&tid).await {
                            replay_details.push(format!("恢复运行重放失败: {e}"));
                        }
                    }
                    let mut rec = TaskRecord {
                        task: t,
                        engine_tid: Some(tid),
                        engine_kind: pt.engine_kind,
                        engine_status: None,
                        events: vec![],
                    };
                    if was_paused {
                        rec.task.state = TaskState::Paused;
                    }
                    if replay_details.is_empty() {
                        rec.push_event("restored", None);
                    } else {
                        rec.push_event("restored", Some(replay_details.join("; ")));
                    }
                    self.tasks.lock().insert(rec.task.id.clone(), rec);
                    restored += 1;
                }
                Err(e) => {
                    tracing::warn!("恢复任务 {} 引擎 add 失败（标 Failed）: {e}", t.id);
                    t.state = TaskState::Failed;
                    let mut rec = TaskRecord {
                        task: t,
                        engine_tid: None,
                        engine_kind: pt.engine_kind,
                        engine_status: None,
                        events: vec![],
                    };
                    rec.push_event("restored", Some(format!("引擎 add 失败: {e}")));
                    self.tasks.lock().insert(rec.task.id.clone(), rec);
                    failed += 1;
                }
            }
        }
        // next_id 推进到已用最大值之后（保留原 task_id 的关键）
        let max_id = self
            .tasks
            .lock()
            .keys()
            .filter_map(|k| k.strip_prefix('t').and_then(|s| s.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        self.next_id.fetch_max(max_id + 1, Ordering::SeqCst);
        tracing::info!("任务恢复完成: {restored} 恢复, {failed} 失败（引擎 add 错误）");
        Ok(restored)
    }

    fn engine_for(&self, kind: EngineKind) -> Result<Arc<dyn DownloadEngine>, DaemonError> {
        self.engines.get(&kind).cloned().ok_or_else(|| {
            DaemonError::InvalidSource(format!("引擎未加载: {:?}（编译时启用对应 feature）", kind))
        })
    }

    /// 活跃 BT 任务的 engine_tid 列表（fastresume 周期/退出保存范围，P4 G4）。
    /// 非终态即保存（Queued/Evaluating/Downloading/Paused/FallbackProvider/
    /// Transferring/Seeding——做种中也保存，防"部分校验进度丢失"）；
    /// Completed/Failed/Stopped 跳过。
    #[cfg(feature = "bt")]
    pub fn active_bt_tids(&self) -> Vec<String> {
        self.tasks
            .lock()
            .values()
            .filter(|r| r.engine_kind == EngineKind::Bt)
            .filter(|r| {
                !matches!(
                    r.task.state,
                    TaskState::Completed | TaskState::Failed | TaskState::Stopped
                )
            })
            .filter_map(|r| r.engine_tid.clone())
            .collect()
    }

    /// 任务当前 engine_tid（引擎侧 id；BT=infohash）。未注册/任务不存在 → None。
    pub fn engine_tid_of(&self, id: &str) -> Option<String> {
        self.tasks.lock().get(id).and_then(|r| r.engine_tid.clone())
    }

    pub fn hub(&self) -> &WsHub {
        &self.hub
    }

    /// 添加任务入口：支持 http/https/thunder:///qqdl:// 链接（归一化后走 HTTP 引擎）；
    /// magnet（feature `bt` 时走 libtorrent 引擎）；ed2k/无法识别 → InvalidSource。
    pub async fn add_link_task(
        &self,
        link: String,
        dest_root: Option<String>,
    ) -> Result<TaskId, DaemonError> {
        self.add_link_task_opts(link, dest_root, AddHttpOpts::default())
            .await
    }

    /// 顺序下载变体：`sequential` 写入任务（HTTP=在飞窗口；BT=sequential
    /// flag；其余引擎忽略）。引擎 add 后对 BT 任务立即下发（handle 级 flag，
    /// metadata 未就绪也可设）。
    /// 任务级代理（E5）：仅 HTTP 任务生效；magnet/ed2k 等任务忽略该字段。
    /// 链接任务创建（E6 opts 收口）：HTTP 分支整体透传 `AddHttpOpts`；
    /// magnet 分支仅取 `sequential`（其余字段对 BT 无语义，静默忽略）；
    /// ed2k/ftp/xunlei 分支不受影响。
    pub async fn add_link_task_opts(
        &self,
        link: String,
        dest_root: Option<String>,
        opts: AddHttpOpts,
    ) -> Result<TaskId, DaemonError> {
        match normalize_user_link(&link) {
            NormalizedSource::Http(real) => self.add_http_task_opts(real, dest_root, opts).await,
            NormalizedSource::Magnet(m) => {
                #[cfg(feature = "bt")]
                {
                    return self.add_bt_task_opts(m, dest_root, opts.sequential).await;
                }
                #[cfg(not(feature = "bt"))]
                {
                    let _ = opts.sequential;
                    Err(DaemonError::InvalidSource(format!(
                        "magnet 需 BT 引擎（编译时启用 --features daemon/bt）: {m}"
                    )))
                }
            }
            NormalizedSource::Ed2k(e) => {
                Err(DaemonError::InvalidSource(format!("ed2k 不支持: {e}")))
            }
            NormalizedSource::Ftp(u) => {
                #[cfg(feature = "ftp")]
                {
                    return self.add_ftp_task(u, dest_root).await;
                }
                #[cfg(not(feature = "ftp"))]
                {
                    Err(DaemonError::InvalidSource(format!(
                        "ftp 需 FTP 引擎（编译时启用 --features ftp）: {u}"
                    )))
                }
            }
            NormalizedSource::XunleiShare(u) => Err(DaemonError::InvalidSource(format!(
                "迅雷网盘分享暂不支持直接导入: {u}"
            ))),
            NormalizedSource::Unsupported(orig) => Err(DaemonError::InvalidSource(format!(
                "无法识别的链接: {orig}"
            ))),
        }
    }

    /// 添加 BT 任务（feature `bt`，顺序下载 opts 直通入口）：btih canonical 查重 → 引擎 add → TaskCreated 事件。
    #[cfg(feature = "bt")]
    async fn add_bt_task_opts(
        &self,
        magnet: String,
        dest_root: Option<String>,
        sequential: bool,
    ) -> Result<TaskId, DaemonError> {
        // B10：目标目录预检（创建/可写）；magnet 总大小元数据前未知 → 空间预检跳过
        // dest 未指定 → 默认落盘目录（与 HTTP 一致：default_dest_root 配置）
        let def = self.default_dest_root.lock().to_string_lossy().into_owned();
        let dest_root = ensure_dest_root(dest_root.or(Some(def)), &self.dest_roots())?;
        let canonical = CanonicalId {
            kind: CanonicalKind::Bt,
            identity: btih_of(&magnet).unwrap_or_else(|| magnet.clone()),
            validator: None,
            token_sensitive: false,
        };
        let task_id = format!("t{}", self.next_id.fetch_add(1, Ordering::SeqCst));

        // 查重（canonical 一致 → DuplicateRejected）
        {
            let tasks = self.tasks.lock();
            for (existing, rec) in tasks.iter() {
                if rec.task.canonical_id == canonical {
                    self.hub.publish(SchedulerEvent::DuplicateRejected {
                        task_id: task_id.clone(),
                        existing: existing.clone(),
                    });
                    return Err(DaemonError::Duplicate(existing.clone()));
                }
            }
        }

        let task = DownloadTask {
            id: task_id.clone(),
            canonical_id: canonical,
            source: DownloadSource::Magnet(magnet.clone()),
            identity: ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                backup_md5: None,
            },
            dest_root: dest_root.clone(),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential,
            metadata: TaskMetadata {
                name: None,
                added_at_unix: 0,
            },
            limits: None,
        };

        let engine_tid = self
            .engine_for(EngineKind::Bt)?
            .add(&task)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        // 顺序下载立即下发（handle 级 flag，metadata 未就绪也可设；
        // 失败不回滚任务，恢复重放 + set_sequential 端点可补）。
        if sequential {
            let engine = self.engine_for(EngineKind::Bt)?;
            if let Err(e) = engine.set_sequential(&engine_tid, true).await {
                tracing::warn!("BT 任务 {task_id} 顺序下载 flag 下发失败: {e}");
            }
        }
        let mut rec = TaskRecord {
            task,
            engine_tid: Some(engine_tid),
            engine_kind: EngineKind::Bt,
            engine_status: None,
            events: vec![],
        };
        rec.push_event("add", None);
        self.tasks.lock().insert(task_id.clone(), rec);
        self.autosave();
        self.hub.publish(SchedulerEvent::TaskCreated {
            task_id: task_id.clone(),
        });
        self.hub.publish(SchedulerEvent::StateChanged {
            task_id: task_id.clone(),
            from: TaskState::Queued,
            to: TaskState::Downloading(EngineKind::Bt),
        });
        Ok(task_id)
    }

    /// 添加 .torrent 文件任务（feature `bt`）：infohash canonical 查重 → 引擎
    /// add_torrent_file → TaskCreated 事件。torrent 字节来自 API base64 解码。
    #[cfg(feature = "bt")]
    pub async fn add_torrent_task(
        &self,
        torrent_bytes: Vec<u8>,
        dest_root: Option<String>,
    ) -> Result<TaskId, DaemonError> {
        self.add_torrent_task_opts(torrent_bytes, dest_root, false)
            .await
    }

    /// 顺序下载变体：`sequential` 写入任务 + 引擎 add 后立即下发 flag。
    #[cfg(feature = "bt")]
    pub async fn add_torrent_task_opts(
        &self,
        torrent_bytes: Vec<u8>,
        dest_root: Option<String>,
        sequential: bool,
    ) -> Result<TaskId, DaemonError> {
        // B10：目标目录预检（创建/可写）；dest 未指定 → 默认落盘目录（与 HTTP/BT-magnet 一致）
        let def = self.default_dest_root.lock().to_string_lossy().into_owned();
        let dest_root = ensure_dest_root(dest_root.or(Some(def)), &self.dest_roots())?;
        let Some(ih) = torrent_infohash(&torrent_bytes) else {
            return Err(DaemonError::InvalidSource(
                ".torrent 解析失败：无法定位 info dict".into(),
            ));
        };
        // B10：torrent 总大小已知 → 空间预检（多文件按 files 各项求和；解析失败
        // 回退单文件最小解析；均拿不到才跳过）
        if let Some(total) = torrent_precheck_total(&torrent_bytes) {
            precheck_space(&dest_root, total, self.disk_precheck_strict)?;
        }
        let canonical = CanonicalId {
            kind: CanonicalKind::Bt,
            identity: ih.clone(),
            validator: None,
            token_sensitive: false,
        };
        let task_id = format!("t{}", self.next_id.fetch_add(1, Ordering::SeqCst));

        // 查重（canonical 一致 → DuplicateRejected）
        {
            let tasks = self.tasks.lock();
            for (existing, rec) in tasks.iter() {
                if rec.task.canonical_id == canonical {
                    self.hub.publish(SchedulerEvent::DuplicateRejected {
                        task_id: task_id.clone(),
                        existing: existing.clone(),
                    });
                    return Err(DaemonError::Duplicate(existing.clone()));
                }
            }
        }

        let task = DownloadTask {
            id: task_id.clone(),
            canonical_id: canonical,
            source: DownloadSource::TorrentFile(torrent_bytes),
            identity: ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                backup_md5: None,
            },
            dest_root: dest_root.clone(),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential,
            metadata: TaskMetadata {
                name: None,
                added_at_unix: 0,
            },
            limits: None,
        };

        let engine_tid = self
            .engine_for(EngineKind::Bt)?
            .add(&task)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        // 顺序下载立即下发（同 magnet 路径：handle 级 flag，失败不回滚）。
        if sequential {
            let engine = self.engine_for(EngineKind::Bt)?;
            if let Err(e) = engine.set_sequential(&engine_tid, true).await {
                tracing::warn!("BT 任务 {task_id} 顺序下载 flag 下发失败: {e}");
            }
        }
        let mut rec = TaskRecord {
            task,
            engine_tid: Some(engine_tid),
            engine_kind: EngineKind::Bt,
            engine_status: None,
            events: vec![],
        };
        rec.push_event("add", None);
        self.tasks.lock().insert(task_id.clone(), rec);
        self.autosave();
        self.hub.publish(SchedulerEvent::TaskCreated {
            task_id: task_id.clone(),
        });
        self.hub.publish(SchedulerEvent::StateChanged {
            task_id: task_id.clone(),
            from: TaskState::Queued,
            to: TaskState::Downloading(EngineKind::Bt),
        });
        Ok(task_id)
    }

    /// 迅雷任务导入（M9）：xlbt.cfg + 一组 .bt.xltd + .torrent → xunlei-convert fastresume
    /// → btcore.add_xunlei_resume → TaskCreated 事件。
    ///
    /// 单文件 torrent：`xltds` 应包含恰好 1 个 `.bt.xltd`（对应唯一文件）。
    /// 多文件 torrent：`xltds` 按 `meta.files` 顺序，每个文件对应一个 `.bt.xltd`。
    #[cfg(feature = "xunlei-import")]
    pub async fn add_xunlei_import_task(
        &self,
        torrent: Vec<u8>,
        cfg: Vec<u8>,
        xltds: Vec<Vec<u8>>,
        dest_root: Option<String>,
    ) -> Result<TaskId, DaemonError> {
        use xunlei_convert::{build_bitfield_lenient, FastresumeConverter, XlbtCfg};

        // 1. 解析 torrent
        let meta = TorrentMeta::parse(&torrent)?;

        // 单文件/多文件统一归一化为文件列表
        let files: Vec<FileMeta> = if meta.files.is_empty() {
            vec![FileMeta {
                path: meta.name.clone(),
                size: meta.file_size,
                piece_offset: 0,
                piece_count: meta.pieces_hash.len(),
            }]
        } else {
            meta.files.clone()
        };
        let total_size: u64 = files.iter().map(|f| f.size).sum();

        // xltd 数量须与文件数一致
        if xltds.len() != files.len() {
            return Err(DaemonError::InvalidSource(format!(
                "xltd 数量 {} 与 torrent 文件数 {} 不匹配",
                xltds.len(),
                files.len()
            )));
        }

        // 2. 确保目标目录存在
        let def = self.default_dest_root.lock().to_string_lossy().into_owned();
        let dest_root = ensure_dest_root(dest_root.or(Some(def)), &self.dest_roots())?;

        // 3. 空间预检（总大小已知）
        precheck_space(&dest_root, total_size, self.disk_precheck_strict)?;

        // 4. 查重
        let canonical = CanonicalId {
            kind: CanonicalKind::Bt,
            identity: meta.info_hash.clone(),
            validator: None,
            token_sensitive: false,
        };
        let task_id = format!("t{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        {
            let tasks = self.tasks.lock();
            for (existing, rec) in tasks.iter() {
                if rec.task.canonical_id == canonical {
                    self.hub.publish(SchedulerEvent::DuplicateRejected {
                        task_id: task_id.clone(),
                        existing: existing.clone(),
                    });
                    return Err(DaemonError::Duplicate(existing.clone()));
                }
            }
        }

        // 5. 转换：逐文件分析 xltd，合并全局 bitfield
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp_dir = std::env::temp_dir().join(format!("xunlei-import-{}-{}", task_id, unique));
        std::fs::create_dir_all(&tmp_dir)
            .map_err(|e| DaemonError::Engine(format!("创建临时目录失败: {e}")))?;
        let torrent_path = tmp_dir.join("source.torrent");
        let cfg_path = tmp_dir.join("source.xlbt.cfg");
        std::fs::write(&torrent_path, &torrent)
            .map_err(|e| DaemonError::Engine(format!("写临时 torrent 失败: {e}")))?;
        std::fs::write(&cfg_path, &cfg)
            .map_err(|e| DaemonError::Engine(format!("写临时 cfg 失败: {e}")))?;

        // 全局完成位图（初始全 0）
        let mut bitfield = vec![0u8; (meta.pieces_hash.len() + 7) / 8];
        let mut completed_total = 0usize;
        let mut partial_infos: Vec<xunlei_convert::PartialPieceInfo> = Vec::new();

        let mut converter = FastresumeConverter::new();
        for (file_idx, file) in files.iter().enumerate() {
            let xltd_path = tmp_dir.join(format!("source.{}.bt.xltd", file_idx));
            std::fs::write(&xltd_path, &xltds[file_idx])
                .map_err(|e| DaemonError::Engine(format!("写临时 xltd[{}] 失败: {e}", file_idx)))?;

            // 该文件对应的 pieces 子集（局部索引从 0 起，xltd 是文件镜像）
            let file_pieces =
                &meta.pieces_hash[file.piece_offset..file.piece_offset + file.piece_count];

            let report = converter.analyze(
                &torrent_path,
                &cfg_path,
                &xltd_path,
                meta.piece_length,
                file_pieces,
                0, // file_offset：xltd 是文件镜像，局部偏移固定 0
                file.size,
            )?;

            // 把局部 partial piece 索引映射回全局索引
            for &(local_idx, nonzero, total) in &report.xltd.partial_details {
                let global_idx = file.piece_offset + local_idx;
                partial_infos.push(xunlei_convert::PartialPieceInfo {
                    index: global_idx,
                    nonzero_bytes: nonzero,
                    total_bytes: total,
                });
            }
            // 累加局部完成数（completed_pieces 是局部索引的前缀计数，这里用位图直接设置更稳妥）
            // completed_pieces 语义：前 N 个 piece 完成（局部），映射到全局连续区间。
            completed_total += report.completed_pieces;
        }

        // 用 lenient 策略构建全局 bitfield（合并所有文件的 partial）
        bitfield =
            build_bitfield_lenient(meta.pieces_hash.len(), completed_total, &partial_infos, 0.5);

        // fastresume file_sizes：[[size, pad], ...]，pad = piece 边界填充
        let file_sizes: Vec<[u64; 2]> = files
            .iter()
            .map(|f| {
                let plen = meta.piece_length as u64;
                let pad = (plen - (f.size % plen)) % plen;
                [f.size, pad]
            })
            .collect();

        let fr = converter.build_fastresume(
            &meta.info_hash,
            &bitfield,
            &meta.name,
            dest_root.to_str().unwrap_or("./"),
            &file_sizes,
        )?;
        let fastresume_bytes = xunlei_convert::fastresume::bencode_fastresume(&fr)
            .map_err(|e| DaemonError::Engine(format!("fastresume bencode 失败: {e}")))?;

        // 清理临时文件（best-effort）
        let _ = std::fs::remove_dir_all(&tmp_dir);

        // 6. 通过 btcore 导入
        let engine_tid = self
            .engine_for(EngineKind::Bt)?
            .add_xunlei_resume(fastresume_bytes)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;

        // 7. 创建任务记录
        let task = DownloadTask {
            id: task_id.clone(),
            canonical_id: canonical,
            source: DownloadSource::TorrentFile(torrent),
            identity: ContentIdentity::SingleFile {
                size: total_size,
                etag: None,
                sha256: None,
                backup_md5: None,
            },
            dest_root: dest_root.clone(),
            files: files
                .iter()
                .map(|f| TaskFile {
                    rel_path: f.path.clone(),
                    size: f.size,
                    done: 0,
                    state: FileState::Pending,
                    source_urls: vec![],
                    identity: None,
                    etag: None,
                    engine: EngineKind::Bt,
                })
                .collect(),
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential: false,
            metadata: TaskMetadata {
                name: Some(meta.name.clone()),
                added_at_unix: 0,
            },
            limits: None,
        };
        let mut rec = TaskRecord {
            task,
            engine_tid: Some(engine_tid.clone()),
            engine_kind: EngineKind::Bt,
            engine_status: None,
            events: vec![],
        };
        rec.push_event("xunlei-import", None);

        // 8. peer 注入（best-effort）：把 cfg 里的 bt:// 地址注入引擎
        if let Ok(cfg_obj) = XlbtCfg::parse(&cfg) {
            let engine = self.engine_for(EngineKind::Bt)?;
            for peer_str in cfg_obj.peers {
                if let Some((ip, port)) = parse_bt_peer(&peer_str) {
                    let addr = format!("{}:{}", ip, port);
                    if let Ok(addr) = addr.parse::<std::net::SocketAddr>() {
                        let _ = engine.add_peer(&engine_tid, addr).await;
                    }
                }
            }
        }

        self.tasks.lock().insert(task_id.clone(), rec);
        self.autosave();
        self.hub.publish(SchedulerEvent::TaskCreated {
            task_id: task_id.clone(),
        });
        self.hub.publish(SchedulerEvent::StateChanged {
            task_id: task_id.clone(),
            from: TaskState::Queued,
            to: TaskState::Downloading(EngineKind::Bt),
        });
        Ok(task_id)
    }

    /// 添加 HTTP 任务：canonical 查重 → HttpEngine.add → TaskCreated 事件。
    pub async fn add_http_task(
        &self,
        url: String,
        dest_root: Option<String>,
    ) -> Result<TaskId, DaemonError> {
        self.add_http_task_opts(url, dest_root, AddHttpOpts::default())
            .await
    }

    /// 创建 HTTP 任务（E6 opts 收口）：sequential/proxy（E5）+ headers/auth/
    /// sha256/backup_url+backup_md5/name（E6 新暴露）。入参校验（E6 validate +
    /// E5 代理构建试水）在探测/建任务之前——远端不可达 ≠ 入参非法，分开定性。
    pub async fn add_http_task_opts(
        &self,
        url: String,
        dest_root: Option<String>,
        opts: AddHttpOpts,
    ) -> Result<TaskId, DaemonError> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(DaemonError::InvalidSource(url));
        }
        opts.validate().map_err(DaemonError::InvalidSource)?;
        let AddHttpOpts {
            sequential,
            proxy,
            headers,
            basic_auth,
            sha256,
            backup_url,
            backup_md5,
            name,
        } = opts;
        // E5：任务级代理 URL 校验（构建一次 client 试水，成功即合法）。
        if let Some(p) = &proxy {
            if p.is_empty() {
                return Err(DaemonError::InvalidSource("proxy 不能为空字符串".into()));
            }
            smart_dl_httpdl::build_proxied_client(p)
                .map_err(|e| DaemonError::InvalidSource(format!("proxy 非法 {p:?}: {e}")))?;
        }
        // 校验和归一（引擎端摘要为小写 hex；trim 防复制粘贴带空白）
        let sha256 = sha256.map(|s| normalize_digest(&s));
        let backup_md5 = backup_md5.map(|s| normalize_digest(&s));
        // B10：目标目录预检（创建/可写）；HTTP 大小在响应头才知 → 空间预检跳过
        // dest 未指定 → 默认落盘目录（serve 配置 dest_root；未注入时为 daemon cwd）
        let def = self.default_dest_root.lock().to_string_lossy().into_owned();
        let dest = dest_root.or(Some(def));
        let dest_root = ensure_dest_root(dest, &self.dest_roots())?;
        let canonical = CanonicalId {
            kind: CanonicalKind::Http,
            identity: canonical_http_url(&url), // D34：剥 token 参数后的 canonical 身份
            validator: None,
            token_sensitive: false,
        };
        let task_id = format!("t{}", self.next_id.fetch_add(1, Ordering::SeqCst));

        // 查重（canonical 一致 → DuplicateRejected）
        {
            let tasks = self.tasks.lock();
            for (existing, rec) in tasks.iter() {
                if rec.task.canonical_id == canonical {
                    self.hub.publish(SchedulerEvent::DuplicateRejected {
                        task_id: task_id.clone(),
                        existing: existing.clone(),
                    });
                    return Err(DaemonError::Duplicate(existing.clone()));
                }
            }
        }

        let task = DownloadTask {
            id: task_id.clone(),
            canonical_id: canonical,
            source: DownloadSource::Http {
                url: url.clone(),
                headers,
                auth: basic_auth.map(|(u, p)| Auth::Basic(u, p)),
                backup_url,
                proxy: proxy.clone(),
            },
            identity: ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256,
                backup_md5,
            },
            dest_root: dest_root.clone(),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential,
            metadata: TaskMetadata {
                name,
                added_at_unix: 0,
            },
            limits: None,
        };

        let engine_tid = self
            .engine_for(EngineKind::Http)?
            .add(&task)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        let mut rec = TaskRecord {
            task,
            engine_tid: Some(engine_tid),
            engine_kind: EngineKind::Http,
            engine_status: None,
            events: vec![],
        };
        rec.push_event("add", None);
        self.tasks.lock().insert(task_id.clone(), rec);
        self.autosave();
        self.hub.publish(SchedulerEvent::TaskCreated {
            task_id: task_id.clone(),
        });
        self.hub.publish(SchedulerEvent::StateChanged {
            task_id: task_id.clone(),
            from: TaskState::Queued,
            to: TaskState::Downloading(EngineKind::Http),
        });
        Ok(task_id)
    }

    /// 添加 FTP 任务（feature `ftp`）：校验 `ftp://` 前缀 → ensure_dest_root →
    /// `parse_ftp_auth` 提取 user/pass → 归一化 URL 作 canonical 查重 → 路由 `EngineKind::Ftp`
    /// 引擎 → add → TaskCreated/StateChanged 事件与持久化（完全仿照 add_http_task）。
    ///
    /// 目录任务（url 以 `/` 结尾）：引擎 `add` 时已同步 LIST 出文件清单，此处做【有限次数的
    /// files 同步】——轮询 `engine.status(tid)` 数次直到 `files` 非空，按 TaskFile 结构映射写入
    /// `task.files`；始终为空（目录瞬时无文件/解析延迟）则静默跳过，文件级进度后续经既有轮询
    /// 链路从 EngineStatus 透出，不做强制阻塞。
    #[cfg(feature = "ftp")]
    pub async fn add_ftp_task(
        &self,
        url: String,
        dest_root: Option<String>,
    ) -> Result<TaskId, DaemonError> {
        if !url.starts_with("ftp://") {
            return Err(DaemonError::InvalidSource(url));
        }
        // B10：目标目录预检；目录总大小需 LIST 才可知 → 空间预检跳过（同 HTTP 逻辑）
        let def = self.default_dest_root.lock().to_string_lossy().into_owned();
        let dest_root = ensure_dest_root(dest_root.or(Some(def)), &self.dest_roots())?;
        let (user, pass) = smart_dl_core::source_parse::ftp::parse_ftp_auth(&url);
        // D34 复用 canonical 归一化（url 无 query 时基本原样）：FTP 身份键 = 归一化 URL
        let canonical = CanonicalId {
            kind: CanonicalKind::Ftp,
            identity: canonical_http_url(&url),
            validator: None,
            token_sensitive: false,
        };
        let task_id = format!("t{}", self.next_id.fetch_add(1, Ordering::SeqCst));

        // 查重（canonical 一致 → DuplicateRejected）
        {
            let tasks = self.tasks.lock();
            for (existing, rec) in tasks.iter() {
                if rec.task.canonical_id == canonical {
                    self.hub.publish(SchedulerEvent::DuplicateRejected {
                        task_id: task_id.clone(),
                        existing: existing.clone(),
                    });
                    return Err(DaemonError::Duplicate(existing.clone()));
                }
            }
        }

        let is_dir = url.ends_with('/');
        // 单文件任务：落盘名取 URL 最后一段（引擎 `add` 用作 dest 相对文件名）；目录任务由引擎自理
        let name = url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let task = DownloadTask {
            id: task_id.clone(),
            canonical_id: canonical,
            source: DownloadSource::Ftp {
                url: url.clone(),
                user,
                pass,
            },
            identity: ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                backup_md5: None,
            },
            dest_root: dest_root.clone(),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential: false,
            metadata: TaskMetadata {
                name: if is_dir { None } else { name },
                added_at_unix: 0,
            },
            limits: None,
        };

        let engine = self.engine_for(EngineKind::Ftp)?;
        let engine_tid = engine
            .add(&task)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        let mut rec = TaskRecord {
            task,
            engine_tid: Some(engine_tid.clone()),
            engine_kind: EngineKind::Ftp,
            engine_status: None,
            events: vec![],
        };
        rec.push_event("add", None);

        // 目录任务：有限次 files 同步（FtpEngine::add 已同步 LIST，首轮通常即可命中）
        if is_dir {
            for _ in 0..8 {
                if let Ok(st) = engine.status(&engine_tid).await {
                    if !st.files.is_empty() {
                        rec.task.files = st
                            .files
                            .into_iter()
                            .map(|f| TaskFile {
                                rel_path: f.rel_path,
                                size: f.size,
                                done: f.done,
                                state: FileState::Active,
                                source_urls: vec![url.clone()],
                                identity: None,
                                etag: None,
                                engine: EngineKind::Ftp,
                            })
                            .collect();
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            }
        }

        self.tasks.lock().insert(task_id.clone(), rec);
        self.autosave();
        self.hub.publish(SchedulerEvent::TaskCreated {
            task_id: task_id.clone(),
        });
        self.hub.publish(SchedulerEvent::StateChanged {
            task_id: task_id.clone(),
            from: TaskState::Queued,
            to: TaskState::Downloading(EngineKind::Ftp),
        });
        Ok(task_id)
    }

    /// 任务快照（实时读引擎状态；未完成时引擎可能已移动）。
    pub async fn task_snapshot(&self, id: &str) -> Option<TaskSnapshot> {
        let rec = self.tasks.lock().get(id).cloned()?;
        let engine = self.engine_for(rec.engine_kind).ok();
        let (engine_name, status) = match (&rec.engine_tid, &engine) {
            (Some(tid), Some(eng)) => {
                let st = eng.status(tid).await.ok();
                (Some(eng.id().to_string()), st)
            }
            _ => (None, None),
        };
        // 显示层权威（qB 式）：用户暂停是记录级事实，不被引擎实时态覆盖
        // （lt 暂停后 status 枚举仍报 downloading，ABI 未透出 paused 位）。
        let effective_state = match &rec.task.state {
            TaskState::Paused => rec.task.state.clone(),
            other => match &status {
                Some(s) => engine_state_to_task(&s.state, rec.engine_kind),
                None => other.clone(),
            },
        };
        let state = state_label(&effective_state);
        Some(TaskSnapshot {
            task_id: id.to_string(),
            state,
            // 安全修复（V6）：source 可能含凭据（headers/auth/userinfo），不得 {:?} 直通
            source: rec.task.source.redacted_debug(),
            dest_root: rec.task.dest_root.clone(),
            engine: engine_name,
            done: status.as_ref().map(|s| s.total_done).unwrap_or(0),
            total: status.as_ref().map(|s| s.total).unwrap_or(0),
            error: status.as_ref().and_then(|s| s.error.clone()),
            files: status.as_ref().map(|s| s.files.clone()).unwrap_or_default(),
            limits: rec.task.limits.clone(),
            file_priorities: rec.task.file_priorities.clone(),
            sequential: rec.task.sequential,
            name: rec.task.metadata.name.clone(),
        })
    }

    /// 全量列表（兼容入口：无过滤无分页，形状与 E7 之前一致）。
    pub fn list(&self) -> Vec<TaskSummary> {
        self.list_filtered(&ListQuery::default()).0
    }

    /// 过滤 + 排序 + 分页列表（E7）：
    /// - 排序：task_id 数值后缀升序（创建序；HashMap 迭代序不稳定，分页必须
    ///   确定性排序。task_id = `t{u64}` 自增序号，parse 失败兜底排序键 u64::MAX）。
    /// - 过滤：states/engines 任一命中即保留（OR 语义）；空集合 = 该维度跳过；
    ///   两维度间 AND；匹配大小写不敏感（查询方可写小写）。
    /// - 返回 `(当前页, 过滤后总数)`——总数供 `X-Total-Count`，客户端算页数。
    pub fn list_filtered(&self, q: &ListQuery) -> (Vec<TaskSummary>, usize) {
        let tasks = self.tasks.lock();
        let mut rows: Vec<(&String, &TaskRecord)> = tasks.iter().collect();
        rows.sort_by_cached_key(|(id, _)| {
            id.strip_prefix('t')
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(u64::MAX)
        });
        let filtered: Vec<&TaskRecord> = rows
            .into_iter()
            .filter(|(_, rec)| {
                let st = state_label(&rec.task.state);
                let en = kind_label(&rec.engine_kind);
                (q.states.is_empty() || q.states.iter().any(|s| s.eq_ignore_ascii_case(&st)))
                    && (q.engines.is_empty()
                        || q.engines.iter().any(|e| e.eq_ignore_ascii_case(en)))
            })
            .map(|(_, rec)| rec)
            .collect();
        let total = filtered.len();
        let page = filtered
            .into_iter()
            .skip(q.offset)
            .take(q.limit.unwrap_or(usize::MAX))
            .map(|rec| TaskSummary {
                task_id: rec.task.id.clone(),
                state: state_label(&rec.task.state),
                // 安全修复（V6）：同快照，source 脱敏
                source: rec.task.source.redacted_debug(),
                engine: kind_label(&rec.engine_kind),
                name: rec.task.metadata.name.clone(),
            })
            .collect();
        (page, total)
    }

    /// 批量操作（E7）：按入参顺序逐任务执行；重复 id 静默去重（保留首次出现序，
    /// 避免同一任务被 pause 两次产生假失败）；单项失败（NotFound/引擎错误）
    /// 记入该项结果后继续，绝不短路。永远返回 BatchOutcome（HTTP 层恒 200）。
    pub async fn batch(&self, ids: &[String], action: BatchAction) -> BatchOutcome {
        let mut seen = std::collections::HashSet::new();
        let mut results = Vec::new();
        let (mut ok, mut bad) = (0usize, 0usize);
        for id in ids {
            if !seen.insert(id.clone()) {
                continue;
            }
            let r = match action {
                BatchAction::Pause => self.pause(id).await,
                BatchAction::Resume => self.resume(id).await,
                BatchAction::Remove { delete_data } => self.remove_with(id, delete_data).await,
            };
            match r {
                Ok(()) => {
                    ok += 1;
                    results.push(BatchItemResult {
                        id: id.clone(),
                        ok: true,
                        error: None,
                    });
                }
                Err(e) => {
                    bad += 1;
                    results.push(BatchItemResult {
                        id: id.clone(),
                        ok: false,
                        error: Some(e.to_string()),
                    });
                }
            }
        }
        BatchOutcome {
            results,
            succeeded: ok,
            failed: bad,
        }
    }

    /// 全局统计快照（`GET /stats`）：总数 + 按状态/引擎聚合 + 速率求和。
    pub fn stats(&self) -> DaemonStats {
        let mut st = DaemonStats::default();
        let tasks = self.tasks.lock();
        st.total = tasks.len();
        for rec in tasks.values() {
            *st.by_state.entry(state_label(&rec.task.state)).or_insert(0) += 1;
            *st.by_engine
                .entry(kind_label(&rec.engine_kind))
                .or_insert(0) += 1;
            if let Some(s) = &rec.engine_status {
                st.down_bytes_s += s.down_rate;
                st.up_bytes_s += s.up_rate;
            }
        }
        st
    }

    pub async fn pause(&self, id: &str) -> Result<(), DaemonError> {
        let rec = self
            .tasks
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        let tid = rec
            .engine_tid
            .clone()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        self.engine_for(rec.engine_kind)?
            .pause(&tid)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        if let Some(rec) = self.tasks.lock().get_mut(id) {
            rec.push_event("pause", None);
            rec.task.state = TaskState::Paused; // 记录缓存同步（alert 流不迁移 pause）
                                                // E11：暂停即清零缓存速率——轮询器不再光顾暂停任务，
                                                // 不清则 /stats 恒把最后窗口速率计入聚合（恢复后下一轮刷新）。
            if let Some(es) = rec.engine_status.as_mut() {
                es.down_rate = 0;
                es.up_rate = 0;
            }
        }
        // 暂停意图必须立刻持久化（P4 G5）：否则重启后暂停任务被当作运行任务恢复
        self.autosave();
        self.hub.publish(SchedulerEvent::StateChanged {
            task_id: id.to_string(),
            from: TaskState::Downloading(rec.engine_kind),
            to: TaskState::Paused,
        });
        Ok(())
    }

    pub async fn resume(&self, id: &str) -> Result<(), DaemonError> {
        let rec = self
            .tasks
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        let tid = rec
            .engine_tid
            .clone()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        self.engine_for(rec.engine_kind)?
            .resume(&tid)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        if let Some(rec) = self.tasks.lock().get_mut(id) {
            rec.push_event("resume", None);
            rec.task.state = TaskState::Downloading(rec.engine_kind);
        }
        // 恢复态同步持久化（P4 G5：与 pause 对称）
        self.autosave();
        self.hub.publish(SchedulerEvent::StateChanged {
            task_id: id.to_string(),
            from: TaskState::Paused,
            to: TaskState::Downloading(rec.engine_kind),
        });
        Ok(())
    }

    /// 任务级限速（P1 能力增强）。合并口径：请求中 `None` 的方向沿用既有值
    /// （首设即不限）；引擎调用总拿到全量两方向（BT 引擎 None 方向按不限下发，
    /// 避免 lt_set_limits 全量语义把已设方向清零）。
    ///
    /// - `Some(0)` = 该方向不限速；`Some(n)` = 上限 n KiB/s
    /// - HTTP/FTP：仅 down 方向有意义（up → 引擎报错，HTTP 层映射 409/422）
    /// - 合并结果持久化（tasks.json）并在恢复时重放；内存中即时生效
    ///   （HTTP 引擎热调速率；BT 引擎 libtorrent per-torrent limit）
    pub async fn set_task_limits(
        &self,
        id: &str,
        down_kb_s: Option<u32>,
        up_kb_s: Option<u32>,
    ) -> Result<smart_dl_core::task::TaskLimits, DaemonError> {
        let (engine, tid, merged) = {
            let mut tasks = self.tasks.lock();
            let rec = tasks
                .get_mut(id)
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            let tid = rec
                .engine_tid
                .clone()
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            // up 方向仅 BT 引擎有意义；其余引擎在此预拒（HTTP 层映射 409，
            // 避免引擎层 Engine 错误被当成服务端 500）
            if up_kb_s.is_some() && rec.engine_kind != EngineKind::Bt {
                return Err(DaemonError::UnsupportedOp(format!(
                    "任务 {id}（{:?}）无上传方向，up_kb_s 仅对 BT 任务有意义",
                    rec.engine_kind
                )));
            }
            let old = rec.task.limits.take().unwrap_or_default();
            let merged = smart_dl_core::task::TaskLimits {
                down_kb_s: down_kb_s.or(old.down_kb_s),
                up_kb_s: up_kb_s.or(old.up_kb_s),
            };
            // 两方向均为空（从未设置且请求未带）→ 维持 None（快照不出噪声字段）
            rec.task.limits = if merged.is_empty() {
                None
            } else {
                Some(merged.clone())
            };
            rec.push_event(
                "limits_changed",
                Some(format!(
                    "down={:?} up={:?}",
                    merged.down_kb_s, merged.up_kb_s
                )),
            );
            let engine = self.engine_for(rec.engine_kind)?;
            (engine, tid, merged)
        };
        engine
            .set_limits(&tid, merged.down_kb_s, merged.up_kb_s)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        self.autosave();
        Ok(merged)
    }

    /// 任务级子文件优先级（P1 能力增强，BT 多文件）。设置后返回当前各文件
    /// 优先级快照（下标 = 文件序，与 TaskSnapshot.files 对齐）。
    ///
    /// - 仅 BT 任务（其余 → UnsupportedOp，HTTP 层映射 409）
    /// - 文件数锚定与 metadata 就绪性探测合一：先 readback 当前优先级表
    ///   （engine 侧真实文件数），metadata 未就绪/句柄缺失 → UnsupportedOp（409）
    /// - 下标越界 / 优先级 >7 → InvalidSource（400）；内核侧两段式校验兜底
    /// - 持久化 + 恢复重放：成功后把全量快照（readback None 视为默认 4）写入
    ///   `task.file_priorities` 并落盘；恢复时原样重放（magnet 未就绪场景由
    ///   重放循环延迟收敛，见 `replay_pending_file_priorities`）
    pub async fn set_task_file_priorities(
        &self,
        id: &str,
        priorities: &[(usize, u32)],
    ) -> Result<Vec<Option<u32>>, DaemonError> {
        let (engine, tid) = {
            let rec = self
                .tasks
                .lock()
                .get(id)
                .cloned()
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            if rec.engine_kind != EngineKind::Bt {
                return Err(DaemonError::UnsupportedOp(format!(
                    "仅 BT 任务支持子文件优先级（{id} 为 {:?}）",
                    rec.engine_kind
                )));
            }
            let tid = rec
                .engine_tid
                .clone()
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            (self.engine_for(rec.engine_kind)?, tid)
        };
        // metadata 就绪探测 + 文件数锚定（当前优先级表长度 = 引擎侧文件数）。
        // 引擎 NotFound（torrent/metadata 缺失）→ 409「metadata 未就绪」，
        // 与任务记录级 404（tasks 表无此 id）语义分离。
        let current = engine.file_priorities(&tid).await.map_err(|e| match e {
            smart_dl_core::types::EngineError::NotFound => DaemonError::UnsupportedOp(
                "BT 任务 metadata 未就绪（或引擎句柄不存在），无法设置子文件优先级".into(),
            ),
            other => DaemonError::Engine(other.to_string()),
        })?;
        let nf = current.len();
        for (idx, prio) in priorities {
            if *idx >= nf {
                return Err(DaemonError::InvalidSource(format!(
                    "文件下标 {idx} 越界（任务 {id} 引擎侧共 {nf} 个文件）"
                )));
            }
            if *prio > 7 {
                return Err(DaemonError::InvalidSource(format!(
                    "优先级 {prio} 越界（0..=7：0=不下载 1=低 4=默认 7=最高）"
                )));
            }
        }
        engine
            .set_file_priorities(&tid, priorities)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        let snapshot = engine
            .file_priorities(&tid)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        // 持久化全量快照：readback 的 None（内核未定值）按 libtorrent 默认
        // 优先级 4 归一，保证重放值与引擎语义一致。
        let persisted: Vec<u32> = snapshot.iter().map(|p| p.unwrap_or(4)).collect();
        {
            let mut tasks = self.tasks.lock();
            if let Some(rec) = tasks.get_mut(id) {
                rec.task.file_priorities = Some(persisted);
                rec.push_event(
                    "file_priorities_changed",
                    Some(
                        priorities
                            .iter()
                            .map(|(i, p)| format!("{i}={p}"))
                            .collect::<Vec<_>>()
                            .join(","),
                    ),
                );
            }
        }
        self.pending_file_prio.lock().remove(id);
        self.autosave();
        Ok(snapshot)
    }

    /// 任务级顺序下载开关（边下边播）：引擎即时生效（HTTP=字段改写下轮拾取；
    /// BT=sequential flag 即时）+ 任务持久化 + TaskSequentialChanged 事件。
    /// FTP 引擎不支持（Unsupported → 400）。
    pub async fn set_task_sequential(&self, id: &str, on: bool) -> Result<(), DaemonError> {
        let (engine, tid) = {
            let rec = self
                .tasks
                .lock()
                .get(id)
                .cloned()
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            let tid = rec
                .engine_tid
                .clone()
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            (self.engine_for(rec.engine_kind)?, tid)
        };
        engine.set_sequential(&tid, on).await.map_err(|e| match e {
            smart_dl_core::types::EngineError::Unsupported => {
                DaemonError::UnsupportedOp(format!("任务 {id} 的引擎不支持顺序下载"))
            }
            other => DaemonError::Engine(other.to_string()),
        })?;
        {
            let mut tasks = self.tasks.lock();
            if let Some(rec) = tasks.get_mut(id) {
                rec.task.sequential = on;
                rec.push_event("sequential_changed", Some(on.to_string()));
            }
        }
        self.autosave();
        Ok(())
    }

    /// 任务级代理热改（E8）：`Some(url)` = 切任务专用 client（覆盖全局，
    /// add 时 E5 语义的运行时版）；`None` = 清除回引擎共享 client。
    ///
    /// - 仅 HTTP 任务：daemon 侧预拒（其余 kind → `UnsupportedOp` 409），
    ///   不依赖引擎 trait default 拒绝——错误信息带任务 kind，且避免
    ///   engine_for 未注册引擎时的笼统报错。
    /// - `Some` 空串拒绝（与 add 口径一致：空串是非法 URL 不是清除；清除
    ///   语义由 `None` 承担）。
    /// - URL 试水（`build_proxied_client`）先行 → `InvalidSource` 400；
    ///   远端不可达 ≠ 代理非法（不发起连接，纯本地构建校验）。
    /// - 引擎应用成功后才改记录（引擎侧对下载中任务 epoch+1 重入，
    ///   段账本恢复进度）；记录改写用 match 下钻 enum（`DownloadSource::Http`
    ///   的 `proxy` 字段）。
    /// - 事件 detail 不放 URL 原文（proxy 可含凭据；push_event 链路无
    ///   脱敏通道）——只记 set/cleared。
    pub async fn set_task_proxy(&self, id: &str, proxy: Option<String>) -> Result<(), DaemonError> {
        if let Some(p) = &proxy {
            if p.is_empty() {
                return Err(DaemonError::InvalidSource(
                    "proxy 不能为空字符串（清除语义请传 null）".into(),
                ));
            }
            smart_dl_httpdl::build_proxied_client(p)
                .map_err(|e| DaemonError::InvalidSource(format!("proxy 非法 {p:?}: {e}")))?;
        }
        let (engine, tid) = {
            let rec = self
                .tasks
                .lock()
                .get(id)
                .cloned()
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            let tid = rec
                .engine_tid
                .clone()
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            if rec.engine_kind != EngineKind::Http {
                return Err(DaemonError::UnsupportedOp(format!(
                    "任务 {id}（{:?}）不支持任务级代理——仅 HTTP 任务（BT 代理属会话级配置）",
                    rec.engine_kind
                )));
            }
            (self.engine_for(rec.engine_kind)?, tid)
        };
        engine
            .set_task_proxy(&tid, proxy.clone())
            .await
            .map_err(|e| match e {
                smart_dl_core::types::EngineError::Unsupported => {
                    DaemonError::UnsupportedOp(format!("任务 {id} 的引擎不支持任务级代理"))
                }
                other => DaemonError::Engine(other.to_string()),
            })?;
        {
            let mut tasks = self.tasks.lock();
            if let Some(rec) = tasks.get_mut(id) {
                // 引擎 kind 已预拒非 Http；此处必然命中 Http 变体（防御性 if let）
                if let DownloadSource::Http { proxy: p, .. } = &mut rec.task.source {
                    *p = proxy.clone();
                }
                rec.push_event(
                    "proxy_changed",
                    Some(if proxy.is_some() { "set" } else { "cleared" }.into()),
                );
            }
        }
        self.autosave();
        Ok(())
    }

    /// 子文件优先级重放收敛（单轮）：对恢复时 metadata 未就绪而挂起的任务，
    /// 探测就绪性（readback 非空）→ 成功后全量重放并移除 pending。
    /// 返回本轮成功重放的任务 id 列表（测试/日志用）。
    ///
    /// 容错口径：任务已移除 / engine_tid 缺失（恢复 add 失败，v1 不会自愈）/
    /// 引擎不支持（Unsupported）→ 移除 pending（永不收敛项不留尾）；
    /// 其余失败（引擎忙/暂不可用）保留 pending 下轮再试。
    pub async fn replay_pending_file_priorities(&self) -> Vec<TaskId> {
        let pending: Vec<TaskId> = self.pending_file_prio.lock().iter().cloned().collect();
        if pending.is_empty() {
            return Vec::new();
        }
        let mut done = Vec::new();
        for id in pending {
            let (engine, tid, prios) = {
                let tasks = self.tasks.lock();
                let Some(rec) = tasks.get(&id) else {
                    self.pending_file_prio.lock().remove(&id);
                    continue;
                };
                let Some(tid) = rec.engine_tid.clone() else {
                    self.pending_file_prio.lock().remove(&id);
                    continue;
                };
                let Some(prios) = rec.task.file_priorities.clone() else {
                    self.pending_file_prio.lock().remove(&id);
                    continue;
                };
                match self.engine_for(rec.engine_kind) {
                    Ok(e) => (e, tid, prios),
                    Err(_) => continue, // 引擎暂不可用：下轮再试
                }
            };
            // 就绪性探测：readback 成功且非空 = metadata 已就绪
            match engine.file_priorities(&tid).await {
                Ok(cur) if !cur.is_empty() => {
                    let pairs: Vec<(usize, u32)> =
                        prios.iter().enumerate().map(|(i, p)| (i, *p)).collect();
                    match engine.set_file_priorities(&tid, &pairs).await {
                        Ok(()) => {
                            self.pending_file_prio.lock().remove(&id);
                            if let Some(rec) = self.tasks.lock().get_mut(&id) {
                                rec.push_event("restored", Some("子文件优先级重放完成".into()));
                            }
                            tracing::info!("任务 {id} 子文件优先级重放完成（{} 项）", pairs.len());
                            done.push(id);
                        }
                        Err(smart_dl_core::types::EngineError::Unsupported) => {
                            self.pending_file_prio.lock().remove(&id);
                        }
                        Err(_) => {} // 引擎忙/瞬态错误：下轮再试
                    }
                }
                Err(smart_dl_core::types::EngineError::Unsupported) => {
                    self.pending_file_prio.lock().remove(&id);
                }
                _ => {} // 未就绪/暂不可读：下轮再试
            }
        }
        done
    }

    /// F5 P2SP：给运行中的 BT 任务逐条注入 web seed（云盘直链，BEP-19），
    /// 返回成功注入条数。仅 BT 任务可注入（其余 → UnsupportedOp，HTTP 层映射
    /// 409）；engine_tid 缺失（尚未入引擎/恢复失败）→ NotFound。
    /// **URL 必须原样使用**：禁止增删改任何 query 参数——云盘直链带 `at=`
    /// 防篡改签名，改动即失效；要多条链请重新调用取链 API 取新链（F5 PoC-1b）。
    pub async fn add_webseeds(&self, id: &str, urls: &[String]) -> Result<usize, DaemonError> {
        let rec = self
            .tasks
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        let tid = rec
            .engine_tid
            .clone()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        if rec.engine_kind != EngineKind::Bt {
            return Err(DaemonError::UnsupportedOp(format!(
                "仅 BT 任务支持注入 web seed（{id} 为 {:?}）",
                rec.engine_kind
            )));
        }
        let engine = self.engine_for(EngineKind::Bt)?;
        let mut added = 0usize;
        for url in urls {
            engine
                .add_url_seed(&tid, url)
                .await
                .map_err(|e| DaemonError::Engine(e.to_string()))?;
            added += 1;
        }
        if let Some(rec) = self.tasks.lock().get_mut(id) {
            rec.push_event("webseed", Some(format!("+{added}")));
        }
        Ok(added)
    }

    /// 删除任务（E7 前 semantics：保留已下载数据）。
    pub async fn remove(&self, id: &str) -> Result<(), DaemonError> {
        self.remove_with(id, false).await
    }

    /// 删除任务 + 数据处置开关（E7）：`delete_data = true` 时引擎侧同步删除
    /// 已下载数据（BT 删种子数据 / HTTP 删落盘文件）。引擎删除失败不阻塞
    /// 记录移除（引擎 remove 本就是尽力而为——任务可能已不在引擎侧）。
    pub async fn remove_with(&self, id: &str, delete_data: bool) -> Result<(), DaemonError> {
        let rec = self
            .tasks
            .lock()
            .remove(id)
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        if let Some(tid) = rec.engine_tid {
            if let Ok(engine) = self.engine_for(rec.engine_kind) {
                let _ = engine.remove(&tid, delete_data).await;
            }
        }
        self.autosave();
        Ok(())
    }

    /// Provider 运行态快照（健康/配额/冷却）。
    pub fn provider_status(&self) -> Vec<(String, ProviderRuntime)> {
        self.providers
            .iter()
            .map(|p| (p.name().to_string(), p.runtime()))
            .collect()
    }

    /// Q-B9 手动兜底（M6 接线）：BT 任务 → 云 Provider → 直链 → HTTP 引擎传输。
    /// 前置（FallbackPolicy 默认冻结）：任务须为 BT 且已暂停；BT 进度 < 50%。
    /// 成功 → 任务置 Completed + 事件广播 + 落盘。
    pub async fn fallback(&self, id: &str) -> Result<FallbackOutcome, DaemonError> {
        // 1. 任务存在性 + 必须是 BT
        let rec = self
            .tasks
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        if rec.engine_kind != EngineKind::Bt {
            return Err(DaemonError::Fallback(format!(
                "仅 BT 任务支持云兜底（{} 为 {:?}）",
                id, rec.engine_kind
            )));
        }
        // 2. 串行策略（默认禁双份占盘）→ 必须先暂停
        if rec.task.state != TaskState::Paused {
            return Err(DaemonError::Fallback(format!(
                "需先暂停 BT 任务 {id}（串行兜底策略：禁 BT/直链双份占盘）"
            )));
        }
        // 3. BT 进度（metadata 未到 → total=0 → 进度 0，允许兜底）；≥50% 拒绝
        let bt_progress = match (&rec.engine_tid, self.engine_for(EngineKind::Bt).ok()) {
            (Some(tid), Some(engine)) => engine
                .status(tid)
                .await
                .ok()
                .map(|s| {
                    if s.total == 0 {
                        0.0
                    } else {
                        s.total_done as f64 / s.total as f64
                    }
                })
                .unwrap_or(0.0),
            _ => 0.0,
        };
        // 4. 协商器 + 传输 sink → 执行兜底
        if self.providers.is_empty() {
            return Err(DaemonError::Fallback(
                "无可用 provider（未配置或全部不可用）".into(),
            ));
        }
        let coord = FallbackCoordinator::new(
            self.providers.clone(),
            smart_dl_core::ownership::FallbackPolicy::default(),
        );
        let http = self
            .engine_for(EngineKind::Http)
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        let sink = FallbackSink { http };
        let outcome = coord
            .begin_fallback(&rec.task, bt_progress, true, &sink)
            .await
            .map_err(map_provider_err)?;
        // 4b. BT 引擎任务退役（直链已替代 BT 传输，keep data）：
        // 快照不再读引擎实时下载态 → 回落到记录态 Completed
        if let (Some(tid), Ok(bt)) = (&rec.engine_tid, self.engine_for(EngineKind::Bt)) {
            let _ = bt.remove(tid, false).await;
        }
        // 5. 成功：置 Completed + 事件 + 落盘
        {
            let mut tasks = self.tasks.lock();
            if let Some(r) = tasks.get_mut(id) {
                r.push_event("fallback", Some(format!("provider={}", outcome.provider)));
                r.task.state = TaskState::Completed;
            }
        }
        self.autosave();
        self.hub.publish(SchedulerEvent::StateChanged {
            task_id: id.to_string(),
            from: TaskState::Downloading(EngineKind::Bt),
            to: TaskState::Completed,
        });
        self.hub.publish(SchedulerEvent::Completed {
            task_id: id.to_string(),
        });
        Ok(outcome)
    }

    /// 任务操作日志（`GET /tasks/:id/logs`）：快照 + 事件序列。
    pub fn task_logs(&self, id: &str) -> Result<serde_json::Value, DaemonError> {
        let tasks = self.tasks.lock();
        let rec = tasks
            .get(id)
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        Ok(serde_json::json!({
            "task_id": rec.task.id,
            "state": state_label(&rec.task.state),
            "source": rec.task.source.redacted_debug(),
            "error": rec.engine_status.as_ref().and_then(|s| s.error.clone()),
            "events": rec.events,
        }))
    }

    /// 生效配置快照（`GET /config` 返回；未注入时给出提示对象）。
    pub fn config_snapshot(&self) -> serde_json::Value {
        self.config_snapshot
            .lock()
            .clone()
            .unwrap_or_else(|| serde_json::json!({ "note": "配置快照未注入（serve 组装）" }))
    }

    /// #6 TOML 热重载应用：配置重读后刷新可热更字段（default_dest_root + /config 快照）。
    /// 变更项记日志；不变项静默。
    pub fn refresh_config(&self, cfg: &crate::config::Config, tasks_path: &std::path::Path) {
        {
            let mut def = self.default_dest_root.lock();
            let new_root = cfg.download.dest_root.clone();
            if *def != new_root {
                tracing::info!("配置热重载: dest_root {:?} → {:?}", *def, new_root);
                *def = new_root;
            }
        }
        // 安全修复（V2）联动：热重载换默认目录时，白名单同步追加新根
        //（追加而非替换——保留旧根允许显式 dest 指向旧目录的存量工作流；
        // 白名单为空表时不必动：dest_roots() 兜底跟随 default_dest_root）。
        {
            let mut roots = self.allowed_roots.lock();
            if !roots.is_empty() && !roots.contains(&cfg.download.dest_root) {
                roots.push(cfg.download.dest_root.clone());
            }
        }
        let snap = crate::config::Config::snapshot_json(cfg, tasks_path);
        if *self.config_snapshot.lock() != Some(snap.clone()) {
            *self.config_snapshot.lock() = Some(snap);
        }
    }
}

/// 兜底传输 sink：HTTP 引擎承接 provider 直链下载（M5 直链 → HttpEngine）。
/// 每个文件建引擎任务 → 轮询到终态 → 移除引擎任务（不留记录，属于父 BT 任务流程）。
struct FallbackSink {
    http: Arc<dyn DownloadEngine>,
}

#[async_trait::async_trait]
impl HttpSink for FallbackSink {
    async fn transfer(
        &self,
        task_id: &str,
        url: &str,
        dest_root: std::path::PathBuf,
        name: Option<String>,
    ) -> Result<(), SinkError> {
        // 目标父目录（rel_path 可能含子目录）
        if let Some(rel) = &name {
            if let Some(parent) = dest_root.join(rel).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let task = DownloadTask {
            id: task_id.to_string(),
            canonical_id: CanonicalId {
                kind: CanonicalKind::Http,
                identity: url.to_string(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Http {
                url: url.to_string(),
                headers: vec![],
                auth: None,
                backup_url: None,
                proxy: None,
            },
            identity: ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                backup_md5: None,
            },
            dest_root,
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential: false,
            metadata: TaskMetadata {
                name,
                added_at_unix: 0,
            },
            limits: None,
        };
        let tid = self
            .http
            .add(&task)
            .await
            .map_err(|e| SinkError::Failed(e.to_string()))?;
        // 轮询到终态（直链传输上限 600s：免费档聚合 ~1MB/s，须容纳数百 MB 文件）
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        let started = std::time::Instant::now();
        let mut last_beat = started;
        let mut last_done = u64::MAX;
        let result = loop {
            let st = self
                .http
                .status(&tid)
                .await
                .map_err(|e| SinkError::Failed(e.to_string()))?;
            if last_done == u64::MAX {
                last_done = st.total_done;
            }
            if st.total_done != last_done {
                last_done = st.total_done;
            }
            if last_beat.elapsed() >= std::time::Duration::from_secs(5) {
                last_beat = std::time::Instant::now();
            }
            match st.state {
                EngineState::Completed => break Ok(()),
                EngineState::Error => {
                    break Err(SinkError::Failed(
                        st.error.unwrap_or_else(|| "engine error".into()),
                    ))
                }
                _ => {
                    if std::time::Instant::now() >= deadline {
                        break Err(SinkError::Failed("直链传输超时(60s)".into()));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        };
        let _ = self.http.remove(&tid, false).await;
        result
    }

    async fn update_sources(&self, _task_id: &str, _urls: Vec<String>) -> Result<(), SinkError> {
        // v1：直链不续期（真实 provider 的 refresh_links 接入后实现）
        Ok(())
    }
}

/// ProviderError → DaemonError 的人类可读映射。
fn map_provider_err(e: ProviderError) -> DaemonError {
    use ProviderError as P;
    let msg = match e {
        P::ManualOnly => "BT 进度 ≥50%，按兜底策略不允许（仅进度 <50% 可兜底）".to_string(),
        P::RequiresPause => "需先暂停 BT 任务（串行兜底策略）".to_string(),
        P::NoProvider => "无可用 provider（未配置/未认证/配额耗尽/冷却中/并发满）".to_string(),
        P::Expired => "直链已过期且刷新/重提交均失败".to_string(),
        P::RetriesExhausted => "直链过期恢复次数超限（update_sources≤3 + resubmit≤2）".to_string(),
        other => other.to_string(),
    };
    DaemonError::Fallback(msg)
}

/// 解析 `bt://ip:port` 为 `(ip, port)`。
#[cfg(feature = "xunlei-import")]
fn parse_bt_peer(s: &str) -> Option<(String, u16)> {
    let s = s.strip_prefix("bt://")?;
    let mut parts = s.rsplitn(2, ':');
    let port_str = parts.next()?;
    let ip = parts.next()?;
    let port = port_str.parse::<u16>().ok()?;
    Some((ip.to_string(), port))
}

/// B10（§12 D36）：dest_root 预检——缺失目录自动创建 + 可写探测（探针文件）。
/// 空间充足性由 `precheck_space` 在总大小已知时另行检查。
///
/// 安全修复（V2，CWE-22 变体）：`allowed_roots` 白名单——dest 规范化后必须落在
/// 某个白名单根内（拒 symlink 逃逸）；原始输入含 `..` 分量直接拒绝。
/// `allowed_roots` 传空切片 = 不校验（仅测试/serve 初始化自身使用；
/// 生产路径必须传非空，DaemonState 内部兜底 default_dest_root）。
pub fn ensure_dest_root(
    dest: Option<String>,
    allowed_roots: &[PathBuf],
) -> Result<PathBuf, DaemonError> {
    let raw = dest.unwrap_or_else(|| ".".to_string());
    let p = PathBuf::from(&raw);
    // 1) 原始输入拒绝 `..`（canonicalize 前快速拒绝，语义清晰）
    for comp in p.components() {
        if matches!(comp, std::path::Component::ParentDir) {
            return Err(DaemonError::InvalidSource(format!(
                "dest 含 `..` 分量已拒绝: {raw}"
            )));
        }
    }
    fs::create_dir_all(&p)
        .map_err(|e| DaemonError::InvalidSource(format!("目标目录不可创建: {e}")))?;
    // 2) 白名单校验：canonicalize 后比对前缀（同时拦截 symlink 指向白名单外）
    if !allowed_roots.is_empty() {
        let cp = p
            .canonicalize()
            .map_err(|e| DaemonError::InvalidSource(format!("目标目录规范化失败: {e}")))?;
        let inside = allowed_roots.iter().any(|r| {
            // root 不存在则先建（首启场景 root == dest 本身，上一步已建好）
            let _ = fs::create_dir_all(r);
            match r.canonicalize() {
                Ok(cr) => cp.starts_with(&cr),
                Err(_) => false,
            }
        });
        if !inside {
            return Err(DaemonError::InvalidSource(format!(
                "dest 越界（不在允许的下载根目录内）: {raw}"
            )));
        }
    }
    // 3) 可写探针：随机后缀防可预测竞态（V10-3）
    let probe = p.join(format!(
        ".write_probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::write(&probe, b"ok")
        .map_err(|e| DaemonError::InvalidSource(format!("目标目录不可写: {e}")))?;
    let _ = fs::remove_file(&probe);
    Ok(p)
}

/// B10：空间预检（总大小已知时调用）——`evaluate_disk` 判定不足 → 拒绝入队。
/// 安全/健壮性修复（V10-2）：磁盘可用空间取不到（fs2 失败）时不再静默放行——
/// 非严格模式（默认）告警日志 + 放行（保留旧行为）；`strict=true` 时拒绝入队，
/// 防止预检被绕过后续盘写满。由配置 `[download] disk_precheck_strict` 控制。
pub fn precheck_space(p: &Path, total: u64, strict: bool) -> Result<(), DaemonError> {
    let Ok(avail) = fs2::free_space(p) else {
        if strict {
            return Err(DaemonError::InvalidSource(format!(
                "磁盘可用空间不可探测且 disk_precheck_strict=true，拒绝入队: {}",
                p.display()
            )));
        }
        tracing::warn!(
            "磁盘可用空间不可探测，空间预检已跳过（可配置 [download] disk_precheck_strict=true 强制拒绝）: {}",
            p.display()
        );
        return Ok(());
    };
    use smart_dl_core::session::output::{evaluate_disk, DiskCheck};
    if let DiskCheck::Insufficient {
        required,
        available,
    } = evaluate_disk(avail, total)
    {
        return Err(DaemonError::InvalidSource(format!(
            "磁盘空间不足: 需要 {} 字节, 可用 {} 字节",
            required, available
        )));
    }
    Ok(())
}

impl DaemonState {
    /// 应用一条 BT alert 到匹配任务（engine_tid 大小写不敏感归一化比较）：
    /// 状态迁移（`bt_events::transition_for`）+ 引擎缓存写入；返回效果供广播。
    /// 无匹配任务或无迁移 → `None`（调用方丢弃该 alert）。
    ///
    /// Bug B 根因修复：`autosave()` 必须在 tasks 锁【外】调用——autosave →
    /// persisted_tasks 会再次取同一把非重入 std Mutex，锁内调用 = 同线程重入
    /// 自死锁（alert 循环线程永久持锁挂死 → 全部端点含 /config 无限 hang）。
    /// 修复前证据：`aba HIT → autosave BEGIN` 后日志静默 + tasks_free=false 永续。
    #[cfg(feature = "bt")]
    pub fn apply_bt_alert(&self, a: &smart_dl_btcore::Alert) -> Option<BtAlertEffect> {
        let ih_l = a.ih.to_ascii_lowercase();
        let effect = {
            let mut tasks = self.tasks.lock();
            let mut found: Option<BtAlertEffect> = None;
            for (id, rec) in tasks.iter_mut() {
                if rec.engine_kind != EngineKind::Bt {
                    continue;
                }
                let Some(tid) = &rec.engine_tid else {
                    continue;
                };
                if tid.to_ascii_lowercase() != ih_l {
                    continue;
                }
                // 命中任务（每条 alert 至多匹配一个 rec）：无迁移 → 丢弃
                let now = rec.task.state.clone();
                let Some((from, to)) = crate::bt_events::transition_for(&now, a) else {
                    break;
                };
                rec.task.state = to.clone();
                if let Some(es) = rec.engine_status.as_mut() {
                    if to == TaskState::Failed {
                        es.error = Some(a.msg.clone());
                    }
                    // E11：BT 走向非活跃态时轮询缓存仍持最后窗口速率——
                    // 轮询器不再光顾非活跃任务，不清则 /stats 聚合虚高（陈旧速率）。
                    // Seeding 不清：仍是活跃轮询候选，下一轮以引擎实时值刷新。
                    if matches!(
                        to,
                        TaskState::Paused
                            | TaskState::Completed
                            | TaskState::Failed
                            | TaskState::Stopped
                    ) {
                        es.down_rate = 0;
                        es.up_rate = 0;
                    }
                }
                found = Some(BtAlertEffect {
                    task_id: id.clone(),
                    from,
                    to,
                    message: a.msg.clone(),
                });
                break;
            }
            found
        }; // ← tasks 锁在此释放（guard drop）
        if effect.is_some() {
            self.autosave(); // 锁外落盘：状态迁移落盘（修复 Bug B 重入自死锁）
        }
        effect
    }

    /// 引擎状态轮询：HTTP/FTP 任务状态推进（记录权威=引擎实时态）+ 全引擎速率缓存。
    /// 每轮对候选项调用 `engine.status()`：
    /// - 缓存：`EngineStatus` 整体写入 `engine_status`（速率/错误供 `/stats`、
    ///   `task_logs` 读取；运行态字段不落盘，无 autosave 负担）；
    /// - HTTP/FTP：引擎终态（Completed/Error）→ 记录推进 Completed/Failed + 落盘；
    ///   引擎活跃（Downloading/MetadataPending）→ Queued 记录顺带推进 Downloading；
    /// - BT：仅缓存（状态权威 = alert 流，轮询不得双头迁移）。
    ///
    /// 返回本批 HTTP/FTP 迁移效果供事件广播；无变化的任务跳过。
    pub async fn poll_engine_states(&self) -> Vec<HttpPollEffect> {
        // 先收集候选（锁外做引擎调用；避免长持锁）。HTTP/FTP 引擎无 alert 回调，
        // 状态推进依赖轮询；BT 活跃任务仅做速率缓存（Downloading/Seeding——
        // 做种中 up_rate 对 /stats 有意义）。
        let candidates: Vec<(String, EngineTaskId, EngineKind)> = {
            let tasks = self.tasks.lock();
            tasks
                .iter()
                .filter(|(_, rec)| match rec.engine_kind {
                    EngineKind::Http | EngineKind::Ftp => matches!(
                        rec.task.state,
                        TaskState::Queued | TaskState::Downloading(_)
                    ),
                    EngineKind::Bt => matches!(
                        rec.task.state,
                        TaskState::Downloading(_) | TaskState::Seeding
                    ),
                    // provider/xunlei-nas 暂无轮询路径
                    EngineKind::Provider | EngineKind::XunleiNas => false,
                })
                .filter_map(|(id, rec)| {
                    rec.engine_tid
                        .clone()
                        .map(|t| (id.clone(), t, rec.engine_kind))
                })
                .collect()
        };
        let mut effects = Vec::new();
        for (id, tid, kind) in candidates {
            let Ok(engine) = self.engine_for(kind) else {
                continue;
            };
            // 引擎侧已移除/不可用 → 跳过（任务移除后轮询器自然停）
            let Ok(st) = engine.status(&tid).await else {
                continue;
            };
            if matches!(kind, EngineKind::Bt) {
                // BT 缓存分支：只写快照，不迁移不回填不落盘（状态权威 = alert 流）。
                let mut tasks = self.tasks.lock();
                if let Some(rec) = tasks.get_mut(&id) {
                    // 双检：轮询间隙状态可能已被 alert 推进至终态
                    //（终态不缓存——与 apply_bt_alert 的终态清零同口径）
                    if matches!(
                        rec.task.state,
                        TaskState::Downloading(_) | TaskState::Seeding
                    ) {
                        rec.engine_status = Some(st.clone());
                    }
                }
                continue;
            }
            // Bug B 根因修复：autosave 移到锁外（persisted_tasks 重入同一把非重入锁
            // 会同线程自死锁——与 apply_bt_alert 同源缺陷）。
            let mut backfilled = false;
            let advanced: Option<TaskState> = {
                let mut tasks = self.tasks.lock();
                let Some(rec) = tasks.get_mut(&id) else {
                    continue;
                };
                // 双检：轮询间隙状态可能已被别处推进（remove/pause/恢复）
                if !matches!(
                    rec.task.state,
                    TaskState::Queued | TaskState::Downloading(_)
                ) {
                    continue;
                }
                // E11 速率缓存：引擎快照整体入缓存（含速率/错误；运行态不落盘，
                // 不 autosave）。置于回填/迁移之前——to==from 轮次缓存仍刷新。
                rec.engine_status = Some(st.clone());
                // E9 名字回填（幂等）：metadata.name 空缺 + 引擎报了最终落盘名
                // → 回填 + 事件。置于状态迁移判断之前：下载中任务 to==from
                // 不迁移，但回填仍需进行（回填一次成功后 name 非 None 自然停）。
                if rec.task.metadata.name.is_none() {
                    if let Some(n) = &st.name {
                        rec.task.metadata.name = Some(n.clone());
                        rec.push_event("name_backfilled", Some(n.clone()));
                        backfilled = true;
                    }
                }
                let from = rec.task.state.clone();
                let to = engine_state_to_task(&st.state, kind);
                if to == from {
                    // 已在目标态（活跃→活跃）：不迁移，但本轮回填/缓存仍生效
                    None
                } else {
                    // 错误随快照整体入缓存（st.error），无需单独写点
                    rec.task.state = to.clone();
                    Some(from)
                }
            }; // ← tasks 锁在此释放
            if backfilled {
                self.autosave(); // 锁外落盘：名字回填持久化（P4 G5 同口径）
            }
            if advanced.is_some() {
                self.autosave(); // 锁外落盘：终态/推进落盘（修复 Bug B 重入自死锁）
            }
            // 仅真迁移产生 effect（to==from 的纯回填轮次不广播）
            if let Some(from) = advanced {
                effects.push(HttpPollEffect {
                    task_id: id,
                    from,
                    to: engine_state_to_task(&st.state, kind),
                    message: st.error.clone().unwrap_or_default(),
                });
            }
        }
        effects
    }
}

/// D34：canonical URL —— 剥离签名/token 参数后作为去重身份，使同一资源的
/// 带签名链接（token 过期/轮换）仍能识别为同一任务。
/// 黑名单（设计文档 §7 D34）：`token|sig|signature|expires|auth|X-Amz-*|X-Goog-*|X-Tencent-*|X-QiNiu-*`
pub fn canonical_http_url(raw: &str) -> String {
    let Ok(mut u) = url::Url::parse(raw) else {
        return raw.to_string();
    };
    let mut kept: Vec<(String, String)> = Vec::new();
    for (k, v) in u.query_pairs() {
        if !is_token_param(&k) {
            kept.push((k.into_owned(), v.into_owned()));
        }
    }
    if kept.is_empty() {
        u.set_query(None);
    } else {
        let qs: Vec<String> = kept.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
        u.set_query(Some(&qs.join("&")));
    }
    u.to_string()
}

/// 参数名是否命中 D34 token 黑名单（大小写敏感匹配，前缀通配 X-* 云签名族）。
fn is_token_param(name: &str) -> bool {
    matches!(name, "token" | "sig" | "signature" | "expires" | "auth")
        || name.starts_with("X-Amz-")
        || name.starts_with("X-Goog-")
        || name.starts_with("X-Tencent-")
        || name.starts_with("X-QiNiu-")
}

/// 从 magnet 提取 btih（40 hex，v1 规范 xt=urn:btih:）。无 → None（canonical 回落全文）。
#[cfg(feature = "bt")]
pub(crate) fn btih_of(magnet: &str) -> Option<String> {
    magnet.split('&').find_map(|p| {
        let v = p.strip_prefix("xt=urn:btih:")?;
        (v.len() == 40 && v.bytes().all(|b| b.is_ascii_hexdigit())).then(|| v.to_ascii_lowercase())
    })
}

/// 从 .torrent 字节提取 BT infohash（40 hex 小写）= SHA1(info dict 原始字节)。
/// 只做最小 bencode 定位（顶层 dict 找键 `info` → 配对结束 `e` 取整段），
/// 不做完整解析——足以支撑 canonical 查重。
#[cfg(feature = "bt")]
pub fn torrent_infohash(b: &[u8]) -> Option<String> {
    use sha1::Digest;
    let (info, end) = locate_info(b)?;
    let digest = sha1::Sha1::digest(&b[info..=end]);
    Some(
        digest
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect::<String>(),
    )
}

/// 单文件 .torrent 总大小（info dict 内 `length` 字段）；多文件（`files`）→ None。
/// v1 仅覆盖单文件场景（B10 空间预检用）；多文件留后续。
#[cfg(feature = "bt")]
pub fn torrent_total_size(b: &[u8]) -> Option<u64> {
    let (info, end) = locate_info(b)?;
    let mut i = info + 1;
    while i < end {
        let (key, ai) = be_str(b, i)?;
        i = ai;
        match key {
            b"length" => {
                if b.get(i) != Some(&b'i') {
                    return None;
                }
                let e = b[i..].iter().position(|&c| c == b'e')? + i;
                return std::str::from_utf8(&b[i + 1..e]).ok()?.parse().ok();
            }
            b"files" => return None, // 多文件：v1 不解析
            _ => i = value_skip(b, i, 0)?,
        }
    }
    None
}

/// .torrent 空间预检总大小（B10）：优先 TorrentMeta::parse——多文件取 files 各项
/// size 求和、单文件取 file_size；parse 失败 → 回退 torrent_total_size（单文件最小
/// 解析）。两者都拿不到 → None（预检跳过）。
#[cfg(feature = "bt")]
pub fn torrent_precheck_total(b: &[u8]) -> Option<u64> {
    match TorrentMeta::parse(b) {
        Ok(meta) => {
            if meta.files.is_empty() {
                Some(meta.file_size)
            } else {
                Some(meta.files.iter().map(|f| f.size).sum())
            }
        }
        Err(_) => torrent_total_size(b),
    }
}

/// 定位 info dict：返回 (info 值起始 'd' 下标, info dict 闭合 'e' 下标)。
#[cfg(feature = "bt")]
fn locate_info(b: &[u8]) -> Option<(usize, usize)> {
    if b.first() != Some(&b'd') {
        return None;
    }
    let mut i = 1; // 顶层 dict 键值对扫描
    while i < b.len() {
        let (key, after_key) = be_str(b, i)?;
        i = after_key;
        if key == b"info" {
            if b.get(i) != Some(&b'd') {
                return None; // info 必须是 dict
            }
            let end = dict_skip(b, i, 0)?;
            return Some((i, end));
        }
        // 跳过值（结构感知），继续找 `info`
        i = value_skip(b, i, 0)?;
    }
    None
}

/// bencode 字符串 `len:data` → (data, 内容后下标)。
#[cfg(feature = "bt")]
fn be_str(b: &[u8], at: usize) -> Option<(&[u8], usize)> {
    let colon = b[at..].iter().position(|&c| c == b':')? + at;
    let len: usize = std::str::from_utf8(&b[at..colon]).ok()?.parse().ok()?;
    let start = colon + 1;
    // 安全修复（H-3 同型）：start+len 裸加法——恶意 fastresume/torrent 的超大
    // 长度字段在 release 下回绕或直接越界 → 切片 panic。checked_add + 界检查。
    let end = start.checked_add(len)?;
    if end > b.len() {
        return None;
    }
    Some((&b[start..end], end))
}

/// bencode 整数 `i<digits>e` → 值。
#[cfg(feature = "bt")]
fn be_int(b: &[u8], at: usize) -> Option<i64> {
    if b.get(at) != Some(&b'i') {
        return None;
    }
    let e = b[at..].iter().position(|&c| c == b'e')? + at;
    let s = std::str::from_utf8(&b[at + 1..e]).ok()?;
    s.parse().ok()
}

/// dict 结束下标：从 `start`（'d'）按 键(字符串)→值 结构推进到闭合 'e'。
/// 键位置固定为字符串（len: 数字开头），值可为任意类型——值内的数据字节
/// （如 pieces 的 20 字节）不会被误当 len: 解析。
/// 安全修复（V4）：带深度参数，超限返回 None（恶意种子不再能栈溢出 abort）。
#[cfg(feature = "bt")]
fn dict_skip(b: &[u8], start: usize, depth: usize) -> Option<usize> {
    const MAX_DEPTH: usize = 64;
    if depth > MAX_DEPTH {
        return None;
    }
    let mut i = start + 1;
    while b.get(i) != Some(&b'e') {
        let (_, after) = be_str(b, i)?; // 键：字符串
        i = value_skip(b, after, depth + 1)?; // 值：任意类型
    }
    Some(i)
}

/// list 结束下标：从 `start`（'l'）按 值* 推进到闭合 'e'。
#[cfg(feature = "bt")]
fn list_skip(b: &[u8], start: usize, depth: usize) -> Option<usize> {
    let mut i = start + 1;
    while b.get(i) != Some(&b'e') {
        i = value_skip(b, i, depth + 1)?;
    }
    Some(i)
}

/// 跳过任意 bencode 值（dict/list/int/str），返回其后的下标。
#[cfg(feature = "bt")]
fn value_skip(b: &[u8], i: usize, depth: usize) -> Option<usize> {
    match b.get(i)? {
        b'd' => dict_skip(b, i, depth).map(|e| e + 1),
        b'l' => list_skip(b, i, depth).map(|e| e + 1),
        b'i' => {
            let e = b[i..].iter().position(|&c| c == b'e')? + i;
            Some(e + 1)
        }
        _ => be_str(b, i).map(|(_, after)| after),
    }
}

/// 引擎状态 → 对外任务状态（快照实时化；元数据获取中归入 Downloading）。
fn engine_state_to_task(st: &EngineState, kind: EngineKind) -> TaskState {
    match st {
        EngineState::MetadataPending | EngineState::Downloading => TaskState::Downloading(kind),
        EngineState::Paused => TaskState::Paused,
        EngineState::Completed => TaskState::Completed,
        EngineState::Seeding => TaskState::Seeding,
        EngineState::Error => TaskState::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::canonical_http_url;

    #[test]
    fn strips_token_param_keeps_others() {
        let c = canonical_http_url("https://host/a?token=abc&x=1&y=2");
        assert_eq!(c, "https://host/a?x=1&y=2");
    }

    #[test]
    fn strips_cloud_signing_family() {
        let c = canonical_http_url(
            "https://host/a?X-Amz-Signature=deadbeef&X-Amz-Date=20260101&sig=zz&expires=999999",
        );
        assert_eq!(c, "https://host/a");
    }

    #[test]
    fn no_token_url_unchanged() {
        let raw = "https://host/a?x=1";
        assert_eq!(canonical_http_url(raw), raw);
    }

    #[test]
    fn only_token_difference_collides() {
        let a = canonical_http_url("https://host/f?token=aaa&v=1");
        let b = canonical_http_url("https://host/f?v=1&token=bbb");
        assert_eq!(a, b);
    }

    #[test]
    fn invalid_url_passthrough() {
        assert_eq!(canonical_http_url("not a url"), "not a url");
    }

    #[test]
    fn fragment_and_path_unaffected() {
        let c = canonical_http_url("https://host/dir/file.bin?token=x&keep=1#frag");
        assert_eq!(c, "https://host/dir/file.bin?keep=1#frag");
    }
}

/// BT alert 事件流单元测试（feature `bt`）：`transition_for` 迁移矩阵 + `apply_bt_alert`
/// 匹配/缓存写入。不依赖真实 libtorrent 会话（手工构造 TaskRecord）。
#[cfg(all(test, feature = "bt"))]
mod bt_alert_tests {
    use super::*;
    use smart_dl_btcore::{Alert, AlertKind};

    fn make_state_with(rec: TaskRecord) -> DaemonState {
        let engine = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
        let state = DaemonState::new(Arc::new(engine), vec![]);
        // 测试同 crate 内可访问私有 tasks 表
        (*state.tasks.lock()).insert(rec.task.id.clone(), rec);
        state
    }

    fn bt_rec(state: TaskState, ih: &str) -> TaskRecord {
        TaskRecord {
            task: DownloadTask {
                id: "t1".into(),
                canonical_id: CanonicalId {
                    kind: CanonicalKind::Bt,
                    identity: ih.to_string(),
                    validator: None,
                    token_sensitive: false,
                },
                source: DownloadSource::Magnet(format!("magnet:?xt=urn:btih:{ih}")),
                identity: ContentIdentity::SingleFile {
                    size: 0,
                    etag: None,
                    sha256: None,
                    backup_md5: None,
                },
                dest_root: PathBuf::from("."),
                files: vec![],
                acquisitions: vec![],
                aggregate: Default::default(),
                state,
                retry: Default::default(),
                created_at: std::time::Instant::now(),
                file_priorities: None,
                sequential: false,
                metadata: TaskMetadata {
                    name: None,
                    added_at_unix: 0,
                },
                limits: None,
            },
            engine_tid: Some(ih.to_string()),
            engine_kind: EngineKind::Bt,
            engine_status: None,
            events: vec![],
        }
    }

    #[test]
    fn finished_alert_promotes_seeding() {
        let state = make_state_with(bt_rec(TaskState::Downloading(EngineKind::Bt), "ABC123"));
        let alert = Alert {
            kind: AlertKind::State,
            ih: "abc123".into(), // 大小写不同 → 归一化匹配
            msg: "torrent finished downloading".into(),
            at: 0,
            resume_ready: false,
        };
        let eff = state.apply_bt_alert(&alert).unwrap();
        assert_eq!(eff.from, TaskState::Downloading(EngineKind::Bt));
        assert_eq!(eff.to, TaskState::Seeding);
        let rec_lock = state.tasks.lock();
        let rec = rec_lock.get("t1").unwrap();
        assert_eq!(rec.task.state, TaskState::Seeding, "任务记录状态必须落盘");
    }

    #[test]
    fn finished_from_queued_also_promotes() {
        // 任务还未被引擎快照驱动（仍 Queued）时，完成 alert 同样推进
        let state = make_state_with(bt_rec(TaskState::Queued, "AABB"));
        let alert = Alert {
            kind: AlertKind::State,
            ih: "aabb".into(),
            msg: "torrent finished downloading".into(),
            at: 0,
            resume_ready: false,
        };
        let eff = state.apply_bt_alert(&alert).unwrap();
        assert_eq!(eff.from, TaskState::Queued);
        assert_eq!(eff.to, TaskState::Seeding);
    }

    #[test]
    fn error_alert_fails_with_message() {
        let state = make_state_with(bt_rec(TaskState::Downloading(EngineKind::Bt), "D9E8"));
        let alert = Alert {
            kind: AlertKind::State,
            ih: "d9e8".into(),
            msg: "torrent error: pex failed".into(),
            at: 0,
            resume_ready: false,
        };
        let eff = state.apply_bt_alert(&alert).unwrap();
        assert_eq!(eff.to, TaskState::Failed);
        assert_eq!(eff.message, "torrent error: pex failed");
        let rec_lock = state.tasks.lock();
        let rec = rec_lock.get("t1").unwrap();
        assert_eq!(rec.task.state, TaskState::Failed);
    }

    #[test]
    fn paused_alert_ignored() {
        // v1 不处理 Paused alert（pause 由 API 直调时同步发布事件）
        let state = make_state_with(bt_rec(TaskState::Downloading(EngineKind::Bt), "P1"));
        let alert = Alert {
            kind: AlertKind::State,
            ih: "p1".into(),
            msg: "torrent paused".into(),
            at: 0,
            resume_ready: false,
        };
        assert!(state.apply_bt_alert(&alert).is_none());
    }

    #[test]
    fn finished_alert_promotes_paused_to_seeding() {
        // Bug C：BT 在记录态 Paused 下被引擎实际完成（Bug A 复活后跑完），
        // Finished alert 应允许推进到 Seeding，避免记录态与引擎态错位。
        let state = make_state_with(bt_rec(TaskState::Paused, "C1"));
        let alert = Alert {
            kind: AlertKind::State,
            ih: "c1".into(),
            msg: "torrent finished downloading".into(),
            at: 0,
            resume_ready: false,
        };
        let eff = state.apply_bt_alert(&alert).unwrap();
        assert_eq!(eff.from, TaskState::Paused);
        assert_eq!(eff.to, TaskState::Seeding);
        let rec_lock = state.tasks.lock();
        let rec = rec_lock.get("t1").unwrap();
        assert_eq!(rec.task.state, TaskState::Seeding, "记录态必须落盘");
    }

    #[test]
    fn non_bt_task_ignored() {
        // HTTP 任务（engine_kind=Http）不匹配 BT alert
        let mut rec = bt_rec(TaskState::Downloading(EngineKind::Bt), "XT77");
        rec.engine_kind = EngineKind::Http;
        let state = make_state_with(rec);
        let alert = Alert {
            kind: AlertKind::State,
            ih: "xt77".into(),
            msg: "torrent finished downloading".into(),
            at: 0,
            resume_ready: false,
        };
        assert!(state.apply_bt_alert(&alert).is_none());
    }

    #[test]
    fn unknown_ih_ignored() {
        let state = make_state_with(bt_rec(TaskState::Downloading(EngineKind::Bt), "KN0WN"));
        let alert = Alert {
            kind: AlertKind::State,
            ih: "na-".into(),
            msg: "torrent finished downloading".into(),
            at: 0,
            resume_ready: false,
        };
        assert!(state.apply_bt_alert(&alert).is_none());
    }

    #[test]
    fn error_alert_zeroes_cached_rates() {
        // E11：轮询缓存持最后窗口速率，Error alert → Failed（非活跃终态）
        // → 速率清零，否则 /stats 聚合把陈旧速率计入失败任务。
        let mut rec = bt_rec(TaskState::Downloading(EngineKind::Bt), "RATES01");
        rec.engine_status = Some(EngineStatus {
            down_rate: 3000,
            up_rate: 1500,
            ..EngineStatus::default()
        });
        let state = make_state_with(rec);
        let alert = Alert {
            kind: AlertKind::State,
            ih: "rates01".into(),
            msg: "torrent error: pex failed".into(),
            at: 0,
            resume_ready: false,
        };
        let eff = state.apply_bt_alert(&alert).unwrap();
        assert_eq!(eff.to, TaskState::Failed);
        let rec_lock = state.tasks.lock();
        let es = rec_lock.get("t1").unwrap().engine_status.as_ref().unwrap();
        assert_eq!(es.down_rate, 0, "Failed 后缓存下行速率必须清零");
        assert_eq!(es.up_rate, 0, "Failed 后缓存上行速率必须清零");
        assert_eq!(
            es.error.as_deref(),
            Some("torrent error: pex failed"),
            "错误信息随迁移写入缓存（task_logs 读取口径）"
        );
    }

    #[test]
    fn finished_alert_keeps_rates_for_seeding_poll() {
        // Finished → Seeding：不做种前清零（Seeding 仍是活跃轮询候选，
        // 下一轮以引擎实时值刷新——上行速率对 /stats 有意义）。
        let mut rec = bt_rec(TaskState::Downloading(EngineKind::Bt), "RATES02");
        rec.engine_status = Some(EngineStatus {
            down_rate: 3000,
            up_rate: 1500,
            ..EngineStatus::default()
        });
        let state = make_state_with(rec);
        let alert = Alert {
            kind: AlertKind::State,
            ih: "rates02".into(),
            msg: "torrent finished downloading".into(),
            at: 0,
            resume_ready: false,
        };
        let eff = state.apply_bt_alert(&alert).unwrap();
        assert_eq!(eff.to, TaskState::Seeding);
        let rec_lock = state.tasks.lock();
        let es = rec_lock.get("t1").unwrap().engine_status.as_ref().unwrap();
        assert_eq!(es.down_rate, 3000, "Seeding 不清零（活跃候选待刷新）");
        assert_eq!(es.up_rate, 1500);
    }

    #[test]
    fn peer_alert_ignored() {
        let state = make_state_with(bt_rec(TaskState::Downloading(EngineKind::Bt), "PR99"));
        let alert = Alert {
            kind: AlertKind::Peer,
            ih: "pr99".into(),
            msg: "peer connected".into(),
            at: 0,
            resume_ready: false,
        };
        assert!(state.apply_bt_alert(&alert).is_none());
    }

    /// Bug B 回归（重入自死锁）：storage 启用 + Finished alert 迁移 → autosave。
    /// 修复前：apply_bt_alert 持 tasks 锁调用 autosave → persisted_tasks 同线程
    /// 重入同一把非重入 Mutex → 本测试 5s 超时失败（真实事故 = 全端点永久 hang）。
    #[tokio::test(flavor = "multi_thread")]
    async fn finished_alert_with_storage_autosave_no_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let engine = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
        let state = DaemonState::new(Arc::new(engine), vec![]).with_storage(store.clone());
        let rec = bt_rec(TaskState::Queued, "BEEF");
        (*state.tasks.lock()).insert(rec.task.id.clone(), rec);
        let alert = Alert {
            kind: AlertKind::State,
            ih: "beef".into(),
            msg: "torrent finished downloading".into(),
            at: 0,
            resume_ready: false,
        };
        let work = state.apply_bt_alert(&alert);
        let eff = tokio::time::timeout(std::time::Duration::from_secs(5), async move { work })
            .await
            .expect("apply_bt_alert 死锁（Bug B 重入回归）");
        assert!(eff.is_some(), "Finished alert 应产生 Seeding 迁移");
        assert_eq!(eff.unwrap().to, TaskState::Seeding);
        // 落盘确实发生（autosave 在锁外完成）
        assert!(store.exists(), "状态迁移应触发持久化落盘");
    }
}

/// torrent_infohash（bencode 最小解析）单元测试。
#[cfg(all(test, feature = "bt"))]
mod torrent_tests {
    use super::*;

    /// 最小合法 .torrent：d4:info<infodict>e
    fn sample_torrent() -> Vec<u8> {
        let mut t = b"d4:infod6:lengthi123e4:name4:test12:piece lengthi16384e6:pieces20:".to_vec();
        t.extend_from_slice(&[0xAB; 20]);
        t.extend_from_slice(b"ee");
        t
    }

    #[test]
    fn extracts_infohash_from_info_dict() {
        let ih = torrent_infohash(&sample_torrent()).unwrap();
        // 预计算：SHA1(info dict) = 7ac2e18f...（info dict = t[7..=86]，80B）
        assert_eq!(ih, "7ac2e18f65f50b19e6bb1069e15ff2398aac220d");
    }

    #[test]
    fn rejects_non_dict_root() {
        assert!(torrent_infohash(b"nonsense").is_none());
        assert!(torrent_infohash(b"").is_none());
    }

    #[test]
    fn rejects_missing_info_key() {
        // 合法 bencode dict 但无 info 键
        let t = b"d3:foo3:bare";
        assert!(torrent_infohash(t).is_none());
    }

    #[test]
    fn skips_values_before_info_key() {
        // info 前有其他键值（含嵌套 list/int）仍能定位
        let mut t = b"d5:hello5:world7:payloadli3ee4:info".to_vec();
        t.extend_from_slice(&sample_torrent()[7..]); // info dict 起点起 + 顶层 e
        let ih = torrent_infohash(&t).unwrap();
        assert_eq!(ih, "7ac2e18f65f50b19e6bb1069e15ff2398aac220d");
    }

    #[test]
    fn total_size_single_file() {
        let t = sample_torrent();
        // length=123（单文件）
        assert_eq!(torrent_total_size(&t), Some(123));
    }

    #[test]
    fn total_size_multi_file_returns_none() {
        // 多文件 torrent：files 列表 → v1 None
        let mut t =
            b"d4:infod5:filesld6:lengthi10e4:pathl1:aeed6:lengthi20e4:pathl1:beeee".to_vec();
        assert_eq!(torrent_total_size(&t), None);
        let _ = &mut t;
    }

    #[test]
    fn total_size_missing_length_none() {
        let mut t = b"d4:info4:name4:teste".to_vec();
        assert_eq!(torrent_total_size(&t), None);
        let _ = &mut t;
    }

    #[test]
    fn parse_minimal_single_file_torrent() {
        // 构造最小单文件 torrent（bencode）
        let mut torrent = Vec::new();
        torrent.extend_from_slice(b"d"); // dict
        torrent.extend_from_slice(b"8:announce14:http://tracker"); // announce
        torrent.extend_from_slice(b"4:infod"); // info dict
        torrent.extend_from_slice(b"6:lengthi12345e"); // length
        torrent.extend_from_slice(b"4:name8:filename"); // name
        torrent.extend_from_slice(b"12:piece lengthi16384e"); // piece length
        torrent.extend_from_slice(b"6:pieces"); // pieces key
        torrent.extend_from_slice(b"20:"); // pieces value 长度前缀
        torrent.extend_from_slice(&[0u8; 20]); // dummy piece hash
        torrent.extend_from_slice(b"ee"); // end info dict, end dict

        let meta = TorrentMeta::parse(&torrent).unwrap();
        assert_eq!(meta.info_hash.len(), 40);
        assert_eq!(meta.piece_length, 16384);
        assert_eq!(meta.pieces_hash.len(), 1);
        assert_eq!(meta.name, "filename");
        assert_eq!(meta.file_size, 12345);
    }

    #[test]
    fn parse_multi_file() {
        // 多文件 torrent 应被解析（files 列表含 2 个文件）
        let mut torrent = Vec::new();
        torrent.extend_from_slice(b"d4:infod"); // 顶层 + info dict
        torrent.extend_from_slice(b"12:piece lengthi16384e"); // piece length
        torrent.extend_from_slice(b"6:pieces20:"); // pieces (1 piece)
        torrent.extend_from_slice(&[0u8; 20]);
        torrent.extend_from_slice(b"4:name8:multidir"); // name
        torrent.extend_from_slice(b"5:filesl"); // files list
                                                // 文件1: length=10, path=["a"]
        torrent.extend_from_slice(b"d6:lengthi10e4:pathl1:aee");
        // 文件2: length=20, path=["b"]
        torrent.extend_from_slice(b"d6:lengthi20e4:pathl1:bee");
        torrent.extend_from_slice(b"e"); // end files list
        torrent.extend_from_slice(b"ee"); // end info dict + top dict

        let meta = TorrentMeta::parse(&torrent).unwrap();
        assert_eq!(meta.files.len(), 2, "应解析出 2 个文件");
        assert_eq!(meta.files[0].path, "a");
        assert_eq!(meta.files[0].size, 10);
        assert_eq!(meta.files[1].path, "b");
        assert_eq!(meta.files[1].size, 20);
    }

    #[test]
    fn precheck_total_multi_file_sums_files() {
        // 多文件 torrent：预检总量 = 各文件 size 之和（torrent_total_size 遇 files
        // 返回 None，此处验证升级后 parse 路径生效）
        let mut torrent = Vec::new();
        torrent.extend_from_slice(b"d4:infod");
        torrent.extend_from_slice(b"12:piece lengthi16384e");
        torrent.extend_from_slice(b"6:pieces20:");
        torrent.extend_from_slice(&[0u8; 20]);
        torrent.extend_from_slice(b"4:name8:multidir");
        torrent.extend_from_slice(b"5:filesl");
        torrent.extend_from_slice(b"d6:lengthi10e4:pathl1:aee");
        torrent.extend_from_slice(b"d6:lengthi20e4:pathl1:bee");
        torrent.extend_from_slice(b"e");
        torrent.extend_from_slice(b"ee");
        assert_eq!(torrent_precheck_total(&torrent), Some(30));
    }

    #[test]
    fn precheck_total_single_file_uses_length() {
        let t = sample_torrent();
        // length=123（单文件走 file_size）
        assert_eq!(torrent_precheck_total(&t), Some(123));
    }

    #[test]
    fn precheck_total_parse_fail_falls_back() {
        // TorrentMeta::parse 失败（缺 piece length/pieces）但 info dict 内 length
        // 可定位 → 回退 torrent_total_size 路径
        let t = b"d4:infod6:lengthi999e4:name4:testee";
        assert_eq!(torrent_precheck_total(t), Some(999));
    }

    /// xunlei 导入测试：add_xunlei_import_task / xunlei_convert 仅在
    /// xunlei-import feature 下存在，单独门控以免 --features bt 编译失败。
    #[cfg(all(test, feature = "xunlei-import"))]
    mod xunlei_import_tests {
        use super::*;
        use std::path::PathBuf;

        #[tokio::test]
        async fn add_xunlei_import_task_e2e() {
            // 使用 tools/xunlei-migrate/e2e_out 合成样本测试完整导入流程
            let e2e = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
                .join("../../tools/xunlei-migrate/e2e_out");
            let torrent_path = e2e.join("test.torrent");
            let cfg_path = e2e.join("test.xlbt.cfg");
            let xltd_path = e2e.join("test.bt.xltd");
            if !torrent_path.exists() || !cfg_path.exists() || !xltd_path.exists() {
                eprintln!("SKIP: e2e files not found at {:?}", e2e);
                return;
            }

            let torrent = std::fs::read(&torrent_path).expect("read torrent");
            let cfg = std::fs::read(&cfg_path).expect("read cfg");
            let xltd = std::fs::read(&xltd_path).expect("read xltd");

            // 构造 DaemonState（FakeEngine 作为 BT 引擎）
            let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
            let dir = tempfile::tempdir().expect("tempdir");
            let state =
                DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());

            let tid = state
                .add_xunlei_import_task(torrent, cfg, vec![xltd], None)
                .await
                .expect("add_xunlei_import_task should succeed");

            // 验证任务已创建
            let tasks = state.tasks.lock();
            let rec = tasks.get(&tid).expect("task exists");
            assert_eq!(rec.task.state, TaskState::Queued);
            assert_eq!(rec.engine_kind, EngineKind::Bt);
            assert!(rec.engine_tid.is_some());
            assert_eq!(fake.xunlei_resumes().len(), 1);
        }

        #[tokio::test]
        async fn add_xunlei_import_task_requires_matching_xltds() {
            // 多文件 torrent（2 文件）传入 0 个 xltd → 数量不匹配
            let mut torrent = Vec::new();
            torrent.extend_from_slice(b"d4:infod");
            torrent.extend_from_slice(b"12:piece lengthi16384e");
            torrent.extend_from_slice(b"6:pieces20:");
            torrent.extend_from_slice(&[0u8; 20]);
            torrent.extend_from_slice(b"4:name8:multidir");
            torrent.extend_from_slice(b"5:filesl");
            torrent.extend_from_slice(b"d6:lengthi10e4:pathl1:aee");
            torrent.extend_from_slice(b"d6:lengthi20e4:pathl1:bee");
            torrent.extend_from_slice(b"e");
            torrent.extend_from_slice(b"ee");

            let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
            let dir = tempfile::tempdir().expect("tempdir");
            let state =
                DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());

            let err = state
                .add_xunlei_import_task(torrent, vec![], vec![], None)
                .await
                .expect_err("应拒绝 xltd 数量不匹配");
            assert!(
                err.to_string().contains("不匹配"),
                "错误信息应提示数量不匹配: {err}"
            );
            assert_eq!(fake.xunlei_resumes().len(), 0);
        }

        #[tokio::test]
        async fn add_xunlei_import_task_rejects_duplicate() {
            // 使用 e2e 合成样本
            let e2e = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
                .join("../../tools/xunlei-migrate/e2e_out");
            let torrent_path = e2e.join("test.torrent");
            let cfg_path = e2e.join("test.xlbt.cfg");
            let xltd_path = e2e.join("test.bt.xltd");
            if !torrent_path.exists() || !cfg_path.exists() || !xltd_path.exists() {
                eprintln!("SKIP: e2e files not found at {:?}", e2e);
                return;
            }

            let torrent = std::fs::read(&torrent_path).expect("read torrent");
            let cfg = std::fs::read(&cfg_path).expect("read cfg");
            let xltd = std::fs::read(&xltd_path).expect("read xltd");

            let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
            let dir = tempfile::tempdir().expect("tempdir");
            let state =
                DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());

            // 第一次导入成功
            let tid1 = state
                .add_xunlei_import_task(torrent.clone(), cfg.clone(), vec![xltd.clone()], None)
                .await
                .expect("first import should succeed");

            // 第二次导入相同 infohash → Duplicate
            let err = state
                .add_xunlei_import_task(torrent, cfg, vec![xltd], None)
                .await
                .expect_err("应拒绝重复 infohash");
            assert!(
                err.to_string().contains("duplicate") || err.to_string().contains("重复"),
                "错误信息应提示重复: {err}"
            );
            // 验证只有第一次任务被创建
            let tasks = state.tasks.lock();
            assert!(tasks.contains_key(&tid1));
            assert_eq!(tasks.len(), 1);
        }

        #[tokio::test]
        async fn add_xunlei_import_task_rejects_bad_torrent() {
            let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
            let dir = tempfile::tempdir().expect("tempdir");
            let state =
                DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());

            let err = state
                .add_xunlei_import_task(vec![], vec![], vec![], None)
                .await
                .expect_err("应拒绝无效 torrent");
            assert!(
                err.to_string().contains("解析失败") || err.to_string().contains("无法定位"),
                "错误信息应提示解析失败: {err}"
            );
            assert_eq!(fake.xunlei_resumes().len(), 0);
        }

        #[tokio::test]
        async fn add_xunlei_import_task_marks_lenient_partial() {
            // 读取预生成的 partial_test.torrent（16KB piece, 16 pieces, 256KB 文件）
            let e2e = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
                .join("../../tools/xunlei-migrate/e2e_out");
            let torrent_path = e2e.join("partial_test.torrent");
            if !torrent_path.exists() {
                eprintln!("SKIP: partial_test.torrent not found");
                return;
            }
            let torrent = std::fs::read(&torrent_path).expect("read torrent");
            let info_hash = torrent_infohash(&torrent).expect("infohash");

            // 构造 minimal cfg（magic + infohash + 头部观测值）
            let mut cfg = Vec::new();
            cfg.extend_from_slice(b"XDLCTX\x00\x00");
            cfg.extend_from_slice(&[0u8; 16]); // 0x08-0x17 随机区
                                               // 0x18-0x37 8 个 u32 观测值（与 e2e 样本一致）
            cfg.extend_from_slice(&30025u32.to_le_bytes());
            cfg.extend_from_slice(&7u32.to_le_bytes());
            cfg.extend_from_slice(&0u32.to_le_bytes());
            cfg.extend_from_slice(&4u32.to_le_bytes());
            cfg.extend_from_slice(&0u32.to_le_bytes());
            cfg.extend_from_slice(&28584u32.to_le_bytes());
            cfg.extend_from_slice(&4u32.to_le_bytes());
            cfg.extend_from_slice(&262145u32.to_le_bytes());
            cfg.extend_from_slice(&40u32.to_le_bytes()); // 0x38 infohash 长度 = 40 (u32)
            cfg.extend_from_slice(info_hash.as_bytes()); // 0x3C-0x63 ASCII infohash

            // 构造 xltd：piece 5 为 partial（前 8192 字节非零 = 50%）
            let piece_length = 16384usize;
            let file_size = 256 * 1024u64;
            let xltd_size = ((file_size + 4095) / 4096 * 4096) as usize;
            let mut xltd = vec![0u8; xltd_size];
            let p5_offset = 5 * piece_length;
            for i in 0..8192 {
                xltd[p5_offset + i] = 0xAB;
            }

            let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
            let dir = tempfile::tempdir().expect("tempdir");
            let state =
                DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());

            let tid = state
                .add_xunlei_import_task(torrent, cfg, vec![xltd], None)
                .await
                .expect("add_xunlei_import_task should succeed");

            let tasks = state.tasks.lock();
            let rec = tasks.get(&tid).expect("task exists");
            assert_eq!(rec.task.state, TaskState::Queued);
            assert_eq!(rec.engine_kind, EngineKind::Bt);
            assert!(rec.engine_tid.is_some());
            assert_eq!(fake.xunlei_resumes().len(), 1);

            // 验证 bitfield：piece 0 和 piece 5 应被标记为完成（lenient 策略）
            let resumes = fake.xunlei_resumes();
            let fastresume = resumes.last().unwrap();
            let bened =
                xunlei_convert::fastresume::bdecode(fastresume).expect("bdecode fastresume");
            let pieces = bened
                .dict_get(b"pieces")
                .expect("pieces")
                .as_bytes()
                .expect("pieces bytes");
            assert_eq!(pieces.len(), 2); // 16 pieces = 2 bytes
            assert_eq!(
                pieces[0], 0x84,
                "piece 0 and 5 should be set: got 0x{:02X}",
                pieces[0]
            );
        }
    }
}

/// B10 预检单元测试（ensure_dest_root / precheck_space）。
#[cfg(test)]
mod b10_tests {
    use super::{ensure_dest_root, precheck_space, DaemonError};

    #[test]
    fn creates_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nested/deep");
        let p = ensure_dest_root(
            Some(missing.to_string_lossy().into_owned()),
            &[dir.path().to_path_buf()],
        )
        .unwrap();
        assert!(p.is_dir(), "缺失目录应自动创建");
    }

    #[test]
    fn default_is_dot() {
        let p = ensure_dest_root(None, &[]).unwrap();
        assert!(p.is_dir());
    }

    #[test]
    fn invalid_path_rejected() {
        // Windows：非法路径字符 → 创建失败 → InvalidSource
        let r = ensure_dest_root(Some("a/b*c/d".into()), &[]);
        if let Err(DaemonError::InvalidSource(msg)) = r {
            assert!(msg.contains("不可创建") || msg.contains("不可写"));
        } else {
            // 某些平台可能允许——不强断言，仅确认类型
            assert!(r.is_ok() || r.is_err());
        }
    }

    // 安全回归（V2）：dest 越界必须被白名单拦截。
    #[test]
    fn dest_outside_allowed_roots_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("dl");
        std::fs::create_dir_all(&root).unwrap();
        // 白名单内的子目录 → 放行
        assert!(ensure_dest_root(
            Some(root.join("sub").to_string_lossy().into_owned()),
            std::slice::from_ref(&root)
        )
        .is_ok());
        // 白名单外的目录 → 拒绝
        let outside = dir.path().join("elsewhere");
        let r = ensure_dest_root(
            Some(outside.to_string_lossy().into_owned()),
            std::slice::from_ref(&root),
        );
        assert!(matches!(r, Err(DaemonError::InvalidSource(m)) if m.contains("越界")));
        // 绝对路径穿越到白名单外 → 拒绝
        let r2 = ensure_dest_root(Some(dir.path().to_string_lossy().into_owned()), &[root]);
        assert!(r2.is_err(), "白名单父目录本身也必须被拒");
    }

    // 安全回归（V2）：`..` 分量在 canonicalize 前即被拒。
    #[test]
    fn dest_with_dotdot_rejected_early() {
        let dir = tempfile::tempdir().unwrap();
        let r = ensure_dest_root(
            Some(
                dir.path()
                    .join("sub/../../escape")
                    .to_string_lossy()
                    .into_owned(),
            ),
            &[dir.path().to_path_buf()],
        );
        assert!(matches!(r, Err(DaemonError::InvalidSource(m)) if m.contains("..")));
    }

    #[test]
    fn check_space_zero_total_ok() {
        let dir = tempfile::tempdir().unwrap();
        assert!(precheck_space(dir.path(), 0, false).is_ok());
    }
}

/// FakeEngine 限速调用记录：（engine_tid, down_kb_s, up_kb_s）。
#[cfg(test)]
type LimitCall = (String, Option<u32>, Option<u32>);

/// 假引擎（持久化恢复测试用）：add 记录输入、可对指定 url 返回错误。
#[cfg(test)]
pub struct FakeEngine {
    kind: EngineKind,
    counter: std::sync::atomic::AtomicU64,
    fail_urls: parking_lot::Mutex<std::collections::HashSet<String>>,
    added: parking_lot::Mutex<Vec<String>>,
    xunlei: parking_lot::Mutex<Vec<Vec<u8>>>,
    /// 已注入的 web seed（(engine_tid, url)，F5 webseed 注入测试用）。
    url_seeds: parking_lot::Mutex<Vec<(String, String)>>,
    /// 已下发的任务级限速（LimitCall 记录，限速重放测试用）。
    limits: parking_lot::Mutex<Vec<LimitCall>>,
    /// status() 额外透出的文件级进度（目录 files 同步测试用；默认空 = 保持旧行为）。
    status_files: parking_lot::Mutex<Vec<FileProgress>>,
    /// 已下发的子文件优先级（(engine_tid, pairs)，优先级重放测试用）。
    #[allow(clippy::type_complexity)] // (tid, [(下标, 优先级)]) 记录类型，测试专用
    prio_calls: parking_lot::Mutex<Vec<(String, Vec<(usize, u32)>)>>,
    /// file_priorities() 的可编程行为：None = 默认成功返回空表；
    /// Some(Err(e)) = 返回该错误（metadata 未就绪模拟）；Some(Ok(v)) = 返回 v。
    prio_readback:
        parking_lot::Mutex<Option<Result<Vec<Option<u32>>, smart_dl_core::types::EngineError>>>,
    /// 已下发的 pause 调用（P4 G5 暂停意图重放测试用）。
    paused: parking_lot::Mutex<Vec<String>>,
    /// 已下发的 resume 调用（P4 G5 运行态恢复重放测试用）。
    resumed: parking_lot::Mutex<Vec<String>>,
    /// 已下发的 remove 调用（(engine_tid, delete_data)，E7 数据处置断言用）。
    removed: parking_lot::Mutex<Vec<(String, bool)>>,
    /// 已下发的 set_task_proxy 调用（(engine_tid, proxy)，E8 热改断言用）。
    proxy_sets: parking_lot::Mutex<Vec<(String, Option<String>)>>,
    /// status().name 可编程回显（E9 名字回填测试用；None = 不透出）。
    status_name: parking_lot::Mutex<Option<String>>,
    /// status() 速率回显（(down, up) B/s，E11 速率缓存测试用；默认 (0,0) 旧行为）。
    status_rates: parking_lot::Mutex<(u64, u64)>,
}

#[cfg(test)]
impl FakeEngine {
    pub fn new(kind: EngineKind) -> Self {
        FakeEngine {
            kind,
            counter: std::sync::atomic::AtomicU64::new(1),
            fail_urls: parking_lot::Mutex::new(std::collections::HashSet::new()),
            added: parking_lot::Mutex::new(Vec::new()),
            xunlei: parking_lot::Mutex::new(Vec::new()),
            url_seeds: parking_lot::Mutex::new(Vec::new()),
            limits: parking_lot::Mutex::new(Vec::new()),
            status_files: parking_lot::Mutex::new(Vec::new()),
            prio_calls: parking_lot::Mutex::new(Vec::new()),
            prio_readback: parking_lot::Mutex::new(None),
            paused: parking_lot::Mutex::new(Vec::new()),
            resumed: parking_lot::Mutex::new(Vec::new()),
            removed: parking_lot::Mutex::new(Vec::new()),
            proxy_sets: parking_lot::Mutex::new(Vec::new()),
            status_name: parking_lot::Mutex::new(None),
            status_rates: parking_lot::Mutex::new((0, 0)),
        }
    }

    pub fn fail_url(&self, url: &str) {
        self.fail_urls.lock().insert(url.to_string());
    }

    pub fn added(&self) -> Vec<String> {
        self.added.lock().clone()
    }

    pub fn xunlei_resumes(&self) -> Vec<Vec<u8>> {
        self.xunlei.lock().clone()
    }

    /// 读取已记录的 web seed 注入（(engine_tid, url)，F5 webseed 测试断言用）。
    pub fn url_seeds(&self) -> Vec<(String, String)> {
        self.url_seeds.lock().clone()
    }

    /// 读取已下发的任务级限速（LimitCall 记录，限速重放测试断言用）。
    pub fn limits(&self) -> Vec<LimitCall> {
        self.limits.lock().clone()
    }

    /// 读取已下发的 pause 调用（P4 G5 重放测试断言用）。
    pub fn paused_calls(&self) -> Vec<String> {
        self.paused.lock().clone()
    }

    /// 读取已下发的 remove 调用（(engine_tid, delete_data)，E7 数据处置断言用）。
    pub fn removed_calls(&self) -> Vec<(String, bool)> {
        self.removed.lock().clone()
    }

    /// 读取已下发的 set_task_proxy 调用（(engine_tid, proxy)，E8 热改断言用）。
    pub fn proxy_set_calls(&self) -> Vec<(String, Option<String>)> {
        self.proxy_sets.lock().clone()
    }

    /// 设置 status().name 回显值（E9 名字回填测试：模拟引擎已定落盘名）。
    pub fn set_status_name(&self, name: &str) {
        *self.status_name.lock() = Some(name.to_string());
    }

    /// 设置 status() 速率回显（(down, up) B/s；E11 速率缓存测试用）。
    pub fn set_status_rates(&self, down: u64, up: u64) {
        *self.status_rates.lock() = (down, up);
    }

    /// 读取已下发的 resume 调用（P4 G5 重放测试断言用）。
    pub fn resumed_calls(&self) -> Vec<String> {
        self.resumed.lock().clone()
    }

    /// 设置 status() 返回的文件级进度（FTP 目录 files 同步测试注入用）。
    pub fn set_status_files(&self, files: Vec<FileProgress>) {
        *self.status_files.lock() = files;
    }

    /// 读取已下发的子文件优先级调用记录（优先级重放测试断言用）。
    #[allow(clippy::type_complexity)]
    pub fn prio_calls(&self) -> Vec<(String, Vec<(usize, u32)>)> {
        self.prio_calls.lock().clone()
    }

    /// 编程 file_priorities() 行为（就绪/未就绪模拟用）。
    pub fn set_prio_readback(
        &self,
        v: Option<Result<Vec<Option<u32>>, smart_dl_core::types::EngineError>>,
    ) {
        *self.prio_readback.lock() = v;
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl DownloadEngine for FakeEngine {
    fn id(&self) -> &str {
        "fake"
    }

    fn kind(&self) -> EngineKind {
        self.kind
    }

    fn capabilities(&self) -> Vec<smart_dl_core::types::Capability> {
        vec![]
    }

    async fn add(
        &self,
        task: &DownloadTask,
    ) -> Result<EngineTaskId, smart_dl_core::types::EngineError> {
        let ident = match &task.source {
            DownloadSource::Http { url, .. } => url.clone(),
            DownloadSource::Magnet(m) => m.clone(),
            DownloadSource::TorrentFile(_) => format!("torrent:{}", task.id),
            _ => task.id.clone(),
        };
        if self.fail_urls.lock().contains(&ident) {
            return Err(smart_dl_core::types::EngineError::Other("fake fail".into()));
        }
        self.added.lock().push(ident);
        Ok(format!(
            "fk{}",
            self.counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ))
    }

    async fn pause(&self, id: &EngineTaskId) -> Result<(), smart_dl_core::types::EngineError> {
        self.paused.lock().push(id.clone());
        Ok(())
    }
    async fn resume(&self, id: &EngineTaskId) -> Result<(), smart_dl_core::types::EngineError> {
        self.resumed.lock().push(id.clone());
        Ok(())
    }
    async fn status(
        &self,
        _id: &EngineTaskId,
    ) -> Result<EngineStatus, smart_dl_core::types::EngineError> {
        let (down_rate, up_rate) = *self.status_rates.lock();
        Ok(EngineStatus {
            down_rate,
            up_rate,
            files: self.status_files.lock().clone(),
            name: self.status_name.lock().clone(),
            ..EngineStatus::default()
        })
    }
    async fn remove(
        &self,
        id: &EngineTaskId,
        delete_data: bool,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        self.removed.lock().push((id.to_string(), delete_data));
        Ok(())
    }

    async fn set_task_proxy(
        &self,
        id: &EngineTaskId,
        proxy: Option<String>,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        self.proxy_sets.lock().push((id.to_string(), proxy));
        Ok(())
    }
    async fn peers(
        &self,
        _id: &EngineTaskId,
    ) -> Result<Vec<smart_dl_core::types::PeerInfo>, smart_dl_core::types::EngineError> {
        Ok(vec![])
    }
    async fn update_sources(
        &self,
        _id: &EngineTaskId,
        _urls: Vec<String>,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        Ok(())
    }
    async fn add_url_seed(
        &self,
        id: &EngineTaskId,
        url: &str,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        // 记录（engine_tid, url）供测试断言逐条转发
        self.url_seeds.lock().push((id.clone(), url.to_string()));
        Ok(())
    }

    async fn set_limits(
        &self,
        id: &EngineTaskId,
        down_kb_s: Option<u32>,
        up_kb_s: Option<u32>,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        // 记录（engine_tid, down, up）供限速重放测试断言
        self.limits.lock().push((id.clone(), down_kb_s, up_kb_s));
        Ok(())
    }

    async fn set_file_priorities(
        &self,
        id: &EngineTaskId,
        priorities: &[(usize, u32)],
    ) -> Result<(), smart_dl_core::types::EngineError> {
        // readback 错误态同步反映到 set（真实 BT 引擎 metadata 未就绪时
        // set/read 同样失败）——pending 重放测试依赖该一致性
        if let Some(Err(e)) = &*self.prio_readback.lock() {
            return Err(e.clone());
        }
        self.prio_calls
            .lock()
            .push((id.clone(), priorities.to_vec()));
        Ok(())
    }

    async fn file_priorities(
        &self,
        _id: &EngineTaskId,
    ) -> Result<Vec<Option<u32>>, smart_dl_core::types::EngineError> {
        match &*self.prio_readback.lock() {
            None => Ok(vec![Some(4)]), // 默认就绪：单文件表
            Some(Ok(v)) => Ok(v.clone()),
            Some(Err(e)) => Err(e.clone()),
        }
    }
    async fn add_peer(
        &self,
        _id: &EngineTaskId,
        _peer: std::net::SocketAddr,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        Ok(())
    }
    async fn ban_peer(
        &self,
        _id: &EngineTaskId,
        _peer: std::net::SocketAddr,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        Ok(())
    }
    async fn read_piece(
        &self,
        _id: &EngineTaskId,
        _idx: u32,
    ) -> Result<Vec<u8>, smart_dl_core::types::EngineError> {
        Ok(vec![])
    }

    async fn add_xunlei_resume(
        &self,
        data: Vec<u8>,
    ) -> Result<EngineTaskId, smart_dl_core::types::EngineError> {
        self.xunlei.lock().push(data);
        Ok(format!(
            "xr{}",
            self.counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ))
    }
}

/// 任务持久化往返测试（FakeEngine，不联网）。
#[cfg(test)]
mod persist_tests {
    use super::*;

    fn wait_file(path: &std::path::Path, timeout_ms: u64) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        while std::time::Instant::now() < deadline {
            if path.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("等待持久化文件超时: {path:?}");
    }

    #[tokio::test]
    async fn persist_then_restore_keeps_task() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
        let tid = state
            .add_http_task("https://example.com/file.bin".into(), None)
            .await
            .unwrap();
        wait_file(&store, 2000);

        // 新 state（新引擎）恢复
        let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
        let state2 = DaemonState::new(fake2.clone(), vec![]);
        let n = state2.restore_from(&store).await.unwrap();
        assert_eq!(n, 1, "应恢复 1 条任务");
        let rec = state2.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(rec.task.id, tid, "task_id 必须保留");
        assert_eq!(rec.engine_kind, EngineKind::Http);
        assert_eq!(rec.task.state, TaskState::Queued, "恢复后重新入队");
        // 引擎重新 add 被调用
        assert_eq!(
            fake2.added(),
            vec!["https://example.com/file.bin".to_string()]
        );
    }

    #[tokio::test]
    async fn next_id_advances_after_restore() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
        let _ = state
            .add_http_task("https://example.com/a.bin".into(), None)
            .await
            .unwrap();
        let _ = state
            .add_http_task("https://example.com/b.bin".into(), None)
            .await
            .unwrap();
        wait_file(&store, 2000);

        let state2 = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![]);
        state2.restore_from(&store).await.unwrap();
        let new_tid = state2
            .add_http_task("https://example.com/c.bin".into(), None)
            .await
            .unwrap();
        let num: u64 = new_tid.strip_prefix('t').unwrap().parse().unwrap();
        assert!(num >= 3, "恢复后新任务 id 应跳过已用 id: {new_tid}");
    }

    #[tokio::test]
    async fn restore_replays_task_limits_to_engine() {
        // 限速重放：持久化的 task.limits 在 restore 后原样下发引擎
        // （FakeEngine set_limits 记录（tid, down, up）三元组）。
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
        let tid = state
            .add_http_task("https://example.com/limited.bin".into(), None)
            .await
            .unwrap();
        let merged = state.set_task_limits(&tid, Some(128), None).await.unwrap();
        assert_eq!(merged.down_kb_s, Some(128));
        assert_eq!(merged.up_kb_s, None, "HTTP 任务 up 方向应保持未设");
        wait_file(&store, 2000);

        // 新 state（新引擎）恢复 → 引擎收到原样限速下发
        let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
        let state2 = DaemonState::new(fake2.clone(), vec![]);
        let n = state2.restore_from(&store).await.unwrap();
        assert_eq!(n, 1);
        let calls = fake2.limits();
        assert_eq!(
            calls.len(),
            1,
            "恢复后必须向引擎重放一次 set_limits: {calls:?}"
        );
        let (etid, down, up) = &calls[0];
        assert_eq!(down, &Some(128), "down 方向原样重放");
        assert_eq!(up, &None, "up 方向 None 原样传递（不触发方向预拒）");
        // 内存中的 limits 配置也随恢复保留
        let rec = state2.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(
            rec.task.limits,
            Some(smart_dl_core::task::TaskLimits {
                down_kb_s: Some(128),
                up_kb_s: None,
            })
        );
        assert!(
            etid.starts_with("fk"),
            "engine_tid 应为 FakeEngine 返回的句柄: {etid}"
        );
    }

    #[cfg(feature = "bt")]
    fn bt_prio_task(id: &str, prios: Option<Vec<u32>>) -> DownloadTask {
        let magnet = "magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        DownloadTask {
            id: id.to_string(),
            canonical_id: CanonicalId {
                kind: CanonicalKind::Bt,
                identity: magnet.to_string(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Magnet(magnet.to_string()),
            identity: ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                backup_md5: None,
            },
            dest_root: PathBuf::from("."),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: prios,
            sequential: false,
            metadata: TaskMetadata {
                name: None,
                added_at_unix: 0,
            },
            limits: None,
        }
    }

    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn restore_replays_file_priorities_when_metadata_ready() {
        // metadata 已就绪（readback 非空表）：restore 必须直接全量重放一次
        // （.torrent 恢复场景；magnet 已解析场景同路）
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let pts = vec![PersistedTask {
            task: bt_prio_task("t1", Some(vec![0, 7])),
            engine_kind: EngineKind::Bt,
            paused: false,
        }];
        std::fs::write(&store, serde_json::to_vec(&pts).unwrap()).unwrap();

        let fake2 = Arc::new(FakeEngine::new(EngineKind::Bt));
        let state2 = DaemonState::new(fake2.clone(), vec![]);
        let n = state2.restore_from(&store).await.unwrap();
        assert_eq!(n, 1);
        let calls = fake2.prio_calls();
        assert_eq!(calls.len(), 1, "metadata 就绪时必须立即重放: {calls:?}");
        assert_eq!(
            calls[0].1,
            vec![(0, 0), (1, 7)],
            "下标-值对必须与持久化全量表一致"
        );
    }

    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn file_priorities_pending_replay_converges_when_metadata_arrives() {
        // magnet 恢复且 metadata 未就绪（readback NotFound，与真实引擎分类一致）
        // → restore 挂 pending → 就绪后由 replay 收敛 → 再跑幂等
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        fake.set_prio_readback(Some(Err(smart_dl_core::types::EngineError::NotFound)));
        let state = Arc::new(DaemonState::new(fake.clone(), vec![]));

        let pts = vec![PersistedTask {
            task: bt_prio_task("t1", Some(vec![0, 7])),
            engine_kind: EngineKind::Bt,
            paused: false,
        }];
        std::fs::write(&store, serde_json::to_vec(&pts).unwrap()).unwrap();
        let n = state.restore_from(&store).await.unwrap();
        assert_eq!(n, 1);
        assert!(
            !fake.prio_calls().iter().any(|(_, p)| !p.is_empty()),
            "未就绪时不得成功重放: {:?}",
            fake.prio_calls()
        );
        assert!(
            state.pending_file_prio.lock().contains("t1"),
            "未就绪必须挂 pending 集合"
        );

        // metadata 到达（readback 返回 2 文件表）→ 单轮收敛
        fake.set_prio_readback(Some(Ok(vec![Some(0), Some(7)])));
        let done = state.replay_pending_file_priorities().await;
        assert_eq!(done, vec!["t1".to_string()]);
        let calls = fake.prio_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, vec![(0, 0), (1, 7)]);
        // pending 清空 + 幂等（再跑无调用）
        assert!(!state.pending_file_prio.lock().contains("t1"));
        assert!(state.replay_pending_file_priorities().await.is_empty());
        assert_eq!(fake.prio_calls().len(), 1);
    }

    #[tokio::test]
    async fn restore_replays_pause_intent_and_marks_paused() {
        // P4 G5：用户暂停的任务持久化 paused=true → 重启恢复后重放 engine.pause
        // 且记录态回写 Paused（不再被当作运行任务重新入队）。
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
        let tid = state
            .add_http_task("https://example.com/paused.bin".into(), None)
            .await
            .unwrap();
        state.pause(&tid).await.unwrap();
        // pause 处理器应立即 autosave（否则暂停意图丢失）；轮询到 paused=true 落盘
        //（文件可能在 add 时已存在，不能只等文件存在）
        {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                let persisted = std::fs::read_to_string(&store)
                    .ok()
                    .and_then(|s| serde_json::from_str::<Vec<PersistedTask>>(&s).ok())
                    .map(|pts| pts.first().map(|p| p.paused).unwrap_or(false))
                    .unwrap_or(false);
                if persisted {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "暂停意图必须落盘 paused=true"
                );
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }

        // "重启"：新引擎 + restore → 引擎收到 pause 重放，记录态 Paused
        let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
        let state2 = DaemonState::new(fake2.clone(), vec![]);
        state2.restore_from(&store).await.unwrap();
        assert_eq!(
            fake2.paused_calls(),
            vec!["fk1".to_string()],
            "暂停意图必须重放到引擎"
        );
        assert!(fake2.resumed_calls().is_empty(), "暂停任务不得 resume");
        let rec = state2.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(rec.task.state, TaskState::Paused, "记录态应回写 Paused");
    }

    #[tokio::test]
    async fn running_task_restored_without_pause_replay_on_non_bt() {
        // P4 G5 对称面：非暂停任务恢复不重放 pause；HTTP 引擎不得 resume
        //（add 已自启下载循环，重复 resume 会产生多余 epoch 循环）。
        // BT 的 resume 重放由 fastresume e2e（真实内核）覆盖。
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
        state
            .add_http_task("https://example.com/running.bin".into(), None)
            .await
            .unwrap();
        wait_file(&store, 2000);

        let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
        let state2 = DaemonState::new(fake2.clone(), vec![]);
        state2.restore_from(&store).await.unwrap();
        assert!(fake2.paused_calls().is_empty());
        assert!(fake2.resumed_calls().is_empty(), "HTTP 恢复不得重复 resume");
        let rec = state2.tasks.lock().values().next().cloned().unwrap();
        assert_eq!(rec.task.state, TaskState::Queued);
    }

    #[tokio::test]
    async fn restore_add_failure_marks_failed() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
        let tid = state
            .add_http_task("https://example.com/gone.bin".into(), None)
            .await
            .unwrap();
        wait_file(&store, 2000);

        let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
        fake2.fail_url("https://example.com/gone.bin");
        let state2 = DaemonState::new(fake2.clone(), vec![]);
        let n = state2.restore_from(&store).await.unwrap();
        assert_eq!(n, 0, "add 失败不计入恢复数");
        let rec = state2.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(rec.task.state, TaskState::Failed, "add 失败任务标 Failed");
        assert!(rec.engine_tid.is_none());
    }

    #[tokio::test]
    async fn no_storage_no_autosave() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let _ = state
            .add_http_task("https://example.com/x.bin".into(), None)
            .await
            .unwrap();
        // 无 persist_path → 无写盘（autosave 直接 return）
        // 此测试验证不 panic；写盘路径由 with_storage 测试覆盖。
        assert!(fake.added().len() == 1);
    }

    #[tokio::test]
    async fn dest_none_uses_default_dest_root() {
        // with_dest_root 注入默认目录后，dest 未指定 → 任务落默认目录（而非 daemon cwd）
        // Task 5-a：用临时目录替代硬编码 /data/default-dl（沙盒无 /data 写权限 → Permission denied）
        let tmp = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(tmp.path().to_path_buf());
        let tid = state
            .add_http_task("https://example.com/dest.bin".into(), None)
            .await
            .unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(
            rec.task.dest_root,
            tmp.path().to_path_buf(),
            "dest 未指定应落到默认 dest_root"
        );
    }

    #[tokio::test]
    async fn explicit_dest_overrides_default() {
        // Task 5-a：默认 dest_root 同样改为临时目录（保持沙盒可跑）。
        // 安全修复（V2）：显式 dest 必须落在白名单内——用 dest_root 子目录验证
        // 「显式 dest 优先于默认」契约保持；白名单外由 reject_dest_outside_roots 覆盖。
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("custom");
        std::fs::create_dir_all(&sub).unwrap();
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(tmp.path().to_path_buf());
        let tid = state
            .add_http_task(
                "https://example.com/override.bin".into(),
                Some(sub.to_string_lossy().into_owned()),
            )
            .await
            .unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(rec.task.dest_root, sub, "显式 dest（白名单内）优先于默认");
    }

    #[tokio::test]
    async fn reject_dest_outside_roots() {
        // 安全回归（V2）：显式 dest 在白名单外 → 拒绝任务（不再任意目录可写）。
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap(); // 白名单外独立目录
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(tmp.path().to_path_buf());
        let r = state
            .add_http_task(
                "https://example.com/escape.bin".into(),
                Some(outside.path().to_string_lossy().into_owned()),
            )
            .await;
        assert!(
            matches!(&r, Err(DaemonError::InvalidSource(m)) if m.contains("越界")),
            "白名单外 dest 必须拒绝: {r:?}"
        );
    }

    /// Bug B 回归（重入自死锁，HTTP 推进路径）：storage 启用 + 引擎状态推进
    /// （Queued → Downloading）触发锁内 autosave。修复前：poll_engine_states
    /// 持 tasks 锁调 autosave → persisted_tasks 同线程重入 → 5s 超时失败。
    #[tokio::test(flavor = "multi_thread")]
    async fn http_poll_transition_with_storage_autosave_no_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]).with_storage(store.clone());
        let _tid = state
            .add_http_task("https://example.com/wedge.bin".into(), None)
            .await
            .unwrap();
        // FakeEngine::status 默认 MetadataPending → 推进 Queued→Downloading(Http)
        //（迁移必发生 → 必走 autosave 路径）
        let work = state.poll_engine_states();
        let effects = tokio::time::timeout(std::time::Duration::from_secs(5), work)
            .await
            .expect("poll_engine_states 死锁（Bug B 重入回归）");
        assert_eq!(effects.len(), 1, "应推进一条任务状态");
        assert!(store.exists(), "状态推进应触发持久化落盘");
    }
}

/// 卡 D：FTP 路由（feature `ftp`）单测——add_ftp_task 前缀校验 / user+pass 提取 /
/// 路由 Ftp 引擎 / 目录 files 同步；以及端到端（真实 FtpEngine + 最小 mock FTP server）。
#[cfg(all(test, feature = "ftp"))]
mod ftp_tests {
    use super::*;

    // ---- 单元：add_ftp_task 基础路由 ----

    #[tokio::test]
    async fn non_ftp_url_is_invalid() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
        let err = state
            .add_ftp_task("https://example.com/f.bin".into(), None)
            .await
            .expect_err("非 ftp:// 前缀应拒绝");
        assert!(
            matches!(err, DaemonError::InvalidSource(_)),
            "应返回 InvalidSource: {err}"
        );
        assert!(fake.added().is_empty(), "不应路由到引擎");
    }

    #[tokio::test]
    async fn extracts_auth_and_routes_to_ftp_engine() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
        let url = "ftp://alice:secret@host/pub/a.bin".to_string();
        let tid = state.add_ftp_task(url.clone(), None).await.unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(rec.engine_kind, EngineKind::Ftp, "engine_kind 记 Ftp");
        match &rec.task.source {
            DownloadSource::Ftp { url: u, user, pass } => {
                assert_eq!(u, &url);
                assert_eq!(user, "alice", "parse_ftp_auth 应提取 user");
                assert_eq!(pass, "secret", "parse_ftp_auth 应提取 pass");
            }
            other => panic!("source 应为 DownloadSource::Ftp: {other:?}"),
        }
        // FakeEngine.add 对 Ftp 源记录 task.id → 证明路由到 Ftp 引擎
        assert_eq!(fake.added(), vec![tid], "应路由到 Ftp 引擎 add");
    }

    #[tokio::test]
    async fn anonymous_auth_falls_back() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
        let tid = state
            .add_ftp_task("ftp://host/pub/a.bin".into(), None)
            .await
            .unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        match &rec.task.source {
            DownloadSource::Ftp { user, pass, .. } => {
                assert_eq!(user, "anonymous");
                assert_eq!(pass, "");
            }
            _ => panic!("source 应为 Ftp"),
        }
    }

    #[tokio::test]
    async fn single_file_uses_url_filename_as_metadata_name() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
        fake.set_status_files(vec![FileProgress {
            rel_path: "x.bin".into(),
            done: 0,
            size: 5,
        }]);
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
        let tid = state
            .add_ftp_task("ftp://host/pub/a.bin".into(), None)
            .await
            .unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(
            rec.task.metadata.name.as_deref(),
            Some("a.bin"),
            "单文件任务落盘名取 URL 最后一段"
        );
        assert!(
            rec.task.files.is_empty(),
            "单文件任务不应触发目录 files 同步"
        );
    }

    #[tokio::test]
    async fn dir_task_syncs_files_from_engine_status() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
        fake.set_status_files(vec![
            FileProgress {
                rel_path: "a.bin".into(),
                done: 0,
                size: 10,
            },
            FileProgress {
                rel_path: "b.bin".into(),
                done: 0,
                size: 20,
            },
        ]);
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
        let tid = state
            .add_ftp_task("ftp://host/pub/".into(), None)
            .await
            .unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(
            rec.task.files.len(),
            2,
            "目录任务 files 应从引擎 status 同步: {:?}",
            rec.task.files
        );
        assert_eq!(rec.task.files[0].rel_path, "a.bin");
        assert_eq!(rec.task.files[0].size, 10);
        assert_eq!(rec.task.files[0].done, 0);
        assert_eq!(rec.task.files[0].engine, EngineKind::Ftp);
        assert_eq!(
            rec.task.files[0].source_urls,
            vec!["ftp://host/pub/".to_string()]
        );
        // 序列化往返：task.files（含 EngineKind::Ftp）应可持久化反序列化
        let json = serde_json::to_vec(&rec.task).expect("task 可序列化");
        let _restored: DownloadTask = serde_json::from_slice(&json).expect("可反序列化");
    }

    #[tokio::test]
    async fn repeated_ftp_dup_is_rejected() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
        let _ = state
            .add_ftp_task("ftp://host/pub/".into(), None)
            .await
            .unwrap();
        let err = state
            .add_ftp_task("ftp://host/pub/".into(), None)
            .await
            .expect_err("重复 canonical 应拒绝");
        assert!(
            matches!(err, DaemonError::Duplicate(_)),
            "应返回 Duplicate: {err}"
        );
    }

    // ---- 端到端：真实 FtpEngine + 最小 mock FTP server（卡 D 验收点） ----

    /// 最小 mock FTP server：支持目录 LIST（文件清单）/ 文件 RETR（完整内容）、PASSIVE 被动模式。
    /// `files` = `(远端绝对路径, 内容)`；路径不带 user:pass@。
    async fn start_mock_ftp(files: Vec<(String, Vec<u8>)>) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let files = SArc::new(files);
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let files = files.clone();
                tokio::spawn(async move {
                    handle_mock_ftp(stream, files).await;
                });
            }
        });
        addr
    }

    use std::net::SocketAddr;
    use std::sync::Arc as SArc;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpStream;

    async fn handle_mock_ftp(mut stream: TcpStream, files: SArc<Vec<(String, Vec<u8>)>>) {
        let mut rest: u64 = 0;
        let mut data_listener: Option<tokio::net::TcpListener> = None;
        let _ = stream.write_all(b"220 test ftp ready\r\n").await;
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let cmd = line.trim_end_matches("\r\n").trim_end();
            let (verb, arg) = match cmd.split_once(' ') {
                Some((v, a)) => (v, a.trim()),
                None => (cmd, ""),
            };
            let conn = reader.get_mut();
            match verb {
                "USER" | "PASS" => {
                    let _ = conn.write_all(b"230 logged in\r\n").await;
                }
                "TYPE" => {
                    let _ = conn.write_all(b"200 type set\r\n").await;
                }
                "PASV" => {
                    let dl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let da = dl.local_addr().unwrap();
                    let (p1, p2) = (da.port() / 256, da.port() % 256);
                    let _ = conn
                        .write_all(
                            format!("227 Entering Passive Mode (127,0,0,1,{p1},{p2})\r\n")
                                .as_bytes(),
                        )
                        .await;
                    data_listener = Some(dl);
                }
                "RETR" => {
                    let data: Option<Vec<u8>> =
                        files.iter().find(|(p, _)| p == arg).map(|(_, d)| d.clone());
                    let Some(body) = data else {
                        let _ = conn.write_all(b"550 file unavailable\r\n").await;
                        continue;
                    };
                    if rest > body.len() as u64 {
                        let _ = conn.write_all(b"550 file unavailable\r\n").await;
                        continue;
                    }
                    let _ = conn.write_all(b"150 opening data connection\r\n").await;
                    if let Some(dl) = data_listener.as_ref() {
                        if let Ok((mut data_conn, _)) = dl.accept().await {
                            let _ = data_conn.write_all(&body[rest as usize..]).await;
                            let _ = data_conn.shutdown().await;
                        }
                    }
                    let _ = conn.write_all(b"226 transfer complete\r\n").await;
                    rest = 0;
                    data_listener = None;
                }
                "LIST" => {
                    let dir = arg.trim_end_matches('/');
                    let prefix = format!("{dir}/");
                    let mut lines: Vec<String> = Vec::new();
                    for (p, d) in files.iter() {
                        if let Some(name) = p.strip_prefix(&prefix) {
                            if !name.is_empty() && !name.contains('/') {
                                lines.push(format!(
                                    "-rw-r--r--  1 owner  group  {:>8} Jan 01 12:00 {name}",
                                    d.len()
                                ));
                            }
                        }
                    }
                    let mut text = format!("total {}\r\n", lines.len());
                    text.push_str(&lines.join("\r\n"));
                    text.push_str("\r\n");
                    let _ = conn.write_all(b"150 opening data connection\r\n").await;
                    if let Some(dl) = data_listener.as_ref() {
                        if let Ok((mut data, _)) = dl.accept().await {
                            let _ = data.write_all(text.as_bytes()).await;
                            let _ = data.shutdown().await;
                        }
                    }
                    let _ = conn.write_all(b"226 transfer complete\r\n").await;
                    data_listener = None;
                }
                _ => {
                    let _ = conn.write_all(b"502 unknown\r\n").await;
                }
            }
        }
    }

    /// 端到端验收（卡 D）：FTP 目录任务（2 文件）→ add 后 task.files 同步 →
    /// poll_engine_states 推进 Completed → 快照含文件信息且逐文件 done==size；
    /// 同时一个 HTTP 任务（独立 Http 槽）不受影响。
    #[tokio::test]
    async fn ftp_dir_completes_and_http_unaffected() {
        let ftp_addr = start_mock_ftp(vec![
            ("/pub/a.bin".to_string(), vec![0x5Au8; 16]),
            ("/pub/b.bin".to_string(), vec![0x5Bu8; 24]),
        ])
        .await;
        let ftp_url = format!("ftp://127.0.0.1:{}/pub/", ftp_addr.port());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().to_str().unwrap().to_string();

        // 同时挂 HTTP 引擎（Fake）与真实 FTP 引擎：验证各占独立槽位
        let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let ftp_engine: Arc<dyn DownloadEngine> = Arc::new(smart_dl_httpdl::FtpEngine::new());
        let state = Arc::new(
            DaemonState::new(http_fake.clone(), vec![])
                .with_ftp(ftp_engine)
                .with_dest_root(dir.path().to_path_buf()),
        );

        // 1) FTP 目录任务
        let ftp_tid = state
            .add_link_task(ftp_url.clone(), Some(dest.clone()))
            .await
            .expect("FTP 目录任务 add 应成功");
        let rec = state.tasks.lock().get(&ftp_tid).cloned().unwrap();
        assert_eq!(rec.engine_kind, EngineKind::Ftp);
        assert_eq!(
            rec.task.files.len(),
            2,
            "add 后 files 应同步 2 个: {:?}",
            rec.task.files
        );

        // 2) HTTP 任务走独立 Http 槽（Fake 引擎记录），不受 FTP 影响
        let http_tid = state
            .add_http_task("https://example.com/keep.bin".into(), Some(dest.clone()))
            .await
            .unwrap();
        assert!(
            http_fake
                .added()
                .contains(&"https://example.com/keep.bin".to_string()),
            "HTTP 任务应路由到 Http 引擎: {:?}",
            http_fake.added()
        );

        // 3) 轮询推进 FTP 到 Completed（FTP 引擎无 alert，靠 poll 推进——同 HTTP 链路）
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let _ = state.poll_engine_states().await;
            if let Some(snap) = state.task_snapshot(&ftp_tid).await {
                if snap.state == "Completed" {
                    // 快照含文件信息，且逐文件 done==size
                    assert_eq!(snap.files.len(), 2, "快照应含 2 个文件进度");
                    for f in &snap.files {
                        assert_eq!(f.done, f.size, "逐文件 done 应等于 size: {f:?}");
                    }
                    // HTTP 任务记录仍存在（未被破坏）
                    assert!(state.task_snapshot(&http_tid).await.is_some());
                    return;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "60s 内 FTP 任务未 Completed"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}

/// 卡 I′：F5 P2SP web seed 注入（feature `webseed`，隐含 `bt`）——
/// 端点语义单测 + 真实 BtCore 本地 HTTP 源端到端（Rust 版 F5 PoC-1）。
#[cfg(all(test, feature = "webseed"))]
mod webseed_tests {
    use super::*;

    /// 最小单文件 .torrent（无 tracker；info: length=123/name=test/piece length/pieces）。
    fn sample_torrent() -> Vec<u8> {
        let mut b = b"d4:infod6:lengthi123e4:name4:test12:piece lengthi16384e6:pieces20:".to_vec();
        b.extend_from_slice(&[0u8; 20]);
        b.extend_from_slice(b"eee");
        b
    }

    #[tokio::test]
    async fn non_bt_task_rejects_webseed() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
        let tid = state
            .add_http_task(
                "http://example.com/a.bin".into(),
                Some(dir.path().to_string_lossy().into_owned()),
            )
            .await
            .unwrap();
        let err = state
            .add_webseeds(&tid, &["http://seed/1".into()])
            .await
            .expect_err("非 BT 任务必须拒绝");
        assert!(
            matches!(err, DaemonError::UnsupportedOp(_)),
            "实际: {err:?}"
        );
        assert!(fake.url_seeds().is_empty());
    }

    #[tokio::test]
    async fn bt_task_forwards_urls_to_engine() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
        let tid = state
            .add_torrent_task(
                sample_torrent(),
                Some(dir.path().to_string_lossy().into_owned()),
            )
            .await
            .expect("add_torrent_task");
        let n = state
            .add_webseeds(&tid, &["http://seed/1".into(), "http://seed/2".into()])
            .await
            .expect("注入应成功");
        assert_eq!(n, 2);
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        let engine_tid = rec.engine_tid.expect("engine_tid");
        assert_eq!(
            fake.url_seeds(),
            vec![
                (engine_tid.clone(), "http://seed/1".into()),
                (engine_tid, "http://seed/2".into()),
            ]
        );
    }

    #[tokio::test]
    async fn webseed_missing_task_is_not_found() {
        let state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Bt)), vec![]);
        let err = state
            .add_webseeds("t_missing", &["http://seed/1".into()])
            .await
            .expect_err("");
        assert!(matches!(err, DaemonError::NotFound(_)));
    }

    // ---- 端到端：真实 BtCore + 本地 HTTP 静态源（F5 PoC-1 的 Rust 复刻）----

    /// 极简 HTTP/1.1 静态源：200 全量 / 206 Range 分片，Connection: close。
    async fn serve_http(mut sock: tokio::net::TcpStream, body: Vec<u8>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]).to_string();
        let range = req
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("range:"))
            .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()));
        if let Some(r) = range {
            let spec = r.trim_start_matches("bytes=");
            let (a, b) = spec.split_once('-').unwrap_or((spec, ""));
            let start: u64 = a.parse().unwrap_or(0);
            let end: u64 = if b.is_empty() {
                body.len() as u64 - 1
            } else {
                b.parse::<u64>().unwrap_or(body.len() as u64 - 1)
            };
            let s = (start as usize).min(body.len().saturating_sub(1));
            let e = (end as usize).min(body.len().saturating_sub(1));
            let head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {s}-{e}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len(),
                e.saturating_sub(s) + 1
            );
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(&body[s..=e]).await;
        } else {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(&body).await;
        }
        let _ = sock.shutdown().await;
    }

    #[tokio::test]
    async fn webseed_e2e_downloads_via_local_http_source() {
        use sha1::{Digest, Sha1};
        // 单 piece 文件：64KB 内容，pieces = SHA1(全量)
        let content: Vec<u8> = (0..65536u32).map(|i| (i % 251) as u8).collect();
        let mut h = Sha1::new();
        h.update(&content);
        let piece = h.finalize();
        let mut meta =
            b"d4:infod6:lengthi65536e4:name8:file.bin12:piece lengthi65536e6:pieces20:".to_vec();
        meta.extend_from_slice(&piece);
        meta.extend_from_slice(b"eee");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                let c = content.clone();
                tokio::spawn(async move { serve_http(sock, c).await });
            }
        });

        let save = tempfile::tempdir().unwrap();
        let core = smart_dl_btcore::BtCore::new(save.path(), "webseed-e2e").expect("session init");
        let ih = core.add_torrent_file(&meta, &[]).expect("add torrent");
        core.add_url_seed(&ih, &format!("http://{addr}/file.bin"))
            .expect("add url seed");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let st = core.status(&ih).expect("status");
            if st.downloaded >= 65536 && st.progress >= 0.999 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "90s 未完成: downloaded={} progress={}",
                st.downloaded,
                st.progress
            );
        }
    }
}

/// 常量时间字节串比较（第六轮审计 9.3.4）：token 精确比较走固定时长路径，
/// 消除逐字节短路比较的时序侧信道。长度不等提前返回会泄露长度信息——
/// 对高熵随机 token 而言长度本身非敏感，业界标准做法可接受。
fn ct_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod ct_eq_tests {
    use super::{ct_eq, DaemonState};
    use std::sync::Arc;

    #[test]
    fn ct_eq_matches_equality_semantics() {
        assert!(ct_eq("abc", "abc"));
        assert!(ct_eq("", ""));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abC"));
        assert!(!ct_eq("abc", "abcd"));
        assert!(!ct_eq("abcd", "abc"));
        assert!(!ct_eq("", "a"));
        // 高熵长 token 等价性
        let t = "a1B2c3D4e5F6g7H8";
        assert!(ct_eq(t, t));
        assert!(!ct_eq(t, "a1B2c3D4e5F6g7H9"));
    }

    #[test]
    fn verify_http_token_end_to_end() {
        let engine = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
        let st =
            DaemonState::new(Arc::new(engine), vec![]).with_http_token(Some("s3cret".to_string()));
        assert!(st.verify_http_token(Some("Bearer s3cret")));
        // 前缀大小写敏感（Bearer 规范）
        assert!(!st.verify_http_token(Some("bearer s3cret")));
        assert!(!st.verify_http_token(Some("Bearer s3cretX")));
        assert!(!st.verify_http_token(Some("Bearer ")));
        assert!(!st.verify_http_token(Some("Basic s3cret")));
        assert!(!st.verify_http_token(None));
    }
}

/// E5 任务级代理：add 入口校验 + source.proxy 持久化回显（FakeEngine 不联网）。
#[cfg(test)]
mod task_proxy_tests {
    use super::*;

    #[tokio::test]
    async fn add_rejects_invalid_proxy_url() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        for bad in ["", "http://127.0.0.1:70000"] {
            let r = state
                .add_http_task_opts(
                    "https://example.com/f.bin".into(),
                    None,
                    AddHttpOpts {
                        proxy: Some(bad.to_string()),
                        ..Default::default()
                    },
                )
                .await;
            match r {
                Err(DaemonError::InvalidSource(m)) => {
                    assert!(m.contains("proxy"), "错误信息应定性 proxy: {m}");
                }
                other => panic!("非法 proxy {bad:?} 必须在入队前拒绝: {other:?}"),
            }
        }
        assert!(fake.added().is_empty(), "被拒任务不得进入引擎");
    }

    #[tokio::test]
    async fn add_persists_wellformed_proxy_in_source() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task_opts(
                "https://example.com/f.bin".into(),
                None,
                AddHttpOpts {
                    proxy: Some("http://127.0.0.1:8080".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        match &rec.task.source {
            DownloadSource::Http { proxy, .. } => {
                assert_eq!(
                    proxy.as_deref(),
                    Some("http://127.0.0.1:8080"),
                    "proxy 应在 source 持久化回显"
                );
            }
            other => panic!("source 应为 Http: {other:?}"),
        }
        // 默认路径：proxy=None 正常建任务
        let tid2 = state
            .add_http_task("https://example.com/g.bin".into(), None)
            .await
            .unwrap();
        let rec2 = state.tasks.lock().get(&tid2).cloned().unwrap();
        match &rec2.task.source {
            DownloadSource::Http { proxy, .. } => assert!(proxy.is_none()),
            other => panic!("source 应为 Http: {other:?}"),
        }
    }
}

/// E6 AddHttpOpts：API 新暴露字段（headers/auth/sha256/backup/name）落任务
/// 记录 + 入参校验（FakeEngine 不联网）。
#[cfg(test)]
mod add_opts_tests {
    use super::*;

    #[tokio::test]
    async fn add_opts_fields_land_in_task_record() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task_opts(
                "https://example.com/f.bin".into(),
                None,
                AddHttpOpts {
                    sequential: true,
                    proxy: Some("socks5://127.0.0.1:1080".into()),
                    headers: vec![
                        ("Referer".into(), "https://ref.example".into()),
                        ("X-Token".into(), "abc".into()),
                    ],
                    basic_auth: Some(("user".into(), "pass".into())),
                    // 大写 + 首尾空白：断言小写归一
                    sha256: Some(
                        " ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789 ".into(),
                    ),
                    backup_url: Some("https://backup.example/f.bin".into()),
                    backup_md5: Some("ABCDEF0123456789ABCDEF0123456789".into()),
                    name: Some("explicit-name.bin".into()),
                },
            )
            .await
            .unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert!(rec.task.sequential, "sequential 应落任务");
        match &rec.task.source {
            DownloadSource::Http {
                headers,
                auth,
                backup_url,
                proxy,
                ..
            } => {
                assert_eq!(headers.len(), 2, "headers 应原序落 source");
                assert_eq!(headers[0].0, "Referer");
                assert_eq!(
                    auth.as_ref(),
                    Some(&Auth::Basic("user".into(), "pass".into()))
                );
                assert_eq!(backup_url.as_deref(), Some("https://backup.example/f.bin"));
                assert_eq!(proxy.as_deref(), Some("socks5://127.0.0.1:1080"));
            }
            other => panic!("source 应为 Http: {other:?}"),
        }
        match &rec.task.identity {
            ContentIdentity::SingleFile {
                sha256, backup_md5, ..
            } => {
                assert_eq!(
                    sha256.as_deref(),
                    Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"),
                    "sha256 大写+空白应小写归一"
                );
                assert_eq!(
                    backup_md5.as_deref(),
                    Some("abcdef0123456789abcdef0123456789"),
                    "backup_md5 大写应小写归一"
                );
            }
            other => panic!("identity 应为 SingleFile: {other:?}"),
        }
        assert_eq!(
            rec.task.metadata.name.as_deref(),
            Some("explicit-name.bin"),
            "显式名应落 metadata"
        );
    }

    #[tokio::test]
    async fn add_opts_validation_rejects_bad_input() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = Arc::new(DaemonState::new(fake.clone(), vec![]));
        let cases: Vec<(AddHttpOpts, &str)> = vec![
            (
                AddHttpOpts {
                    sha256: Some("ab".repeat(31)), // 63 hex
                    ..Default::default()
                },
                "sha256",
            ),
            (
                AddHttpOpts {
                    sha256: Some("g".repeat(64)), // 非 hex
                    ..Default::default()
                },
                "sha256",
            ),
            (
                AddHttpOpts {
                    backup_md5: Some("a".repeat(32)), // 无 backup_url
                    ..Default::default()
                },
                "成对",
            ),
            (
                AddHttpOpts {
                    backup_url: Some("https://b.example/f".into()),
                    backup_md5: Some("a".repeat(31)),
                    ..Default::default()
                },
                "backup_md5",
            ),
            (
                AddHttpOpts {
                    backup_url: Some("ftp://b.example/f".into()),
                    ..Default::default()
                },
                "backup_url",
            ),
            (
                AddHttpOpts {
                    headers: vec![("X-Bad:Header".into(), "v".into())],
                    ..Default::default()
                },
                "header",
            ),
            (
                AddHttpOpts {
                    headers: vec![("".into(), "v".into())],
                    ..Default::default()
                },
                "header",
            ),
            (
                AddHttpOpts {
                    headers: vec![("X-Ok".into(), "v\ninjected".into())],
                    ..Default::default()
                },
                "换行",
            ),
            (
                AddHttpOpts {
                    name: Some("../escape.bin".into()),
                    ..Default::default()
                },
                "name",
            ),
        ];
        for (opts, tag) in cases {
            let r = state
                .add_http_task_opts("https://example.com/f.bin".into(), None, opts)
                .await;
            match r {
                Err(DaemonError::InvalidSource(m)) => {
                    assert!(m.contains(tag), "错误信息应定性 {tag}: {m}");
                }
                other => panic!("非法入参（{tag}）必须 InvalidSource 拒绝: {other:?}"),
            }
        }
        assert!(fake.added().is_empty(), "被拒任务不得进入引擎");
    }
}

/// E7 任务管理面：列表过滤/分页/确定性排序 + 批量操作 + remove delete_data
/// 透传（FakeEngine 不联网；状态直接改记录白盒构造，避免真实下载竞态）。
#[cfg(test)]
mod list_batch_tests {
    use super::*;

    /// 造 n 个不同 URL 的 HTTP 任务（dest 走默认路径），返回 task_id 列表。
    async fn add_n(state: &DaemonState, n: usize) -> Vec<String> {
        let mut ids = Vec::new();
        for i in 0..n {
            let tid = state
                .add_http_task(format!("https://example.com/f{i}.bin"), None)
                .await
                .unwrap();
            ids.push(tid);
        }
        ids
    }

    #[tokio::test]
    async fn list_filtered_orders_by_creation_and_paginates() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let ids = add_n(&state, 5).await;

        // 无过滤：全量，创建序（HashMap 迭代序不稳定 → 排序必须确定性）
        let (all, total) = state.list_filtered(&ListQuery::default());
        assert_eq!(total, 5);
        assert_eq!(
            all.iter().map(|r| r.task_id.clone()).collect::<Vec<_>>(),
            ids,
            "默认列表必须按创建序（task_id 数值后缀）"
        );

        // 分页：limit=2 offset=1 → 第 2、3 个；total 是过滤后总数而非页长
        let (page, total) = state.list_filtered(&ListQuery {
            limit: Some(2),
            offset: 1,
            ..Default::default()
        });
        assert_eq!(total, 5);
        assert_eq!(
            page.iter().map(|r| r.task_id.clone()).collect::<Vec<_>>(),
            ids[1..3].to_vec()
        );

        // offset 越界 → 空页但 total 不丢
        let (page, total) = state.list_filtered(&ListQuery {
            offset: 99,
            ..Default::default()
        });
        assert!(page.is_empty());
        assert_eq!(total, 5);
    }

    #[tokio::test]
    async fn list_filtered_by_state_and_engine() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let ids = add_n(&state, 3).await;
        // 白盒改状态：t1 → Paused，t2 → Failed（t3 保持 Queued）
        {
            let mut tasks = state.tasks.lock();
            tasks.get_mut(&ids[0]).unwrap().task.state = TaskState::Paused;
            tasks.get_mut(&ids[1]).unwrap().task.state = TaskState::Failed;
        }

        // 单状态过滤 + 大小写不敏感
        let (rows, total) = state.list_filtered(&ListQuery {
            states: vec!["paused".into()],
            ..Default::default()
        });
        assert_eq!((rows.len(), total), (1, 1));
        assert_eq!(rows[0].task_id, ids[0]);

        // 多状态 OR
        let (rows, _) = state.list_filtered(&ListQuery {
            states: vec!["Paused".into(), "Failed".into()],
            ..Default::default()
        });
        assert_eq!(rows.len(), 2);

        // 引擎过滤（大小写不敏感）+ 维度间 AND
        let (rows, total) = state.list_filtered(&ListQuery {
            engines: vec!["HTTP".into()],
            ..Default::default()
        });
        assert_eq!((rows.len(), total), (3, 3));
        assert!(
            rows.iter().all(|r| r.engine == "http"),
            "engine 标签必须回显"
        );
        let (rows, _) = state.list_filtered(&ListQuery {
            states: vec!["Paused".into()],
            engines: vec!["bt".into()],
            ..Default::default()
        });
        assert!(rows.is_empty(), "状态命中但引擎不命中 → AND 过滤为空");

        // 列表条目 name 字段：未显式命名 → 省略（E4 派生链未回填）
        let (rows, _) = state.list_filtered(&ListQuery::default());
        assert!(rows.iter().all(|r| r.name.is_none()));
    }

    #[tokio::test]
    async fn batch_pause_resume_remove_semantics() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let ids = add_n(&state, 3).await;

        // 批量 pause：2 存在 + 1 不存在 + 1 重复 id（静默去重，不产生假失败）
        let mut req = ids[..2].to_vec();
        req.push("t999".into());
        req.push(ids[0].clone());
        let out = state.batch(&req, BatchAction::Pause).await;
        assert_eq!(out.results.len(), 3, "重复 id 去重后仅 3 项");
        assert_eq!((out.succeeded, out.failed), (2, 1));
        assert_eq!(
            out.results.iter().filter(|r| r.ok).count(),
            2,
            "逐项结果形状完整"
        );
        let missing = out.results.iter().find(|r| !r.ok).unwrap();
        assert_eq!(missing.id, "t999");
        assert!(
            missing.error.as_deref().unwrap_or("").contains("not found"),
            "单项失败必须带原因: {:?}",
            missing.error
        );
        assert_eq!(fake.paused_calls().len(), 2, "引擎 pause 恰好 2 次");
        for tid in &ids[..2] {
            let rec = state.tasks.lock().get(tid).cloned().unwrap();
            assert!(matches!(rec.task.state, TaskState::Paused));
        }

        // 批量 resume：1 存在 + 1 不存在
        let out = state
            .batch(&[ids[0].clone(), "tX".into()], BatchAction::Resume)
            .await;
        assert_eq!((out.succeeded, out.failed), (1, 1));
        assert_eq!(fake.resumed_calls().len(), 1);

        // 批量 remove：3 全成功（引擎侧 delete_data=false）
        let out = state
            .batch(
                &[ids[0].clone(), ids[1].clone(), ids[2].clone()],
                BatchAction::Remove { delete_data: false },
            )
            .await;
        assert_eq!((out.succeeded, out.failed), (3, 0));
        assert_eq!(fake.removed_calls().len(), 3);
        assert!(
            fake.removed_calls().iter().all(|(_, dd)| !dd),
            "delete_data=false 必须原样透传"
        );
        assert!(state.list().is_empty(), "批量 remove 后任务表必须清空");
    }

    #[tokio::test]
    async fn remove_with_forwards_delete_data_to_engine() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let ids = add_n(&state, 1).await;
        let engine_tid = state.tasks.lock().get(&ids[0]).unwrap().engine_tid.clone();

        // 不存在的 id → NotFound，且引擎零调用
        assert!(state.remove_with("t404", true).await.is_err());
        assert!(fake.removed_calls().is_empty());

        state.remove_with(&ids[0], true).await.unwrap();
        assert_eq!(
            fake.removed_calls(),
            vec![(engine_tid.unwrap(), true)],
            "delete_data=true 必须透传到引擎"
        );
        assert!(state.list().is_empty());
    }

    #[test]
    fn known_labels_cover_all_variants() {
        let states = known_state_labels();
        for s in [
            "Queued",
            "Evaluating",
            "Downloading",
            "Paused",
            "FallbackProvider",
            "Transferring",
            "Completed",
            "Stopped",
            "Seeding",
            "Failed",
        ] {
            assert!(
                states.iter().any(|k| k == s),
                "state 合法值全集缺 {s}: {states:?}"
            );
        }
        let engines = known_engine_labels();
        assert_eq!(engines, vec!["bt", "http", "ftp", "provider", "xunlei-nas"]);
    }
}

/// E8 任务级代理热改：记录回显 + 引擎调用 + 入参校验 + 非 HTTP 预拒
/// （FakeEngine 记录调用；BT 任务白盒插记录，precheck 在 engine_for 之前，
/// 非 bt 构建同样可测）。
#[cfg(test)]
mod task_proxy_set_tests {
    use super::*;
    use smart_dl_core::identity::{CanonicalId, CanonicalKind, ContentIdentity};

    /// 手工插一条 BT kind 记录（不注册 BT 引擎——set_task_proxy 的 kind
    /// 预拒发生在 engine_for 之前，未注册引擎不影响断言路径）。
    fn insert_bt_rec(state: &DaemonState, id: &str, ih: &str) {
        let rec = TaskRecord {
            task: DownloadTask {
                id: id.into(),
                canonical_id: CanonicalId {
                    kind: CanonicalKind::Bt,
                    identity: ih.to_string(),
                    validator: None,
                    token_sensitive: false,
                },
                source: DownloadSource::Magnet(format!("magnet:?xt=urn:btih:{ih}")),
                identity: ContentIdentity::SingleFile {
                    size: 0,
                    etag: None,
                    sha256: None,
                    backup_md5: None,
                },
                dest_root: PathBuf::from("."),
                files: vec![],
                acquisitions: vec![],
                aggregate: Default::default(),
                state: TaskState::Downloading(EngineKind::Bt),
                retry: Default::default(),
                created_at: std::time::Instant::now(),
                file_priorities: None,
                sequential: false,
                metadata: TaskMetadata {
                    name: None,
                    added_at_unix: 0,
                },
                limits: None,
            },
            engine_tid: Some(ih.to_string()),
            engine_kind: EngineKind::Bt,
            engine_status: None,
            events: vec![],
        };
        state.tasks.lock().insert(id.into(), rec);
    }

    fn source_proxy(state: &DaemonState, id: &str) -> Option<String> {
        match &state.tasks.lock().get(id).unwrap().task.source {
            DownloadSource::Http { proxy, .. } => proxy.clone(),
            other => panic!("source 应为 Http: {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_task_proxy_updates_record_and_engine() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task("https://example.com/f.bin".into(), None)
            .await
            .unwrap();

        state
            .set_task_proxy(&tid, Some("http://127.0.0.1:8080".into()))
            .await
            .unwrap();
        assert_eq!(
            source_proxy(&state, &tid),
            Some("http://127.0.0.1:8080".into()),
            "proxy 必须回显到任务 source（持久化口径）"
        );
        let engine_tid = state.tasks.lock().get(&tid).unwrap().engine_tid.clone();
        assert_eq!(
            fake.proxy_set_calls(),
            vec![(
                engine_tid.clone().unwrap(),
                Some("http://127.0.0.1:8080".into())
            )],
            "引擎必须收到热改调用（按 engine_tid 记录）"
        );
        // 清除回共享 client
        state.set_task_proxy(&tid, None).await.unwrap();
        assert_eq!(
            source_proxy(&state, &tid),
            None,
            "清除后 source.proxy = None"
        );
        assert_eq!(
            fake.proxy_set_calls()[1],
            (engine_tid.unwrap(), None),
            "清除语义必须透传引擎（None 而非空串）"
        );
    }

    #[tokio::test]
    async fn set_task_proxy_rejects_invalid_without_side_effect() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task("https://example.com/f.bin".into(), None)
            .await
            .unwrap();

        for bad in [
            "",                       // 空串：非法 URL 不是清除（清除传 None）
            "http://127.0.0.1:70000", // 端口越界
        ] {
            let r = state.set_task_proxy(&tid, Some(bad.to_string())).await;
            match r {
                Err(DaemonError::InvalidSource(m)) => {
                    assert!(!m.is_empty(), "{bad:?} 必须带错误说明");
                }
                other => panic!("非法 proxy {bad:?} 必须 InvalidSource: {other:?}"),
            }
        }
        assert!(
            fake.proxy_set_calls().is_empty(),
            "非法 proxy 不得触达引擎（零副作用）"
        );
        assert_eq!(source_proxy(&state, &tid), None, "记录保持原状");
        // 不存在的任务 → NotFound
        assert!(matches!(
            state
                .set_task_proxy("t404", Some("http://127.0.0.1:1".into()))
                .await,
            Err(DaemonError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn set_task_proxy_rejects_non_http_task() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        insert_bt_rec(&state, "t-bt", "ABC123");

        let r = state
            .set_task_proxy("t-bt", Some("http://127.0.0.1:8080".into()))
            .await;
        match r {
            Err(DaemonError::UnsupportedOp(m)) => {
                assert!(m.contains("HTTP"), "409 错误信息应定性仅 HTTP 支持: {m}");
            }
            other => panic!("BT 任务 set proxy 必须 UnsupportedOp(409): {other:?}"),
        }
        assert!(
            fake.proxy_set_calls().is_empty(),
            "HTTP 引擎不得收到 BT 任务的调用"
        );
    }
}

/// E9 名字回填：引擎 status().name（落盘名决策结果）→ 轮询回填
/// metadata.name（空缺时）→ 列表/快照透出链。显式名权威不被覆盖；幂等。
#[cfg(test)]
mod name_backfill_tests {
    use super::*;

    #[tokio::test]
    async fn poll_backfills_derived_name_once() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task("https://example.com/f.bin".into(), None)
            .await
            .unwrap();
        assert!(
            state
                .tasks
                .lock()
                .get(&tid)
                .unwrap()
                .task
                .metadata
                .name
                .is_none(),
            "前置：无显式名任务 metadata.name 为空"
        );
        fake.set_status_name("cd-derived.bin");

        // 首轮：迁移（Queued→Downloading，FakeEngine 恒 MetadataPending）+ 回填
        let effects = state.poll_engine_states().await;
        assert_eq!(effects.len(), 1, "首轮应有状态迁移");
        {
            let rec = state.tasks.lock().get(&tid).cloned().unwrap();
            assert_eq!(
                rec.task.metadata.name.as_deref(),
                Some("cd-derived.bin"),
                "引擎报的派生名必须回填 metadata.name"
            );
            assert!(
                rec.events.iter().any(|e| e.op == "name_backfilled"),
                "回填必须留事件痕迹"
            );
        }

        // 幂等：次轮零迁移零重复回填（to==from 纯无操作）
        let effects2 = state.poll_engine_states().await;
        assert!(effects2.is_empty(), "to==from 纯回填轮次不产生迁移广播");
        let rec2 = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(rec2.task.metadata.name.as_deref(), Some("cd-derived.bin"));
        let n = rec2
            .events
            .iter()
            .filter(|e| e.op == "name_backfilled")
            .count();
        assert_eq!(n, 1, "回填事件恰好一次（幂等）");

        // E7 透出链：列表条目 name 生效
        let list = state.list();
        assert_eq!(list[0].name.as_deref(), Some("cd-derived.bin"));
    }

    #[tokio::test]
    async fn poll_never_overrides_explicit_name() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task_opts(
                "https://example.com/f.bin".into(),
                None,
                AddHttpOpts {
                    name: Some("explicit.bin".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        fake.set_status_name("engine-name.bin");

        state.poll_engine_states().await;
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(
            rec.task.metadata.name.as_deref(),
            Some("explicit.bin"),
            "显式名权威——引擎回显同值也不覆盖"
        );
        assert!(
            !rec.events.iter().any(|e| e.op == "name_backfilled"),
            "显式名任务无回填事件"
        );
    }

    #[tokio::test]
    async fn poll_skips_tasks_with_engine_silent_name() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task("https://example.com/f.bin".into(), None)
            .await
            .unwrap();
        // 引擎不透出 name（status_name 恒 None）→ 不回填不事件
        state.poll_engine_states().await;
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert!(rec.task.metadata.name.is_none(), "引擎未报 → 保持 None");
        assert!(!rec.events.iter().any(|e| e.op == "name_backfilled"));
    }
}

/// E11 速率缓存：轮询器把引擎快照写入 `engine_status` → `/stats` 聚合生效；
/// BT 仅缓存不迁移（状态权威 = alert 流）；暂停/终态清零防陈旧速率。
#[cfg(test)]
mod rate_cache_tests {
    use super::*;

    /// 插入指定状态的 BT 任务记录（不联网，engine_tid = infohash）。
    fn insert_bt_rec_with(state: &DaemonState, id: &str, ih: &str, st: TaskState) {
        let rec = TaskRecord {
            task: DownloadTask {
                id: id.into(),
                canonical_id: CanonicalId {
                    kind: CanonicalKind::Bt,
                    identity: ih.to_string(),
                    validator: None,
                    token_sensitive: false,
                },
                source: DownloadSource::Magnet(format!("magnet:?xt=urn:btih:{ih}")),
                identity: ContentIdentity::SingleFile {
                    size: 0,
                    etag: None,
                    sha256: None,
                    backup_md5: None,
                },
                dest_root: PathBuf::from("."),
                files: vec![],
                acquisitions: vec![],
                aggregate: Default::default(),
                state: st,
                retry: Default::default(),
                created_at: std::time::Instant::now(),
                file_priorities: None,
                sequential: false,
                metadata: TaskMetadata {
                    name: None,
                    added_at_unix: 0,
                },
                limits: None,
            },
            engine_tid: Some(ih.to_string()),
            engine_kind: EngineKind::Bt,
            engine_status: None,
            events: vec![],
        };
        state.tasks.lock().insert(id.into(), rec);
    }

    #[tokio::test]
    async fn poll_caches_rates_into_stats_and_pause_zeroes() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task("https://example.com/f.bin".into(), None)
            .await
            .unwrap();
        assert_eq!(state.stats().down_bytes_s, 0, "轮询前缓存为空 → 聚合速率 0");

        fake.set_status_rates(1000, 50);
        state.poll_engine_states().await;
        let st = state.stats();
        assert_eq!(st.down_bytes_s, 1000, "HTTP 引擎报的下行速率应入 /stats");
        assert_eq!(st.up_bytes_s, 50);
        {
            let rec = state.tasks.lock().get(&tid).cloned().unwrap();
            let es = rec.engine_status.expect("缓存应有引擎快照");
            assert_eq!(es.down_rate, 1000);
            assert_eq!(es.up_rate, 50);
        }

        // 暂停：清零缓存速率（轮询器不再光顾暂停任务，不清则聚合虚高）
        state.pause(&tid).await.unwrap();
        let st = state.stats();
        assert_eq!(st.down_bytes_s, 0, "暂停后聚合下行速率必须清零");
        assert_eq!(st.up_bytes_s, 0, "暂停后聚合上行速率必须清零");
        assert_eq!(
            state.task_logs(&tid).unwrap()["state"],
            serde_json::json!("Paused")
        );
    }

    #[tokio::test]
    async fn poll_refreshes_rates_each_round() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        state
            .add_http_task("https://example.com/f.bin".into(), None)
            .await
            .unwrap();

        fake.set_status_rates(500, 0);
        state.poll_engine_states().await;
        assert_eq!(state.stats().down_bytes_s, 500);

        fake.set_status_rates(0, 0);
        state.poll_engine_states().await;
        assert_eq!(state.stats().down_bytes_s, 0, "缓存跟随引擎每轮刷新");
    }

    #[tokio::test]
    async fn bt_rates_cached_without_transition() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let state = DaemonState::new(fake.clone(), vec![]);
        insert_bt_rec_with(
            &state,
            "t-bt",
            "ABC123",
            TaskState::Downloading(EngineKind::Bt),
        );
        fake.set_status_rates(2000, 1000);

        let effects = state.poll_engine_states().await;
        assert!(effects.is_empty(), "BT 轮询仅缓存，不产生迁移效果");
        {
            let rec = state.tasks.lock().get("t-bt").cloned().unwrap();
            assert_eq!(
                rec.task.state,
                TaskState::Downloading(EngineKind::Bt),
                "BT 状态权威 = alert 流，轮询不得迁移"
            );
            let es = rec.engine_status.expect("BT 快照应入缓存");
            assert_eq!(es.down_rate, 2000);
            assert_eq!(es.up_rate, 1000);
        }
        let st = state.stats();
        assert_eq!(st.down_bytes_s, 2000);
        assert_eq!(st.up_bytes_s, 1000);
    }

    #[tokio::test]
    async fn seeding_bt_task_rates_cached() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let state = DaemonState::new(fake.clone(), vec![]);
        insert_bt_rec_with(&state, "t-seed", "DEF456", TaskState::Seeding);
        fake.set_status_rates(0, 800);

        let effects = state.poll_engine_states().await;
        assert!(effects.is_empty());
        let st = state.stats();
        assert_eq!(st.up_bytes_s, 800, "做种中上行速率应入聚合");
        assert_eq!(st.down_bytes_s, 0);
    }

    #[tokio::test]
    async fn paused_bt_task_not_polled() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let state = DaemonState::new(fake.clone(), vec![]);
        insert_bt_rec_with(&state, "t-paused", "FFF111", TaskState::Paused);
        fake.set_status_rates(999, 999);

        state.poll_engine_states().await;
        let rec = state.tasks.lock().get("t-paused").cloned().unwrap();
        assert!(
            rec.engine_status.is_none(),
            "暂停任务不在候选集——缓存不得被写入陈旧速率"
        );
        assert_eq!(state.stats().down_bytes_s, 0);
    }
}
