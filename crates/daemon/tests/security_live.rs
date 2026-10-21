//! 安全实机测试（Issue #121 维度 D，REALTEST-PLAN §5）。
//!
//! 用裸 HTTP 客户端 / 裸 TCP 构造攻击请求打**真实 daemon**：
//! - D-1 鉴权与 CSRF：DNS rebinding（Host: evil.com）、跨源 POST
//!   （Origin: https://evil.com）、沙箱 iframe（Origin: null）、
//!   Host 缺失（HTTP/1.0 手改报文）、token 模式 401 矩阵
//! - D-2 路径穿越：dest `..`/白名单外/symlink 逃逸；name 保留名
//! - D-4 凭证安全：source 字段脱敏（query token 不泄露）、
//!   tasks.json 0600 权限
//!
//! 原则 2：断言**用户/攻击者可见效果**（403/401/400，快照无敏感值），
//! 非实现细节。全部 `tokio::time::timeout` 包裹（原则 3）。
//!
//! D-3（SSRF）现状说明：FTP PASV 内网回连 / 重定向内网策略未见实现，
//! 本轮不测未定义行为（已在 PR 记录为待办项，与 REALTEST-PLAN 的
//! 「可配限制」预期一致——需要产品决策默认策略）。

mod common;

use common::{patterned, TestServer};
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(45);

/// tokenless daemon（同源守卫生效面）。
async fn serve_tokenless() -> String {
    serve(None).await
}

/// token 模式 daemon。
async fn serve_token(token: &str) -> String {
    serve(Some(token.to_string())).await
}

async fn serve(token: Option<String>) -> String {
    let engine = HttpEngine::new(reqwest::Client::new());
    let mut state = DaemonState::new(Arc::new(engine), vec![]).with_dest_root(std::env::temp_dir());
    if let Some(t) = token {
        state = state.with_http_token(Some(t));
    }
    let state = Arc::new(state);
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// 带 Host 覆盖的 GET（reqwest 正常请求自动带 Host: 127.0.0.1:port，
/// 攻击模拟需要显式覆盖——用 HeaderMap 直接设）。
async fn get_with_host(base: &str, path: &str, host: &str) -> reqwest::Result<reqwest::Response> {
    let url: reqwest::Url = format!("{base}{path}").parse().unwrap();
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::HOST,
        reqwest::header::HeaderValue::from_str(host).unwrap(),
    );
    let client = reqwest::Client::new();
    client.get(url).headers(headers).send().await
}

async fn post_with_origin(base: &str, origin: &str) -> reqwest::Result<reqwest::Response> {
    let client = reqwest::Client::new();
    client
        .post(format!("{base}/tasks"))
        .header("Origin", origin)
        .json(&serde_json::json!({ "url": "http://127.0.0.1:1/x" }))
        .send()
        .await
}

// ============ D-1 #3：DNS rebinding ============

/// `Host: evil.com`（浏览器 rebinding 链路的攻击面）→ 403。
/// 对照组：正常 Host（127.0.0.1）→ 200。
#[tokio::test]
async fn d1_host_evil_rejected() {
    tokio::time::timeout(TIMEOUT, async {
        let base = serve_tokenless().await;

        let r = get_with_host(&base, "/tasks", "evil.com").await.unwrap();
        assert_eq!(r.status(), 403, "Host: evil.com（DNS rebinding）必须 403");

        // 对照组：本机 Host 放行（CLI/同源浏览器请求形态）
        let addr = base.trim_start_matches("http://");
        let r = get_with_host(&base, "/tasks", addr).await.unwrap();
        assert_eq!(r.status(), 200, "正常 Host 不得误伤");
    })
    .await
    .expect("超时");
}

// ============ D-1 #4：跨源 POST（CSWSH/CSRF） ============

#[tokio::test]
async fn d1_origin_cross_site_rejected() {
    tokio::time::timeout(TIMEOUT, async {
        let base = serve_tokenless().await;
        let r = post_with_origin(&base, "https://evil.com").await.unwrap();
        assert_eq!(
            r.status(),
            403,
            "Origin: https://evil.com（跨站 CSRF/CSWSH）必须 403"
        );
    })
    .await
    .expect("超时");
}

// ============ D-1 #6：Origin null（沙箱 iframe） ============

#[tokio::test]
async fn d1_origin_null_rejected() {
    tokio::time::timeout(TIMEOUT, async {
        let base = serve_tokenless().await;
        let r = post_with_origin(&base, "null").await.unwrap();
        assert_eq!(r.status(), 403, "Origin: null（沙箱 iframe）必须 403");
    })
    .await
    .expect("超时");
}

// ============ D-1 #5：Host 缺失（手改 HTTP/1.0 报文） ============

/// reqwest 恒带 Host——用裸 TCP 手写 HTTP/1.0 报文（攻击探针形态）。
/// fail-closed（P2-3）：缺失即 403。
#[tokio::test]
async fn d1_host_missing_rejected() {
    tokio::time::timeout(TIMEOUT, async {
        use tokio::io::AsyncWriteExt;
        let base = serve_tokenless().await;
        let addr = base.trim_start_matches("http://").to_string();

        let mut stream = tokio::net::TcpStream::connect(&addr).await.unwrap();
        // 手改报文：无 Host 头的 HTTP/1.0 GET（正常客户端不会出现）
        stream
            .write_all(b"GET /tasks HTTP/1.0\r\n\r\n")
            .await
            .unwrap();
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(10), async {
            use tokio::io::AsyncReadExt;
            let mut chunk = [0u8; 1024];
            loop {
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
        })
        .await;
        let head = String::from_utf8_lossy(&buf);
        let first_line = head.lines().next().unwrap_or_default();
        assert!(
            first_line.contains("403"),
            "无 Host 的 HTTP/1.0 报文必须 403（fail-closed）: {first_line}"
        );
    })
    .await
    .expect("超时");
}

// ============ D-1 #1/#2：token 模式 401 矩阵 ============

/// 无 token / 错 token / 错长度 token → 一致 401（不因 token 长度/
/// 格式差异返回不同状态码——时序与响应形状的信息泄露面）；
/// 正确 token → 200。
#[tokio::test]
async fn d1_token_auth_matrix() {
    tokio::time::timeout(TIMEOUT, async {
        let base = serve_token("live-secret-token-xyz").await;
        let client = reqwest::Client::new();

        // 无 token → 401
        let r = client.get(format!("{base}/tasks")).send().await.unwrap();
        assert_eq!(r.status(), 401, "无 token 必须 401");

        // 错 token → 401
        let r = client
            .get(format!("{base}/tasks"))
            .header("Authorization", "Bearer wrong-token")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401, "错 token 必须 401");

        // 前缀碰撞 token → 401
        let r = client
            .get(format!("{base}/tasks"))
            .header("Authorization", "Bearer live-secret-token-xy")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401, "前缀碰撞 token 必须 401");

        // 超长 token → 401（同口径，不 400/403 泄露长度语义）
        let r = client
            .get(format!("{base}/tasks"))
            .header("Authorization", &format!("Bearer {}", "a".repeat(4096)))
            .send()
            .await
            .unwrap();
        assert_eq!(
            r.status(),
            401,
            "超长错 token 必须 401（与短错 token 同响应——不泄露长度比较细节）"
        );

        // 正确 token → 200
        let r = client
            .get(format!("{base}/tasks"))
            .header("Authorization", "Bearer live-secret-token-xyz")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "正确 token 必须放行");
    })
    .await
    .expect("超时");
}

// ============ D-2：路径穿越（dest 与 name） ============

/// dest 字段攻击面（ensure_dest_root 三层：`..` 分量 / 白名单外
/// 绝对路径 / symlink 逃逸——canonicalize 前缀比对）。
#[cfg(unix)]
#[tokio::test]
async fn d2_dest_traversal_rejected() {
    tokio::time::timeout(TIMEOUT, async {
        let base = serve_tokenless().await;
        let client = reqwest::Client::new();
        let srv = TestServer::start(patterned(1024)).await;
        let allowed = tempfile::tempdir().unwrap();
        // symlink 逃逸：指向白名单外的真实目录（/etc）——canonicalize
        // 后前缀比对拦截（指向 /tmp 内目录合法放行，那是白名单语义）
        let evil_link = allowed.path().join("escape");
        std::os::unix::fs::symlink("/etc", &evil_link).unwrap();

        let evils: Vec<String> = vec![
            "../../../etc".into(),
            "a/../../b".into(),
            "/etc".into(),                   // 白名单外绝对路径
            evil_link.display().to_string(), // symlink 逃逸（→ /etc）
        ];
        let mut rejected = 0;
        for dest in &evils {
            let r = client
                .post(format!("{base}/tasks"))
                .json(&serde_json::json!({
                    "url": srv.url(),
                    "dest": dest
                }))
                .send()
                .await
                .unwrap();
            // 用户可见效果：任务不得进入系统（400 = 拒绝；201 = 攻击成功）
            assert_ne!(
                r.status(),
                201,
                "恶意 dest {dest:?} 被接受（路径穿越未拦截）"
            );
            if r.status() == 400 {
                rejected += 1;
            }
        }
        assert!(
            rejected >= 3,
            "`..`/绝对路径/symlink 至少三形态必须显式 400（实际 {rejected}）"
        );

        // 对照组：白名单内正常 dest → 201
        let r = client
            .post(format!("{base}/tasks"))
            .json(&serde_json::json!({
                "url": srv.url(),
                "dest": allowed.path().join("ok").display().to_string()
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 201, "白名单内 dest 不得误伤");
    })
    .await
    .expect("超时");
}

/// name 字段攻击面（sanitize_rel 终审：`..` 分量 / Win32 保留设备名 /
/// 尾点尾空格绕过）。实机断言：恶意 name 的 add 请求被拒。
#[tokio::test]
async fn d2_name_traversal_and_reserved_rejected() {
    tokio::time::timeout(TIMEOUT, async {
        let base = serve_tokenless().await;
        let client = reqwest::Client::new();
        let srv = TestServer::start(patterned(1024)).await;

        let evils = [
            "../../../etc/passwd",
            "a/../../b",
            "con",      // Win32 设备名
            "con .txt", // 尾随空格绕过（Win32 语义等价 con.txt）
            "COM1.txt",
            "nul.",
            "trailing.", // 尾点
        ];
        let mut ok = 0;
        for name in evils {
            let r = client
                .post(format!("{base}/tasks"))
                .json(&serde_json::json!({
                    "url": srv.url(),
                    "name": name
                }))
                .send()
                .await
                .unwrap();
            assert_ne!(
                r.status(),
                201,
                "恶意 name {name:?} 被接受（文件名穿越/保留名未拦截）"
            );
            if r.status() == 400 {
                ok += 1;
            }
        }
        assert!(ok >= 6, "7 个恶意 name 至少 6 个显式 400（实际 {ok}）");

        // 对照组：合法 name → 201
        let r = client
            .post(format!("{base}/tasks"))
            .json(&serde_json::json!({
                "url": srv.url(),
                "name": "normal-file.bin"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 201, "合法 name 不得误伤");
    })
    .await
    .expect("超时");
}

// ============ D-4 #2：source 脱敏（query token 不泄露） ============

/// 带 `?token=secret&sig=...` 的 URL 添加任务 → 列表/快照的 source
/// 字段不得包含明文 secret（浏览器开发者工具/日志侧信道可见面）。
#[tokio::test]
async fn d4_source_redacts_sensitive_query() {
    tokio::time::timeout(TIMEOUT, async {
        let base = serve_tokenless().await;
        let client = reqwest::Client::new();
        let srv = TestServer::start(patterned(1024)).await;
        let secret = "sup3r-s3cret-apikey-value";

        let r = client
            .post(format!("{base}/tasks"))
            .json(&serde_json::json!({
                "url": format!("{}?token={secret}&sig=abcd1234", srv.url())
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 201);
        let v: serde_json::Value = r.json().await.unwrap_or_default();
        let id = v["task_id"].as_str().unwrap_or_default().to_string();

        // 列表
        let list: Vec<serde_json::Value> = client
            .get(format!("{base}/tasks"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let listed = list
            .iter()
            .find(|t| t["task_id"] == *id)
            .expect("任务必须在列表");
        let src_list = listed["source"].as_str().unwrap_or_default();
        assert!(
            !src_list.contains(secret),
            "列表 source 泄露 query token: {src_list}"
        );
        assert!(
            src_list.contains("***"),
            "敏感 query 必须以 *** 脱敏显示: {src_list}"
        );

        // 快照（详情面板可见面）
        let snap: serde_json::Value = client
            .get(format!("{base}/tasks/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let src_snap = snap["source"].as_str().unwrap_or_default();
        assert!(
            !src_snap.contains(secret),
            "快照 source 泄露 query token: {src_snap}"
        );

        // 日志（events 的 detail 面可见面）
        let logs: serde_json::Value = client
            .get(format!("{base}/tasks/{id}/logs"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let logs_text = logs.to_string();
        assert!(
            !logs_text.contains(secret),
            "任务日志泄露 query token: {logs_text}"
        );
    })
    .await
    .expect("超时");
}

// ============ D-4 #1：tasks.json 权限 0600 ============

/// with_storage + 触发 autosave → tasks.json 权限必须 0600
///（V12 口径：任务源含 URL 凭据/token，落盘文件不得组/其他可读）。
#[cfg(unix)]
#[tokio::test]
async fn d4_tasks_json_permission_0600() {
    tokio::time::timeout(TIMEOUT, async {
        let engine = HttpEngine::new(reqwest::Client::new());
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("tasks.json");
        let state = DaemonState::new(Arc::new(engine), vec![])
            .with_dest_root(std::env::temp_dir())
            .with_storage(storage.clone());
        let state = Arc::new(state);
        let app = http::router(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();
        let srv = TestServer::start(patterned(1024)).await;

        // 触发一次落盘
        let r = client
            .post(format!("{base}/tasks"))
            .json(&serde_json::json!({ "url": srv.url() }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 201);
        assert!(storage.exists(), "add 后 tasks.json 必须已落盘（autosave）");

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&storage).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "tasks.json 权限必须 0600（实际 {:o}）—— 任务源含凭据",
            mode & 0o777
        );
    })
    .await
    .expect("超时");
}
