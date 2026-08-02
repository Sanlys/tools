//! The only module allowed OS-specific isms (PLAN.md §14): process/file
//! permission handling. `#[cfg(unix)]` covers Linux (the only platform this
//! has run on so far); `#[cfg(windows)]` is this port's first Windows slice
//! -- see its doc comments below for what's a full equivalent of the Unix
//! behavior and what's a deliberately narrower first pass.

use std::path::Path;

#[cfg(unix)]
pub fn make_executable(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

/// Windows has no executable permission bit -- a `.exe`/`.bat`/etc. is
/// executable by extension alone, so a downloaded tool (innoextract, an
/// AppImage-equivalent, ...) never needs this step there. A genuine no-op,
/// not a stand-in for missing functionality.
#[cfg(windows)]
pub fn make_executable(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// Write a file readable only by the current user (token storage).
#[cfg(unix)]
pub fn write_private(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)?;
    Ok(())
}

/// Write a file intended to be readable only by the current user. On
/// Windows this currently relies on the *default* ACL a file inherits from
/// its parent directory -- correct for the common case (this always lands
/// under `%LOCALAPPDATA%`/`dirs::state_dir()`, itself owner-restricted by
/// default), but it is **not** an explicit, enforced equivalent of Unix's
/// `0600` the way `write_private` above is: a caller (or a misconfigured
/// parent directory) could end up with a more permissive ACL and this
/// function wouldn't correct it. Tightening this for real means setting an
/// explicit DACL via `windows-sys`/`windows` crate's security APIs --
/// deliberately left for a follow-up rather than landing untested Win32 ACL
/// code with no Windows machine in this port's own CI to verify it against.
#[cfg(windows)]
pub fn write_private(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn private_files_are_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/auth.json");
        write_private(&path, b"{\"secret\":true}").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"secret\":true}");
    }

    #[test]
    fn make_executable_sets_exec_bits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool");
        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        make_executable(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111);
    }
}
