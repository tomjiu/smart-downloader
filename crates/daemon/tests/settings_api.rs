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
        "cleanup",
        "post_download",
        "webhook",
        "scheduler",
        "queue",
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
    assert_eq!(s["queue"]["max_active_bt"], 3);
    assert!(s["meta"]["persist_path"].is_string(), "持久化路径可见");
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
    assert_eq!(reloaded.queue.max_active_bt, 3, "未落盘：文件值不变");
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
