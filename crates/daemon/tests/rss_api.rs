//! RSS 订阅自动下载 e2e（qbit RSS 对标，v1）：
//! 1. `POST /rss/feeds` → 立即拉取解析 → 201 {id,title,item_count}；重复 URL → 409；
//! 2. `POST /rss/rules`（must_contain）→ `POST /rss/refresh` → 命中条目自动建
//!    HTTP 任务（tag 透传）→ 任务真实下载到 Completed + 落盘一致；
//! 3. 去重：再次 refresh 不重复建任务（item.task_id 标记）；
//! 4. `must_not_contain` 排除 + Atom 条目 + 规则名空 → 400；
//! 5. rss.json 持久化（tasks.json 同目录）。

mod common;

use axum::routing::get;
use common::patterned;
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;

const ISO_SHA: &str = "fdeab9acf3710362bd2658cdc9a29e8f9c757fcf9811603a8c447cd1d9151108";

/// RSS 2.0 feed：ubuntu 条目（命中规则）+ debian 条目（被排除关键词命中）。
fn rss2_xml(host: &str) -> String {
    format!(
        r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
<title>Test Distro Feed</title>
<item><title>Ubuntu 24.04.2 Desktop amd64 iso</title><link>http://{host}/live/ubuntu-24.04.iso</link><guid isPermaLink="false">u24042</guid><pubDate>Tue, 01 Sep 2026 10:00:00 GMT</pubDate></item>
<item><title>Debian 13 alpha iso</title><link>http://{host}/live/debian-13.iso</link><guid>d13</guid></item>
</channel></rss>"#
    )
}

/// Atom feed：自闭合 link href（验证 Empty 事件路径）。
fn atom_xml(host: &str) -> String {
    format!(
        r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
<title>Atom Only Feed</title>
<entry><title>Ubuntu 25.10 iso</title><link href="http://{host}/live/ubuntu-25.10.iso" rel="alternate"/><id>a2510</id><updated>2026-09-02T00:00:00Z</updated></entry>
</feed>"#
    )
}

async fn serve_feed_host() -> String {
    let body = Arc::new(patterned(64));
    let app = axum::Router::new()
        .route(
            "/live/ubuntu-24.04.iso",
            get(move || {
                let b = body.clone();
                async move { (axum::http::StatusCode::OK, (*b).clone()) }
            }),
        )
        .route(
            "/live/debian-13.iso",
            get(move || async move { (axum::http::StatusCode::OK, b"debian".to_vec()) }),
        )
        .route(
            "/live/ubuntu-25.10.iso",
            get(move || async move { (axum::http::StatusCode::OK, b"atom".to_vec()) }),
        )
        .route(
            "/feed.xml",
            get(move |axum::extract::Host(host): axum::extract::Host| async move {
                rss2_xml(&host)
            }),
        )
        .route(
            "/atom.xml",
            get(move |axum::extract::Host(host): axum::extract::Host| async move {
                atom_xml(&host)
            }),
        )
        .route("/bad.xml", get(|| async move { "<html>not a feed</html>" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn serve_daemon(dest: std::path::PathBuf) -> String {
    let engine = HttpEngine::new(reqwest::Client::new());
    let state = DaemonState::new(Arc::new(engine), vec![])
        .with_dest_root(dest.clone())
        .with_storage(dest.join("tasks.json"));
    let state = Arc::new(state.with_bootstrap_client(reqwest::Client::new()));
    let app = http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn wait_completed(base: &str, id: &str) -> String {
    for _ in 0..100 {
        if let Ok(resp) = reqwest::get(format!("{base}/tasks/{id}")).await {
            if let Ok(v) = resp.json::<serde_json::Value>().await {
                let st = v["state"].as_str().unwrap_or("").to_string();
                if st == "Completed" || st == "Error" {
                    return st;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    "timeout".to_string()
}

#[tokio::test]
async fn rss_rule_auto_downloads_to_completed() {
    let tmp = tempfile::tempdir().unwrap();
    let host = serve_feed_host().await;
    let base = serve_daemon(tmp.path().to_path_buf()).await;
    let client = reqwest::Client::new();

    // 1. 添加订阅 → 201 + 解析元数据
    let resp = client
        .post(format!("{base}/rss/feeds"))
        .json(&serde_json::json!({ "url": format!("{host}/feed.xml") }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "add feed 应 201");
    let feed = resp.json::<serde_json::Value>().await.unwrap();
    assert_eq!(feed["item_count"], 2, "feed 应解析出 2 条");
    assert_eq!(feed["title"], "Test Distro Feed");

    // 2. 重复 URL → 409
    let resp = client
        .post(format!("{base}/rss/feeds"))
        .json(&serde_json::json!({ "url": format!("{host}/feed.xml") }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);

    // 3. 规则：命中 ubuntu、排除 alpha
    let resp = client
        .post(format!("{base}/rss/rules"))
        .json(&serde_json::json!({
            "name": "ubuntu-iso",
            "must_contain": ["ubuntu"],
            "must_not_contain": ["alpha"],
            "tags": ["rss", "linux"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "add rule 应 201");

    // 4. refresh → ubuntu 命中建任务（debian 排除）
    let resp = client
        .post(format!("{base}/rss/refresh"))
        .send()
        .await
        .unwrap();
    let refresh = resp.json::<serde_json::Value>().await.unwrap();
    assert_eq!(
        refresh["new_items"], 0,
        "添加订阅时已入库 2 条，refresh 无新增"
    );
    assert_eq!(
        refresh["matched"], 1,
        "仅 ubuntu 条目命中（debian 被 alpha 排除）"
    );
    let task_ids = refresh["task_ids"].as_array().unwrap();
    assert_eq!(task_ids.len(), 1);
    let tid = task_ids[0].as_str().unwrap().to_string();
    assert!(refresh["errors"].as_array().unwrap().is_empty());

    // 5. 任务真实下载到 Completed（全链：规则 → add_link_task_opts → 引擎）
    assert_eq!(wait_completed(&base, &tid).await, "Completed");
    // 落盘一致（ISO_SHA = patterned(64)）。RSS 建任务用条目标题作显式名（V3
    // 语义：opts.name = title），非 URL 末段派生。
    let dest_file = tmp.path().join("Ubuntu 24.04.2 Desktop amd64 iso");
    let bytes = std::fs::read(&dest_file).unwrap();
    assert_eq!(bytes.len(), 64);
    assert_eq!(sha256_hex(&bytes), ISO_SHA);
    // tag 透传
    let resp = reqwest::get(format!("{base}/tasks/{tid}")).await.unwrap();
    let snap = resp.json::<serde_json::Value>().await.unwrap();
    let tags = snap["tags"].as_array().unwrap();
    assert!(tags.contains(&serde_json::json!("rss")));
    assert!(tags.contains(&serde_json::json!("linux")));

    // 6. 去重：再 refresh 无新增任务
    let resp = client
        .post(format!("{base}/rss/refresh"))
        .send()
        .await
        .unwrap();
    let again = resp.json::<serde_json::Value>().await.unwrap();
    assert_eq!(again["new_items"], 0);
    assert_eq!(again["matched"], 0, "已处理条目不重复建任务");
    assert!(again["task_ids"].as_array().unwrap().is_empty());

    // 7. rss.json 落盘（tasks.json 同目录）
    let rss_path = tmp.path().join("rss.json");
    assert!(rss_path.exists(), "rss.json 应落盘");
    let rss_text = std::fs::read_to_string(&rss_path).unwrap();
    assert!(rss_text.contains("Test Distro Feed"));
    assert!(rss_text.contains("ubuntu-iso"));

    // 8. 列表与条目端点
    let resp = reqwest::get(format!("{base}/rss/feeds")).await.unwrap();
    let feeds = resp.json::<serde_json::Value>().await.unwrap();
    assert_eq!(feeds["feeds"].as_array().unwrap().len(), 1);
    let resp = reqwest::get(format!("{base}/rss/items?feed_id=1"))
        .await
        .unwrap();
    let items = resp.json::<serde_json::Value>().await.unwrap();
    let arr = items["items"].as_array().unwrap();
    let ubuntu = arr
        .iter()
        .find(|i| i["guid"] == "u24042")
        .expect("ubuntu 条目应在列");
    assert_eq!(ubuntu["task_id"], serde_json::json!(tid), "task_id 已落位");

    // 9. 删除 feed / rule
    let resp = client
        .delete(format!("{base}/rss/feeds/1"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let resp = client
        .delete(format!("{base}/rss/feeds/1"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let resp = client
        .delete(format!("{base}/rss/rules/1"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
}

#[tokio::test]
async fn rss_atom_feed_and_empty_rule_validation() {
    let tmp = tempfile::tempdir().unwrap();
    let host = serve_feed_host().await;
    let base = serve_daemon(tmp.path().to_path_buf()).await;
    let client = reqwest::Client::new();

    // Atom feed 添加（自闭合 link href）
    let resp = client
        .post(format!("{base}/rss/feeds"))
        .json(&serde_json::json!({ "url": format!("{host}/atom.xml") }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let feed = resp.json::<serde_json::Value>().await.unwrap();
    assert_eq!(feed["title"], "Atom Only Feed");
    assert_eq!(feed["item_count"], 1);

    // 规则校验：空关键词 → 400
    let resp = client
        .post(format!("{base}/rss/rules"))
        .json(&serde_json::json!({ "name": "bad" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 规则校验：空名 → 400
    let resp = client
        .post(format!("{base}/rss/rules"))
        .json(&serde_json::json!({ "name": " ", "must_contain": ["x"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 不存在 feed_id → 400
    let resp = client
        .post(format!("{base}/rss/rules"))
        .json(&serde_json::json!({ "name": "ghost", "must_contain": ["x"], "feed_id": 99 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 坏 feed → 400
    let resp = client
        .post(format!("{base}/rss/feeds"))
        .json(&serde_json::json!({ "url": format!("{host}/bad.xml") }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // refresh：坏 feed 单点失败不拖垮整体（errors 记录，其余正常）
    let resp = client
        .post(format!("{base}/rss/refresh"))
        .send()
        .await
        .unwrap();
    let v = resp.json::<serde_json::Value>().await.unwrap();
    assert_eq!(
        v["errors"].as_array().unwrap().len(),
        0,
        "坏 feed add 即拒不入库（fail-closed），refresh 无错误可报"
    );
    assert_eq!(v["new_items"], 0);
    assert_eq!(v["matched"], 0);
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    let out = h.finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}
