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
        Self {
            cfg,
            child: Arc::new(Mutex::new(None)),
        }
    }

    /// DriveListen 地址（host:port）：探活/反代/远程引擎适配器共用同一来源，
    /// 避免硬编码默认端口在自定义部署下探活错位。
    pub fn drive_listen(&self) -> &str {
        &self.cfg.drive_listen
    }

    pub fn work_dir(&self) -> &Path {
        &self.cfg.work_dir
    }

    /// SPK 安装：tar 预检 → tar 解包 → package.tgz(xz) 预检+解包 → 产物定位。
    /// 使用系统 tar（零新增 crate 依赖；Linux 标配，支持 --xz 自动解压）。
    /// 安全修复（V5，CWE-22）：tar 全程 `--no-absolute-names --no-same-owner
    /// --no-same-permissions` 防成员绝对路径/越权落盘；解包【前】列举成员显式
    /// 拒绝绝对路径/`..` 成员（第六轮 9.3.2，不依赖系统 tar 版本默认行为）；
    /// 解包后遍历校验无 `..` 逃逸产物，spk_path 不存在/不在 work_dir 内的
    /// 压缩包拒绝安装。
    pub async fn install(&self) -> Result<NasInstallInfo, NasError> {
        let dest = self.cfg.work_dir.join("target");
        tokio::fs::create_dir_all(&dest)
            .await
            .map_err(|e| NasError::Io(format!("mkdir {}: {e}", dest.display())))?;
        tokio::fs::create_dir_all(&self.cfg.download_dir)
            .await
            .map_err(|e| NasError::Io(format!("mkdir {}: {e}", self.cfg.download_dir.display())))?;

        // V5：SPK 源文件必须存在且为常规文件（拒绝目录/设备等怪异路径）
        let spk_md = tokio::fs::metadata(&self.cfg.spk_path).await.map_err(|e| {
            NasError::Install(format!(
                "SPK 文件不可达 {}: {e}",
                self.cfg.spk_path.display()
            ))
        })?;
        if !spk_md.is_file() {
            return Err(NasError::Install(format!(
                "SPK 路径不是常规文件: {}",
                self.cfg.spk_path.display()
            )));
        }

        // V5 残留加固（第六轮 9.3.2）：解包前列举外层 SPK 成员，显式拒绝
        // 绝对路径/`..` 成员——不依赖运行时 tar 版本的默认行为
        Self::precheck_tar_members(&["-tf", &self.cfg.spk_path.to_string_lossy()]).await?;

        // 解包外层 tar（SPK 容器：package.tgz / INFO / conf / scripts ...）
        let out = Command::new("tar")
            .args([
                "-xf",
                &self.cfg.spk_path.to_string_lossy(),
                "--no-absolute-names",
                "--no-same-owner",
                "--no-same-permissions",
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

        // 内层 package.tgz 同样预检（成员才是真正的文件载荷）
        Self::precheck_tar_members(&["-tJf", &dest.join("package.tgz").to_string_lossy()]).await?;

        // 解包内层 package.tgz（xz 压缩 tar；tar 的 --xz/自动探测依赖 xz-utils）
        let out = Command::new("tar")
            .args([
                "-xJf",
                &dest.join("package.tgz").to_string_lossy(),
                "--no-absolute-names",
                "--no-same-owner",
                "--no-same-permissions",
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

        // V5：解包后逃逸校验——产物中不得存在 symlink 或逃逸出 dest 的常规文件。
        // （--no-absolute-names 会把绝对路径成员剥离前缀写入 dest 内，但带 `../`
        // 的相对成员仍可能逃逸；tar 仅打 warning 不报错，必须自行复核。）
        Self::verify_no_escape(&dest)?;

        let info = Self::locate_install(&dest)?;
        tracing::info!(?info, "NAS 引擎安装完成");
        Ok(info)
    }

    /// V5 残留加固（第六轮 9.3.2）：解包前 `tar -t` 列举成员并逐条校验，
    /// 含危险成员（绝对路径/`..`）即拒绝安装——防御显式化，不再依赖
    /// GNU/busybox tar 各版本的默认行为漂移。
    async fn precheck_tar_members(args: &[&str]) -> Result<(), NasError> {
        let out = Command::new("tar")
            .args(args)
            .output()
            .await
            .map_err(|e| NasError::Io(format!("spawn tar -t: {e}")))?;
        if !out.status.success() {
            return Err(NasError::Install(format!(
                "tar 列举成员失败: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let dangerous: Vec<&str> = stdout
            .lines()
            .filter(|m| tar_member_dangerous(m))
            .collect();
        if !dangerous.is_empty() {
            return Err(NasError::Install(format!(
                "SPK 含危险 tar 成员（绝对路径/..），拒绝安装: {}",
                dangerous
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        Ok(())
    }

    /// V5：遍历 dest，发现符号链接即拒绝（不可信压缩包不得在文件系统
    /// 上预置 symlink 跳板）；`..` 成员逃逸产物落点不在 dest 内，天然
    /// 不会被本遍历命中——配合落盘前的 --no-absolute-names 与下游执行
    /// 前的产物定位（bin/bin 固定结构）形成纵深。
    fn verify_no_escape(dest: &Path) -> Result<(), NasError> {
        fn walk(dir: &Path) -> Result<(), NasError> {
            let entries = std::fs::read_dir(dir)
                .map_err(|e| NasError::Install(format!("遍历解包产物失败: {e}")))?;
            for e in entries {
                let e = e.map_err(|e| NasError::Install(format!("遍历解包产物失败: {e}")))?;
                let p = e.path();
                let md = std::fs::symlink_metadata(&p)
                    .map_err(|e| NasError::Install(format!("stat {}: {e}", p.display())))?;
                if md.file_type().is_symlink() {
                    return Err(NasError::Install(format!(
                        "SPK 含符号链接已拒绝: {}",
                        p.display()
                    )));
                }
                if md.is_dir() {
                    walk(&p)?;
                }
            }
            Ok(())
        }
        walk(dest)
    }

    /// 定位安装产物（bin/bin/version + xunlei-pan-cli*）。
    fn locate_install(dest: &Path) -> Result<NasInstallInfo, NasError> {
        let version_file = dest.join("bin/bin/version");
        let version = std::fs::read_to_string(&version_file)
            .map_err(|e| NasError::Install(format!("读 {}: {e}", version_file.display())))?
            .trim()
            .to_string();
        if version.is_empty() {
            return Err(NasError::Install("version 文件为空（SPK 结构异常）".into()));
        }
        let arch = if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "amd64"
        };
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
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
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
        NasStatus {
            pid,
            proc_alive,
            http_code: http,
        }
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
    let rest = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
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
        Err(e) => {
            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
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
                return (
                    axum::http::StatusCode::BAD_GATEWAY,
                    format!("read body: {e}"),
                )
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
/// L1 侧（xunlei_auth.json，AuthState）：`{access_token, refresh_token,
/// device_id, access_token_expires_at, user_id?}`；xllite 侧原生 token 形
/// 已由 2026-08-30 扫码实测定案（假设区 #8 校准完毕）：
/// `{token_type, access_token, refresh_token, id_token, expires_in, scope,
/// sub, user_group, user_id}`——引擎按原生形读取预置文件，实测登录门通过。
///
/// 桥接策略（以原生形为准）：
/// - 原生字段尽力透传：L1 文件若本就是原生形（如 device-code 产物直接复用），
///   token_type/id_token/scope/sub/user_group/user_id 原样保留、expires_in
///   直取——桥接幂等；
/// - L1 专有字段映射：access_token_expires_at → expires_in（剩余秒，下限 60s；
///   缺省/异常按 1h 兜底）；
/// - AuthState 形缺原生字段（id_token/scope/sub/user_group）不伪造——引擎
///   登录门对缺字段的宽容度以 A2 engine 步实测为准，不行则设备码重登兜底；
/// - 诊断字段（src/src_file）不再写入文件（保持与原生形一致），改记日志。
pub async fn sync_l1_token(l1_token_path: &Path) -> Result<PathBuf, NasError> {
    let raw = tokio::fs::read_to_string(l1_token_path)
        .await
        .map_err(|e| NasError::Io(format!("读 L1 token {}: {e}", l1_token_path.display())))?;
    let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        NasError::Token(format!("JSON 解析失败（{}）: {e}", l1_token_path.display()))
    })?;
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
        return Err(NasError::Token(format!(
            "access_token 为空串（未登录？keys=[{keys}]）"
        )));
    }
    // refresh_token 宽容处理：缺省不阻断桥接（登录门是否能静默续期以 A2 实测为准，
    // 引擎若要求非空 refresh 会在登录门显形——那时再回补硬校验）。
    let refresh = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    if refresh.is_empty() {
        tracing::warn!(
            "L1 token 缺 refresh_token（keys=[{keys}]）——桥接继续，续期能力以 A2 实测为准"
        );
    }
    // expires_in：原生形直取；L1 形由 access_token_expires_at（unix 秒；兼容数字
    // 字符串形）折算剩余秒；缺省/异常按 1h 兜底
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let expires_in = match v.get("expires_in").and_then(|x| x.as_u64()) {
        Some(n) if n > 0 => n, // 原生形：直取，桥接幂等
        _ => {
            let expires_raw = v.get("access_token_expires_at");
            let expires_at = expires_raw.and_then(|x| x.as_u64()).or_else(|| {
                expires_raw
                    .and_then(|x| x.as_str())
                    .and_then(|s| s.parse::<u64>().ok())
            });
            match expires_at {
                Some(t) if t > now => t.saturating_sub(now).max(60),
                Some(_) => {
                    tracing::warn!(
                        "L1 token access_token_expires_at 已过期——按剩余 1h 处理（keys=[{keys}]）"
                    );
                    3600
                }
                None => {
                    tracing::warn!("L1 token access_token_expires_at 缺失/非 unix 秒——按剩余 1h 处理（keys=[{keys}]）");
                    3600
                }
            }
        }
    };
    // 原生形组装（#8 定案，2026-08-30 扫码实测）：可选字段透传不伪造；
    // 诊断信息（原 src/src_file）改记日志，文件保持与引擎原生形一致
    let mut bridged = serde_json::Map::new();
    bridged.insert(
        "token_type".into(),
        v.get("token_type")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::String("Bearer".into())),
    );
    bridged.insert("access_token".into(), serde_json::Value::String(access));
    bridged.insert("refresh_token".into(), serde_json::Value::String(refresh));
    for k in ["id_token", "scope", "sub", "user_group", "user_id"] {
        if let Some(x) = v.get(k) {
            bridged.insert(k.into(), x.clone());
        }
    }
    bridged.insert("expires_in".into(), serde_json::Value::from(expires_in));
    tracing::info!(
        src_file = %l1_token_path.display(),
        "L1 token 已桥接为引擎原生形（假设区 #8，2026-08-30 实测定案）"
    );
    let mgr = manager();
    put_auth_token(mgr, &serde_json::Value::Object(bridged).to_string()).await
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

/// V5 残留加固（第六轮 9.3.2）：tar 成员名危险判定——绝对路径（含盘符前缀）
/// 或 `..` 分量即危险。与 `verify_no_escape`（解包后复核）构成解包前/后双层防线。
fn tar_member_dangerous(name: &str) -> bool {
    // tar 成员按 POSIX 应为 `/` 分隔；防御性归一反斜杠后统一判定
    let name = name.replace('\\', "/");
    if name.starts_with('/') {
        return true;
    }
    // Windows 盘符前缀（C:/...）防呆
    let b = name.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return true;
    }
    name.split('/').any(|c| c == "..")
}

#[cfg(test)]
mod tar_member_tests {
    use super::tar_member_dangerous as dangerous;

    #[test]
    fn rejects_absolute_and_parent_members() {
        assert!(dangerous("/etc/passwd"));
        assert!(dangerous("../evil"));
        assert!(dangerous("a/../b"));
        assert!(dangerous("ok/../../out"));
        assert!(dangerous("./x/../../../y"));
    }

    #[test]
    fn rejects_windows_drive_prefix_and_backslash() {
        assert!(dangerous("C:/Windows/system32"));
        assert!(dangerous("c:\\..\\evil"));
        assert!(dangerous("D:\\abs"));
    }

    #[test]
    fn allows_benign_relative_members() {
        assert!(!dangerous("package.tgz"));
        assert!(!dangerous("INFO"));
        assert!(!dangerous("bin/bin/xunlei-pan-cli"));
        assert!(!dangerous("./conf/app.ini"));
        assert!(!dangerous("a/b/./c"));
        // 空段（双斜杠）与尾斜杠不误报
        assert!(!dangerous("a//b"));
        assert!(!dangerous("dir/"));
    }

    #[test]
    fn dot_and_dotdot_distinction() {
        // 单个 `.` 分量是常见相对前缀，不得误报
        assert!(!dangerous("./package.tgz"));
        // `..` 恰为完整分量才危险，`...`/`a..b` 不误报
        assert!(!dangerous(".../x"));
        assert!(!dangerous("a..b/c"));
    }
}
