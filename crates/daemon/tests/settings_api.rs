//! S1 设置面 e2e：`GET /settings` 快照形状 + `PUT /settings` 部分更新
//! （运行时生效 + 落盘持久化 + 事件广播 + 验证失败 400 零副作用）。
//!
//! 引擎侧形态（BT 发现/传输/连接下发、HTTP 全局代理合并）为 crate 内单测 /
//! bt 门控测试覆盖；本文件聚焦 API 表面与持久化契约。

use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::path::PathBuf;
use std::sync::Arc;

type Spawned = (tempfile::TempDir, String, PathBuf);

fn base_cfg() -> smart_dl_daemon::config::Config {
    smart_dl_daemon::config::Config::default()
}

/// 组装 daemon（真实 HTTP 引擎 + 权威配置 + 持久化目标 = 临时 toml）。
/// tempdir 由调用方持有（返回元组首元素），测试全程保活。
async fn spawn_daemon(cfg: smart_dl_daemon::config::Config) -> Spawned {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("daemon.toml");
    std::fs::write(&cfg_path, cfg.to_toml_string().unwrap()).unwrap();
    let engine = HttpEngine::new(reqwest::Client::new());
    let dest = dir.path().join("downloads");
    std::fs::create_dir_all(&dest).unwrap();
    let state = DaemonState::new(Arc::new(engine), vec![])
        .with_dest_root(dest.clone())
        .with_global_limits(cfg.download.max_download_kb_s, cfg.bt.max_upload_kb_s)
        .with_config(smart_dl_daemon::config::Config::snapshot_json(
            &cfg,
            &PathBuf::from("./tasks.json"),
        ))
        .with_config_path(Some(cfg_path.clone()))
        .with_limits_cfg(cfg.limits.clone())
        .with_queue_cfg(cfg.queue.clone())
        .with_live_config(cfg);
    let state = Arc::new(state);
    let app = http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (dir, format!("http://{addr}"), cfg_path)
}

#[tokio::test]
async fn get_settings_returns_all_domains() {
    let (_dir, base, _p) = spawn_daemon(base_cfg()).await;
    let client = reqwest::Client::new();
    let s: serde_json::Value = client
        .get(format!("{base}/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for domain in [
        "bandwidth",
        "connection",
        "bittorrent",
        "download",
        "rss",
        "cleanup",
        "post_download",
        "webhook",
        "scheduler",
        "queue",
        "server",
        "provider",
        "provider_xunlei",
        "provider_quark",
        "xunlei",
        "meta",
    ] {
        assert!(s.get(domain).is_some(), "settings 快照缺域 {domain}");
    }
    assert_eq!(s["bandwidth"]["max_download_kb_s"], 0);
    assert_eq!(s["bandwidth"]["alt_enabled"], false);
    assert_eq!(s["bittorrent"]["encrypt"], "allow");
    assert_eq!(
        s["bittorrent"]["bt_available"], false,
        "非 bt 构建无 BT 引擎"
    );
    assert_eq!(
        s["queue"]["max_active_bt"], 0,
        "S1-b 默认 0 = 不限（禁用排队）"
    );
    assert!(s["meta"]["persist_path"].is_string(), "持久化路径可见");
    // B1（#214）：新接入域默认值断言（与 config.rs Default 单源对齐）。
    assert_eq!(s["rss"]["auto_refresh"], true);
    assert_eq!(s["rss"]["refresh_interval_secs"], 900);
    assert_eq!(s["rss"]["max_processed_items_per_feed"], 200);
    assert_eq!(s["download"]["fsync_per_segment"], true);
    assert_eq!(s["download"]["fsync_batch_segments"], 8);
    assert_eq!(s["download"]["fsync_batch_interval_ms"], 2000);
    assert_eq!(s["bittorrent"]["enabled"], true);
    assert!(
        s["bittorrent"]["save_path"].is_null(),
        "默认 None = 跟随 dest_root"
    );
    assert!(
        s["bittorrent"]["resolved_save_path"].is_string(),
        "解析后落盘目录恒可见"
    );
    assert_eq!(s["server"]["addr"], "127.0.0.1:8787");
    assert_eq!(s["server"]["http_token_set"], false);
    assert_eq!(s["provider"]["enabled"], false);
    assert_eq!(s["provider_xunlei"]["tier"], "web");
    assert_eq!(s["provider_quark"]["enabled"], false);
    assert_eq!(s["xunlei"]["available"], false, "默认构建无 xunlei feature");
}

/// B1（#214）：只读装配域不出明文敏感项——http_token 即使已配置也只出
/// set/unset 布尔（脱敏口径同 connection.proxy）。反例断言（全文搜不到
/// 明文）防回归。
#[tokio::test]
async fn readonly_domains_never_leak_http_token() {
    // 直接装配带鉴权的 state（spawn_daemon 不注入 http_token；serve 的
    // 注入点 = with_http_token，此处同源补齐）。
    let engine = HttpEngine::new(reqwest::Client::new());
    let state = Arc::new(
        DaemonState::new(Arc::new(engine), vec![])
            .with_http_token(Some("secret-token-xyz-174".into()))
            .with_live_config(base_cfg()),
    );
    let app = http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let body = client
        .get(format!("{base}/settings"))
        .bearer_auth("secret-token-xyz-174")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        !body.contains("secret-token-xyz-174"),
        "设置面泄漏 http_token 明文"
    );
    let s: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(s["server"]["http_token_set"], true, "set 布尔可见");
    assert!(
        s["server"].get("http_token").is_none(),
        "server 域不得出现 http_token 键"
    );
}

/// B1（#214）：rss 域三层语义——max_processed 立即生效（applied），
/// auto_refresh / refresh_interval_secs 重启生效（restart_required + 落盘）。
#[tokio::test]
async fn put_rss_domains_split_runtime_and_restart() {
    let (_dir, base, cfg_path) = spawn_daemon(base_cfg()).await;
    let client = reqwest::Client::new();

    let report: serde_json::Value = client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({
            "rss": {
                "max_processed_items_per_feed": 500,
                "auto_refresh": false,
                "refresh_interval_secs": 1800
            }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let applied = report["applied"].as_array().unwrap();
    assert!(
        applied
            .iter()
            .any(|v| v == "rss.max_processed_items_per_feed"),
        "max_processed 应在 applied：{applied:?}"
    );
    let restart = report["restart_required"].as_array().unwrap();
    assert!(
        restart.iter().any(|v| v == "rss.auto_refresh"),
        "{restart:?}"
    );
    assert!(
        restart.iter().any(|v| v == "rss.refresh_interval_secs"),
        "{restart:?}"
    );

    // 快照回读：三键均已写入 live_config（运行时值/待重启值都从快照可见）。
    let s: serde_json::Value = client
        .get(format!("{base}/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["rss"]["max_processed_items_per_feed"], 500);
    assert_eq!(s["rss"]["auto_refresh"], false);
    assert_eq!(s["rss"]["refresh_interval_secs"], 1800);

    // 落盘断言（回读解析）：三键回写 toml。
    let reloaded = smart_dl_daemon::config::Config::load(Some(&cfg_path)).unwrap();
    assert_eq!(reloaded.rss.max_processed_items_per_feed, 500);
    assert!(!reloaded.rss.auto_refresh);
    assert_eq!(reloaded.rss.refresh_interval_secs, 1800);
}

/// B1（#214）：fsync 三件套 + BT 引擎开关/独立目录——全部重启生效
/// （PUT 仅持久化；引擎运行中行为不变）。
#[tokio::test]
async fn put_fsync_and_bt_assembly_persist_only() {
    let (_dir, base, cfg_path) = spawn_daemon(base_cfg()).await;
    let client = reqwest::Client::new();

    let report: serde_json::Value = client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({
            "download": {
                "fsync_per_segment": false,
                "fsync_batch_segments": 16,
                "fsync_batch_interval_ms": 500
            },
            "bittorrent": { "enabled": false, "save_path": "/tmp/bt-b1" }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let restart = report["restart_required"].as_array().unwrap();
    for k in [
        "download.fsync_per_segment",
        "download.fsync_batch_segments",
        "download.fsync_batch_interval_ms",
        "bittorrent.enabled",
        "bittorrent.save_path",
    ] {
        assert!(restart.iter().any(|v| v == k), "缺 {k}：{restart:?}");
    }
    assert!(
        report["applied"].as_array().unwrap().is_empty(),
        "重启域不得进 applied：{:?}",
        report["applied"]
    );

    // 落盘断言：toml 回写（回读解析断言，避免多域同名字段误判）。
    let reloaded = smart_dl_daemon::config::Config::load(Some(&cfg_path)).unwrap();
    assert!(!reloaded.download.fsync_per_segment);
    assert_eq!(reloaded.download.fsync_batch_segments, 16);
    assert_eq!(reloaded.download.fsync_batch_interval_ms, 500);
    assert!(!reloaded.bt.enabled);
    assert_eq!(
        reloaded.bt.save_path,
        Some(std::path::PathBuf::from("/tmp/bt-b1"))
    );
    // 快照同步断言（resolved 跟随新目录）。
    let s: serde_json::Value = client
        .get(format!("{base}/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["download"]["fsync_per_segment"], false);
    assert_eq!(s["download"]["fsync_batch_segments"], 16);
    assert_eq!(s["download"]["fsync_batch_interval_ms"], 500);
    assert_eq!(s["bittorrent"]["enabled"], false);
    assert_eq!(s["bittorrent"]["save_path"], "/tmp/bt-b1");
    assert_eq!(s["bittorrent"]["resolved_save_path"], "/tmp/bt-b1");
}

/// B1（#214）：bt.save_path 空串 = 清除回落（None → 跟随默认目录）。
#[tokio::test]
async fn put_bt_save_path_empty_clears_to_dest_root() {
    let (_dir, base, _cfg_path) = spawn_daemon(base_cfg()).await;
    let client = reqwest::Client::new();
    // 先设一个独立目录，再空串清除。
    client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({ "bittorrent": { "save_path": "/tmp/bt-orig" } }))
        .send()
        .await
        .unwrap();
    let s: serde_json::Value = client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({ "bittorrent": { "save_path": "" } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let _ = s; // report 形状已由上一测覆盖；此处验证快照
    let snap: serde_json::Value = client
        .get(format!("{base}/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        snap["bittorrent"]["save_path"].is_null(),
        "空串应清除 save_path"
    );
    let resolved = snap["bittorrent"]["resolved_save_path"].as_str().unwrap();
    assert!(
        resolved.ends_with("downloads"),
        "清除后应回落 dest_root（downloads）：{resolved}"
    );
    // 落盘断言（回读解析）：save_path 已清回 None。
    let reloaded = smart_dl_daemon::config::Config::load(Some(&_cfg_path)).unwrap();
    assert!(reloaded.bt.save_path.is_none(), "toml 应已清除 save_path");
}

/// B1（#214）：越界值 400 零副作用（fsync/rss 合法域从严拒绝）。
#[tokio::test]
async fn put_b1_out_of_range_rejected_zero_side_effect() {
    let (_dir, base, cfg_path) = spawn_daemon(base_cfg()).await;
    let client = reqwest::Client::new();
    for (body, key) in [
        (
            serde_json::json!({ "download": { "fsync_batch_segments": 0 } }),
            "fsync_batch_segments 下限",
        ),
        (
            serde_json::json!({ "download": { "fsync_batch_interval_ms": 50 } }),
            "fsync_batch_interval_ms 下限",
        ),
        (
            serde_json::json!({ "rss": { "refresh_interval_secs": 30 } }),
            "refresh_interval_secs 下限",
        ),
        (
            serde_json::json!({ "rss": { "max_processed_items_per_feed": 0 } }),
            "max_processed 下限",
        ),
    ] {
        let resp = client
            .put(format!("{base}/settings"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "{key} 应 400");
    }
    // 零副作用：快照与落盘均保持默认（回读解析断言，避免初始全字段
    // 序列化的字符串误判）。
    let s: serde_json::Value = client
        .get(format!("{base}/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["download"]["fsync_batch_segments"], 8);
    assert_eq!(s["rss"]["refresh_interval_secs"], 900);
    assert_eq!(s["rss"]["max_processed_items_per_feed"], 200);
    let reloaded = smart_dl_daemon::config::Config::load(Some(&cfg_path)).unwrap();
    assert_eq!(
        reloaded.download.fsync_batch_segments, 8,
        "越界 PUT 不得改值"
    );
    assert_eq!(reloaded.rss.refresh_interval_secs, 900, "越界 PUT 不得改值");
    assert_eq!(
        reloaded.rss.max_processed_items_per_feed, 200,
        "越界 PUT 不得改值"
    );
}

#[tokio::test]
async fn put_bandwidth_applies_persists_and_broadcasts() {
    let (_dir, base, cfg_path) = spawn_daemon(base_cfg()).await;
    let client = reqwest::Client::new();

    let resp = client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({
            "bandwidth": {
                "max_download_kb_s": 2048,
                "max_upload_kb_s": 512,
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "PUT /settings 应成功");
    let report: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(report["persisted"], true, "有 --config → 默认落盘");
    let applied = report["applied"].as_array().unwrap();
    assert!(applied.contains(&serde_json::json!("bandwidth.max_download_kb_s")));
    assert!(applied.contains(&serde_json::json!("bandwidth.max_upload_kb_s")));
    assert_eq!(report["limits"]["max_download_kb_s"], 2048);

    // GET /settings 回读（基准 + 生效值）
    let s: serde_json::Value = client
        .get(format!("{base}/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["bandwidth"]["max_download_kb_s"], 2048);
    assert_eq!(s["bandwidth"]["effective_max_download_kb_s"], 2048);

    // 落盘：文件重读可解析且值一致（round-trip 契约）
    let text = std::fs::read_to_string(&cfg_path).unwrap();
    let reloaded = smart_dl_daemon::config::Config::load(Some(&cfg_path)).unwrap();
    assert_eq!(reloaded.download.max_download_kb_s, 2048);
    assert_eq!(reloaded.bt.max_upload_kb_s, 512);
    assert!(text.contains("[download]"), "回写为 TOML 分节格式");

    // global_limits_changed + settings_changed 双事件
    let events: serde_json::Value = client
        .get(format!(
            "{base}/events?type=global_limits_changed,settings_changed"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let list = events["events"].as_array().unwrap();
    let types: Vec<&str> = list
        .iter()
        .map(|e| e["event"]["type"].as_str().unwrap())
        .collect();
    assert!(types.contains(&"global_limits_changed"), "types={types:?}");
    assert!(types.contains(&"settings_changed"), "types={types:?}");
}

#[tokio::test]
async fn put_alt_limits_switches_effective_immediately() {
    let (_dir, base, _p) = spawn_daemon(base_cfg()).await;
    let client = reqwest::Client::new();

    // 开启全天候备用限速（00:00-23:59 覆盖任意当前时刻；空 days = 每天）
    let resp = client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({
            "bandwidth": {
                "alt_enabled": true,
                "alt_max_download_kb_s": 100,
                "alt_max_upload_kb_s": 50,
                "alt_from": "00:00",
                "alt_to": "23:59",
                "alt_days": [],
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let report: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(report["limits"]["max_download_kb_s"], 100, "备用值立即生效");
    assert_eq!(report["limits"]["max_upload_kb_s"], 50);

    let s: serde_json::Value = client
        .get(format!("{base}/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["bandwidth"]["alt_active_now"], true);
    assert_eq!(s["bandwidth"]["max_download_kb_s"], 0, "基准值保持不变");
    assert_eq!(s["bandwidth"]["effective_max_download_kb_s"], 100);

    // 关闭 → 回基准
    let resp = client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({ "bandwidth": { "alt_enabled": false } }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let s: serde_json::Value = client
        .get(format!("{base}/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["bandwidth"]["alt_active_now"], false);
    assert_eq!(s["bandwidth"]["effective_max_download_kb_s"], 0);
}

#[tokio::test]
async fn invalid_settings_rejected_with_400_zero_side_effect() {
    let (_dir, base, cfg_path) = spawn_daemon(base_cfg()).await;
    let client = reqwest::Client::new();
    let before = std::fs::read_to_string(&cfg_path).unwrap();

    for bad in [
        serde_json::json!({ "bittorrent": { "encrypt": "aggressive" } }),
        serde_json::json!({ "bandwidth": { "alt_from": "25:00" } }),
        serde_json::json!({ "bandwidth": { "alt_days": [7] } }),
        serde_json::json!({ "connection": { "proxy": "ftp://x:1" } }),
        serde_json::json!({ "download": { "dest_root": " " } }),
    ] {
        let resp = client
            .put(format!("{base}/settings"))
            .json(&bad)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "非法请求应 400: {bad}");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["error"].as_str().is_some(), "错误信息可读");
    }

    // 零副作用：文件未动、快照未变
    let after = std::fs::read_to_string(&cfg_path).unwrap();
    assert_eq!(before, after, "验证失败不落盘");
}

#[tokio::test]
async fn bittorrent_and_queue_and_misc_domains_apply() {
    let (_dir, base, cfg_path) = spawn_daemon(base_cfg()).await;
    let client = reqwest::Client::new();

    let resp = client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({
            "bittorrent": { "enable_dht": true, "encrypt": "require" },
            "queue": { "max_active_bt": 5 },
            "scheduler": { "start_jitter_seconds": 30 },
            "webhook": { "url": "http://hook.example/done" },
            "cleanup": { "auto_remove_completed_days": 7 },
            "post_download": { "move_to": "/tmp/moved" },
            "download": { "disk_precheck_strict": true },
            "persist": false,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let report: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(report["persisted"], false, "body persist=false 不落盘");
    let applied = report["applied"].as_array().unwrap();
    for key in [
        "bittorrent.enable_dht",
        "bittorrent.encrypt",
        "queue.max_active_bt",
        "scheduler.start_jitter_seconds",
        "webhook.url",
        "cleanup.auto_remove_completed_days",
        "post_download.move_to",
    ] {
        assert!(applied.contains(&serde_json::json!(key)), "缺 {key}");
    }
    // 非热更键 → restart_required
    let rr = report["restart_required"].as_array().unwrap();
    assert!(rr.contains(&serde_json::json!("download.disk_precheck_strict")));

    let s: serde_json::Value = client
        .get(format!("{base}/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["bittorrent"]["enable_dht"], true);
    assert_eq!(s["bittorrent"]["encrypt"], "require");
    assert_eq!(s["queue"]["max_active_bt"], 5);
    assert_eq!(s["webhook"]["url"], "http://hook.example/done");

    // persist=false → 文件保持原样
    let text = std::fs::read_to_string(&cfg_path).unwrap();
    let reloaded = smart_dl_daemon::config::Config::load(Some(&cfg_path)).unwrap();
    assert_eq!(
        reloaded.queue.max_active_bt, 0,
        "未落盘：文件值不变（默认 0 = 不限）"
    );
    let _ = text;
}

#[tokio::test]
async fn proxy_and_dest_root_apply() {
    let (_dir, base, _p) = spawn_daemon(base_cfg()).await;
    let client = reqwest::Client::new();

    let resp = client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({
            "connection": { "proxy": "socks5://127.0.0.1:1080" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "合法 socks5 代理应通过");
    let s: serde_json::Value = client
        .get(format!("{base}/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["connection"]["proxy"], "socks5://127.0.0.1:1080");

    // 清除代理（空串）
    let resp = client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({ "connection": { "proxy": "" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // dest_root：目录创建 + 白名单追加 + 快照刷新
    let new_root = format!("{}/newdest", std::env::temp_dir().display());
    let resp = client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({ "download": { "dest_root": new_root } }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let s: serde_json::Value = client
        .get(format!("{base}/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["download"]["dest_root"], new_root);
    let snap: serde_json::Value = client
        .get(format!("{base}/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snap["dest_root"], new_root, "/config 快照跟随");
}

#[tokio::test]
async fn bittorrent_extra_trackers_and_ratio_apply() {
    // Task 38 qbit 对标：extra_trackers（新任务自动追加 tracker）+
    // max_share_ratio（做种分享率上限）——PUT 应用 + 快照回读 + 校验拒绝。
    let (_dir, base, _p) = spawn_daemon(base_cfg()).await;
    let client = reqwest::Client::new();

    let resp = client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({
            "bittorrent": {
                "extra_trackers": [
                    "udp://tracker.example:6969/announce",
                    "http://t2.example/announce"
                ],
                "max_share_ratio": 2.5,
                "max_seeding_time_min": 120
            },
            "persist": false,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{:?}", resp.text().await.unwrap());
    let report: serde_json::Value = resp.json().await.unwrap();
    let applied = report["applied"].as_array().unwrap();
    assert!(applied.contains(&serde_json::json!("bittorrent.extra_trackers")));
    assert!(applied.contains(&serde_json::json!("bittorrent.max_share_ratio")));
    assert!(applied.contains(&serde_json::json!("bittorrent.max_seeding_time_min")));

    let s: serde_json::Value = client
        .get(format!("{base}/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        s["bittorrent"]["extra_trackers"].as_array().unwrap().len(),
        2
    );
    assert_eq!(s["bittorrent"]["max_share_ratio"], 2.5);
    assert_eq!(s["bittorrent"]["max_seeding_time_min"], 120);

    // 负值 ratio → 400 零副作用
    let resp = client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({ "bittorrent": { "max_share_ratio": -1.0 } }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 超上限 ratio → 400
    let resp = client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({ "bittorrent": { "max_share_ratio": 10000.0 } }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 超上限 seeding time → 400
    let resp = client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({ "bittorrent": { "max_seeding_time_min": 1000000 } }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 空条目 tracker → 400
    let resp = client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({ "bittorrent": { "extra_trackers": ["  "] } }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 0.0 = 关闭上限，合法
    let resp = client
        .put(format!("{base}/settings"))
        .json(&serde_json::json!({ "bittorrent": { "max_share_ratio": 0.0 } }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}
