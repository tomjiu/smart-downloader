//! 实验：设备码（扫码）token 能否配对同 client 的 captcha 调 pan API。
//!
//! 背景：扫码 token 绑定 DEVICE_CLIENT_ID（XW5Sk…），此前用 pan client（Xqp0…）
//! 的 captcha 去调 list_files 得到 400 "client_id not match"。本实验把三件套
//! （token/captcha/x-client-id）全部统一到 XW5Sk 再测一次：
//!   1. captcha/init（action=POST:/drive/v1/files, client_id=XW5Sk, 空 meta）
//!   2. GET /drive/v1/files?parent_id=&usage=DISPLAY&with_audit=true&limit=10
//!      头：Authorization(Bearer 扫码token) + x-captcha-token + x-client-id=XW5Sk + x-device-id
//!
//! 通过 → 扫码登录态可直接驱动云盘；失败（含错误体）→ 坐死必须账密路径。

use serde::Deserialize;
use std::path::Path;

const XLUSER: &str = "https://xluser-ssl.xunlei.com";
const PAN: &str = "https://api-pan.xunlei.com";
/// 扫码（设备码流程）使用的 client —— 与 access_token 同源。
const DEVICE_CLIENT_ID: &str = "XW5SkOhLDjnOZP7J";

#[tokio::main]
async fn main() {
    #[derive(Deserialize, serde::Serialize)]
    struct Auth {
        access_token: String,
        refresh_token: String,
        device_id: String,
    }
    let path = std::env::args().nth(1).unwrap_or_else(|| "xunlei_auth.json".into());
    let mut auth: Auth = serde_json::from_str(
        &std::fs::read_to_string(Path::new(&path)).expect("读登录态失败"),
    )
    .expect("解析登录态失败");
    let http = reqwest::Client::new();

    // 0) 用同 client（XW5Sk）刷新过期 token —— OAuth 正确姿势
    println!("[0] refresh_token 续期（client_id=XW5Sk）…");
    #[derive(Deserialize)]
    struct TokenResp {
        #[serde(default)] access_token: String,
        #[serde(default)] refresh_token: String,
        #[serde(default)] expires_in: u64,
    }
    let tr = http
        .post(format!("{XLUSER}/v1/auth/token"))
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": auth.refresh_token,
            "client_id": DEVICE_CLIENT_ID,
        }))
        .send()
        .await
        .unwrap_or_else(|e| { eprintln!("网络错误: {e}"); std::process::exit(1); });
    let t_status = tr.status();
    let t_body = tr.text().await.unwrap_or_default();
    if !t_status.is_success() {
        eprintln!("❌ refresh {t_status}: {}", &t_body[..t_body.len().min(300)]);
        std::process::exit(1);
    }
    let tok: TokenResp = serde_json::from_str(&t_body).expect("解析 token 响应失败");
    if tok.access_token.is_empty() {
        eprintln!("❌ refresh 未返回 access_token: {t_body}");
        std::process::exit(1);
    }
    auth.access_token = tok.access_token;
    if !tok.refresh_token.is_empty() {
        auth.refresh_token = tok.refresh_token.clone();
    }
    // 回写登录态（续期成果不丢）
    let _ = std::fs::write(Path::new(&path), serde_json::to_string_pretty(&auth).unwrap());
    println!("    ✅ 已续期并回写 {}（expires_in={}s）", path, tok.expires_in);

    // 1) 用与 token 同源的 client 拿 captcha（App 式空 meta，无需 captcha_sign）
    println!("[1] captcha/init（client_id={DEVICE_CLIENT_ID}, 空 meta）…");
    #[derive(Deserialize)]
    struct CapResp {
        #[serde(default)]
        captcha_token: String,
    }
    let cap_resp = http
        .post(format!("{XLUSER}/v1/shield/captcha/init"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "action": "POST:/drive/v1/files",
            "captcha_token": "",
            "client_id": DEVICE_CLIENT_ID,
            "device_id": auth.device_id,
            "meta": {},
            "redirect_uri": "xlaccsdk01://xunlei.com/callback?state=harbor",
        }))
        .send()
        .await
        .unwrap_or_else(|e| { eprintln!("网络错误: {e}"); std::process::exit(1); });
    let cap_status = cap_resp.status();
    let cap_body = cap_resp.text().await.unwrap_or_default();
    if !cap_status.is_success() {
        eprintln!("❌ captcha/init {cap_status}: {cap_body}");
        std::process::exit(1);
    }
    let cap: CapResp = serde_json::from_str(&cap_body).expect("解析 captcha 响应失败");
    if cap.captcha_token.is_empty() {
        eprintln!("❌ captcha_token 为空: {cap_body}");
        std::process::exit(1);
    }
    println!("    ✅ 拿到 captcha_token（{} 字符）", cap.captcha_token.len());

    // 2) 三件套同源调 pan 列目录
    println!("[2] GET /drive/v1/files（x-client-id=XW5Sk）…");
    let resp = http
        .get(format!("{PAN}/drive/v1/files?parent_id=&usage=DISPLAY&with_audit=true&limit=10"))
        .header("Authorization", format!("Bearer {}", auth.access_token))
        .header("x-device-id", &auth.device_id)
        .header("x-captcha-token", &cap.captcha_token)
        .header("x-client-id", DEVICE_CLIENT_ID)
        .send()
        .await
        .map_err(|e| { eprintln!("网络错误: {e}"); std::process::exit(1); })
        .unwrap();
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    println!("    HTTP {status}");
    println!("    body: {}", &body[..body.len().min(600)]);
    if status.is_success() {
        println!();
        println!("✅ 实验通过：扫码 token + 同源 captcha 可驱动云盘 API！");
        println!("   后续把 client.rs 的 CLIENT_ID 参数化即可复用扫码登录态。");
    } else {
        println!();
        println!("❌ 实验失败：确认扫码 token 无法用于 pan API，账密路径为唯一解。");
        std::process::exit(1);
    }
}
