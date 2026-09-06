//! Smart Downloader 桌面端（Tauri v2 壳）：
//! 1. 启动时拉起 `smart-dl-daemon` sidecar（--ui-dir 指向打包的内嵌 UI 资源）；
//! 2. 轮询 TCP 端口就绪后打开主窗口加载 daemon URL（同源 API，无需 CORS）；
//! 3. 托盘图标（显示主窗 / 退出）+ 退出时回收 daemon 子进程。
//!
//! 布局约定（CI 打包，见 .github/workflows/desktop.yml）：
//! - sidecar：`binaries/smart-dl-daemon-<target-triple>`（tauri externalBin）
//! - UI 资源：`resources/ui/`（tauri bundle resources，源自 `ui/out`）
//!
//! 端口默认 8788，环境变量 `SMART_DL_DESKTOP_PORT` 可覆盖（避免与本机
//! 常驻 daemon 冲突）。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{Manager, State};
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;

const DEFAULT_PORT: u16 = 8788;

/// daemon sidecar 子进程句柄（退出时 kill）。
#[derive(Default)]
struct DaemonChild(Mutex<Option<CommandChild>>);

struct DaemonPort(u16);

fn port() -> u16 {
    std::env::var("SMART_DL_DESKTOP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// 轮询 daemon 监听端口直到可连（serve 在路由装配完成后才 bind——
/// TCP 可连 = 全部引擎/存储初始化完成）。
async fn wait_daemon(port: u16) -> bool {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().expect("addr");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if tokio::time::timeout(Duration::from_millis(400), tokio::net::TcpStream::connect(addr))
            .await
            .is_ok()
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

fn kill_daemon(child: &State<DaemonChild>) {
    if let Some(c) = child.0.lock().unwrap().take() {
        let _ = c.kill();
    }
}

fn main() {
    let port = port();
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(DaemonChild::default())
        .manage(DaemonPort(port))
        .setup(move |handle| {
            // —— 托盘（显示 / 退出）——
            {
                use tauri::menu::{Menu, MenuItem};
                let show =
                    MenuItem::with_id(handle, "show", "显示主窗口", true, None::<&str>)?;
                let quit = MenuItem::with_id(
                    handle,
                    "quit",
                    "退出（并停止后台下载守护）",
                    true,
                    None::<&str>,
                )?;
                let menu = Menu::with_items(handle, &[&show, &quit])?;
                tauri::tray::TrayIconBuilder::with_id("main-tray")
                    .icon(handle.default_window_icon().unwrap().clone())
                    .menu(&menu)
                    .on_menu_event(|app, ev| match ev.id.as_ref() {
                        "show" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        "quit" => app.exit(0),
                        _ => {}
                    })
                    .build(handle)?;
            }

            // —— sidecar daemon ——
            let ui_dir = handle
                .path()
                .resource_dir()
                .ok()
                .map(|p| p.join("ui"))
                .ok_or("缺少内嵌 UI 资源（resources/ui）")?;
            let child_state: State<DaemonChild> = handle.state();
            let (mut rx, child) = handle
                .shell()
                .sidecar("smart-dl-daemon")
                .map_err(|e| format!("sidecar 解析失败: {e}"))?
                .args(["serve", "--ui-dir", &ui_dir.to_string_lossy()])
                .spawn()
                .map_err(|e| format!("daemon sidecar 启动失败: {e}"))?;
            *child_state.0.lock().unwrap() = Some(child);
            tauri::async_runtime::spawn(async move {
                while let Some(ev) = rx.recv().await {
                    match ev {
                        tauri_plugin_shell::process::CommandEvent::Stderr(line) => {
                            eprintln!("[daemon] {}", String::from_utf8_lossy(&line));
                        }
                        tauri_plugin_shell::process::CommandEvent::Stdout(line) => {
                            println!("[daemon] {}", String::from_utf8_lossy(&line));
                        }
                        _ => {}
                    }
                }
            });

            // —— 等就绪 → 开窗 ——
            let app_handle = handle.app_handle().clone();
            tauri::async_runtime::spawn(async move {
                let ready = wait_daemon(port).await;
                if !ready {
                    eprintln!("daemon 监听等待超时（15s）——仍尝试打开窗口");
                }
                let url: tauri::Url = format!("http://127.0.0.1:{port}/")
                    .parse()
                    .expect("daemon url");
                if let Ok(win) = tauri::WebviewWindowBuilder::new(
                    &app_handle,
                    "main",
                    tauri::WebviewUrl::External(url),
                )
                .title("Smart Downloader")
                .inner_size(1280.0, 840.0)
                .min_inner_size(960.0, 600.0)
                .visible(false)
                .build()
                {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // 主窗关闭 = 整体退出（回收 daemon sidecar）
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_exit();
                kill_daemon(window.app_handle().state::<DaemonChild>());
                window.app_handle().exit(0);
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
