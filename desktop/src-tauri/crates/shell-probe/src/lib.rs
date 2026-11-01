//! 桌面壳纯逻辑（#212 抽出）：daemon 一次性身份验证探针 + 就绪失败文案。
//!
//! 独立 crate（无 tauri 依赖）的原因：桌面壳主 crate 链接 webkit2gtk /
//! appindicator 等系统库，常规开发机与 CI 的 rust job 没有这些原生依赖
//! 无法编译——把可单测的纯逻辑（tokio + std）拆出来，
//! `cargo test -p smart-dl-shell-probe` 在任何环境可跑。tauri 胶水
//! （错误弹窗 / sidecar 拉起）留在 `src/main.rs`。
//!
//! # 背景（Issue #212）
//!
//! 旧 `wait_daemon` 探测 `GET /health` 只认状态行前缀 `HTTP/1.`：端口被
//! 残留 daemon（上次强杀壳产生的孤儿）或任意 HTTP 服务占用时假阳性——壳
//! 静默连到旧实例，UI 一切「正常」但操作对象是上一轮进程，用户不可见。
//!
//! 修复语义：壳生成一次性 nonce 经 `--instance-token` 注入本次拉起的
//! daemon；就绪探测改 `GET /instance-token/{nonce}`，仅当回 200（nonce
//! 匹配）才 Ready。非本次拉起的进程（旧实例 / 手动 serve / 外来服务）
//! 对本次 nonce 必然 404/401 → 永不 Ready → 壳 fail-fast 弹窗报错，
//! 不开窗。daemon 侧路由见 `crates/daemon/src/http.rs`
//! （`instance_token_probe`）。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// 就绪探测超时（沿用 P1-3 旧值）：本机 daemon 启动远小于此值，超时即失败。
pub const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// daemon stderr 诊断尾巴上限（错误弹窗携带，防无限增长）。
pub const STDERR_TAIL_MAX: usize = 24;

/// 就绪探测结果：失败需区分根因（#212）——「进程退出 / 401 token 模式 /
/// 端口被残留 daemon 或外来服务占用 / 无应答超时」对应不同的用户指引。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// 身份验证通过：daemon 对本次启动 nonce 回 200——可安全开窗。
    Ready,
    /// sidecar 进程在就绪前退出（Terminated 短路；最常见根因：端口被
    /// 残留 daemon 占用 → 本次 bind 失败 → 子进程立即退出，stderr 尾巴
    /// 携带具体原因）。
    ProcessExited,
    /// 超时未观察到身份验证通过。`last_status` = 探测期间最后一次 HTTP
    /// 状态码：`Some(401)` = 端口上的 daemon 配置了 http_token（桌面端
    /// 不支持该模式）；`Some(404)` 等其他码 = 端口被「对本次 nonce 恒
    /// 非 200 的进程」应答（残留 daemon / 手动 serve / 外来 HTTP 服务）；
    /// `None` = 从未得到可解析的 HTTP 应答。
    Timeout { last_status: Option<u16> },
}

/// #212：生成本次启动的一次性 nonce（128-bit hex）。
///
/// 非凭据：仅用于 daemon 回声验证「监听端口的进程 = 壳本次拉起的子进程」，
/// 持有它不授予任何 API 权限。熵源：两个 OS 随机种子 SipHash
/// （`RandomState` 键每次构造随机）+ 单调时间 + PID——区分同一机器上
/// 两次连续启动绰绰有余，无需为此引入 rand 依赖。
pub fn gen_nonce() -> String {
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut h1 = std::collections::hash_map::RandomState::new().build_hasher();
    h1.write_u64(nanos);
    h1.write_u32(std::process::id());
    let a = h1.finish();
    let mut h2 = std::collections::hash_map::RandomState::new().build_hasher();
    h2.write_u64(a ^ 0x9E37_79B9_7F4A_7C15);
    h2.write_u64(nanos.rotate_left(32));
    let b = h2.finish();
    format!("{a:016x}{b:016x}")
}

/// #212 身份验证就绪探测：`GET /instance-token/{nonce}`，仅当 daemon 回
/// 200（nonce 匹配）才 [`WaitOutcome::Ready`]。
///
/// 只解析状态行（daemon 应答 `HTTP/1.x NNN ...`，NNN 固定偏移 9..=11），
/// 不读 body——与旧探测同等轻量，免去手写 HTTP 客户端的 body/分块解析。
/// `dead`（Terminated 先于就绪到达）是唯一快速失败路径；旧实例对探针
/// 恒 404 不构成短路——由超时或 dead 兜底收口。
pub async fn wait_daemon_impl(
    port: u16,
    nonce: &str,
    dead: &AtomicBool,
    timeout: Duration,
) -> WaitOutcome {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().expect("addr");
    let deadline = Instant::now() + timeout;
    let probe = format!(
        "GET /instance-token/{nonce} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    let mut last_status: Option<u16> = None;
    while Instant::now() < deadline {
        if dead.load(Ordering::SeqCst) {
            return WaitOutcome::ProcessExited;
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
                    if let Some(code) = status_line_code(&buf[..n]) {
                        if code == 200 {
                            return WaitOutcome::Ready;
                        }
                        last_status = Some(code);
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    WaitOutcome::Timeout { last_status }
}

/// 便捷包装：默认 [`READY_TIMEOUT`]。
pub async fn wait_daemon(port: u16, nonce: &str, dead: &AtomicBool) -> WaitOutcome {
    wait_daemon_impl(port, nonce, dead, READY_TIMEOUT).await
}

/// 缓冲内解析 HTTP 状态行状态码：`HTTP/1.x NNN`（NNN 固定偏移 9..=11，
/// 需全 ASCII 数字）。不完整/畸形返回 None——视作无应答，继续轮询。
/// （数字前置校验同时防 `b'!' - b'0'` 类下溢 panic。）
fn status_line_code(buf: &[u8]) -> Option<u16> {
    if buf.len() >= 12 && buf.starts_with(b"HTTP/1.") {
        let d = &buf[9..12];
        if d.iter().all(u8::is_ascii_digit) {
            let code =
                (d[0] - b'0') as u16 * 100 + (d[1] - b'0') as u16 * 10 + (d[2] - b'0') as u16;
            return Some(code);
        }
    }
    None
}

/// #212 / #211-Q3：就绪失败 → 可行动的错误弹窗文案（区分根因 + 携带
/// daemon stderr 尾巴）。调用方保证 `outcome` 非 Ready（开窗路径不进来）。
pub fn startup_failure_message(port: u16, outcome: &WaitOutcome, stderr_tail: &[String]) -> String {
    debug_assert!(
        !matches!(outcome, WaitOutcome::Ready),
        "就绪路径不构造失败文案"
    );
    let cause: String = match outcome {
        WaitOutcome::Ready => String::new(), // 防御：调用方契约违例时不 panic
        WaitOutcome::ProcessExited => format!(
            "daemon 进程在就绪前退出（最常见：端口 {port} 被上次异常退出残留的\n\
             smart-dl-daemon 占用，导致本次监听失败）。"
        ),
        WaitOutcome::Timeout {
            last_status: Some(401),
        } => "端口上的 daemon 运行在鉴权模式（配置了 [server] http_token），\n\
              桌面端不支持该模式：请移除应用数据目录 daemon.toml 中的该配置后重试。"
            .to_string(),
        WaitOutcome::Timeout {
            last_status: Some(code),
        } => format!(
            "端口 {port} 被其他进程占用（HTTP {code} 应答，非本次启动的\n\
             smart-dl-daemon——可能是残留实例或别的服务）。"
        ),
        WaitOutcome::Timeout { last_status: None } => format!(
            "daemon 在 {} 秒内未就绪（端口 {port} 上无 HTTP 应答）。",
            READY_TIMEOUT.as_secs()
        ),
    };
    let mut m = format!(
        "Smart Downloader 后台服务未能按预期启动，已停止打开界面。\n\n\
         原因：{cause}\n\n\
         建议操作：\n\
         1. 打开任务管理器，结束所有 smart-dl-daemon 进程后重试；\n\
         2. 或设置环境变量 SMART_DL_DESKTOP_PORT 为其他端口（如 8790）。"
    );
    if !stderr_tail.is_empty() {
        m.push_str("\n\n—— daemon 输出（最后若干行）——\n");
        m.push_str(&stderr_tail.join("\n"));
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_format_and_uniqueness() {
        let a = gen_nonce();
        let b = gen_nonce();
        assert_eq!(a.len(), 32, "128-bit hex = 32 字符");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "仅 hex 字符：{a}");
        assert_ne!(a, b, "两次启动的 nonce 必须不同（身份区分的根基）");
    }

    #[test]
    fn status_line_code_parsing() {
        assert_eq!(status_line_code(b"HTTP/1.1 200 OK\r\nServer: x"), Some(200));
        assert_eq!(status_line_code(b"HTTP/1.1 404 Not Found\r\n"), Some(404));
        assert_eq!(status_line_code(b"HTTP/1.0 401 Unauthor"), Some(401));
        assert_eq!(status_line_code(b"HTTP/1.1 500"), Some(500));
        // 不完整 / 非数字（防下溢 panic 路径）/ 非 HTTP 前缀 / 空
        assert_eq!(status_line_code(b"HTTP/1.1 2"), None);
        assert_eq!(status_line_code(b"HTTP/1.1 abc"), None);
        assert_eq!(status_line_code(b"HTTP/1.1 20x"), None);
        assert_eq!(status_line_code(b"NOT-HTTP-AT-ALL\n"), None);
        assert_eq!(status_line_code(b""), None);
    }

    /// 伪造 HTTP 应答源（模拟端口上「非本次拉起」的进程行为）。
    async fn echo_status_server(status_line: &'static str) -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    break;
                };
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf).await;
                let resp = format!("{status_line}\r\nContent-Length: 0\r\n\r\n");
                let _ = s.write_all(resp.as_bytes()).await;
            }
        });
        port
    }

    #[tokio::test]
    async fn probe_ready_on_200() {
        let port = echo_status_server("HTTP/1.1 200 OK").await;
        let dead = AtomicBool::new(false);
        let out = wait_daemon_impl(port, "nonce-a", &dead, Duration::from_millis(600)).await;
        assert_eq!(out, WaitOutcome::Ready);
    }

    #[tokio::test]
    async fn probe_stale_daemon_404_not_ready() {
        // #212 主场景：残留 daemon 对「本次 nonce」恒 404 → 绝不 Ready。
        // 旧实现在此场景会假阳性开窗连旧实例。
        let port = echo_status_server("HTTP/1.1 404 Not Found").await;
        let dead = AtomicBool::new(false);
        let out = wait_daemon_impl(port, "nonce-a", &dead, Duration::from_millis(600)).await;
        assert_eq!(
            out,
            WaitOutcome::Timeout {
                last_status: Some(404)
            }
        );
    }

    #[tokio::test]
    async fn probe_token_mode_401_not_ready() {
        let port = echo_status_server("HTTP/1.1 401 Unauthorized").await;
        let dead = AtomicBool::new(false);
        let out = wait_daemon_impl(port, "nonce-a", &dead, Duration::from_millis(600)).await;
        assert_eq!(
            out,
            WaitOutcome::Timeout {
                last_status: Some(401)
            }
        );
    }

    #[tokio::test]
    async fn probe_foreign_non_http_not_ready() {
        // 端口被非 HTTP 的 TCP 服务占用（垃圾字节）→ 无状态码 → 超时
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    break;
                };
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf).await;
                let _ = s.write_all(b"NOT-HTTP-AT-ALL\n").await;
            }
        });
        let dead = AtomicBool::new(false);
        let out = wait_daemon_impl(port, "nonce-a", &dead, Duration::from_millis(500)).await;
        assert_eq!(out, WaitOutcome::Timeout { last_status: None });
    }

    #[tokio::test]
    async fn probe_dead_short_circuits_immediately() {
        // Terminated（进程退出）先于就绪 → 立即失败，不等满超时
        let dead = AtomicBool::new(true);
        let start = Instant::now();
        let out = wait_daemon_impl(1, "x", &dead, Duration::from_secs(30)).await;
        assert_eq!(out, WaitOutcome::ProcessExited);
        assert!(start.elapsed() < Duration::from_secs(1), "必须立即返回");
    }

    #[tokio::test]
    async fn probe_no_listener_times_out() {
        // 端口 1（特权端口，本机无监听）：连接立即拒绝 → 无应答超时
        let dead = AtomicBool::new(false);
        let out = wait_daemon_impl(1, "x", &dead, Duration::from_millis(400)).await;
        assert_eq!(out, WaitOutcome::Timeout { last_status: None });
    }

    #[test]
    fn failure_message_covers_root_causes() {
        // ProcessExited：含端口与「残留」指引
        let m = startup_failure_message(
            8788,
            &WaitOutcome::ProcessExited,
            &["daemon 退出: 监听 127.0.0.1:8788: 地址已被占用".to_string()],
        );
        assert!(m.contains("8788"));
        assert!(m.contains("残留"));
        assert!(m.contains("SMART_DL_DESKTOP_PORT"));
        assert!(m.contains("地址已被占用"), "stderr 尾巴必须携带进弹窗");

        // 401：指向 http_token
        let m = startup_failure_message(
            8788,
            &WaitOutcome::Timeout {
                last_status: Some(401),
            },
            &[],
        );
        assert!(m.contains("http_token"));

        // 404：端口被占
        let m = startup_failure_message(
            8788,
            &WaitOutcome::Timeout {
                last_status: Some(404),
            },
            &[],
        );
        assert!(m.contains("404"));

        // 无应答：超时秒数
        let m = startup_failure_message(8788, &WaitOutcome::Timeout { last_status: None }, &[]);
        assert!(m.contains("30"));
        // 空尾巴不拼接分隔段
        assert!(!m.contains("—— daemon 输出"));
    }
}
