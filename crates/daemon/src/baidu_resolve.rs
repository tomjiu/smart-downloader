//! `smart-dl baidu-resolve` 命令实现（B3-a）。
//!
//! 免登录解析百度网盘分享 → 文件清单：verify（POST，实测协议）→
//! BDCLND → 分享页 meta（shareid/uk）→ share/list。
//! 协议证据见 `provider::baidu::client` 模块文档与
//! `docs/research/baidu/share_protocol.md`。
//!
//! 范围边界：dlink 直链转换需登录态（BDUSS，免登录实测 errno -6），
//! 属 B3-b；本命令当前产出为文件清单（名称/大小/md5/fs_id），可直接
//! 服务「分享里有什么」的确认场景。

use smart_dl_provider::baidu::{parse_share_link, BaiduClient};

/// 运行 baidu-resolve（main.rs 在客户端分发前拦截调用）。
pub async fn run(
    url: String,
    pwd: Option<String>,
    dir: Option<String>,
    json: bool,
) -> Result<(), String> {
    let mut link = parse_share_link(&url).ok_or_else(|| {
        "不是 pan.baidu.com 分享链接（支持 /s/1xxx 与 /share/init?surl= 形态）".to_string()
    })?;
    // URL 未带 ?pwd= 时用显式 --pwd 补缺；两者都有时 URL 优先
    if link.passcode.is_empty() {
        if let Some(p) = pwd {
            link.passcode = p;
        }
    }
    let client = BaiduClient::new();
    let (meta, root_files) = client
        .resolve_share(&link)
        .await
        .map_err(|e| format!("解析失败: {e}"))?;
    let files = match dir.as_deref() {
        Some(d) => client
            .list_dir(&meta, &link, Some(d))
            .await
            .map_err(|e| format!("列目录 {d} 失败: {e}"))?,
        None => root_files,
    };

    if json {
        let v = serde_json::json!({
            "share_id": meta.share_id,
            "uk": meta.uk,
            "passcode_required": !link.passcode.is_empty(),
            "dir": dir,
            "count": files.len(),
            "files": files,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&v).expect("baidu-resolve json 序列化")
        );
        return Ok(());
    }

    println!(
        "分享 shareid={} uk={}{}",
        meta.share_id,
        meta.uk,
        if link.passcode.is_empty() {
            "（公开分享）"
        } else {
            "（提取码已校验）"
        }
    );
    println!("文件 {} 项：", files.len());
    for f in &files {
        if f.is_dir() {
            println!("  [目录] {}", f.name);
        } else {
            println!(
                "  [文件] {}  {}  md5:{}",
                f.name,
                human(f.size_bytes()),
                if f.md5.is_empty() { "-" } else { &f.md5 }
            );
        }
    }
    if files.iter().any(|f| f.is_dir()) {
        println!("提示：用 --dir <目录名> 查看子目录内容。");
    }
    Ok(())
}

/// 人类可读大小。
fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_sizes() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(512), "512 B");
        assert_eq!(human(1082476), "1.0 MB");
        assert_eq!(human(193590884), "184.6 MB");
    }
}
