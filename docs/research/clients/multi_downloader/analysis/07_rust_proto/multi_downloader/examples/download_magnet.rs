//! Example: BT magnet download (placeholder — BT engine is not implemented).
//!
//! Demonstrates the trait surface and graceful error path when BT is disabled.

use std::sync::Arc;

use multi_downloader::config::ConfigStore;
use multi_downloader::core::listener::NoopListener;
use multi_downloader::core::task::{DownloadTask, TaskKind};
use multi_downloader::engine::http_engine::HttpEngine;
use multi_downloader::engine::protocol::ProtocolKind;
use multi_downloader::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    multi_downloader::init_tracing();
    let store = ConfigStore::open(&std::path::PathBuf::from(".mdc"))?;
    let cfg = store.snapshot();

    let url: url::Url = "magnet:?xt=urn:btih:abcdef0123456789&dn=test".parse()?;
    let task = DownloadTask::new(TaskKind::Magnet, url);

    match ProtocolKind::from_url(&task.url) {
        ProtocolKind::Magnet => {
            let engine = HttpEngine::new(cfg, Arc::new(NoopListener));
            // Expect this to fail with a "BT not implemented" error.
            match engine.run_task(&task).await {
                Ok(_) => println!("unexpected success"),
                Err(e) => println!("expected failure: {e:?}"),
            }
        }
        other => println!("unexpected protocol: {other:?}"),
    }
    Ok(())
}
