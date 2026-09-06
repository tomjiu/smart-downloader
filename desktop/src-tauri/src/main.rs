//! Smart Downloader 桌面端（Tauri v2 壳）：
//! 1. 启动时拉起 `smart-dl-daemon` sidecar（--ui-dir 指向打包的内嵌 UI 资源）；
//! 2. 轮询 TCP 端口就绪后打开主窗口加载 daemon URL（同源 API，无需 CORS）；
//! 3. 托盘图标（显示主窗 / 退出）+ 退出时回收 daemon 子进程。
//!
//! 布局约定（CI 打包，见 .github/workflows/desktop.yml）：
//! - sidecar：`binaries/smart-dl-daemon-<target-triple>`（tauri externalBin）
//! - UI 资源：`resources/ui/`（tauri bundle resources，源自 `ui/out`）
//!
//! 端口契约：壳与 sidecar 同源约定——daemon 以 `--addr 127.0.0.1:<port>` 启动，
//! 壳轮询同一端口就绪后开窗指向它，零漂移。默认 8788，环境变量
//! `SMART_DL_DESKTOP_PORT` 可覆盖（避免与本机常驻 daemon 冲突）。
//!
//! 运行时目录：sidecar 的 CWD 设为应用配置目录（app_config_dir），daemon 的
//! 相对路径（daemon.toml / tasks.json / downloads/ / daemon.lock）全部收纳其中，
//! 避免打包后落到只读/系统目录。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{Manager, State};
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;

const DEFAULT_PORT: u16 = 8788;

/// daemon sidecar 子进程句柄（退出时 kill）。
#[derive(Default)]
struct DaemonChild(Mutex<Option<CommandChild>>);

struct DaemonPort(u16);

/// 审计修复（P2-8）：端口环境变量严格校验——原 `.parse().ok().unwrap_or(DEFAULT)`
/// 对非法值静默回退 8788（恰与「避开常驻 daemon」的初衷相反），且 `0` 可
/// 通过 parse → 壳轮询 `127.0.0.1:0` 必败 → 白屏。现非法值/0 直接报错退出。
fn port() -> u16 {
    match std::env::var("SMART_DL_DESKTOP_PORT") {
        Ok(v) => match v.trim().parse::<u16>() {
            Ok(p) if p != 0 => p,
            _ => {
                eprintln!(
                    "SMART_DL_DESKTOP_PORT 非法（{v:?}）：需 1-65535 且非 0"
                );
                std::process::exit(2);
            }
        },
        Err(_) => DEFAULT_PORT,
    }
}

/// 审计修复（P1-3）：就绪探测改 HTTP GET /health（原 TCP 可连即就绪——
/// 端口被外来常驻进程占用时假阳性开窗指向非 daemon；HTTP 响应行同时确认
/// 对端身份）。接收任意 HTTP 状态行：200/401（配了 token）均证明 daemon
/// 已就绪（serve 在路由装配完成后才 bind）。返回 false = 超时或 daemon
/// 进程已死（Terminated 提前短路，不再干等 30s）。
async fn wait_daemon(port: u16, dead: &AtomicBool) -> bool {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().expect("addr");
    let deadline = Instant::now() + Duration::from_secs(30);
    let probe =
        format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    while Instant::now() < deadline {
        if dead.load(Ordering::SeqCst) {
            return false;
        }
        if let Ok(Ok(mut stream)) = tokio::time::timeout(
            Duration::from_millis(800),
            tokio::net::TcpStream::connect(addr),
        )
        .await
        {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if stream.write_all(probe.as_bytes()).await.is_ok() {
                let mut buf = [0u8; 32];
                let read = tokio::time::timeout(Duration::from_millis(800), stream.read(&mut buf));
                if let Ok(Ok(n)) = read.await {
                    if n >= 7 && buf.starts_with(b"HTTP/1.") {
                        return true;
                    }
                }
            }
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
                        "quit" => {
                            // 审计修复（P1-1）：托盘退出必须先杀 daemon——
                            // AppHandle::exit 不触发窗口 CloseRequested（daemon
                            // 回收只挂在窗口关闭路径上）→ 旧实现产出僵尸 daemon
                            // （占住 8788，下次启动 sidecar bind 失败 + 壳连到旧
                            // 实例假正常）。ExitRequested 兜底（见 run 回调）另
                            // 覆盖 macOS Cmd+Q / 系统注销路径。
                            kill_daemon(&app.state::<DaemonChild>());
                            app.exit(0);
                        }
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
            // 运行时目录：app_config_dir（Linux ~/.config/<id>；macOS ~/Library/
            // Application Support/<id>；Windows %APPDATA%/<id>）。CWD 指过去后
            // daemon 的默认相对路径全部收纳于此；目录创建失败则 fail-closed。
            let data_dir = handle
                .path()
                .app_config_dir()
                .map_err(|e| format!("应用数据目录解析失败: {e}"))?;
            std::fs::create_dir_all(&data_dir)
                .map_err(|e| format!("应用数据目录创建失败（{data_dir:?}）: {e}"))?;
            let child_state: State<DaemonChild> = handle.state();
            let (mut rx, child) = handle
                .shell()
                .sidecar("smart-dl-daemon")
                .map_err(|e| format!("sidecar 解析失败: {e}"))?
                .args([
                    "serve",
                    "--addr",
                    &format!("127.0.0.1:{port}"),
                    "--ui-dir",
                    &ui_dir.to_string_lossy(),
                ])
                .current_dir(&data_dir)
                .spawn()
                .map_err(|e| format!("daemon sidecar 启动失败: {e}"))?;
            *child_state.0.lock().unwrap() = Some(child);
            // 审计修复（P1-2）：daemon 崩溃/被杀感知——Terminated 落入 `_ => {}`
            // 被吞，壳无感知不重启不提示；至少短路就绪等待 + 日志留痕，并清空
            // 句柄避免退出时对已死进程 kill。
            let daemon_dead = Arc::new(AtomicBool::new(false));
            let ev_handle = handle.app_handle().clone();
            let dead_flag = daemon_dead.clone();
            tauri::async_runtime::spawn(async move {
                while let Some(ev) = rx.recv().await {
                    match ev {
                        tauri_plugin_shell::process::CommandEvent::Stderr(line) => {
                            eprintln!("[daemon] {}", String::from_utf8_lossy(&line));
                        }
                        tauri_plugin_shell::process::CommandEvent::Stdout(line) => {
                            println!("[daemon] {}", String::from_utf8_lossy(&line));
                        }
                        tauri_plugin_shell::process::CommandEvent::Terminated => {
                            eprintln!("[daemon] sidecar 进程已退出（崩溃/被杀）——不再等待就绪");
                            dead_flag.store(true, Ordering::SeqCst);
                            kill_daemon(&ev_handle.state::<DaemonChild>());
                        }
                        _ => {}
                    }
                }
            });

            // —— 等就绪 → 开窗 ——
            let app_handle = handle.app_handle().clone();
            let dead_flag_wait = daemon_dead.clone();
            tauri::async_runtime::spawn(async move {
                let ready = wait_daemon(port, &dead_flag_wait).await;
                if !ready {
                    eprintln!(
                        "daemon 就绪等待失败（超时 30s 或进程退出）——仍打开窗口（UI 会显示连接断开态）"
                    );
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
                // Tauri 2 API：阻止默认关闭后自行 exit（顺带回收 daemon sidecar）
                api.prevent_close();
                kill_daemon(&window.app_handle().state::<DaemonChild>());
                window.app_handle().exit(0);
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // 审计修复（P1-1 兕底）：全部退出路径（托盘退出/macOS Cmd+Q/
            // 系统注销/ExitRequested）统一回收 daemon——旧实现只挂窗口
            // CloseRequested，托盘退出产出僵尸 daemon。
            if let tauri::RunEvent::ExitRequested { .. } = event {
                kill_daemon(&app.state::<DaemonChild>());
            }
        });
}
