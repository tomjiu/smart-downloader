//! M2/M3 共享测试辅助：构造 DownloadTask 与临时目录。

use smart_dl_core::identity::{CanonicalId, CanonicalKind, ContentIdentity};
use smart_dl_core::ownership::{AcqKind, AcqState, Acquisition};
use smart_dl_core::state_machine::{EvalPhase, TaskState};
use smart_dl_core::task::{
    DownloadTask, FileState, ProgressAggregate, RetryState, TaskFile, TaskMetadata,
};
use smart_dl_core::types::{DownloadSource, EngineKind};
use std::path::PathBuf;

pub fn make_task(id: &str, name: &str) -> DownloadTask {
    DownloadTask {
        id: id.to_string(),
        canonical_id: CanonicalId {
            kind: CanonicalKind::Bt,
            identity: "0123456789abcdef0123".to_string(),
            validator: None,
            token_sensitive: false,
        },
        source: DownloadSource::Magnet(format!("magnet:?xt=urn:btih:{id}")),
        identity: ContentIdentity::InfoHash([0xAB; 20]),
        dest_root: PathBuf::from("."),
        files: vec![
            TaskFile {
                rel_path: format!("{name}.bin"),
                size: 1000,
                done: 500,
                state: FileState::Active,
                source_urls: vec![],
                identity: None,
                etag: None,
                engine: EngineKind::Bt,
            },
            TaskFile {
                rel_path: format!("{name}.txt"),
                size: 100,
                done: 100,
                state: FileState::Done,
                source_urls: vec![],
                identity: None,
                etag: None,
                engine: EngineKind::Bt,
            },
        ],
        acquisitions: vec![Acquisition {
            kind: AcqKind::Bt,
            engine_id: "bt".into(),
            engine_task_id: format!("{id}-1"),
            state: AcqState::Active,
            done: 600,
            total: 1100,
            started_at_unix: Some(1),
        }],
        aggregate: ProgressAggregate {
            done: 600,
            total: 1100,
        },
        state: TaskState::Evaluating(EvalPhase::MetadataPending),
        retry: RetryState {
            retries: 0,
            max_retries: 3,
        },
        created_at: std::time::Instant::now(),
        file_priorities: None,
        sequential: false,
        metadata: TaskMetadata {
            name: Some(name.to_string()),
            added_at_unix: 1,
            tags: Vec::new(),
        },
        limits: None,
    }
}
