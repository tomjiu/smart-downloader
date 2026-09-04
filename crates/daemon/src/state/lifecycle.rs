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
