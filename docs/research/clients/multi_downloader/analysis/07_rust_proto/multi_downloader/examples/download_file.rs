//! Example: HTTP multi-thread download with mirror fallback.
//!
//! Run with:
//! ```bash
//! cargo run --example download_file -- \
//!   "https://example.com/large.zip" \
//!   ./output.zip \
//!   --mirror "https://mirror1.example.com/large.zip"
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use multi_downloader::config::ConfigStore;
use multi_downloader::core::listener::NoopListener;
use multi_downloader::core::task::{DownloadTask, TaskKind};
use multi_downloader::engine::http_engine::HttpEngine;
use multi_downloader::prelude::*;

#[derive(Debug, Parser)]
struct Args {
    /// Primary URL.
    url: String,
    /// Output file path.
    out: PathBuf,
    /// Mirror URL (FlashGet-style).
    #[arg(long)]
    mirror: Option<String>,
    /// Expected SHA-256 hex digest.
    #[arg(long)]
    sha256: Option<String>,
    /// Concurrency (slices).
    #[arg(short, long, default_value = "4")]
    concurrency: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    multi_downloader::init_tracing();
    let args = Args::parse();

    let store = ConfigStore::open(&PathBuf::from(".mdc"))?;
    let cfg = store.snapshot();

    let mut task = DownloadTask::new(TaskKind::Http, args.url.parse()?)
        .with_concurrency(args.concurrency)
        .with_dest(args.out.clone());

    if let Some(m) = args.mirror {
        task = task.with_backup_url(m.parse()?);
    }
    if let Some(h) = args.sha256 {
        task = task.with_expected_sha256(h);
    }

    let engine = HttpEngine::new(cfg, Arc::new(NoopListener));
    engine.run_task(&task).await?;
    println!("done: {}", args.out.display());
    Ok(())
}
