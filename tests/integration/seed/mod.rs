//! tests/integration/seed — 本地测试 seeder（自研 seed_main：生成 2MB 确定性文件并做种）
//! 约定：seed_main <port> <dir> 启动后输出一行 `SEED <magnet> PORT <port>`，随后常驻。

use std::io::{BufRead, BufReader};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use fs2::FileExt;

#[allow(dead_code)] // 诊断字段保留供排障（无调用方时静默）
pub struct TestSeeder {
    child: Child,
    magnet: String,
    port: u16,
    dir: TempDir,
}

pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new() -> std::io::Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let path =
            std::env::temp_dir().join(format!("smart-dl-test-{}-{}", std::process::id(), nanos));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

static SEED_MAIN: OnceLock<Option<PathBuf>> = OnceLock::new();

fn seed_main_path() -> Option<PathBuf> {
    SEED_MAIN
        .get_or_init(|| {
            std::env::var("SEED_MAIN")
                .ok()
                .map(PathBuf::from)
                .or_else(|| {
                    let root = std::env::current_dir().ok()?;
                    let mut p = root;
                    loop {
                        let cand = p
                            .join("ffi")
                            .join("build")
                            .join("Release")
                            .join("seed_main.exe");
                        if cand.exists() {
                            return Some(cand);
                        }
                        if !p.pop() {
                            return None;
                        }
                    }
                })
        })
        .clone()
}

fn pick_free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("bind ephemeral")
        .local_addr()
        .expect("local_addr")
        .port()
}

impl TestSeeder {
    /// 启动 seed_main；要求 02_build.ps1 已产出 seed_main.exe（M0 出口前置）
    /// stdout 重定向到临时文件（避免管道不被排空导致 seed_main 阻塞自身 session，
    /// 令 peer 连接全部断开——M0 e2e 调试实测）。
    /// 并发测试各自 pick_free_port 存在竞态（bind 0 释放后可能被复选），
    /// 子进程提前退出（如端口被占）则自动换端口重试。
    pub fn start() -> Self {
        let exe = seed_main_path().expect("seed_main.exe 未构建：先跑 scripts/m0/02_build.ps1");
        // 跨进程锁：避免 cargo test 多二进制并行时多个 seed_main 同时启动导致端口竞态
        let lock_path = std::env::temp_dir().join("smart-dl-seeder.lock");
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("open seeder lock");
        let dl = Instant::now() + Duration::from_secs(30);
        while Instant::now() < dl {
            if lock_file.try_lock_exclusive().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            Instant::now() < dl,
            "等待 seeder 跨进程锁超时（可能有残留锁文件）"
        );

        for attempt in 0..10 {
            let port = pick_free_port();
            let dir = TempDir::new().expect("tempdir");
            let save: PathBuf = dir.path().to_path_buf(); // 由 seeder 写入 2MB 测试文件
            let log = dir.path().join("seed.log");
            let logf = std::fs::File::create(&log).expect("create seed.log");
            let logf_stderr = logf.try_clone().expect("clone logf");

            let vcpkg_bin = exe
                .parent()
                .map(|p| p.join("..").join("..").join("vcpkg_installed").join("x64-windows").join("bin"))
                .unwrap_or_else(|| PathBuf::from("ffi/vcpkg_installed/x64-windows/bin"));

            let mut cmd = Command::new(&exe);
            cmd.arg(port.to_string())
                .arg(&save)
                .stdout(Stdio::from(logf))
                .stderr(Stdio::from(logf_stderr));
            if let Some(current_path) = std::env::var_os("PATH") {
                let mut paths: Vec<std::path::PathBuf> =
                    std::env::split_paths(&current_path).collect();
                paths.insert(0, vcpkg_bin.clone());
                if let Ok(new_path) = std::env::join_paths(paths.clone()) {
                    cmd.env("PATH", new_path);
                } else {
                    eprintln!("join_paths failed for paths: {:?}", paths);
                }
            } else {
                eprintln!("no PATH in environment");
            }
            eprintln!("seed_main PATH entries: vcpkg_bin={:?}", vcpkg_bin);

            let mut child = cmd.spawn().expect("spawn seed_main");
            eprintln!("spawned seed_main: pid={} port={} exe={:?}", child.id(), port, exe);

            // 30s 内从文件读到 "SEED <magnet> PORT <port>"
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if let Ok(f) = std::fs::File::open(&log) {
                    let mut lines = BufReader::new(f).lines();
                    while let Some(Ok(line)) = lines.next() {
                        let t = line.trim().to_string();
                        if t.starts_with("SEED ") {
                            let mut parts = t.split_whitespace();
                            parts.next(); // SEED
                            let magnet = parts.next().expect("magnet").to_string();
                            parts.next(); // PORT
                            let p: u16 = parts.next().expect("port").parse().expect("port num");
                            assert_eq!(p, port, "seed_main 报告的端口与请求不一致");
                            return Self {
                                child,
                                magnet,
                                port,
                                dir,
                            };
                        }
                    }
                }
                if let Some(status) = child.try_wait().ok().flatten() {
                    eprintln!("seed_main exited early: pid={} status={}", child.id(), status);
                    let _ = child.kill();
                    break; // 子进程提前退出 → 换端口重试
                }
                assert!(Instant::now() < deadline, "seed_main 30s 内未输出 SEED 行");
                std::thread::sleep(Duration::from_millis(200));
            }
            // 子进程提前退出（端口竞态等）→ 换端口重试（最多 10 次）
            let tail = std::fs::read_to_string(&log).unwrap_or_default();
            std::thread::sleep(Duration::from_millis(50));
            if attempt == 9 {
                panic!("seed_main 10 次启动失败（端口竞态？）: {}", tail);
            }
        }
        unreachable!()
    }

    pub fn magnet(&self) -> &str {
        &self.magnet
    }
    /// (ip, port) 供 lt_add_peer 直连注入
    pub fn addr(&self) -> (String, u16) {
        (
            SocketAddr::from((Ipv4Addr::LOCALHOST, self.port))
                .ip()
                .to_string(),
            self.port,
        )
    }
    /// seeder 诊断输出（seed_main 打印的 SEED-STATUS/SEED-ALERT 行）
    #[allow(dead_code)]
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("seed.log")).unwrap_or_default()
    }
}

impl Drop for TestSeeder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
