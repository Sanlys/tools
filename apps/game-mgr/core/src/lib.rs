//! `gm-core` — all client logic, no GUI dependencies.
//!
//! The egui shell in `gm-client` (and any future iced/other shell) talks to
//! this crate exclusively through [`core::CoreHandle`]: commands in via a
//! channel, state out via immutable snapshots. Keeping GUI types out of this
//! crate is what makes the toolkit swappable (PLAN.md §6.1).

pub mod classes;
pub mod config;
pub mod core;
pub mod engine;
pub mod game;
pub mod oidc;
pub mod paths;
pub mod platform;
pub mod registry;
pub mod run;
pub mod s3;
pub mod scan;
pub mod services;
pub mod statedb;
pub mod stats;
pub mod steps;
pub mod syncthing;
pub mod watcher;

/// Initialize tracing for an app: keep logging to stdout (the `cargo run`
/// terminal, filtered by `RUST_LOG`, default `info`) **and** mirror everything
/// to a daily-rolling file under [`paths::log_dir`]. The returned guard must be
/// kept alive for the process lifetime so the file writer flushes on exit.
pub fn init_tracing(app: &str) -> tracing_appender::non_blocking::WorkerGuard {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, Layer, fmt};

    let log_dir = paths::log_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::daily(&log_dir, format!("{app}.log"));
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());

    tracing_subscriber::registry()
        .with(fmt::layer().with_filter(filter()))
        .with(
            fmt::layer()
                .with_ansi(false)
                .with_writer(file_writer)
                .with_filter(filter()),
        )
        .init();

    tracing::info!(app, log_file = %log_dir.join(format!("{app}.log")).display(), "logging started");
    guard
}
