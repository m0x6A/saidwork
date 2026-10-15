use std::path::Path;

use anyhow::Context;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// Send `tracing` output to `log_file` (never to the terminal: the TUI owns it).
///
/// The level comes from `RUST_LOG`, defaulting to `info`. Keep the returned guard
/// alive until exit so buffered lines are flushed.
pub fn init(log_file: &Path) -> anyhow::Result<WorkerGuard> {
    let dir = log_file
        .parent()
        .context("log file has no parent directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .with_context(|| format!("opening {}", log_file.display()))?;
    let (writer, guard) = tracing_appender::non_blocking(file);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .with_target(true)
        .init();
    Ok(guard)
}
