/// serve 配置（TOML）：HTTP 监听地址 / 默认下载目录 / BT 引擎开关 / 单实例锁路径 /
/// 云兜底 Provider / 任务持久化。
/// 文件缺失时使用默认值（Config::default）；`--config <path>` 覆盖。
/// 全量 `Serialize`（S1 设置面）：支撑 `PUT /settings?persist=true` 的原子回写
/// （`Config::save_to`）——注释不保留（TOML round-trip 丢失注释为已知边界）。
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 安全修复（P1-b）：子表手写 Default 与本处 derive Default 单源——
/// 此前 Config::default() 与子表 derive Default 双源漂移，导致
/// 「部分表 TOML」解析时其余字段退化为零值（addr=""/dest_root=""/
/// bt.enabled=false 等）。子表 Default 修正后此处 derive 即复用子表实现。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerCfg,
    pub download: DownloadCfg,
    pub bt: BtCfg,
    /// 备用限速调度（S1）：指定时段/星期内自动切换到备用上下行限速。
    pub limits: LimitsCfg,
    /// 引擎并发队列配额（S1）：各引擎同时传输任务上限。
    pub queue: QueueCfg,
    /// RSS 订阅自动下载（qbit RSS 对标）：feed 周期拉取 + 规则匹配自动建任务。
    pub rss: RssCfg,
    pub xunlei: XunleiCfg,
    pub provider: ProviderCfg,
    pub provider_xunlei: ProviderXunleiCfg,
    pub provider_quark: ProviderQuarkCfg,
    pub webhook: WebhookCfg,
    pub cleanup: CleanupCfg,
    pub post_download: PostDownloadCfg,
    pub scheduler: SchedulerCfg,
    pub lock: LockCfg,
    pub storage: StorageCfg,
}

/// 安全修复（第三轮审查 P1-b）：子表手写 Default（与 Config::default() 单源）。
/// 修复前：子表用 derive Default（addr = ""），而 Config::default() 手写
/// "127.0.0.1:8787" —— 容器级 #[serde(default)] 下，用户只写同表任一键
///（如 `[server] http_token = "x"`），其余字段退化为 derive 零值：
/// addr 变空串 → bind("") 失败 → daemon 起不来。实测复现见 repro round3。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ServerCfg {
    /// HTTP/WS 监听地址，如 `127.0.0.1:8787`。
    pub addr: String,
    /// 安全修复（V1/V13）：HTTP API Bearer token。配置后全端点（含 /ws 握手）
    /// 要求 `Authorization: Bearer <token>`；未配置时：回环监听放行（本机 CLI
    /// 兼容），**非回环监听拒绝启动**（fail-closed，防 0.0.0.0 裸奔）。
    /// 不参与热重载（避免认证态中途抖动）。敏感项：不出现在 `/config` 快照。
    pub http_token: Option<String>,
    /// tokenless 模式 Host/Origin 同源守卫的额外放行名（batch8）：反向代理/
    /// 本机域名场景（如 `myhost.local`）。仅 http_token 未配置时消费；
    /// 大小写不敏感，剥端口比对。
    #[serde(default)]
    pub extra_allowed_hosts: Vec<String>,
    /// R24 跨源 CORS 放行 Origin 清单（完整 origin 含端口，如
    /// `http://ui.lan:3000`）：静态托管 UI → 远程 daemon 场景浏览器读取
    /// 响应所需。回环 Origin（localhost/127.*/::1）始终放行（与 tokenless
    /// 守卫的回环信任同口径）；非回环远端建议配 `http_token`（tokenless
    /// 守卫对非回环 Origin 仍 403，CORS 头不放松它）。不参与热重载。
    #[serde(default)]
    pub extra_allowed_origins: Vec<String>,
}

impl Default for ServerCfg {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:8787".into(),
            http_token: None,
            extra_allowed_hosts: Vec::new(),
            extra_allowed_origins: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DownloadCfg {
    /// 默认下载落盘根目录（三 add 入口的 dest 缺省值）。
    pub dest_root: PathBuf,
    /// 全局代理（HTTP 引擎 + BT 引擎共用）：`http://host:port` / `socks5://host:port` /
    /// `socks4://host:port`（BT 支持带凭据 `user:pass@`）；空 = 直连。启动时生效
    /// （proxy 不参与热重载，避免重建连接）。敏感项：不出现在 `/config` 快照。
    pub proxy: String,
    /// 全局下载限速（KiB/s，HTTP + FTP + BT 共用总阀门）；0 = 不限。
    /// 启动时生效；运行中可经 `POST /config/limit` 热改或随配置热重载
    /// 生效（E16，文件为准）。
    pub max_download_kb_s: u32,
    /// 安全修复（V10-2）：磁盘预检严格模式——true = 磁盘可用空间不可探测时
    /// 拒绝入队（防预检被绕过后续盘写满）；false（默认）= 告警 + 放行（旧行为）。
    pub disk_precheck_strict: bool,
    /// HTTP 重定向最大跳数（batch5 对标 qB）：全局 client
    /// `redirect::Policy::limited(n)`；默认 10 = reqwest 默认值（不改变
    /// 现有行为）。合法域 1..=100（越界钳到边界）；仅启动时生效（烘入 client）。
    #[serde(default = "default_max_redirects")]
    pub max_redirects: u32,
    /// 段数据落盘策略主开关（fsync 性能开关，PR #114 审查 §3.3 跟进项）：
    /// true（默认）= 每段 fsync 后才记账（P0-4 WAL 最严粒度，SSD 推荐）；
    /// false = 批量模式——累计 `fsync_batch_segments` 段或
    /// `fsync_batch_interval_ms` 毫秒（先到者）才做一次「数据 fsync +
    /// 账本落盘」。机械盘推荐 false：段级 fsync 把吞吐支配到个位数 MB/s
    /// （审查实测 2022 MB/s → 6.4 MB/s），批量化摊销到 N 段一次。
    /// 两模式均保持 WAL 不变量（数据 fsync 先于账本，仅粒度变粗）。
    /// 仅启动时生效（烘入 HTTP 引擎）。
    #[serde(default = "default_true")]
    pub fsync_per_segment: bool,
    /// 批量模式阈值一：自上次落盘累计完成段数。默认 8（审查建议 N=8：
    /// 13ms fsync 摊到 8×16MB=128MB）。合法域 1..=1024（越界钳到边界）。
    #[serde(default = "default_fsync_batch_segments")]
    pub fsync_batch_segments: u32,
    /// 批量模式阈值二：距上次落盘时长（毫秒），任一阈值先到即触发。
    /// 默认 2000。合法域 100..=600000（越界钳到边界）。
    #[serde(default = "default_fsync_batch_interval_ms")]
    pub fsync_batch_interval_ms: u64,
}

/// 安全修复（P1-b）：与 Config::default() 单源——修复前 derive Default 令
/// dest_root = ""，只写 `[download] proxy = ...` 时落盘位置退化为相对 cwd。
impl Default for DownloadCfg {
    fn default() -> Self {
        Self {
            dest_root: PathBuf::from("./downloads"),
            proxy: String::new(),
            max_download_kb_s: 0,
            disk_precheck_strict: false,
            max_redirects: default_max_redirects(),
            fsync_per_segment: default_true(),
            fsync_batch_segments: default_fsync_batch_segments(),
            fsync_batch_interval_ms: default_fsync_batch_interval_ms(),
        }
    }
}

fn default_max_redirects() -> u32 {
    10
}

fn default_true() -> bool {
    true
}

fn default_fsync_batch_segments() -> u32 {
    8
}

fn default_fsync_batch_interval_ms() -> u64 {
    2000
}

impl DownloadCfg {
    /// `[download]` 配置段 → httpdl 段落盘策略（含越界钳制，口径对齐
    /// max_redirects 的使用点钳边界模式）。daemon 启动时烘入 HTTP 引擎。
    pub fn segment_sync_policy(&self) -> smart_dl_httpdl::download::SegmentSyncPolicy {
        smart_dl_httpdl::download::SegmentSyncPolicy {
            per_segment: self.fsync_per_segment,
            batch_segments: self.fsync_batch_segments.clamp(1, 1024),
            batch_interval: std::time::Duration::from_millis(
                self.fsync_batch_interval_ms.clamp(100, 600_000),
            ),
        }
    }
}

/// BT 引擎配置。`encrypt` 缺省值是 `allow`（内核默认行为）而非空串，
/// 故手动实现 Default + 字段级 serde 默认（derive Default 会给空串）。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct BtCfg {
    /// 启用 BT 引擎（需编译时 --features bt）。
    pub enabled: bool,
    /// BT 落盘目录（须存在；默认与 dest_root 相同）。
    pub save_path: Option<PathBuf>,
    /// BT 上传限速（KiB/s）；0 = 不限。启动时生效；运行中可经
    /// `POST /config/limit` 热改或随配置热重载生效（E16）。
    pub max_upload_kb_s: u32,
    /// 启用 DHT（去中心化 peer 发现）。默认关闭保持确定性；纯磁力无 tracker
    /// 冷启动可开启。启动时生效。
    pub enable_dht: bool,
    /// 启用 LSD（本地网络 peer 发现）。默认关闭保持确定性。启动时生效。
    pub enable_lsd: bool,
    /// 启用 UPnP/NAT-PMP 端口映射（两者同进退）。默认关闭保持确定性。启动时生效。
    pub enable_upnp: bool,
    /// 启用 PEX（peer 交换，BT peer 间互换已有 peer 列表）。默认关闭保持确定性
    /// 且对私有 tracker 更友好。启动时生效。
    pub enable_pex: bool,
    /// 启用 uTP（BT 传输 uTP/UDP 双向开关，incoming/outgoing 同进退）。默认
    /// 关闭保持确定性（v1 内核决策：uTP 首连超时会扰动 e2e 时序）。启动时生效。
    pub enable_utp: bool,
    /// MSE（Protocol Encryption）握手策略：`disable`（纯明文，拒 MSE）/
    /// `allow`（明文+加密皆收，内核默认）/ `require`（强制加密，明文直接拒）。
    /// 缺省 allow = 不改变现有行为；非法值拒绝启动。启动时生效。
    /// 合法值口径与 btcore::engine::parse_encrypt_policy 一致。
    #[serde(default = "default_bt_encrypt")]
    pub encrypt: String,
    /// BT 监听端口（S1，qbit「连接」页对齐项）：内核 `listen_interfaces`
    /// = `0.0.0.0:<port>,[::]:<port>`。0 = 不下发（用内核默认 6881 系）。
    /// 启动时生效；运行中可经 `PUT /settings` 热改（内核 re-listen）。
    pub listen_port: u16,
    /// BT 会话全局连接数上限（S1）：内核 `connections_limit`。
    /// 0 = 不下发（内核默认 200）。运行中可热改。
    pub max_connections: u32,
    /// 新建 BT 任务自动追加的 tracker 列表（qbit「自动添加以下 tracker 到
    /// 新任务」对标）：add 成功后逐条 `add_tracker`（best-effort，单条失败
    /// 不阻断建任务）。运行中可经 `PUT /settings` 热改（只影响后续新任务）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_trackers: Vec<String>,
    /// 做种分享率上限（qbit Share Ratio Limit 对标）：Seeding 态任务
    /// `uploaded/downloaded >= 阈值` 时自动暂停。0.0（默认）= 不启用；
    /// >0 = 生效阈值（上限 9999.0）。运行中可经 `PUT /settings` 热改。
    #[serde(default = "default_bt_max_share_ratio")]
    pub max_share_ratio: f64,
    /// 做种时长上限（分钟，qbit「做种时间限制」对标）：Seeding 态经过指定
    /// 分钟即自动暂停。0（默认）= 不启用；>0 = 生效阈值（上限 999999）。
    /// 口径 = 本次运行内做种时长（重启后经重新 checking 重新计时）。
    /// （u32 serde default = 0 = 不启用，无需具名缺省函数。）
    #[serde(default)]
    pub max_seeding_time_min: u32,
    /// 存储模式（batch5 对标 qB「预分配磁盘空间」）：true = 后续新增任务
    /// 预分配完整文件大小（allocate，磁盘占用即时到位、碎片更少）；
    /// false（默认）= 稀疏（sparse，按需增长）。fastresume 回灌任务保留
    /// 原模式；仅启动时生效（会话级注入）。
    #[serde(default)]
    pub storage_allocate: bool,
}

/// 备用限速调度（S1，qbit「速度」页 alternate rate limits 对齐项）：
/// `alt_enabled=true` 且当前时间落在 [`alt_from`, `alt_to`) 窗口（支持跨零点，
/// `alt_from > alt_to` 时自动回卷）且星期命中 `alt_days` 时，全局上下行
/// 限速切到备用值；窗口外回到基准值。空 `alt_days` = 每天适用。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct LimitsCfg {
    /// 备用限速调度总开关；false = 永远使用基准限速。
    pub alt_enabled: bool,
    /// 备用下行限速（KiB/s）；0 = 不限。
    pub alt_max_download_kb_s: u32,
    /// 备用上行限速（KiB/s）；0 = 不限。
    pub alt_max_upload_kb_s: u32,
    /// 窗口起点 `HH:MM`（本地时区）；非法格式视为永不在窗口。
    pub alt_from: String,
    /// 窗口终点 `HH:MM`（本地时区）。
    pub alt_to: String,
    /// 适用星期（0=周日 .. 6=周六）；空 = 每天适用。
    pub alt_days: Vec<u8>,
}

/// 引擎并发队列配额（S1）：各引擎**同时传输中**的任务数上限；超出的新任务
/// 在 daemon 层挂 Queued 等待，任一在传任务到达非传输态（完成/暂停/失败/
/// 移除）后 FIFO 递补。0 = 不限（保留旧行为）。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct QueueCfg {
    /// BT 引擎同时在传任务上限（S1-b；**0 = 不限/禁用排队**，默认值——
    /// 保持升级前行为零变化；>0 启用门控，超配额任务排队由调度循环递补）。
    pub max_active_bt: u32,
    /// HTTP 引擎同时在传任务上限（S1-b；0 = 不限）。
    pub max_active_http: u32,
    /// FTP 引擎同时在传任务上限（S1-b；0 = 不限）。
    pub max_active_ftp: u32,
}

fn default_bt_encrypt() -> String {
    "allow".to_string()
}

fn default_bt_max_share_ratio() -> f64 {
    0.0
}

/// RSS 订阅自动下载（qbit RSS 对标）。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct RssCfg {
    /// 自动刷新开关（false = 仅手动 `POST /rss/refresh`）。
    pub auto_refresh: bool,
    /// 刷新间隔（秒；ticker 下限 60 防误配风暴）。
    pub refresh_interval_secs: u64,
    /// 单订阅已处理条目保留上限（防 rss.json 无限膨胀；未处理条目不淘汰
    /// ——截掉会丢失去重标记，源刷新时重复建任务）。
    pub max_processed_items_per_feed: u32,
}

impl Default for RssCfg {
    fn default() -> Self {
        Self {
            auto_refresh: true,
            refresh_interval_secs: 900,
            max_processed_items_per_feed: 200,
        }
    }
}

impl Default for BtCfg {
    fn default() -> Self {
        BtCfg {
            // 安全修复（P1-b）：与 Config::default() 单源——修复前此处 false，
            // 只写 [bt] 任一其他键（如 max_upload_kb_s）时 enabled 退化为
            // false → BT 静默关闭。
            enabled: true,
            save_path: None,
            max_upload_kb_s: 0,
            enable_dht: false,
            enable_lsd: false,
            enable_upnp: false,
            enable_pex: false,
            enable_utp: false,
            encrypt: default_bt_encrypt(),
            listen_port: 0,
            max_connections: 0,
            extra_trackers: Vec::new(),
            max_share_ratio: default_bt_max_share_ratio(),
            max_seeding_time_min: 0,
            storage_allocate: false,
        }
    }
}

/// 迅雷 SDK 引擎配置（Windows-only）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct XunleiCfg {
    /// 启用迅雷 SDK 引擎（需编译时 --features xunlei）。
    pub enabled: bool,
    /// 迅雷 SDK 目录（包含 DownloadSDKProxy.dll 等文件）。
    pub sdk_dir: Option<PathBuf>,
    /// 落盘目录（须存在；默认与 dest_root 相同）。
    pub save_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ProviderCfg {
    /// 云兜底总开关（`POST /tasks/:id/fallback` 需要 ≥1 个可用 provider）。
    /// 默认关：不自动烧配额；显式开启才注入 provider 列表。
    pub enabled: bool,
    /// 开发/演示用 MockProvider（仅有的现成实现；真实 provider 待迅雷线落地）。
    /// 仅当 `enabled=true` 时生效。
    pub mock: bool,
}

/// 迅雷云盘 Provider（XunleiProvider）装配配置。
/// 默认关：需显式 `enabled=true` 才把 XunleiProvider 注入 provider 列表。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ProviderXunleiCfg {
    pub enabled: bool,
    /// 登录态 JSON 路径（examples 会写入；daemon 只读加载 + 续期回写）。
    /// 缺省时 daemon 用 `xunlei_auth.json`。
    pub token_path: Option<PathBuf>,
    /// 身份档位（P1-1）：`web`（默认）/ `nas`；未知档拒绝启动。
    /// 环境变量 `SMART_DL_XUNLEI_TIER` 优先于此字段（部署覆盖友好）。
    pub tier: Option<String>,
}

/// 夸克网盘 Provider（QuarkProvider）装配配置（UNWIRED #3 接线）。
/// 默认关：需显式 `enabled=true` 才把 QuarkProvider 注入 provider 列表。
/// Cookie 登录态：JSON 文件（provider 侧 load/save_auth，0600 原子写）；
/// daemon 只读加载，登录写入由示例/CLI 侧完成（无 HTTP 登录端点）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ProviderQuarkCfg {
    pub enabled: bool,
    /// Cookie 登录态 JSON 路径；缺省时 daemon 用 `quark_auth.json`。
    pub cookie_path: Option<PathBuf>,
}

/// 任务完成 Webhook 配置（E17）：任务到达完成态时 daemon 向 `url` POST 一条
/// JSON 通知（fire-and-forget，单次尝试 5s 超时，失败仅记日志）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct WebhookCfg {
    /// 完成通知 Webhook URL；空 = 禁用（默认）。参与热重载。
    pub url: String,
}

/// 已完成任务自动清理配置（E20）：按完成龄期清扫 Completed 任务。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct CleanupCfg {
    /// Completed 任务保留天数（从完成时刻起算）；0 = 禁用（默认）。
    /// 清扫间隔固定 10min；文件默认保留（keep_data）。
    pub auto_remove_completed_days: u32,
    /// 自动清理时是否保留已下载数据；true = 保留文件（默认）。
    pub auto_remove_keep_data: bool,
}

impl Default for CleanupCfg {
    fn default() -> Self {
        CleanupCfg {
            auto_remove_completed_days: 0,
            // 与文档口径单源（「文件默认保留」）：修复前此处随 derive 退化
            // 为 false——用户只写 `[cleanup] auto_remove_completed_days = 7`
            // 不显式设 keep_data 时，清扫会连已下载数据一起删，与两处
            // 字段注释（config.rs / settings.rs「默认保留」）相反。
            // 子表手写 Default 约定（P1-b 同模式：ServerCfg/RssCfg/BtCfg…）
            // 本就要求与文档声明单源，CleanupCfg 漏挂。
            auto_remove_keep_data: true,
        }
    }
}

/// 下载完成自动处理配置（E27，清单 #15）：完成后移动 + 外部钩子。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct PostDownloadCfg {
    /// 完成后把落盘文件移动到该目录（目录自动创建；同盘 rename，跨盘
    /// copy+delete 回退；同名冲突自动改名 `name(1).ext`）。空 = 禁用（默认）。
    /// batch5 起 BT 多文件目录任务也支持（目录整体移动，同名冲突改名
    /// `name(1)`）；`conflict_policy=skip` 的任务不移动（尊重"既有文件
    /// 保持原样"语义，钩子照发）。
    pub move_to: String,
    /// 完成后执行的外部程序路径（不带 shell 直启；任务上下文经环境变量
    /// 传入：SD_TASK_ID / SD_TASK_NAME / SD_FILE_PATH（移动后终路径）/
    /// SD_ENGINE）。空 = 禁用（默认）。fire-and-forget：后台线程收尾，
    /// 失败仅记日志，不反压下载主链路（钩子挂起只滞留一个后台线程）。
    /// 安全提示：程序以 daemon 同权限执行，请自行评估脚本内容。
    pub hook: String,
}

/// 调度配置（E23 定时/错峰下载）：任务定时启动与批量入队错峰。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SchedulerCfg {
    /// 错峰随机延迟上限（秒）：任务添加时未显式指定 start_at 且本值 > 0，
    /// 则在 0..=N 秒内随机延迟启动（到点前不入引擎，与显式 start_at 同
    /// 机制）。0 = 关闭（默认，立即入引擎）。参与热重载（只影响新任务）。
    pub start_jitter_seconds: u32,
    /// 全部任务终态后的动作（Task 46，qbit「下载完成后关机」对标）：
    /// none（默认）/ exit（进程退出）/ shutdown（系统关机）/ sleep（休眠）/
    /// hibernate。每进程会话最多触发一次（fired 旗标）；空任务表不触发。
    #[serde(default)]
    pub completion_action: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LockCfg {
    /// 单实例锁文件路径（重复启动 → 拒绝）。
    pub path: PathBuf,
}

/// 安全修复（P1-b）：与 Config::default() 单源（path 非空串）。
impl Default for LockCfg {
    fn default() -> Self {
        Self {
            path: PathBuf::from("./daemon.lock"),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct StorageCfg {
    /// 任务持久化文件（add/remove/状态变更自动落盘；启动时恢复）。
    pub tasks_path: PathBuf,
}

/// 安全修复（P1-b）：与 Config::default() 单源（tasks_path 非空串）。
impl Default for StorageCfg {
    fn default() -> Self {
        Self {
            tasks_path: PathBuf::from("./tasks.json"),
        }
    }
}

impl Config {
    /// 从 TOML 文件加载；文件不存在 → 默认值；解析失败 → Err（含行号）。
    pub fn load(path: Option<&std::path::Path>) -> Result<Config, String> {
        let Some(p) = path else {
            return Ok(Config::default());
        };
        let text = std::fs::read_to_string(p).map_err(|e| format!("读取配置 {p:?} 失败: {e}"))?;
        let cfg: Config = toml::from_str(&text).map_err(|e| format!("配置解析失败 {p:?}: {e}"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// 语义校验（启动与热重载同口径——审查修复：热重载原走裸 toml::from_str
    /// 绕过全部校验，`bt.max_share_ratio = -1.0` 启动被拒却热重载接受，
    /// 负阈值使分享率达标记恒真 → 任务一进 Seeding 立刻暂停）。
    pub fn validate(&self) -> Result<(), String> {
        // bt.encrypt 三态校验（fail-fast：未知值启动即报错，不等到引擎装配）。
        // 合法值口径须与 btcore::engine::parse_encrypt_policy 一致（btcore 是
        // feature 门控依赖，config 不能引用其类型，此处本地同步白名单）。
        if !matches!(self.bt.encrypt.trim(), "disable" | "allow" | "require") {
            return Err(format!(
                "配置 bt.encrypt = {:?} 无效：仅支持 disable / allow / require",
                self.bt.encrypt
            ));
        }
        // 做种分享率上限（qbit Share Ratio Limit）：负数/NaN/超上限 → 拒绝启动
        if !(0.0..=9999.0).contains(&self.bt.max_share_ratio) {
            return Err(format!(
                "配置 bt.max_share_ratio = {} 无效：须在 0.0..=9999.0（0 = 不启用）",
                self.bt.max_share_ratio
            ));
        }
        // 做种时长上限（分钟）：超上限 → 拒绝启动
        if self.bt.max_seeding_time_min > 999_999 {
            return Err(format!(
                "配置 bt.max_seeding_time_min = {} 无效：须在 0..=999999（0 = 不启用）",
                self.bt.max_seeding_time_min
            ));
        }
        Ok(())
    }

    /// 判定 addr 是否仅绑定回环地址（127.x/::1/localhost）。
    /// serve 启动检查用：非回环 + 无 http_token → 拒绝启动（V1 fail-closed）。
    /// batch8：优先 SocketAddr/IpAddr 语义判定（含 v4-mapped 归一）。
    /// 安全修复（第三轮审查 P1-a）：删除 `starts_with("127.")` 字符串前缀兜底——
    /// `127.evil.com:8787` / `127.0.0.1.nip.io:8787` 等非 IP 主机名可借前缀
    /// 通过判定，经 DNS 解析到公网 → 未鉴权绑公网（serve.rs fail-closed 闸门
    /// 被绕过）。修复后：非 IP 主机名仅字面量 localhost 放行，其余 fail closed。
    pub fn is_loopback_addr(addr: &str) -> bool {
        fn ip_loopback(ip: std::net::IpAddr) -> bool {
            // v4-mapped IPv6（::ffff:a.b.c.d）按映射后的 v4 判定
            match ip {
                std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                    Some(v4) => v4.is_loopback(),
                    None => v6.is_loopback(),
                },
                v4 => v4.is_loopback(),
            }
        }
        if let Ok(sa) = addr.parse::<std::net::SocketAddr>() {
            return ip_loopback(sa.ip());
        }
        let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            return ip_loopback(ip);
        }
        // 非 IP 主机名：仅字面量 localhost 放行。
        // `127.` 前缀不再兜底——DNS 可将任意 `127.x.y.z` 形态主机名解析到公网。
        host.eq_ignore_ascii_case("localhost")
    }

    /// 序列化为 TOML 文本（S1 设置面持久化）。无默认值裁剪——全量字段
    /// 写出，用户手改注释/字段顺序会丢（文档化边界）。
    pub fn to_toml_string(&self) -> Result<String, String> {
        toml::to_string_pretty(self).map_err(|e| format!("配置序列化失败: {e}"))
    }

    /// 原子落盘（tmp + rename，S1）：`PUT /settings?persist=true` 用。
    /// 目标目录不存在 → Err。
    /// batch3-P2：tmp 名唯一化（并发 PUT 不再互踩同一固定名）+ fsync
    ///（rename 先于数据落盘 → 断电配置损坏）+ 0600（配置含 http_token/proxy
    /// 凭据，旧实现 rename 后回落 umask 0644 泄露）。
    pub fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let text = self.to_toml_string()?;
        let tmp = path.with_extension(format!("toml.tmp.{}", SEQ.fetch_add(1, Ordering::Relaxed)));
        let mut f =
            std::fs::File::create(&tmp).map_err(|e| format!("创建临时文件 {tmp:?} 失败: {e}"))?;
        f.write_all(text.as_bytes())
            .map_err(|e| format!("写入临时文件 {tmp:?} 失败: {e}"))?;
        f.sync_all()
            .map_err(|e| format!("刷盘临时文件 {tmp:?} 失败: {e}"))?;
        drop(f);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("替换配置 {path:?} 失败: {e}")
        })
    }

    /// BT 实际落盘目录（save_path 或默认 dest_root）。
    pub fn bt_save_path(&self) -> PathBuf {
        self.bt
            .save_path
            .clone()
            .unwrap_or_else(|| self.download.dest_root.clone())
    }

    /// 迅雷实际落盘目录（save_path 或默认 dest_root）。
    pub fn xunlei_save_path(&self) -> PathBuf {
        self.xunlei
            .save_path
            .clone()
            .unwrap_or_else(|| self.download.dest_root.clone())
    }

    /// 解析迅雷身份档位名（P1-1）：env `SMART_DL_XUNLEI_TIER` > config > web。
    /// 只返回名字字符串；合法性校验归 serve（需要报错上下文）。
    pub fn resolve_xunlei_tier_name(&self) -> String {
        std::env::var("SMART_DL_XUNLEI_TIER")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| self.provider_xunlei.tier.clone())
            .unwrap_or_else(|| "web".to_string())
    }

    /// 精简配置快照（`GET /config` 返回；不含敏感项——proxy 可能带凭据故隐藏；
    /// serve 注入 + 热重载共用）。
    pub fn snapshot_json(&self, tasks_path: &std::path::Path) -> serde_json::Value {
        serde_json::json!({
            "dest_root": self.download.dest_root,
            "bt_save_path": self.bt_save_path(),
            "bt_enabled": self.bt.enabled,
            "bt_enable_dht": self.bt.enable_dht,
            "bt_enable_lsd": self.bt.enable_lsd,
            "bt_enable_upnp": self.bt.enable_upnp,
            "bt_enable_pex": self.bt.enable_pex,
            "bt_enable_utp": self.bt.enable_utp,
            "bt_encrypt": self.bt.encrypt,
            "bt_listen_port": self.bt.listen_port,
            "bt_max_connections": self.bt.max_connections,
            "bt_extra_trackers": self.bt.extra_trackers,
            "bt_max_share_ratio": self.bt.max_share_ratio,
            "bt_max_seeding_time_min": self.bt.max_seeding_time_min,
            "xunlei_enabled": self.xunlei.enabled,
            "listen_addr": self.server.addr,
            // 安全修复（V1）：仅暴露是否启用认证（布尔），token 本身绝不出快照
            "http_token_enabled": self
                .server
                .http_token
                .as_deref()
                .map(|t| !t.is_empty())
                .unwrap_or(false),
            "persist_path": tasks_path,
            "max_download_kb_s": self.download.max_download_kb_s,
            "disk_precheck_strict": self.download.disk_precheck_strict,
            "max_redirects": self.download.max_redirects,
            "fsync_per_segment": self.download.fsync_per_segment,
            "fsync_batch_segments": self.download.fsync_batch_segments,
            "fsync_batch_interval_ms": self.download.fsync_batch_interval_ms,
            "bt_storage_allocate": self.bt.storage_allocate,
            "max_upload_kb_s": self.bt.max_upload_kb_s,
            "proxy_enabled": !self.download.proxy.is_empty(),
            "provider_enabled": self.provider.enabled,
            "provider_xunlei_enabled": self.provider_xunlei.enabled,
            "provider_quark_enabled": self.provider_quark.enabled,
            // 身份档位名（非敏感；无 token 布尔那样有路径泄露风险）。
            "provider_xunlei_tier": self.resolve_xunlei_tier_name(),
            "webhook_url": self.webhook.url,
            "auto_remove_completed_days": self.cleanup.auto_remove_completed_days,
            "auto_remove_keep_data": self.cleanup.auto_remove_keep_data,
            "post_move_to": self.post_download.move_to,
            "post_hook": self.post_download.hook,
            "start_jitter_seconds": self.scheduler.start_jitter_seconds,
            // S1：备用限速调度 + 并发队列配额（设置面可见口径）
            "alt_enabled": self.limits.alt_enabled,
            "alt_max_download_kb_s": self.limits.alt_max_download_kb_s,
            "alt_max_upload_kb_s": self.limits.alt_max_upload_kb_s,
            "alt_from": self.limits.alt_from,
            "alt_to": self.limits.alt_to,
            "alt_days": self.limits.alt_days,
            "queue_max_active_bt": self.queue.max_active_bt,
            "queue_max_active_http": self.queue.max_active_http,
            "queue_max_active_ftp": self.queue.max_active_ftp,
            // 仅暴露「登录态文件是否存在」布尔，不泄露路径字符串本身。
            "provider_xunlei_token_exists": self
                .provider_xunlei
                .token_path
                .as_ref()
                .map(|p| p.exists())
                .unwrap_or(false),
            "provider_quark_cookie_exists": self
                .provider_quark
                .cookie_path
                .as_ref()
                .map(|p| p.exists())
                .unwrap_or(false),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_defaults_keep_data_per_doc() {
        // 文档两处口径（config.rs / settings.rs 字段注释）：「文件默认保留」。
        // 修复前：CleanupCfg 随 derive 退化 keep_data=false——只开
        // auto_remove_completed_days 不显式设 keep_data 的用户，清扫会
        // 连已下载数据一起删（与文档相反的数据丢失面）。
        let c = Config::default();
        assert_eq!(c.cleanup.auto_remove_completed_days, 0, "默认禁用清扫");
        assert!(
            c.cleanup.auto_remove_keep_data,
            "默认必须保留已下载数据（文档口径）"
        );
        // TOML 局部键路径：只写 days 不写 keep_data → 后者取文档默认 true
        let toml = "[cleanup]\nauto_remove_completed_days = 7\n";
        let c2: Config = toml::from_str(toml).unwrap();
        assert_eq!(c2.cleanup.auto_remove_completed_days, 7);
        assert!(
            c2.cleanup.auto_remove_keep_data,
            "局部 [cleanup] 只写 days 时 keep_data 须回退到文档默认 true"
        );
    }

    #[test]
    fn fsync_defaults_are_per_segment_wal_strict() {
        // 默认必须保持 P0-4 语义：机械盘用户显式 opt-in 才进批量模式
        let c = Config::default();
        assert!(c.download.fsync_per_segment);
        assert_eq!(c.download.fsync_batch_segments, 8);
        assert_eq!(c.download.fsync_batch_interval_ms, 2000);
        let p = c.download.segment_sync_policy();
        assert!(p.per_segment);
        assert_eq!(p.batch_segments, 8);
        assert_eq!(p.batch_interval, std::time::Duration::from_millis(2000));
    }

    #[test]
    fn fsync_batch_config_parses_and_clamps() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("fsync.toml");
        // 越界值：segments=0（非法）/ interval=10ms（低于下限）
        std::fs::write(
            &p,
            "[download]\nfsync_per_segment = false\nfsync_batch_segments = 0\nfsync_batch_interval_ms = 10\n",
        )
        .unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert!(!c.download.fsync_per_segment, "批量模式 opt-in 生效");
        let pol = c.download.segment_sync_policy();
        assert!(!pol.per_segment);
        assert_eq!(pol.batch_segments, 1, "segments=0 必须钳到下界 1");
        assert_eq!(
            pol.batch_interval,
            std::time::Duration::from_millis(100),
            "interval=10ms 必须钳到下界 100ms"
        );
        // 上界钳制
        let c2 = Config {
            download: DownloadCfg {
                fsync_per_segment: false,
                fsync_batch_segments: 999_999,
                fsync_batch_interval_ms: 999_999_999,
                ..DownloadCfg::default()
            },
            ..Config::default()
        };
        let pol2 = c2.download.segment_sync_policy();
        assert_eq!(pol2.batch_segments, 1024, "segments 越界必须钳到上界 1024");
        assert_eq!(
            pol2.batch_interval,
            std::time::Duration::from_millis(600_000),
            "interval 越界必须钳到上界 600s"
        );
    }

    #[test]
    fn fsync_fields_in_snapshot() {
        // 设置面可见口径：三个旋钮进 /config 快照（非敏感项）
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("snap.toml");
        std::fs::write(
            &p,
            "[download]\nfsync_per_segment = false\nfsync_batch_segments = 16\n",
        )
        .unwrap();
        let c = Config::load(Some(&p)).unwrap();
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["fsync_per_segment"], false);
        assert_eq!(snap["fsync_batch_segments"], 16);
        assert_eq!(snap["fsync_batch_interval_ms"], 2000);
    }

    #[test]
    fn http_token_not_in_snapshot() {
        // 安全回归（V1）：快照只给布尔，绝不泄露 token 本身
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "[server]\nhttp_token = \"s3cret\"\n").unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert_eq!(c.server.http_token.as_deref(), Some("s3cret"));
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["http_token_enabled"], true);
        let raw = snap.to_string();
        assert!(!raw.contains("s3cret"), "token 不得出现在快照: {raw}");
    }

    #[test]
    fn loopback_addr_detection() {
        assert!(Config::is_loopback_addr("127.0.0.1:8787"));
        assert!(Config::is_loopback_addr("127.9.1.1:80"));
        assert!(Config::is_loopback_addr("localhost:8787"));
        assert!(Config::is_loopback_addr("[::1]:8787"));
        assert!(!Config::is_loopback_addr("0.0.0.0:8787"));
        assert!(!Config::is_loopback_addr("192.168.1.5:8787"));
        // batch8：v4-mapped IPv6 回环按映射 v4 判定（字符串前缀法会漏判）
        assert!(Config::is_loopback_addr("[::ffff:127.0.0.1]:8787"));
        assert!(Config::is_loopback_addr("[::1]:1"));
        assert!(!Config::is_loopback_addr("[::ffff:192.168.1.5]:8787"));
    }

    /// 安全回归（第三轮审查 P1-a）：`127.` 字符串前缀绕过必须堵死——
    /// 非 IP 主机名（DNS 可解析到公网）不得通过回环判定。
    /// 修复前实测 4 项绕过：`127.evil.com` / `127.0.0.1.attacker.example` /
    /// `127.0.0.1.nip.io` / `127.0.0.1x`（repro/p0repro round3）。
    #[test]
    fn loopback_prefix_bypass_rejected() {
        assert!(
            !Config::is_loopback_addr("127.evil.com:8787"),
            "127. 前缀非 IP 主机名必须 fail-closed"
        );
        assert!(!Config::is_loopback_addr("127.0.0.1.attacker.example:8787"));
        assert!(!Config::is_loopback_addr("127.0.0.1.nip.io:8787"));
        assert!(!Config::is_loopback_addr("127.0.0.1x:8787"));
        // 真回环与字面量 localhost 仍放行
        assert!(Config::is_loopback_addr("127.0.0.1:8787"));
        assert!(Config::is_loopback_addr("LOCALHOST:8787"));
        assert!(Config::is_loopback_addr("localhost"));
        // 其他非回环主机名同样 fail-closed
        assert!(!Config::is_loopback_addr("example.com:8787"));
    }

    /// 安全回归（第三轮审查 P1-b）：部分表 TOML 的其余字段不得退化为
    /// derive 零值。修复前：`[download] proxy = ...` → dest_root 变 ""；
    /// `[server] http_token = ...` → addr 变 ""（bind 失败）；
    /// `[bt] max_upload_kb_s = ...` → enabled 变 false（BT 静默关闭）。
    #[test]
    fn partial_table_toml_keeps_sibling_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("partial.toml");

        // 只写 [download] 的一个键 → dest_root 必须保持 ./downloads
        std::fs::write(&p, "[download]\nproxy = \"socks5://127.0.0.1:1080\"\n").unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert_eq!(
            c.download.dest_root,
            PathBuf::from("./downloads"),
            "部分表不得令 dest_root 退化为空串"
        );
        assert_eq!(c.download.proxy, "socks5://127.0.0.1:1080");
        assert_eq!(c.download.max_redirects, 10);

        // 只写 [server] 的一个键 → addr 必须保持 127.0.0.1:8787
        std::fs::write(&p, "[server]\nhttp_token = \"x\"\n").unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert_eq!(
            c.server.addr, "127.0.0.1:8787",
            "部分表不得令 addr 退化为空串（bind 失败）"
        );

        // 只写 [bt] 的一个键 → enabled 必须保持 true
        std::fs::write(&p, "[bt]\nmax_upload_kb_s = 100\n").unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert!(
            c.bt.enabled,
            "部分表不得令 bt.enabled 退化为 false（BT 静默关闭）"
        );
        assert_eq!(c.bt.max_upload_kb_s, 100);

        // 只写 [storage] 的一个键 → lock.path 不受影响（不同表互不干扰）
        std::fs::write(&p, "[storage]\ntasks_path = \"/data/t.json\"\n").unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert_eq!(c.storage.tasks_path, PathBuf::from("/data/t.json"));
        assert_eq!(c.lock.path, PathBuf::from("./daemon.lock"));
    }

    #[test]
    fn extra_allowed_hosts_field_defaults_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "[server]\naddr = \"127.0.0.1:8787\"\n").unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert!(
            c.server.extra_allowed_hosts.is_empty(),
            "serde default 必须空"
        );
        std::fs::write(
            &p,
            "[server]\nextra_allowed_hosts = [\"MyHost.local\", \" dev.box \"]\n",
        )
        .unwrap();
        let c = Config::load(Some(&p)).unwrap();
        // 原样透传（归一在守卫比较点做——大小写不敏感 + trim）
        assert_eq!(
            c.server.extra_allowed_hosts,
            vec!["MyHost.local".to_string(), " dev.box ".to_string()]
        );
        assert!(!Config::is_loopback_addr(":::8787"));
    }

    #[test]
    fn extra_allowed_origins_field_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "sd-cfg-cors-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        std::fs::write(&p, "[server]\naddr = \"127.0.0.1:8787\"\n").unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert!(
            c.server.extra_allowed_origins.is_empty(),
            "serde default 必须空（不启用 CORS 时不放行任何非回环 Origin）"
        );
        std::fs::write(
            &p,
            "[server]\nextra_allowed_origins = [\"http://ui.lan:3000\", \" https://cdn.example.org \"]\n",
        )
        .unwrap();
        let c = Config::load(Some(&p)).unwrap();
        // 原样透传（归一在 builder 注入点做——与 extra_allowed_hosts 同口径）
        assert_eq!(
            c.server.extra_allowed_origins,
            vec![
                "http://ui.lan:3000".to_string(),
                " https://cdn.example.org ".to_string()
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn default_values() {
        let c = Config::default();
        assert_eq!(c.server.addr, "127.0.0.1:8787");
        assert_eq!(c.download.dest_root, PathBuf::from("./downloads"));
        assert!(c.bt.enabled);
        assert_eq!(c.lock.path, PathBuf::from("./daemon.lock"));
        assert_eq!(c.bt_save_path(), c.download.dest_root);
        // 发现层开关默认全关（不改变现有行为）
        assert!(!c.bt.enable_dht);
        assert!(!c.bt.enable_lsd);
        assert!(!c.bt.enable_upnp);
        // 传输层默认：PEX/uTP 关，加密 allow（= 内核默认行为不变）
        assert!(!c.bt.enable_pex);
        assert!(!c.bt.enable_utp);
        assert_eq!(c.bt.encrypt, "allow");
    }

    #[test]
    fn bt_discovery_defaults_off_in_snapshot() {
        // 快照含三键且默认 false
        let c = Config::default();
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["bt_enable_dht"], false);
        assert_eq!(snap["bt_enable_lsd"], false);
        assert_eq!(snap["bt_enable_upnp"], false);
        assert_eq!(snap["bt_enable_pex"], false);
        assert_eq!(snap["bt_enable_utp"], false);
        assert_eq!(snap["bt_encrypt"], "allow");
    }

    #[test]
    fn bt_discovery_toml_overrides() {
        // TOML 开启值解析 + 快照反映开启值
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(
            &p,
            r#"
[bt]
enable_dht = true
enable_lsd = true
enable_upnp = true
"#,
        )
        .unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert!(c.bt.enable_dht);
        assert!(c.bt.enable_lsd);
        assert!(c.bt.enable_upnp);
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["bt_enable_dht"], true);
        assert_eq!(snap["bt_enable_lsd"], true);
        assert_eq!(snap["bt_enable_upnp"], true);

        // 部分开启：仅 dht
        let p2 = dir.path().join("config2.toml");
        std::fs::write(&p2, "[bt]\nenable_dht = true\n").unwrap();
        let c2 = Config::load(Some(&p2)).unwrap();
        assert!(c2.bt.enable_dht);
        assert!(!c2.bt.enable_lsd);
        assert!(!c2.bt.enable_upnp);
    }

    #[test]
    fn bt_transport_toml_overrides() {
        // 传输层三键 TOML 解析 + 快照反映；encrypt 缺省回填 allow（serde 字段默认）
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(
            &p,
            r#"
[bt]
enable_pex = true
enable_utp = true
encrypt = "require"
"#,
        )
        .unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert!(c.bt.enable_pex);
        assert!(c.bt.enable_utp);
        assert_eq!(c.bt.encrypt, "require");
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["bt_enable_pex"], true);
        assert_eq!(snap["bt_enable_utp"], true);
        assert_eq!(snap["bt_encrypt"], "require");

        // [bt] 段存在但 encrypt 缺省 → serde 字段默认回填 allow（非空串）
        let p2 = dir.path().join("config2.toml");
        std::fs::write(&p2, "[bt]\nenabled = true\n").unwrap();
        let c2 = Config::load(Some(&p2)).unwrap();
        assert_eq!(c2.bt.encrypt, "allow");
    }

    #[test]
    fn bt_encrypt_invalid_rejected() {
        // 非法 encrypt 值 → load 报错（fail-fast，含合法值提示）
        let dir = tempfile::tempdir().unwrap();
        for bad in ["forced", "ALLOW", "", "on"] {
            let p = dir.path().join(format!("{bad}.toml"));
            std::fs::write(&p, format!("[bt]\nencrypt = \"{bad}\"\n")).unwrap();
            let err = Config::load(Some(&p)).unwrap_err();
            assert!(
                err.contains("disable / allow / require"),
                "报错应含合法值提示: {err}"
            );
        }
        // 前后空白容忍（与 btcore parse_encrypt_policy 口径一致）
        let p = dir.path().join("ok.toml");
        std::fs::write(&p, "[bt]\nencrypt = \" require \"\n").unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert_eq!(c.bt.encrypt, " require ");
    }

    #[test]
    fn parse_toml_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(
            &p,
            r#"
[server]
addr = "0.0.0.0:9999"

[download]
dest_root = "/data/dl"
proxy = "socks5://u:p@127.0.0.1:1080"
max_download_kb_s = 2048

[bt]
enabled = false
save_path = "/data/bt"
max_upload_kb_s = 512

[provider]
enabled = true
mock = true

[lock]
path = "/tmp/sd.lock"
"#,
        )
        .unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert_eq!(c.server.addr, "0.0.0.0:9999");
        assert_eq!(c.download.dest_root, PathBuf::from("/data/dl"));
        assert_eq!(c.download.proxy, "socks5://u:p@127.0.0.1:1080");
        assert_eq!(c.download.max_download_kb_s, 2048);
        assert!(!c.bt.enabled);
        assert_eq!(c.bt_save_path(), PathBuf::from("/data/bt"));
        assert_eq!(c.bt.max_upload_kb_s, 512);
        assert!(c.provider.enabled);
        assert!(c.provider.mock);
        assert_eq!(c.lock.path, PathBuf::from("/tmp/sd.lock"));
        // 快照：含限速、代理仅暴露开关（不泄露凭据）、provider 开关
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["max_download_kb_s"], 2048);
        assert_eq!(snap["max_upload_kb_s"], 512);
        assert!(snap["proxy_enabled"].as_bool().unwrap());
        assert!(snap["provider_enabled"].as_bool().unwrap());
        assert!(
            !snap.as_object().unwrap().contains_key("proxy"),
            "快照不得含代理 URL"
        );
    }

    #[test]
    fn provider_xunlei_default_off() {
        // 默认关闭、无 token_path，快照不得暴露路径，仅给布尔。
        let c = Config::default();
        assert!(!c.provider_xunlei.enabled);
        assert!(c.provider_xunlei.token_path.is_none());
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["provider_xunlei_enabled"], false);
        assert_eq!(snap["provider_xunlei_token_exists"], false);
        assert!(
            !snap
                .as_object()
                .unwrap()
                .contains_key("provider_xunlei_token_path"),
            "快照不得含 token 路径"
        );
    }

    #[test]
    fn provider_xunlei_enabled_with_token_path() {
        let dir = tempfile::tempdir().unwrap();
        // 写一个真实的登录态文件，验证快照能反映其存在性。
        let auth = dir.path().join("xunlei_auth.json");
        std::fs::write(&auth, r#"{"access_token":"a","refresh_token":"r","device_id":"d","captcha_token":"c","user_id":"1","access_token_expires_at":0,"captcha_token_expires_at":0}"#).unwrap();
        let missing = dir.path().join("no_such.json");
        let p = dir.path().join("config.toml");
        std::fs::write(
            &p,
            format!(
                "[provider_xunlei]\nenabled = true\ntoken_path = \"{}\"\n",
                auth.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert!(c.provider_xunlei.enabled);
        assert_eq!(c.provider_xunlei.token_path, Some(auth.clone()));
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["provider_xunlei_enabled"], true);
        assert_eq!(snap["provider_xunlei_token_exists"], true);

        // 不存在的 token_path：enabled 仍 true，但 token_exists 为 false。
        let p2 = dir.path().join("config2.toml");
        std::fs::write(
            &p2,
            format!(
                "[provider_xunlei]\nenabled = true\ntoken_path = \"{}\"\n",
                missing.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let c2 = Config::load(Some(&p2)).unwrap();
        assert!(c2.provider_xunlei.enabled);
        let snap2 = c2.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap2["provider_xunlei_enabled"], true);
        assert_eq!(snap2["provider_xunlei_token_exists"], false);
    }

    #[test]
    fn provider_quark_default_off() {
        // UNWIRED #3：默认关闭、无 cookie_path；快照仅布尔，不泄露路径。
        let c = Config::default();
        assert!(!c.provider_quark.enabled);
        assert!(c.provider_quark.cookie_path.is_none());
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["provider_quark_enabled"], false);
        assert_eq!(snap["provider_quark_cookie_exists"], false);
        assert!(
            !snap
                .as_object()
                .unwrap()
                .contains_key("provider_quark_cookie_path"),
            "快照不得含 cookie 路径"
        );
    }

    #[test]
    fn provider_quark_enabled_with_cookie_path() {
        // enabled=true + 真实 cookie 文件 → 快照反映存在性；缺失文件 → false。
        let dir = tempfile::tempdir().unwrap();
        let cookie = dir.path().join("quark_auth.json");
        std::fs::write(
            &cookie,
            r#"{"cookie":"__pus=demo; __puus=demo","user_id":"42"}"#,
        )
        .unwrap();
        let missing = dir.path().join("no_such.json");
        let p = dir.path().join("config.toml");
        std::fs::write(
            &p,
            format!(
                "[provider_quark]\nenabled = true\ncookie_path = \"{}\"\n",
                cookie.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert!(c.provider_quark.enabled);
        assert_eq!(c.provider_quark.cookie_path, Some(cookie.clone()));
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["provider_quark_enabled"], true);
        assert_eq!(snap["provider_quark_cookie_exists"], true);

        let p2 = dir.path().join("config2.toml");
        std::fs::write(
            &p2,
            format!(
                "[provider_quark]\nenabled = true\ncookie_path = \"{}\"\n",
                missing.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let c2 = Config::load(Some(&p2)).unwrap();
        assert!(c2.provider_quark.enabled);
        let snap2 = c2.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap2["provider_quark_cookie_exists"], false);
    }

    #[test]
    fn provider_xunlei_tier_parsing_and_precedence() {
        // P1-1：tier 字段解析 + env 覆盖 + 默认 web。
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "[provider_xunlei]\nenabled = true\ntier = \"nas\"\n").unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert_eq!(c.provider_xunlei.tier.as_deref(), Some("nas"));
        // 无 env：config 生效
        std::env::remove_var("SMART_DL_XUNLEI_TIER");
        assert_eq!(c.resolve_xunlei_tier_name(), "nas");
        // 快照暴露档位名（非敏感）
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["provider_xunlei_tier"], "nas");

        // env 优先于 config
        std::env::set_var("SMART_DL_XUNLEI_TIER", "web");
        assert_eq!(c.resolve_xunlei_tier_name(), "web");
        std::env::remove_var("SMART_DL_XUNLEI_TIER");

        // 默认（无 config 无 env）= web
        let d = Config::default();
        assert_eq!(d.resolve_xunlei_tier_name(), "web");
        let snap_d = d.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap_d["provider_xunlei_tier"], "web");
    }

    #[test]
    fn missing_file_uses_default() {
        assert_eq!(Config::load(None).unwrap().server.addr, "127.0.0.1:8787");
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.toml");
        assert!(
            Config::load(Some(&missing)).is_err(),
            "缺失文件应报错（显式路径）"
        );
    }

    #[test]
    fn partial_toml_keeps_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "[server]\naddr = \"127.0.0.1:8080\"\n").unwrap();
        let c = Config::load(Some(&p)).unwrap();
        assert_eq!(c.server.addr, "127.0.0.1:8080");
        assert_eq!(
            c.download.dest_root,
            PathBuf::from("./downloads"),
            "未写字段用默认"
        );
    }

    #[test]
    fn bad_toml_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "server = [unclosed").unwrap();
        let err = Config::load(Some(&p)).unwrap_err();
        assert!(err.contains("解析失败"), "应报解析错误: {err}");
    }

    #[test]
    fn webhook_url_parse_and_snapshot() {
        // E17：[webhook] url 解析 + 快照透出（运维可读，非敏感）
        let c = Config::default();
        assert_eq!(c.webhook.url, "", "默认禁用");
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["webhook_url"], "");

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "[webhook]\nurl = \"http://127.0.0.1:9000/hook\"\n").unwrap();
        let c2 = Config::load(Some(&p)).unwrap();
        assert_eq!(c2.webhook.url, "http://127.0.0.1:9000/hook");
        let snap2 = c2.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap2["webhook_url"], "http://127.0.0.1:9000/hook");
    }

    #[test]
    fn post_download_parse_and_snapshot() {
        // E27：[post_download] move_to/hook 解析 + 快照透出
        let c = Config::default();
        assert_eq!(c.post_download.move_to, "", "默认禁用");
        assert_eq!(c.post_download.hook, "", "默认禁用");
        let snap = c.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap["post_move_to"], "");
        assert_eq!(snap["post_hook"], "");

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(
            &p,
            "[post_download]\nmove_to = \"/data/inbox\"\nhook = \"/usr/local/bin/on-done.sh\"\n",
        )
        .unwrap();
        let c2 = Config::load(Some(&p)).unwrap();
        assert_eq!(c2.post_download.move_to, "/data/inbox");
        assert_eq!(c2.post_download.hook, "/usr/local/bin/on-done.sh");
        let snap2 = c2.snapshot_json(&PathBuf::from("/tmp/tasks.json"));
        assert_eq!(snap2["post_move_to"], "/data/inbox");
        assert_eq!(snap2["post_hook"], "/usr/local/bin/on-done.sh");
    }
}
