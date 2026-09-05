//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! E27 完成自动处理：move_to 移动 + hook 外部程序 + conflict-skip/目录豁免。
#![cfg(test)]

use super::*;

/// 造一个"已完成 + 显式名 + 文件已落盘"的任务（FakeEngine 不联网）。
async fn completed_task_with_file(
    state: &DaemonState,
    url_path: &str,
    name: &str,
    content: &[u8],
) -> String {
    let dir = state.default_dest_root.lock().clone();
    std::fs::write(dir.join(name), content).unwrap();
    let id = state
        .add_http_task_opts(
            format!("https://example.com/{url_path}"),
            None,
            crate::state::AddHttpOpts {
                name: Some(name.into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut tasks = state.tasks.lock();
    let rec = tasks.get_mut(&id).unwrap();
    rec.task.state = TaskState::Completed;
    rec.task.metadata.name = Some(name.into());
    id
}

#[tokio::test]
async fn post_move_relocates_file_and_records_event() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![])
        .with_dest_root(dir.path().to_path_buf())
        .with_post_download(Some(inbox.path().to_string_lossy().into_owned()), None);

    let id = completed_task_with_file(&state, "a.bin", "done.bin", b"payload").await;
    state.publish_task_completed(&id);

    assert!(!dir.path().join("done.bin").exists(), "源文件应已移走");
    let target = inbox.path().join("done.bin");
    assert_eq!(std::fs::read(&target).unwrap(), b"payload", "内容不变");
    let tasks = state.tasks.lock();
    let rec = tasks.get(&id).unwrap();
    assert!(
        rec.events.iter().any(|e| e.op == "post_move"
            && e.detail.as_deref() == Some(target.to_string_lossy().as_ref())),
        "应有 post_move 事件且记录目标路径: {:?}",
        rec.events
    );
}

#[tokio::test]
async fn post_move_collision_autorenames() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = tempfile::tempdir().unwrap();
    std::fs::write(inbox.path().join("done.bin"), b"old").unwrap();
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![])
        .with_dest_root(dir.path().to_path_buf())
        .with_post_download(Some(inbox.path().to_string_lossy().into_owned()), None);

    let id = completed_task_with_file(&state, "b.bin", "done.bin", b"new").await;
    state.publish_task_completed(&id);

    assert_eq!(
        std::fs::read(inbox.path().join("done.bin")).unwrap(),
        b"old"
    );
    assert_eq!(
        std::fs::read(inbox.path().join("done(1).bin")).unwrap(),
        b"new",
        "同名冲突自动改名落位"
    );
}

#[tokio::test]
async fn post_move_skipped_for_conflict_skip_tasks() {
    // conflict_policy=skip：既有文件用户明确要求不动 → 不移动
    let dir = tempfile::tempdir().unwrap();
    let inbox = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dup.bin"), b"existing").unwrap();
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![])
        .with_dest_root(dir.path().to_path_buf())
        .with_post_download(Some(inbox.path().to_string_lossy().into_owned()), None);

    let id = state
        .add_http_task_opts(
            "https://example.com/dup.bin".into(),
            Some(dir.path().to_string_lossy().into_owned()),
            crate::state::AddHttpOpts {
                name: Some("dup.bin".into()),
                conflict: Some(ConflictPolicy::Skip),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    state.publish_task_completed(&id);

    assert!(
        dir.path().join("dup.bin").exists(),
        "skip 任务既有文件必须保持原样"
    );
    assert!(!inbox.path().join("dup.bin").exists(), "不得移动进目标目录");
    let tasks = state.tasks.lock();
    let rec = tasks.get(&id).unwrap();
    assert!(
        !rec.events.iter().any(|e| e.op == "post_move"),
        "不得有 post_move 事件"
    );
}

#[tokio::test]
async fn post_move_skipped_for_directory_task() {
    // 落盘路径是目录（BT 多文件）→ 移动跳过
    let dir = tempfile::tempdir().unwrap();
    let inbox = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![])
        .with_dest_root(dir.path().to_path_buf())
        .with_post_download(Some(inbox.path().to_string_lossy().into_owned()), None);

    let id = completed_task_with_file(&state, "c.torrent", "bundle", b"").await;
    // 把落盘路径改成目录
    std::fs::remove_file(dir.path().join("bundle")).unwrap();
    std::fs::create_dir(dir.path().join("bundle")).unwrap();
    state.publish_task_completed(&id);

    assert!(dir.path().join("bundle").is_dir(), "目录任务不得被移动");
    assert!(!inbox.path().join("bundle").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn post_hook_receives_final_path_env() {
    // unix-only：hook 脚本 dump 环境变量 → 断言 SD_FILE_PATH = 移动后终路径
    let dir = tempfile::tempdir().unwrap();
    let inbox = tempfile::tempdir().unwrap();
    let dump = dir.path().join("envdump.txt");
    let script = dir.path().join("hook.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nenv > {}\n", dump.to_string_lossy().into_owned()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![])
        .with_dest_root(dir.path().to_path_buf())
        .with_post_download(
            Some(inbox.path().to_string_lossy().into_owned()),
            Some(script.to_string_lossy().into_owned()),
        );

    let id = completed_task_with_file(&state, "d.bin", "done.bin", b"payload").await;
    state.publish_task_completed(&id);

    // 钩子在后台线程执行 → 轮询等待 dump 文件内容就绪。
    // `env > dump` 重定向：文件创建 ≠ 内容写毕（CI 高负载下写入窗口
    // 可达数百 ms，曾致读空误报"应有 SD_TASK_ID"）→ 等内容而非等存在。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let env = loop {
        assert!(
            std::time::Instant::now() < deadline,
            "钩子 10s 内未产出环境 dump"
        );
        if dump.exists() {
            if let Ok(content) = std::fs::read_to_string(&dump) {
                if content.contains("SD_TASK_ID=") {
                    break content;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(env.contains("SD_TASK_ID="), "应有 SD_TASK_ID: {env}");
    assert!(
        env.contains(&format!(
            "SD_FILE_PATH={}",
            inbox.path().join("done.bin").to_string_lossy()
        )),
        "SD_FILE_PATH 应为移动后终路径: {env}"
    );
    let tasks = state.tasks.lock();
    let rec = tasks.get(&id).unwrap();
    assert!(
        rec.events.iter().any(|e| e.op == "post_hook"),
        "应有 post_hook 事件"
    );
}
