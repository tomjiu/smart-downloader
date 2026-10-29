//! Smart Downloader 桌面端（Tauri v2 壳）：
//! 1. 启动时拉起 `smart-dl-daemon` sidecar（--ui-dir 指向打包的内嵌 UI 资源），
//!    并注入一次性启动 nonce（--instance-token，#212）；
//! 2. 轮询 `GET /instance-token/{nonce}` 验证「监听端口的正是本次拉起的子
//!    进程」后打开主窗口加载 daemon URL（同源 API，无需 CORS）；
//! 3. 托盘图标（显示主窗 / 退出）+ 退出时回收 daemon 子进程。
//!
//! #212 失败可见化：就绪探测失败（残留 daemon 占端口 / 子进程退出 / 超时 /
//! token 模式）不再「照样开窗静默连旧实例」——原生错误弹窗（含根因与
//! daemon stderr 尾巴）后退出。纯逻辑（探针/文案）在
//! `crates/shell-probe`（无 tauri 依赖，可独立单测）。
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

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tauri::{Manager, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;

use smart_dl_shell_probe::{
    gen_nonce, startup_failure_message, wait_daemon, WaitOutcome, STDERR_TAIL_MAX,
};

const DEFAULT_PORT: u16 = 8788;

/// daemon sidecar 子进程句柄（退出时 kill）。
#[derive(Default)]
struct DaemonChild(Mutex<Option<CommandChild>>);

struct DaemonPort(u16);

/// #212：daemon stderr 尾巴（FIFO 截断，错误弹窗诊断用）。
#[derive(Default)]
struct DaemonLogs(Mutex<VecDeque<String>>);

/// 审计修复（P2-8）：端口环境变量严格校验——原 `.parse().ok().unwrap_or(DEFAULT)`
/// 对非法值静默回退 8788（恰与「避开常驻 daemon」的初衷相反），且 `0` 可
/// 通过 parse → 壳轮询 `127.0.0.1:0` 必败 → 白屏。现非法值/0 直接报错退出。
fn port() -> u16 {
    match std::env::var("SMART_DL_DESKTOP_PORT") {
        Ok(v) => match v.trim().parse::<u16>() {
            Ok(p) if p != 0 => p,
            _ => {
                eprintln!("SMART_DL_DESKTOP_PORT 非法（{v:?}）：需 1-65535 且非 0");
                std::process::exit(2);
            }
        },
        Err(_) => DEFAULT_PORT,
    }
}

fn kill_daemon(child: &State<DaemonChild>) {
    if let Some(c) = child.0.lock().unwrap().take() {
        let _ = c.kill();
    }
}

/// #212 / #211-Q3：启动失败可见化——原生错误弹窗（Windows MessageBoxW /
/// macOS NSAlert / Linux GTK）后以非零码退出。
///
/// 旧路径：setup 返回 Err → `build().expect` panic → Windows 无控制台子系统
/// 下静默退出（进程消失，用户完全无从知晓）。弹窗需在工作线程上 blocking
/// （不能阻塞主线程），故内部 spawn 到 tauri 异步运行时。
fn fatal(app: &tauri::AppHandle, msg: String) {
    let ah = app.clone();
    tauri::async_runtime::spawn(async move {
        ah.dialog()
            .message(msg)
            .title("Smart Downloader 启动失败")
            .kind(MessageDialogKind::Error)
            .blocking_show();
        ah.exit(1);
    });
}

fn main() {
    let port = port();
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(DaemonChild::default())
        .manage(DaemonLogs::default())
        .manage(DaemonPort(port))
        .setup(move |handle| {
            // —— 托盘（显示 / 退出）——
            {
                use tauri::menu::{Menu, MenuItem};
                let show = MenuItem::with_id(handle, "show", "显示主窗口", true, None::<&str>)?;
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
            let ui_dir = match handle
                .path()
                .resource_dir()
                .ok()
                .map(|p| p.join("ui"))
                .ok_or("缺少内嵌 UI 资源（resources/ui）")
            {
                Ok(d) => d,
                Err(e) => {
                    // #211-Q3：打包/资源错误同样弹窗可见（旧：Err→panic→静默）
                    fatal(handle.app_handle(), e.to_string());
                    return Ok(());
                }
            };
            // 运行时目录：app_config_dir（Linux ~/.config/<id>；macOS ~/Library/
            // Application Support/<id>；Windows %APPDATA%/<id>）。CWD 指过去后
            // daemon 的默认相对路径全部收纳于此；目录创建失败则 fail-closed。
            let data_dir = match handle.path().app_config_dir() {
                Ok(d) => d,
                Err(e) => {
                    fatal(handle.app_handle(), format!("应用数据目录解析失败: {e}"));
                    return Ok(());
                }
            };
            if let Err(e) = std::fs::create_dir_all(&data_dir) {
                fatal(
                    handle.app_handle(),
                    format!("应用数据目录创建失败（{data_dir:?}）: {e}"),
                );
                return Ok(());
            }
            // #212：一次性启动 nonce——本次拉起的 daemon 经
            // `GET /instance-token/{nonce}` 回声验证身份；残留 daemon /
            // 手动 serve / 外来 HTTP 服务对它必然 404/401 → 壳 fail-fast。
            let nonce = gen_nonce();
            let child_state: State<DaemonChild> = handle.state();
            let spawned = handle
                .shell()
                .sidecar("smart-dl-daemon")
                .map_err(|e| format!("sidecar 解析失败: {e}"))
                .and_then(|s| {
                    s.args([
                        "serve",
                        "--addr",
                        &format!("127.0.0.1:{port}"),
                        "--ui-dir",
                        &ui_dir.to_string_lossy(),
                        "--instance-token",
                        nonce.as_str(),
                    ])
                    .current_dir(&data_dir)
                    .spawn()
                    .map_err(|e| format!("daemon sidecar 启动失败: {e}"))
                });
            let (mut rx, child) = match spawned {
                Ok(v) => v,
                Err(e) => {
                    // #211-Q3：spawn 失败可见化（旧路径见 fatal 文档注释）
                    fatal(handle.app_handle(), e);
                    return Ok(());
                }
            };
            *child_state.0.lock().unwrap() = Some(child);
            // 审计修复（P1-2）：daemon 崩溃/被杀感知——Terminated 落入 `_ => {}`
            // 被吞，壳无感知不重启不提示；至少短路就绪等待 + 日志留痕，并清空
            // 句柄避免退出时对已死进程 kill。stderr 同时入 #212 诊断尾巴。
            let daemon_dead = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let ev_handle = handle.app_handle().clone();
            let dead_flag = daemon_dead.clone();
            tauri::async_runtime::spawn(async move {
                while let Some(ev) = rx.recv().await {
                    match ev {
                        tauri_plugin_shell::process::CommandEvent::Stderr(line) => {
                            let s = String::from_utf8_lossy(&line).to_string();
                            eprintln!("[daemon] {s}");
                            // State 是借用句柄：先绑定再取锁（临时值在语句末
                            // 释放，guard 跨语句存活会 E0716——fork 桌面构建
                            // 实测抓到的编译错误）
                            let logs_state = ev_handle.state::<DaemonLogs>();
                            let mut logs = logs_state.0.lock().unwrap();
                            logs.push_back(s);
                            while logs.len() > STDERR_TAIL_MAX {
                                logs.pop_front();
                            }
                        }
                        tauri_plugin_shell::process::CommandEvent::Stdout(line) => {
                            println!("[daemon] {}", String::from_utf8_lossy(&line));
                        }
                        tauri_plugin_shell::process::CommandEvent::Terminated(_) => {
                            // 2.3.x 起 Terminated 为元组变体（携带 Option<退出状态>）；
                            // (_) 兼容任意携带形态（cargo lock 随 release 刷新漂移实锤）
                            eprintln!("[daemon] sidecar 进程已退出（崩溃/被杀）——不再等待就绪");
                            dead_flag.store(true, Ordering::SeqCst);
                            kill_daemon(&ev_handle.state::<DaemonChild>());
                        }
                        _ => {}
                    }
                }
            });

            // —— 等就绪（#212 身份验证）→ 开窗 ——
            let app_handle = handle.app_handle().clone();
            let dead_flag_wait = daemon_dead.clone();
            tauri::async_runtime::spawn(async move {
                let out = wait_daemon(port, &nonce, &dead_flag_wait).await;
                match out {
                    WaitOutcome::Ready => {
                        let url: tauri::Url = format!("http://127.0.0.1:{port}/")
                            .parse()
                            .expect("daemon url");
                        let builder = tauri::WebviewWindowBuilder::new(
                            &app_handle,
                            "main",
                            tauri::WebviewUrl::External(url),
                        )
                        .title("Smart Downloader")
                        .inner_size(1280.0, 840.0)
                        .min_inner_size(960.0, 600.0)
                        .visible(false);
                        // #213-1 窗口装饰：Windows/Linux 去系统装饰，改由 UI 侧
                        // TitleBar 组件自绘（拖拽区 + 最小化/最大化/关闭，见
                        // ui/src/components/dl/TitleBar.tsx；IPC 权限见
                        // capabilities/shell-ui.json）。macOS 刻意保留原生装饰——
                        // 红绿灯按钮/圆角/阴影是平台惯例，去装饰会全部失去；
                        // UI 侧同款探测（layout.tsx data-shell 旗标）不渲染
                        // TitleBar。`title` 仍保留：任务栏/Alt-Tab 标识来源。
                        #[cfg(not(target_os = "macos"))]
                        let builder = builder.decorations(false);
                        if let Ok(win) = builder.build() {
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                    }
                    // #212 根因③修复：就绪失败不再「照样开窗（连到旧实例或断
                    // 连态）」——回收本次子进程（若仍存活）+ 根因弹窗 + 退出。
                    _ => {
                        let tail: Vec<String> = app_handle
                            .state::<DaemonLogs>()
                            .0
                            .lock()
                            .unwrap()
                            .iter()
                            .cloned()
                            .collect();
                        let msg = startup_failure_message(port, &out, &tail);
                        eprintln!("daemon 就绪失败: {msg}");
                        kill_daemon(&app_handle.state::<DaemonChild>());
                        fatal(&app_handle, msg);
                    }
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
