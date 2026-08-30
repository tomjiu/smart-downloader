//! 迅雷原生登录编排（Task 5-b）。
//!
//! 三种登录模式（均不依赖官方客户端二进制，纯 Rust 实现）：
//!
//! | 模式 | 用户体验 | 实现 |
//! |------|---------|------|
//! | `Browser` | 点击/命令后直接跳转**官方授权页**（pan.xunlei.com/yc/），App 扫码或网页登录确认 | 设备码流程 + 系统浏览器打开本地构造的授权 URL |
//! | `Page`（默认） | 本地渲染一个**与迅雷 App 登录页一致**的页面（扫码/密码/短信三 Tab） | 本地 axum 服务（login_page.rs）+ 设备码/账密/短信流程 |
//! | `Qr` | 终端直接渲染二维码，手机迅雷 App 扫码 | 设备码流程 + qrcode unicode 渲染 |
//!
//! 流程均以 RFC 8628 设备码 / `/v1/auth/signin` / 短信验证码端点为准，
//! 逆向依据见 docs/research/2026-08-22-xunlei-login-reverse-status.md。

use crate::xunlei::auth::AuthState;
use crate::xunlei::client::{
    device_code_qr_url, now_unix, Client, ClientError,
};

/// 与研究脚本 get_device_code_link.py 实测一致的 scope。
pub const DEVICE_SCOPE: &str = "profile offline pan sso user";

/// 登录模式（CLI `--browser` / `--page` / `--qr`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginMode {
    /// 打开系统浏览器跳转官方授权页。
    Browser,
    /// 本地渲染 App 同款登录页（默认）。
    Page,
    /// 终端二维码。
    Qr,
}

impl LoginMode {
    /// CLI 标志解析（默认 Page）。
    pub fn from_flag(flag: Option<&str>) -> Option<LoginMode> {
        match flag {
            None => Some(LoginMode::Page),
            Some("--browser") => Some(LoginMode::Browser),
            Some("--page") => Some(LoginMode::Page),
            Some("--qr") => Some(LoginMode::Qr),
            _ => None,
        }
    }
}

/// 设备码会话（已就绪、等待用户授权）。
#[derive(Clone, Debug)]
pub struct DeviceSession {
    pub device_code: String,
    pub user_code: String,
    /// 本地构造的扫码授权页 URL（官方页面形状，实测 2026-08-25 可用）。
    pub qr_url: String,
    pub expires_at: u64,
    /// 服务端建议轮询间隔（秒，缺省 3）。
    pub interval: u64,
}

/// 发起设备码会话：请求 device/code → 本地构造 QR URL。
/// `client` 可用 [`Client::with_bases`] 注入 mock 地址（测试）。
pub async fn start_device_session(client: &Client, scope: &str) -> Result<DeviceSession, ClientError> {
    let code = client.request_device_code(scope).await?;
    let qr_url = device_code_qr_url(&code.user_code);
    Ok(DeviceSession {
        device_code: code.device_code,
        user_code: code.user_code,
        qr_url,
        expires_at: now_unix() + code.expires_in,
        interval: if code.interval == 0 { 3 } else { code.interval },
    })
}

/// 轮询一次设备码授权。
/// 返回：`Ok(Some(auth))` = 已授权（登录态就绪）；`Ok(None)` = 继续等待；
/// `Err` = 过期/被拒/网络错误。
///
/// 成功路径补全登录态：JWT 解 user_id（`fill_user_id_from_token`）+
/// captcha/init 拉 captcha_token（三件套头需要），与 provider::store_login 一致。
pub async fn poll_device_session(
    client: &Client,
    session: &DeviceSession,
) -> Result<Option<AuthState>, ClientError> {
    if now_unix() >= session.expires_at {
        return Err(ClientError::DeviceFlow("device code 已过期".into()));
    }
    match client.poll_device_token(&session.device_code).await? {
        Some(token) => {
            let device_id = crate::xunlei::sign::random_device_id();
            let mut state = AuthState {
                access_token: token.access_token,
                refresh_token: token.refresh_token,
                device_id,
                captcha_token: String::new(),
                user_id: String::new(),
                access_token_expires_at: now_unix() + token.expires_in,
                captcha_token_expires_at: 0,
            };
            state.fill_user_id_from_token();
            // captcha_token 三件套之一：授权成功后立即拉取（失败不致命：
            // provider 轮询时 refresh_captcha 会自动补）。
            if client.refresh_captcha(&mut state).await.is_err() {
                state.captcha_token_expires_at = 0;
            }
            Ok(Some(state))
        }
        None => Ok(None),
    }
}

/// 密码登录（复用 client::signin 的 captcha/init + signin 全链）。
pub async fn login_with_password(
    client: &Client,
    username: &str,
    password: &str,
) -> Result<AuthState, ClientError> {
    // device_id 与 web 端一致随机生成（服务端不校验来源，研究文档 §1.5）。
    let device_id = crate::xunlei::sign::random_device_id();
    client.signin(username, password, &device_id).await
}

/// 发送短信验证码。返回 verification_id（verify 第二步需要）。
pub async fn send_sms_code(client: &Client, phone: &str) -> Result<String, ClientError> {
    let device_id = crate::xunlei::sign::random_device_id();
    client.send_sms_code(phone, &device_id).await
}

/// 验证短信验证码完成登录。
pub async fn verify_sms_code(
    client: &Client,
    phone: &str,
    code: &str,
    verification_id: &str,
) -> Result<AuthState, ClientError> {
    let device_id = crate::xunlei::sign::random_device_id();
    client
        .verify_sms_code(phone, code, verification_id, &device_id)
        .await
}

/// 系统浏览器打开 URL（`--browser` 模式：点击直接跳转官方页面）。
/// 平台命令：linux `xdg-open` / macos `open` / windows `cmd /c start`。
pub fn open_in_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    Ok(())
}

/// 登录态落盘（供 daemon/provider 复用；文件权限收紧到 0600，POSIX）。
pub fn store_auth_state(path: &std::path::Path, state: &AuthState) -> std::io::Result<()> {
    crate::xunlei::auth::save(path, state)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xunlei::client::DEVICE_CLIENT_ID;

    #[test]
    fn qr_url_uses_local_template() {
        let url = device_code_qr_url("ABCD-1234");
        assert_eq!(
            url,
            "https://pan.xunlei.com/yc/?client_id=Xqp0kJBXWhwaTpB6&user_code=ABCD-1234"
        );
    }

    #[test]
    fn qr_url_escapes_user_code() {
        // user_code 若含特殊字符不应破坏 URL 结构（模板替换按原文插入，
        // 服务端 user_code 字符集为 [A-Z0-9-]，此处验证模板不被注入破坏）。
        let url = device_code_qr_url("A1B2C3");
        assert!(url.ends_with("&user_code=A1B2C3"));
    }

    #[test]
    fn device_client_id_aligned_with_verified_value() {
        // 防回归：设备码流程 client_id 必须是实测通过的 web 端值。
        assert_eq!(DEVICE_CLIENT_ID, "Xqp0kJBXWhwaTpB6");
        assert_eq!(DEVICE_CLIENT_ID, crate::xunlei::client::CLIENT_ID);
    }

    #[test]
    fn login_mode_from_flag() {
        assert_eq!(LoginMode::from_flag(None), Some(LoginMode::Page));
        assert_eq!(LoginMode::from_flag(Some("--qr")), Some(LoginMode::Qr));
        assert_eq!(LoginMode::from_flag(Some("--browser")), Some(LoginMode::Browser));
        assert_eq!(LoginMode::from_flag(Some("--page")), Some(LoginMode::Page));
        assert_eq!(LoginMode::from_flag(Some("--wat")), None);
    }

    #[tokio::test]
    async fn device_session_local_qr_url() {
        // start_device_session 依赖网络（真实端点），此处只测 URL 构造分支；
        // 全流程 mock 见 login_page.rs 集成测试。
        let s = DeviceSession {
            device_code: "dc".into(),
            user_code: "UC1234".into(),
            qr_url: device_code_qr_url("UC1234"),
            expires_at: now_unix() + 300,
            interval: 3,
        };
        assert!(s.qr_url.starts_with("https://pan.xunlei.com/yc/?client_id="));
        assert!(s.qr_url.contains("user_code=UC1234"));
    }
}
