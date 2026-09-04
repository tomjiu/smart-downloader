//! E30 失败自动重试 e2e：`POST /tasks` 携带 `auto_retry`（真实 HttpEngine +
//! 恒 404 源）→ 引擎报 Error → 重试预算内回 Queued 按指数退避 → 调度循环
//! 重激活 → 预算用尽落 Failed 终态。默认（无 auto_retry）保持一次性失败
//! 语义（回归）。全链路：API add → poll 轮询 → fail_or_schedule_retry →
//! activate_due_tasks → 引擎重试。

use smart_dl_daemon::http;
use smart_dl_daemon::state::DaemonState;
use smart_dl_httpdl::HttpEngine;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 组装 daemon + 100ms 加速轮询/调度循环（生产为 2s/1s 周期，测试加速收敛）。
/// 轮询面（poll_engine_states）也挂进循环——生产由 http_events 循环驱动，
/// 这里为了确定性直接全速跑。
async fn serve_with_scheduler() -> (std::net::SocketAddr, Arc<DaemonState>) {
    let engine = HttpEngine::new(reqwest::Client::new());
    let state = DaemonState::new(Arc::new(engine), vec![]).with_dest_root(std::env::temp_dir());
    let state = Arc::new(state);
    let app = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let st = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = st.poll_engine_states().await;
            let _ = st.activate_due_tasks().await;
        }
    });
    (addr, state)
}

async fn get_snapshot(client: &reqwest::Client, base: &str, tid: &str) -> serde_json::Value {
    client
        .get(format!("{base}/tasks/{tid}"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()
}

/// 轮询直到谓词成立（护栏 30s；默认 200ms 步进）。
async fn wait_until<F>(
    client: &reqwest::Client,
    base: &str,
    tid: &str,
    mut pred: F,
) -> serde_json::Value
where
    F: FnMut(&serde_json::Value) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let snap = get_snapshot(client, base, tid).await;
        if pred(&snap) {
            return snap;
        }
        assert!(Instant::now() < deadline, "30s 内未等到期望状态: {snap}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// flaky 源：首次请求（add 探测）返回 16KB 正常内容；之后全部 404——
/// 模拟「下载中途源失效」：add 成功 → 下载段请求 404 → 引擎报 Error；
/// 重试激活时的 add 探测也 404 → activate 失败路径 → 预算尽 → Failed。
async fn flaky_server() -> String {
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = axum::Router::new().route(
        "/flaky",
        axum::routing::get(move || async move {
            let n = hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                let body = vec![7u8; 16 * 1024];
                axum::response::Response::builder()
                    .status(axum::http::StatusCode::OK)
                    .header(axum::http::header::CONTENT_LENGTH, body.len())
                    .body(axum::body::Body::from(body))
                    .unwrap()
            } else {
                axum::response::Response::builder()
                    .status(axum::http::StatusCode::NOT_FOUND)
                    .body(axum::body::Body::empty())
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/flaky")
}

/// 恢复源（E32 手动重试 e2e）：**首个请求**（add 探测）恒 200，此后 5s 内
/// 恒 404（下载失败 + 自动重试激活失败 → 预算尽 Failed），之后全部 206/200
///（手动重试全通；带 Range 头 → 206 段语义，无 Range → 200 全量）。
/// 以首请求为时钟起点——add 探测必须在窗口外；时间窗口比请求计数稳健
///（httpdl 探测/分段/重试的请求数不可预判）。
/// 节奏：t=0 add（200）→ 下载 404 → t≈2s 自动重试激活 404 → Failed
/// → 测试等到 Failed 后 sleep 3s（t≈5.5s > 窗口）→ 手动重试全通。
async fn recovering_server() -> String {
    let first_hit = Arc::new(std::sync::OnceLock::<std::time::Instant>::new());
    let app = axum::Router::new().route(
        "/recovering",
        axum::routing::get(move |headers: axum::http::HeaderMap| async move {
            // 首请求（add 探测）恒 200 并起表；此后 5s 内 404，之后 206/200
            let first = first_hit.get().is_none();
            if first {
                let _ = first_hit.set(std::time::Instant::now());
            }
            let ok = first
                || first_hit
                    .get()
                    .map(|t0| t0.elapsed() >= std::time::Duration::from_secs(5))
                    .unwrap_or(false);
            if !ok {
                return axum::response::Response::builder()
                    .status(axum::http::StatusCode::NOT_FOUND)
                    .body(axum::body::Body::empty())
                    .unwrap();
            }
            let total: u64 = 16 * 1024;
            let body = vec![9u8; total as usize];
            match headers
                .get(axum::http::header::RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|r| r.strip_prefix("bytes="))
                .and_then(|s| s.split('-').next().and_then(|s| s.parse::<u64>().ok()))
            {
                Some(start) => axum::response::Response::builder()
                    .status(axum::http::StatusCode::PARTIAL_CONTENT)
                    .header(
                        axum::http::header::CONTENT_RANGE,
                        format!("bytes {start}-{}/{total}", total - 1),
                    )
                    .body(axum::body::Body::from(body[start as usize..].to_vec()))
                    .unwrap(),
                None => axum::response::Response::builder()
                    .status(axum::http::StatusCode::OK)
                    .header(axum::http::header::CONTENT_LENGTH, total)
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/recovering")
}

#[tokio::test]
async fn manual_retry_after_budget_exhausted_completes_when_source_recovers() {
    let url = recovering_server().await;
    let (addr, _state) = serve_with_scheduler().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let dest = std::env::temp_dir().join(format!("e32-manual-{}", std::process::id()));
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": url,
            "dest": dest.to_str().unwrap(),
            "name": "manual.bin",
            "auto_retry": 1,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 失败 → 自动重试 → 再失败 → 预算尽 Failed
    let snap = wait_until(&client, &base, &tid, |s| s["state"] == "Failed").await;
    assert_eq!(snap["retries"], 1);

    // 等恢复窗口打开（5s 窗口；Failed 时约 t≈2-3s，再等 3s 稳过窗口）
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // E32：手动重试（resume）→ 源已恢复 → 下载成功 → Completed
    let resp = client
        .post(format!("{base}/tasks/{tid}/resume"))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    assert_eq!(status, 200, "Failed 任务手动重试必须成功: {status} {body}");

    let snap = wait_until(&client, &base, &tid, |s| s["state"] == "Completed").await;
    assert_eq!(snap["state"], "Completed");
    assert_eq!(snap["retries"], 1, "手动重试不重置/不追加预算计数");
    assert_eq!(snap["total"], 16384);
}

#[tokio::test]
async fn auto_retry_schedules_backoff_then_exhausts_to_failed() {
    let url = flaky_server().await;
    let (addr, _state) = serve_with_scheduler().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let dest = std::env::temp_dir().join(format!("e30-retry-{}", std::process::id()));
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": url,
            "dest": dest.to_str().unwrap(),
            "name": "retry.bin",
            "auto_retry": 1,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 阶段 1：首次失败 → 预算内 → Queued + retries=1 + 退避时刻已安排
    let snap = wait_until(&client, &base, &tid, |s| {
        s["state"] == "Queued"
            && s["retries"] == 1
            && s["max_retries"] == 1
            && s["next_retry_at_unix"].as_u64().unwrap_or(0) > 0
    })
    .await;
    assert_eq!(snap["state"], "Queued", "重试等待中：{snap}");

    // 阶段 2：退避到期（2s）→ 调度循环重激活 → 再次 404 → 预算尽 → Failed
    let snap = wait_until(&client, &base, &tid, |s| s["state"] == "Failed").await;
    assert_eq!(snap["state"], "Failed");
    assert_eq!(snap["retries"], 1, "终态时 retries 停在 max");
    assert_eq!(snap["max_retries"], 1);
}

#[tokio::test]
async fn zero_budget_keeps_one_shot_failed_semantics() {
    let url = flaky_server().await;
    let (addr, _state) = serve_with_scheduler().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let dest = std::env::temp_dir().join(format!("e30-noretry-{}", std::process::id()));
    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": url,
            "dest": dest.to_str().unwrap(),
            "name": "noretry.bin",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let tid = resp.json::<serde_json::Value>().await.unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 无 auto_retry：失败直接终态（不安排重试、不回队列）
    let snap = wait_until(&client, &base, &tid, |s| s["state"] == "Failed").await;
    assert_eq!(snap["state"], "Failed");
    assert!(
        snap.get("retries").is_none(),
        "retries=0 序列化省略: {snap}"
    );
    assert!(
        snap.get("max_retries").is_none(),
        "max=0 序列化省略: {snap}"
    );
}

#[tokio::test]
async fn auto_retry_out_of_range_rejected() {
    let (addr, _state) = serve_with_scheduler().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/tasks"))
        .json(&serde_json::json!({
            "url": "https://example.com/f.bin",
            "auto_retry": 11,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body = resp.json::<serde_json::Value>().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("0..=10"),
        "错误信息应含合法范围: {body}"
    );
}
