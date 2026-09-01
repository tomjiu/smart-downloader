//! `smart-dl xunlei-login` 命令实现（Task 5-b）。
//!
//! 三种模式（详见 provider::xunlei::login_flow 模块文档）：
//! - Page（默认）：本地起登录页服务（127.0.0.1 随机端口），控制台打印可点击
//!   地址，用户在本地渲染的 App 同款页面里扫码/账密/短信登录；
//! - Browser：本地起服务拿设备码后，直接调系统浏览器跳转**官方授权页**
//!   （pan.xunlei.com/yc/?client_id=…&user_code=…），命令行轮询状态；
//! - Qr：终端直接渲染二维码（qrcode unicode），手机迅雷 App 扫码。
//!
//! 设备码服务端固定 120s 有效（expires_in）；Qr/Browser 模式轮询遇过期
//! 自动换新码重打链接/二维码（与 scripts/nas/nas_qr_daemon.py 循环续发同款），
//! 不再失败退出——实测最高频失败即「打开链接时码已过期」。
//!
//! 三种模式成功后登录态都写入 token_path（默认 ./xunlei_auth.json，0600），
//! 后续 `XunleiProvider::new(_, token_path)` / daemon 直接复用。
//!
//! 身份档位（P1-1）：`--tier <web|nas>` 切换参数集（client_id/页面参数/
//! captcha meta）；登录态按档分文件（web → `xunlei_auth.json`、其余 →
//! `xunlei_auth_<tier>.json`，显式 `--token` 优先），保证同账号多档并存时
//! 每档独立 device_id 不互踢。未知档直接报错列出可用项。

use crate::cli::XunleiLoginMode;
use qrcode::QrCode;
use smart_dl_provider::xunlei::client::Client;
use smart_dl_provider::xunlei::login_flow::{
    open_in_browser, poll_device_session, start_device_session, store_auth_state, DEVICE_SCOPE,
};
use smart_dl_provider::xunlei::login_page::{serve_login_page, LoginSession};
use smart_dl_provider::xunlei::tier::{Tier, ALL_TIERS};

/// 运行登录命令（main.rs 在客户端分发前拦截调用）。
pub async fn run(
    mode: XunleiLoginMode,
    token_path: Option<String>,
    port: u16,
    tier_name: Option<String>,
) -> Result<(), String> {
    // 档位解析：未知档直接拒绝（列出可用项），绝不静默回退 web。
    let tier: &'static Tier = match tier_name.as_deref() {
        None => &smart_dl_provider::xunlei::tier::TIER_WEB,
        Some(name) => Tier::by_name(name).ok_or_else(|| {
            format!(
                "未知身份档位 '{name}'（可用: {}）",
                ALL_TIERS
                    .iter()
                    .map(|t| t.name)
                    .collect::<Vec<_>>()
                    .join("/")
            )
        })?,
    };
    // 登录态按档分文件（防互踢）：显式 --token 优先；否则 web 保持旧文件名
    // （零回归），非 web 档加后缀。
    let default_name = if tier.name == "web" {
        "xunlei_auth.json".to_string()
    } else {
        format!("xunlei_auth_{}.json", tier.name)
    };
    let token_path = std::path::PathBuf::from(token_path.unwrap_or(default_name));
    let client = Client::new().with_tier(tier);
    println!("身份档位: {}（client_id={}）", tier.name, tier.client_id);
    println!("  {}", tier.authorize_note);
    match mode {
        XunleiLoginMode::Qr => run_qr(&client, &token_path).await,
        XunleiLoginMode::Browser => run_browser(&client, &token_path, port).await,
        XunleiLoginMode::Page => run_page(&client, &token_path, port).await,
    }
}

/// 终端二维码模式。
async fn run_qr(client: &Client, token_path: &std::path::Path) -> Result<(), String> {
    let session = start_device_session(client, smart_dl_provider::xunlei::login_flow::DEVICE_SCOPE)
        .await
        .map_err(|e| format!("设备码获取失败: {e}"))?;
    println!();
    println!(
        "=== 迅雷设备码登录（终端二维码，档位: {}） ===",
        client.tier().name
    );
    println!("手机迅雷 App → 右上角「扫一扫」扫描下方二维码：");
    println!("  授权页: {}", session.qr_url);
    println!("  授权码: {}（页面要求手动输入时使用）", session.user_code);
    println!("  （设备码 120s 有效；过期会自动换新并重打二维码，旧码作废）");
    print_qr_terminal(&session.qr_url)?;
    wait_loop(client, &session, token_path, true, false).await
}

/// 浏览器跳转官方页模式。
async fn run_browser(
    client: &Client,
    token_path: &std::path::Path,
    port: u16,
) -> Result<(), String> {
    let session = start_device_session(client, smart_dl_provider::xunlei::login_flow::DEVICE_SCOPE)
        .await
        .map_err(|e| format!("设备码获取失败: {e}"))?;
    println!();
    println!(
        "=== 迅雷登录（跳转官方授权页，档位: {}） ===",
        client.tier().name
    );
    // Browser 模式同样本地起一个登录页作备用入口（若浏览器被拦截可手动打开）。
    let sess_state = LoginSession::new_with_client(
        client.clone(),
        token_path.to_path_buf(),
        Some(session.clone()),
    );
    let addr = serve_login_page(sess_state, port)
        .await
        .map_err(|e| format!("本地登录页启动失败: {e}"))?;
    // 打开带 scope 的 /yc/ 统一授权页（与本地页第三方 Tab 同源，Task 22/25）。
    let web_url = if session.web_auth_url.is_empty() {
        session.qr_url.clone()
    } else {
        session.web_auth_url.clone()
    };
    println!("正在打开系统浏览器跳转迅雷官方授权页…");
    println!("  官方页: {web_url}");
    if open_in_browser(&web_url).is_err() {
        println!("  ⚠ 系统浏览器打开失败，请手动复制上方链接，或打开本地备用页: http://{addr}");
    } else {
        println!("  备用本地页: http://{addr}（浏览器被拦截时使用）");
    }
    println!("  授权码: {}", session.user_code);
    println!("  （设备码 120s 有效；过期会自动换新并重开浏览器，旧码作废）");
    wait_loop(client, &session, token_path, false, true).await
}

/// 本地登录页模式（默认）。
async fn run_page(client: &Client, token_path: &std::path::Path, port: u16) -> Result<(), String> {
    let sess_state = LoginSession::new_with_client(client.clone(), token_path.to_path_buf(), None);
    let addr = serve_login_page(sess_state, port)
        .await
        .map_err(|e| format!("本地登录页启动失败: {e}"))?;
    println!();
    println!("=== 迅雷登录（本地 App 同款页面） ===");
    println!("请在浏览器打开: http://{addr}");
    println!("  · 扫码 Tab：手机迅雷 App 扫描页内二维码（跳转官方授权页确认）");
    println!("  · 密码 Tab：手机号/邮箱/用户名 + 密码（HTTPS 直连迅雷）");
    println!("  · 短信 Tab：手机号 + 验证码");
    println!("  · 第三方 Tab：微信/QQ/微博（打开官方授权页登录确认，自动取证）");
    println!("登录态保存: {}（仅本机，权限 0600）", token_path.display());
    println!("按 Ctrl+C 退出。");
    // 页面服务由 serve_login_page 的 spawned task 承载；此处挂起等待用户操作。
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}

/// 判断轮询错误是否为设备码过期（本地 expires_at 检查或服务端 expired_token）。
fn is_device_code_expired(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    msg.contains("过期")
        || lower.contains("expired")
        || lower.contains("gone")
        || lower.contains("410")
}

/// Browser/Qr 模式的轮询等待循环。
///
/// `show_qr`：换新码时是否重打终端二维码（Qr 模式）；`reopen_browser`：
/// 换新码时是否重开系统浏览器跳新授权页（Browser 模式）。旧登录进度不会丢：
/// SSO 登录态在浏览器侧，新码打开后直接进确认步。
async fn wait_loop(
    client: &Client,
    session: &smart_dl_provider::xunlei::login_flow::DeviceSession,
    token_path: &std::path::Path,
    show_qr: bool,
    reopen_browser: bool,
) -> Result<(), String> {
    let mut current = session.clone();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(session.interval.max(1))).await;
        match poll_device_session(client, &current).await {
            Ok(Some(auth)) => {
                store_auth_state(token_path, &auth).map_err(|e| format!("登录态保存失败: {e}"))?;
                println!();
                println!(
                    "✅ 登录成功！user_id: {}，登录态已写入: {}",
                    if auth.user_id.is_empty() {
                        "-"
                    } else {
                        &auth.user_id
                    },
                    token_path.display()
                );
                return Ok(());
            }
            Ok(None) => {
                print!(".");
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
            }
            Err(e) => {
                if !is_device_code_expired(&e.to_string()) {
                    return Err(format!("登录失败: {e}"));
                }
                // 过期 → 换新码继续轮询（与 nas_qr_daemon.py 同款循环续发）。
                println!();
                println!("⏳ 设备码已过期（120s 有效），自动换新码，旧链接/二维码作废：");
                let s = start_device_session(client, DEVICE_SCOPE)
                    .await
                    .map_err(|se| format!("换新设备码失败: {se}"))?;
                println!("  授权页: {}", s.web_auth_url);
                println!("  授权码: {}", s.user_code);
                if show_qr {
                    print_qr_terminal(&s.qr_url)?;
                }
                if reopen_browser && open_in_browser(&s.web_auth_url).is_err() {
                    println!("  ⚠ 自动打开浏览器失败，请手动复制上方授权页链接");
                }
                current = s;
            }
        }
    }
}

/// 终端二维码渲染（qrcode unicode）。
fn print_qr_terminal(url: &str) -> Result<(), String> {
    use qrcode::render::unicode;
    let code = QrCode::new(url.as_bytes()).map_err(|e| format!("二维码生成失败: {e}"))?;
    let image = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Dark)
        .light_color(unicode::Dense1x2::Light)
        .quiet_zone(true)
        .build();
    println!("{image}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_qr_terminal_renders_ascii() {
        // 渲染不 panic 且输出包含二维码字符块。
        let r = print_qr_terminal("https://pan.xunlei.com/yc/?client_id=x&user_code=Y");
        assert!(r.is_ok());
    }

    #[test]
    fn device_expired_detection() {
        assert!(is_device_code_expired("device code 已过期"));
        assert!(is_device_code_expired(
            "token endpoint returned expired_token"
        ));
        assert!(is_device_code_expired("HTTP Error 410: Gone"));
        assert!(!is_device_code_expired("HTTP Error 400: Bad Request"));
        assert!(!is_device_code_expired("登录失败: connection refused"));
    }
}
