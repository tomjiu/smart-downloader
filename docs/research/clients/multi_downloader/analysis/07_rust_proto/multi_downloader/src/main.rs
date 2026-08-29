//! `multi_downloader` CLI binary entry-point.
//!
//! The CLI is intentionally minimal — it exposes `download`, `resume`, and
//! `info` subcommands enough to exercise the prototype end-to-end on HTTP(S)
//! URLs. A full TUI / daemon mode is out of scope for the prototype.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tracing::info;

use multi_downloader::config::ConfigStore;
use multi_downloader::core::listener::NoopListener;
use multi_downloader::core::task::{DownloadTask, TaskKind};
use multi_downloader::engine::http_engine::HttpEngine;
use multi_downloader::engine::protocol::{ProtocolEngine, ProtocolKind};
use multi_downloader::prelude::*;

/// Command-line interface for `multi_downloader`.
#[derive(Debug, Parser)]
#[command(name = "mdc", version, about, long_about = None)]
struct Cli {
    /// Path to the SQLite-backed config directory.
    #[arg(long, env = "MDC_CONFIG_DIR", default_value = ".mdc")]
    config_dir: PathBuf,

    /// Increase verbosity (can be repeated).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Start a new HTTP(S) download task.
    Download {
        /// Source URL.
        url: String,
        /// Output file (default: basename of the URL under the download dir).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Number of concurrent slices.
        #[arg(short, long)]
        concurrency: Option<u32>,
        /// Backup URL (Quark-style `backup_url`).
        #[arg(long)]
        backup: Option<String>,
        /// Expected SHA-256 hex of the final file.
        #[arg(long)]
        sha256: Option<String>,
    },
    /// Print the current configuration.
    Info,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let filter = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", filter);
    }
    multi_downloader::init_tracing();

    let store = ConfigStore::open(&cli.config_dir)?;
    let cfg = store.snapshot();
    info!(?cfg.download_dir, "configuration loaded");

    match cli.cmd {
        Cmd::Info => {
            println!("{}", serde_json::to_string_pretty(&cfg)?);
        }
        Cmd::Download {
            url,
            out,
            concurrency,
            backup,
            sha256,
        } => {
            let mut task = DownloadTask::new(TaskKind::Http, url.parse()?);
            if let Some(c) = concurrency {
                task = task.with_concurrency(c);
            }
            if let Some(b) = backup {
                task = task.with_backup_url(b.parse()?);
            }
            if let Some(h) = sha256 {
                task = task.with_expected_sha256(h);
            }
            let out_path = out.unwrap_or_else(|| {
                cfg.download_dir.join(task.basename())
            });
            std::fs::create_dir_all(out_path.parent().unwrap_or(PathBuf::from(".")))?;
            task = task.with_dest(out_path.clone());

            // Dispatch by protocol — currently only HTTP is implemented.
            let kind = ProtocolKind::from_url(&task.url);
            match kind {
                ProtocolKind::Http | ProtocolKind::Https => {
                    let engine = HttpEngine::new(cfg, Arc::new(NoopListener));
                    engine.run_task(&task).await?;
                }
                ProtocolKind::Magnet | ProtocolKind::Torrent => {
                    info!("BT protocol selected — prototype placeholder.");
                    return Err(anyhow::anyhow!(
                        "BT engine is a placeholder in this prototype; \
                         integrate librqbit/libtorrent-rs to enable"
                    ));
                }
                _ => return Err(anyhow::anyhow!("unsupported protocol: {kind:?}")),
            }
            info!(?out_path, "download complete");
        }
    }
    Ok(())
}
