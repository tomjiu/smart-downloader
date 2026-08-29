//! 活票全链自检（F3.1 前置）：
//! 加载凭证 → access 过期自动 refresh → captcha 过期自动取 → 列目录 → PLAY 直链 → Range 下载。
//!
//! 凭证兼容两种格式：标准 AuthState / 网页版 localStorage 导出（credentials_Xqp0…）。
//!
//! 用法：
//!   cargo run --offline -p smart-dl-provider --example xunlei_live_check -- [token.json]
//! 默认读 xunlei_auth_web.json。

use smart_dl_provider::xunlei::auth::{load as load_auth, save as save_auth};
use smart_dl_provider::xunlei::client::Client;

#[tokio::main]
async fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "xunlei_auth_web.json".into());
    let path = std::path::PathBuf::from(path);

    let Some(mut state) = load_auth(&path) else {
        eprintln!("❌ 无法加载凭证 {path:?}——确认文件存在且为 AuthState 或网页导出格式");
        std::process::exit(1);
    };
    println!("[0] 凭证加载 OK：user_id={} did32={}", state.user_id, &state.device_id[..8.min(state.device_id.len())]);

    let client = Client::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    if state.access_token_expiring(now) {
        println!("[1] access 过期 → refresh …");
        client.refresh(&mut state).await.expect("refresh");
        println!("    新 expires_at={}", state.access_token_expires_at);
    } else {
        println!("[1] access 仍有效（剩 {}s）", state.access_token_expires_at - now);
    }
    if state.captcha_token_expiring(now) || state.captcha_token.is_empty() {
        println!("[2] captcha 取新 …");
        client.refresh_captcha(&mut state).await.expect("captcha");
        println!("    OK len={}", state.captcha_token.len());
    }

    let _ = save_auth(&path, &state); // 旋转后的 refresh_token 落盘

    println!("[3] drive/v1/files …");
    let files = client.list_files(&state, "").await.expect("list_files");
    println!("    根目录 {} 项", files.files.len());
    for f in files.files.iter().take(5) {
        println!("    - [{}] {}", if f.is_folder { "DIR" } else { "FILE" }, f.name);
    }

    // 找一个真实文件（跳过文件夹；根目录没有则进第一个文件夹）
    let mut target: Option<(String, String)> = None;
    for f in files.files.iter() {
        if !f.is_folder {
            target = Some((f.id.clone(), f.name.clone()));
            break;
        }
    }
    if target.is_none() {
        for dir in files.files.iter().filter(|f| f.is_folder) {
            let sub = client.list_files(&state, &dir.id).await.expect("list sub");
            if let Some(f) = sub.files.iter().find(|f| !f.is_folder) {
                target = Some((f.id.clone(), f.name.clone()));
                break;
            }
        }
    }
    let Some((fid, fname)) = target else {
        println!("[4] 全盘无真实文件可取直链（目录均为空）——链路上半段已验证 ✅");
        return;
    };

    println!("[4] PLAY → {fname}");
    let play = client.resolve_link(&state, &fid).await.expect("resolve_link");
    if play.web_content_link.is_empty() {
        println!("    web_content_link 为空（可能文件类型不支持直链）——列表/鉴权段已验证 ✅");
        return;
    }
    println!("    LINK: {}…", &play.web_content_link[..play.web_content_link.len().min(90)]);

    println!("[5] Range 下载首块 …");
    let resp = reqwest::Client::new()
        .get(&play.web_content_link)
        .header("Range", "bytes=0-1023")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .expect("range download");
    let status = resp.status();
    let bytes = resp.bytes().await.expect("body").len();
    println!("    HTTP {status}, got {bytes} bytes");

    if status.as_u16() == 206 && bytes > 0 {
        println!("\n✅ 全链验证通过：token→captcha→list→PLAY→Range 下载");
    } else {
        println!("\n⚠️ 链路通但末段异常：HTTP {status} bytes={bytes}");
    }
}
