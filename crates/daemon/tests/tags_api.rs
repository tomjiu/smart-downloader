//! E18 任务标签 e2e：`POST /tasks/:id/tags` 设置（替换式 + 归一化）→
//! 快照/列表透出 → `?tag=` any-of 过滤（大小写不敏感）→ `?search=` 命中标签
//! → 清除（null/空表）→ 超限 400 / 未知任务 404。
//! 持久化口径：tags 随 TaskMetadata 走 tasks.json（serde default 兼容旧档案），
//! 恢复/落盘由 state 层既有持久化测试覆盖。

mod common;

use common::TestServer;
use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;

async fn serve() -> (String, Arc<DaemonState>) {
    let engine = HttpEngine::new(reqwest::Client::new());
    // V2 dest 白名单：测试显式 dest 落系统临时目录 → 注入为白名单根
    let state = DaemonState::new(Arc::new(engine), vec![]).with_dest_root(std::env::temp_dir());
    let state = Arc::new(state);
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
}

async fn add_task(client: &reqwest::Client, base: &str, url: &str) -> String {
    let dest = std::env::temp_dir().join(format!(
        "e18-tags-{}-{url:x}",
        std::process::id(),
        url = fnv64(url)
    ));
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({ "url": url, "dest": dest.to_str().unwrap() }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// FNV-1a 64：种子串 → 十六进制后缀（避免并行测试重复 dest）。
fn fnv64(seed: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in seed.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[tokio::test]
async fn tags_set_filter_search_clear_full_flow() {
    let (base, _state) = serve().await;
    let client = reqwest::Client::new();
    // 本地源（add 会真实探测外网 URL，必须用 TestServer 环回源）
    let srv_a = TestServer::start(common::patterned(1024)).await;
    let srv_b = TestServer::start(common::patterned(2048)).await;

    let id_a = add_task(&client, &base, &srv_a.url()).await;
    let id_b = add_task(&client, &base, &srv_b.url()).await;

    // 设置标签（带空串/空白，验证归一化）
    let resp = client
        .post(format!("{base}/tasks/{id_a}/tags"))
        .json(&serde_json::json!({ "tags": [" Movie ", "4K", ""] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["tags"], serde_json::json!(["Movie", "4K"]), "归一化");

    // 快照透出
    let snap: serde_json::Value = client
        .get(format!("{base}/tasks/{id_a}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snap["tags"], serde_json::json!(["Movie", "4K"]));

    // 无标签任务快照省略 tags 键（噪声控制）
    let snap_b: serde_json::Value = client
        .get(format!("{base}/tasks/{id_b}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(snap_b.get("tags").is_none(), "无标签省略 tags 键");

    // 列表行透出
    let rows: serde_json::Value = client
        .get(format!("{base}/tasks"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row_a = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["task_id"] == id_a.as_str())
        .unwrap();
    assert_eq!(row_a["tags"], serde_json::json!(["Movie", "4K"]));

    // id_b 打标签（music 场景）
    let resp = client
        .post(format!("{base}/tasks/{id_b}/tags"))
        .json(&serde_json::json!({ "tags": ["Music"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // ?tag= 单值命中（大小写不敏感）
    let rows: serde_json::Value = client
        .get(format!("{base}/tasks?tag=movie"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["task_id"], id_a.as_str());

    // 多值 any-of
    let rows: serde_json::Value = client
        .get(format!("{base}/tasks?tag=movie,music"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2);

    // 与 state 过滤 AND
    let rows: serde_json::Value = client
        .get(format!("{base}/tasks?tag=movie,music&state=queued"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2);

    // ?search= 命中标签语料
    let rows: serde_json::Value = client
        .get(format!("{base}/tasks?search=4k"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["task_id"], id_a.as_str());

    // 清除（null）
    let resp = client
        .post(format!("{base}/tasks/{id_a}/tags"))
        .json(&serde_json::json!({ "tags": null }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["tags"], serde_json::json!([]));

    // 清除后 ?tag= 不再命中
    let rows: serde_json::Value = client
        .get(format!("{base}/tasks?tag=movie"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(rows.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn tags_validation_rejected() {
    let (base, _state) = serve().await;
    let client = reqwest::Client::new();
    let srv = TestServer::start(common::patterned(1024)).await;
    let id_a = add_task(&client, &base, &srv.url()).await;

    // 17 个标签 → 400
    let many: Vec<String> = (0..17).map(|i| format!("t{i}")).collect();
    let resp = client
        .post(format!("{base}/tasks/{id_a}/tags"))
        .json(&serde_json::json!({ "tags": many }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 65 字符 → 400
    let resp = client
        .post(format!("{base}/tasks/{id_a}/tags"))
        .json(&serde_json::json!({ "tags": ["a".repeat(65)] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 不存在任务 → 404
    let resp = client
        .post(format!("{base}/tasks/t999/tags"))
        .json(&serde_json::json!({ "tags": ["x"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
