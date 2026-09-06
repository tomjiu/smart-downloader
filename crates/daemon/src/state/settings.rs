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
//! 持久化：`PUT /settings?persist=true`（默认）在权威配置（live_config）上打
//! 补丁后原子回写 `--config` 文件（tmp+rename）。未指定 `--config` 时仅运行
//! 时生效（report.persisted=false）。热重载循环按文本变更检测（文件为准），
//! 与本路径天然协同：回写后 5s 内热重载读取同值，幂等 no-op。
//!
//! 事件：`settings_changed { keys }`（应用键全集）；限速变更另发
//! `global_limits_changed`（E16 既有语义，UI 曲线联动）。

use super::*;
use smart_dl_core::types::BtSessionPatch;

/// `PUT /settings` 请求体：各域可选，域内字段可选（None = 不调整）。
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct SettingsReq {
    pub bandwidth: Option<BandwidthReq>,
    pub connection: Option<ConnectionReq>,
    pub bittorrent: Option<BtSessionReq>,
    pub download: Option<DownloadReq>,
    pub cleanup: Option<CleanupReq>,
    pub post_download: Option<PostDownloadReq>,
    pub webhook: Option<WebhookReq>,
    pub scheduler: Option<SchedulerReq>,
    pub queue: Option<QueueReq>,
    /// 持久化开关（默认 true）；与 query `?persist=` 等价（body 优先）。
    pub persist: Option<bool>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
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

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct ConnectionReq {
    /// 全局代理 URL；空串 = 清除代理（直连）。
    pub proxy: Option<String>,
    pub bt_listen_port: Option<u16>,
    pub bt_max_connections: Option<u32>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct BtSessionReq {
    pub enable_dht: Option<bool>,
    pub enable_lsd: Option<bool>,
    pub enable_upnp: Option<bool>,
    pub enable_pex: Option<bool>,
    pub enable_utp: Option<bool>,
    pub encrypt: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct DownloadReq {
    pub dest_root: Option<String>,
    pub disk_precheck_strict: Option<bool>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct CleanupReq {
    pub auto_remove_completed_days: Option<u32>,
    pub auto_remove_keep_data: Option<bool>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct PostDownloadReq {
    pub move_to: Option<String>,
    pub hook: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct WebhookReq {
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct SchedulerReq {
    pub start_jitter_seconds: Option<u32>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct QueueReq {
    pub max_active_bt: Option<u32>,
    pub max_active_http: Option<u32>,
    pub max_active_ftp: Option<u32>,
}

/// `PUT /settings` 响应：应用结果分类 + 当前生效限速。
#[derive(Debug, Clone, serde::Serialize)]
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

impl DaemonState {
    /// 全量设置快照（`GET /settings`）：权威配置（live_config）为底，运行时
    /// 可变值（限速/备用/队列/清理等）以 state 当前值为准——两者在
    /// apply_settings/refresh_config 维护下保持同步。
    pub fn settings_snapshot(&self) -> serde_json::Value {
        let cfg = self.live_config.lock().clone().unwrap_or_default();
        let base = *self.base_limits.lock();
        let alt = self.alt_cfg.lock().clone();
        let effective = *self.global_limits.lock();
        let queue = self.queue_cfg.lock().clone();
        serde_json::json!({
            "bandwidth": {
                // 基准限速（用户语义值）；引擎实际值见 effective_*
                "max_download_kb_s": base.max_download_kb_s,
                "max_upload_kb_s": base.max_upload_kb_s,
                "alt_enabled": alt.alt_enabled,
                "alt_max_download_kb_s": alt.alt_max_download_kb_s,
                "alt_max_upload_kb_s": alt.alt_max_upload_kb_s,
                "alt_from": alt.alt_from,
                "alt_to": alt.alt_to,
                "alt_days": alt.alt_days,
                // 备用窗口当前是否命中（只读展示）
                "alt_active_now": self
                    .alt_active
                    .load(std::sync::atomic::Ordering::Relaxed),
                "effective_max_download_kb_s": effective.max_download_kb_s,
                "effective_max_upload_kb_s": effective.max_upload_kb_s,
            },
            "connection": {
                "proxy": cfg.download.proxy,
                "bt_listen_port": cfg.bt.listen_port,
                "bt_max_connections": cfg.bt.max_connections,
            },
            "bittorrent": {
                "enable_dht": cfg.bt.enable_dht,
                "enable_lsd": cfg.bt.enable_lsd,
                "enable_upnp": cfg.bt.enable_upnp,
                "enable_pex": cfg.bt.enable_pex,
                "enable_utp": cfg.bt.enable_utp,
                "encrypt": cfg.bt.encrypt,
                "bt_available": self.engines.contains_key(&EngineKind::Bt),
            },
            "download": {
                "dest_root": self.default_dest_root.lock().display().to_string(),
                "disk_precheck_strict": self.disk_precheck_strict,
            },
            "cleanup": {
                "auto_remove_completed_days": self.cleanup.lock().auto_remove_completed_days,
                "auto_remove_keep_data": self.cleanup.lock().auto_remove_keep_data,
            },
            "post_download": {
                "move_to": self
                    .post_move_to
                    .lock()
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                "hook": self.post_hook.lock().clone().unwrap_or_default(),
            },
            "webhook": {
                "url": self.webhook_url.lock().clone().unwrap_or_default(),
            },
            "scheduler": {
                "start_jitter_seconds": self
                    .start_jitter_secs
                    .load(std::sync::atomic::Ordering::Relaxed),
            },
            "queue": {
                "max_active_bt": queue.max_active_bt,
                "max_active_http": queue.max_active_http,
                "max_active_ftp": queue.max_active_ftp,
            },
            "meta": {
                // 持久化目标；None = 未指定 --config（PUT 仅运行时生效）
                "persist_path": (*self.config_path.lock())
                    .as_ref()
                    .map(|p| p.display().to_string()),
            },
        })
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
                || bt.encrypt.is_some();
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
                }
                for k in [
                    "enable_dht",
                    "enable_lsd",
                    "enable_upnp",
                    "enable_pex",
                    "enable_utp",
                    "encrypt",
                ] {
                    applied.push(format!("bittorrent.{k}"));
                }
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
            *self.queue_cfg.lock() = cur.clone();
            if let Some(cfg) = self.live_config.lock().as_mut() {
                cfg.queue = cur;
            }
            for k in ["max_active_bt", "max_active_http", "max_active_ftp"] {
                applied.push(format!("queue.{k}"));
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
        if let Some(bt) = self.engines.get(&EngineKind::Bt).cloned() {
            match bt.apply_bt_session(patch.clone()).await {
                Ok(()) => {}
                Err(EngineError::Unsupported) => {}
                Err(e) => tracing::warn!("BT 会话设置下发失败（保留旧值）: {e}"),
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
        for kind in [EngineKind::Ftp, EngineKind::Http] {
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
}
