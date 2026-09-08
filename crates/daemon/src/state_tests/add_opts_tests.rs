//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! E6 AddHttpOpts：API 新暴露字段（headers/auth/sha256/backup/name）落任务
//! 记录 + 入参校验（FakeEngine 不联网）。
#![cfg(test)]

use super::*;

#[tokio::test]
async fn add_opts_fields_land_in_task_record() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task_opts(
            "https://example.com/f.bin".into(),
            None,
            AddHttpOpts {
                sequential: true,
                proxy: Some("socks5://127.0.0.1:1080".into()),
                headers: vec![
                    ("Referer".into(), "https://ref.example".into()),
                    ("X-Token".into(), "abc".into()),
                ],
                basic_auth: Some(("user".into(), "pass".into())),
                // 大写 + 首尾空白：断言小写归一
                sha256: Some(
                    " ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789 ".into(),
                ),
                sha1: None,
                md5: None,
                backup_url: Some("https://backup.example/f.bin".into()),
                backup_md5: Some("ABCDEF0123456789ABCDEF0123456789".into()),
                name: Some("explicit-name.bin".into()),
                conflict: None,
                start_at_unix: None,
                auto_retry: 0,
            },
        )
        .await
        .unwrap();
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    assert!(rec.task.sequential, "sequential 应落任务");
    match &rec.task.source {
        DownloadSource::Http {
            headers,
            auth,
            backup_url,
            proxy,
            ..
        } => {
            assert_eq!(headers.len(), 2, "headers 应原序落 source");
            assert_eq!(headers[0].0, "Referer");
            assert_eq!(
                auth.as_ref(),
                Some(&Auth::Basic("user".into(), "pass".into()))
            );
            assert_eq!(backup_url.as_deref(), Some("https://backup.example/f.bin"));
            assert_eq!(proxy.as_deref(), Some("socks5://127.0.0.1:1080"));
        }
        other => panic!("source 应为 Http: {other:?}"),
    }
    match &rec.task.identity {
        ContentIdentity::SingleFile {
            sha256, backup_md5, ..
        } => {
            assert_eq!(
                sha256.as_deref(),
                Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"),
                "sha256 大写+空白应小写归一"
            );
            assert_eq!(
                backup_md5.as_deref(),
                Some("abcdef0123456789abcdef0123456789"),
                "backup_md5 大写应小写归一"
            );
        }
        other => panic!("identity 应为 SingleFile: {other:?}"),
    }
    assert_eq!(
        rec.task.metadata.name.as_deref(),
        Some("explicit-name.bin"),
        "显式名应落 metadata"
    );
}

#[tokio::test]
async fn add_opts_validation_rejects_bad_input() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = Arc::new(DaemonState::new(fake.clone(), vec![]));
    let cases: Vec<(AddHttpOpts, &str)> = vec![
        (
            AddHttpOpts {
                sha256: Some("ab".repeat(31)), // 63 hex
                ..Default::default()
            },
            "sha256",
        ),
        (
            AddHttpOpts {
                sha256: Some("g".repeat(64)), // 非 hex
                ..Default::default()
            },
            "sha256",
        ),
        (
            AddHttpOpts {
                sha1: Some("ab".repeat(19)), // 38 hex
                ..Default::default()
            },
            "sha1",
        ),
        (
            AddHttpOpts {
                sha1: Some("z".repeat(40)), // 非 hex
                ..Default::default()
            },
            "sha1",
        ),
        (
            AddHttpOpts {
                md5: Some("ab".repeat(15)), // 30 hex
                ..Default::default()
            },
            "md5",
        ),
        (
            AddHttpOpts {
                md5: Some("w".repeat(32)), // 非 hex
                ..Default::default()
            },
            "md5",
        ),
        (
            AddHttpOpts {
                // E25 互斥：sha256 + sha1
                sha256: Some("a".repeat(64)),
                sha1: Some("a".repeat(40)),
                ..Default::default()
            },
            "互斥",
        ),
        (
            AddHttpOpts {
                // E25 互斥：sha256 + md5
                sha256: Some("a".repeat(64)),
                md5: Some("a".repeat(32)),
                ..Default::default()
            },
            "互斥",
        ),
        (
            AddHttpOpts {
                // E25 互斥：sha1 + md5
                sha1: Some("a".repeat(40)),
                md5: Some("a".repeat(32)),
                ..Default::default()
            },
            "互斥",
        ),
        (
            AddHttpOpts {
                // E25 互斥：三者同时
                sha256: Some("a".repeat(64)),
                sha1: Some("a".repeat(40)),
                md5: Some("a".repeat(32)),
                ..Default::default()
            },
            "互斥",
        ),
        (
            AddHttpOpts {
                backup_md5: Some("a".repeat(32)), // 无 backup_url
                ..Default::default()
            },
            "成对",
        ),
        (
            AddHttpOpts {
                backup_url: Some("https://b.example/f".into()),
                backup_md5: Some("a".repeat(31)),
                ..Default::default()
            },
            "backup_md5",
        ),
        (
            AddHttpOpts {
                backup_url: Some("ftp://b.example/f".into()),
                ..Default::default()
            },
            "backup_url",
        ),
        (
            AddHttpOpts {
                headers: vec![("X-Bad:Header".into(), "v".into())],
                ..Default::default()
            },
            "header",
        ),
        (
            AddHttpOpts {
                headers: vec![("".into(), "v".into())],
                ..Default::default()
            },
            "header",
        ),
        (
            AddHttpOpts {
                headers: vec![("X-Ok".into(), "v\ninjected".into())],
                ..Default::default()
            },
            "换行",
        ),
        (
            AddHttpOpts {
                name: Some("../escape.bin".into()),
                ..Default::default()
            },
            "name",
        ),
    ];
    for (opts, tag) in cases {
        let r = state
            .add_http_task_opts("https://example.com/f.bin".into(), None, opts)
            .await;
        match r {
            Err(DaemonError::InvalidSource(m)) => {
                assert!(m.contains(tag), "错误信息应定性 {tag}: {m}");
            }
            other => panic!("非法入参（{tag}）必须 InvalidSource 拒绝: {other:?}"),
        }
    }
    assert!(fake.added().is_empty(), "被拒任务不得进入引擎");
}

#[tokio::test]
async fn add_opts_verify_algo_e25_normalize_and_landing() {
    // E25：sha1/md5 主源校验目标入参大写+空白 → 小写归一落 identity，
    // 且经 API 层（AddHttpOpts）进入引擎可见的任务身份。
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone(), vec![]);
    let tid = state
        .add_http_task_opts(
            "https://example.com/e25.bin".into(),
            None,
            AddHttpOpts {
                sha1: Some(format!(" {} ", "ab".repeat(20))), // 40 hex + 空白
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let rec = state.tasks.lock().get(&tid).cloned().unwrap();
    match &rec.task.identity {
        ContentIdentity::SingleFile { sha1, md5, .. } => {
            assert_eq!(
                sha1.as_deref(),
                Some("ab".repeat(20)).as_deref(),
                "sha1 大写+空白应小写归一"
            );
            assert_eq!(md5, &None);
        }
        other => panic!("identity 应为 SingleFile: {other:?}"),
    }
    // md5 同理（第二个任务，URL 不同避开 canonical 查重）
    let tid2 = state
        .add_http_task_opts(
            "https://example.com/e25-md5.bin".into(),
            None,
            AddHttpOpts {
                md5: Some(" ABCDEF0123456789ABCDEF0123456789".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let rec2 = state.tasks.lock().get(&tid2).cloned().unwrap();
    match &rec2.task.identity {
        ContentIdentity::SingleFile { sha1, md5, .. } => {
            assert_eq!(sha1, &None);
            assert_eq!(
                md5.as_deref(),
                Some("abcdef0123456789abcdef0123456789"),
                "md5 大写+空白应小写归一"
            );
        }
        other => panic!("identity 应为 SingleFile: {other:?}"),
    }
}
