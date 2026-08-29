//! Example: HTTP download with mirror discovery enabled.
//!
//! Demonstrates how the FlashGet-style mirror discovery can be opted into for
//! a single task. By default mirror discovery is OFF (`AppConfig`).
//!
//! Run with:
//!
//! ```sh
//! cargo run --example download_with_mirror -- \
//!     https://primary.example.com/file.zip \
//!     https://mirror1.example.com/file.zip \
//!     https://mirror2.example.com/file.zip
//! ```

use std::sync::Arc;

use multi_downloader::config::AppConfig;
use multi_downloader::core::listener::NoopListener;
use multi_downloader::core::task::{DownloadTask, TaskKind};
use multi_downloader::engine::http_engine::HttpEngine;
use multi_downloader::engine::mirror::{MirrorDiscovery, MirrorSource};
use multi_downloader::engine::protocol::ProtocolEngine;
use url::Url;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    multi_downloader::init_tracing();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "usage: {} <primary-url> [mirror-url...] [dest]",
            args.get(0).map(String::as_str).unwrap_or("download_with_mirror")
        );
        std::process::exit(2);
    }

    let primary = Url::parse(&args[1])?;
    let dest = args
        .last()
        .filter(|s| !s.starts_with("http"))
        .cloned()
        .unwrap_or_else(|| "./download.bin".into());

    // Opt in to mirror discovery (off by default).
    let cfg = AppConfig {
        enable_mirror_discovery: true,
        default_concurrency: 8,
        ..Default::default()
    };

    let client = multi_downloader::net::tls::build_https_client(&cfg)?;
    let discovery = MirrorDiscovery::new(client);

    // Add mirrors from CLI (positional args 2..n that look like URLs).
    for s in args.iter().skip(2).filter(|s| s.starts_with("http")) {
        if let Ok(u) = Url::parse(s) {
            discovery.add(u, MirrorSource::User);
        }
    }

    // Probe each mirror (no expected size — pass 0 to allow size discovery).
    let ranked = discovery.rank(0).await;
    println!("mirrors ranked: {}", ranked.len());
    for (s, m) in ranked.iter().take(5) {
        println!(
            "  score={:.2}  reliability={:.2}  url={}",
            s.0,
            m.reliability,
            m.url
        );
    }

    // Build the task with the highest-ranked mirror as backup.
    let mut task = DownloadTask::new(TaskKind::Http, primary).with_dest(dest.into());
    if let Some((_, m)) = ranked.first() {
        task = task.with_backup_url(m.url.clone());
    }

    let engine = HttpEngine::new(cfg, Arc::new(NoopListener));
    engine.run_task(&task).await?;
    println!("OK task_id={}", task.task_id);
    Ok(())
}
