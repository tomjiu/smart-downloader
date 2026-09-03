//! serve 组装：单实例锁 → 引擎（HTTP [+BT]）→ DaemonState → HTTP/WS + BT alert 事件流 → 优雅退出。

use crate::config::Config;
use crate::http;
use crate::lockfile::InstanceLock;
use crate::state::DaemonState;
use smart_dl_provider::RemoteProvider;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("单实例锁: {0}")]
    Lock(#[from] crate::lockfile::LockError),
    #[error("引擎初始化: {0}")]
    Engine(String),
    #[error("监听 {0}: {1}")]
    Bind(String, std::io::Error),
    #[error("serve: {0}")]
    Io(#[from] std::io::Error),
}

/// 运行 daemon（阻塞至 Ctrl+C / 服务错误）。`cfg_path` 为配置文件源路径（Some 时启用
/// #6 TOML 热重载：5s 轮询变更 → 刷新可热更字段）。
pub async fn run(cfg: Config, cfg_path: Option<PathBuf>) -> Result<(), ServeError> {
    // 1. 单实例锁（重复启动立即退出）
    let _lock = InstanceLock::acquire(&cfg.lock.path)?;
    tracing::info!("单实例锁已持有: {:?}", cfg.lock.path);

    // 1b. 安全修复（V1/V13）fail-closed：非回环监听 + 未配置 http_token → 拒绝启动。
    // 防止用户把 addr 改成 0.0.0.0 后 API（写文件/NAS 执行链）对局域网裸奔。
    // token 解析：env `SMART_DL_HTTP_TOKEN` > config；`auto` → 生成强随机临时值
    // 并打印到 stdout（第六轮 9.3.5，防手设弱 token）。
    let (http_token, token_generated) = resolve_http_token(
        std::env::var("SMART_DL_HTTP_TOKEN").ok(),
        cfg.server.http_token.clone(),
    );
    if token_generated {
        println!(
            "[smart-dl] SMART_DL_HTTP_TOKEN=auto：已生成本次运行的临时 token: {}",
            http_token.as_deref().unwrap_or_default()
        );
    }
    if !Config::is_loopback_addr(&cfg.server.addr) && http_token.is_none() {
        return Err(ServeError::Engine(
            "检测到非回环监听地址但未配置 [server] http_token：API 将对网络裸奔，
            已拒绝启动。请在配置中设置 http_token（或环境变量 SMART_DL_HTTP_TOKEN），
            或改回 127.0.0.1"
                .into(),
        ));
    }

    // 2. dest_root 预检（缺失目录自动创建）；白名单 = [dest_root]（V2）
    crate::state::ensure_dest_root(
        Some(cfg.download.dest_root.to_string_lossy().into_owned()),
        std::slice::from_ref(&cfg.download.dest_root),
    )
    .map_err(|e| ServeError::Engine(format!("dest_root 预检失败: {e}")))?;

    // 3. 引擎组装：HTTP（必需）+ BT（feature bt 且配置开启）
    // 全局代理（config `[download] proxy`，启动时生效）：http/socks5/socks4，可带凭据
    // 安全修复（H-9）：connect/read 超时兜底——connect_timeout 防黑洞地址挂死，
    // read_timeout 防对端断流挂死（idle 语义，单次读超时即断）；刻意不设总超时，
    // 避免误杀正常的长耗时大文件下载。
    let mut client_builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30));
    if !cfg.download.proxy.is_empty() {
        let proxy = reqwest::Proxy::all(&cfg.download.proxy)
            .map_err(|e| ServeError::Engine(format!("代理解析失败: {e}")))?;
        // E5：proxy_auth_of 提升至 httpdl（任务级代理同源同实现），此处复用
        if let Some(auth) = smart_dl_httpdl::engine::proxy_auth_of(&cfg.download.proxy) {
            client_builder = client_builder.proxy(proxy.basic_auth(&auth.0, &auth.1));
        } else {
            client_builder = client_builder.proxy(proxy);
        }
    }
    let client = client_builder
        .build()
        .map_err(|e| ServeError::Engine(format!("HTTP client 构建失败: {e}")))?;
    let http_engine: Arc<dyn smart_dl_core::types::DownloadEngine> = Arc::new(
        smart_dl_httpdl::HttpEngine::new_limited(client, cfg.download.max_download_kb_s),
    );
    // 3b. 云兜底 provider 列表（`[provider]` 配置；仅 MockProvider 现成实现——
    // 开发/演示占位，真实 provider（迅雷云盘等）落地后按类型构造）
    let mut providers: Vec<Arc<dyn smart_dl_provider::RemoteProvider>> = Vec::new();
    if cfg.provider.enabled && cfg.provider.mock {
        providers.push(Arc::new(smart_dl_provider::MockProvider::new("mock")));
        tracing::info!("云兜底已启用（provider=mock，开发占位）");
    }
    // 3b+. 迅雷云盘 Provider 装配（XunleiProvider）。
    // 决策说明：不引入新 feature 门，仅按 `cfg.provider_xunlei.enabled` 注入——
    // smart-dl-provider 本就是 daemon 的常驻（非 optional）依赖，且现有 MockProvider
    // 也仅由配置开关控制、无 feature 门；故复用同一最少惊讶原则，避免 xunlei-import
    // （语义为「BT 任务导入 xlbt.cfg→fastresume」，且会联动拉起 bt + xunlei-convert）
    // 作为无关门控而误导。
    if cfg.provider_xunlei.enabled {
        let tp = cfg
            .provider_xunlei
            .token_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("xunlei_auth.json"));
        // 身份档位（P1-1）：env SMART_DL_XUNLEI_TIER > [provider_xunlei] tier > web。
        // 未知档拒绝启动（防错档静默运行——错档 = 服务端可见的异常指纹）。
        let tier_name = cfg.resolve_xunlei_tier_name();
        let tier = smart_dl_provider::xunlei::tier::Tier::by_name(&tier_name).ok_or_else(|| {
            ServeError::Engine(format!(
                "未知迅雷身份档位 '{tier_name}'（可用: web/nas；env SMART_DL_XUNLEI_TIER 或 [provider_xunlei] tier）"
            ))
        })?;
        let xp = smart_dl_provider::xunlei::provider::XunleiProvider::with_tier(
            "xunlei",
            tp.clone(),
            tier,
        );
        let authenticated = xp.runtime().authenticated;
        providers.push(Arc::new(xp));
        tracing::info!(
            "迅雷云盘 Provider 已装配: tier={}, token_path={:?}, authenticated={}",
            tier.name,
            tp,
            authenticated
        );
    }
    #[cfg(feature = "bt")]
    let mut state = DaemonState::new(http_engine, providers)
        .with_dest_root(cfg.download.dest_root.clone())
        .with_http_token(http_token.clone())
        .with_disk_precheck_strict(cfg.download.disk_precheck_strict)
        .with_global_limits(cfg.download.max_download_kb_s, cfg.bt.max_upload_kb_s)
        .with_webhook_url((!cfg.webhook.url.is_empty()).then(|| cfg.webhook.url.clone()))
        .with_cleanup(cfg.cleanup.clone());
    #[cfg(not(feature = "bt"))]
    let mut state = DaemonState::new(http_engine, providers)
        .with_dest_root(cfg.download.dest_root.clone())
        .with_http_token(http_token.clone())
        .with_disk_precheck_strict(cfg.download.disk_precheck_strict)
        .with_global_limits(cfg.download.max_download_kb_s, cfg.bt.max_upload_kb_s)
        .with_webhook_url((!cfg.webhook.url.is_empty()).then(|| cfg.webhook.url.clone()))
        .with_cleanup(cfg.cleanup.clone());

    // 4. BT 引擎（先取 core 句柄，供 alert 事件流）
    #[cfg(feature = "bt")]
    let mut bt_typed: Option<Arc<crate::bt::BtEngine>> = None;
    #[cfg(feature = "bt")]
    let bt_core = {
        if cfg.bt.enabled {
            let save = cfg.bt_save_path();
            std::fs::create_dir_all(&save)
                .map_err(|e| ServeError::Engine(format!("BT 落盘目录创建失败 {save:?}: {e}")))?;
            let bt = Arc::new(
                crate::bt::BtEngine::new(
                    &save,
                    Some(cfg.download.proxy.as_str()),
                    cfg.download.max_download_kb_s,
                    cfg.bt.max_upload_kb_s,
                    cfg.bt.enable_dht,
                    cfg.bt.enable_lsd,
                    cfg.bt.enable_upnp,
                )
                .map_err(ServeError::Engine)?,
            );
            let core = bt.core(); // Arc<BtCore>：alert 轮询句柄（trait 化前保存）
            bt_typed = Some(bt.clone()); // Bug A：alert 循环的暂停意图压制句柄
            let bt_arc: Arc<dyn smart_dl_core::types::DownloadEngine> = bt.clone();
            state = state.with_bt(bt_arc);
            tracing::info!("BT 引擎已启用, 落盘: {save:?}");
            Some(core)
        } else {
            None
        }
    };
    #[cfg(not(feature = "bt"))]
    if cfg.bt.enabled {
        tracing::warn!("配置启用了 bt 但编译未带 --features bt，BT 不可用");
    }

    // 4c. 迅雷 SDK 引擎（Windows-only，免登录匿名 + 可选带身份模式；与 BT 共用 EngineKind::Bt）
    #[cfg(feature = "xunlei")]
    if cfg.xunlei.enabled {
        let save = cfg.xunlei_save_path();
        std::fs::create_dir_all(&save)
            .map_err(|e| ServeError::Engine(format!("Xunlei 落盘目录创建失败 {save:?}: {e}")))?;
        let sdk_dir = cfg.xunlei.sdk_dir.clone().unwrap_or_else(|| {
            // 默认尝试常见安装路径
            PathBuf::from(r"C:\Program Files\Thunder Network\ThunderSDK")
        });
        // 若同配置启用了 xunlei 云盘 Provider，取 user_id 注入 SDK（带身份模式）。
        // cert 来源尚未完全澄清（speed.auth.vip.xunlei.com 下发流程未知），暂不注入。
        let xunlei_user_id = if cfg.provider_xunlei.enabled {
            let tp = cfg
                .provider_xunlei
                .token_path
                .clone()
                .unwrap_or_else(|| PathBuf::from("xunlei_auth.json"));
            smart_dl_provider::xunlei::auth::load(&tp)
                .ok()
                .flatten()
                .map(|s| s.user_id)
        } else {
            None
        };
        let mut xl_builder = smart_dl_btcore::XunleiBtEngine::builder(&sdk_dir, &save);
        if let Some(ref uid) = xunlei_user_id {
            xl_builder = xl_builder.with_user_id(uid);
        }
        let xl = xl_builder
            .build()
            .await
            .map_err(|e| ServeError::Engine(format!("Xunlei 引擎初始化失败: {e}")))?;
        let xl_arc: Arc<dyn smart_dl_core::types::DownloadEngine> = Arc::new(xl);
        state = state.with_bt(xl_arc);
        tracing::info!("Xunlei 引擎已启用, sdk: {sdk_dir:?}, 落盘: {save:?}");
    }
    #[cfg(not(feature = "xunlei"))]
    if cfg.xunlei.enabled {
        tracing::warn!("配置启用了 xunlei 但编译未带 --features xunlei，Xunlei 不可用");
    }

    // 4d. FTP 引擎（feature `ftp`；与 HTTP 共用默认 dest_root + 全局限速总阀门）
    #[cfg(feature = "ftp")]
    {
        let ftp_engine: Arc<dyn smart_dl_core::types::DownloadEngine> = Arc::new(
            smart_dl_httpdl::FtpEngine::new_limited(cfg.download.max_download_kb_s),
        );
        state = state.with_ftp(ftp_engine);
        tracing::info!("FTP 引擎已启用");
    }

    // 4e. NAS 引擎身份桥（feature `nas`，B-3 统一身份层）：L1 登录态存在时
    // 自动同步为 xllite 引擎预置 token（免扫码启动；格式校准=假设区 #8）。
    #[cfg(feature = "nas")]
    {
        let l1_path =
            std::env::var("SD_L1_TOKEN").unwrap_or_else(|_| "xunlei_auth.json".to_string());
        let l1 = std::path::PathBuf::from(&l1_path);
        if l1.exists() {
            match crate::nas::sync_l1_token(&l1).await {
                Ok(p) => tracing::info!("L1→xllite 身份桥：token 已预置至 {}", p.display()),
                Err(e) => tracing::warn!("L1→xllite 身份桥跳过：{e}"),
            }
        } else {
            tracing::info!(
                "NAS 身份桥：L1 token 不存在（{l1_path}），引擎首次启动需扫码或 /nas/token 投喂"
            );
        }
    }

    // 4b. 任务持久化 + 启动恢复（须在引擎表就绪后：恢复会重新 add）
    let tasks_path = cfg.storage.tasks_path.clone();
    if let Some(parent) = tasks_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ServeError::Engine(format!("任务目录创建失败 {parent:?}: {e}")))?;
        }
    }
    state = state.with_storage(tasks_path.clone());
    // 4b+. 注入生效配置快照（GET /config 返回；热重载后由 refresh_config 刷新）
    state = state.with_config(Config::snapshot_json(&cfg, &tasks_path));
    if tasks_path.exists() {
        match state.restore_from(&tasks_path).await {
            Ok(n) => tracing::info!("已从 {tasks_path:?} 恢复 {n} 个任务"),
            Err(e) => tracing::warn!("任务恢复失败（继续空启动）: {e}"),
        }
    }

    // 4c. BT alert 事件流（feature bt 且 BT 启用时）
    #[cfg(feature = "bt")]
    let (alert_handle, state_arc) = {
        let state_arc = Arc::new(state);
        let handle = bt_core.map(|core| {
            crate::bt_events::spawn_alert_loop(
                state_arc.clone(),
                core,
                Duration::from_millis(500),
                bt_typed.clone(),
            )
        });
        (handle, state_arc)
    };
    #[cfg(not(feature = "bt"))]
    let state_arc = Arc::new(state);

    // 4c+. HTTP 状态推进循环（list 状态准确化）：2s 轮询引擎终态 → 记录推进 +
    // 事件广播（v1 HTTP 引擎无 alert 回调，记录 state 此前停留 Queued）
    let _http_events_handle =
        crate::http_events::spawn_http_events(state_arc.clone(), Duration::from_secs(2));

    // 4c++. 子文件优先级重放循环（P1-3）：magnet 恢复任务 metadata 就绪后
    // 延迟下发持久化的 file_priorities（pending 集合收敛；非 bt 构建无 BT 任务
    // 挂起，空转无害——但仍按 feature 门控避免无谓任务）
    #[cfg(feature = "bt")]
    let _prio_replay_handle = crate::bt_events::spawn_file_priority_replay_loop(
        state_arc.clone(),
        Duration::from_secs(2),
    );

    // 4c+++ 周期 fastresume 保存（P4 G4）：crash/断电时进度凭据最多丢一个
    // 间隔（5min），不再依赖 pause/remove 两个显式时机。逐任务 spawn_blocking
    //（save_resume_now 同步轮询 alert ≤3s），顺序保存避免 alert 消费竞态放大。
    #[cfg(feature = "bt")]
    if let Some(bt) = bt_typed.clone() {
        let st = state_arc.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(300)).await;
                for tid in st.active_bt_tids() {
                    let b = bt.clone();
                    let t = tid.clone();
                    let _ = tokio::task::spawn_blocking(move || b.save_resume_now(&t)).await;
                }
            }
        });
    }

    // 4d-. 已完成任务自动清扫循环（E20）：10min 周期扫描（配置 0 = 空转
    // no-op）；配置随 TOML 热重载生效，循环每轮读取当前生效值。
    {
        let st = state_arc.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(600)).await;
                let _ = st.sweep_completed_cleanup().await;
            }
        });
    }

    // 4d. #6 TOML 热重载：5s 轮询配置文件内容变更 → 解析 → refresh_config
    // （默认落盘目录 + /config 快照刷新；解析失败保留旧配置并告警）。
    if let Some(path) = cfg_path {
        let st = state_arc.clone();
        let tasks = tasks_path.clone();
        let mut last = std::fs::read_to_string(&path).ok();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue; // 文件暂不可读（被编辑器暂存等）：下轮再试
                };
                if last.as_deref() == Some(text.as_str()) {
                    continue;
                }
                match toml::from_str::<Config>(&text) {
                    Ok(new_cfg) => {
                        st.refresh_config(&new_cfg, &tasks);
                        // E16：限速总阀门随热重载生效（文件为准——与 dest_root
                        // 同口径）；值无变化时 apply 内部 no-op，无事件噪声。
                        if let Err(e) = st
                            .apply_global_limits(
                                Some(new_cfg.download.max_download_kb_s),
                                Some(new_cfg.bt.max_upload_kb_s),
                            )
                            .await
                        {
                            tracing::warn!("配置热重载限速下发失败（保留引擎侧旧值）: {e}");
                        }
                        tracing::info!("配置热重载生效: {}", path.display());
                        last = Some(text);
                    }
                    Err(e) => {
                        tracing::warn!("配置热重载解析失败（保留旧配置）: {e}");
                        last = Some(text); // 避免同一坏内容反复告警
                    }
                }
            }
        });
    }

    // 5. 路由 + 监听
    // S2：清扫上次运行遗留的 magnet 抓取 scratch（kill -9/断电残骸，best-effort；
    // PID+mtime 双重保护，活跃抓取与并发实例不受影响）。
    http::cleanup_stale_magnet_scratch();
    let app = http::router(state_arc.clone());
    let listener = tokio::net::TcpListener::bind(&cfg.server.addr)
        .await
        .map_err(|e| ServeError::Bind(cfg.server.addr.clone(), e))?;
    tracing::info!("smart-dl-daemon 监听 {}", cfg.server.addr);

    // 6. 优雅退出（Ctrl+C → 停服 → alert loop 随进程结束）
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = tx.send(());
    });

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = rx.await;
            tracing::info!("收到退出信号，优雅关闭…");
        })
        .await
        .map_err(ServeError::Io)?;

    #[cfg(feature = "bt")]
    if let Some(h) = alert_handle {
        h.abort(); // 进程退出前停止 alert 轮询（锁随 _lock drop 释放）
    }

    // G4：优雅退出前保存活跃 BT 任务 fastresume（crash 时凭据尽可能新）。
    // alert 循环已 abort → save 内部 pop_alerts 无消费竞态；同步直调阻塞
    // 主线程 ≤3s/任务——进程正在退出，可接受。逐任务 best-effort，失败不阻断退出。
    #[cfg(feature = "bt")]
    if let Some(bt) = &bt_typed {
        for tid in state_arc.active_bt_tids() {
            let _ = bt.save_resume_now(&tid);
        }
    }

    Ok(())
}

/// 解析生效 HTTP token（第六轮 9.3.5）：env `SMART_DL_HTTP_TOKEN`（空串视为未设）
/// > config `[server] http_token`；值为 `auto` → 生成强随机临时 token
/// > （uuid v4，122 位随机，getrandom 支撑），`generated=true` 由调用方负责打印。
fn resolve_http_token(env_val: Option<String>, cfg_val: Option<String>) -> (Option<String>, bool) {
    let raw = env_val.filter(|t| !t.is_empty()).or(cfg_val);
    match raw.as_deref() {
        Some("auto") => (Some(uuid::Uuid::new_v4().simple().to_string()), true),
        _ => (raw, false),
    }
}

/// 进程参数：`serve [--config <path>]`。
pub fn parse_args(args: &[String]) -> Result<Option<std::path::PathBuf>, String> {
    let mut cfg_path = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" | "-c" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--config 缺少路径".to_string())?;
                cfg_path = Some(std::path::PathBuf::from(v));
                i += 2;
            }
            a if a.starts_with('-') => return Err(format!("未知参数: {a}")),
            _ => i += 1,
        }
    }
    Ok(cfg_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_http_token_env_priority_and_auto() {
        // env 优先于 config
        let (t, gen) = resolve_http_token(Some("env-tok".into()), Some("cfg-tok".into()));
        assert_eq!(t.as_deref(), Some("env-tok"));
        assert!(!gen);
        // env 空串视为未设 → 回落 config
        let (t, gen) = resolve_http_token(Some(String::new()), Some("cfg-tok".into()));
        assert_eq!(t.as_deref(), Some("cfg-tok"));
        assert!(!gen);
        // auto → 生成强随机值（非 "auto" 字面量），generated 标记
        let (t1, gen) = resolve_http_token(Some("auto".into()), None);
        assert!(gen);
        assert_ne!(t1.as_deref(), Some("auto"));
        assert_eq!(t1.as_deref().map(str::len), Some(32)); // uuid simple = 32 hex
                                                           // 两次生成互不相同
        let (t2, _) = resolve_http_token(Some("auto".into()), None);
        assert_ne!(t1, t2);
        // config 值为 auto 同样触发生成
        let (_, gen) = resolve_http_token(None, Some("auto".into()));
        assert!(gen);
        // 双 None → 无 token（回环兼容模式）
        let (t, gen) = resolve_http_token(None, None);
        assert!(t.is_none());
        assert!(!gen);
    }

    #[test]
    fn parse_config_flag() {
        let p = parse_args(&["serve".into(), "--config".into(), "x.toml".into()]).unwrap();
        assert_eq!(p, Some(std::path::PathBuf::from("x.toml")));
    }

    #[test]
    fn parse_short_flag() {
        let p = parse_args(&["-c".into(), "y.toml".into()]).unwrap();
        assert_eq!(p, Some(std::path::PathBuf::from("y.toml")));
    }

    #[test]
    fn parse_default_no_flag() {
        assert_eq!(parse_args(&["serve".into()]).unwrap(), None);
    }

    #[test]
    fn parse_unknown_flag_errors() {
        assert!(parse_args(&["--bogus".into()]).is_err());
    }

    #[test]
    fn parse_missing_value_errors() {
        assert!(parse_args(&["--config".into()]).is_err());
    }
}
