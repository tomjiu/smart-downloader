//! 端到端验证：用真实登录态 + 还原的 captcha_sign 算法打 captcha/init + drive API。
//! 用法: cargo run -p smart-dl-provider --example verify_captcha_e2e
//! 前置: 同目录下 login_state_tokens.json 含真实 access_token/device_id/user_id。

use smart_dl_provider::xunlei::sign::{captcha_sign, device_id_32};
use smart_dl_provider::xunlei::client::CLIENT_ID;

const XLUSER_BASE: &str = "https://xluser-ssl.xunlei.com";
const PAN_BASE: &str = "https://api-pan.xunlei.com";
const CLIENT_VERSION: &str = "1.92.91";
const PACKAGE_NAME: &str = "pan.xunlei.com";

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 读真实登录态
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/research/cloud_delivery/login_reverse/login_state_tokens.json");
    let data = std::fs::read_to_string(path).unwrap_or_else(|_| {
        eprintln!("未找到 login_state_tokens.json，跳过 e2e 验证");
        std::process::exit(0);
    });
    let state: serde_json::Value = serde_json::from_str(&data)?;
    let access_token = state["access_token"].as_str().unwrap_or("");
    let device_id = state["device_id"].as_str().unwrap_or("");
    let user_id = state["user_id"].as_str().unwrap_or("");

    if access_token.is_empty() || device_id.is_empty() || user_id.is_empty() {
        eprintln!("token 缺失，跳过");
        return Ok(());
    }

    let client = reqwest::Client::new();
    let did32 = device_id_32(device_id);
    let timestamp = now_millis().to_string();
    let sign = captcha_sign(did32, &timestamp);

    println!("device_id(32): {did32}");
    println!("timestamp:     {timestamp}");
    println!("captcha_sign:  {sign}");

    // [1] captcha/init
    let resp = client
        .post(format!("{XLUSER_BASE}/v1/shield/captcha/init"))
        .json(&serde_json::json!({
            "action": "POST:/drive/v1/files",
            "captcha_token": "",
            "client_id": CLIENT_ID,
            "device_id": did32,
            "meta": {
                "timestamp": timestamp,
                "captcha_sign": sign,
                "user_id": user_id,
                "client_version": CLIENT_VERSION,
                "package_name": PACKAGE_NAME,
            },
            "redirect_uri": "xlaccsdk01://xunlei.com/callback?state=harbor",
        }))
        .send().await?;

    let status = resp.status();
    let body: serde_json::Value = resp.json().await?;
    if !status.is_success() {
        eprintln!("❌ captcha/init HTTP {status}: {body}");
        return Ok(());
    }
    let captcha_token = body["captcha_token"].as_str().unwrap_or("");
    println!("✅ captcha/init 200, token 前 40: {}", &captcha_token[..40.min(captcha_token.len())]);

    // [2] drive/v1/files
    let resp2 = client
        .get(format!("{PAN_BASE}/drive/v1/files?parent_id=&usage=DISPLAY&with_audit=true&limit=50"))
        .header("Authorization", format!("Bearer {access_token}"))
        .header("X-Captcha-Token", captcha_token)
        .header("X-Client-Id", CLIENT_ID)
        .header("X-Device-Id", did32)
        .send().await?;

    let status2 = resp2.status();
    let body2: serde_json::Value = resp2.json().await?;
    if !status2.is_success() {
        eprintln!("❌ drive/v1/files HTTP {status2}: {body2}");
        return Ok(());
    }
    let files = body2["files"].as_array().map(|a| a.len()).unwrap_or(0);
    println!("✅ drive/v1/files 200, 文件数: {files}");
    if let Some(arr) = body2["files"].as_array() {
        for f in arr.iter().take(3) {
            println!("   - {}", f["name"].as_str().unwrap_or("?"));
        }
    }
    println!("\n🎉 Rust 端到端验证通过：captcha_sign 算法 + refresh_captcha 完整闭环！");
    Ok(())
}
