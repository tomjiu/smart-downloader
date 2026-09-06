/// serve 配置（TOML）：HTTP 监听地址 / 默认下载目录 / BT 引擎开关 / 单实例锁路径 /
/// 云兜底 Provider / 任务持久化。
/// 文件缺失时使用默认值（Config::default）；`--config <path>` 覆盖。
/// 全量 `Serialize`（S1 设置面）：支撑 `PUT /settings?persist=true` 的原子回写
/// （`Config::save_to`）——注释不保留（TOML round-trip 丢失注释为已知边界）。
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    pub webhook: WebhookCfg,
    pub cleanup: CleanupCfg,
    pub post_download: PostDownloadCfg,
    pub scheduler: SchedulerCfg,
    pub lock: LockCfg,
    pub storage: StorageCfg,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ServerCfg {
    /// HTTP/WS 监听地址，如 `127.0.0.1:8787`。
    pub addr: String,
    /// 安全修复（V1/V13）：HTTP API Bearer token。配置后全端点（含 /ws 握手）
    /// 要求 `Authorization: Bearer <token>`；未配置时：回环监听放行（本机 CLI
    /// 兼容），**非回环监听拒绝启动**（fail-closed，防 0.0.0.0 裸奔）。
    /// 不参与热重载（避免认证态中途抖动）。敏感项：不出现在 `/config` 快照。
    pub http_token: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
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
            enabled: false,
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

/// 任务完成 Webhook 配置（E17）：任务到达完成态时 daemon 向 `url` POST 一条
/// JSON 通知（fire-and-forget，单次尝试 5s 超时，失败仅记日志）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct WebhookCfg {
    /// 完成通知 Webhook URL；空 = 禁用（默认）。参与热重载。
    pub url: String,
}

/// 已完成任务自动清理配置（E20）：按完成龄期清扫 Completed 任务。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct CleanupCfg {
    /// Completed 任务保留天数（从完成时刻起算）；0 = 禁用（默认）。
    /// 清扫间隔固定 10min；文件默认保留（keep_data）。
    pub auto_remove_completed_days: u32,
    /// 自动清理时是否同时删除已下载数据；true = 保留文件（默认）。
    pub auto_remove_keep_data: bool,
}

/// 下载完成自动处理配置（E27，清单 #15）：完成后移动 + 外部钩子。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct PostDownloadCfg {
    /// 完成后把落盘文件移动到该目录（目录自动创建；同盘 rename，跨盘
    /// copy+delete 回退；同名冲突自动改名 `name(1).ext`）。空 = 禁用（默认）。
    /// 仅对单文件任务生效（BT 多文件目录跳过）；`conflict_policy=skip` 的
    /// 任务不移动（尊重"既有文件保持原样"语义，钩子照发）。
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
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct LockCfg {
    /// 单实例锁文件路径（重复启动 → 拒绝）。
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct StorageCfg {
    /// 任务持久化文件（add/remove/状态变更自动落盘；启动时恢复）。
    pub tasks_path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server: ServerCfg {
                addr: "127.0.0.1:8787".into(),
                http_token: None,
            },
            download: DownloadCfg {
                dest_root: PathBuf::from("./downloads"),
                proxy: String::new(),
                max_download_kb_s: 0,
                disk_precheck_strict: false,
            },
            bt: BtCfg {
                enabled: true,
                save_path: None,
                max_upload_kb_s: 0,
                enable_dht: false,
                enable_lsd: false,
                enable_upnp: false,
                enable_pex: false,
                enable_utp: false,
                encrypt: "allow".to_string(),
                listen_port: 0,
                max_connections: 0,
                extra_trackers: Vec::new(),
                max_share_ratio: default_bt_max_share_ratio(),
                max_seeding_time_min: 0,
            },
            limits: LimitsCfg::default(),
            queue: QueueCfg::default(),
            rss: RssCfg::default(),
            xunlei: XunleiCfg::default(),
            provider: ProviderCfg {
                enabled: false,
                mock: false,
            },
            provider_xunlei: ProviderXunleiCfg::default(),
            webhook: WebhookCfg::default(),
            cleanup: CleanupCfg::default(),
            post_download: PostDownloadCfg::default(),
            scheduler: SchedulerCfg::default(),
            lock: LockCfg {
                path: PathBuf::from("./daemon.lock"),
            },
            storage: StorageCfg {
                tasks_path: PathBuf::from("./tasks.json"),
            },
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
        // bt.encrypt 三态校验（fail-fast：未知值启动即报错，不等到引擎装配）。
        // 合法值口径须与 btcore::engine::parse_encrypt_policy 一致（btcore 是
        // feature 门控依赖，config 不能引用其类型，此处本地同步白名单）。
        if !matches!(cfg.bt.encrypt.trim(), "disable" | "allow" | "require") {
            return Err(format!(
                "配置 bt.encrypt = {:?} 无效：仅支持 disable / allow / require",
                cfg.bt.encrypt
            ));
        }
        // 做种分享率上限（qbit Share Ratio Limit）：负数/NaN/超上限 → 拒绝启动
        if !(0.0..=9999.0).contains(&cfg.bt.max_share_ratio) {
            return Err(format!(
                "配置 bt.max_share_ratio = {} 无效：须在 0.0..=9999.0（0 = 不启用）",
                cfg.bt.max_share_ratio
            ));
        }
        // 做种时长上限（分钟）：超上限 → 拒绝启动
        if cfg.bt.max_seeding_time_min > 999_999 {
            return Err(format!(
                "配置 bt.max_seeding_time_min = {} 无效：须在 0..=999999（0 = 不启用）",
                cfg.bt.max_seeding_time_min
            ));
        }
        Ok(cfg)
    }

    /// 判定 addr 是否仅绑定回环地址（127.x/::1/localhost）。
    /// serve 启动检查用：非回环 + 无 http_token → 拒绝启动（V1 fail-closed）。
    pub fn is_loopback_addr(addr: &str) -> bool {
        let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
        let host = host.trim_start_matches('[').trim_end_matches(']');
        host == "localhost" || host.starts_with("127.") || host == "::1" || host == "[::1]"
    }

    /// 序列化为 TOML 文本（S1 设置面持久化）。无默认值裁剪——全量字段
    /// 写出，用户手改注释/字段顺序会丢（文档化边界）。
    pub fn to_toml_string(&self) -> Result<String, String> {
        toml::to_string_pretty(self).map_err(|e| format!("配置序列化失败: {e}"))
    }

    /// 原子落盘（tmp + rename，S1）：`PUT /settings?persist=true` 用。
    /// 目标目录不存在 → Err。
    pub fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        let text = self.to_toml_string()?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, &text).map_err(|e| format!("写入临时文件 {tmp:?} 失败: {e}"))?;
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
            "max_upload_kb_s": self.bt.max_upload_kb_s,
            "proxy_enabled": !self.download.proxy.is_empty(),
            "provider_enabled": self.provider.enabled,
            "provider_xunlei_enabled": self.provider_xunlei.enabled,
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
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!Config::is_loopback_addr(":::8787"));
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
