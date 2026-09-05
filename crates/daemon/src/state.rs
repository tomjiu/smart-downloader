//! DaemonState（M6 集成层）：任务目录 + HttpEngine + FallbackPolicy + WsHub；
//! add/pause/resume/remove/snapshot/list/provider 快照；重复 canonical → 409 事件。

use parking_lot::Mutex;
use smart_dl_core::identity::{CanonicalId, CanonicalKind, ContentIdentity};
use smart_dl_core::source_parse::normalize::{normalize_user_link, NormalizedSource};
use smart_dl_core::state_machine::TaskState;
use smart_dl_core::task::{DownloadTask, RetryState, TaskId, TaskMetadata};
#[cfg(any(feature = "ftp", feature = "xunlei-import"))]
use smart_dl_core::task::{FileState, TaskFile};
use smart_dl_core::types::{
    Auth, DownloadEngine, DownloadSource, EngineError, EngineKind, EngineState, EngineStatus,
    EngineTaskId, FileProgress, TrackerEntry,
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

    /// 失败处置统一入口（E30）：重试预算未用尽 → 清引擎句柄、任务回 Queued、
    /// 按指数退避安排到期重激活；预算用尽/未配置 → Failed 终态。锁内调用。
    /// 返回最终状态（Queued = 已安排重试，Failed = 终态）。
    fn fail_or_schedule_retry(&mut self, reason: Option<&str>) -> TaskState {
        if self.task.retry.retries < self.task.retry.max_retries {
            self.task.retry.retries += 1;
            let delay = retry_backoff_delay_s(self.task.retry.retries);
            self.task.metadata.next_retry_at_unix = now_unix() + delay;
            self.engine_tid = None;
            self.task.state = TaskState::Queued;
            self.push_event(
                "auto_retry",
                Some(format!(
                    "第 {}/{} 次自动重试已安排，{}s 后执行: {}",
                    self.task.retry.retries,
                    self.task.retry.max_retries,
                    delay,
                    reason.unwrap_or("")
                )),
            );
            TaskState::Queued
        } else {
            self.task.state = TaskState::Failed;
            TaskState::Failed
        }
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
    /// 实时速率（E13）：取自与 `done`/`total` 同一次引擎快照，非 `engine_status`
    /// 轮询缓存（缓存 2s 龄且仅活跃任务有写入，快照应即时）。记录级 Paused
    /// 恒 0（对齐 `pause()` 清零语义，防 <200ms 平滑窗口的陈旧值毛刺）。
    /// None = 引擎不可达/任务未接入引擎，序列化时省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rates: Option<TaskRates>,
    /// 累计统计（E33，BT 透出）：任务全生命周期累计下行/上行字节——BT 来自
    /// libtorrent all_time_download/all_time_upload（随 resume data 跨会话
    /// 持久），与 rates 取自同一次引擎快照。HTTP/FTP 等单向引擎无对等统计
    /// 恒 0（序列化省略）。累计非瞬时值，暂停不清零（与 rates 语义相反）。
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub total_downloaded: u64,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub total_uploaded: u64,
    /// 分享率（E33）：total_uploaded / total_downloaded，down 为 0 时 None
    /// （无数据/尚未产生下行，纯上传侧比率无意义）。None 序列化省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_ratio: Option<f64>,
    /// 任务级限速配置（KiB/s；None = 未设置走全局）。set 语义见
    /// `DaemonState::set_task_limits`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<smart_dl_core::task::TaskLimits>,
    /// 任务名（E7 透出：E6 显式名 / FTP URL 派生 / xunlei import；None = 引擎
    /// 派生链未回填，序列化时省略）。与列表 `TaskSummary::name` 同口径。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 用户标签（E18）。与列表 `TaskSummary::tags` 同口径（空省略）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// BT 子文件优先级表（None = 未设置走 libtorrent 默认 4；Some = 持久化
    /// 全量快照，下标 = 文件序）。set 语义见 `DaemonState::set_task_file_priorities`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_priorities: Option<Vec<u32>>,
    /// 顺序下载（边下边播）。set 语义见 `DaemonState::set_task_sequential`；
    /// false = 默认并行策略（不序列化，快照向后兼容）。
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub sequential: bool,
    /// 定时启动时刻（E23，unix 秒；0 = 未调度，序列化省略）。与列表
    /// `TaskSummary::start_at_unix` 同口径。
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub start_at_unix: u64,
    /// 已执行自动重试次数（E30；0 = 无，序列化省略）。与列表同口径。
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub retries: u64,
    /// 自动重试次数上限（E30；0 = 不自动重试，序列化省略）。与列表同口径。
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub max_retries: u64,
    /// 自动重试到期时刻（E30，unix 秒；0 = 无重试安排，序列化省略）。
    /// 非 0 且状态 Queued = 重试等待中，列表据此可展示「重试中」。
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub next_retry_at_unix: u64,
}

/// 实时速率（E13 透出）：与快照 `done`/`total` 同一次引擎快照取样——
/// HTTP/FTP 为引擎侧增量采样值（`RateSample`，快照按需查询与轮询器共用
/// 同一采样点，<200ms 窗口沿用平滑值），BT 为 FFI 实时值。字段名与
/// `DaemonStats` 聚合口径一致。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct TaskRates {
    /// 下行速率（B/s）。
    pub down_bytes_s: u64,
    /// 上行速率（B/s；仅 BT 等双向引擎非零）。
    pub up_bytes_s: u64,
}

/// 分享率计算（E33）：`uploaded / downloaded`；`down == 0`（无数据或尚未
/// 产生下行）时 None——除零与「纯上传侧比率」都不给值，序列化时省略。
/// 保留 3 位小数（qBittorrent 同级精度），负值/NaN 在源头（u64 字段）不可能。
pub fn share_ratio(uploaded: u64, downloaded: u64) -> Option<f64> {
    if downloaded == 0 {
        None
    } else {
        Some(((uploaded as f64) / (downloaded as f64) * 1000.0).round() / 1000.0)
    }
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
    /// 用户标签（E18）：空 = 无标签（序列化省略，不产生噪声字段）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// 定时启动时刻（E23，unix 秒；0 = 未调度，序列化省略）：未到期任务
    /// 停留 Queued 且不入引擎，列表据此可展示「定时中」。
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub start_at_unix: u64,
    /// 自动重试预算/进度（E30）：`retries`/`max_retries`/`next_retry_at_unix`
    /// 均 0 省略；非 0 = 任务配置了自动重试（或已安排重试）。
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub retries: u64,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub max_retries: u64,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub next_retry_at_unix: u64,
}

/// 列表过滤/分页查询（E7）。`states`/`engines` 空 = 不过滤；匹配均大小写不敏感。
/// `limit`/`offset` 由 HTTP 层校验（limit 1..=500，offset ≥ 0 由类型保证）后下推。
/// `search`（E14）：关键字子串匹配任务名或来源 URL（均大小写不敏感；
/// None/空串 = 不过滤），语料经 `DownloadSource::search_urls` 脱敏。
#[derive(Clone, Debug, Default)]
pub struct ListQuery {
    pub states: Vec<String>,
    pub engines: Vec<String>,
    pub limit: Option<usize>,
    pub offset: usize,
    pub search: Option<String>,
    /// 标签 any-of 过滤（E18）：空 = 不过滤；命中任一标签即保留（维度内
    /// OR、与 states/engines/search 维度间 AND）；大小写不敏感。
    pub tags: Vec<String>,
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
/// 文件冲突策略（E21）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// 覆盖既有文件（默认，旧行为）。
    Overwrite,
    /// 自动改名：`name.bin` → `name(1).bin` → `name(2).bin` … 取首个空闲。
    Rename,
    /// 跳过下载：任务直接置 Completed（既有文件保持原样），照常发完成事件/Webhook。
    Skip,
}

impl ConflictPolicy {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "overwrite" => Some(Self::Overwrite),
            "rename" => Some(Self::Rename),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
}

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
    /// 主源 SHA1 校验目标（E25，40 位十六进制）。与 sha256/md5 互斥
    /// （同时提供多个 → add 拒绝 InvalidSource）。
    pub sha1: Option<String>,
    /// 主源 MD5 校验目标（E25，32 位十六进制）。与 sha256/sha1 互斥。
    pub md5: Option<String>,
    /// 备用源 URL（主源探测/校验失败兕底，E2/E3）。http(s):// 前缀校验同主源。
    pub backup_url: Option<String>,
    /// 备用源 md5 校验目标（32 位十六进制）。必须与 backup_url 成对（单独给 md5
    /// 无处安放）；主源校验失败切备用源时由引擎既有身份切换逻辑接管。
    pub backup_md5: Option<String>,
    /// 用户显式落盘名（V3 语义：非法即拒；None = 引擎派生链 E4：CD → URL 末段 → 兕底）。
    pub name: Option<String>,
    /// 文件冲突策略（E21）：目标文件已存在时的处置。None = overwrite（默认）。
    /// 仅对显式名任务生效（派生名任务最终名在引擎侧 CD 才确定，v1 保持覆盖）。
    pub conflict: Option<ConflictPolicy>,
    /// 定时启动时刻（E23，unix 秒）：Some(未来) = 延迟入引擎，到点由调度
    /// 循环激活；Some(过去)/None/0 = 立即。仅 HTTP 分支消费（AddTaskReq
    /// 直传）；BT/FTP 走各自 add 参传同语义字段。
    pub start_at_unix: Option<u64>,
    /// 失败自动重试次数上限（E30，仅 HTTP/FTP 链路生效）：任务失败且预算未
    /// 用尽时清引擎句柄回 Queued，按指数退避（2s/4s/8s…封顶 60s）由调度
    /// 循环重激活。0 = 不自动重试（默认，保持既有一次性失败语义）。
    pub auto_retry: u32,
}

/// E30 退避延迟（秒）：第 n 次重试延迟 `2^n` s（2/4/8/…），封顶 60s。
/// 纯函数；n=0 不会被调用（重试预算判定在前），兑底 2。
fn retry_backoff_delay_s(retries: u32) -> u64 {
    2u64.saturating_pow(retries.min(31)).clamp(2, 60)
}

/// 校验和归一：小写化（引擎端 sha256/md5 摘要格式化为小写 hex，入参大写需归一后参与比较）。
fn normalize_digest(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

/// 当前 unix 秒（E23 调度判定用；时钟回拨/系统异常兑底 0 = 立即语义）。
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// serde skip_serializing_if 谓词：start_at_unix 为 0（未调度）时快照省略。
fn is_zero_u64(v: &u64) -> bool {
    *v == 0
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
        if let Some(s) = &self.sha1 {
            let s = normalize_digest(s);
            if !is_hex_digest(&s, 40) {
                return Err(format!("sha1 必须是 40 位十六进制: {s:?}"));
            }
        }
        if let Some(m) = &self.md5 {
            let m = normalize_digest(m);
            if !is_hex_digest(&m, 32) {
                return Err(format!("md5 必须是 32 位十六进制: {m:?}"));
            }
        }
        // E25 互斥：主源校验目标至多一个（引擎单槽位择一校验）
        let provided: Vec<&str> = [
            self.sha256.as_ref().map(|_| "sha256"),
            self.sha1.as_ref().map(|_| "sha1"),
            self.md5.as_ref().map(|_| "md5"),
        ]
        .into_iter()
        .flatten()
        .collect();
        if provided.len() > 1 {
            return Err(format!(
                "sha256/sha1/md5 主源校验目标互斥，至多提供一个（收到 {}）",
                provided.join(" + ")
            ));
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
    /// 全局限速总阀门当前值（E16）：启动时由 config 注入；运行中经
    /// POST /config/limit 或 TOML 热重载调整（apply_global_limits）。
    /// 不持久化（重启回到配置文件口径——与 dest_root 同为配置层，任务层
    /// 不感知）。
    global_limits: Mutex<GlobalLimits>,
    /// 任务完成 Webhook URL（E17）：Some = 完成态时 POST 通知；None = 禁用。
    /// serve 从 `[webhook] url` 注入，热重载跟随（refresh_config）。
    webhook_url: Mutex<Option<String>>,
    /// Webhook 投递 client（共享连接池；完成频率低，单实例足够）。
    webhook_client: reqwest::Client,
    /// Metalink 引导 XML 拉取 client（B1）：serve 注入引擎全局 client 克隆
    /// （同源代理/cookie jar/超时口径）；None（部分测试/嵌入式调用）时
    /// fetch_metalink_xml 按需新建裸 client 兜底。
    bootstrap_client: Option<reqwest::Client>,
    /// 完成后移动目标目录（E27）：Some = 完成后把落盘文件移入该目录；
    /// None = 禁用。serve 从 `[post_download] move_to` 注入。
    post_move_to: Mutex<Option<PathBuf>>,
    /// 完成后外部钩子程序（E27）：Some = 完成后 spawn 执行（环境变量传
    /// 任务上下文）；None = 禁用。serve 从 `[post_download] hook` 注入。
    post_hook: Mutex<Option<String>>,
    /// 自动清理当前配置（E20）：days=0 禁用；serve 注入 + 热重载跟随。
    cleanup: Mutex<crate::config::CleanupCfg>,
    /// 错峰随机延迟上限（E23，秒；0 = 关）：任务添加未显式 start_at 时在
    /// 0..=N 秒内延迟启动。serve 从 `[scheduler] start_jitter_seconds`
    /// 注入，热重载跟随（只影响新任务；AtomicU32 无锁读取，add 热路径）。
    start_jitter_secs: std::sync::atomic::AtomicU32,
}

/// 全局限速总阀门当前值（E16，KiB/s；0 = 不限）。
/// `max_download_kb_s` = 所有引擎合计下行上限；`max_upload_kb_s` = BT 合计
/// 上行上限（HTTP/FTP 无上传方向）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct GlobalLimits {
    pub max_download_kb_s: u32,
    pub max_upload_kb_s: u32,
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

mod bt_alerts;
mod lifecycle;
mod ops;
mod persistence;

// 路径稳定 re-export：外部引用（serve.rs/bt.rs/http.rs/events.rs）与
// state_tests.rs 的 `use super::*` 名字解析保持拆分前语义不变。
#[cfg(feature = "bt")]
pub(crate) use bt_alerts::btih_of;
#[cfg(feature = "bt")]
pub use bt_alerts::{torrent_infohash, torrent_precheck_total, torrent_total_size};
pub use bt_alerts::{FileMeta, TorrentMeta};
#[cfg(test)]
use lifecycle::ct_eq;
pub use ops::{canonical_http_url, ensure_dest_root, precheck_space};
pub use persistence::write_tasks_atomic;

#[cfg(test)]
#[path = "state_tests/mod.rs"]
mod state_tests;
