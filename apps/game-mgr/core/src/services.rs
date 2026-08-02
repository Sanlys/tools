//! Shared service container handed to game classes and steps.

use std::path::PathBuf;
use std::sync::Arc;

use crate::config::ClientConfig;
use crate::game::GameDirs;
use crate::s3::S3Client;
use crate::syncthing::SyncthingClient;

pub struct Services {
    pub config: ClientConfig,
    pub http: reqwest::Client,
    /// `None` when S3 is not configured; steps that need it fail with a
    /// pointer to the missing `GM_S3_*` settings.
    pub s3: Option<Arc<S3Client>>,
    /// `Err` carries the reason syncthing is unavailable. Installs are
    /// fail-fast on this (decision: syncthing is required).
    pub syncthing: Result<Arc<SyncthingClient>, String>,
    pub library_dir: PathBuf,
    pub tools_dir: PathBuf,
    pub downloads_dir: PathBuf,
}

impl Services {
    pub fn s3(&self) -> anyhow::Result<&S3Client> {
        self.s3.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "S3 is not configured — set GM_S3_ENDPOINT, GM_S3_BUCKET, \
                 GM_S3_ACCESS_KEY_ID and GM_S3_SECRET_ACCESS_KEY (see docs/dev-setup.md)"
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

    #[test]
    fn which_finds_sh() {
        let found = which("sh").expect("sh should exist on any unix test machine");
        assert!(found.ends_with("sh"));
    }

    #[test]
    fn which_misses_nonsense() {
        assert!(which("definitely-not-a-real-binary-gm").is_none());
    }
}
