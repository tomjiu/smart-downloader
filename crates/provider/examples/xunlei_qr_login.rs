//! 迅雷设备码登录（RFC 8628）端到端验证入口。
//!
//! 流程：
//! 1. 请求 device code → 拿到 verification_uri（迅雷官方授权页）
//! 2. 自动用默认浏览器打开该页面（Windows `start`；其他平台打印链接手动开）
//! 3. 在官方页面上用任意方式登录：**微信扫码 / QQ / 迅雷 App 扫码 / 账密**
//!    —— 都完成同一个 device code 的授权，第三方无需任何平台凭据
//! 4. 本程序轮询 token 端点直到授权成功
//! 5. 组装完整 AuthState（JWT 解 user_id + 拉 captcha_token）并原子落盘
//!
//! 落盘后的登录态可直接被 `XunleiProvider::new(token_path)` 加载。
//!
//! 运行：
//! ```text
//! cargo run -p smart-dl-provider --example xunlei_qr_login [-- path/to/auth.json]
//! ```
//!
//! 为什么不在终端画二维码：微信 OAuth 的 appid/secret 是迅雷在微信开放平台
//! 注册的，第三方生成的二维码微信不认；而官方授权页自带微信扫码入口，
//! 打开链接即等价于扫码（见 docs/research/2026-08-22-xunlei-login-reverse-status.md）。

use qrcode::{QrCode, render::unicode};
use smart_dl_provider::xunlei::client::device_code_qr_url;
use smart_dl_provider::xunlei::device::DeviceFlowState;
use smart_dl_provider::xunlei::provider::XunleiProvider;

/// 与研究脚本 get_device_code_link.py 实测一致的 scope。
const SCOPE: &str = "profile offline pan sso user";

#[tokio::main]
async fn main() {
    // 登录态落盘路径：命令行第 1 参数可覆盖，默认当前目录 xunlei_auth.json
    let token_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "xunlei_auth.json".into());
    let token_path = std::path::PathBuf::from(token_path);

    let provider = XunleiProvider::new("xunlei", token_path.clone());
    let flow = provider.begin_device_login();

    // 1. 请求设备码
    let state = match flow.start(SCOPE).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("请求设备码失败: {e}");
            std::process::exit(1);
        }
    };
    let DeviceFlowState::AwaitingScan { user_code, verification_uri, expires_at, .. } = &state else {
        unreachable!("start 只返回 AwaitingScan");
    };
    // 本地构造官方授权页 URL（PROJECT_STATUS「QR 构造」对齐项：
    // pan.xunlei.com/yc/?client_id=…&user_code=…，2026-08-25 实测可用）。
    // 服务端 verification_uri 仅作回退展示。
    let qr_url = device_code_qr_url(user_code);

    // 2. 终端渲染二维码（用手机【迅雷 App】扫一扫）+ 备用浏览器打开
    println!();
    println!("=== 迅雷设备码登录 ===");
    println!("请打开手机迅雷 App → 右上角扫一扫 → 扫描下方二维码：");
    println!("  链接: {qr_url}");
    println!("  授权码: {user_code}（若页面要求手动输入）");
    let code = match QrCode::new(qr_url.as_bytes()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("二维码生成失败: {e}（可直接在 App 输入上方链接）");
            std::process::exit(1);
        }
    };
    let image = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Dark)
        .light_color(unicode::Dense1x2::Light)
        .quiet_zone(true)
        .build();
    println!("{image}");
    println!("等待授权确认中…（{} 秒内有效）", expires_at - now_unix());

    // 3. 轮询直到 Done / Failed / 超时
    let mut current = state;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        current = match flow.poll_once(&current).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("\n轮询失败: {e}");
                std::process::exit(1);
            }
        };
        match &current {
            DeviceFlowState::AwaitingScan { expires_at, .. } => {
                if now_unix() >= *expires_at {
                    eprintln!("\n设备码已过期，请重跑本程序。");
                    std::process::exit(1);
                }
                print!(".");
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
            }
            DeviceFlowState::Failed { reason } => {
                eprintln!("\n登录失败: {reason}");
                std::process::exit(1);
            }
            DeviceFlowState::Done { .. } => break,
        }
    }

    // 4. 取 token 并持久化完整登录态
    // store_login 内部：JWT 解 user_id + refresh_captcha 拉取 captcha_token + 原子写盘。
    let DeviceFlowState::Done { access_token, refresh_token } = current else {
        unreachable!("break 只发生在 Done");
    };
    if let Err(e) = provider.store_login(access_token, refresh_token).await {
        eprintln!("\n登录态保存失败: {e}");
        std::process::exit(1);
    }

    // 回读展示结果
    match smart_dl_provider::xunlei::auth::load(&token_path) {
        Some(st) => {
            let left = st.access_token_expires_at.saturating_sub(now_unix());
            println!();
            println!("✅ 登录成功！");
            println!("  user_id: {}", if st.user_id.is_empty() { "(未知)" } else { &st.user_id });
            println!("  access_token 剩余 ~{left} 秒");
            println!("  登录态已写入: {}", token_path.display());
            println!();
            println!("后续可启动 daemon 或调用 resolve 流程验证取链。");
        }
        None => {
            eprintln!("\n登录态写盘后读回失败: {}", token_path.display());
            std::process::exit(1);
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
