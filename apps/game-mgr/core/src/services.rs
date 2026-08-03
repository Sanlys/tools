//! Shared service container handed to game classes and steps.

use std::path::PathBuf;
use std::sync::Arc;

use crate::config::ClientConfig;
use crate::game::GameDirs;
use crate::stats::ServerClient;
use crate::syncthing::SyncthingClient;

pub struct Services {
    pub config: ClientConfig,
    pub http: reqwest::Client,
    /// `None` when no server is configured; steps that need bucket access
    /// (downloads, sidecar-less hashing) fail with a pointer to the missing
    /// `server_url` setting. There is no client-side S3 config anymore --
    /// every bucket access goes through the backend (PLAN.md §4.3, see
    /// `crate::s3`'s module doc).
    pub server: Option<Arc<ServerClient>>,
    /// `Err` carries the reason syncthing is unavailable. Installs are
    /// fail-fast on this (decision: syncthing is required).
    pub syncthing: Result<Arc<SyncthingClient>, String>,
    pub library_dir: PathBuf,
    pub tools_dir: PathBuf,
    pub downloads_dir: PathBuf,
}

impl Services {
    pub fn server(&self) -> anyhow::Result<&ServerClient> {
        self.server.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "no server configured -- bucket access needs `server_url` set \
                 (see docs/dev-setup.md)"
            )
        })
    }

    pub fn syncthing(&self) -> anyhow::Result<&SyncthingClient> {
        match &self.syncthing {
            Ok(client) => Ok(client),
            Err(reason) => anyhow::bail!(
                "Syncthing is required but unavailable: {reason} \
                 (saves always sync — see docs/dev-setup.md)"
            ),
        }
    }

    /// Per-game directory layout (PLAN.md §6.6).
    pub fn dirs_for(&self, id: &str) -> GameDirs {
        GameDirs {
            install_root: self.library_dir.join(id),
            prefix: crate::paths::data_dir().join("prefixes").join(id),
            downloads: self.downloads_dir.join(id),
        }
    }

    /// Locate an external tool binary: explicit config dir → `$PATH` →
    /// managed download dir (PLAN.md §4.3/4.4).
    pub fn find_tool(&self, name: &str, version: &str, exe_rel: &str) -> Option<PathBuf> {
        // managed install location
        let managed = self.tools_dir.join(name).join(version).join(exe_rel);
        if managed.is_file() {
            return Some(managed);
        }
        // $PATH (NixOS/Arch system installs)
        which(name)
    }
}

/// Minimal `which` (no extra dependency).
pub fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(bin))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    // `sh` isn't a meaningful PATH lookup on Windows (no file literally
    // named `sh`, and Git Bash's `sh.exe` wouldn't match this exact-name
    // `which` anyway) -- caught by the rust-windows CI job actually running
    // this test, not just compiling it.
    #[test]
    #[cfg(unix)]
    fn which_finds_sh() {
        let found = which("sh").expect("sh should exist on any unix test machine");
        assert!(found.ends_with("sh"));
    }

    #[test]
    fn which_misses_nonsense() {
        assert!(which("definitely-not-a-real-binary-gm").is_none());
    }
}
