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

    // 2. dest_root 预检（缺失目录自动创建）
    crate::state::ensure_dest_root(Some(cfg.download.dest_root.to_string_lossy().into_owned()))
        .map_err(|e| ServeError::Engine(format!("dest_root 预检失败: {e}")))?;

    // 3. 引擎组装：HTTP（必需）+ BT（feature bt 且配置开启）
    // 全局代理（config `[download] proxy`，启动时生效）：http/socks5/socks4，可带凭据
    let mut client_builder = reqwest::Client::builder();
    if !cfg.download.proxy.is_empty() {
        let proxy = reqwest::Proxy::all(&cfg.download.proxy)
            .map_err(|e| ServeError::Engine(format!("代理解析失败: {e}")))?;
        if let Some(auth) = proxy_auth_of(&cfg.download.proxy) {
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
        // BUGB-INSTR（临时诊断装配，修复验证后移除）：env SMART_DL_MOCK_URL 注入
        // mock 直链文件，使兜底链在无真实云端配额时也能走完整传输路径。
        let mut mp = smart_dl_provider::MockProvider::new("mock");
        if let Ok(url) = std::env::var("SMART_DL_MOCK_URL") {
            if !url.is_empty() {
                let rel = std::env::var("SMART_DL_MOCK_NAME").unwrap_or_else(|_| "mockfile.bin".into());
                let size = std::env::var("SMART_DL_MOCK_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
                mp = mp.with_files(vec![smart_dl_provider::ResolvedRemoteFile {
                    rel_path: rel,
                    url: url.clone(),
                    size,
                    etag: None,
                    expires_at: None,
                }]);
            }
        }
        // BUGB-INSTR（临时诊断装配，修复验证后移除）：SMARTDL_MOCK_READY_DELAY_SECS
        // 延迟 Ready——submit 后 N 秒内 status=Downloading，模拟真实云盘「离线
        // 数分钟」等待窗（免配额复现 Bug B 协调器状态窗）；缺省 0 = 旧行为。
        if let Ok(secs) = std::env::var("SMARTDL_MOCK_READY_DELAY_SECS") {
            let secs = secs.trim().parse::<u64>().unwrap_or(0);
            if secs > 0 {
                mp = mp.with_ready_delay_secs(secs);
            }
        }
        providers.push(Arc::new(mp));
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
        let xp = smart_dl_provider::xunlei::provider::XunleiProvider::new("xunlei", tp.clone());
        let authenticated = xp.runtime().authenticated;
        providers.push(Arc::new(xp));
        tracing::info!(
            "迅雷云盘 Provider 已装配: token_path={:?}, authenticated={}",
            tp,
            authenticated
        );
    }
    #[cfg(feature = "bt")]
    let mut state =
        DaemonState::new(http_engine, providers).with_dest_root(cfg.download.dest_root.clone());
    #[cfg(not(feature = "bt"))]
    let mut state =
        DaemonState::new(http_engine, providers).with_dest_root(cfg.download.dest_root.clone());

    // 4. BT 引擎（先取 core 句柄，供 alert 事件流）
    #[cfg(feature = "bt")]
    let mut bt_typed: Option<Arc<crate::bt::BtEngine>> = None;
    #[cfg(feature = "bt")]
    let bt_core = {
        if cfg.bt.enabled {
            let save = cfg.bt_save_path();
            std::fs::create_dir_all(&save)
                .map_err(|e| ServeError::Engine(format!("BT 落盘目录创建失败 {save:?}: {e}")))?;
            let bt = Arc::new(crate::bt::BtEngine::new(
                &save,
                Some(cfg.download.proxy.as_str()),
                cfg.download.max_download_kb_s,
                cfg.bt.max_upload_kb_s,
                cfg.bt.enable_dht,
                cfg.bt.enable_lsd,
                cfg.bt.enable_upnp,
            )
            .map_err(ServeError::Engine)?);
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

    // 4d. FTP 引擎（feature `ftp`；与 HTTP 共用默认 dest_root）
    #[cfg(feature = "ftp")]
    {
        let ftp_engine: Arc<dyn smart_dl_core::types::DownloadEngine> =
            Arc::new(smart_dl_httpdl::FtpEngine::new());
        state = state.with_ftp(ftp_engine);
        tracing::info!("FTP 引擎已启用");
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

    // BUGB-INSTR（临时诊断，修复后移除）：纯 sleep 心跳——若此日志停更，
    // 说明 tokio runtime 已无可调度 worker（全被同步阻塞调用钉死）。
    {
        let t0 = std::time::Instant::now();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                tracing::info!("[bugb] watchdog alive uptime_ms={}", t0.elapsed().as_millis());
            }
        });
    }

    // BUGB-INSTR（临时诊断，修复验证后移除）：OS 线程心跳 + tasks 锁探测——
    // 与 tokio watchdog 互为对照：os-tick 停更 = 进程级冻结；os-tick 活而
    // tasks_free=false = tasks Mutex 被长期持有且 tokio worker 全数饿死。
    {
        let st2 = state_arc.clone();
        let t0 = std::time::Instant::now();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(300));
            tracing::debug!(
                "[bugb] os-tick alive_ms={} tasks_free={}",
                t0.elapsed().as_millis(),
                st2.debug_try_lock_tasks()
            );
        });
    }

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

    Ok(())
}

/// 从代理 URL 提取 `user:pass@`（HTTP 引擎 reqwest basic_auth 用；BT 引擎由
/// btcore::parse_proxy 解析同一格式）。无凭据 → None。
fn proxy_auth_of(url: &str) -> Option<(String, String)> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let (auth, _) = rest.rsplit_once('@')?;
    let (u, p) = auth.split_once(':').unwrap_or((auth, ""));
    Some((u.to_string(), p.to_string()))
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
