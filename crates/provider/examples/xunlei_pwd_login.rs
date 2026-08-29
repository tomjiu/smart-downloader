//! 迅雷账密登录端到端验证入口（路线 A，无需扫码/App）。
//!
//! 流程：captcha/init(登录 action) → POST /v1/auth/signin → 组装登录态
//! → 刷新 drive 场景 captcha_token → 原子落盘。
//!
//! 运行：
//! ```text
//! cargo run -p smart-dl-provider --example xunlei_pwd_login -- <username> <password> [auth.json]
//! ```
//! username 规则：`+86...` 手机号 / 邮箱 / 用户名均可（服务端按格式识别）。
//! 落盘后的登录态可直接被 `XunleiProvider::new(token_path)` 加载。

use smart_dl_provider::xunlei::auth::{save as save_auth};
use smart_dl_provider::xunlei::client::Client;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("用法: xunlei_pwd_login <username> <password> [auth.json] [device_id]");
        eprintln!("  username: 手机号(+86xx)/邮箱/用户名");
        eprintln!("  device_id: 可选；传入浏览器已注册的可信设备号可降低短信二次验证概率");
        std::process::exit(2);
    }
    let username = &args[1];
    let password = &args[2];
    let token_path = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| "xunlei_auth.json".into());

    // device_id：优先用调用方注入的可信设备号；否则时间戳 md5 生成随机 32 hex。
    let device_id = match args.get(4) {
        Some(d) if !d.is_empty() => d.clone(),
        _ => {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            md5_hex(ts.to_string().as_bytes())[..32].to_string()
        }
    };
    println!("device_id: {device_id}");

    let client = Client::new();
    println!("[1/3] 登录中（{username}）…");
    let mut state = match client.signin(username, password, &device_id).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("登录失败: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "  ✅ access_token 到期时间戳: {}（剩余 {} 秒）",
        state.access_token_expires_at,
        state.access_token_expires_at.saturating_sub(now_unix())
    );

    println!("[2/3] 补全 user_id 与 drive 场景 captcha_token …");
    state.fill_user_id_from_token();
    if let Err(e) = client.refresh_captcha(&mut state).await {
        eprintln!("刷新 captcha 失败: {e}");
        std::process::exit(1);
    }

    println!("[3/3] 落盘 …");
    if let Err(e) = save_auth(std::path::Path::new(&token_path), &state) {
        eprintln!("写盘失败: {e}");
        std::process::exit(1);
    }
    println!();
    println!("✅ 登录成功，登录态已写入: {token_path}");
    println!("  user_id: {}", if state.user_id.is_empty() { "(未知)" } else { &state.user_id });
    println!("后续可启动 daemon 或运行 resolve 取链验证。");
}

/// 极简 MD5 hex（复用 workspace 已有 md-5 crate）。
fn md5_hex(data: &[u8]) -> String {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
