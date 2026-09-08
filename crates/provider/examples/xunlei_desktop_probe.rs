//! 实验：桌面迅雷缓存提取的登录态（client=XW-G4v1H72tgfJym）能否驱动 pan API。
//! 前置：PowerShell 已从 %APPDATA%\thunder\Cache 提取并写出 xunlei_desktop_creds.json。
//!
//! 步骤：
//!   0. （可选）若 access_token 过期 → 用同 client refresh 续期
//!   1. captcha/init（action=POST:/drive/v1/files, client_id=同源, 空 meta 起步）
//!   2. GET /drive/v1/files 三件套同源
//!
//! 通过 → 云盘功能新增第四条登录通道「桌面提取」，全程免密免码。

use serde::Deserialize;
use std::path::Path;

const PAN: &str = "https://api-pan.xunlei.com";
const XLUSER: &str = "https://xluser-ssl.xunlei.com";

#[tokio::main]
async fn main() {
    #[derive(Deserialize, serde::Serialize)]
    struct Creds {
        client_id: String,
        #[serde(default)]
        access_token: String,
        #[serde(default)]
        refresh_token: String,
    }
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "xunlei_desktop_creds.json".into());
    let mut c: Creds =
        serde_json::from_str(&std::fs::read_to_string(Path::new(&path)).expect("读 creds 失败"))
            .expect("解析 creds 失败");
    let http = reqwest::Client::new();

    // 0) 过期则续期（同 client）
    #[derive(Deserialize)]
    struct JwtP {
        #[serde(default)]
        exp: u64,
    }
    fn jwt_exp(tok: &str) -> u64 {
        let p = tok.split('.').nth(1).unwrap_or("");
        let mut s = p.replace('-', "+").replace('_', "/");
        while !s.len().is_multiple_of(4) {
            s.push('=');
        }
        serde_json::from_str::<JwtP>(&base64_decode(&s))
            .map(|j| j.exp)
            .unwrap_or(0)
    }
    fn base64_decode(s: &str) -> String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let (mut out, mut buf, mut bits) = (Vec::new(), 0u32, 0u32);
        for ch in s.bytes() {
            if let Some(i) = T.iter().position(|&t| t == ch) {
                buf = (buf << 6) | i as u32;
                bits += 6;
                if bits >= 8 {
                    bits -= 8;
                    out.push((buf >> bits) as u8);
                    buf &= (1 << bits) - 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    if jwt_exp(&c.access_token) < now + 60 && !c.refresh_token.is_empty() {
        println!("[0] access 过期，同 client refresh …");
        #[derive(Deserialize)]
        struct TR {
            #[serde(default)]
            access_token: String,
            #[serde(default)]
            refresh_token: String,
        }
        let r = http.post(format!("{XLUSER}/v1/auth/token"))
            .json(&serde_json::json!({"grant_type":"refresh_token","refresh_token":c.refresh_token,"client_id":c.client_id}))
            .send().await.unwrap_or_else(|e| { eprintln!("网络错误: {e}"); std::process::exit(1); });
        let st = r.status();
        let body = r.text().await.unwrap_or_default();
        if !st.is_success() {
            eprintln!("❌ refresh {st}: {}", &body[..body.len().min(300)]);
            std::process::exit(1);
        }
        let tr: TR = serde_json::from_str(&body).unwrap();
        c.access_token = tr.access_token;
        if !tr.refresh_token.is_empty() {
            c.refresh_token = tr.refresh_token;
        }
        let _ = std::fs::write(Path::new(&path), serde_json::to_string_pretty(&c).unwrap());
        println!("    ✅ 续期回写");
    }

    // 设备号：随机 32 hex（App 式；先试空 meta）
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let digest = md5(ts.to_string().as_bytes());
    let did = &digest[..32];

    // 1) captcha/init 同 client
    println!("[1] captcha/init（client={}）…", c.client_id);
    #[derive(Deserialize)]
    struct CapR {
        #[serde(default)]
        captcha_token: String,
    }
    let cr = http
        .post(format!("{XLUSER}/v1/shield/captcha/init"))
        .header("x-client-version", "25.0.90.1592")
        .json(&serde_json::json!({
            "action": "POST:/drive/v1/files",
            "captcha_token": "",
            "client_id": c.client_id,
            "device_id": did,
            "meta": {},
            "redirect_uri": "xlaccsdk01://xunlei.com/callback?state=harbor",
        }))
        .send()
        .await
        .unwrap_or_else(|e| {
            eprintln!("网络错误: {e}");
            std::process::exit(1);
        });
    let cst = cr.status();
    let cbody = cr.text().await.unwrap_or_default();
    if !cst.is_success() {
        eprintln!("❌ captcha/init {cst}: {}", &cbody[..cbody.len().min(300)]);
        std::process::exit(1);
    }
    let cap: CapR = serde_json::from_str(&cbody).expect("解析 captcha 失败");
    println!("    ✅ token（{} 字符）", cap.captcha_token.len());

    // 2) 列目录
    println!("[2] GET /drive/v1/files …");
    let resp = http
        .get(format!(
            "{PAN}/drive/v1/files?parent_id=&usage=DISPLAY&with_audit=true&limit=10"
        ))
        .header("Authorization", format!("Bearer {}", c.access_token))
        .header("x-device-id", did)
        .header("x-captcha-token", &cap.captcha_token)
        .header("x-client-id", &c.client_id)
        .header("x-client-version", "25.0.90.1592")
        .send()
        .await
        .unwrap_or_else(|e| {
            eprintln!("网络错误: {e}");
            std::process::exit(1);
        });
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    println!("    HTTP {status}");
    println!("    {}", &body[..body.len().min(700)]);
    if status.is_success() {
        println!();
        println!("🎉 桌面提取登录态可驱动云盘！第四条通道成立。");
    } else {
        println!();
        println!("❌ 失败——把上方错误体发回分析。");
        std::process::exit(1);
    }
}

fn md5(data: &[u8]) -> String {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
