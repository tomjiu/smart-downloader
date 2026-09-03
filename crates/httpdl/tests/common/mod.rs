//! httpdl 测试共享：构造 Http 任务（最小 DownloadTask）。

//! 测试 helper（按测试二进制编译，未使用的 helper 属正常）
#![allow(dead_code)]

use smart_dl_core::identity::{CanonicalId, CanonicalKind, ContentIdentity};
use smart_dl_core::state_machine::{EvalPhase, TaskState};
use smart_dl_core::task::{DownloadTask, ProgressAggregate, RetryState, TaskMetadata};
use smart_dl_core::types::DownloadSource;
use std::path::PathBuf;
use std::time::Instant;

pub fn make_http_task(id: &str, url: &str) -> DownloadTask {
    make_http_task_to(id, url, PathBuf::from("."), None)
}

/// 完整参数版：dest_root + 输出文件名（M4b 下载落位用）。
pub fn make_http_task_to(
    id: &str,
    url: &str,
    dest_root: PathBuf,
    name: Option<&str>,
) -> DownloadTask {
    DownloadTask {
        id: id.to_string(),
        canonical_id: CanonicalId {
            kind: CanonicalKind::Http,
            identity: url.to_string(),
            validator: None,
            token_sensitive: false,
        },
        source: DownloadSource::Http {
            url: url.to_string(),
            headers: vec![],
            auth: None,
            backup_url: None,
            proxy: None,
        },
        identity: ContentIdentity::SingleFile {
            size: 0,
            etag: None,
            sha256: None,
            sha1: None,
            md5: None,
            backup_md5: None,
        },
        dest_root,
        files: vec![],
        acquisitions: vec![],
        aggregate: ProgressAggregate::default(),
        state: TaskState::Evaluating(EvalPhase::MetadataPending),
        retry: RetryState::default(),
        created_at: Instant::now(),
        file_priorities: None,
        sequential: false,
        metadata: TaskMetadata {
            name: name.map(str::to_string),
            added_at_unix: 0,
            tags: Vec::new(),
            finished_at_unix: 0,
            start_at_unix: 0,
        },
        limits: None,
    }
}

/// 带 sha256 的任务（verify 用例）。
pub fn make_http_task_sha256(
    id: &str,
    url: &str,
    dest_root: PathBuf,
    name: &str,
    sha256: &str,
) -> DownloadTask {
    let mut t = make_http_task_to(id, url, dest_root, Some(name));
    t.identity = ContentIdentity::SingleFile {
        size: 0,
        etag: None,
        sha256: Some(sha256.to_string()),
        sha1: None,
        md5: None,
        backup_md5: None,
    };
    t
}

/// 带主源 sha1 的任务（E25 verify 用例）。
pub fn make_http_task_sha1(
    id: &str,
    url: &str,
    dest_root: PathBuf,
    name: &str,
    sha1: &str,
) -> DownloadTask {
    let mut t = make_http_task_to(id, url, dest_root, Some(name));
    t.identity = ContentIdentity::SingleFile {
        size: 0,
        etag: None,
        sha256: None,
        sha1: Some(sha1.to_string()),
        md5: None,
        backup_md5: None,
    };
    t
}

/// 带主源 md5 的任务（E25 verify 用例）。
pub fn make_http_task_md5(
    id: &str,
    url: &str,
    dest_root: PathBuf,
    name: &str,
    md5: &str,
) -> DownloadTask {
    let mut t = make_http_task_to(id, url, dest_root, Some(name));
    t.identity = ContentIdentity::SingleFile {
        size: 0,
        etag: None,
        sha256: None,
        sha1: None,
        md5: Some(md5.to_string()),
        backup_md5: None,
    };
    t
}

/// 带主源 sha256 + 备用源 url/md5 的任务（backup_failover 用例）。
pub fn make_http_task_backup(
    id: &str,
    url: &str,
    backup_url: &str,
    backup_md5: &str,
    dest_root: PathBuf,
    name: &str,
    sha256: &str,
) -> DownloadTask {
    let mut t = make_http_task_to(id, url, dest_root, Some(name));
    t.source = DownloadSource::Http {
        url: url.to_string(),
        headers: vec![],
        auth: None,
        backup_url: Some(backup_url.to_string()),
        proxy: None,
    };
    t.identity = ContentIdentity::SingleFile {
        size: 0,
        etag: None,
        sha256: Some(sha256.to_string()),
        sha1: None,
        md5: None,
        backup_md5: Some(backup_md5.to_string()),
    };
    t
}

/// FTP 任务（feature=ftp 测试用）。
#[cfg(feature = "ftp")]
pub fn make_ftp_task(id: &str, url: &str, dest_root: PathBuf, name: &str) -> DownloadTask {
    DownloadTask {
        id: id.to_string(),
        canonical_id: CanonicalId {
            kind: CanonicalKind::Ftp,
            identity: url.to_string(),
            validator: None,
            token_sensitive: false,
        },
        source: DownloadSource::Ftp {
            url: url.to_string(),
            user: "user".to_string(),
            pass: "pass".to_string(),
        },
        identity: ContentIdentity::SingleFile {
            size: 0,
            etag: None,
            sha256: None,
            sha1: None,
            md5: None,
            backup_md5: None,
        },
        dest_root,
        files: vec![],
        acquisitions: vec![],
        aggregate: ProgressAggregate::default(),
        state: TaskState::Evaluating(EvalPhase::MetadataPending),
        retry: RetryState::default(),
        created_at: Instant::now(),
        file_priorities: None,
        sequential: false,
        metadata: TaskMetadata {
            name: Some(name.to_string()),
            added_at_unix: 0,
            tags: Vec::new(),
            finished_at_unix: 0,
            start_at_unix: 0,
        },
        limits: None,
    }
}

/// 轮询 status 直到 Completed/Error（30s 超时）。
pub async fn wait_terminal(
    engine: &impl smart_dl_core::types::DownloadEngine,
    tid: &str,
) -> smart_dl_core::types::EngineStatus {
    use smart_dl_core::types::EngineState;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let st = engine.status(&tid.to_string()).await.unwrap();
        if matches!(st.state, EngineState::Completed | EngineState::Error) {
            return st;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "下载 30s 未完成: {:?}",
            st.state
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
