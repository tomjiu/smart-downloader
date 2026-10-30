//! S1 设置面：`GET /settings` 快照 + `PUT /settings` 部分更新（运行时生效 +
//! 可选落盘持久化）。qBittorrent 式设置域映射：
//!
//! | 域 | 生效语义 |
//! |---|---|
//! | `bandwidth`（基准限速 + 备用限速调度） | 立即（引擎总阀门热改，E16 同链路） |
//! | `connection.proxy` | BT 立即（settings_pack 全量重放）；HTTP 新任务生效（逐任务 client 构建） |
//! | `connection.bt_listen_port` / `bt_max_connections` | 立即（内核 settings_pack；非 bt 构建静默跳过） |
//! | `bittorrent`（DHT/LSD/UPnP/PEX/uTP/MSE 加密） | 立即（apply_discovery/apply_transport） |
//! | `download.dest_root` | 立即（默认落盘 + 白名单追加，V2 语义同热重载） |
//! | `download.disk_precheck_strict` | 重启生效（仅持久化） |
//! | `cleanup` / `post_download` / `webhook` / `scheduler` | 立即（同 refresh_config 热更语义） |
//! | `queue` | 持久化预留（门控接线见 BACKLOG S1-b，当前仅存配置） |
//!
//! B1（#214 设置覆盖度，2026-10）：config 16 域中 8 域首次接入设置面：
//!
//! | 域 | 生效语义 |
//!|---|---|
//! | `rss.max_processed_items_per_feed` | 立即（刷新时经 live_config 现读，无中间缓存） |
//! | `rss.auto_refresh` / `rss.refresh_interval_secs` | 重启生效（ticker 启动期固化周期/开关；PUT 仅持久化） |
//! | `download.fsync_*` 三件套 | 重启生效（烘入 HTTP 引擎；PUT 仅持久化） |
//! | `bittorrent.enabled` / `bittorrent.save_path` | 重启生效（引擎装配期读；PUT 仅持久化） |
//! | `server` / `provider` / `provider_xunlei` / `provider_quark` / `xunlei` | **只读快照**（装配期生效，不提供 PUT——避免「设置了看似生效实则没有」的假阳性，#160 同族预防）；`server.http_token` 只出 set/unset 布尔，明文永不出设置面 |
//!
//! B3（#214 §三 6/11/12，BT 调谐三件）：
//!
//! | 域 | 生效语义 |
//! |---|---|
//! | `bittorrent.dht_bootstrap_nodes` | 快照立即（内核热改不保证即时生效——DHT 重启[daemon 重启或切换 enable_dht]后保证生效；启动装配期三明治注入见 apply_startup_tuning） |
//! | `bittorrent.max_upload_slots` | 立即（内核 unchoke_slots_limit；-1 = 不限 / 0 = 复位默认 / >0 = 槽位） |
//! | `bittorrent.seeding_idle_limit_min` | 立即（daemon 侧执法：距上次上行负载达阈值自动暂停；口径 = upload_idle_secs 与做种起始取小） |
//!
//! B4+B5（#214 §三-13 日志级别 + §五-4 导入/导出）：
//!
//! | 面 | 生效语义 |
//!|---|---|
//! | `logging.level` | 立即（tracing reload；订阅者未装配的库嵌入降级为重启生效——诚实报告不虚报） |
//! | `GET /settings/export` | 权威配置全量 TOML 文本（含 http_token——与磁盘文件同密级，鉴权面内传出；UI 提示妥善保管） |
//! | `POST /settings/import` | 解析+校验 → 全域应用（热域立即 / 冷域重启）→ 原子落盘；语义 = 文件热重载与 PUT 各域的并集（见 import_settings） |
//!
//! 持久化：`PUT /settings?persist=true`（默认）在权威配置（live_config）上打
//! 补丁后原子回写 `--config` 文件（tmp+rename）。未指定 `--config` 时仅运行
//! 时生效（report.persisted=false）。热重载循环按文本变更检测（文件为准），
//! 与本路径天然协同：回写后 5s 内热重载读取同值，幂等 no-op。
//!
//! 事件：`settings_changed { keys }`（应用键全集）；限速变更另发
//! `global_limits_changed`（E16 既有语义，UI 曲线联动）。

use super::*;
use smart_dl_core::types::BtSessionPatch;
use ts_rs::TS;

/// `PUT /settings` 请求体：各域可选，域内字段可选（None = 不调整）。
#[derive(Debug, Clone, Default, serde::Deserialize, TS)]
#[serde(default)]
#[ts(export, optional_fields)]
pub struct SettingsReq {
    pub bandwidth: Option<BandwidthReq>,
    pub connection: Option<ConnectionReq>,
    pub bittorrent: Option<BtSessionReq>,
    pub download: Option<DownloadReq>,
    pub rss: Option<RssReq>,
    pub cleanup: Option<CleanupReq>,
    pub post_download: Option<PostDownloadReq>,
    pub webhook: Option<WebhookReq>,
    pub scheduler: Option<SchedulerReq>,
    pub queue: Option<QueueReq>,
    /// 日志级别域（B4，#214 §三-13）。
    pub logging: Option<LoggingReq>,
    /// 持久化开关（默认 true）；与 query `?persist=` 等价（body 优先）。
    pub persist: Option<bool>,
}

/// RSS 域（B1，#214）：全局刷新策略。max_processed 运行时生效；
/// auto_refresh / refresh_interval_secs 由 serve 启动期 ticker 固化，
/// PUT 仅持久化（重启生效）。
#[derive(Debug, Clone, Default, serde::Deserialize, TS)]
#[serde(default)]
#[ts(export, optional_fields)]
pub struct RssReq {
    /// 自动刷新总开关（重启生效：ticker 是否 spawn 在启动期决定）。
    pub auto_refresh: Option<bool>,
    /// 刷新间隔秒（重启生效；合法域 60..=604800，下限与 serve 同源）。
    pub refresh_interval_secs: Option<u64>,
    /// 单订阅已处理条目保留上限（立即生效：刷新路径经 live_config 现读）。
    pub max_processed_items_per_feed: Option<u32>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, TS)]
#[serde(default)]
#[ts(export, optional_fields)]
pub struct BandwidthReq {
    pub max_download_kb_s: Option<u32>,
    pub max_upload_kb_s: Option<u32>,
    pub alt_enabled: Option<bool>,
    pub alt_max_download_kb_s: Option<u32>,
    pub alt_max_upload_kb_s: Option<u32>,
    pub alt_from: Option<String>,
    pub alt_to: Option<String>,
    pub alt_days: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, TS)]
#[serde(default)]
#[ts(export, optional_fields)]
pub struct ConnectionReq {
    /// 全局代理 URL；空串 = 清除代理（直连）。
    pub proxy: Option<String>,
    pub bt_listen_port: Option<u16>,
    pub bt_max_connections: Option<u32>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, TS)]
#[serde(default)]
#[ts(export, optional_fields)]
pub struct BtSessionReq {
    pub enable_dht: Option<bool>,
    pub enable_lsd: Option<bool>,
    pub enable_upnp: Option<bool>,
    pub enable_pex: Option<bool>,
    pub enable_utp: Option<bool>,
    pub encrypt: Option<String>,
    /// 新建 BT 任务自动追加 tracker（qbit 对标）；整表替换。
    pub extra_trackers: Option<Vec<String>>,
    /// 做种分享率上限（qbit Share Ratio Limit 对标）；0 = 不启用。
    pub max_share_ratio: Option<f64>,
    /// 做种时长上限（分钟，qbit「做种时间限制」对标）；0 = 不启用。
    pub max_seeding_time_min: Option<u32>,
    /// DHT 引导节点（B3，qB「DHT 节点」对标）："host:port" 列表整表替换
    /// （空表 = 清除自定义回落内核默认）。运行中热改为快照先行（DHT
    /// 重启后生效，见 lt.h 契约注释）。
    pub dht_bootstrap_nodes: Option<Vec<String>>,
    /// 会话全局上传槽位（B3，qB「全局上传槽位」）：-1 = 不限；0 = 内核
    /// 默认（8）；>0 = 槽位数。运行中即时生效。
    pub max_upload_slots: Option<i32>,
    /// 做种空闲上限（分钟，B3，qB「当做种空闲时间达到」对标）；0 = 不启用。
    /// 运行中即时生效（daemon 侧执法）。
    pub seeding_idle_limit_min: Option<u32>,
    /// 存储模式（batch5 对标 qB「预分配磁盘空间」）：true = 新任务预分配。
    /// 会话级开关，仅重启时注入引擎（PUT 仅持久化）。
    pub storage_allocate: Option<bool>,
    /// BT 引擎总开关（B1，#214）：装配期生效（引擎是否注入），PUT 仅
    /// 持久化（重启生效；运行中引擎不摘除）。
    pub enabled: Option<bool>,
    /// BT 落盘目录（B1，#214）：空串 = 清除（回落跟随默认下载目录）；
    /// 装配期生效，PUT 仅持久化（重启生效）。
    pub save_path: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, TS)]
#[serde(default)]
#[ts(export, optional_fields)]
pub struct DownloadReq {
    pub dest_root: Option<String>,
    pub disk_precheck_strict: Option<bool>,
    /// HTTP 重定向最大跳数（batch5，1..=100）；仅重启时烘入 client。
    pub max_redirects: Option<u32>,
    /// 段数据落盘策略主开关（B1，#214；PR #128）：true = 每段 fsync 后
    /// 记账；false = 批量模式。烘入 HTTP 引擎，PUT 仅持久化（重启生效）。
    pub fsync_per_segment: Option<bool>,
    /// 批量模式阈值一：累计完成段数（1..=1024；重启生效）。
    pub fsync_batch_segments: Option<u32>,
    /// 批量模式阈值二：距上次落盘毫秒（100..=600000；重启生效）。
    pub fsync_batch_interval_ms: Option<u64>,
    /// 自定义 User-Agent（B2，#214）：空串 = 产品默认；新任务生效。
    pub user_agent: Option<String>,
    /// 代理例外列表（B2，#214）：整表替换；新任务生效。
    pub no_proxy: Option<Vec<String>>,
    /// TCP 连接超时秒（B2，#214，1..=300）；新任务生效。
    pub connect_timeout_secs: Option<u64>,
    /// 空闲读超时秒（B2，#214，1..=3600）；新任务生效。
    pub read_timeout_secs: Option<u64>,
    /// 单任务最大并发连接（B2，#214，0..=64；0 = 自动公式）；新任务生效。
    pub max_connections_per_task: Option<u32>,
    /// 分段粒度 MB（B2，#214，1..=1024）；仅无账本新下载采用。
    pub min_split_mb: Option<u64>,
    /// 全局自动重试默认次数（B2，#214，0..=10）；任务未指定 auto_retry 时
    /// 兜底；立即生效（add 时现读）。
    pub retry_max_attempts: Option<u32>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, TS)]
#[serde(default)]
#[ts(export, optional_fields)]
pub struct CleanupReq {
    pub auto_remove_completed_days: Option<u32>,
    pub auto_remove_keep_data: Option<bool>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, TS)]
#[serde(default)]
#[ts(export, optional_fields)]
pub struct PostDownloadReq {
    pub move_to: Option<String>,
    pub hook: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, TS)]
#[serde(default)]
#[ts(export, optional_fields)]
pub struct WebhookReq {
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, TS)]
#[serde(default)]
#[ts(export, optional_fields)]
pub struct SchedulerReq {
    pub start_jitter_seconds: Option<u32>,
    /// 全部任务终态后动作（Task 46）：none/exit/shutdown/sleep/hibernate；
    /// 非法值在应用层归一化回落 none。
    pub completion_action: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, TS)]
#[serde(default)]
#[ts(export, optional_fields)]
pub struct QueueReq {
    pub max_active_bt: Option<u32>,
    pub max_active_http: Option<u32>,
    pub max_active_ftp: Option<u32>,
    /// 跨引擎全局在传总量上限（#214 §三-10）；0 = 不限。
    /// 热键：门控读 live queue_cfg，PUT 即生效（后续 add/递补立即受闸）。
    pub max_active_total: Option<u32>,
}

/// 日志级别域（B4，#214 §三-13，qB「行为→日志」对标）：tracing 级别名
/// off/error/warn/info/debug/trace（大小写不敏感；trim 后比对）。立即生效
///（tracing reload）；RUST_LOG 环境变量在启动期优先，运行时热改覆盖之。
#[derive(Debug, Clone, Default, serde::Deserialize, TS)]
#[serde(default)]
#[ts(export, optional_fields)]
pub struct LoggingReq {
    /// 目标级别；未知值 400 整体拒绝（零副作用）。
    pub level: Option<String>,
}

/// `PUT /settings` 响应：应用结果分类 + 当前生效限速。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsApplyReport {
    /// 立即生效（或新任务生效）的设置键（点分路径）。
    pub applied: Vec<String>,
    /// 仅落盘、待重启生效的设置键。
    pub restart_required: Vec<String>,
    /// 是否已回写配置文件（无 `--config` 时 false）。
    pub persisted: bool,
    /// 应用后引擎实际生效限速（备用窗口命中时 = 备用值）。
    pub limits: GlobalLimits,
}

// ---------------------------------------------------------------------------
// GET /settings 快照
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// GET /settings 快照 DTO（原 serde_json::json! 动态构建；DTO 化后纳入
// ts-rs 生成，UI 手写 Settings 类型退役。wire 逐键一致：全部字段恒序列化
// （无 skip），Option 仅 meta.persist_path（None = 未指定 --config → null）。
// ---------------------------------------------------------------------------

/// 带宽域：基准限速（用户语义值）+ 备用限速调度窗口 + 引擎实际生效值。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsBandwidthDto {
    pub max_download_kb_s: u32,
    pub max_upload_kb_s: u32,
    pub alt_enabled: bool,
    pub alt_max_download_kb_s: u32,
    pub alt_max_upload_kb_s: u32,
    /// 窗口起点 `HH:MM`（本地时区）。
    pub alt_from: String,
    /// 窗口终点 `HH:MM`（本地时区）。
    pub alt_to: String,
    /// 适用星期（0=周日 .. 6=周六）；空 = 每天适用。
    pub alt_days: Vec<u8>,
    /// 备用窗口当前是否命中（只读展示）。
    pub alt_active_now: bool,
    /// 引擎实际生效限速（备用窗口命中时 = 备用值）。
    pub effective_max_download_kb_s: u32,
    pub effective_max_upload_kb_s: u32,
}

/// 连接域：代理（脱敏回显——明文凭据不出设置面）+ BT 监听。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsConnectionDto {
    pub proxy: String,
    pub bt_listen_port: u16,
    pub bt_max_connections: u32,
}

/// BT 域：发现/传输协议开关 + tracker/做种执法。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsBittorrentDto {
    pub enable_dht: bool,
    pub enable_lsd: bool,
    pub enable_upnp: bool,
    pub enable_pex: bool,
    pub enable_utp: bool,
    /// 加密策略：allow / require / disable。
    pub encrypt: String,
    pub extra_trackers: Vec<String>,
    pub max_share_ratio: f64,
    pub max_seeding_time_min: u32,
    /// DHT 引导节点（B3，qB「DHT 节点」对标）：空表 = 内核内置默认表。
    /// 热改后需 DHT 重启（重启 daemon 或切换 enable_dht）才保证生效。
    pub dht_bootstrap_nodes: Vec<String>,
    /// 会话全局上传槽位（B3，qB「全局上传槽位」）：-1 = 不限；0 = 内核
    /// 默认（8）；>0 = 槽位数。运行中即时生效。
    pub max_upload_slots: i32,
    /// 做种空闲上限（分钟，B3，qB「当做种空闲时间达到」对标）；0 = 不启用。
    /// 口径 = 距上次上行负载（从未上行自进入做种起算）。
    pub seeding_idle_limit_min: u32,
    /// 存储模式：true = 新任务预分配（batch5，重启生效）。
    pub storage_allocate: bool,
    /// 当前构建是否装配 BT 引擎（feature bt）。
    pub bt_available: bool,
    /// BT 引擎总开关（#214 B1，重启生效）：false 时下次启动不装配 BT 引擎。
    pub enabled: bool,
    /// BT 独立落盘目录（#214 B1，重启生效）；None = 跟随默认下载目录。
    pub save_path: Option<String>,
    /// 解析后实际落盘目录（save_path 或 dest_root；装配期采用值）。
    pub resolved_save_path: String,
}

/// 下载域：默认落盘目录 + 预检严格度 + 重定向预算 + fsync 落盘策略（B1）。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsDownloadDto {
    pub dest_root: String,
    pub disk_precheck_strict: bool,
    /// HTTP 重定向最大跳数（batch5，1..=100，重启生效）。
    pub max_redirects: u32,
    /// 段数据落盘策略主开关（#214 B1）：true = 每段 fsync 后记账（P0-4
    /// 最严粒度，SSD 推荐）；false = 批量模式（机械盘推荐）。
    pub fsync_per_segment: bool,
    /// 批量模式阈值一：自上次落盘累计完成段数。
    pub fsync_batch_segments: u32,
    /// 批量模式阈值二：距上次落盘时长（毫秒）。
    pub fsync_batch_interval_ms: u64,
    /// 自定义 User-Agent（B2，#214）：空 = 产品默认 smart-dl/&lt;version&gt;。
    pub user_agent: String,
    /// 代理例外列表（B2，#214）：host 后缀命中 → 绕过代理直连。
    pub no_proxy: Vec<String>,
    /// TCP 连接超时秒（B2，#214；默认 10）。
    pub connect_timeout_secs: u64,
    /// 空闲读超时秒（B2，#214；默认 30；无总超时——护长下载）。
    pub read_timeout_secs: u64,
    /// 单任务最大并发连接（B2，#214；0 = 自动公式 clamp(total/64MB,2,8)）。
    pub max_connections_per_task: u32,
    /// 分段粒度 MB（B2，#214；默认 16；账本恢复沿用原粒度）。
    pub min_split_mb: u64,
    /// 全局自动重试默认次数（B2，#214；0 = 任务不指定则不重试）。
    pub retry_max_attempts: u32,
}

/// 清理域：Completed 自动清扫。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsCleanupDto {
    pub auto_remove_completed_days: u32,
    /// true = 清扫时保留已下载数据（默认）。
    pub auto_remove_keep_data: bool,
}

/// 完成后处理域：移动 + 外部钩子（E27）。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsPostDownloadDto {
    pub move_to: String,
    pub hook: String,
}

#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsWebhookDto {
    pub url: String,
}

/// 调度域：错峰 + 全终态动作（Task 46）。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsSchedulerDto {
    pub start_jitter_seconds: u32,
    /// 全部任务终态后的动作：none/exit/shutdown/sleep/hibernate；每进程
    /// 会话最多触发一次；空任务表不触发。
    pub completion_action: String,
}

/// 队列域（S1）：各引擎同时在传任务上限；0 = 不限。
/// `max_active_total`（#214 §三-10）= 跨引擎全局总量闸，叠加在分引擎
/// 配额之上（取小生效）；同为热键（live queue_cfg 现读）。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsQueueDto {
    pub max_active_bt: u32,
    pub max_active_http: u32,
    pub max_active_ftp: u32,
    pub max_active_total: u32,
}

/// RSS 域（B1，#214）：全局刷新策略快照。max_processed 运行时可热改；
/// auto_refresh / refresh_interval_secs 重启生效（ticker 固化）。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsRssDto {
    /// 自动刷新开关（重启生效；false 时仅手动刷新）。
    pub auto_refresh: bool,
    /// 刷新间隔秒（重启生效；实际周期下限 60）。
    pub refresh_interval_secs: u64,
    /// 单订阅已处理条目保留上限（立即生效；防 rss.json 无限膨胀）。
    pub max_processed_items_per_feed: u32,
}

/// 服务端域（B1，#214，只读）：监听地址 + 鉴权/跨源装配快照。
/// `http_token` 明文永不出设置面（脱敏口径同 connection.proxy），
/// 只暴露 set/unset 布尔供 UI 提示「已配置鉴权」。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsServerDto {
    /// HTTP/WS 监听地址（如 127.0.0.1:8787）。
    pub addr: String,
    /// 是否已配置 Bearer 鉴权令牌（只读；明文不出）。
    pub http_token_set: bool,
    /// 额外允许的跨源请求 Host 白名单。
    pub extra_allowed_hosts: Vec<String>,
    /// 额外允许的跨源请求 Origin 白名单。
    pub extra_allowed_origins: Vec<String>,
}

/// 云兜底 Provider 总开关（B1，#214，只读；装配期生效）。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsProviderDto {
    /// 云兜底总开关（POST /tasks/:id/fallback 需 ≥1 可用 provider）。
    pub enabled: bool,
    /// MockProvider（开发/演示）。
    pub mock: bool,
    /// 运行中已装配的 provider 名单（assembly 结果，非配置声明）。
    pub assembled: Vec<String>,
}

/// 迅雷云盘 Provider（B1，#214，只读；装配期生效）。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsProviderXunleiDto {
    pub enabled: bool,
    /// 登录态 JSON 路径（登录写入由 CLI/示例侧完成，daemon 只读）。
    pub token_path: Option<String>,
    /// 身份档位：web / nas（解析后生效值：env 覆盖 > config > web）。
    pub tier: String,
    /// 档位是否来自环境变量覆盖（部署提示用）。
    pub tier_from_env: bool,
    /// 登录态文件当前是否存在（排障提示用；不读内容）。
    pub token_file_present: bool,
}

/// 夸克网盘 Provider（B1，#214，只读；装配期生效）。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsProviderQuarkDto {
    pub enabled: bool,
    /// Cookie 登录态 JSON 路径。
    pub cookie_path: Option<String>,
    /// 登录态文件当前是否存在（排障提示用；不读内容）。
    pub cookie_file_present: bool,
}

/// 迅雷 SDK 引擎（B1，#214，只读；装配期生效，Windows-only feature）。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsXunleiDto {
    /// 迅雷 SDK 引擎开关（需编译 --features xunlei）。
    pub enabled: bool,
    /// 当前构建是否装配迅雷引擎（feature xunlei）。
    pub available: bool,
    /// 迅雷 SDK 目录（含 DownloadSDKProxy.dll 等）。
    pub sdk_dir: Option<String>,
    /// 落盘目录；None = 跟随默认下载目录。
    pub save_path: Option<String>,
    /// 解析后实际落盘目录（save_path 或 dest_root）。
    pub resolved_save_path: String,
}

#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsMetaDto {
    /// 持久化目标；None = 未指定 --config（PUT 仅运行时生效）。
    pub persist_path: Option<String>,
}

/// 日志域（B4，#214 §三-13）：tracing 级别（canonical 小写）。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsLoggingDto {
    /// 当前配置级别（归一口径：off/error/warn/info/debug/trace）。启动期
    /// 实际过滤可能被 RUST_LOG 覆盖（开发者排障杠杆，热改后以本值为准）。
    pub level: String,
    /// 当前进程实际生效级别（logging::current_level 记账；与 level 不一致
    /// = RUST_LOG 覆盖中——UI 可提示「环境变量覆盖」）。
    pub effective_level: String,
}

/// `GET /settings` 全量快照。
#[derive(Debug, Clone, serde::Serialize, TS)]
#[ts(export)]
pub struct SettingsSnapshot {
    pub bandwidth: SettingsBandwidthDto,
    pub connection: SettingsConnectionDto,
    pub bittorrent: SettingsBittorrentDto,
    pub download: SettingsDownloadDto,
    pub rss: SettingsRssDto,
    pub cleanup: SettingsCleanupDto,
    pub post_download: SettingsPostDownloadDto,
    pub webhook: SettingsWebhookDto,
    pub scheduler: SettingsSchedulerDto,
    pub queue: SettingsQueueDto,
    pub server: SettingsServerDto,
    pub provider: SettingsProviderDto,
    pub provider_xunlei: SettingsProviderXunleiDto,
    pub provider_quark: SettingsProviderQuarkDto,
    pub xunlei: SettingsXunleiDto,
    pub meta: SettingsMetaDto,
    /// 日志域（B4，#214）。
    pub logging: SettingsLoggingDto,
}

impl DaemonState {
    /// 全量设置快照（`GET /settings`）：权威配置（live_config）为底，运行时
    /// 可变值（限速/备用/队列/清理等）以 state 当前值为准——两者在
    /// apply_settings/refresh_config 维护下保持同步。
    pub fn settings_snapshot(&self) -> SettingsSnapshot {
        let cfg = self.live_config.lock().clone().unwrap_or_default();
        let base = *self.base_limits.lock();
        let alt = self.alt_cfg.lock().clone();
        let effective = *self.global_limits.lock();
        let queue = self.queue_cfg.lock().clone();
        let cleanup = self.cleanup.lock().clone();
        // B1（#214）：解析目录先算后用——后续域字段会部分 move cfg（bt_resolved
        // 同理提前拿出，避免借用/move 交错）。
        let bt_resolved_save_path = cfg.bt_save_path().display().to_string();
        let xunlei_resolved_save_path = cfg.xunlei_save_path().display().to_string();
        // tier 解析走 &self 方法——须在结构体字面量部分 move cfg 之前调
        //（域字段直取不受影响，但 &self 方法与 partial move 互斥）。
        let xunlei_tier = cfg.resolve_xunlei_tier_name();
        let xunlei_tier_from_env = std::env::var("SMART_DL_XUNLEI_TIER")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        SettingsSnapshot {
            bandwidth: SettingsBandwidthDto {
                max_download_kb_s: base.max_download_kb_s,
                max_upload_kb_s: base.max_upload_kb_s,
                alt_enabled: alt.alt_enabled,
                alt_max_download_kb_s: alt.alt_max_download_kb_s,
                alt_max_upload_kb_s: alt.alt_max_upload_kb_s,
                alt_from: alt.alt_from,
                alt_to: alt.alt_to,
                alt_days: alt.alt_days,
                alt_active_now: self.alt_active.load(std::sync::atomic::Ordering::Relaxed),
                effective_max_download_kb_s: effective.max_download_kb_s,
                effective_max_upload_kb_s: effective.max_upload_kb_s,
            },
            connection: SettingsConnectionDto {
                // batch8（P2）：脱敏回显——代理 URL 可带 user:pass@（与
                // /config 快照同口径），明文凭据不出设置面。
                proxy: smart_dl_core::types::redact_url(&cfg.download.proxy),
                bt_listen_port: cfg.bt.listen_port,
                bt_max_connections: cfg.bt.max_connections,
            },
            bittorrent: SettingsBittorrentDto {
                enable_dht: cfg.bt.enable_dht,
                enable_lsd: cfg.bt.enable_lsd,
                enable_upnp: cfg.bt.enable_upnp,
                enable_pex: cfg.bt.enable_pex,
                enable_utp: cfg.bt.enable_utp,
                encrypt: cfg.bt.encrypt,
                extra_trackers: cfg.bt.extra_trackers,
                max_share_ratio: cfg.bt.max_share_ratio,
                max_seeding_time_min: cfg.bt.max_seeding_time_min,
                dht_bootstrap_nodes: cfg.bt.dht_bootstrap_nodes,
                max_upload_slots: cfg.bt.max_upload_slots,
                seeding_idle_limit_min: cfg.bt.seeding_idle_limit_min,
                storage_allocate: cfg.bt.storage_allocate,
                bt_available: self.engines.contains_key(&EngineKind::Bt),
                enabled: cfg.bt.enabled,
                save_path: cfg.bt.save_path.as_ref().map(|p| p.display().to_string()),
                resolved_save_path: bt_resolved_save_path,
            },
            download: SettingsDownloadDto {
                dest_root: self.default_dest_root.lock().display().to_string(),
                disk_precheck_strict: self.disk_precheck_strict,
                max_redirects: cfg.download.max_redirects,
                fsync_per_segment: cfg.download.fsync_per_segment,
                fsync_batch_segments: cfg.download.fsync_batch_segments,
                fsync_batch_interval_ms: cfg.download.fsync_batch_interval_ms,
                user_agent: cfg.download.user_agent.clone(),
                no_proxy: cfg.download.no_proxy.clone(),
                connect_timeout_secs: cfg.download.connect_timeout_secs,
                read_timeout_secs: cfg.download.read_timeout_secs,
                max_connections_per_task: cfg.download.max_connections_per_task,
                min_split_mb: cfg.download.min_split_mb,
                retry_max_attempts: cfg.download.retry_max_attempts,
            },
            rss: SettingsRssDto {
                auto_refresh: cfg.rss.auto_refresh,
                refresh_interval_secs: cfg.rss.refresh_interval_secs,
                max_processed_items_per_feed: cfg.rss.max_processed_items_per_feed,
            },
            cleanup: SettingsCleanupDto {
                auto_remove_completed_days: cleanup.auto_remove_completed_days,
                auto_remove_keep_data: cleanup.auto_remove_keep_data,
            },
            post_download: SettingsPostDownloadDto {
                move_to: self
                    .post_move_to
                    .lock()
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                hook: self.post_hook.lock().clone().unwrap_or_default(),
            },
            webhook: SettingsWebhookDto {
                url: self.webhook_url.lock().clone().unwrap_or_default(),
            },
            scheduler: SettingsSchedulerDto {
                start_jitter_seconds: self
                    .start_jitter_secs
                    .load(std::sync::atomic::Ordering::Relaxed),
                completion_action: self.completion_action.lock().clone(),
            },
            queue: SettingsQueueDto {
                max_active_bt: queue.max_active_bt,
                max_active_http: queue.max_active_http,
                max_active_ftp: queue.max_active_ftp,
                max_active_total: queue.max_active_total,
            },
            // —— B1 只读装配快照（#214）：装配期生效，PUT 不可写 ——
            server: SettingsServerDto {
                addr: cfg.server.addr.clone(),
                // 明文 token 永不出设置面（脱敏口径同 proxy）：只出 set/unset。
                http_token_set: self
                    .http_token
                    .as_deref()
                    .map(|t| !t.trim().is_empty())
                    .unwrap_or(false),
                extra_allowed_hosts: cfg.server.extra_allowed_hosts.clone(),
                extra_allowed_origins: cfg.server.extra_allowed_origins.clone(),
            },
            provider: SettingsProviderDto {
                enabled: cfg.provider.enabled,
                mock: cfg.provider.mock,
                // 装配结果（非配置声明）：providers 为 serve 注入的运行时名单。
                assembled: self
                    .providers
                    .iter()
                    .map(|p| p.name().to_string())
                    .collect(),
            },
            provider_xunlei: SettingsProviderXunleiDto {
                enabled: cfg.provider_xunlei.enabled,
                token_path: cfg
                    .provider_xunlei
                    .token_path
                    .as_ref()
                    .map(|p| p.display().to_string()),
                tier: xunlei_tier,
                tier_from_env: xunlei_tier_from_env,
                token_file_present: cfg
                    .provider_xunlei
                    .token_path
                    .clone()
                    .unwrap_or_else(|| std::path::PathBuf::from("xunlei_auth.json"))
                    .exists(),
            },
            provider_quark: SettingsProviderQuarkDto {
                enabled: cfg.provider_quark.enabled,
                cookie_path: cfg
                    .provider_quark
                    .cookie_path
                    .as_ref()
                    .map(|p| p.display().to_string()),
                cookie_file_present: cfg
                    .provider_quark
                    .cookie_path
                    .clone()
                    .unwrap_or_else(|| std::path::PathBuf::from("quark_auth.json"))
                    .exists(),
            },
            xunlei: SettingsXunleiDto {
                enabled: cfg.xunlei.enabled,
                available: cfg!(feature = "xunlei"),
                sdk_dir: cfg.xunlei.sdk_dir.as_ref().map(|p| p.display().to_string()),
                save_path: cfg
                    .xunlei
                    .save_path
                    .as_ref()
                    .map(|p| p.display().to_string()),
                resolved_save_path: xunlei_resolved_save_path,
            },
            meta: SettingsMetaDto {
                // 持久化目标；None = 未指定 --config（PUT 仅运行时生效）
                persist_path: (*self.config_path.lock())
                    .as_ref()
                    .map(|p| p.display().to_string()),
            },
            logging: SettingsLoggingDto {
                level: cfg.logging.normalized_level(),
                effective_level: crate::logging::current_level().to_string(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// PUT /settings 应用
// ---------------------------------------------------------------------------

impl DaemonState {
    /// 部分更新设置：验证 → 运行时下发 → 补丁权威配置 → 可选落盘 → 快照刷新
    /// + `settings_changed` 事件。任一验证失败整体 400（零副作用，先验证后写）。
    pub async fn apply_settings(
        &self,
        req: SettingsReq,
        persist_default: bool,
    ) -> Result<SettingsApplyReport, DaemonError> {
        // ---- 第一阶段：全量验证（零副作用）----
        let mut applied: Vec<String> = Vec::new();
        let mut restart_required: Vec<String> = Vec::new();

        if let Some(bt) = &req.bittorrent {
            if let Some(e) = bt.encrypt.as_deref() {
                let e = e.trim();
                if !matches!(e, "disable" | "allow" | "require") {
                    return Err(DaemonError::InvalidSource(format!(
                        "bittorrent.encrypt = {e:?} 无效：仅支持 disable / allow / require"
                    )));
                }
            }
            // extra_trackers：逐条 trim 非空 + 长度上限；条数上限 50
            if let Some(v) = &bt.extra_trackers {
                if v.len() > 50 {
                    return Err(DaemonError::InvalidSource(
                        "bittorrent.extra_trackers 最多 50 条".into(),
                    ));
                }
                for t in v {
                    let t = t.trim();
                    if t.is_empty() {
                        return Err(DaemonError::InvalidSource(
                            "bittorrent.extra_trackers 含空条目".into(),
                        ));
                    }
                    if t.len() > 512 {
                        return Err(DaemonError::InvalidSource(format!(
                            "bittorrent.extra_trackers 单条超长（>512）: {t:.32}…"
                        )));
                    }
                }
            }
            // max_share_ratio：非负 + 有限 + 上限 9999（0 = 关闭）
            if let Some(r) = bt.max_share_ratio {
                if !r.is_finite() || !(0.0..=9999.0).contains(&r) {
                    return Err(DaemonError::InvalidSource(format!(
                        "bittorrent.max_share_ratio = {r} 无效：须在 0.0..=9999.0（0 = 不启用）"
                    )));
                }
            }
            // max_seeding_time_min：上限 999999 分钟（0 = 关闭）
            if let Some(m) = bt.max_seeding_time_min {
                if m > 999_999 {
                    return Err(DaemonError::InvalidSource(format!(
                        "bittorrent.max_seeding_time_min = {m} 无效：须在 0..=999999（0 = 不启用）"
                    )));
                }
            }
            // B3：DHT 引导节点——条目门（≤32 × "host[:port]"；端口 1..=65535；
            // 拒 \r\n\t 与逗号[CSV 分隔符]防注入；host 非空 ≤253）。
            if let Some(nodes) = &bt.dht_bootstrap_nodes {
                if nodes.len() > 32 {
                    return Err(DaemonError::InvalidSource(format!(
                        "bittorrent.dht_bootstrap_nodes 最多 32 条（实际 {}）",
                        nodes.len()
                    )));
                }
                for n in nodes {
                    let n = n.trim();
                    if n.is_empty() {
                        return Err(DaemonError::InvalidSource(
                            "bittorrent.dht_bootstrap_nodes 含空条目".into(),
                        ));
                    }
                    if n.len() > 300
                        || n.contains(',')
                        || n.chars().any(|c| matches!(c, '\r' | '\n' | '\t' | '\0'))
                    {
                        return Err(DaemonError::InvalidSource(format!(
                            "bittorrent.dht_bootstrap_nodes 条目非法: {n:.48}…"
                        )));
                    }
                    // host[:port] 拆分：末段全数字视作端口（IPv6 含多冒号不拆）
                    let (host, port) = match n.rsplit_once(':') {
                        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
                            (h, Some(p))
                        }
                        _ => (n, None),
                    };
                    if host.is_empty() || host.len() > 253 {
                        return Err(DaemonError::InvalidSource(format!(
                            "bittorrent.dht_bootstrap_nodes host 非法: {n:.48}…"
                        )));
                    }
                    if let Some(p) = port {
                        let Ok(v) = p.parse::<u32>() else {
                            return Err(DaemonError::InvalidSource(format!(
                                "bittorrent.dht_bootstrap_nodes 端口非法: {n:.48}…"
                            )));
                        };
                        if v == 0 || v > 65535 {
                            return Err(DaemonError::InvalidSource(format!(
                                "bittorrent.dht_bootstrap_nodes 端口越界（1..=65535）: {n:.48}…"
                            )));
                        }
                    }
                }
            }
            // B3：全局上传槽位——-1（不限）/ 0（内核默认）/ 1..=1000
            if let Some(s) = bt.max_upload_slots {
                if !(-1..=1000).contains(&s) {
                    return Err(DaemonError::InvalidSource(format!(
                        "bittorrent.max_upload_slots = {s} 无效：-1（不限）/ 0（内核默认）/ 1..=1000"
                    )));
                }
            }
            // B3：做种空闲上限（0 = 关闭；上限同时长口径 999999 分钟）
            if let Some(m) = bt.seeding_idle_limit_min {
                if m > 999_999 {
                    return Err(DaemonError::InvalidSource(format!(
                        "bittorrent.seeding_idle_limit_min = {m} 无效：须在 0..=999999（0 = 不启用）"
                    )));
                }
            }
            // B1（#214）：save_path 只做可打印性/长度检查（目录创建在重启装配期，
            // 与 serve 对 bt_save_path 的 create_dir_all 同源）；空串 = 清除回落。
            if let Some(p) = bt.save_path.as_deref() {
                if p.trim().len() > 4096 {
                    return Err(DaemonError::InvalidSource(
                        "bittorrent.save_path 超长（>4096）".into(),
                    ));
                }
                if p.contains('\0') {
                    return Err(DaemonError::InvalidSource(
                        "bittorrent.save_path 含非法字符（NUL）".into(),
                    ));
                }
            }
        }
        if let Some(c) = &req.connection {
            if let Some(p) = c.proxy.as_deref() {
                validate_proxy_url(p)?;
            }
        }
        if let Some(b) = &req.bandwidth {
            if let Some(d) = &b.alt_days {
                if d.iter().any(|x| *x > 6) {
                    return Err(DaemonError::InvalidSource(
                        "bandwidth.alt_days 取值须为 0..=6（0=周日）".into(),
                    ));
                }
            }
            for (k, v) in [("alt_from", &b.alt_from), ("alt_to", &b.alt_to)] {
                if let Some(s) = v {
                    if parse_hhmm(s).is_none() {
                        return Err(DaemonError::InvalidSource(format!(
                            "bandwidth.{k} = {s:?} 无效：须为 HH:MM（00:00..=23:59）"
                        )));
                    }
                }
            }
        }
        if let Some(d) = &req.download {
            if let Some(root) = d.dest_root.as_deref() {
                if root.trim().is_empty() {
                    return Err(DaemonError::InvalidSource(
                        "download.dest_root 不能为空".into(),
                    ));
                }
            }
            if let Some(v) = d.max_redirects {
                if !(1..=100).contains(&v) {
                    return Err(DaemonError::InvalidSource(format!(
                        "download.max_redirects = {v} 无效：须在 1..=100"
                    )));
                }
            }
            // B1（#214）：fsync 三件套——合法域与 config.rs 加载期钳制同源，
            // 但 PUT 侧从严拒绝（先验证后写，零副作用）而非静默钳制。
            if let Some(v) = d.fsync_batch_segments {
                if !(1..=1024).contains(&v) {
                    return Err(DaemonError::InvalidSource(format!(
                        "download.fsync_batch_segments = {v} 无效：须在 1..=1024"
                    )));
                }
            }
            if let Some(v) = d.fsync_batch_interval_ms {
                if !(100..=600_000).contains(&v) {
                    return Err(DaemonError::InvalidSource(format!(
                        "download.fsync_batch_interval_ms = {v} 无效：须在 100..=600000"
                    )));
                }
            }
            // B2（#214）：HTTP 调谐域验证。加载期宽容钳制（config.rs
            // http_tuning），PUT 侧从严拒绝——先验证后写零副作用。
            if let Some(v) = d.user_agent.as_deref() {
                let t = v.trim();
                if t.len() > 512 {
                    return Err(DaemonError::InvalidSource(
                        "download.user_agent 超长（>512）".into(),
                    ));
                }
                if t.contains(['\r', '\n']) {
                    return Err(DaemonError::InvalidSource(
                        "download.user_agent 含换行符（头注入防护）".into(),
                    ));
                }
            }
            if let Some(list) = &d.no_proxy {
                if list.len() > 64 {
                    return Err(DaemonError::InvalidSource(
                        "download.no_proxy 最多 64 条".into(),
                    ));
                }
                for e in list {
                    let e = e.trim();
                    if e.is_empty() {
                        return Err(DaemonError::InvalidSource(
                            "download.no_proxy 含空条目".into(),
                        ));
                    }
                    if e.len() > 256 {
                        return Err(DaemonError::InvalidSource(
                            "download.no_proxy 单条超长（>256）".into(),
                        ));
                    }
                }
            }
            if let Some(v) = d.connect_timeout_secs {
                if !(1..=300).contains(&v) {
                    return Err(DaemonError::InvalidSource(format!(
                        "download.connect_timeout_secs = {v} 无效：须在 1..=300"
                    )));
                }
            }
            if let Some(v) = d.read_timeout_secs {
                if !(1..=3600).contains(&v) {
                    return Err(DaemonError::InvalidSource(format!(
                        "download.read_timeout_secs = {v} 无效：须在 1..=3600"
                    )));
                }
            }
            if let Some(v) = d.max_connections_per_task {
                if v > 64 {
                    return Err(DaemonError::InvalidSource(format!(
                        "download.max_connections_per_task = {v} 无效：须在 0..=64（0 = 自动公式）"
                    )));
                }
            }
            if let Some(v) = d.min_split_mb {
                if !(1..=1024).contains(&v) {
                    return Err(DaemonError::InvalidSource(format!(
                        "download.min_split_mb = {v} 无效：须在 1..=1024"
                    )));
                }
            }
            if let Some(v) = d.retry_max_attempts {
                // 与任务级 auto_retry 同门（E30：0..=10 防极端配置拖垮调度循环）
                if v > 10 {
                    return Err(DaemonError::InvalidSource(format!(
                        "download.retry_max_attempts = {v} 无效：须在 0..=10"
                    )));
                }
            }
        }
        // B1（#214）：rss 域验证。refresh_interval_secs 下限 60 与 serve
        // ticker 的 .max(60) 同源（防误配风暴）；上限 7 天防 rss.json 膨胀
        // 前的空窗过长。max_processed_items_per_feed 下限 1：0 会截掉全部
        // 已处理条目 → 去重标记丢失 → 源刷新时全量重复建任务（P1-10 同族）。
        if let Some(r) = &req.rss {
            if let Some(v) = r.refresh_interval_secs {
                if !(60..=604_800).contains(&v) {
                    return Err(DaemonError::InvalidSource(format!(
                        "rss.refresh_interval_secs = {v} 无效：须在 60..=604800（秒）"
                    )));
                }
            }
            if let Some(v) = r.max_processed_items_per_feed {
                if !(1..=100_000).contains(&v) {
                    return Err(DaemonError::InvalidSource(format!(
                        "rss.max_processed_items_per_feed = {v} 无效：须在 1..=100000"
                    )));
                }
            }
        }
        // B4（#214 §三-13）：日志级别——白名单从严（六值，大小写不敏感）；
        // 加载期宽容回落 info 的口径仅限 config.rs 启动路径，设置面不笼络。
        if let Some(l) = &req.logging {
            if let Some(v) = l.level.as_deref() {
                if crate::logging::normalize_level(v).is_none() {
                    return Err(DaemonError::InvalidSource(format!(
                        "logging.level = {v:?} 无效：仅支持 off / error / warn / info / debug / trace"
                    )));
                }
            }
        }
        // B6（#214 §三-10）：全局并发总量——0 = 不限；上限 100000（防误输
        // 巨值反而等同不限的困惑；分引擎三项历史无界保持不动，新键从严）。
        if let Some(q) = &req.queue {
            if let Some(t) = q.max_active_total {
                if t > 100_000 {
                    return Err(DaemonError::InvalidSource(format!(
                        "queue.max_active_total = {t} 无效：须在 0..=100000（0 = 不限）"
                    )));
                }
            }
        }

        // ---- 第二阶段：运行时应用（顺序无关失败即中止；各域幂等）----
        let mut limits_touched = false;
        if let Some(b) = &req.bandwidth {
            if b.max_download_kb_s.is_some() || b.max_upload_kb_s.is_some() {
                limits_touched = true;
                self.apply_global_limits(b.max_download_kb_s, b.max_upload_kb_s)
                    .await?;
                // 权威配置同步（persist 回写带宽域的唯一来源）
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    if let Some(v) = b.max_download_kb_s {
                        cfg.download.max_download_kb_s = v;
                    }
                    if let Some(v) = b.max_upload_kb_s {
                        cfg.bt.max_upload_kb_s = v;
                    }
                }
                applied.push("bandwidth.max_download_kb_s".into());
                applied.push("bandwidth.max_upload_kb_s".into());
            }
            // 备用限速：补丁配置 → 立即重评估窗口（可能切换引擎实际值）
            let mut alt = self.alt_cfg.lock().clone();
            let alt_changed = [
                b.alt_enabled.map(|v| alt.alt_enabled = v),
                b.alt_max_download_kb_s
                    .map(|v| alt.alt_max_download_kb_s = v),
                b.alt_max_upload_kb_s.map(|v| alt.alt_max_upload_kb_s = v),
                b.alt_from.clone().map(|v| alt.alt_from = v),
                b.alt_to.clone().map(|v| alt.alt_to = v),
                b.alt_days.clone().map(|v| alt.alt_days = v),
            ]
            .iter()
            .any(|x| x.is_some());
            if alt_changed {
                *self.alt_cfg.lock() = alt.clone();
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    cfg.limits = alt.clone();
                }
                limits_touched = true;
                for k in [
                    "alt_enabled",
                    "alt_max_download_kb_s",
                    "alt_max_upload_kb_s",
                    "alt_from",
                    "alt_to",
                    "alt_days",
                ] {
                    applied.push(format!("bandwidth.{k}"));
                }
            }
        }
        if let Some(c) = &req.connection {
            if let Some(p) = c.proxy.as_deref() {
                let proxy = (!p.trim().is_empty()).then(|| p.trim().to_string());
                self.dispatch_global_proxy(proxy.as_deref()).await?;
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    cfg.download.proxy = proxy.clone().unwrap_or_default();
                }
                applied.push("connection.proxy".into());
            }
            // BT 会话连接参数（监听端口/连接数上限）：经 BtSessionPatch 下发；
            // 非 bt 构建（引擎缺失）静默跳过——值仍持久化，启用 bt 后按配置生效。
            if c.bt_listen_port.is_some() || c.bt_max_connections.is_some() {
                let patch = BtSessionPatch {
                    listen_port: c.bt_listen_port,
                    max_connections: c.bt_max_connections,
                    ..BtSessionPatch::default()
                };
                self.dispatch_bt_patch(&patch).await;
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    if let Some(v) = c.bt_listen_port {
                        cfg.bt.listen_port = v;
                    }
                    if let Some(v) = c.bt_max_connections {
                        cfg.bt.max_connections = v;
                    }
                }
                applied.push("connection.bt_listen_port".into());
                applied.push("connection.bt_max_connections".into());
            }
        }
        if let Some(bt) = &req.bittorrent {
            let any = bt.enable_dht.is_some()
                || bt.enable_lsd.is_some()
                || bt.enable_upnp.is_some()
                || bt.enable_pex.is_some()
                || bt.enable_utp.is_some()
                || bt.encrypt.is_some()
                || bt.extra_trackers.is_some()
                || bt.max_share_ratio.is_some()
                || bt.max_seeding_time_min.is_some()
                || bt.dht_bootstrap_nodes.is_some()
                || bt.max_upload_slots.is_some()
                || bt.seeding_idle_limit_min.is_some();
            if any {
                let patch = BtSessionPatch {
                    enable_dht: bt.enable_dht,
                    enable_lsd: bt.enable_lsd,
                    enable_upnp: bt.enable_upnp,
                    enable_pex: bt.enable_pex,
                    enable_utp: bt.enable_utp,
                    encrypt: bt.encrypt.clone(),
                    listen_port: None,
                    max_connections: None,
                    extra_trackers: bt.extra_trackers.clone(),
                    max_share_ratio: bt.max_share_ratio,
                    max_seeding_time_min: bt.max_seeding_time_min,
                    dht_bootstrap_nodes: bt.dht_bootstrap_nodes.clone(),
                    max_upload_slots: bt.max_upload_slots,
                    seeding_idle_limit_min: bt.seeding_idle_limit_min,
                };
                self.dispatch_bt_patch(&patch).await;
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    if let Some(v) = bt.enable_dht {
                        cfg.bt.enable_dht = v;
                    }
                    if let Some(v) = bt.enable_lsd {
                        cfg.bt.enable_lsd = v;
                    }
                    if let Some(v) = bt.enable_upnp {
                        cfg.bt.enable_upnp = v;
                    }
                    if let Some(v) = bt.enable_pex {
                        cfg.bt.enable_pex = v;
                    }
                    if let Some(v) = bt.enable_utp {
                        cfg.bt.enable_utp = v;
                    }
                    if let Some(v) = bt.encrypt.as_deref() {
                        cfg.bt.encrypt = v.trim().to_string();
                    }
                    if let Some(v) = &bt.extra_trackers {
                        cfg.bt.extra_trackers = v
                            .iter()
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                    }
                    if let Some(v) = bt.max_share_ratio {
                        cfg.bt.max_share_ratio = v;
                    }
                    if let Some(v) = bt.max_seeding_time_min {
                        cfg.bt.max_seeding_time_min = v;
                    }
                    if let Some(v) = &bt.dht_bootstrap_nodes {
                        cfg.bt.dht_bootstrap_nodes = v
                            .iter()
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                    }
                    if let Some(v) = bt.max_upload_slots {
                        cfg.bt.max_upload_slots = v;
                    }
                    if let Some(v) = bt.seeding_idle_limit_min {
                        cfg.bt.seeding_idle_limit_min = v;
                    }
                }
                for k in [
                    "enable_dht",
                    "enable_lsd",
                    "enable_upnp",
                    "enable_pex",
                    "enable_utp",
                    "encrypt",
                    "extra_trackers",
                    "max_share_ratio",
                    "max_seeding_time_min",
                    "dht_bootstrap_nodes",
                    "max_upload_slots",
                    "seeding_idle_limit_min",
                ] {
                    applied.push(format!("bittorrent.{k}"));
                }
            }
            // batch5：存储模式——会话级开关，重启装配时注入引擎（PUT 仅持久化）
            if let Some(v) = bt.storage_allocate {
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    cfg.bt.storage_allocate = v;
                }
                restart_required.push("bittorrent.storage_allocate".into());
            }
            // B1（#214）：引擎总开关——装配期生效（运行中引擎不摘除：摘除会
            // 令在传 BT 任务句柄悬空）；PUT 仅持久化。
            if let Some(v) = bt.enabled {
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    cfg.bt.enabled = v;
                }
                restart_required.push("bittorrent.enabled".into());
            }
            // B1（#214）：BT 独立落盘目录——空串 = 清除（回落 dest_root）；
            // 引擎构造期读，PUT 仅持久化。
            if let Some(v) = bt.save_path.as_deref() {
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    cfg.bt.save_path = (!v.trim().is_empty()).then(|| PathBuf::from(v.trim()));
                }
                restart_required.push("bittorrent.save_path".into());
            }
        }
        if let Some(d) = &req.download {
            if let Some(root) = d.dest_root.as_deref() {
                let path = PathBuf::from(root.trim());
                std::fs::create_dir_all(&path).map_err(|e| {
                    DaemonError::InvalidSource(format!(
                        "download.dest_root = {root:?} 创建失败: {e}"
                    ))
                })?;
                *self.default_dest_root.lock() = path.clone();
                let mut roots = self.allowed_roots.lock();
                if !roots.is_empty() && !roots.contains(&path) {
                    roots.push(path.clone());
                }
                drop(roots);
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    cfg.download.dest_root = path;
                }
                applied.push("download.dest_root".into());
            }
            if let Some(v) = d.disk_precheck_strict {
                // 运行时 bool 字段不热切（预检语义中途抖动影响入队判定一致性）——
                // 仅落盘 + 重启生效
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    cfg.download.disk_precheck_strict = v;
                }
                restart_required.push("download.disk_precheck_strict".into());
            }
            if let Some(v) = d.max_redirects {
                // 重定向策略烘入全局 client（reqwest Client 不可变）——仅落盘
                // + 重启生效；越界钳制发生在 serve 使用点（1..=100）。
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    cfg.download.max_redirects = v;
                }
                restart_required.push("download.max_redirects".into());
            }
            // B1（#214）：fsync 三件套——烘入 HTTP 引擎构造（每任务段写策略），
            // PUT 仅持久化（disk_precheck_strict 先例：中途热切会令同任务
            // 段级 WAL 粒度不一致）。restart_required 只报实际提交的键
            // （e2e HC-9 抓出的契约缺陷：整组硬推三键会虚报「重启生效 3 项」）。
            if let Some(cfg) = self.live_config.lock().as_mut() {
                if let Some(v) = d.fsync_per_segment {
                    cfg.download.fsync_per_segment = v;
                    restart_required.push("download.fsync_per_segment".into());
                }
                if let Some(v) = d.fsync_batch_segments {
                    cfg.download.fsync_batch_segments = v;
                    restart_required.push("download.fsync_batch_segments".into());
                }
                if let Some(v) = d.fsync_batch_interval_ms {
                    cfg.download.fsync_batch_interval_ms = v;
                    restart_required.push("download.fsync_batch_interval_ms".into());
                }
            }
            // B2（#214）：HTTP 调谐域——引擎级热改（新任务生效）+ 权威配置
            // 补丁。整组快照式下发（引擎置换 tuning，非逐字段补丁）：任一
            // 键提交即以合并后的全量 tuning 下发，applied 只报实际提交键。
            let b2_touched = d.user_agent.is_some()
                || d.no_proxy.is_some()
                || d.connect_timeout_secs.is_some()
                || d.read_timeout_secs.is_some()
                || d.max_connections_per_task.is_some()
                || d.min_split_mb.is_some();
            if b2_touched {
                {
                    let mut cfg_guard = self.live_config.lock();
                    if let Some(cfg) = cfg_guard.as_mut() {
                        if let Some(v) = d.user_agent.as_deref() {
                            cfg.download.user_agent = v.trim().to_string();
                        }
                        if let Some(v) = &d.no_proxy {
                            cfg.download.no_proxy = v
                                .iter()
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect();
                        }
                        if let Some(v) = d.connect_timeout_secs {
                            cfg.download.connect_timeout_secs = v;
                        }
                        if let Some(v) = d.read_timeout_secs {
                            cfg.download.read_timeout_secs = v;
                        }
                        if let Some(v) = d.max_connections_per_task {
                            cfg.download.max_connections_per_task = v;
                        }
                        if let Some(v) = d.min_split_mb {
                            cfg.download.min_split_mb = v;
                        }
                    }
                }
                // 引擎热改（失败仅告警不回滚：新任务沿用旧 tuning，快照与
                // 引擎短暂不一致由下次 PUT / 重启收敛——与 dispatch_bt_patch
                // 同口径）。
                let tuning = self
                    .live_config
                    .lock()
                    .as_ref()
                    .map(|c| c.download.http_tuning())
                    .unwrap_or_default();
                self.dispatch_http_tuning(&tuning).await;
                // 只报实际提交键（fsync 虚报缺陷同教训：整组硬推会虚报计数）
                if d.user_agent.is_some() {
                    applied.push("download.user_agent".into());
                }
                if d.no_proxy.is_some() {
                    applied.push("download.no_proxy".into());
                }
                if d.connect_timeout_secs.is_some() {
                    applied.push("download.connect_timeout_secs".into());
                }
                if d.read_timeout_secs.is_some() {
                    applied.push("download.read_timeout_secs".into());
                }
                if d.max_connections_per_task.is_some() {
                    applied.push("download.max_connections_per_task".into());
                }
                if d.min_split_mb.is_some() {
                    applied.push("download.min_split_mb".into());
                }
            }
            // B2（#214）：全局重试默认——add 时现读 live_config（立即生效），
            // 此处仅补丁权威配置。
            if let Some(v) = d.retry_max_attempts {
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    cfg.download.retry_max_attempts = v;
                }
                applied.push("download.retry_max_attempts".into());
            }
        }
        // B1（#214）：rss 域。max_processed_items_per_feed 为唯一运行时生效
        // 键（刷新路径 rss_max_processed_items 经 live_config 现读，无中间
        // 缓存 → 打补丁即热切）；auto_refresh / refresh_interval_secs 由
        // serve 启动期 ticker 固化，PUT 仅持久化。
        if let Some(r) = &req.rss {
            if r.auto_refresh.is_some()
                || r.refresh_interval_secs.is_some()
                || r.max_processed_items_per_feed.is_some()
            {
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    if let Some(v) = r.auto_refresh {
                        cfg.rss.auto_refresh = v;
                    }
                    if let Some(v) = r.refresh_interval_secs {
                        cfg.rss.refresh_interval_secs = v;
                    }
                    if let Some(v) = r.max_processed_items_per_feed {
                        cfg.rss.max_processed_items_per_feed = v;
                        applied.push("rss.max_processed_items_per_feed".into());
                    }
                }
                if r.auto_refresh.is_some() {
                    restart_required.push("rss.auto_refresh".into());
                }
                if r.refresh_interval_secs.is_some() {
                    restart_required.push("rss.refresh_interval_secs".into());
                }
            }
        }
        if let Some(c) = &req.cleanup {
            let mut cur = self.cleanup.lock().clone();
            if let Some(v) = c.auto_remove_completed_days {
                cur.auto_remove_completed_days = v;
            }
            if let Some(v) = c.auto_remove_keep_data {
                cur.auto_remove_keep_data = v;
            }
            *self.cleanup.lock() = cur.clone();
            if let Some(cfg) = self.live_config.lock().as_mut() {
                cfg.cleanup = cur;
            }
            applied.push("cleanup.auto_remove_completed_days".into());
            applied.push("cleanup.auto_remove_keep_data".into());
        }
        if let Some(p) = &req.post_download {
            if let Some(v) = p.move_to.as_deref() {
                *self.post_move_to.lock() =
                    (!v.is_empty()).then(|| v.to_string()).map(PathBuf::from);
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    cfg.post_download.move_to = v.to_string();
                }
                applied.push("post_download.move_to".into());
            }
            if let Some(v) = p.hook.as_deref() {
                *self.post_hook.lock() = (!v.is_empty()).then(|| v.to_string());
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    cfg.post_download.hook = v.to_string();
                }
                applied.push("post_download.hook".into());
            }
        }
        if let Some(w) = &req.webhook {
            if let Some(v) = w.url.as_deref() {
                *self.webhook_url.lock() = (!v.is_empty()).then(|| v.to_string());
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    cfg.webhook.url = v.to_string();
                }
                applied.push("webhook.url".into());
            }
        }
        if let Some(s) = &req.scheduler {
            if let Some(v) = s.start_jitter_seconds {
                self.start_jitter_secs
                    .store(v, std::sync::atomic::Ordering::Relaxed);
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    cfg.scheduler.start_jitter_seconds = v;
                }
                applied.push("scheduler.start_jitter_seconds".into());
            }
            // Task 46：完成动作热更（归一化 + 回写 live_config）
            if let Some(v) = &s.completion_action {
                let norm = super::lifecycle::normalize_completion_action(v);
                self.set_completion_action(&norm);
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    cfg.scheduler.completion_action = norm.clone();
                }
                applied.push(format!("scheduler.completion_action={norm}"));
            }
        }
        if let Some(q) = &req.queue {
            let mut cur = self.queue_cfg.lock().clone();
            if let Some(v) = q.max_active_bt {
                cur.max_active_bt = v;
            }
            if let Some(v) = q.max_active_http {
                cur.max_active_http = v;
            }
            if let Some(v) = q.max_active_ftp {
                cur.max_active_ftp = v;
            }
            // B6（#214 §三-10）：总量闸热更——门控/递补均现读 live
            // queue_cfg，PUT 后下一个 add/tick 即受新闸约束；已在传
            // 任务不回退（收紧闸门只影响新激活，与分引擎配额同口径）。
            if let Some(v) = q.max_active_total {
                cur.max_active_total = v;
            }
            *self.queue_cfg.lock() = cur.clone();
            if let Some(cfg) = self.live_config.lock().as_mut() {
                cfg.queue = cur;
            }
            for k in [
                "max_active_bt",
                "max_active_http",
                "max_active_ftp",
                "max_active_total",
            ] {
                applied.push(format!("queue.{k}"));
            }
        }
        // B4（#214 §三-13）：日志级别——热改（tracing reload）+ 权威配置补丁。
        // 订阅者未装配（库嵌入/测试进程）→ restart_required（诚实语义，
        // #160 同族预防：「设置了看似生效实则没有」的假阳性禁止）。
        if let Some(l) = &req.logging {
            if let Some(v) = l.level.as_deref() {
                // 验证阶段已过白名单；此处 unwrap 不可达（防御式二次归一）。
                let norm = crate::logging::normalize_level(v).unwrap_or("info");
                if let Some(cfg) = self.live_config.lock().as_mut() {
                    cfg.logging.level = norm.to_string();
                }
                if crate::logging::try_set_level(norm) {
                    applied.push("logging.level".into());
                } else {
                    restart_required.push("logging.level".into());
                }
            }
        }

        // 备用窗口重评估（备用字段变更或基准变更经 apply_global_limits 已处理；
        // 这里兜底 alt 字段单独更新时立即切换引擎实际值）
        if limits_touched {
            self.tick_alt_limits().await?;
        }

        // ---- 第三阶段：持久化 + 快照刷新 + 事件 ----
        let persist = req.persist.unwrap_or(persist_default);
        let mut persisted = false;
        if persist {
            let path = self.config_path.lock().clone();
            if let Some(p) = path {
                let cfg = self.live_config.lock().clone().unwrap_or_default();
                cfg.save_to(&p).map_err(DaemonError::Engine)?;
                persisted = true;
            }
        }
        // /config 快照重建（权威配置已打补丁）+ 限速两键覆盖
        {
            let tasks_path = self
                .persist_path
                .clone()
                .unwrap_or_else(|| PathBuf::from("./tasks.json"));
            let cfg = self.live_config.lock().clone().unwrap_or_default();
            let snap = crate::config::Config::snapshot_json(&cfg, &tasks_path);
            *self.config_snapshot.lock() = Some(snap);
            self.overlay_config_limits(*self.global_limits.lock());
        }
        let mut keys = applied.clone();
        keys.extend(restart_required.iter().cloned());
        if !keys.is_empty() {
            self.hub.publish(SchedulerEvent::SettingsChanged { keys });
        }
        Ok(SettingsApplyReport {
            applied,
            restart_required,
            persisted,
            limits: *self.global_limits.lock(),
        })
    }

    /// 导出权威配置全量 TOML（B5，#214 §五-4，`GET /settings/export`）。
    /// live_config 缺席（无 --config 且从未 PUT）→ 默认值导出（幂等兜底，
    /// 与快照/live_config 消费点的 unwrap_or_default 同口径）。
    pub fn export_config_toml(&self) -> Result<String, DaemonError> {
        let cfg = self.live_config.lock().clone().unwrap_or_default();
        cfg.to_toml_string().map_err(DaemonError::Engine)
    }

    /// 导入配置（B5，#214 §五-4，`POST /settings/import`）：
    ///
    /// 语义 = **文件热重载与 `PUT /settings` 各域的并集**——
    /// 1. 解析 + `validate()`（与启动/热重载同口径，batch10-F）→ 失败
    ///    400 零副作用；
    /// 2. `refresh_config` 全量换权威配置（dest_root/alt/queue/webhook/
    ///    cleanup/jitter/completion/日志级别热跟随）；
    /// 3. 限速两键经 `apply_global_limits`（E16 文件为准路径）+ 备用窗口
    ///    即时重评估；
    /// 4. BT 会话 / HTTP 调谐 / 全局代理逐引擎下发（能力门控 + warn 降级，
    ///    与 PUT 同语义）；
    /// 5. 原子落盘（tmp+rename+fsync+0600）。
    ///
    /// 报告：字段级 diff（新旧两份 Config 的 TOML 树递归比对）→ 热键入
    /// `applied`、冷键入 `restart_required`（见 [`is_restart_scoped`]）。
    /// `settings_changed` 事件携带 diff 全集。
    pub async fn import_settings(&self, text: &str) -> Result<SettingsApplyReport, DaemonError> {
        // ---- 第一阶段：解析 + 校验（零副作用）----
        let mut new_cfg: crate::config::Config = toml::from_str(text)
            .map_err(|e| DaemonError::InvalidSource(format!("导入配置解析失败: {e}")))?;
        new_cfg
            .validate()
            .map_err(|e| DaemonError::InvalidSource(format!("导入配置校验失败: {e}")))?;
        // 日志级别归一（六值小写；非法值与加载期同口径回落 info——备份
        // 文件可能来自旧版本，导入路径不可比 PUT 更严）。
        new_cfg.logging.level = new_cfg.logging.normalized_level();
        // 导入必须落盘——无 --config 的运行时态没有恢复目标，明确拒绝
        //（PUT 语义「仅运行时生效」对整文件导入无意义：丢了源文件即丢配置）。
        let cfg_path = self.config_path.lock().clone().ok_or_else(|| {
            DaemonError::InvalidSource("导入需要持久化目标（未指定 --config）".into())
        })?;
        let old_cfg = self.live_config.lock().clone().unwrap_or_default();
        let diff = diff_config_keys(&old_cfg, &new_cfg);

        // ---- 第二阶段：应用（文件热重载链路 + PUT 引擎下发并集）----
        let tasks_path = self
            .persist_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("./tasks.json"));
        self.refresh_config(&new_cfg, &tasks_path);
        // 限速（E16）：apply_global_limits 设基准 + tick 重评估备用窗口
        //（与 PUT bandwidth 域同序）。
        self.apply_global_limits(
            Some(new_cfg.download.max_download_kb_s),
            Some(new_cfg.bt.max_upload_kb_s),
        )
        .await?;
        self.tick_alt_limits().await?;
        // BT 会话全量补丁（能力门控：引擎缺失/Unsupported 静默；warn 降级）。
        let patch = BtSessionPatch {
            enable_dht: Some(new_cfg.bt.enable_dht),
            enable_lsd: Some(new_cfg.bt.enable_lsd),
            enable_upnp: Some(new_cfg.bt.enable_upnp),
            enable_pex: Some(new_cfg.bt.enable_pex),
            enable_utp: Some(new_cfg.bt.enable_utp),
            encrypt: Some(new_cfg.bt.encrypt.clone()),
            listen_port: Some(new_cfg.bt.listen_port),
            max_connections: Some(new_cfg.bt.max_connections),
            extra_trackers: Some(new_cfg.bt.extra_trackers.clone()),
            max_share_ratio: Some(new_cfg.bt.max_share_ratio),
            max_seeding_time_min: Some(new_cfg.bt.max_seeding_time_min),
            dht_bootstrap_nodes: Some(new_cfg.bt.dht_bootstrap_nodes.clone()),
            max_upload_slots: Some(new_cfg.bt.max_upload_slots),
            seeding_idle_limit_min: Some(new_cfg.bt.seeding_idle_limit_min),
        };
        self.dispatch_bt_patch(&patch).await;
        // HTTP 调谐 + 全局代理（新任务生效语义，与 PUT download/connection 同）。
        let tuning = new_cfg.download.http_tuning();
        self.dispatch_http_tuning(&tuning).await;
        let proxy = (!new_cfg.download.proxy.trim().is_empty())
            .then(|| new_cfg.download.proxy.trim().to_string());
        self.dispatch_global_proxy(proxy.as_deref()).await?;

        // ---- 第三阶段：落盘 + 报告 + 事件 ----
        new_cfg.save_to(&cfg_path).map_err(DaemonError::Engine)?;
        let applied: Vec<String> = diff
            .iter()
            .filter(|k| !is_restart_scoped(k))
            .cloned()
            .collect();
        let restart_required: Vec<String> = diff
            .iter()
            .filter(|k| is_restart_scoped(k))
            .cloned()
            .collect();
        if !diff.is_empty() {
            self.hub
                .publish(SchedulerEvent::SettingsChanged { keys: diff });
        }
        tracing::info!(
            "配置导入: 应用 {} 项 / 重启生效 {} 项 / 落盘 {}",
            applied.len(),
            restart_required.len(),
            cfg_path.display()
        );
        Ok(SettingsApplyReport {
            applied,
            restart_required,
            persisted: true,
            limits: *self.global_limits.lock(),
        })
    }

    /// 全局代理热改（S1）：逐引擎 `set_global_proxy`（`Unsupported` 静默跳过）。
    /// BT = settings_pack 立即重放；HTTP = 新任务生效（逐任务 client 构建时
    /// 合并运行时全局代理）；None = 清除代理（直连）。
    pub(super) async fn dispatch_global_proxy(
        &self,
        proxy: Option<&str>,
    ) -> Result<(), DaemonError> {
        for (kind, eng) in self.engines.iter() {
            match eng.set_global_proxy(proxy).await {
                Ok(()) => {}
                Err(EngineError::Unsupported) => {}
                Err(e) => {
                    return Err(DaemonError::Engine(format!(
                        "{kind:?} 全局代理下发失败: {e}"
                    )))
                }
            }
        }
        Ok(())
    }

    /// BT 会话补丁下发（发现/传输/连接/加密）：引擎缺失或 `Unsupported`（非
    /// BT 引擎）静默跳过；其余错误降级为 warn（会话级设置尽力而为，不阻塞
    /// 设置保存——与限速阀门 fail-fast 语义区分）。
    pub(super) async fn dispatch_bt_patch(&self, patch: &BtSessionPatch) {
        // 能力询问（TD-2 Step 3）：注册表借用 + bt() 一步到位——引擎缺失
        // 或无 BT 面（Unsupported 旧口径）均静默跳过。
        if let Some(bt) = self.engines.get(&EngineKind::Bt).and_then(|e| e.bt()) {
            match bt.apply_bt_session(patch.clone()).await {
                Ok(()) => {}
                Err(EngineError::Unsupported) => {}
                Err(e) => tracing::warn!("BT 会话设置下发失败（保留旧值）: {e}"),
            }
        }
    }

    /// HTTP 调谐下发（B2，#214）：能力询问（TD-2 http() 一步到位）——引擎
    /// 缺失或无 HTTP 面均静默跳过；Unsupported 静默（非 HTTP 引擎默认实现）。
    pub(super) async fn dispatch_http_tuning(&self, tuning: &smart_dl_core::types::HttpTuning) {
        if let Some(http) = self.engines.get(&EngineKind::Http).and_then(|e| e.http()) {
            match http.set_tuning(tuning.clone()).await {
                Ok(()) => {}
                Err(EngineError::Unsupported) => {}
                Err(e) => tracing::warn!("HTTP 调谐下发失败（保留旧值）: {e}"),
            }
        }
    }

    /// 备用限速窗口评估（S1，30s ticker + 设置变更即时调用）：
    /// 目标值 = 窗口命中 ? 备用值 : 基准值；与引擎实际值不一致时下发 + 事件。
    /// 幂等：无差异时零副作用。
    pub async fn tick_alt_limits(&self) -> Result<(), DaemonError> {
        let cfg = self.alt_cfg.lock().clone();
        let base = *self.base_limits.lock();
        let active = cfg.alt_enabled && alt_window_active(&cfg, &chrono::Local::now());
        self.alt_active
            .store(active, std::sync::atomic::Ordering::Relaxed);
        let target = if active {
            GlobalLimits {
                max_download_kb_s: cfg.alt_max_download_kb_s,
                max_upload_kb_s: cfg.alt_max_upload_kb_s,
            }
        } else {
            base
        };
        if *self.global_limits.lock() == target {
            return Ok(());
        }
        // 引擎下发：BT → FTP/HTTP（与 apply_global_limits 同序同语义）
        if let Some(bt) = self.engines.get(&EngineKind::Bt).cloned() {
            Self::dispatch_global_limits(
                bt.as_ref(),
                Some(target.max_download_kb_s),
                Some(target.max_upload_kb_s),
                "BT",
            )
            .await?;
        }
        for kind in [EngineKind::Ftp, EngineKind::Http, EngineKind::Sftp] {
            if let Some(eng) = self.engines.get(&kind).cloned() {
                Self::dispatch_global_limits(
                    eng.as_ref(),
                    Some(target.max_download_kb_s),
                    None,
                    &format!("{kind:?}"),
                )
                .await?;
            }
        }
        *self.global_limits.lock() = target;
        self.overlay_config_limits(target);
        self.hub.publish(SchedulerEvent::GlobalLimitsChanged {
            max_download_kb_s: target.max_download_kb_s,
            max_upload_kb_s: target.max_upload_kb_s,
        });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 纯函数助手（单测直打）
// ---------------------------------------------------------------------------

/// 解析 `HH:MM` 为当日分钟数（None = 非法格式）。
pub(crate) fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    if h <= 23 && m <= 59 {
        Some(h * 60 + m)
    } else {
        None
    }
}

/// 备用限速窗口判定：`alt_enabled` 由调用方检查，这里只判时间/星期。
/// 窗口 `[from, to)` 支持跨零点回卷（from > to）；`from == to` 视为空窗口；
/// 星期未命中或格式非法 → false（安全侧：不切备用值）。
pub(crate) fn alt_window_active(
    cfg: &crate::config::LimitsCfg,
    now: &chrono::DateTime<chrono::Local>,
) -> bool {
    use chrono::{Datelike, Timelike};
    let (Some(from), Some(to)) = (parse_hhmm(&cfg.alt_from), parse_hhmm(&cfg.alt_to)) else {
        return false;
    };
    let weekday = now.weekday().num_days_from_sunday() as u8;
    if !cfg.alt_days.is_empty() && !cfg.alt_days.contains(&weekday) {
        return false;
    }
    let now_min = now.hour() * 60 + now.minute();
    if from == to {
        return false;
    }
    if from < to {
        (from..to).contains(&now_min)
    } else {
        now_min >= from || now_min < to
    }
}

/// 全局代理 URL 验证（S1）：scheme 白名单 + 非空主机结构（BT 内核
/// parse_proxy 同口径：http/https/socks5/socks4；空 = 清除恒合法）。
/// 不构造 reqwest::Proxy（避免 socks feature 耦合——引擎侧按各自能力解析）。
fn validate_proxy_url(url: &str) -> Result<(), DaemonError> {
    let t = url.trim();
    if t.is_empty() {
        return Ok(());
    }
    let lower = t.to_ascii_lowercase();
    let rest = ["http://", "https://", "socks5://", "socks4://"]
        .iter()
        .find_map(|p| lower.strip_prefix(p))
        .ok_or_else(|| {
            DaemonError::InvalidSource(format!(
                "connection.proxy = {url:?} 无效：仅支持 http/https/socks5/socks4 前缀"
            ))
        })?;
    let host = rest.split(['/', '?']).next().unwrap_or("");
    // host[:port] 至少要有一个非空主机段（凭据前缀 @ 后取主机）
    let host = host.rsplit('@').next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    if host.trim().is_empty() {
        return Err(DaemonError::InvalidSource(format!(
            "connection.proxy = {url:?} 无效：缺少主机段"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// B5（#214 §五-4）：导入 diff 工具（纯函数，单测直打）
// ---------------------------------------------------------------------------

/// 新旧配置字段级 diff：两份 Config 序列化为 TOML 树后递归比对，输出
/// 变更叶节点点分路径（如 `bt.max_upload_slots`；单侧新增/删除同样计变更）。
/// 序列化失败（含非 TOML 可表达字段）→ 空表（导入报告降级为不分类，
/// 应用语义不受影响——引擎下发与落盘不依赖本表）。
fn diff_config_keys(old: &crate::config::Config, new: &crate::config::Config) -> Vec<String> {
    let (Ok(a), Ok(b)) = (toml::Value::try_from(old), toml::Value::try_from(new)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut path = String::new();
    walk_toml_diff(&a, &b, &mut path, &mut out);
    out.sort();
    out
}

/// TOML 树递归 diff（叶子值不等或单侧缺失 → 记路径）。
fn walk_toml_diff(a: &toml::Value, b: &toml::Value, path: &mut String, out: &mut Vec<String>) {
    if let (toml::Value::Table(ta), toml::Value::Table(tb)) = (a, b) {
        let mut keys: Vec<&String> = ta.keys().chain(tb.keys()).collect();
        keys.sort();
        keys.dedup();
        for k in keys {
            let len = path.len();
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(k);
            match (ta.get(k), tb.get(k)) {
                (Some(va), Some(vb)) => walk_toml_diff(va, vb, path, out),
                _ => out.push(path.clone()),
            }
            path.truncate(len);
        }
    } else if a != b {
        out.push(path.clone());
    }
}

/// 导入 diff 键的冷/热分类：整域冷（装配期/启动期固化）+ 点名冷键，
/// 其余视为导入链路可热达（refresh_config + 引擎下发 + add 时现读）。
fn is_restart_scoped(key: &str) -> bool {
    /// 整域重启生效：服务端装配（addr/token/CORS）、云盘 Provider/SDK
    /// 装配、daemon 自身路径（单实例锁 / 任务持久化文件）。
    const FULL_DOMAINS: &[&str] = &[
        "server",
        "xunlei",
        "provider",
        "provider_xunlei",
        "provider_quark",
        "lock",
        "storage",
    ];
    /// 点名冷键：引擎开关/目录（装配期）、fsync 烘入（PR #128 语义）、
    /// 重定向烘入全局 client（reqwest Client 不可变）、预检一致性、
    /// RSS ticker 启动期固化、BT 预分配会话级开关。
    const KEYED: &[&str] = &[
        "bt.enabled",
        "bt.save_path",
        "bt.storage_allocate",
        "download.disk_precheck_strict",
        "download.max_redirects",
        "download.fsync_per_segment",
        "download.fsync_batch_segments",
        "download.fsync_batch_interval_ms",
        "rss.auto_refresh",
        "rss.refresh_interval_secs",
    ];
    let head = key.split('.').next().unwrap_or("");
    FULL_DOMAINS.contains(&head) || KEYED.contains(&key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hhmm_accepts_valid_and_rejects_invalid() {
        assert_eq!(parse_hhmm("00:00"), Some(0));
        assert_eq!(parse_hhmm("08:30"), Some(8 * 60 + 30));
        assert_eq!(parse_hhmm("23:59"), Some(23 * 60 + 59));
        assert_eq!(parse_hhmm(" 7:05 "), Some(7 * 60 + 5));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("12:60"), None);
        assert_eq!(parse_hhmm("8"), None);
        assert_eq!(parse_hhmm("ab:cd"), None);
        assert_eq!(parse_hhmm(""), None);
    }

    fn cfg_from(from: &str, to: &str, days: &[u8]) -> crate::config::LimitsCfg {
        crate::config::LimitsCfg {
            alt_enabled: true,
            alt_max_download_kb_s: 0,
            alt_max_upload_kb_s: 0,
            alt_from: from.into(),
            alt_to: to.into(),
            alt_days: days.to_vec(),
        }
    }

    #[test]
    fn alt_window_normal_range_inclusive_start_exclusive_end() {
        use chrono::TimeZone;
        let cfg = cfg_from("08:00", "23:00", &[]);
        let inside = chrono::Local.with_ymd_and_hms(2026, 9, 6, 8, 0, 0).unwrap();
        let inside2 = chrono::Local
            .with_ymd_and_hms(2026, 9, 6, 12, 0, 0)
            .unwrap();
        let before = chrono::Local
            .with_ymd_and_hms(2026, 9, 6, 7, 59, 0)
            .unwrap();
        let at_end = chrono::Local
            .with_ymd_and_hms(2026, 9, 6, 23, 0, 0)
            .unwrap();
        assert!(alt_window_active(&cfg, &inside));
        assert!(alt_window_active(&cfg, &inside2));
        assert!(!alt_window_active(&cfg, &before));
        assert!(!alt_window_active(&cfg, &at_end), "[from, to) 半开区间");
    }

    #[test]
    fn alt_window_wraps_midnight() {
        use chrono::TimeZone;
        let cfg = cfg_from("22:00", "06:00", &[]);
        let late = chrono::Local
            .with_ymd_and_hms(2026, 9, 6, 23, 30, 0)
            .unwrap();
        let early = chrono::Local
            .with_ymd_and_hms(2026, 9, 7, 5, 59, 0)
            .unwrap();
        let noon = chrono::Local
            .with_ymd_and_hms(2026, 9, 6, 12, 0, 0)
            .unwrap();
        assert!(alt_window_active(&cfg, &late));
        assert!(alt_window_active(&cfg, &early));
        assert!(!alt_window_active(&cfg, &noon));
    }

    #[test]
    fn alt_window_filters_by_weekday_and_rejects_invalid_format() {
        use chrono::{Datelike, TimeZone};
        // 2026-09-06 是周日（weekday 0）
        let t = chrono::Local
            .with_ymd_and_hms(2026, 9, 6, 12, 0, 0)
            .unwrap();
        assert_eq!(t.weekday().num_days_from_sunday(), 0);
        let sunday_only = cfg_from("00:00", "23:59", &[0]);
        let weekdays_only = cfg_from("00:00", "23:59", &[1, 2, 3, 4, 5]);
        assert!(alt_window_active(&sunday_only, &t));
        assert!(!alt_window_active(&weekdays_only, &t));
        // 非法 HH:MM → 永不在窗口（安全侧）
        let bad = cfg_from("xx", "23:59", &[]);
        assert!(!alt_window_active(&bad, &t));
        // from == to → 空窗口
        let degenerate = cfg_from("08:00", "08:00", &[]);
        assert!(!alt_window_active(&degenerate, &t));
    }

    /// B5（#214 §五-4）：TOML 树 diff——单字段变更 / 多域变更 / 相等空表。
    #[test]
    fn diff_config_keys_reports_only_changed_leaves() {
        let a = crate::config::Config::default();
        let mut b = a.clone();
        b.logging.level = "debug".into();
        b.bt.max_upload_slots = 16;
        assert_eq!(
            diff_config_keys(&a, &b),
            vec![
                "bt.max_upload_slots".to_string(),
                "logging.level".to_string()
            ]
        );
        // 相等 → 空表
        assert!(diff_config_keys(&a, &a.clone()).is_empty());
        // 深层嵌套（Vec 字段）变更
        let mut c = a.clone();
        c.bt.extra_trackers = vec!["http://t.example/announce".into()];
        assert_eq!(
            diff_config_keys(&a, &c),
            vec!["bt.extra_trackers".to_string()]
        );
    }

    /// B5：冷/热分类——整域冷 + 点名冷键 + 热键白名单外默认热。
    #[test]
    fn is_restart_scoped_classifies_domains_and_keys() {
        for k in [
            "server.addr",
            "server.http_token",
            "xunlei.enabled",
            "provider.mock",
            "lock.path",
            "storage.tasks_path",
            "bt.enabled",
            "bt.save_path",
            "bt.storage_allocate",
            "download.fsync_per_segment",
            "download.max_redirects",
            "rss.auto_refresh",
            "rss.refresh_interval_secs",
        ] {
            assert!(is_restart_scoped(k), "{k} 应为冷键");
        }
        for k in [
            "logging.level",
            "bt.max_upload_slots",
            "bt.enable_dht",
            "download.user_agent",
            "download.min_split_mb",
            "queue.max_active_http",
            "queue.max_active_total",
            "limits.alt_from",
            "webhook.url",
            "cleanup.auto_remove_completed_days",
            "scheduler.completion_action",
        ] {
            assert!(!is_restart_scoped(k), "{k} 应为热键");
        }
    }
}
