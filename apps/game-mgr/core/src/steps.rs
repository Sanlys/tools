//! Shared install-step library (PLAN.md §4.3). Every step is idempotent:
//! `is_done` short-circuits and resume = rerun.

mod extract;
mod manual;
mod prefix;
mod remove;
mod run_installer;
mod s3_fetch;
mod syncthing_folder;
mod tools;

pub use extract::{ExtractArchiveStep, InnoExtractStep, ZipExtractStep};
pub use manual::GuidedManualStep;
pub use prefix::EnsurePrefixStep;
pub use remove::RemovePathsStep;
pub use run_installer::RunInstallerInPrefixStep;
pub use s3_fetch::S3FetchStep;
pub use syncthing_folder::EnsureSyncFolderStep;
pub use tools::InstallToolStep;

use anyhow::Context;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

use crate::game::{GameDirs, InstallStep, SyncFolderSpec};

/// Read `reader` and emit a string each time a `\n` *or* `\r` is seen. The
/// `\r` split surfaces tools that draw an updating progress bar in place
/// (innoextract, downloaders) instead of swallowing it until the final
/// newline.
async fn pump<R: AsyncRead + Unpin>(reader: R, mut emit: impl FnMut(&str)) {
    let mut reader = tokio::io::BufReader::new(reader);
    let mut buf: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte).await {
            Ok(0) => break,
            Ok(_) => {
                if byte[0] == b'\n' || byte[0] == b'\r' {
                    if !buf.is_empty() {
                        emit(&String::from_utf8_lossy(&buf));
                        buf.clear();
                    }
                } else {
                    buf.push(byte[0]);
                }
            }
            Err(_) => break,
        }
    }
    if !buf.is_empty() {
        emit(&String::from_utf8_lossy(&buf));
    }
}

/// Stream a child's stdout/stderr into `tracing`: stdout → info, stderr → warn,
/// both tagged with the game id and a process label so they show up in the
/// `cargo run` terminal and the log file. Takes the pipe handles; the caller
/// keeps ownership of the `Child` (used by the launch watcher, which still
/// `wait()`s on it).
pub(crate) fn forward_output(child: &mut tokio::process::Child, proc: &'static str, game_id: &str) {
    if let Some(stdout) = child.stdout.take() {
        let game = game_id.to_string();
        tokio::spawn(async move {
            pump(stdout, |line| {
                tracing::info!(target: "subprocess", game = %game, proc, "{line}");
            })
            .await;
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let game = game_id.to_string();
        tokio::spawn(async move {
            pump(stderr, |line| {
                tracing::warn!(target: "subprocess", game = %game, proc, "{line}");
            })
            .await;
        });
    }
}

/// Spawn `cmd` with piped output, stream it to `tracing`, and wait — killing
/// the child if `cancel` fires. Logs spawn (with the command) and exit code.
/// The single choke point for subprocess logging used by the install steps.
pub(crate) async fn run_logged(
    mut cmd: tokio::process::Command,
    proc: &'static str,
    game_id: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<std::process::ExitStatus> {
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    tracing::info!(target: "subprocess", game = %game_id, proc, cmd = ?cmd.as_std(), "spawn");
    let mut child = cmd.spawn().with_context(|| format!("spawning {proc}"))?;
    forward_output(&mut child, proc, game_id);
    let status = tokio::select! {
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            anyhow::bail!("{proc} cancelled");
        }
        status = child.wait() => status?,
    };
    tracing::info!(target: "subprocess", game = %game_id, proc, code = status.code(), "exit");
    Ok(status)
}

/// Default uninstall plan: pause the game's sync folders, then remove the
/// managed roots. Sync folders are paused (not deleted) so peers never see
/// a deletion wave (PLAN.md Risk 2); update flows resume them afterwards.
pub fn default_uninstall(
    folders: Vec<SyncFolderSpec>,
    dirs: &GameDirs,
) -> Vec<Box<dyn InstallStep>> {
    vec![Box::new(RemovePathsStep {
        pause_folders: folders,
        paths: vec![dirs.install_root.clone(), dirs.prefix.clone()],
    })]
}

/// Write a sentinel marker after a completed extraction/step so `is_done`
/// stays cheap (no re-hashing of gigabytes).
pub(crate) fn write_sentinel(path: &std::path::Path, content: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

pub(crate) fn sentinel_matches(path: &std::path::Path, content: &str) -> bool {
    std::fs::read_to_string(path)
        .map(|c| c == content)
        .unwrap_or(false)
}
