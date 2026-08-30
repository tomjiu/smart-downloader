//! NAS 版迅雷引擎（xllite / pan-cli）托管管理器（feature `nas`，Linux-only）。
//!
//! 原理（附录 E-2026-08-30 实证）：
//! - 迅雷官方 Synology 套件 `pan-xunlei-com`（down.sandai.net/nas/nasxunlei-DSM7-*.spk）
//!   内含官方 Linux 原生引擎 `xunlei-pan-cli.{ver}.{arch}`（内部代号 xllite，Go 编译），
//!   动态依赖仅 libc/libstdc++/libm/libpthread/libgcc —— 任意 x86_64/aarch64 Linux 可跑。
//! - 引擎自带平台检测（pkg/platformdetect）：检测到 docker 容器即启用 label 集
//!   [disableLauncherAuth withQrcodeLogin withHighSpeedFlowCtrl driveApiAllowLocalToken ...]，
//!   免群晖认证；首次登录走 OAuth 设备码（RFC 8628）：
//!   POST xluser-ssl.xunlei.com/v1/auth/device/code，
//!   client_id=X9ibISwpIp8jQ4Ya（docker 平台）。
//! - 控制面：DriveListen（默认 TCP 127.0.0.1:5050，gin HTTP）/ LauncherListen 5051。
//!   本模块走 TCP 反代（比 unix socket 便于多客户端与调试）。
//!
//! 已实测（本机 Debian 13 容器，非 root）：
//! - 二进制可执行、模块初始化全通、设备码请求成功拿到 device_code/user_code；
//! - 登录门在前：无有效 token/未扫码时 web 层不监听（DoLogin 阻塞等待扫码，
//!   无 TTY 会 panic —— 因此本管理器以 token 预置为主要启动路径，扫码路径
//!   需在有 TTY 的终端先完成一次）。
//! - UNTESTED：登录成功后的 API 全链路（等待真实扫码/token 后校准，
//!   对齐假设区清单 §D.3 第 5 项 vip_speedup get_info/apply/cert 形状）。
//!
//! 启动协议（自 SPK service-setup + config.init dump 反推）：
//! ```text
//! export DriveListen=127.0.0.1:5050  LauncherListen=127.0.0.1:5051
//! export ConfigPath=<data>  DownloadPATH=<downloads>  HOME=<data>/.drive
//! export GIN_MODE=release
//! ./bin/xunlei-pan-cli.<ver>.<arch> -pid <work>/engine.pid
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::Uri;
use axum::response::{IntoResponse, Response};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::state::DaemonState;

/// NAS 引擎管理器默认配置。
pub struct NasConfig {
    /// SPK 包路径（.spk = tar{package.tgz(xz)}）。为空则要求预先安装。
    pub spk_path: PathBuf,
    /// 安装/运行根目录（payload 与运行时数据都放这里）。
    pub work_dir: PathBuf,
    /// 下载目录（DownloadPATH）。
    pub download_dir: PathBuf,
    /// 引擎主 HTTP（DriveListen）。
    pub drive_listen: String,
    /// launcher HTTP（LauncherListen）。
    pub launcher_listen: String,
}

impl Default for NasConfig {
    fn default() -> Self {
        Self {
            spk_path: PathBuf::from("nasxunlei-DSM7-x86_64.spk"),
            work_dir: PathBuf::from("/var/lib/smart-dl/nas-engine"),
            download_dir: PathBuf::from("/var/lib/smart-dl/downloads"),
            drive_listen: "127.0.0.1:5050".into(),
            launcher_listen: "127.0.0.1:5051".into(),
        }
    }
}

/// NAS 引擎托管状态（daemon 全局一份）。
pub struct NasManager {
    cfg: NasConfig,
    child: Arc<Mutex<Option<u32>>>,
}

impl NasManager {
    pub fn new(cfg: NasConfig) -> Self {
        Self { cfg, child: Arc::new(Mutex::new(None)) }
    }

    /// DriveListen 地址（host:port）：探活/反代/远程引擎适配器共用同一来源，
    /// 避免硬编码默认端口在自定义部署下探活错位。
    pub fn drive_listen(&self) -> &str {
        &self.cfg.drive_listen
    }

    pub fn work_dir(&self) -> &Path {
        &self.cfg.work_dir
    }

    /// SPK 安装：tar 解包 → package.tgz(xz) 解包 → 产物定位。
    /// 使用系统 tar（零新增 crate 依赖；Linux 标配，支持 --xz 自动解压）。
    pub async fn install(&self) -> Result<NasInstallInfo, NasError> {
        let dest = self.cfg.work_dir.join("target");
        tokio::fs::create_dir_all(&dest)
            .await
            .map_err(|e| NasError::Io(format!("mkdir {}: {e}", dest.display())))?;
        tokio::fs::create_dir_all(&self.cfg.download_dir)
            .await
            .map_err(|e| NasError::Io(format!("mkdir {}: {e}", self.cfg.download_dir.display())))?;

        // 解包外层 tar（SPK 容器：package.tgz / INFO / conf / scripts ...）
        let out = Command::new("tar")
            .args([
                "-xf",
                &self.cfg.spk_path.to_string_lossy(),
                "-C",
                &dest.to_string_lossy(),
            ])
            .output()
            .await
            .map_err(|e| NasError::Io(format!("spawn tar: {e}")))?;
        if !out.status.success() {
            return Err(NasError::Install(format!(
                "tar 解包 SPK 失败: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }

        // 解包内层 package.tgz（xz 压缩 tar；tar 的 --xz/自动探测依赖 xz-utils）
        let out = Command::new("tar")
            .args([
                "-xJf",
                &dest.join("package.tgz").to_string_lossy(),
                "-C",
                &dest.to_string_lossy(),
            ])
            .output()
            .await
            .map_err(|e| NasError::Io(format!("spawn tar xJ: {e}")))?;
        if !out.status.success() {
            return Err(NasError::Install(format!(
                "tar 解包 package.tgz 失败（需要 xz-utils）: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }

        let info = Self::locate_install(&dest)?;
        tracing::info!(?info, "NAS 引擎安装完成");
        Ok(info)
    }

    /// 定位安装产物（bin/bin/version + xunlei-pan-cli*）。
    fn locate_install(dest: &Path) -> Result<NasInstallInfo, NasError> {
        let version_file = dest.join("bin/bin/version");
        let version = std::fs::read_to_string(&version_file)
            .map_err(|e| NasError::Install(format!("读 {}: {e}", version_file.display())))?
            .trim()
            .to_string();
        if version.is_empty() {
            return Err(NasError::Install(
                "version 文件为空（SPK 结构异常）".into(),
            ));
        }
        let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "amd64" };
        let launcher = dest.join(format!("bin/bin/xunlei-pan-cli-launcher.{arch}"));
        let engine = dest.join(format!("bin/bin/xunlei-pan-cli.{version}.{arch}"));
        // arm64 包文件名约定同 amd64（3.1.10 实证）
        let engine = if engine.exists() {
            engine
        } else {
            dest.join(format!("bin/bin/xunlei-pan-cli.{version}.{arch}"))
        };
        if !launcher.exists() || !engine.exists() {
            return Err(NasError::Install(format!(
                "引擎二进制缺失: launcher={} engine={}",
                launcher.display(),
                engine.display()
            )));
        }
        Ok(NasInstallInfo {
            dest: dest.to_path_buf(),
            version,
            launcher,
            engine,
        })
    }

    /// 启动引擎（前置：install 完成；登录路径：预置 token 或外部扫码）。
    ///
    /// 环境变量协议与 SPK service-setup 对齐（附录 E-2026-08-30）。
    /// PLATFORM 不注入：交由引擎自检（docker 环境 → disableLauncherAuth +
    /// withQrcodeLogin + withHighSpeedFlowCtrl 全 label 集，实测通过）。
    pub async fn start(&self) -> Result<u32, NasError> {
        let dest = self.cfg.work_dir.join("target");
        let info = Self::locate_install(&dest)?;

        let data_dir = self.cfg.work_dir.join("data");
        let home = data_dir.join(".drive");
        tokio::fs::create_dir_all(&home)
            .await
            .map_err(|e| NasError::Io(format!("mkdir .drive: {e}")))?;
        tokio::fs::create_dir_all(&self.cfg.download_dir)
            .await
            .map_err(|e| NasError::Io(format!("mkdir downloads: {e}")))?;

        let pid_file = self.cfg.work_dir.join("engine.pid");
        let log_file = std::fs::File::create(self.cfg.work_dir.join("engine.log"))
            .map_err(|e| NasError::Io(format!("create engine.log: {e}")))?;
        let log_err = log_file
            .try_clone()
            .map_err(|e| NasError::Io(format!("clone log: {e}")))?;

        // 防外层脏环境干扰（PLATFORM=lexar:xxx 实测会报 "not exist name"）
        let mut envs: HashMap<String, String> = HashMap::new();
        envs.insert("DriveListen".into(), self.cfg.drive_listen.clone());
        envs.insert("LauncherListen".into(), self.cfg.launcher_listen.clone());
        envs.insert("ConfigPath".into(), data_dir.to_string_lossy().into_owned());
        envs.insert(
            "DownloadPATH".into(),
            self.cfg.download_dir.to_string_lossy().into_owned(),
        );
        envs.insert("HOME".into(), home.to_string_lossy().into_owned());
        envs.insert("GIN_MODE".into(), "release".into());
        envs.remove("PLATFORM");

        let child = Command::new(&info.engine)
            .args(["-pid", &pid_file.to_string_lossy()])
            .env_clear()
            .envs(envs)
            .env("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(log_err))
            .spawn()
            .map_err(|e| NasError::Start(format!("启动引擎失败: {e}")))?;
        let pid = child.id().ok_or_else(|| NasError::Start("无 PID".into()))?;
        *self.child.lock().await = Some(pid);
        tracing::info!(pid, version = %info.version, "NAS 引擎已启动（等待登录/token）");
        Ok(pid)
    }

    /// 停止引擎。
    pub async fn stop(&self) -> Result<(), NasError> {
        if let Some(pid) = self.child.lock().await.take() {
            let _ = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .output()
                .await;
            tracing::info!(pid, "NAS 引擎已发送 SIGTERM");
        }
        Ok(())
    }

    /// 状态：进程存活 + HTTP 探活。
    pub async fn status(&self) -> NasStatus {
        let pid = *self.child.lock().await;
        let proc_alive = pid
            .map(|p| Path::new(&format!("/proc/{p}")).exists())
            .unwrap_or(false);
        let http = reqwest::Client::new()
            .get(format!("http://{}/", self.cfg.drive_listen))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map(|r| r.status().as_u16())
            .ok();
        NasStatus { pid, proc_alive, http_code: http }
    }
}

/// 安装产物定位结果。
#[derive(Debug, serde::Serialize)]
pub struct NasInstallInfo {
    pub dest: PathBuf,
    pub version: String,
    pub launcher: PathBuf,
    pub engine: PathBuf,
}

/// 运行状态。
#[derive(Debug, serde::Serialize)]
pub struct NasStatus {
    pub pid: Option<u32>,
    pub proc_alive: bool,
    pub http_code: Option<u16>,
}

#[derive(Debug, thiserror::Error)]
pub enum NasError {
    #[error("IO: {0}")]
    Io(String),
    #[error("安装失败: {0}")]
    Install(String),
    #[error("启动失败: {0}")]
    Start(String),
    /// token 形状/桥接类失败（假设区 #8）：与安装/启动错误可编程区分。
    #[error("token 桥接失败: {0}")]
    Token(String),
}

/// `/nas/*` → `http://DriveListen/*` 透明反代（axum wildcard 兜底）。
///
/// 与引擎控制面（gin HTTP）全兼容；web UI 静态资源由引擎直出。
/// 未启用引擎或登录门未过时表现为 502 —— 属预期（见模块 doc 注释）。
pub async fn nas_proxy(State(_state): State<Arc<DaemonState>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    // 去掉 /nas 前缀后透传
    let rest = parts.uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    let rest = rest.strip_prefix("/nas").unwrap_or(rest);
    let rest = if rest.is_empty() { "/" } else { rest };
    let target_uri: Uri = match rest.parse() {
        Ok(u) => u,
        Err(_) => return (axum::http::StatusCode::BAD_REQUEST, "bad path").into_response(),
    };
    let _ = target_uri; // 反代用完整 rest 串拼接（含 query），解析仅为校验

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes())
        .unwrap_or(reqwest::Method::GET);
    let url = format!("http://{}{rest}", manager().drive_listen());
    let mut out = client.request(method, &url);
    for (k, v) in parts.headers.iter() {
        if k == axum::http::header::HOST || k == axum::http::header::CONNECTION {
            continue;
        }
        if let Ok(name) = reqwest::header::HeaderName::from_bytes(k.as_str().as_bytes()) {
            if let Ok(val) = reqwest::header::HeaderValue::from_bytes(v.as_bytes()) {
                out = out.header(name, val);
            }
        }
    }
    if parts.method != axum::http::Method::GET && parts.method != axum::http::Method::HEAD {
        match axum::body::to_bytes(body, 64 * 1024 * 1024).await {
            Ok(bytes) => {
                out = out.body(bytes.to_vec());
            }
            Err(e) => {
                return (axum::http::StatusCode::BAD_GATEWAY, format!("read body: {e}"))
                    .into_response()
            }
        }
    }

    match out.send().await {
        Ok(resp) => {
            let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
            let mut builder = Response::builder().status(status);
            for (k, v) in resp.headers().iter() {
                if k == reqwest::header::TRANSFER_ENCODING || k == reqwest::header::CONNECTION {
                    continue;
                }
                if let (Ok(name), Ok(val)) = (
                    axum::http::HeaderName::from_bytes(k.as_str().as_bytes()),
                    axum::http::HeaderValue::from_bytes(v.as_bytes()),
                ) {
                    builder = builder.header(name, val);
                }
            }
            let bytes = resp.bytes().await.unwrap_or_default();
            match builder.body(Body::from(bytes)) {
                Ok(r) => r,
                Err(e) => (axum::http::StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
            }
        }
        // 引擎未起/登录门未过 → 502 + 引导信息
        Err(_) => (
            axum::http::StatusCode::BAD_GATEWAY,
            "NAS engine not reachable (未启动或登录门未过)。先 POST /nas/install 与 /nas/start，\
             首次登录需扫码或预置 token（见 docs/research/xunlei 附录 E）。"
                .into_response(),
        )
            .into_response(),
    }
}

/// 写 token 预置文件（登录免扫码路径）。
/// token 内容来自 L1 云登录（xluser OAuth）——格式校准列入假设区实测项。
pub async fn put_auth_token(mgr: &NasManager, token_json: &str) -> Result<PathBuf, NasError> {
    let home = mgr.cfg.work_dir.join("data/.drive");
    tokio::fs::create_dir_all(&home)
        .await
        .map_err(|e| NasError::Io(format!("mkdir .drive: {e}")))?;
    let path = home.join("auth_token.json");
    let mut f = tokio::fs::File::create(&path)
        .await
        .map_err(|e| NasError::Io(format!("create token: {e}")))?;
    f.write_all(token_json.as_bytes())
        .await
        .map_err(|e| NasError::Io(format!("write token: {e}")))?;
    Ok(path)
}

/// 统一身份层（B-3）：L1 云登录 token → xllite 引擎预置桥。
///
/// L1 侧（xunlei_auth.json）：`{access_token, refresh_token, device_id,
/// access_token_expires_at, user_id?}`；xllite 侧为 xluser OAuth 响应形
/// `{access_token, refresh_token, token_type, expires_in}`（RFC 8628 兑换产物）。
/// 字段映射为**格式假设**（假设区 #8）：引擎实际读取的字段集与文件名，以扫码
/// 实测拿到的原生 token 文件为准校准；本桥保证 L1 已登录时引擎可免扫码复用。
pub async fn sync_l1_token(l1_token_path: &Path) -> Result<PathBuf, NasError> {
    let raw = tokio::fs::read_to_string(l1_token_path)
        .await
        .map_err(|e| NasError::Io(format!("读 L1 token {}: {e}", l1_token_path.display())))?;
    let v: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| NasError::Token(format!("JSON 解析失败（{}）: {e}", l1_token_path.display())))?;
    // 诊断信息：实际存在的字段集（格式漂移时一眼定位，避免盲目猜）
    let keys = v
        .as_object()
        .map(|o| o.keys().cloned().collect::<Vec<_>>().join(","))
        .unwrap_or_default();
    let access = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            NasError::Token(format!(
                "缺 access_token 字段（keys=[{keys}]；若 L1 格式变更请对假设区 #8 校准本桥）"
            ))
        })?;
    if access.is_empty() {
        return Err(NasError::Token(format!("access_token 为空串（未登录？keys=[{keys}]）")));
    }
    // refresh_token 宽容处理：缺省不阻断桥接（登录门是否能静默续期以 A2 实测为准，
    // 引擎若要求非空 refresh 会在登录门显形——那时再回补硬校验）。
    let refresh = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    if refresh.is_empty() {
        tracing::warn!("L1 token 缺 refresh_token（keys=[{keys}]）——桥接继续，续期能力以 A2 实测为准");
    }
    // expires_at（unix 秒；兼容数字字符串形）→ expires_in（剩余秒）；缺省/异常 1h
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let expires_raw = v.get("access_token_expires_at");
    let expires_at = expires_raw
        .and_then(|x| x.as_u64())
        .or_else(|| expires_raw.and_then(|x| x.as_str()).and_then(|s| s.parse::<u64>().ok()));
    let expires_in = match expires_at {
        Some(t) if t > now => t.saturating_sub(now).max(60),
        Some(_) => {
            tracing::warn!("L1 token access_token_expires_at 已过期——按剩余 1h 处理（keys=[{keys}]）");
            3600
        }
        None => {
            tracing::warn!("L1 token access_token_expires_at 缺失/非 unix 秒——按剩余 1h 处理（keys=[{keys}]）");
            3600
        }
    };
    let bridged = serde_json::json!({
        "access_token": access,
        "refresh_token": refresh,
        "token_type": "Bearer",
        "expires_in": expires_in,
        "src": "l1-bridge",
        "src_file": l1_token_path.to_string_lossy(),
    });
    let mgr = manager();
    put_auth_token(mgr, &bridged.to_string()).await
}

/// 自检：当前平台是否支持（Linux only）。
pub const fn platform_supported() -> bool {
    cfg!(target_os = "linux")
}

/// 全局单例（daemon 生命周期一份；配置经环境变量覆盖默认值）。
static NAS_MANAGER: std::sync::OnceLock<NasManager> = std::sync::OnceLock::new();

pub fn manager() -> &'static NasManager {
    NAS_MANAGER.get_or_init(|| {
        let mut cfg = NasConfig::default();
        if let Ok(v) = std::env::var("SD_NAS_SPK") {
            cfg.spk_path = v.into();
        }
        if let Ok(v) = std::env::var("SD_NAS_WORK") {
            cfg.work_dir = v.into();
        }
        if let Ok(v) = std::env::var("SD_NAS_DOWNLOADS") {
            cfg.download_dir = v.into();
        }
        if let Ok(v) = std::env::var("SD_NAS_DRIVE_LISTEN") {
            cfg.drive_listen = v;
        }
        NasManager::new(cfg)
    })
}
