//! DaemonState 生命周期与配置面：构造器（with_*）、全局限速、配置快照与热重载、鉴权（verify_http_token/ct_eq）、dest 白名单、引擎接入。

use super::*;

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
            persist_lock: Mutex::new(()),
            default_dest_root: Mutex::new(PathBuf::from(".")),
            allowed_roots: Mutex::new(Vec::new()),
            http_token: None,
            disk_precheck_strict: false,
            config_snapshot: Mutex::new(None),
            pending_file_prio: Mutex::new(HashSet::new()),
            slot_reservations: Mutex::new(HashSet::new()),
            ban_replay_failed: Mutex::new(Vec::new()),
            extra_allowed_hosts: Vec::new(),
            global_limits: Mutex::new(GlobalLimits {
                max_download_kb_s: 0,
                max_upload_kb_s: 0,
            }),
            webhook_url: Mutex::new(None),
            completion_action: Mutex::new("none".to_string()),
            completion_fired: std::sync::atomic::AtomicBool::new(false),
            ban_ops: tokio::sync::Mutex::new(()),
            rss_refresh_gate: tokio::sync::Mutex::new(()),
            session_traffic: Mutex::new((0, 0)),
            webhook_client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
            bootstrap_client: None,
            post_move_to: Mutex::new(None),
            post_hook: Mutex::new(None),
            cleanup: Mutex::new(crate::config::CleanupCfg::default()),
            start_jitter_secs: std::sync::atomic::AtomicU32::new(0),
            base_limits: Mutex::new(GlobalLimits {
                max_download_kb_s: 0,
                max_upload_kb_s: 0,
            }),
            alt_cfg: Mutex::new(crate::config::LimitsCfg::default()),
            alt_active: std::sync::atomic::AtomicBool::new(false),
            queue_cfg: Mutex::new(crate::config::QueueCfg::default()),
            config_path: Mutex::new(None),
            live_config: Mutex::new(None),
            rss: Mutex::new(crate::rss::RssState::default()),
            rss_persist_path: None,
            bt_bans: Mutex::new(Vec::new()),
            bans_persist_path: None,
        }
    }

    /// 注入错峰随机延迟上限（E23；serve 从 `[scheduler] start_jitter_seconds` 传入）。
    pub fn with_start_jitter(self, secs: u32) -> Self {
        self.start_jitter_secs
            .store(secs, std::sync::atomic::Ordering::Relaxed);
        self
    }

    /// 注入完成动作（Task 46；serve 从 `[scheduler] completion_action` 传入）。
    /// 非法值仅记 warn 并回落 none（配置合法性由 settings 校验面把关）。
    pub fn with_completion_action(self, action: &str) -> Self {
        *self.completion_action.lock() = normalize_completion_action(action);
        self
    }

    /// 完成动作热更（settings PUT/refresh_config 路径）。
    pub(crate) fn set_completion_action(&self, action: &str) {
        *self.completion_action.lock() = normalize_completion_action(action);
        // batch5-P2（热更补判）：原实现只在任务完成事件触发判定——若全部任务
        // 已终态后才改配置/热重载启用 shutdown/exit，永远不会再有完成事件，
        // 新设动作静默失效。注入后补判一次（空表/未全终态时内部自复位）。
        self.maybe_run_completion_action();
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

    /// tokenless 模式判定（batch8）：http_token 为 None（空串已过滤）。
    /// Host/Origin 同源守卫仅在此模式激活——配置 token 后攻击者无凭据，
    /// 且反代域名 Host 合法性不应被收紧。
    pub(crate) fn tokenless(&self) -> bool {
        self.http_token.is_none()
    }

    /// 注入 tokenless 模式额外放行的 Host 名（batch8）：反向代理/本机域名
    /// 场景（如 `myhost.local`）。仅 tokenless 模式消费；配置 token 后无效。
    pub fn with_extra_allowed_hosts(mut self, hosts: Vec<String>) -> Self {
        self.extra_allowed_hosts = hosts
            .into_iter()
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        self
    }

    /// 注入磁盘预检严格模式（V10-2）：true = 空间不可探测时拒绝入队。
    pub fn with_disk_precheck_strict(mut self, strict: bool) -> Self {
        self.disk_precheck_strict = strict;
        self
    }

    /// 注入 Metalink 引导 XML 拉取 client（B1）：serve 传入引擎全局 client
    /// 克隆（同源代理/cookie jar/超时口径，见 serve.rs 组装处）。
    pub fn with_bootstrap_client(mut self, client: reqwest::Client) -> Self {
        self.bootstrap_client = Some(client);
        self
    }

    /// 注入全局限速总阀门初始值（E16）：serve 从 config
    /// `[download] max_download_kb_s` + `[bt] max_upload_kb_s` 传入——
    /// 引擎构造时已携同值，此处仅同步内存口径（GET /config/限速查询一致）。
    /// S1：基准限速同步注入（启动配置即基准值）。
    pub fn with_global_limits(mut self, max_download_kb_s: u32, max_upload_kb_s: u32) -> Self {
        self.global_limits = Mutex::new(GlobalLimits {
            max_download_kb_s,
            max_upload_kb_s,
        });
        self.base_limits = Mutex::new(GlobalLimits {
            max_download_kb_s,
            max_upload_kb_s,
        });
        self
    }

    /// 读取全局限速总阀门当前值（E16）。
    pub fn global_limits(&self) -> GlobalLimits {
        *self.global_limits.lock()
    }

    /// 读取基准限速（S1）：备用窗口生效时 ≠ global_limits（引擎实际值）。
    pub fn base_limits(&self) -> GlobalLimits {
        *self.base_limits.lock()
    }

    /// 注入备用限速调度配置（S1）：serve 从 `[limits]` 传入；热重载跟随。
    pub fn with_limits_cfg(self, cfg: crate::config::LimitsCfg) -> Self {
        *self.alt_cfg.lock() = cfg;
        self
    }

    /// 注入并发队列配额（S1）：serve 从 `[queue]` 传入；热重载跟随。
    pub fn with_queue_cfg(self, cfg: crate::config::QueueCfg) -> Self {
        *self.queue_cfg.lock() = cfg;
        self
    }

    /// 注入配置文件路径（S1 持久化）：`--config` 存在时由 serve 传入。
    pub fn with_config_path(self, path: Option<PathBuf>) -> Self {
        *self.config_path.lock() = path;
        self
    }

    /// 注入当前权威配置（S1）：serve 启动时传入；热重载 refresh_config 刷新。
    pub fn with_live_config(self, cfg: crate::config::Config) -> Self {
        *self.live_config.lock() = Some(cfg);
        self
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
    /// S1 语义补充：本方法=「用户手动设定」——同时更新基准限速（base_limits）；
    /// 若备用限速窗口正生效，立即重评估（备用优先级 > 基准，见 tick_alt_limits）。
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
        let base = {
            let b = *self.base_limits.lock();
            GlobalLimits {
                max_download_kb_s: down_kb_s.unwrap_or(b.max_download_kb_s),
                max_upload_kb_s: up_kb_s.unwrap_or(b.max_upload_kb_s),
            }
        };
        *self.base_limits.lock() = base;
        // 备用窗口正命中时：基准变更不直接生效，重评估取备用/基准优胜者
        //（首次调用链上 alt 未配置时 alt_active=false → 等价直接生效）。
        if self.alt_active.load(std::sync::atomic::Ordering::Relaxed) {
            self.tick_alt_limits().await?;
            return Ok(*self.global_limits.lock());
        }
        let effective = base;
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
        for kind in [EngineKind::Ftp, EngineKind::Http, EngineKind::Sftp] {
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
    pub(super) fn overlay_config_limits(&self, g: GlobalLimits) {
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
    pub(super) async fn dispatch_global_limits(
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

    /// 生效的 dest 白名单（V2）：未显式注入时兜底 default_dest_root。
    ///
    /// 锁序约定（docs/LOCK_MODEL.md）：顺序获取 `allowed_roots` → guard
    /// 语句尾即释放 → 按需获取 `default_dest_root`，两把锁任何路径不同时
    /// 持有——全域锁模型维持「任何时刻至多持一把锁」强不变量（2026-09
    /// 锁模型审计中唯一的多锁同持边，现已消除）。
    pub(super) fn dest_roots(&self) -> Vec<PathBuf> {
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

    /// 审计修复（40-e P1-6）：SSE EventSource 鉴权回退——浏览器规范限制
    /// EventSource 无法自定义请求头，配置 token 后 UI 事件流永久 401。
    /// 仅限 /events/stream 的 ?token= 查询参数（原值/percent-decode 双比较，
    /// 常量时间与 Bearer 同口径）。
    pub fn verify_http_token_query(&self, token: &str) -> bool {
        match self.http_token.as_deref() {
            None | Some("") => true,
            Some(expect) => ct_eq(token, expect),
        }
    }

    /// 注入生效配置快照（`GET /config` 返回；serve 组装精简字段）。
    pub fn with_config(self, snapshot: serde_json::Value) -> Self {
        *self.config_snapshot.lock() = Some(snapshot);
        self
    }

    /// 启用任务持久化（每次变更自动写 JSON 到 `path`）。
    /// 同时派生 RSS 状态持久化路径（tasks.json 同目录 rss.json，qbit RSS 对标）。
    /// batch3-P1：回读 rss.json——旧实现只写不读（RssState::load 零调用点），
    /// 重启后订阅/规则全部丢失，规则自动下载静默停摆。
    pub fn with_storage(mut self, path: PathBuf) -> Self {
        let rss_path = path.parent().map(|d| d.join("rss.json"));
        if let Some(rp) = &rss_path {
            if let Some(st) = crate::rss::RssState::load(rp) {
                tracing::info!(
                    "RSS 状态回读: {} feeds / {} rules",
                    st.feeds.len(),
                    st.rules.len()
                );
                self.rss = parking_lot::Mutex::new(st);
            }
        }
        self.rss_persist_path = rss_path;
        // Task 46：bans.json 回读（tasks.json 同目录；缺省空 = 无显式封禁）。
        // 引擎侧重放由 serve 在 with_storage 之后调 replay_bans（batch5-P1：
        // 原注释写"BT 装配完成后"，实际装配处调用早于本回读，恒空转）。
        let bans_path = path.parent().map(|d| d.join("bans.json"));
        if let Some(bp) = &bans_path {
            match super::persistence::read_bans(bp) {
                Ok(bans) if !bans.is_empty() => {
                    tracing::info!("IP 封禁列表回读: {} 条", bans.len());
                    self.bt_bans = Mutex::new(bans);
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("bans.json 回读失败 {bp:?}: {e}"),
            }
        }
        self.bans_persist_path = bans_path;
        self.persist_path = Some(path);
        self
    }

    /// 启动封禁重放（Task 46；serve 在 with_storage 之后调用）：逐条下发引擎
    /// session 级 ban（best-effort：单条失败不再仅 warn 后整会话丢失——
    /// batch7-P2 失败条目挂入 ban_replay_failed 待重试集，serve 30s tick
    /// 重发至成功；引擎不可用场景全量入集而非整体放弃）。
    /// batch5：支持区间条目（"a-b" → ban_ip_range；Task 46 持久化格式扩展）。
    pub async fn replay_bans(&self) {
        let bans = self.bt_bans.lock().clone();
        if bans.is_empty() {
            return;
        }
        let engine = match self.engine_for(EngineKind::Bt) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    "IP 封禁重放暂缓：BT 引擎不可用（{e}）——{} 条挂入待重试集，30s tick 重发",
                    bans.len()
                );
                *self.ban_replay_failed.lock() = bans.clone();
                return;
            }
        };
        let mut ok = 0usize;
        let mut failed = Vec::new();
        for entry in &bans {
            let res = match entry.split_once('-') {
                // 区间条目（batch5）："a-b"；split_once 对 IPv6 冒号无影响
                //（用 '-' 分隔持久化格式，v6 字面量自身不含 '-'）
                Some((a, b)) => engine.ban_ip_range(a, b).await,
                None => engine.ban_ip(entry).await,
            };
            match res {
                Ok(()) => ok += 1,
                Err(e) => {
                    tracing::warn!("IP 封禁重放失败 {entry}: {e}（挂入待重试集）");
                    failed.push(entry.clone());
                }
            }
        }
        *self.ban_replay_failed.lock() = failed;
        tracing::info!("IP 封禁重放完成: {ok}/{} 条", bans.len());
    }

    /// ban 重放失败重试泵（batch7-P2；serve 30s tick 驱动）：待重试集空时
    /// 零开销直返。全程持 ban_ops 串行锁（与 ban/unban/import 同纪律）——
    /// 防止用户 unban 与重试下发交错（重试把已解封 IP 重新封回去）。
    /// 已被用户从 bt_bans 移除的条目直接丢弃（不再下发）。重试仍失败 →
    /// 留在集内下轮再试；引擎不可用 → 整轮保留。
    pub async fn retry_pending_bans(&self) {
        let pending = self.ban_replay_failed.lock().clone();
        if pending.is_empty() {
            return;
        }
        let Ok(engine) = self.engine_for(EngineKind::Bt) else {
            return; // 引擎不可用：保留待重试集，下轮再试
        };
        let _gate = self.ban_ops.lock().await;
        let mut remaining = Vec::new();
        let mut ok = 0usize;
        for entry in &pending {
            // 用户可能已在待重试期间 unban：以权威表为准，不在表内即丢弃
            if !self.bt_bans.lock().contains(entry) {
                ok += 1; // 视作已收敛（无需下发）
                continue;
            }
            let res = match entry.split_once('-') {
                Some((a, b)) => engine.ban_ip_range(a, b).await,
                None => engine.ban_ip(entry).await,
            };
            match res {
                Ok(()) => ok += 1,
                Err(_) => remaining.push(entry.clone()),
            }
        }
        *self.ban_replay_failed.lock() = remaining;
        if ok > 0 {
            tracing::info!("IP 封禁待重试集处理完成: {ok}/{} 条", pending.len());
        }
    }

    /// RSS bootstrap client 克隆（与 metalink bootstrap 同源 client；None = 测试装配）。
    pub(crate) fn bootstrap_client_opt(&self) -> Option<reqwest::Client> {
        self.bootstrap_client.clone()
    }

    /// `[rss] max_processed_items_per_feed`（live_config 注入优先；None = 上层兑底）。
    pub(crate) fn rss_max_processed_items_opt(&self) -> Option<usize> {
        self.live_config
            .lock()
            .as_ref()
            .map(|c| c.rss.max_processed_items_per_feed as usize)
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

    /// 追加 SFTP 引擎（feature `sftp`，C-S1；sftp:// 链接路由到该引擎）。
    /// 独立占用 `EngineKind::Sftp` 槽位；队列门控与 FTP 共用 ftp 桶（slot_index）。
    #[cfg(feature = "sftp")]
    pub fn with_sftp(mut self, sftp: Arc<dyn DownloadEngine>) -> Self {
        self.engines.insert(EngineKind::Sftp, sftp);
        self
    }

    pub(super) fn engine_for(
        &self,
        kind: EngineKind,
    ) -> Result<Arc<dyn DownloadEngine>, DaemonError> {
        self.engines.get(&kind).cloned().ok_or_else(|| {
            DaemonError::InvalidSource(format!("引擎未加载: {:?}（编译时启用对应 feature）", kind))
        })
    }

    pub fn hub(&self) -> &WsHub {
        &self.hub
    }

    /// 生效配置快照（`GET /config` 返回；未注入时给出提示对象）。
    pub fn config_snapshot(&self) -> serde_json::Value {
        self.config_snapshot
            .lock()
            .clone()
            .unwrap_or_else(|| serde_json::json!({ "note": "配置快照未注入（serve 组装）" }))
    }

    /// #6 TOML 热重载应用：配置重读后刷新可热更字段（default_dest_root + /config 快照）。
    /// 变更项记日志；不变项静默。S1：同步刷新权威配置 + 备用限速/队列配置。
    pub fn refresh_config(&self, cfg: &crate::config::Config, tasks_path: &std::path::Path) {
        // S1：权威配置先落地（后续字段均从 cfg 读，最后统一快照）
        *self.live_config.lock() = Some(cfg.clone());
        // S1：备用限速/队列配置热重载跟随（运行时效果由 serve 的 ticker
        // 下轮评估 + apply_global_limits 文件为准路径接力，此处仅存配置）
        *self.alt_cfg.lock() = cfg.limits.clone();
        *self.queue_cfg.lock() = cfg.queue.clone();
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
        // Task 46 审查修复（P2）：completion_action 热重载跟随（字段文档承诺
        // 「热重载跟随」；原实现仅启动时 with_completion_action 注入一次，
        // 直接编辑 TOML 改完成动作被静默忽略）。
        self.set_completion_action(&cfg.scheduler.completion_action);
    }
}

/// 常量时间字节串比较（第六轮审计 9.3.4）：token 精确比较走固定时长路径，
/// 消除逐字节短路比较的时序侧信道。长度不等提前返回会泄露长度信息——
/// 对高熵随机 token 而言长度本身非敏感，业界标准做法可接受。
pub(super) fn ct_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// 完成动作归一化（Task 46）：trim + 小写；未知值回落 "none"（warn 由
/// 调用方路径记录）。合法全集 = none/exit/shutdown/sleep/hibernate。
pub(crate) fn normalize_completion_action(action: &str) -> String {
    let a = action.trim().to_ascii_lowercase();
    match a.as_str() {
        "" | "none" => "none".to_string(),
        "exit" | "shutdown" | "sleep" | "hibernate" => a,
        other => {
            tracing::warn!("未知 completion_action {other:?}，回落 none");
            "none".to_string()
        }
    }
}

/// 完成动作执行（Task 46）：exit = 进程退出（桌面壳随之退出）；shutdown /
/// sleep / hibernate = 系统级动作（跨平台命令 spawn，失败仅记日志）。
/// 调用方保证一次性（fired 旗标）；此处 fire-and-forget，不阻塞完成链路。
pub(crate) fn execute_completion_action(action: &str) {
    tracing::info!("全部任务已终态，执行完成动作: {action}");
    match action {
        "exit" => {
            // batch5-P2：真给日志一帧落盘窗口（原注释称有窗口实则立即
            // exit）；短眠后退出（桌面端壳会随 sidecar 退出联动收尾）。
            std::thread::sleep(std::time::Duration::from_millis(250));
            std::process::exit(0);
        }
        "shutdown" | "sleep" | "hibernate" => {
            // batch5-P2：system_power_action 同步阻塞（systemctl 可能慢），
            // 改 spawn_blocking 不占死 tokio worker。失败日志移入闭包内
            // （原失败处理在同步路径，现 fire-and-forget）。
            let act = action.to_string();
            match tokio::runtime::Handle::try_current() {
                Ok(h) => {
                    // 运行时上下文：blocking 池执行（systemctl 可能慢，不占 worker）
                    h.spawn_blocking(move || {
                        if let Err(e) = system_power_action(&act) {
                            tracing::error!("系统动作 {act} 执行失败: {e}");
                        }
                    });
                }
                Err(_) => {
                    // 无 runtime（测试直调等）：同步兜底（旧行为）
                    if let Err(e) = system_power_action(&act) {
                        tracing::error!("系统动作 {act} 执行失败: {e}");
                    }
                }
            }
        }
        _ => {} // none：不可达（调用方已过滤）
    }
}

/// 系统电源动作（Task 46）：优先 systemctl（Linux systemd）；shutdown 另有
/// 通用 `shutdown -h now` 兜底；Windows 走原生命令。spawn 不等待。
fn system_power_action(action: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::process::Command;
        let (sub, args): (&str, &[&str]) = match action {
            "shutdown" => ("poweroff", &[]),
            "sleep" => ("suspend", &[]),
            "hibernate" => ("hibernate", &[]),
            _ => return Ok(()),
        };
        let st = Command::new("systemctl").args([sub]).args(args).status();
        match st {
            Ok(s) if s.success() => Ok(()),
            _ if action == "shutdown" => {
                // 无 systemd（容器/无权限）→ 传统 shutdown 兜底
                Command::new("shutdown")
                    .arg("-h")
                    .arg("now")
                    .status()
                    .map(|_| ())
            }
            Ok(s) => Err(std::io::Error::other(format!(
                "systemctl {sub} 退出码非 0: {s}"
            ))),
            Err(e) => Err(e),
        }
    }
    #[cfg(windows)]
    {
        use std::process::Command;
        match action {
            "shutdown" => Command::new("shutdown")
                .args(["/s", "/t", "0"])
                .status()
                .map(|_| ()),
            "sleep" | "hibernate" => Command::new("rundll32")
                .args(["powrprof.dll,SetSuspendState", "0,1,0"])
                .status()
                .map(|_| ()),
            _ => Ok(()),
        }
    }
}
