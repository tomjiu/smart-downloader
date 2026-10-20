//! daemon 进程生命周期实机测试（Issue #121 维度 C-2 的 daemon 等价物，
//! unix-only：SIGTERM 发送依赖 nix；Windows 形态见文件头注释）。
//!
//! REALTEST-PLAN C-2 的「关窗后 daemon 退出」面向桌面壳（Job Object /
//! PDEATHSIG）——当前仓库形态为 daemon 独立进程 + Web UI（无壳），
//! 故以进程级等价物覆盖：lockfile 互斥 → SIGTERM 优雅退出 → 端口复用。
//! 桌面壳上线后在 scripts/test-desktop-lifecycle.ps1 补壳进程场景。

#![cfg(unix)]

use std::time::Duration;

/// 真实二进制进程级验证：
/// ① 启动 → /health 200；② 同 storage 第二实例被 lockfile 拒绝；
/// ③ SIGTERM → 优雅退出（进程终结）；④ 端口可立即复用。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c2_daemon_process_lifecycle() {
    tokio::time::timeout(Duration::from_secs(90), async {
        let dir = tempfile::tempdir().unwrap();
        let port = pick_free_port();
        let addr = format!("127.0.0.1:{port}");
        let cfg = dir.path().join("daemon.toml");
        std::fs::write(
            &cfg,
            format!(
                "[server]\naddr = \"{addr}\"\n\
                 [storage]\ntasks_path = \"{}\"\n\
                 [lock]\npath = \"{}\"\n",
                dir.path().join("tasks.json").display(),
                dir.path().join("daemon.lock").display()
            ),
        )
        .unwrap();

        let exe = env!("CARGO_BIN_EXE_smart-dl-daemon");
        let mut cmd = std::process::Command::new(exe);
        cmd.args(["serve", "--config", cfg.to_str().unwrap()]);
        let mut child = cmd.spawn().expect("启动 daemon 二进制（CARGO_BIN_EXE）");

        // ① 起来了（/health 200）
        let client = reqwest::Client::new();
        let mut healthy = false;
        for _ in 0..50 {
            if let Ok(r) = client.get(format!("http://{addr}/health")).send().await {
                if r.status() == 200 {
                    healthy = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert!(healthy, "daemon 5s 内未就绪（/health）");

        // ② 同 storage 第二实例：lockfile 互斥 → 立即退出（非 0）
        let mut second = std::process::Command::new(exe);
        second.args(["serve", "--config", cfg.to_str().unwrap()]);
        let second = second.spawn().expect("第二实例 spawn");
        let out = second.wait_with_output().unwrap();
        assert!(
            !out.status.success(),
            "同 storage 第二实例必须被 lockfile 拒绝（单实例互斥）"
        );

        // ③ SIGTERM → 优雅退出
        kill_sigterm(child.id());

        let exited = wait_child_exit(&mut child, Duration::from_secs(15)).await;
        assert!(exited, "SIGTERM 后 daemon 必须退出（15s 宽限）—— 疑似挂死");

        // ④ 端口可立即复用
        let listener = tokio::net::TcpListener::bind(&addr).await;
        assert!(listener.is_ok(), "daemon 退出后端口 {addr} 必须立即可复用");
    })
    .await
    .expect("超时 —— daemon 生命周期验证挂死");
}

fn pick_free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn kill_sigterm(pid: u32) {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), Signal::SIGTERM).expect("SIGTERM 发送失败");
}

/// 轮询等待子进程退出（tokio 兼容：非阻塞轮询 child.try_wait）。
async fn wait_child_exit(child: &mut std::process::Child, budget: Duration) -> bool {
    let deadline = std::time::Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(_) => return false,
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
