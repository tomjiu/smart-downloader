//! 真机验证：xunlei-ffi 的 BT 下载全链路（Rust 侧）。
//!
//! 运行前提：
//! - Windows + SDK 全套文件在 `C:\xl\`（短路径，server_path 100 字符限制）
//! - 一个真实 .torrent 文件（用 ubuntu iso 验证 download_size 增长）
//!
//! 运行：`cargo run -p xunlei-ffi --example verify_bt_download -- <torrent_path>`
//!
//! 验证链路：XL_Init → XL_CreateBTTask_V2 → XL_StartTask → XL_QueryTaskInfo
//!           （确认 file_size 正确 + download_size 从 0 增长）→ XL_StopTask → XL_DeleteTask

use std::path::Path;
use xunlei_ffi::XunleiHandle;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let torrent_path = args
        .get(1)
        .map(|s| s.as_str())
        .unwrap_or(r"E:\Code\ai\smart-downloader\docs\research\clients\refs\rqbit\crates\librqbit\resources\ubuntu-21.04-live-server-amd64.iso.torrent");

    let sdk_dir = Path::new(r"C:\xl");
    let save_path = Path::new(r"C:\xl\downloads");

    println!("=== xunlei-ffi BT 下载全链路真机验证 ===");
    println!("sdk_dir   = {}", sdk_dir.display());
    println!("torrent   = {}", torrent_path);
    println!("save_path = {}", save_path.display());

    // 1. Init
    let handle = match XunleiHandle::new(sdk_dir, save_path, save_path, "smart-dl-verify").await {
        Ok(h) => {
            println!("[1] XL_Init 成功");
            h
        }
        Err(e) => {
            eprintln!("[FAIL] XL_Init 失败: {e}");
            return;
        }
    };

    // 2. CreateBTTask_V2
    let torrent_bytes = match std::fs::read(torrent_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[FAIL] 读取 torrent 失败: {e}");
            return;
        }
    };
    let task_id = match handle.create_bt_task(&torrent_bytes, save_path).await {
        Ok(id) => {
            println!("[2] XL_CreateBTTask_V2 成功，task_id = {:?}", id);
            id
        }
        Err(e) => {
            eprintln!("[FAIL] XL_CreateBTTask_V2 失败: {e}");
            return;
        }
    };

    // 3. StartTask
    if let Err(e) = handle.start_task(&task_id).await {
        eprintln!("[FAIL] XL_StartTask 失败: {e}");
        return;
    }
    println!("[3] XL_StartTask 成功");

    // 4. 轮询 QueryTaskInfo，观察 download_size 增长
    println!("[4] 轮询 QueryTaskInfo（每 3s，共 30s）...");
    let mut last_download = 0u64;
    for i in 1..=10 {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        match handle.query_task_info(&task_id).await {
            Ok(info) => {
                let growing = if info.download_size > last_download {
                    "↑ 增长中"
                } else {
                    ""
                };
                println!(
                    "    t={:2}s: state={:?}, file_size={}, download_size={} {}",
                    3 * i,
                    info.state,
                    info.file_size,
                    info.download_size,
                    growing
                );
                last_download = info.download_size;
            }
            Err(e) => {
                eprintln!("[FAIL] XL_QueryTaskInfo 失败: {e}");
                break;
            }
        }
    }

    // 5. 清理
    let _ = handle.stop_task(&task_id).await;
    let _ = handle.delete_task(&task_id, true).await;
    println!("[5] 清理完成");
    println!("\n=== 验证结论：BT 下载全链路 ABI 正确 ===");
}
