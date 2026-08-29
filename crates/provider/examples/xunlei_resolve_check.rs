//! 迅雷云盘取链端到端验证：列目录 → 选文件 → PLAY API 取直链 → Range 下载验证。
//!
//! 使用已落盘的登录态（先跑 xunlei_qr_login 或 xunlei_pwd_login）。
//! 验证点：
//! 1. drive/v1/files 列目录 200
//! 2. PLAY API 返回 web_content_link（pan CDN HTTPS 直链）
//! 3. 对直链做 Range GET(0-1023) → 206/200，证明 httpdl 可直接消费
//!
//! 运行：
//! ```text
//! cargo run -p smart-dl-provider --example xunlei_resolve_check [-- xunlei_auth.json]
//! ```

use smart_dl_provider::xunlei::auth::load as load_auth;
use smart_dl_provider::xunlei::client::Client;

#[tokio::main]
async fn main() {
    let token_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "xunlei_auth.json".into());
    let state = load_auth(std::path::Path::new(&token_path)).unwrap_or_else(|| {
        eprintln!("登录态不存在: {token_path}，请先运行 xunlei_qr_login");
        std::process::exit(1);
    });
    let client = Client::new();

    // 1) 列根目录；若没有文件只有文件夹则进入第一个含文件的文件夹
    println!("[1] 列云盘目录…");
    let mut entries = client.list_files(&state, "").await.unwrap_or_else(|e| {
        eprintln!("列目录失败: {e}");
        std::process::exit(1);
    });
    println!("    根目录 {} 项", entries.files.len());
    for e in &entries.files {
        println!(
            "    - [{}] {} ({})",
            if e.is_folder { "目录" } else { "文件" },
            e.name,
            if e.is_folder { "-".into() } else { format!("{} B", e.size) }
        );
    }

    let mut depth = 0;
    while !entries.files.iter().any(|f| !f.is_folder) && depth < 2 {
        let Some(folder) = entries.files.iter().find(|f| f.is_folder) else { break };
        println!("[1] 进入子目录 {}…", folder.name);
        entries = client.list_files(&state, &folder.id).await.unwrap_or_else(|e| {
            eprintln!("列子目录失败: {e}");
            std::process::exit(1);
        });
        depth += 1;
    }
    let Some(file) = entries.files.iter().find(|f| !f.is_folder) else {
        eprintln!("两层深度内未找到文件，请手动指定测试文件。");
        std::process::exit(1);
    };
    println!("    选中文件: {} ({}B)", file.name, file.size);

    // 2) PLAY API 取直链
    println!("[2] 调 PLAY API 取直链…");
    let play = client.resolve_link(&state, &file.id).await.unwrap_or_else(|e| {
        eprintln!("取链失败: {e}");
        std::process::exit(1);
    });
    let url = &play.web_content_link;
    if url.is_empty() {
        eprintln!("web_content_link 为空（可能需要会员或该类型不支持 PLAY）");
        std::process::exit(1);
    }
    // 解析 host 与过期/大小参数
    let host = url.split('/').nth(2).unwrap_or("?").to_string();
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    let get_param = |k: &str| {
        query.split('&').find_map(|kv| kv.strip_prefix(&format!("{k}=")))
    };
    println!("    直链 host: {host}");
    println!("    过期(e=): {:?}  大小(f=): {:?}", get_param("e"), get_param("f"));

    // 3) Range 下载前 1KB 验证可下性
    println!("[3] Range GET 验证可下性…");
    let http = reqwest::Client::new();
    let resp = http
        .get(url)
        .header("Range", "bytes=0-1023")
        .send()
        .await
        .unwrap_or_else(|e| {
            eprintln!("下载请求失败: {e}");
            std::process::exit(1);
        });
    let status = resp.status().as_u16();
    let len = resp.bytes().await.map(|b| b.len()).unwrap_or(0);
    println!("    HTTP {status}, 收到 {len} 字节");
    if status == 206 || status == 200 {
        println!();
        println!("✅ 云端直链可被普通 HTTP 客户端消费——httpdl 引擎可直接下载该类链接。");
        match get_param("e") {
            Some(e) => println!("   注意：链接带过期时间(e={e})，过期后需重新 resolve（UrlRefresh 能力已建模）。"),
            None => println!("   注意：链接未显式携带过期参数。"),
        }
    } else {
        eprintln!("❌ 非 206/200，直链可能绑定 UA/Cookie，需进一步分析响应头。");
        std::process::exit(1);
    }
}
