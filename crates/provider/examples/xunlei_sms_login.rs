//! 迅雷短信验证码登录（免密、无滑块路径）—— 两阶段 CLI。
//!
//! 账密 signin 实测被风控打回 result:review（交互式滑块，服务端绕不过）；
//! 短信登录走 /v1/auth/verification + verify，不经过密码与滑块。
//!
//! 用法（两个终端步骤，或由编排方分两次调用）：
//! ```text
//! cargo run -p smart-dl-provider --example xunlei_sms_login -- send <手机号> [session.json]
//! cargo run -p smart-dl-provider --example xunlei_sms_login -- verify <验证码> [session.json]
//! ```
//! send 把 verification_id/phone/device_id 存进 session.json；verify 读取后
//! 换 token，并补齐 drive 场景 captcha 后原子落盘 xunlei_auth.json。

use smart_dl_provider::xunlei::auth::save as save_auth;
use smart_dl_provider::xunlei::client::Client;
use std::path::{Path, PathBuf};

const DEFAULT_SESSION: &str = "xunlei_sms_session.json";
const DEFAULT_AUTH: &str = "xunlei_auth.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct SmsSession {
    phone: String,
    device_id: String,
    verification_id: String,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("");
    match mode {
        "send" => {
            let phone = args.get(2).cloned().unwrap_or_else(|| {
                eprintln!("用法: xunlei_sms_login send <手机号> [session.json]");
                std::process::exit(2);
            });
            let session_path = args
                .get(3)
                .cloned()
                .unwrap_or_else(|| DEFAULT_SESSION.into());
            send_phase(&phone, Path::new(&session_path)).await;
        }
        "verify" => {
            let code = args.get(2).cloned().unwrap_or_else(|| {
                eprintln!("用法: xunlei_sms_login verify <验证码> [session.json] [auth.json]");
                std::process::exit(2);
            });
            let session_path = args
                .get(3)
                .cloned()
                .unwrap_or_else(|| DEFAULT_SESSION.into());
            let auth_path = args.get(4).cloned().unwrap_or_else(|| DEFAULT_AUTH.into());
            verify_phase(&code, Path::new(&session_path), PathBuf::from(auth_path)).await;
        }
        _ => {
            eprintln!("用法: xunlei_sms_login <send|verify> …");
            eprintln!("  send   <手机号> [session.json]");
            eprintln!("  verify <验证码> [session.json] [auth.json]");
            std::process::exit(2);
        }
    }
}

async fn send_phase(phone: &str, session_path: &Path) {
    // 设备号：时间戳 md5（匿名即可；短信下发按手机号风控）
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let device_id = md5_hex(ts.to_string().as_bytes())[..32].to_string();

    let client = Client::new();
    println!("[1] 发送短信验证码到 {phone} …");
    let verification_id = client
        .send_sms_code(phone, &device_id)
        .await
        .unwrap_or_else(|e| {
            eprintln!("发送失败: {e}");
            std::process::exit(1);
        });
    println!("    ✅ 已下发（verification_id={verification_id}）");

    let session = SmsSession {
        phone: phone.to_string(),
        device_id,
        verification_id,
    };
    std::fs::write(
        session_path,
        serde_json::to_string_pretty(&session).unwrap(),
    )
    .expect("写会话文件失败");
    println!("    会话已存: {}", session_path.display());
    println!();
    println!("下一步：收到短信后执行");
    println!("  cargo run -p smart-dl-provider --example xunlei_sms_login -- verify <验证码>");
}

async fn verify_phase(code: &str, session_path: &Path, auth_path: PathBuf) {
    let session: SmsSession = serde_json::from_str(
        &std::fs::read_to_string(session_path).expect("读会话失败（先跑 send？）"),
    )
    .expect("解析会话失败");

    let client = Client::new();
    println!("[1] 校验验证码 {code} …");
    let mut state = client
        .verify_sms_code(
            &session.phone,
            code,
            &session.verification_id,
            &session.device_id,
        )
        .await
        .unwrap_or_else(|e| {
            eprintln!("校验失败: {e}");
            std::process::exit(1);
        });

    // 补齐 pan API 三要素：device_id + user_id + drive 场景 captcha
    if state.device_id.is_empty() {
        state.device_id = session.device_id.clone();
    }
    state.fill_user_id_from_token();
    println!("[2] 拉取 drive 场景 captcha_token …");
    client
        .refresh_captcha(&mut state)
        .await
        .unwrap_or_else(|e| {
            eprintln!("刷新 captcha 失败: {e}");
            std::process::exit(1);
        });

    save_auth(&auth_path, &state).expect("写登录态失败");
    let left = state.access_token_expires_at.saturating_sub(now_unix());
    println!();
    println!("✅ 短信登录成功！");
    println!(
        "  user_id: {}",
        if state.user_id.is_empty() {
            "(未知)"
        } else {
            &state.user_id
        }
    );
    println!("  access_token 剩余 ~{left} 秒");
    println!("  登录态已写入: {}", auth_path.display());
}

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
