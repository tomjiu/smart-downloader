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
            global_limits: Mutex::new(GlobalLimits {
                max_download_kb_s: 0,
                max_upload_kb_s: 0,
            }),
            webhook_url: Mutex::new(None),
            webhook_client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
            post_move_to: Mutex::new(None),
            post_hook: Mutex::new(None),
            cleanup: Mutex::new(crate::config::CleanupCfg::default()),
            start_jitter_secs: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// 注入错峰随机延迟上限（E23；serve 从 `[scheduler] start_jitter_seconds` 传入）。
    pub fn with_start_jitter(self, secs: u32) -> Self {
        self.start_jitter_secs
            .store(secs, std::sync::atomic::Ordering::Relaxed);
        self
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

    /// 注入全局限速总阀门初始值（E16）：serve 从 config
    /// `[download] max_download_kb_s` + `[bt] max_upload_kb_s` 传入——
    /// 引擎构造时已携同值，此处仅同步内存口径（GET /config/限速查询一致）。
    pub fn with_global_limits(mut self, max_download_kb_s: u32, max_upload_kb_s: u32) -> Self {
        self.global_limits = Mutex::new(GlobalLimits {
            max_download_kb_s,
            max_upload_kb_s,
        });
        self
    }

    /// 读取全局限速总阀门当前值（E16）。
    pub fn global_limits(&self) -> GlobalLimits {
        *self.global_limits.lock()
    }

    /// 注入任务完成 Webhook URL（E17）：None/空 = 禁用。
    pub fn with_webhook_url(self, url: Option<String>) -> Self {
        *self.webhook_url.lock() = url.filter(|u| !u.is_empty());
        self
    }

    /// 注入完成自动处理配置（E27）：move_to/hook 均空 = 禁用。
    pub fn with_post_download(self, move_to: Option<String>, hook: Option<String>) -> Self {
        *self.post_move_to.lock() = move_to.filter(|s| !s.is_empty()).map(PathBuf::from);
        *self.post_hook.lock() = hook.filter(|s| !s.is_empty());
        self
    }

    /// 注入自动清理配置（E20）：serve 从 `[cleanup]` 传入，热重载跟随。
    pub fn with_cleanup(self, cfg: crate::config::CleanupCfg) -> Self {
        *self.cleanup.lock() = cfg;
        self
    }

    /// 全局限速总阀门热改（E16）：合并方向后下发各引擎（BT → FTP → HTTP 顺序，
    /// 可失败引擎先行保证近全有或全无），成功后同步内存值 + /config 快照覆盖
    /// + `global_limits_changed` 事件。
    ///
    /// - `None` 方向 = 不调整；`Some(0)` = 不限；`Some(n)` = 合计上限 n KiB/s
    /// - 双 `None` = 纯查询（返回当前值，零副作用）
    /// - 合并后值与当前一致 → 无变化 no-op（引擎侧已是该值，不发事件）
    /// - 引擎调用：HTTP/FTP 仅 down 方向；BT 双方向（settings_pack 全量语义，
    ///   代理原样重放）。`Unsupported`（引擎无该设施）静默跳过——引擎尽力
    ///   而为，不阻塞总阀门下发；`Other` 级失败 → Err（BT 先行故此时 HTTP
    ///   尚未改动，阀门状态保持一致）
    /// - 不落盘：重启回到配置文件口径（与 dest_root 同为配置层）
    pub async fn apply_global_limits(
        &self,
        down_kb_s: Option<u32>,
        up_kb_s: Option<u32>,
    ) -> Result<GlobalLimits, DaemonError> {
        let old = *self.global_limits.lock();
        if down_kb_s.is_none() && up_kb_s.is_none() {
            return Ok(old); // 纯查询
        }
        let effective = GlobalLimits {
            max_download_kb_s: down_kb_s.unwrap_or(old.max_download_kb_s),
            max_upload_kb_s: up_kb_s.unwrap_or(old.max_upload_kb_s),
        };
        if effective == old {
            return Ok(old); // 无变化 no-op
        }
        // 引擎下发：BT（可失败，FFI settings_pack）→ FTP/HTTP（原子 store，
        // 实际不可失败）——可失败者先行，失败时其余引擎未动，阀门保持旧值。
        // `Unsupported`（引擎无限速设施，如 NAS 远程引擎）静默跳过。
        if let Some(bt) = self.engines.get(&EngineKind::Bt).cloned() {
            Self::dispatch_global_limits(
                bt.as_ref(),
                Some(effective.max_download_kb_s),
                Some(effective.max_upload_kb_s),
                "BT",
            )
            .await?;
        }
        for kind in [EngineKind::Ftp, EngineKind::Http] {
            if let Some(eng) = self.engines.get(&kind).cloned() {
                Self::dispatch_global_limits(
                    eng.as_ref(),
                    Some(effective.max_download_kb_s),
                    None,
                    &format!("{kind:?}"),
                )
                .await?;
            }
        }
        *self.global_limits.lock() = effective;
        self.overlay_config_limits(effective);
        self.hub.publish(SchedulerEvent::GlobalLimitsChanged {
            max_download_kb_s: effective.max_download_kb_s,
            max_upload_kb_s: effective.max_upload_kb_s,
        });
        Ok(effective)
    }

    /// /config 快照限速两键覆盖（E16）：API/热重载改阀门后 GET /config 与
    /// 实际生效值保持一致（快照本身不含敏感项，覆盖安全）。
    fn overlay_config_limits(&self, g: GlobalLimits) {
        let mut snap = self.config_snapshot.lock();
        if let Some(v) = snap.as_mut() {
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "max_download_kb_s".into(),
                    serde_json::json!(g.max_download_kb_s),
                );
                obj.insert(
                    "max_upload_kb_s".into(),
                    serde_json::json!(g.max_upload_kb_s),
                );
            }
        }
    }

    /// 单引擎全局限速下发（E16 内部助手）：`Unsupported`（引擎无限速设施）
    /// 静默跳过；其余错误定性为阀门下发失败（Err）。
    async fn dispatch_global_limits(
        engine: &dyn DownloadEngine,
        down_kb_s: Option<u32>,
        up_kb_s: Option<u32>,
        label: &str,
    ) -> Result<(), DaemonError> {
        match engine.set_global_limits(down_kb_s, up_kb_s).await {
            Ok(()) => Ok(()),
            Err(EngineError::Unsupported) => Ok(()),
            Err(e) => Err(DaemonError::Engine(format!(
                "{label} 全局限速下发失败: {e}"
            ))),
        }
    }

    /// 任务完成事件统一出口（E17）：广播 `SchedulerEvent::Completed` +
    /// 触发完成 Webhook。三个完成转移点（HTTP/FTP 轮询循环、BT alert 流
    /// Seeding 转移、Provider 兜底成功）一律经此，保证事件与通知不脱钩。
    pub fn publish_task_completed(&self, task_id: &str) {
        // E20：完成时刻入档（自动清理判龄依据；记录不存在则跳过写点）
        {
            if let Some(rec) = self.tasks.lock().get_mut(task_id) {
                rec.task.metadata.finished_at_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
            }
        }
        self.hub.publish(SchedulerEvent::Completed {
            task_id: task_id.to_string(),
        });
        self.fire_completion_webhook(task_id);
        self.run_post_download_actions(task_id);
    }

    /// 完成自动处理（E27，清单 #15）：`[post_download] move_to` 移动 +
    /// `hook` 外部程序。fire-and-forget——失败仅记日志/事件，不反压链路；
    /// 未配置两者时零开销直返。锁内快照、锁外行动（同 webhook 纪律）。
    fn run_post_download_actions(&self, task_id: &str) {
        let move_to = self.post_move_to.lock().clone();
        let hook = self.post_hook.lock().clone();
        if move_to.is_none() && hook.is_none() {
            return;
        }
        // 锁内快照：任务名/引擎/落盘路径/conflict-skip 标记
        let snap = {
            let tasks = self.tasks.lock();
            let Some(rec) = tasks.get(task_id) else {
                return; // 任务已移除 → 无处理主体
            };
            let conflict_skip = rec
                .events
                .iter()
                .any(|e| e.op == "add" && e.detail.as_deref() == Some("conflict_skip"));
            Some((
                rec.task.metadata.name.clone(),
                kind_label(&rec.engine_kind),
                rec.task.dest_root.clone(),
                conflict_skip,
            ))
        };
        let Some((Some(name), engine, dest_root, conflict_skip)) = snap else {
            return; // 无名任务（BT metadata 未回填等）→ 无落盘文件可定位
        };
        let src = dest_root.join(&name);
        // 单文件门控：路径不存在 / 是目录（BT 多文件）→ 移动无意义；
        // hook 仍照发（webhook 同口径：通知尽力而为）
        let is_file = src.is_file();
        let mut final_path = src.clone();

        // 1) 移动（conflict-skip 任务不动既有文件——尊重 skip 语义）
        if let Some(dst_dir) = &move_to {
            if conflict_skip {
                tracing::info!("post_download: 任务 {task_id} 为 conflict-skip，既有文件不移动");
            } else if !is_file {
                tracing::info!(
                    "post_download: 任务 {task_id} 落盘路径非单文件（{:?}），移动跳过",
                    src
                );
            } else {
                match Self::move_completed_file(&src, dst_dir, &name) {
                    Ok(target) => {
                        tracing::info!("post_download: 任务 {task_id} 文件已移动 → {target:?}");
                        final_path = target.clone();
                        let mut tasks = self.tasks.lock();
                        if let Some(rec) = tasks.get_mut(task_id) {
                            rec.push_event(
                                "post_move",
                                Some(target.to_string_lossy().into_owned()),
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!("post_download: 任务 {task_id} 移动失败: {e}");
                        let mut tasks = self.tasks.lock();
                        if let Some(rec) = tasks.get_mut(task_id) {
                            rec.push_event("post_move", Some(format!("failed: {e}")));
                        }
                    }
                }
            }
        }

        // 2) 外部钩子（移动后终路径经 SD_FILE_PATH 传递；后台线程收尾）
        if let Some(prog) = &hook {
            let prog = prog.clone();
            let envs = vec![
                ("SD_TASK_ID".to_string(), task_id.to_string()),
                ("SD_TASK_NAME".to_string(), name),
                (
                    "SD_FILE_PATH".to_string(),
                    final_path.to_string_lossy().into_owned(),
                ),
                ("SD_ENGINE".to_string(), engine.to_string()),
            ];
            let hook_task_id = task_id.to_string();
            let hook_prog = prog.clone();
            std::thread::spawn(move || {
                match std::process::Command::new(&hook_prog)
                    .envs(envs)
                    .stdin(std::process::Stdio::null())
                    .output()
                {
                    Ok(out) if out.status.success() => {
                        tracing::info!("post_download: 任务 {hook_task_id} 钩子执行成功");
                    }
                    Ok(out) => {
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        tracing::warn!(
                            "post_download: 任务 {hook_task_id} 钩子非零退出 status={:?} stdout={stdout} stderr={stderr}",
                            out.status.code()
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "post_download: 任务 {hook_task_id} 钩子启动失败 {hook_prog:?}: {e}"
                        );
                    }
                }
            });
            let mut tasks = self.tasks.lock();
            if let Some(rec) = tasks.get_mut(task_id) {
                rec.push_event("post_hook", Some(prog));
            }
        }
    }

    /// 完成通知投递（E17）：fire-and-forget——单次 POST、5s 超时、失败仅记
    /// 警告日志（不重试不排队；通知属尽力而为，不得反压下载主链路）。
    /// 未配置 URL 时零开销直返。payload 从任务记录快照构建（锁内取值锁外投递）。
    fn fire_completion_webhook(&self, task_id: &str) {
        let Some(url) = self.webhook_url.lock().clone() else {
            return;
        };
        let payload = {
            let tasks = self.tasks.lock();
            match tasks.get(task_id) {
                None => return, // 任务已移除（完成通知失去主体）→ 静默
                Some(rec) => {
                    // 总字节：优先 add 探测 identity，缺省回退 E11 引擎快照缓存
                    //（HTTP 探测失败/信息聚合型源 identity.size=0 时仍有值）
                    let total_bytes = match &rec.task.identity {
                        ContentIdentity::SingleFile { size, .. } if *size > 0 => Some(*size),
                        _ => rec
                            .engine_status
                            .as_ref()
                            .map(|s| s.total)
                            .filter(|t| *t > 0),
                    };
                    serde_json::json!({
                        "event": "task_completed",
                        "task_id": rec.task.id,
                        "name": rec.task.metadata.name,
                        "engine": kind_label(&rec.engine_kind),
                        "total_bytes": total_bytes,
                        "finished_at_unix": std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                    })
                }
            }
        };
        let client = self.webhook_client.clone();
        tokio::spawn(async move {
            let result = client
                .post(&url)
                .timeout(std::time::Duration::from_secs(5))
                .json(&payload)
                .send()
                .await;
            match result {
                Ok(resp) if resp.status().is_success() => {}
                Ok(resp) => {
                    tracing::warn!("完成 Webhook 非成功响应: {url} status={}", resp.status())
                }
                Err(e) => tracing::warn!("完成 Webhook 投递失败: {url} {e}"),
            }
        });
    }

    /// 生效的 dest 白名单（V2）：未显式注入时兜底 default_dest_root。
    ///
    /// 锁序约定（docs/LOCK_MODEL.md）：顺序获取 `allowed_roots` → guard
    /// 语句尾即释放 → 按需获取 `default_dest_root`，两把锁任何路径不同时
    /// 持有——全域锁模型维持「任何时刻至多持一把锁」强不变量（2026-09
    /// 锁模型审计中唯一的多锁同持边，现已消除）。
    fn dest_roots(&self) -> Vec<PathBuf> {
        let g = self.allowed_roots.lock().clone();
        if g.is_empty() {
            vec![self.default_dest_root.lock().clone()]
        } else {
            g
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
                                        // E23：定时任务未到期 → 不入引擎（engine_tid 空），到点由调度
                                        // 循环激活。paused 意图保留（用户在调度等待期暂停过）——恢复后
                                        // 仍 Paused，激活器只认 Queued 不会误触发；resume = 立即激活。
            if t.metadata.start_at_unix > now_unix() {
                t.state = if was_paused {
                    TaskState::Paused
                } else {
                    TaskState::Queued
                };
                let mut rec = TaskRecord {
                    task: t,
                    engine_tid: None,
                    engine_kind: pt.engine_kind,
                    engine_status: None,
                    events: vec![],
                };
                rec.push_event("restored", Some("scheduled_start".into()));
                self.tasks.lock().insert(rec.task.id.clone(), rec);
                restored += 1;
                continue;
            }
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

    // ===== 定时/错峰下载（E23）=====
    //
    // 语义：start_at 在未来的任务**不接入引擎**（engine_tid 空、停留 Queued），
    // 到点由调度循环 `activate_due_tasks` 调引擎 add 激活（与普通 add 同链路，
    // 查重/预检/目录创建已在 add 路径完成）。两个入口：
    // - 显式定时：`AddTaskReq.start_at_unix`（unix 秒；过去时刻 = 立即，宽容不 400）
    // - 错峰：`[scheduler] start_jitter_seconds` > 0 时，未显式指定的任务在
    //   0..=N 秒内随机延迟启动（批量入队不被同时压向引擎/带宽）。

    /// 解析任务定时启动时刻（E23）：显式值直传（0/过去 = 立即）；未显式且
    /// 配置了错峰抖动 → now + 0..=jitter 秒（亚秒纳秒 ^ next_id 混熵，错峰
    /// 无需密码学随机）；否则 0（立即）。
    fn resolve_start_at(&self, explicit: Option<u64>) -> u64 {
        if let Some(t) = explicit {
            return t;
        }
        let jitter = self
            .start_jitter_secs
            .load(std::sync::atomic::Ordering::Relaxed) as u64;
        if jitter == 0 {
            return 0;
        }
        let nano = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0);
        let mix = nano
            ^ self
                .next_id
                .load(std::sync::atomic::Ordering::Relaxed)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15);
        now_unix() + (mix % (jitter + 1))
    }

    /// 落一条调度等待任务记录（E23）：不入引擎（engine_tid 空），到点由
    /// 调度循环激活。与 E21 conflict_skip 同款「有记录无句柄」形态。
    fn insert_scheduled_task(&self, task: DownloadTask, kind: EngineKind) -> String {
        let task_id = task.id.clone();
        let start_at = task.metadata.start_at_unix;
        let mut rec = TaskRecord {
            task,
            engine_tid: None,
            engine_kind: kind,
            engine_status: None,
            events: vec![],
        };
        rec.push_event("add", Some(format!("scheduled_start@{start_at}")));
        self.tasks.lock().insert(task_id.clone(), rec);
        self.autosave();
        self.hub.publish(SchedulerEvent::TaskCreated {
            task_id: task_id.clone(),
        });
        task_id
    }

    /// 激活单个定时任务（E23）：调引擎 add 接入 + 记录句柄 + 事件。
    /// add 失败/引擎不可用 → E30 重试拦截（预算未用尽安排退避重试）否则置
    /// Failed（对齐 restore add 失败语义）。激活成功时消费重试安排
    /// （next_retry_at 清零，快照不再显示过期时间）。
    /// 返回是否激活成功。调用方需保证任务处于 Queued（调度等待态）。
    async fn activate_one(&self, id: &str, task: DownloadTask, kind: EngineKind) -> bool {
        let engine = match self.engine_for(kind) {
            Ok(e) => e,
            Err(e) => {
                let retrying = {
                    let mut tasks = self.tasks.lock();
                    match tasks.get_mut(id) {
                        Some(rec) => {
                            rec.push_event("scheduled_start", Some(format!("引擎不可用: {e}")));
                            let to = rec.fail_or_schedule_retry(Some(&format!("引擎不可用: {e}")));
                            to == TaskState::Queued
                        }
                        None => false,
                    }
                };
                self.autosave();
                if !retrying {
                    self.hub.publish(SchedulerEvent::Failed {
                        task_id: id.to_string(),
                        reason: format!("定时激活引擎不可用: {e}"),
                    });
                }
                return false;
            }
        };
        match engine.add(&task).await {
            Ok(tid) => {
                {
                    let mut tasks = self.tasks.lock();
                    match tasks.get_mut(id) {
                        // 双检：激活间隙任务可能已被 resume 路径抢先激活/被移除。
                        // 激活成功即消费重试安排（E30：next_retry_at 清零）。
                        Some(rec) if rec.engine_tid.is_none() => {
                            rec.engine_tid = Some(tid);
                            rec.task.metadata.next_retry_at_unix = 0;
                            rec.push_event("scheduled_start", None);
                        }
                        _ => return false,
                    }
                }
                self.autosave();
                self.hub.publish(SchedulerEvent::TaskActivated {
                    task_id: id.to_string(),
                });
                true
            }
            Err(e) => {
                let retrying = {
                    let mut tasks = self.tasks.lock();
                    match tasks.get_mut(id) {
                        Some(rec) => {
                            rec.push_event("scheduled_start", Some(format!("引擎 add 失败: {e}")));
                            let to =
                                rec.fail_or_schedule_retry(Some(&format!("引擎 add 失败: {e}")));
                            to == TaskState::Queued
                        }
                        None => false,
                    }
                };
                self.autosave();
                if !retrying {
                    self.hub.publish(SchedulerEvent::Failed {
                        task_id: id.to_string(),
                        reason: format!("定时激活失败: {e}"),
                    });
                }
                false
            }
        }
    }

    /// 调度激活循环驱动点（E23+E30）：把到期任务（未接入引擎、Queued）逐个
    /// 接入引擎。到期判定：任务带重试安排（next_retry_at > 0）→ 按 next_retry_at
    /// 判定（重试安排优先，避免定时任务首次激活间隙被误读）；否则按 E23
    /// start_at 判定。serve 以 1s 周期驱动；测试可直接调用。返回激活成功的
    /// task_id 列表（保持迭代序）。
    pub async fn activate_due_tasks(&self) -> Vec<String> {
        let now = now_unix();
        let due: Vec<(String, DownloadTask, EngineKind)> = {
            let tasks = self.tasks.lock();
            tasks
                .iter()
                .filter(|(_, rec)| {
                    if rec.engine_tid.is_some() || rec.task.state != TaskState::Queued {
                        return false;
                    }
                    let m = &rec.task.metadata;
                    if m.next_retry_at_unix > 0 {
                        // E30：重试等待中——到期才激活
                        m.next_retry_at_unix <= now
                    } else {
                        // E23：定时启动等待中——到期才激活
                        m.start_at_unix > 0 && m.start_at_unix <= now
                    }
                })
                .map(|(id, rec)| (id.clone(), rec.task.clone(), rec.engine_kind))
                .collect()
        };
        let mut activated = Vec::new();
        for (id, task, kind) in due {
            if self.activate_one(&id, task, kind).await {
                activated.push(id);
            }
        }
        activated
    }

    /// 顺序下载变体：`sequential` 写入任务（HTTP=在飞窗口；BT=sequential
    /// flag；其余引擎忽略）。引擎 add 后对 BT 任务立即下发（handle 级 flag，
    /// metadata 未就绪也可设）。
    /// 任务级代理（E5）：仅 HTTP 任务生效；magnet/ed2k 等任务忽略该字段。
    /// 链接任务创建（E6 opts 收口）：HTTP 分支整体透传 `AddHttpOpts`；
    /// magnet 分支仅取 `sequential`（其余字段对 BT 无语义，静默忽略）；
    /// ed2k/ftp/xunlei 分支不受影响。
    /// 文件冲突改名候选（E21）：`a.bin` → `a(1).bin`（无扩展名 → `a(1)`）；
    /// 首个磁盘不存在的候选。上限 1000（防极端目录全占满时死循环）。
    fn bump_conflict_name(dir: &Path, name: &str) -> Option<String> {
        let (stem, ext) = match name.rsplit_once('.') {
            // 无扩展名或纯点：整体当 stem（对齐常见下载器行为）
            Some((st, e)) if !st.is_empty() && !e.is_empty() => (st, Some(e)),
            _ => (name, None),
        };
        for k in 1..1000 {
            let cand = match ext {
                Some(e) => format!("{stem}({k}).{e}"),
                None => format!("{stem}({k})"),
            };
            if !dir.join(&cand).exists() {
                return Some(cand);
            }
        }
        None
    }

    /// 完成文件移动（E27）：目标目录自动创建；同名冲突自动改名
    /// （`bump_conflict_name`）；同盘 rename 直达，跨盘（EXDEV 等错误）
    /// copy+remove 回退。返回最终落位路径。
    fn move_completed_file(src: &Path, dst_dir: &Path, name: &str) -> Result<PathBuf, String> {
        fs::create_dir_all(dst_dir).map_err(|e| format!("目标目录创建失败 {dst_dir:?}: {e}"))?;
        let target_name = if dst_dir.join(name).exists() {
            DaemonState::bump_conflict_name(dst_dir, name)
                .ok_or_else(|| format!("目标目录同名冲突且改名候选耗尽: {dst_dir:?}/{name}"))?
        } else {
            name.to_string()
        };
        let target = dst_dir.join(target_name);
        if let Err(e) = fs::rename(src, &target) {
            // 跨设备 rename 失败（EXDEV）→ copy + remove 回退
            fs::copy(src, &target).map_err(|e2| {
                let _ = fs::remove_file(&target); // 半份拷贝不留垃圾
                format!("rename 失败（{e}）且 copy 回退也失败: {e2}")
            })?;
            fs::remove_file(src)
                .map_err(|e| format!("copy 成功但源文件删除失败（存在重复副本）: {e}"))?;
        }
        Ok(target)
    }

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
                    return self
                        .add_bt_task_opts(m, dest_root, opts.sequential, opts.start_at_unix)
                        .await;
                }
                #[cfg(not(feature = "bt"))]
                {
                    let _ = opts.sequential;
                    let _ = opts.start_at_unix;
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
                    return self
                        .add_ftp_task_opts(u, dest_root, opts.start_at_unix)
                        .await;
                }
                #[cfg(not(feature = "ftp"))]
                {
                    let _ = opts.start_at_unix;
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
    /// `start_at_unix`（E23）：Some(未来) = 延迟入引擎（不调 engine.add），
    /// 到点由调度循环激活。
    #[cfg(feature = "bt")]
    async fn add_bt_task_opts(
        &self,
        magnet: String,
        dest_root: Option<String>,
        sequential: bool,
        start_at_unix: Option<u64>,
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
                sha1: None,
                md5: None,
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
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: self.resolve_start_at(start_at_unix),
                next_retry_at_unix: 0,
            },
            limits: None,
        };

        // E23 定时启动：start_at 未来 → 延迟入引擎（记录 Queued + 无句柄），
        // 到点由调度循环接入（engine.add 与查重/预检后置同链路）。
        if task.metadata.start_at_unix > now_unix() {
            return Ok(self.insert_scheduled_task(task, EngineKind::Bt));
        }

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
        self.add_torrent_task_opts(torrent_bytes, dest_root, false, None)
            .await
    }

    /// 顺序下载变体：`sequential` 写入任务 + 引擎 add 后立即下发 flag。
    /// `start_at_unix`（E23）：Some(未来) = 延迟入引擎，到点由调度循环激活。
    #[cfg(feature = "bt")]
    pub async fn add_torrent_task_opts(
        &self,
        torrent_bytes: Vec<u8>,
        dest_root: Option<String>,
        sequential: bool,
        start_at_unix: Option<u64>,
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
                sha1: None,
                md5: None,
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
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: self.resolve_start_at(start_at_unix),
                next_retry_at_unix: 0,
            },
            limits: None,
        };

        // E23 定时启动：start_at 未来 → 延迟入引擎，到点由调度循环接入。
        if task.metadata.start_at_unix > now_unix() {
            return Ok(self.insert_scheduled_task(task, EngineKind::Bt));
        }

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
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: 0,
                next_retry_at_unix: 0,
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
            sha1,
            md5,
            backup_url,
            backup_md5,
            name,
            conflict,
            start_at_unix,
            auto_retry,
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
        let sha1 = sha1.map(|s| normalize_digest(&s));
        let md5 = md5.map(|s| normalize_digest(&s));
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

        // E21 文件冲突策略：仅显式名任务可预判目标路径（派生名任务最终名在
        // 引擎侧 CD 才确定，v1 保持引擎默认覆盖）。`.part` 存在不属冲突
        //（那是续传现场），只看最终落盘名。
        let mut skip_download = false;
        let name = match (name, conflict) {
            (Some(n), Some(ConflictPolicy::Rename)) => {
                if dest_root.join(&n).exists() {
                    let bumped = Self::bump_conflict_name(&dest_root, &n).ok_or_else(|| {
                        DaemonError::InvalidSource("改名冲突：连续 1000 个候选名均被占用".into())
                    })?;
                    tracing::info!("冲突策略 rename: {n:?} → {bumped:?}");
                    Some(bumped)
                } else {
                    Some(n)
                }
            }
            (Some(n), Some(ConflictPolicy::Skip)) if dest_root.join(&n).exists() => {
                skip_download = true;
                Some(n)
            }
            (n, _) => n, // overwrite（默认）或目标不存在：原样
        };

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
                sha1,
                md5,
                backup_md5,
            },
            dest_root: dest_root.clone(),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: RetryState {
                retries: 0,
                max_retries: auto_retry,
            },
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential,
            metadata: TaskMetadata {
                name,
                added_at_unix: 0,
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: self.resolve_start_at(start_at_unix),
                next_retry_at_unix: 0,
            },
            limits: None,
        };

        // E21 skip：目标文件已在 → 不入引擎，任务直接落 Completed
        //（既有文件保持原样；完成事件/Webhook 照常——publish_task_completed
        // 一并写 finished_at）
        if skip_download {
            let mut rec = TaskRecord {
                task,
                engine_tid: None,
                engine_kind: EngineKind::Http,
                engine_status: None,
                events: vec![],
            };
            rec.push_event("add", Some("conflict_skip".into()));
            rec.task.state = TaskState::Completed;
            rec.task.identity = ContentIdentity::SingleFile {
                size: rec
                    .task
                    .metadata
                    .name
                    .as_deref()
                    .and_then(|n| dest_root.join(n).metadata().ok())
                    .map(|m| m.len())
                    .unwrap_or(0),
                etag: None,
                sha256: None,
                sha1: None,
                md5: None,
                backup_md5: None,
            };
            rec.task.metadata.finished_at_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            self.tasks.lock().insert(task_id.clone(), rec);
            self.autosave();
            self.hub.publish(SchedulerEvent::TaskCreated {
                task_id: task_id.clone(),
            });
            self.publish_task_completed(&task_id);
            return Ok(task_id);
        }
        // E23 定时启动：start_at 未来 → 延迟入引擎（记录 Queued + 无句柄），
        // 到点由调度循环接入。置于 conflict_skip 之后：文件已在即完成，
        // 调度无意义（两开关同时给出时 skip 优先）。
        if task.metadata.start_at_unix > now_unix() {
            return Ok(self.insert_scheduled_task(task, EngineKind::Http));
        }
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
        self.add_ftp_task_opts(url, dest_root, None).await
    }

    /// 定时变体（E23）：`start_at_unix` Some(未来) = 延迟入引擎，到点由
    /// 调度循环激活。
    #[cfg(feature = "ftp")]
    pub async fn add_ftp_task_opts(
        &self,
        url: String,
        dest_root: Option<String>,
        start_at_unix: Option<u64>,
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
                sha1: None,
                md5: None,
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
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: self.resolve_start_at(start_at_unix),
                next_retry_at_unix: 0,
            },
            limits: None,
        };

        // E23 定时启动：start_at 未来 → 延迟入引擎，到点由调度循环接入。
        if task.metadata.start_at_unix > now_unix() {
            return Ok(self.insert_scheduled_task(task, EngineKind::Ftp));
        }

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
        // E13：速率与 done/total 同源同新鲜度；Paused 记录态是显示层权威
        // （qB 式），速率同样以记录为准清零——引擎侧 <200ms 窗口会沿用
        // 平滑值（陈旧非零），不锁则暂停后最长 200ms 内快照仍报旧速率。
        let rates = status.as_ref().map(|s| {
            if matches!(effective_state, TaskState::Paused) {
                TaskRates {
                    down_bytes_s: 0,
                    up_bytes_s: 0,
                }
            } else {
                TaskRates {
                    down_bytes_s: s.down_rate,
                    up_bytes_s: s.up_rate,
                }
            }
        });
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
            rates,
            // E33：累计统计与分享率（同一次引擎快照取样，非 2s 轮询缓存）
            total_downloaded: status.as_ref().map(|s| s.total_downloaded).unwrap_or(0),
            total_uploaded: status.as_ref().map(|s| s.total_uploaded).unwrap_or(0),
            share_ratio: status
                .as_ref()
                .and_then(|s| share_ratio(s.total_uploaded, s.total_downloaded)),
            limits: rec.task.limits.clone(),
            file_priorities: rec.task.file_priorities.clone(),
            sequential: rec.task.sequential,
            name: rec.task.metadata.name.clone(),
            tags: rec.task.metadata.tags.clone(),
            start_at_unix: rec.task.metadata.start_at_unix,
            retries: rec.task.retry.retries as u64,
            max_retries: rec.task.retry.max_retries as u64,
            next_retry_at_unix: rec.task.metadata.next_retry_at_unix,
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
                    // E14：名字或脱敏 URL 子串命中即保留（大小写不敏感；
                    // 空针 contains 恒真 → 自然退化为不过滤）
                    && match &q.search {
                        None => true,
                        Some(needle) => {
                            let n = needle.trim().to_lowercase();
                            let name_hit = rec
                                .task
                                .metadata
                                .name
                                .as_deref()
                                .is_some_and(|s| s.to_lowercase().contains(&n));
                            let url_hit = rec
                                .task
                                .source
                                .search_urls()
                                .iter()
                                .any(|u| u.to_lowercase().contains(&n));
                            // E18：标签入搜索语料（名字/URL 同款子串命中）
                            let tag_hit = rec
                                .task
                                .metadata
                                .tags
                                .iter()
                                .any(|t| t.to_lowercase().contains(&n));
                            name_hit || url_hit || tag_hit
                        }
                    }
                    // E18：标签 any-of 过滤（空集合跳过；大小写不敏感）
                    && (q.tags.is_empty()
                        || rec.task.metadata.tags.iter().any(|t| {
                            q.tags
                                .iter()
                                .any(|want| want.eq_ignore_ascii_case(t))
                        }))
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
                tags: rec.task.metadata.tags.clone(),
                start_at_unix: rec.task.metadata.start_at_unix,
                retries: rec.task.retry.retries as u64,
                max_retries: rec.task.retry.max_retries as u64,
                next_retry_at_unix: rec.task.metadata.next_retry_at_unix,
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

    /// 按条件批量操作（E19）：`ListQuery` 选择器（states/engines/tags/search
    /// 复用列表过滤口径，无分页 → 全量命中集）解析命中任务后复用 `batch`。
    ///
    /// - **只开放非破坏性动作**（pause/resume）：按过滤条件选择后 remove 属
    ///   危险操作（误配过滤 = 批量误删），批量删除仍走显式 id 路径（E7）
    /// - 命中集上限 `batch_select` 内 1000（防御性；显式 id 批量另有 100 上限
    ///   语义，选择器面向"一键重试全部失败"这类可能超 100 的运维场景）
    /// - 命中集为空 → 空 BatchOutcome（幂等便利，不报错）
    pub async fn batch_select(
        &self,
        q: &ListQuery,
        action: BatchAction,
    ) -> Result<BatchOutcome, DaemonError> {
        if matches!(action, BatchAction::Remove { .. }) {
            return Err(DaemonError::InvalidSource(
                "按条件选择不支持 remove（批量删除请走显式 ids 路径）".into(),
            ));
        }
        // 无分页取全量命中（limit=None offset=0），按创建序稳定执行
        let (rows, _) = self.list_filtered(&ListQuery {
            states: q.states.clone(),
            engines: q.engines.clone(),
            limit: None,
            offset: 0,
            search: q.search.clone(),
            tags: q.tags.clone(),
        });
        const SELECT_CAP: usize = 1000;
        if rows.len() > SELECT_CAP {
            return Err(DaemonError::InvalidSource(format!(
                "条件命中数量超上限（{} > {SELECT_CAP}），请收窄选择条件",
                rows.len()
            )));
        }
        let ids: Vec<String> = rows.into_iter().map(|r| r.task_id).collect();
        Ok(self.batch(&ids, action).await)
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
        // E23：调度等待中任务（engine_tid 空 + Queued）无引擎句柄可暂停——
        // 语义 = 取消自动启动（记录置 Paused；start_at 保留供展示，激活器
        // 只认 Queued 不会误触发）。resume 回该任务 = 立即激活。
        // 其余无句柄任务（E21 skip Completed / restore add 失败 Failed）→
        // 落到引擎侧逻辑按 404 口径拒绝，与现行为一致。
        // 结构约束：锁作用域内不得出现 await（guard 非 Send，会污染整个
        // handler future 的 Send 判定）——锁内纯决策，await 全部在锁外。
        let decision = {
            let mut tasks = self.tasks.lock();
            match tasks.get_mut(id) {
                Some(rec) if rec.engine_tid.is_some() => Some(true), // 已被激活 → 引擎侧暂停
                Some(rec) => {
                    if rec.task.state != TaskState::Queued {
                        return Err(DaemonError::NotFound(id.to_string()));
                    }
                    rec.push_event("pause", Some("scheduled".into()));
                    rec.task.state = TaskState::Paused;
                    if let Some(es) = rec.engine_status.as_mut() {
                        es.down_rate = 0;
                        es.up_rate = 0;
                    }
                    Some(false) // 调度中 → 记录级暂停已完成
                }
                None => return Err(DaemonError::NotFound(id.to_string())),
            }
        };
        match decision {
            Some(true) => self.pause_engine_task(id).await,
            _ => {
                self.autosave();
                self.hub.publish(SchedulerEvent::StateChanged {
                    task_id: id.to_string(),
                    from: TaskState::Queued,
                    to: TaskState::Paused,
                });
                Ok(())
            }
        }
    }

    /// 引擎侧暂停（原 pause 主体，E23 拆出：调度中任务走记录级暂停分支）。
    async fn pause_engine_task(&self, id: &str) -> Result<(), DaemonError> {
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
        // E23：未接入引擎的任务（调度等待 Queued / 调度等待期被暂停 Paused）
        // → resume = 立即激活（消费定时，直接开始）。激活后记录态置
        // Downloading（对齐引擎侧 resume 语义；HTTP add 自启下载循环，BT 内
        // 核由 add 后正常下载链路接管）。
        // E32：终态 Failed（无句柄——激活失败/E30 激活失败路径）→ resume =
        // 手动重试：重新接入引擎。auto_retry 预算【不重置】——耗尽后手动
        // 重试仅再给一次机会，任务再败时 fail_or_schedule_retry 依既有计数
        // 直接终态（防预算白给循环）；有句柄 Failed 走下方引擎侧 resume
        // （httpdl epoch 重入 + 段账本续传，BT handle 恢复）语义不变。
        // 其余无句柄终态（E21 skip Completed / Stopped）→ 404 口径拒绝。
        let pending = {
            let tasks = self.tasks.lock();
            match tasks.get(id) {
                Some(rec) => {
                    if rec.engine_tid.is_none() {
                        Some((rec.task.clone(), rec.engine_kind))
                    } else {
                        None
                    }
                }
                None => return Err(DaemonError::NotFound(id.to_string())),
            }
        };
        if let Some((task, kind)) = pending {
            if !matches!(
                task.state,
                TaskState::Queued | TaskState::Paused | TaskState::Failed
            ) {
                return Err(DaemonError::NotFound(id.to_string()));
            }
            let from = task.state.clone();
            if !self.activate_one(id, task, kind).await {
                return Err(DaemonError::Engine(format!(
                    "任务 {id} 调度激活失败（任务已标 Failed，详情见任务事件）"
                )));
            }
            {
                let mut tasks = self.tasks.lock();
                if let Some(rec) = tasks.get_mut(id) {
                    if from == TaskState::Failed {
                        rec.push_event("retry", Some("手动重试（resume）".into()));
                    } else {
                        rec.push_event("resume", None);
                    }
                    rec.task.state = TaskState::Downloading(kind);
                }
            }
            self.autosave();
            self.hub.publish(SchedulerEvent::StateChanged {
                task_id: id.to_string(),
                from,
                to: TaskState::Downloading(kind),
            });
            return Ok(());
        }
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

    /// 任务重命名（E15）：`POST /tasks/:id/name`。显示层改名——落盘路径在
    /// 引擎 add 时即已决定（httpdl `resolved_name` 决策链），改名不迁移已
    /// 落盘/在传文件；名字是列表/快照透出与 E14 搜索语料（name 分量）。
    /// `None` = 清除显式名（E9 回填仅在"名字为空且引擎报名"的轮询点发生，
    /// 活跃任务下一轮可能自动补回派生名——清除语义即"交还派生链"）。
    /// 事件 detail 只记 set/cleared（与 proxy_changed 同口径，名字本体走
    /// 快照/列表查询，不进事件链路）。
    pub fn set_task_name(&self, id: &str, name: Option<String>) -> Result<(), DaemonError> {
        if let Some(n) = &name {
            if n.trim().is_empty() {
                return Err(DaemonError::InvalidSource(
                    "name 不能为空白（清除语义请传 null）".into(),
                ));
            }
            // V3 终审同函数：与 add 入参同一裁决点（非法路径分量即拒）
            smart_dl_core::session::output::sanitize_rel(n)
                .map_err(|e| DaemonError::InvalidSource(format!("name 非法: {e}")))?;
        }
        {
            let mut tasks = self.tasks.lock();
            let rec = tasks
                .get_mut(id)
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            let detail = if name.is_some() { "set" } else { "cleared" };
            rec.task.metadata.name = name;
            rec.push_event("name_changed", Some(detail.into()));
        }
        self.autosave();
        Ok(())
    }

    /// 任务标签设置（E18）：**替换式**全量覆盖（请求携带的标签列表即为最终权威，
    /// 语义可预测）；`None`/空表 = 清除全部。显示/分组元数据——引擎无关零副作用
    /// （对齐 set_task_name 边界），持久化随 tasks.json（TaskMetadata serde default
    /// 兼容旧档案），入 `?tag=` 过滤与 `?search=` 语料。
    ///
    /// 归一化：逐个 trim → 丢空串 → 去重（保留首次出现序，大小写敏感——
    /// 标签匹配大小写不敏感但显示保留原样）→ 上限 16 个/单个 1..=64 字符
    ///（超限 400 InvalidSource，调用方可先归一化再展示）。
    pub fn set_task_tags(
        &self,
        id: &str,
        tags: Option<Vec<String>>,
    ) -> Result<Vec<String>, DaemonError> {
        let normalized = match tags {
            None => Vec::new(),
            Some(list) => {
                if list.len() > 16 {
                    return Err(DaemonError::InvalidSource(format!(
                        "标签数量超上限 16（实际 {}）",
                        list.len()
                    )));
                }
                let mut out: Vec<String> = Vec::new();
                for t in list {
                    let t = t.trim();
                    if t.is_empty() {
                        continue;
                    }
                    if t.chars().count() > 64 {
                        return Err(DaemonError::InvalidSource(format!("标签超 64 字符: {t:?}")));
                    }
                    if !out.iter().any(|e| e == t) {
                        out.push(t.to_string());
                    }
                }
                out
            }
        };
        {
            let mut tasks = self.tasks.lock();
            let rec = tasks
                .get_mut(id)
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            let detail = if normalized.is_empty() {
                "cleared"
            } else {
                "set"
            };
            rec.task.metadata.tags = normalized.clone();
            rec.push_event("tags_changed", Some(detail.into()));
        }
        self.autosave();
        Ok(normalized)
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

    /// 列举任务 tracker 表（E29，仅 BT 任务；metadata 未就绪也可查）。
    pub async fn list_trackers(&self, id: &str) -> Result<Vec<TrackerEntry>, DaemonError> {
        let rec = self
            .tasks
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        if rec.engine_kind != EngineKind::Bt {
            return Err(DaemonError::UnsupportedOp(format!(
                "仅 BT 任务支持 tracker 管理（{id} 为 {:?}）",
                rec.engine_kind
            )));
        }
        let tid = rec
            .engine_tid
            .clone()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        let engine = self.engine_for(EngineKind::Bt)?;
        engine
            .list_trackers(&tid)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))
    }

    /// 批量追加 tracker（E29，仅 BT 任务）：URL 非空 + 无空白校验；
    /// 返回实际追加数。追加即时生效（libtorrent announce 表，metadata
    /// 未就绪也可设）。运行时追加不持久化（重启后以 magnet/.torrent 自带
    /// 表为准——与 webseed 注入同口径）。
    pub async fn add_trackers(&self, id: &str, urls: &[String]) -> Result<usize, DaemonError> {
        if urls.is_empty() {
            return Err(DaemonError::InvalidSource("urls 不能为空".into()));
        }
        for u in urls {
            if u.trim() != u || u.is_empty() || u.split_whitespace().count() != 1 {
                return Err(DaemonError::InvalidSource(format!(
                    "tracker URL 非法（空白/空串）: {u:?}"
                )));
            }
        }
        let rec = self
            .tasks
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        if rec.engine_kind != EngineKind::Bt {
            return Err(DaemonError::UnsupportedOp(format!(
                "仅 BT 任务支持 tracker 管理（{id} 为 {:?}）",
                rec.engine_kind
            )));
        }
        let tid = rec
            .engine_tid
            .clone()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        let engine = self.engine_for(EngineKind::Bt)?;
        engine
            .add_trackers(&tid, urls)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        if let Some(rec) = self.tasks.lock().get_mut(id) {
            rec.push_event("tracker", Some(format!("+{}", urls.len())));
        }
        Ok(urls.len())
    }

    /// 删 tracker（E29，仅 BT 任务）：URL 精确匹配；无匹配 → NotFound（404）。
    pub async fn remove_tracker(&self, id: &str, url: &str) -> Result<(), DaemonError> {
        let rec = self
            .tasks
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        if rec.engine_kind != EngineKind::Bt {
            return Err(DaemonError::UnsupportedOp(format!(
                "仅 BT 任务支持 tracker 管理（{id} 为 {:?}）",
                rec.engine_kind
            )));
        }
        let tid = rec
            .engine_tid
            .clone()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        let engine = self.engine_for(EngineKind::Bt)?;
        engine
            .remove_tracker(&tid, url)
            .await
            .map_err(|e| match e {
                smart_dl_core::types::EngineError::NotFound => {
                    DaemonError::NotFound(format!("tracker 不存在: {url}"))
                }
                other => DaemonError::Engine(other.to_string()),
            })?;
        if let Some(rec) = self.tasks.lock().get_mut(id) {
            rec.push_event("tracker", Some(format!("-{url}")));
        }
        Ok(())
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
        // E17：完成事件统一出口（广播 + Webhook）
        self.publish_task_completed(id);
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
        // E17：完成 Webhook URL 热重载（空 = 禁用）
        {
            let mut hook = self.webhook_url.lock();
            let new = (!cfg.webhook.url.is_empty()).then(|| cfg.webhook.url.clone());
            if *hook != new {
                tracing::info!("配置热重载: webhook_url {:?} → {:?}", *hook, new);
                *hook = new;
            }
        }
        // E20：自动清理配置热重载
        {
            let mut c = self.cleanup.lock();
            if *c != cfg.cleanup {
                tracing::info!(
                    "配置热重载: auto_remove_completed_days {} → {}",
                    c.auto_remove_completed_days,
                    cfg.cleanup.auto_remove_completed_days
                );
                *c = cfg.cleanup.clone();
            }
        }
        // E23：错峰抖动热重载（只影响之后新添加的任务；存量等待任务不受影响）
        {
            let old = self.start_jitter_secs.swap(
                cfg.scheduler.start_jitter_seconds,
                std::sync::atomic::Ordering::Relaxed,
            );
            if old != cfg.scheduler.start_jitter_seconds {
                tracing::info!(
                    "配置热重载: start_jitter_seconds {} → {}",
                    old,
                    cfg.scheduler.start_jitter_seconds
                );
            }
        }
    }

    /// 已完成任务自动清扫（E20）：扫描 Completed 且完成龄期 ≥
    /// `auto_remove_completed_days` 的任务，逐个按 `auto_remove_keep_data`
    /// 处置（默认保留文件）。返回本次清扫的任务 id（测试断言用）。
    /// days=0 禁用；记录无完成时刻（旧档未记）→ 跳过（不猜测龄期）。
    pub async fn sweep_completed_cleanup(&self) -> Vec<String> {
        let (days, keep_data) = {
            let c = self.cleanup.lock();
            (c.auto_remove_completed_days, c.auto_remove_keep_data)
        };
        if days == 0 {
            return Vec::new(); // 禁用
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let deadline = now.saturating_sub(days as u64 * 86_400);
        let due: Vec<String> = {
            let tasks = self.tasks.lock();
            tasks
                .values()
                .filter(|r| {
                    r.task.state == TaskState::Completed
                        && r.task.metadata.finished_at_unix > 0
                        && r.task.metadata.finished_at_unix <= deadline
                })
                .map(|r| r.task.id.clone())
                .collect()
        };
        let mut swept = Vec::new();
        for id in due {
            // remove_with 处置记录 + 引擎退役 + 落盘；保持清扫尽力而为不短路
            match self.remove_with(&id, !keep_data).await {
                Ok(()) => {
                    tracing::info!("自动清扫已完成任务: {id}（保留数据={keep_data}）");
                    swept.push(id);
                }
                Err(e) => tracing::warn!("自动清扫 {id} 失败（跳过）: {e}"),
            }
        }
        if !swept.is_empty() {
            tracing::info!("本次自动清扫 {} 个已完成任务", swept.len());
        }
        swept
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
                sha1: None,
                md5: None,
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
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: 0,
                next_retry_at_unix: 0,
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
                // BT 缓存分支：状态权威 = alert 流（状态不迁移、不落盘）。
                // E28：任务名回填在此放行——torrent metadata name 就绪 +
                // metadata.name 空缺 → 幂等回填 + 事件（E9 同语义：一次成功
                // 后 name 非 None 自然停）。快照缓存照旧整体入缓存。
                let mut tasks = self.tasks.lock();
                if let Some(rec) = tasks.get_mut(&id) {
                    // 双检：轮询间隙状态可能已被 alert 推进至终态
                    //（终态不缓存——与 apply_bt_alert 的终态清零同口径）
                    if matches!(
                        rec.task.state,
                        TaskState::Downloading(_) | TaskState::Seeding
                    ) {
                        if rec.task.metadata.name.is_none() {
                            if let Some(n) = &st.name {
                                rec.task.metadata.name = Some(n.clone());
                                rec.push_event("name_backfilled", Some(n.clone()));
                            }
                        }
                        rec.engine_status = Some(st.clone());
                    }
                }
                continue;
            }
            // Bug B 根因修复：autosave 移到锁外（persisted_tasks 重入同一把非重入锁
            // 会同线程自死锁——与 apply_bt_alert 同源缺陷）。
            let mut backfilled = false;
            let advanced: Option<(TaskState, TaskState)> = {
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
                let raw_to = engine_state_to_task(&st.state, kind);
                // E30：失败拦截——重试预算未用尽 → 清句柄回 Queued 安排退避
                // 重激活（调度循环到期重接入引擎）；用尽 → Failed 终态。
                let to = if raw_to == TaskState::Failed {
                    rec.fail_or_schedule_retry(st.error.as_deref())
                } else {
                    raw_to.clone()
                };
                if to == from {
                    // 已在目标态（活跃→活跃）：不迁移，但本轮回填/缓存仍生效
                    None
                } else {
                    // 错误随快照整体入缓存（st.error），无需单独写点
                    rec.task.state = to.clone();
                    Some((from, to))
                }
            }; // ← tasks 锁在此释放
            if backfilled {
                self.autosave(); // 锁外落盘：名字回填持久化（P4 G5 同口径）
            }
            if advanced.is_some() {
                self.autosave(); // 锁外落盘：终态/推进落盘（修复 Bug B 重入自死锁）
            }
            // 仅真迁移产生 effect（to==from 的纯回填轮次不广播）；
            // E30：to 取拦截后的实际目标（重试安排 = Queued，非引擎报的 Failed）
            if let Some((from, to)) = advanced {
                effects.push(HttpPollEffect {
                    task_id: id,
                    from,
                    to,
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
#[path = "state_tests.rs"]
mod state_tests;
