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

/// Windows' own default `PATHEXT`, used when the env var isn't set (rare,
/// but not impossible for a launched-without-a-shell process).
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// Minimal `which` (no extra dependency).
pub fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    if cfg!(windows) {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| DEFAULT_PATHEXT.to_string());
        which_windows(bin, &dirs, &pathext)
    } else {
        dirs.into_iter()
            .map(|dir| dir.join(bin))
            .find(|c| c.is_file())
    }
}

/// A PATH lookup on Windows resolves `PATHEXT` extensions
/// (`.EXE`/`.BAT`/`.CMD`/...) onto a bare command name -- there's no file
/// literally named `innoextract` on disk, only `innoextract.exe`, so
/// matching `bin` verbatim (the Unix behavior above) always misses it even
/// when the tool is genuinely installed and on PATH (winget, scoop, a
/// manual PATH entry -- all of them). Split out as its own function, not
/// `#[cfg(windows)]`-gated, so it's exercised by tests on every platform
/// this crate's CI actually runs, not only the Windows job.
fn which_windows(bin: &str, dirs: &[PathBuf], pathext: &str) -> Option<PathBuf> {
    // Caller already spelled out the extension (e.g. "run.bat") -- trust it
    // rather than trying to also append PATHEXT suffixes on top.
    if std::path::Path::new(bin).extension().is_some() {
        return dirs.iter().map(|dir| dir.join(bin)).find(|c| c.is_file());
    }
    for dir in dirs {
        for ext in pathext.split(';').filter(|e| !e.is_empty()) {
            let candidate = dir.join(format!("{bin}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // `sh` isn't a meaningful PATH lookup on Windows (no file literally
    // named `sh`, just Git Bash's `sh.exe` if present at all) -- exercises
    // the Unix branch of `which`; `which_windows` below covers the other.
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

    // Not `#[cfg(windows)]`: `which_windows` is pure path/string logic with
    // no actual Windows API calls, so it's worth running everywhere this
    // crate's tests already run rather than only on the rust-windows CI
    // job -- this is the exact scenario that shipped broken (innoextract
    // installed via winget, on PATH, as `innoextract.exe`, invisible to a
    // lookup that only ever matched the bare name).
    #[test]
    fn which_windows_resolves_pathext_suffix() {
        let dir = tempfile::tempdir().unwrap();
        // `DEFAULT_PATHEXT`'s conventional uppercase (".EXE") would still
        // match a real installer's lowercase `innoextract.exe` on an actual
        // Windows machine -- NTFS/FAT file lookups are case-insensitive at
        // the OS level, which `std::path::Path::is_file()` correctly
        // inherits without this crate doing anything extra for it. This
        // test runs on whatever case-sensitive filesystem CI happens to
        // use though, so it matches the exact case `which_windows` will
        // construct rather than relying on OS case-folding this code
        // doesn't itself implement.
        std::fs::write(dir.path().join("innoextract.EXE"), b"").unwrap();
        let found = which_windows("innoextract", &[dir.path().to_path_buf()], DEFAULT_PATHEXT)
            .expect("should find innoextract.EXE via PATHEXT");
        assert_eq!(found, dir.path().join("innoextract.EXE"));
    }

    #[test]
    fn which_windows_is_case_insensitive_on_extension() {
        // PATHEXT entries are conventionally uppercase (".EXE"); NTFS/FAT
        // are case-insensitive so this matters in practice, but tempfile
        // names here are literal bytes on any filesystem -- assert the
        // lookup logic doesn't itself lowercase/uppercase anything that
        // would break a case-insensitive filesystem's real match.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tool.EXE"), b"").unwrap();
        let found = which_windows("tool", &[dir.path().to_path_buf()], ".EXE")
            .expect("should find tool.EXE");
        assert_eq!(found, dir.path().join("tool.EXE"));
    }

    #[test]
    fn which_windows_respects_an_explicit_extension() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("run.bat"), b"").unwrap();
        let found = which_windows("run.bat", &[dir.path().to_path_buf()], DEFAULT_PATHEXT)
            .expect("should find run.bat verbatim");
        assert_eq!(found, dir.path().join("run.bat"));
    }

    #[test]
    fn which_windows_misses_nonsense() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            which_windows(
                "definitely-not-a-real-binary-gm",
                &[dir.path().to_path_buf()],
                DEFAULT_PATHEXT
            )
            .is_none()
        );
    }
}
