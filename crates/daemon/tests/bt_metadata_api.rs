//! B-1 HTTP API：`POST /bt/metadata`（magnet → .torrent 抓取）。
//! - feature `bt`：400（坏 magnet）/ 408（不可达 infohash 超时）语义；
//!   成功路径（真实 seeder）由 btcore/tests/magnet_metadata.rs 覆盖。
//! - 无 `bt`：端点恒 400（提示编译开关）。

use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use std::sync::Arc;

const FAKE_IH: &str = "0123456789abcdef0123456789abcdef01234567";

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
        let bt = smart_dl_daemon::bt::BtEngine::new(&dest_root, None, 0, 0, false, false, false)
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
        let addr = serve_with_bt().await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/bt/metadata"))
            .json(&serde_json::json!({ "magnet": "magnet:?dn=no-hash" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST, "缺 xt → 400");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["error"].as_str().unwrap().contains("xt"));
    }

    #[tokio::test]
    async fn bad_peer_is_400() {
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
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST, "peer 解析失败 → 400");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["error"].as_str().unwrap().contains("peers"));
    }

    #[tokio::test]
    async fn busy_gate_409_then_reusable_after_completion() {
        // 门禁语义（V16）：进行中 → 409；完成/取消（RAII permit drop）后端点可再用。
        // 门禁是进程级 static 单例 → 占门禁的 e2e 场景集中在本测试串行验证，
        // 避免与其他测试并行时互抢单并发锁。
        let addr = serve_with_bt().await;
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/bt/metadata");
        // 第一个请求占门禁（随机 infohash 无任何来源 → 必然超时，5s）
        let first = tokio::spawn({
            let client = client.clone();
            let url = url.clone();
            async move {
                client
                    .post(url)
                    .json(&serde_json::json!({
                        "magnet": format!("magnet:?xt=urn:btih:{FAKE_IH}&dn=unreachable"),
                        "timeout_s": 5,
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
                "timeout_s": 5,
                "dht": false,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(second.status(), reqwest::StatusCode::CONFLICT, "占用中 → 409");
        // 第一个请求完成：超时 → 408（permit 随 handler 结束归还）
        let first = first.await.unwrap();
        assert_eq!(first.status(), reqwest::StatusCode::REQUEST_TIMEOUT, "超时 → 408");
        let body: serde_json::Value = first.json().await.unwrap();
        assert!(body["error"].as_str().unwrap().contains("超时"));
        // 门禁已释放：坏 magnet 走到 parse 段 400（先过门禁）→ 证明端点可再用
        let third = client
            .post(&url)
            .json(&serde_json::json!({ "magnet": "magnet:?dn=no-hash" }))
            .send()
            .await
            .unwrap();
        assert_eq!(third.status(), reqwest::StatusCode::BAD_REQUEST, "门禁释放后可再用");
    }

    /// —— V15 e2e：save_to 越界拒绝（bt 构建；校验在抓取前快速失败）——

    #[tokio::test]
    async fn save_to_escape_is_400_before_fetch() {
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
