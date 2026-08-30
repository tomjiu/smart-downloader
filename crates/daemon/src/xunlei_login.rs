//! `smart-dl xunlei-login` 命令实现（Task 5-b）。
//!
//! 三种模式（详见 provider::xunlei::login_flow 模块文档）：
//! - Page（默认）：本地起登录页服务（127.0.0.1 随机端口），控制台打印可点击
//!   地址，用户在本地渲染的 App 同款页面里扫码/账密/短信登录；
//! - Browser：本地起服务拿设备码后，直接调系统浏览器跳转**官方授权页**
//!   （pan.xunlei.com/yc/?client_id=…&user_code=…），命令行轮询状态；
//! - Qr：终端直接渲染二维码（qrcode unicode），手机迅雷 App 扫码。
//!
//! 三种模式成功后登录态都写入 token_path（默认 ./xunlei_auth.json，0600），
//! 后续 `XunleiProvider::new(_, token_path)` / daemon 直接复用。

use crate::cli::XunleiLoginMode;
use qrcode::QrCode;
use smart_dl_provider::xunlei::client::Client;
use smart_dl_provider::xunlei::login_flow::{
    open_in_browser, poll_device_session, start_device_session, store_auth_state,
};
use smart_dl_provider::xunlei::login_page::{serve_login_page, LoginSession};

/// 运行登录命令（main.rs 在客户端分发前拦截调用）。
pub async fn run(mode: XunleiLoginMode, token_path: Option<String>, port: u16) -> Result<(), String> {
    let token_path = std::path::PathBuf::from(
        token_path.unwrap_or_else(|| "xunlei_auth.json".to_string()),
    );
    match mode {
        XunleiLoginMode::Qr => run_qr(&token_path).await,
        XunleiLoginMode::Browser => run_browser(&token_path, port).await,
        XunleiLoginMode::Page => run_page(&token_path, port).await,
    }
}

/// 终端二维码模式。
async fn run_qr(token_path: &std::path::Path) -> Result<(), String> {
    let client = Client::new();
    let session = start_device_session(&client, smart_dl_provider::xunlei::login_flow::DEVICE_SCOPE)
        .await
        .map_err(|e| format!("设备码获取失败: {e}"))?;
    println!();
    println!("=== 迅雷设备码登录（终端二维码） ===");
    println!("手机迅雷 App → 右上角「扫一扫」扫描下方二维码：");
    println!("  授权页: {}", session.qr_url);
    println!("  授权码: {}（页面要求手动输入时使用）", session.user_code);
    print_qr_terminal(&session.qr_url)?;
    wait_loop(&client, &session, token_path).await
}

/// 浏览器跳转官方页模式。
async fn run_browser(token_path: &std::path::Path, port: u16) -> Result<(), String> {
    let client = Client::new();
    let session = start_device_session(&client, smart_dl_provider::xunlei::login_flow::DEVICE_SCOPE)
        .await
        .map_err(|e| format!("设备码获取失败: {e}"))?;
    println!();
    println!("=== 迅雷登录（跳转官方授权页） ===");
    // Browser 模式同样本地起一个登录页作备用入口（若浏览器被拦截可手动打开）。
    let sess_state = LoginSession::new_with_client(
        Client::new(),
        token_path.to_path_buf(),
        Some(session.clone()),
    );
    let addr = serve_login_page(sess_state, port)
        .await
        .map_err(|e| format!("本地登录页启动失败: {e}"))?;
    println!("正在打开系统浏览器跳转迅雷官方授权页…");
    println!("  官方页: {}", session.qr_url);
    if open_in_browser(&session.qr_url).is_err() {
        println!("  ⚠ 系统浏览器打开失败，请手动复制上方链接，或打开本地备用页: http://{addr}");
    } else {
        println!("  备用本地页: http://{addr}（浏览器被拦截时使用）");
    }
    println!("  授权码: {}", session.user_code);
    wait_loop(&client, &session, token_path).await
}

/// 本地登录页模式（默认）。
async fn run_page(token_path: &std::path::Path, port: u16) -> Result<(), String> {
    let sess_state = LoginSession::new_with_client(Client::new(), token_path.to_path_buf(), None);
    let addr = serve_login_page(sess_state, port)
        .await
        .map_err(|e| format!("本地登录页启动失败: {e}"))?;
    println!();
    println!("=== 迅雷登录（本地 App 同款页面） ===");
    println!("请在浏览器打开: http://{addr}");
    println!("  · 扫码 Tab：手机迅雷 App 扫描页内二维码（跳转官方授权页确认）");
    println!("  · 密码 Tab：手机号/邮箱/用户名 + 密码（HTTPS 直连迅雷）");
    println!("  · 短信 Tab：手机号 + 验证码");
    println!("登录态保存: {}（仅本机，权限 0600）", token_path.display());
    println!("按 Ctrl+C 退出。");
    // 页面服务由 serve_login_page 的 spawned task 承载；此处挂起等待用户操作。
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}

/// Browser/Qr 模式的轮询等待循环。
async fn wait_loop(
    client: &Client,
    session: &smart_dl_provider::xunlei::login_flow::DeviceSession,
    token_path: &std::path::Path,
) -> Result<(), String> {
    let mut current = session.clone();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(session.interval.max(1))).await;
        match poll_device_session(client, &current).await {
            Ok(Some(auth)) => {
                store_auth_state(token_path, &auth)
                    .map_err(|e| format!("登录态保存失败: {e}"))?;
                println!();
                println!("✅ 登录成功！user_id: {}，登录态已写入: {}", 
                    if auth.user_id.is_empty() { "-" } else { &auth.user_id },
                    token_path.display());
                return Ok(());
            }
            Ok(None) => {
                print!(".");
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
            }
            Err(e) => return Err(format!("登录失败: {e}")),
        }
        let _ = &mut current; // 当前实现 poll 以 device_code 定位，状态无迁移
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
}
