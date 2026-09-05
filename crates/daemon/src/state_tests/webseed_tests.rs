//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! 卡 I′：F5 P2SP web seed 注入（feature `webseed`，隐含 `bt`）——
//! 端点语义单测 + 真实 BtCore 本地 HTTP 源端到端（Rust 版 F5 PoC-1）。
#![cfg(all(test, feature = "webseed"))]

use super::*;

/// 最小单文件 .torrent（无 tracker；info: length=123/name=test/piece length/pieces）。
fn sample_torrent() -> Vec<u8> {
    let mut b = b"d4:infod6:lengthi123e4:name4:test12:piece lengthi16384e6:pieces20:".to_vec();
    b.extend_from_slice(&[0u8; 20]);
    b.extend_from_slice(b"eee");
    b
}

#[tokio::test]
async fn non_bt_task_rejects_webseed() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
    let tid = state
        .add_http_task(
            "http://example.com/a.bin".into(),
            Some(dir.path().to_string_lossy().into_owned()),
        )
        .await
        .unwrap();
    let err = state
        .add_webseeds(&tid, &["http://seed/1".into()])
        .await
        .expect_err("非 BT 任务必须拒绝");
    assert!(
        matches!(err, DaemonError::UnsupportedOp(_)),
        "实际: {err:?}"
    );
    assert!(fake.url_seeds().is_empty());
}

#[tokio::test]
async fn bt_task_forwards_urls_to_engine() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
    let tid = state
        .add_torrent_task(
            sample_torrent(),
            Some(dir.path().to_string_lossy().into_owned()),
        )
        .await
        .expect("add_torrent_task");
    let n = state
        .add_webseeds(&tid, &["http://seed/1".into(), "http://seed/2".into()])
        .await
        .expect("注入应成功");
    assert_eq!(n, 2);
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    let engine_tid = rec.engine_tid.expect("engine_tid");
    assert_eq!(
        fake.url_seeds(),
        vec![
            (engine_tid.clone(), "http://seed/1".into()),
            (engine_tid, "http://seed/2".into()),
        ]
    );
}

#[tokio::test]
async fn webseed_missing_task_is_not_found() {
    let state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Bt)), vec![]);
    let err = state
        .add_webseeds("t_missing", &["http://seed/1".into()])
        .await
        .expect_err("");
    assert!(matches!(err, DaemonError::NotFound(_)));
}

// ---- 端到端：真实 BtCore + 本地 HTTP 静态源（F5 PoC-1 的 Rust 复刻）----

/// 极简 HTTP/1.1 静态源：200 全量 / 206 Range 分片，Connection: close。
async fn serve_http(mut sock: tokio::net::TcpStream, body: Vec<u8>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 8192];
    let n = sock.read(&mut buf).await.unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n]).to_string();
    let range = req
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("range:"))
        .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()));
    if let Some(r) = range {
        let spec = r.trim_start_matches("bytes=");
        let (a, b) = spec.split_once('-').unwrap_or((spec, ""));
        let start: u64 = a.parse().unwrap_or(0);
        let end: u64 = if b.is_empty() {
            body.len() as u64 - 1
        } else {
            b.parse::<u64>().unwrap_or(body.len() as u64 - 1)
        };
        let s = (start as usize).min(body.len().saturating_sub(1));
        let e = (end as usize).min(body.len().saturating_sub(1));
        let head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {s}-{e}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len(),
                e.saturating_sub(s) + 1
            );
        let _ = sock.write_all(head.as_bytes()).await;
        let _ = sock.write_all(&body[s..=e]).await;
    } else {
        let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                body.len()
            );
        let _ = sock.write_all(head.as_bytes()).await;
        let _ = sock.write_all(&body).await;
    }
    let _ = sock.shutdown().await;
}

#[tokio::test]
async fn webseed_e2e_downloads_via_local_http_source() {
    use sha1::{Digest, Sha1};
    // 单 piece 文件：64KB 内容，pieces = SHA1(全量)
    let content: Vec<u8> = (0..65536u32).map(|i| (i % 251) as u8).collect();
    let mut h = Sha1::new();
    h.update(&content);
    let piece = h.finalize();
    let mut meta =
        b"d4:infod6:lengthi65536e4:name8:file.bin12:piece lengthi65536e6:pieces20:".to_vec();
    meta.extend_from_slice(&piece);
    meta.extend_from_slice(b"eee");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                break;
            };
            let c = content.clone();
            tokio::spawn(async move { serve_http(sock, c).await });
        }
    });

    let save = tempfile::tempdir().unwrap();
    let core = smart_dl_btcore::BtCore::new(save.path(), "webseed-e2e").expect("session init");
    let ih = core.add_torrent_file(&meta, &[]).expect("add torrent");
    core.add_url_seed(&ih, &format!("http://{addr}/file.bin"))
        .expect("add url seed");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let st = core.status(&ih).expect("status");
        if st.downloaded >= 65536 && st.progress >= 0.999 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "90s 未完成: downloaded={} progress={}",
            st.downloaded,
            st.progress
        );
    }
}
