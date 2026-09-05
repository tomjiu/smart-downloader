//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! 卡 D：FTP 路由（feature `ftp`）单测——add_ftp_task 前缀校验 / user+pass 提取 /
//! 路由 Ftp 引擎 / 目录 files 同步；以及端到端（真实 FtpEngine + 最小 mock FTP server）。
#![cfg(all(test, feature = "ftp"))]

use super::*;

// ---- 单元：add_ftp_task 基础路由 ----

#[tokio::test]
async fn non_ftp_url_is_invalid() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
    let err = state
        .add_ftp_task("https://example.com/f.bin".into(), None)
        .await
        .expect_err("非 ftp:// 前缀应拒绝");
    assert!(
        matches!(err, DaemonError::InvalidSource(_)),
        "应返回 InvalidSource: {err}"
    );
    assert!(fake.added().is_empty(), "不应路由到引擎");
}

#[tokio::test]
async fn extracts_auth_and_routes_to_ftp_engine() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
    let url = "ftp://alice:secret@host/pub/a.bin".to_string();
    let tid = state.add_ftp_task(url.clone(), None).await.unwrap();
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    assert_eq!(rec.engine_kind, EngineKind::Ftp, "engine_kind 记 Ftp");
    match &rec.task.source {
        DownloadSource::Ftp { url: u, user, pass } => {
            assert_eq!(u, &url);
            assert_eq!(user, "alice", "parse_ftp_auth 应提取 user");
            assert_eq!(pass, "secret", "parse_ftp_auth 应提取 pass");
        }
        other => panic!("source 应为 DownloadSource::Ftp: {other:?}"),
    }
    // FakeEngine.add 对 Ftp 源记录 task.id → 证明路由到 Ftp 引擎
    assert_eq!(fake.added(), vec![tid], "应路由到 Ftp 引擎 add");
}

#[tokio::test]
async fn anonymous_auth_falls_back() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
    let tid = state
        .add_ftp_task("ftp://host/pub/a.bin".into(), None)
        .await
        .unwrap();
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    match &rec.task.source {
        DownloadSource::Ftp { user, pass, .. } => {
            assert_eq!(user, "anonymous");
            assert_eq!(pass, "");
        }
        _ => panic!("source 应为 Ftp"),
    }
}

#[tokio::test]
async fn single_file_uses_url_filename_as_metadata_name() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
    fake.set_status_files(vec![FileProgress {
        rel_path: "x.bin".into(),
        done: 0,
        size: 5,
    }]);
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
    let tid = state
        .add_ftp_task("ftp://host/pub/a.bin".into(), None)
        .await
        .unwrap();
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    assert_eq!(
        rec.task.metadata.name.as_deref(),
        Some("a.bin"),
        "单文件任务落盘名取 URL 最后一段"
    );
    assert!(
        rec.task.files.is_empty(),
        "单文件任务不应触发目录 files 同步"
    );
}

#[tokio::test]
async fn dir_task_syncs_files_from_engine_status() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
    fake.set_status_files(vec![
        FileProgress {
            rel_path: "a.bin".into(),
            done: 0,
            size: 10,
        },
        FileProgress {
            rel_path: "b.bin".into(),
            done: 0,
            size: 20,
        },
    ]);
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
    let tid = state
        .add_ftp_task("ftp://host/pub/".into(), None)
        .await
        .unwrap();
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    assert_eq!(
        rec.task.files.len(),
        2,
        "目录任务 files 应从引擎 status 同步: {:?}",
        rec.task.files
    );
    assert_eq!(rec.task.files[0].rel_path, "a.bin");
    assert_eq!(rec.task.files[0].size, 10);
    assert_eq!(rec.task.files[0].done, 0);
    assert_eq!(rec.task.files[0].engine, EngineKind::Ftp);
    assert_eq!(
        rec.task.files[0].source_urls,
        vec!["ftp://host/pub/".to_string()]
    );
    // 序列化往返：task.files（含 EngineKind::Ftp）应可持久化反序列化
    let json = serde_json::to_vec(&rec.task).expect("task 可序列化");
    let _restored: DownloadTask = serde_json::from_slice(&json).expect("可反序列化");
}

#[tokio::test]
async fn repeated_ftp_dup_is_rejected() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
    let dir = tempfile::tempdir().unwrap();
    let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
    let _ = state
        .add_ftp_task("ftp://host/pub/".into(), None)
        .await
        .unwrap();
    let err = state
        .add_ftp_task("ftp://host/pub/".into(), None)
        .await
        .expect_err("重复 canonical 应拒绝");
    assert!(
        matches!(err, DaemonError::Duplicate(_)),
        "应返回 Duplicate: {err}"
    );
}

// ---- 端到端：真实 FtpEngine + 最小 mock FTP server（卡 D 验收点） ----

/// 最小 mock FTP server：支持目录 LIST（文件清单）/ 文件 RETR（完整内容）、PASSIVE 被动模式。
/// `files` = `(远端绝对路径, 内容)`；路径不带 user:pass@。
async fn start_mock_ftp(files: Vec<(String, Vec<u8>)>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let files = SArc::new(files);
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let files = files.clone();
            tokio::spawn(async move {
                handle_mock_ftp(stream, files).await;
            });
        }
    });
    addr
}

use std::net::SocketAddr;
use std::sync::Arc as SArc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

async fn handle_mock_ftp(mut stream: TcpStream, files: SArc<Vec<(String, Vec<u8>)>>) {
    let mut rest: u64 = 0;
    let mut data_listener: Option<tokio::net::TcpListener> = None;
    let _ = stream.write_all(b"220 test ftp ready\r\n").await;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let cmd = line.trim_end_matches("\r\n").trim_end();
        let (verb, arg) = match cmd.split_once(' ') {
            Some((v, a)) => (v, a.trim()),
            None => (cmd, ""),
        };
        let conn = reader.get_mut();
        match verb {
            "USER" | "PASS" => {
                let _ = conn.write_all(b"230 logged in\r\n").await;
            }
            "TYPE" => {
                let _ = conn.write_all(b"200 type set\r\n").await;
            }
            "PASV" => {
                let dl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let da = dl.local_addr().unwrap();
                let (p1, p2) = (da.port() / 256, da.port() % 256);
                let _ = conn
                    .write_all(
                        format!("227 Entering Passive Mode (127,0,0,1,{p1},{p2})\r\n").as_bytes(),
                    )
                    .await;
                data_listener = Some(dl);
            }
            "RETR" => {
                let data: Option<Vec<u8>> =
                    files.iter().find(|(p, _)| p == arg).map(|(_, d)| d.clone());
                let Some(body) = data else {
                    let _ = conn.write_all(b"550 file unavailable\r\n").await;
                    continue;
                };
                if rest > body.len() as u64 {
                    let _ = conn.write_all(b"550 file unavailable\r\n").await;
                    continue;
                }
                let _ = conn.write_all(b"150 opening data connection\r\n").await;
                if let Some(dl) = data_listener.as_ref() {
                    if let Ok((mut data_conn, _)) = dl.accept().await {
                        let _ = data_conn.write_all(&body[rest as usize..]).await;
                        let _ = data_conn.shutdown().await;
                    }
                }
                let _ = conn.write_all(b"226 transfer complete\r\n").await;
                rest = 0;
                data_listener = None;
            }
            "LIST" => {
                let dir = arg.trim_end_matches('/');
                let prefix = format!("{dir}/");
                let mut lines: Vec<String> = Vec::new();
                for (p, d) in files.iter() {
                    if let Some(name) = p.strip_prefix(&prefix) {
                        if !name.is_empty() && !name.contains('/') {
                            lines.push(format!(
                                "-rw-r--r--  1 owner  group  {:>8} Jan 01 12:00 {name}",
                                d.len()
                            ));
                        }
                    }
                }
                let mut text = format!("total {}\r\n", lines.len());
                text.push_str(&lines.join("\r\n"));
                text.push_str("\r\n");
                let _ = conn.write_all(b"150 opening data connection\r\n").await;
                if let Some(dl) = data_listener.as_ref() {
                    if let Ok((mut data, _)) = dl.accept().await {
                        let _ = data.write_all(text.as_bytes()).await;
                        let _ = data.shutdown().await;
                    }
                }
                let _ = conn.write_all(b"226 transfer complete\r\n").await;
                data_listener = None;
            }
            _ => {
                let _ = conn.write_all(b"502 unknown\r\n").await;
            }
        }
    }
}

/// 端到端验收（卡 D）：FTP 目录任务（2 文件）→ add 后 task.files 同步 →
/// poll_engine_states 推进 Completed → 快照含文件信息且逐文件 done==size；
/// 同时一个 HTTP 任务（独立 Http 槽）不受影响。
#[tokio::test]
async fn ftp_dir_completes_and_http_unaffected() {
    let ftp_addr = start_mock_ftp(vec![
        ("/pub/a.bin".to_string(), vec![0x5Au8; 16]),
        ("/pub/b.bin".to_string(), vec![0x5Bu8; 24]),
    ])
    .await;
    let ftp_url = format!("ftp://127.0.0.1:{}/pub/", ftp_addr.port());
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().to_str().unwrap().to_string();

    // 同时挂 HTTP 引擎（Fake）与真实 FTP 引擎：验证各占独立槽位
    let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let ftp_engine: Arc<dyn DownloadEngine> = Arc::new(smart_dl_httpdl::FtpEngine::new());
    let state = Arc::new(
        DaemonState::new(http_fake.clone(), vec![])
            .with_ftp(ftp_engine)
            .with_dest_root(dir.path().to_path_buf()),
    );

    // 1) FTP 目录任务
    let ftp_tid = state
        .add_link_task(ftp_url.clone(), Some(dest.clone()))
        .await
        .expect("FTP 目录任务 add 应成功");
    let rec = state.tasks.lock().get(&ftp_tid).cloned().unwrap();
    assert_eq!(rec.engine_kind, EngineKind::Ftp);
    assert_eq!(
        rec.task.files.len(),
        2,
        "add 后 files 应同步 2 个: {:?}",
        rec.task.files
    );

    // 2) HTTP 任务走独立 Http 槽（Fake 引擎记录），不受 FTP 影响
    let http_tid = state
        .add_http_task("https://example.com/keep.bin".into(), Some(dest.clone()))
        .await
        .unwrap();
    assert!(
        http_fake
            .added()
            .contains(&"https://example.com/keep.bin".to_string()),
        "HTTP 任务应路由到 Http 引擎: {:?}",
        http_fake.added()
    );

    // 3) 轮询推进 FTP 到 Completed（FTP 引擎无 alert，靠 poll 推进——同 HTTP 链路）
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let _ = state.poll_engine_states().await;
        if let Some(snap) = state.task_snapshot(&ftp_tid).await {
            if snap.state == "Completed" {
                // 快照含文件信息，且逐文件 done==size
                assert_eq!(snap.files.len(), 2, "快照应含 2 个文件进度");
                for f in &snap.files {
                    assert_eq!(f.done, f.size, "逐文件 done 应等于 size: {f:?}");
                }
                // HTTP 任务记录仍存在（未被破坏）
                assert!(state.task_snapshot(&http_tid).await.is_some());
                return;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "60s 内 FTP 任务未 Completed"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
