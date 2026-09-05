//! B-1 HTTP API：`POST /bt/metadata`（magnet → .torrent 抓取）。
//! - feature `bt`：400（坏 magnet）/ 408（不可达 infohash 超时）语义；
//!   成功路径（真实 seeder）由 btcore/tests/magnet_metadata.rs 覆盖。
//! - 无 `bt`：端点恒 400（提示编译开关）。

mod common;
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use std::sync::Arc;

const FAKE_IH: &str = "0123456789abcdef0123456789abcdef01234567";

/// 端点测试互斥：/bt/metadata 的单并发门禁是进程级 static，而本文件多个测试
/// 的断言路径在门禁 acquisition 之后（如坏 magnet 的 parse 在 fetch 内部）——
/// 并行跑时会互吃 409/400（GitHub runner 双核实测复现：bad_magnet 得 409）。
/// 所有触达该端点的测试先拿这把锁串行执行，互斥于本地/CI 调度顺序。
static ENDPOINT_SER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// —— 无 bt 构建：端点恒 400 ——
#[cfg(not(feature = "bt"))]
async fn serve() -> std::net::SocketAddr {
    let http = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let state = Arc::new(DaemonState::new(Arc::new(http), vec![]));
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

/// —— 无 bt 构建：端点恒 400 ——
#[cfg(not(feature = "bt"))]
#[tokio::test]
async fn metadata_endpoint_disabled_without_bt() {
    let _ser = ENDPOINT_SER.lock().await;
    let addr = serve().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/bt/metadata"))
        .json(&serde_json::json!({ "magnet": format!("magnet:?xt=urn:btih:{FAKE_IH}") }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("BT 引擎"));
}

/// —— V15：save_to 落盘路径校验（纯路径校验，无 bt 也可跑）——

#[test]
fn validate_save_dest_rejects_unsafe_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for bad in [
        "",                       // 空
        "/etc/crontab",           // 绝对路径（任意路径写入）
        "../escape.torrent",      // 穿越
        "a/../../escape.torrent", // 混合穿越
    ] {
        assert!(
            http::validate_save_dest(root, bad).is_err(),
            "save_to={bad:?} 应拒绝"
        );
    }
}

#[test]
fn validate_save_dest_accepts_relative_within_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // 单分量（父目录 = 根本身）
    let p = http::validate_save_dest(root, "x.torrent").unwrap();
    assert_eq!(p, root.join("x.torrent"));
    // 多级相对路径：父目录须已存在（保留原契约）
    std::fs::create_dir_all(root.join("sub/dir")).unwrap();
    let p = http::validate_save_dest(root, "sub/dir/x.torrent").unwrap();
    assert_eq!(p, root.join("sub/dir/x.torrent"));
    // ./ CurDir 分量放行（与 PR #5 sanitize_rel 语义一致）
    let p = http::validate_save_dest(root, "./x.torrent").unwrap();
    assert_eq!(p, root.join("./x.torrent"));
    // 父目录不存在 → 拒绝（原契约）
    assert!(http::validate_save_dest(root, "no/such/dir/x.torrent").is_err());
}

#[cfg(unix)]
#[test]
fn validate_save_dest_blocks_symlink_escape() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("out")).unwrap();
    // root/out 指向根外 → canonicalize 前缀校验必须拦截
    assert!(http::validate_save_dest(root, "out/evil.torrent").is_err());
}

/// H-2（CWE-59）：末段 symlink 写穿逃逸——parent canonicalize 只覆盖中间
/// 分量，dest 本身为链接时此前全部检查照过、后续 fs::write 写到根外。
#[cfg(unix)]
#[test]
fn validate_save_dest_blocks_final_component_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim.txt");
    std::fs::write(&victim, b"do-not-touch").unwrap();

    // 形态 1：单分量 —— root/evil.torrent → 根外文件（父目录 = 根，canonicalize 必过）
    std::os::unix::fs::symlink(&victim, root.join("evil.torrent")).unwrap();
    assert!(
        http::validate_save_dest(root, "evil.torrent").is_err(),
        "末段 symlink 必须拒绝"
    );
    // 链接目标未被触碰（校验层不写文件；防未来误改成 follow 写）
    assert_eq!(std::fs::read(&victim).unwrap(), b"do-not-touch");

    // 形态 2：多级相对路径的末段 —— 父目录链真实存在（canonicalize 通过），
    // 仅最后一段是链接
    std::fs::create_dir_all(root.join("sub/dir")).unwrap();
    std::os::unix::fs::symlink(&victim, root.join("sub/dir/x.torrent")).unwrap();
    assert!(
        http::validate_save_dest(root, "sub/dir/x.torrent").is_err(),
        "多级路径的末段 symlink 同样必须拒绝"
    );
}

/// H-2 非回归：dest 已存在且为普通文件（非链接）→ 语义不变（放行，
/// 与既有 fs::write 覆盖写契约一致）。
#[test]
fn validate_save_dest_allows_regular_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("existing.torrent"), b"old").unwrap();
    let p = http::validate_save_dest(root, "existing.torrent").unwrap();
    assert_eq!(p, root.join("existing.torrent"));
}

/// —— S1（V17 回归）：/bt/metadata 必须在 auth_mw 覆盖内 ——
/// 端点在 router_base! 内，auth_mw 挂 router 末尾覆盖全部路由（merge b6c408f
/// 已实证）。本测试锁定「认证优先于 handler 语义」契约：若日后路由被挪出
/// 认证层，此处会以 400/408 取代 401 而失败。双构建（有/无 bt）均跑。
async fn serve_with_token() -> std::net::SocketAddr {
    let http = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
    let state = Arc::new(
        DaemonState::new(Arc::new(http), vec![]).with_http_token(Some("s3cret-token".to_string())),
    );
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn metadata_endpoint_requires_auth_when_token_configured() {
    let _ser = ENDPOINT_SER.lock().await;
    let addr = serve_with_token().await;
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/bt/metadata");
    // 坏 magnet：认证放行后 handler 应快速 400（而非 401/408）
    let body = serde_json::json!({ "magnet": "magnet:?dn=no-hash" });

    // 1) 无 Authorization 头 → 401（认证层固定文案；handler 未执行）
    let r = client.post(&url).json(&body).send().await.unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED);
    let b: serde_json::Value = r.json().await.unwrap();
    assert!(
        b["error"].as_str().unwrap().contains("unauthorized"),
        "401 应来自认证层: {b}"
    );

    // 2) 错误 token → 401
    let r = client
        .post(&url)
        .header("Authorization", "Bearer wrong-token")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED);

    // 3) Bearer 前缀大小写敏感（规范要求，verify_http_token 用 ct_eq）
    let r = client
        .post(&url)
        .header("Authorization", "bearer s3cret-token")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED);

    // 4) 正确 token → 认证放行，走到 handler 400（双构建语义分别断言）。
    // 注意：用「非法 save_to」这条 handler 内门禁之前的快速失败路径——magnet
    // 解析在门禁之后，bt 构建下并行跑的 busy_gate 测试持有进程级单并发门禁
    // 时，正常 magnet 请求会先吃 409（与认证无关），会污染本断言。
    let r = client
        .post(&url)
        .header("Authorization", "Bearer s3cret-token")
        .json(&serde_json::json!({
            "magnet": format!("magnet:?xt=urn:btih:{FAKE_IH}"),
            "save_to": "../escape.torrent",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "正确 token 应通过认证并到达 handler"
    );
    let b: serde_json::Value = r.json().await.unwrap();
    let msg = b["error"].as_str().unwrap_or_default();
    #[cfg(not(feature = "bt"))]
    assert!(
        msg.contains("BT 引擎"),
        "无 bt 构建：handler 400 语义（BT 引擎未编译），得到: {msg}"
    );
    #[cfg(feature = "bt")]
    assert!(
        msg.contains("save_to"),
        "bt 构建：save_to 前置校验 400 语义（门禁前路径），得到: {msg}"
    );
}

/// —— S2：magnet 抓取 scratch 遗留清理（纯 fs，双构建可跑）——

#[test]
fn stale_magnet_scratch_cleanup_pid_and_mtime_guards() {
    let tmp = std::env::temp_dir();
    let pid = std::process::id();
    // 外来 PID 残骸（模拟 kill -9；PID 取 u32 高位段，不会是真实存活进程）
    let stale_foreign = tmp.join("smart-dl-magnet-fetch-4294967290-42");
    // 本进程 scratch（模拟活跃抓取）：任何情况下不得删除
    let active_current = tmp.join(format!("smart-dl-magnet-fetch-{pid}-42"));
    std::fs::create_dir_all(&stale_foreign).unwrap();
    std::fs::create_dir_all(&active_current).unwrap();
    std::fs::write(stale_foreign.join("session"), b"x").unwrap();
    std::fs::write(active_current.join("session"), b"x").unwrap();

    // max_age=0：外来 PID 遗留必删；本进程 scratch 必留
    http::cleanup_stale_magnet_scratch_with(std::time::Duration::ZERO);
    assert!(!stale_foreign.exists(), "外来 PID 遗留应被删除");
    assert!(active_current.exists(), "本进程活跃 scratch 必须保留");

    // 大阈值：新鲜的外来 PID scratch 受 mtime 保护（并发实例/跨进程活跃抓取）
    std::fs::create_dir_all(&stale_foreign).unwrap();
    http::cleanup_stale_magnet_scratch_with(std::time::Duration::from_secs(3600));
    assert!(
        stale_foreign.exists(),
        "新鲜外来 scratch 应受 max_age 阈值保护"
    );

    // 不可归属的名字（PID 解析失败）→ 视为非本程序产物，不动
    let junk = tmp.join("smart-dl-magnet-fetch-notapid");
    std::fs::create_dir_all(&junk).unwrap();
    http::cleanup_stale_magnet_scratch_with(std::time::Duration::ZERO);
    assert!(junk.exists(), "不可解析的名字不得删除");

    // 同前缀的散文件（非目录异常残留）→ 兜底 remove_file
    let stale_file = tmp.join("smart-dl-magnet-fetch-4294967291-7");
    std::fs::write(&stale_file, b"x").unwrap();
    http::cleanup_stale_magnet_scratch_with(std::time::Duration::ZERO);
    assert!(!stale_file.exists(), "同前缀散文件残留应被兜底删除");

    // 清理测试自建工件（含被保留的两处）
    let _ = std::fs::remove_dir_all(&stale_foreign);
    let _ = std::fs::remove_dir_all(&active_current);
    let _ = std::fs::remove_dir_all(&junk);
}

#[cfg(feature = "bt")]
mod bt_enabled {
    use super::*;

    async fn serve_with_bt() -> std::net::SocketAddr {
        let dir = tempfile::tempdir().unwrap();
        let dest_root = dir.path().join("dl");
        std::fs::create_dir_all(&dest_root).unwrap();
        // V15：save_to 校验需要 canonicalize 根目录 → 目录须在测试进程存活期内
        // 有效；forget 交给测试进程生命周期（进程退出随 tmp 清理）。
        std::mem::forget(dir);
        let bt = smart_dl_daemon::bt::BtEngine::new(
            &dest_root, None, 0, 0, false, false, false, false, false, "allow",
        )
        .unwrap();
        let http = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
        let state = Arc::new(
            DaemonState::new(Arc::new(http), vec![])
                .with_bt(Arc::new(bt))
                .with_dest_root(dest_root),
        );
        let app = http::router(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    }

    #[tokio::test]
    async fn bad_magnet_is_400() {
        let _lt = crate::common::lt_gate::LT_SESSION_GATE.lock().await;
        let _ser = ENDPOINT_SER.lock().await;
        let addr = serve_with_bt().await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/bt/metadata"))
            .json(&serde_json::json!({ "magnet": "magnet:?dn=no-hash" }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "缺 xt → 400"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["error"].as_str().unwrap().contains("xt"));
    }

    #[tokio::test]
    async fn bad_peer_is_400() {
        let _lt = crate::common::lt_gate::LT_SESSION_GATE.lock().await;
        let _ser = ENDPOINT_SER.lock().await;
        let addr = serve_with_bt().await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/bt/metadata"))
            .json(&serde_json::json!({
                "magnet": format!("magnet:?xt=urn:btih:{FAKE_IH}"),
                "peers": ["not-a-sockaddr"],
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "peer 解析失败 → 400"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["error"].as_str().unwrap().contains("peers"));
    }

    #[tokio::test]
    async fn busy_gate_409_then_reusable_after_completion() {
        let _lt = crate::common::lt_gate::LT_SESSION_GATE.lock().await;
        let _ser = ENDPOINT_SER.lock().await;
        // 门禁语义（V16）：进行中 → 409；完成/取消（RAII permit drop）后端点可再用。
        // 门禁是进程级 static 单例 → 占门禁的 e2e 场景集中在本测试串行验证，
        // 避免与其他测试并行时互抢单并发锁。
        //
        // CI 稳定性（GitHub runner 实测复现）：盲等 300ms 后断言 409 是开环假设
        // —— 并行调度下 first 可能在 µs 级窗口被同进程其他测试的短暂持门者
        // 抢走 409 快速返回，或 fetch 提前错误释放门禁，second 反而持门超时
        // （408）。改为整场景 3 轮重试：健康产品 + 健康环境第 1 轮即过；
        // 调度抖动后续轮过；真回归则确定性失败。
        for attempt in 0..3u32 {
            let addr = serve_with_bt().await;
            let client = reqwest::Client::new();
            let url = format!("http://{addr}/bt/metadata");
            // 第一个请求占门禁（随机 infohash 无任何来源 → 必然超时，3s）
            let first = tokio::spawn({
                let client = client.clone();
                let url = url.clone();
                async move {
                    client
                        .post(url)
                        .json(&serde_json::json!({
                            "magnet": format!("magnet:?xt=urn:btih:{FAKE_IH}&dn=unreachable"),
                            "timeout_s": 3,
                            "dht": false,
                        }))
                        .send()
                        .await
                        .unwrap()
                }
            });
            // 等第一个请求进入抓取段（handler 同步段微秒级，300ms 裕量充足）
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            // 第二个请求：门禁被占 → 409
            let second = client
                .post(&url)
                .json(&serde_json::json!({
                    "magnet": format!("magnet:?xt=urn:btih:{FAKE_IH}"),
                    "timeout_s": 3,
                    "dht": false,
                }))
                .send()
                .await
                .unwrap();
            if second.status() != reqwest::StatusCode::CONFLICT {
                // 本轮 first 没持住门禁（见上注）→ 收尾悬留任务后弃轮重试
                let _ = first.await;
                eprintln!(
                    "busy_gate 第 {attempt} 轮未观察到 409（second={}），弃轮重试",
                    second.status()
                );
                continue;
            }
            // 第一个请求完成：超时 → 408（permit 随 handler 结束归还）
            let first = first.await.unwrap();
            assert_eq!(
                first.status(),
                reqwest::StatusCode::REQUEST_TIMEOUT,
                "超时 → 408"
            );
            let body: serde_json::Value = first.json().await.unwrap();
            assert!(body["error"].as_str().unwrap().contains("超时"));
            // 门禁已释放：坏 magnet 走到 parse 段 400（先过门禁）→ 证明端点可再用
            let third = client
                .post(&url)
                .json(&serde_json::json!({ "magnet": "magnet:?dn=no-hash" }))
                .send()
                .await
                .unwrap();
            assert_eq!(
                third.status(),
                reqwest::StatusCode::BAD_REQUEST,
                "门禁释放后可再用"
            );
            return;
        }
        panic!("busy_gate 3 轮均未观察到 409 —— 门禁语义回归或测试环境异常");
    }

    /// —— V15 e2e：save_to 越界拒绝（bt 构建；校验在抓取前快速失败）——

    #[tokio::test]
    async fn save_to_escape_is_400_before_fetch() {
        let _lt = crate::common::lt_gate::LT_SESSION_GATE.lock().await;
        let _ser = ENDPOINT_SER.lock().await;
        // V15：越界 save_to 在抓取开始前 400 快速失败（若校验回归，此处会等满
        // 5s 得到 408，断言失败）。不占门禁（校验在 try_acquire 之前）。
        let addr = serve_with_bt().await;
        let client = reqwest::Client::new();
        for bad in ["/etc/crontab", "../escape.torrent"] {
            let resp = client
                .post(format!("http://{addr}/bt/metadata"))
                .json(&serde_json::json!({
                    "magnet": format!("magnet:?xt=urn:btih:{FAKE_IH}"),
                    "timeout_s": 5,
                    "dht": false,
                    "save_to": bad,
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                reqwest::StatusCode::BAD_REQUEST,
                "save_to={bad} 应 400"
            );
            let body: serde_json::Value = resp.json().await.unwrap();
            let msg = body["error"].as_str().unwrap();
            assert!(msg.contains("save_to"), "错误消息应指向 save_to: {msg}");
        }
    }
}
