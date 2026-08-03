//! Extraction steps: GOG offline installers via `innoextract` (external
//! tool), zip archives in-process.

use std::path::PathBuf;

use anyhow::{Context, bail};
use tokio_util::sync::CancellationToken;

use crate::game::{GameCtx, InstallStep, Progress, ProgressSink};
use crate::services::which;

/// Run `innoextract --gog` on a GOG offline installer (multi-part `.bin`
/// files are picked up automatically from the same directory).
pub struct InnoExtractStep {
    pub installer: PathBuf,
    pub out_dir: PathBuf,
}

impl InnoExtractStep {
    /// Per-installer sentinel so several installers (base + patches + DLC) can
    /// overlay into the *same* out_dir without clobbering each other's marker.
    fn sentinel(&self) -> PathBuf {
        let name = self
            .installer
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        self.out_dir.join(format!(".gm-extracted.{name}"))
    }

    fn sentinel_content(&self) -> String {
        format!("innoextract:{}", self.installer.display())
    }
}

#[async_trait::async_trait]
impl InstallStep for InnoExtractStep {
    fn id(&self) -> String {
        format!("innoextract:{}", self.installer.display())
    }

    fn label(&self) -> String {
        format!(
            "Extract {}",
            self.installer
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        )
    }

    async fn is_done(&self, _ctx: &GameCtx) -> anyhow::Result<bool> {
        Ok(crate::steps::sentinel_matches(
            &self.sentinel(),
            &self.sentinel_content(),
        ))
    }

    async fn run(
        &self,
        ctx: &GameCtx,
        progress: &ProgressSink,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        let innoextract = which("innoextract").context(
            "innoextract not found — install it (pacman -S innoextract / nixpkgs) — see docs/dev-setup.md",
        )?;
        tokio::fs::create_dir_all(&self.out_dir).await?;
        progress.send(Progress::Message(self.label()));

        let mut cmd = tokio::process::Command::new(&innoextract);
        // no --silent: innoextract then streams its progress (captured into the
        // terminal + log via run_logged, incl. the \r-updated progress bar).
        cmd.arg("--gog")
            .arg("--progress")
            .arg("-d")
            .arg(&self.out_dir)
            .arg(&self.installer);
        let status = crate::steps::run_logged(cmd, "innoextract", &ctx.game_id, cancel).await?;
        if !status.success() {
            bail!(
                "innoextract failed with {status} for {}",
                self.installer.display()
            );
        }
        // innoextract can exit 0 without producing anything useful (e.g. a
        // native Linux installer misclassified as a base .exe). Don't write
        // the done-sentinel — and don't let the install report success — until
        // we've confirmed real files landed; otherwise the game reaches
        // "Installed" with a missing executable and Play silently fails.
        let produced = count_files(&self.out_dir).await?;
        tracing::info!(
            target: "install",
            game = %ctx.game_id,
            out_dir = %self.out_dir.display(),
            files = produced,
            "innoextract finished",
        );
        if produced == 0 {
            bail!(
                "innoextract reported success but produced no files in {} — the base file \
                 may be a native Linux installer (.sh) misclassified as base, or a partial \
                 upload; check the log",
                self.out_dir.display()
            );
        }
        crate::steps::write_sentinel(&self.sentinel(), &self.sentinel_content())
    }
}

/// Count regular files anywhere under `dir` (recursively). Used to verify an
/// extraction actually produced output.
async fn count_files(dir: &std::path::Path) -> anyhow::Result<usize> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut count = 0usize;
        let mut stack = vec![dir];
        while let Some(d) = stack.pop() {
            let entries = match std::fs::read_dir(&d) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    count += 1;
                }
            }
        }
        count
    })
    .await
    .map_err(Into::into)
}

/// In-process zip extraction (tool archives, simple game data).
pub struct ZipExtractStep {
    pub archive: PathBuf,
    pub out_dir: PathBuf,
}

#[async_trait::async_trait]
impl InstallStep for ZipExtractStep {
    fn id(&self) -> String {
        format!("unzip:{}", self.archive.display())
    }

    fn label(&self) -> String {
        format!(
            "Unpack {}",
            self.archive
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        )
    }

    async fn is_done(&self, _ctx: &GameCtx) -> anyhow::Result<bool> {
        Ok(crate::steps::sentinel_matches(
            &self.out_dir.join(".gm-extracted"),
            &format!("zip:{}", self.archive.display()),
        ))
    }

    async fn run(
        &self,
        _ctx: &GameCtx,
        progress: &ProgressSink,
        _cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        progress.send(Progress::Message(self.label()));
        let archive = self.archive.clone();
        let out_dir = self.out_dir.clone();
        tokio::task::spawn_blocking(move || extract_zip(&archive, &out_dir)).await??;
        crate::steps::write_sentinel(
            &self.out_dir.join(".gm-extracted"),
            &format!("zip:{}", self.archive.display()),
        )
    }
}

/// Extract any archive we understand by file extension (zip, 7z, tar.gz).
/// Used for bucket artifacts whose format isn't fixed (MO2, SKSE).
pub struct ExtractArchiveStep {
    pub archive: PathBuf,
    pub out_dir: PathBuf,
    /// When the archive wraps everything in a single top-level directory
    /// (e.g. SKSE's `skse64_<ver>/…`), hoist that directory's contents up so
    /// they land directly in `out_dir` (the loader/DLLs next to the game exe).
    pub strip_top_level: bool,
}

impl ExtractArchiveStep {
    fn sentinel(&self) -> PathBuf {
        let name = self
            .archive
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        self.out_dir.join(format!(".gm-extracted.{name}"))
    }

    fn sentinel_content(&self) -> String {
        format!("archive:{}", self.archive.display())
    }
}

#[async_trait::async_trait]
impl InstallStep for ExtractArchiveStep {
    fn id(&self) -> String {
        format!("extract:{}", self.archive.display())
    }

    fn label(&self) -> String {
        format!(
            "Unpack {}",
            self.archive
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        )
    }

    async fn is_done(&self, _ctx: &GameCtx) -> anyhow::Result<bool> {
        Ok(crate::steps::sentinel_matches(
            &self.sentinel(),
            &self.sentinel_content(),
        ))
    }

    async fn run(
        &self,
        _ctx: &GameCtx,
        progress: &ProgressSink,
        _cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        progress.send(Progress::Message(self.label()));
        let archive = self.archive.clone();
        let out_dir = self.out_dir.clone();
        let strip = self.strip_top_level;
        tokio::task::spawn_blocking(move || extract_archive(&archive, &out_dir, strip)).await??;
        crate::steps::write_sentinel(&self.sentinel(), &self.sentinel_content())
    }
}

/// Extract into `out_dir`, optionally hoisting a single wrapping top-level
/// directory. Extracts to a temp dir first, then merges into `out_dir`
/// (overlaying onto existing files — SKSE drops next to the game exe and its
/// `Data/` merges with the game's `Data/`).
pub(crate) fn extract_archive(
    archive: &std::path::Path,
    out_dir: &std::path::Path,
    strip_top_level: bool,
) -> anyhow::Result<()> {
    if !strip_top_level {
        return extract_any(archive, out_dir);
    }
    std::fs::create_dir_all(out_dir)?;
    let tmp = out_dir.join(".gm-extract-tmp");
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)?;
    }
    std::fs::create_dir_all(&tmp)?;
    extract_any(archive, &tmp)?;

    // hoist a lone wrapping directory; otherwise merge the temp root as-is
    let entries: Vec<_> = std::fs::read_dir(&tmp)?.flatten().collect();
    let src = match entries.as_slice() {
        [only] if only.path().is_dir() => only.path(),
        _ => tmp.clone(),
    };
    merge_dir(&src, out_dir)?;
    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Recursively copy `src` into `dst`, creating directories and overwriting
/// files (a merge, so it overlays onto an existing tree).
fn merge_dir(src: &std::path::Path, dst: &std::path::Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            merge_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .with_context(|| format!("copying {} → {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

/// Dispatch extraction by extension. `.7z`, `.zip`, `.tar.gz`/`.tgz`.
pub(crate) fn extract_any(
    archive: &std::path::Path,
    out_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let name = archive.to_string_lossy().to_lowercase();
    if name.ends_with(".7z") {
        extract_7z(archive, out_dir)
    } else if name.ends_with(".zip") {
        extract_zip(archive, out_dir)
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        extract_tar_gz(archive, out_dir)
    } else {
        bail!(
            "unsupported archive format: {} (expected .7z/.zip/.tar.gz)",
            archive.display()
        )
    }
}

/// In-process 7z extraction (pure Rust) for MO2/SKSE archives.
pub(crate) fn extract_7z(
    archive: &std::path::Path,
    out_dir: &std::path::Path,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(out_dir)?;
    sevenz_rust2::decompress_file(archive, out_dir)
        .with_context(|| format!("extracting 7z {}", archive.display()))?;
    Ok(())
}

pub(crate) fn extract_zip(
    archive: &std::path::Path,
    out_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let file =
        std::fs::File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file)?;
    std::fs::create_dir_all(out_dir)?;
    zip.extract(out_dir).context("extracting zip")?;
    Ok(())
}

/// tar.gz extraction used by the tool installer.
pub(crate) fn extract_tar_gz(
    archive: &std::path::Path,
    out_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let file =
        std::fs::File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    std::fs::create_dir_all(out_dir)?;
    tar.unpack(out_dir).context("unpacking tar.gz")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn zip_roundtrip_extracts_files() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("fixture.zip");

        let file = std::fs::File::create(&zip_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        writer.add_directory("sub", opts).unwrap();
        writer.start_file("sub/hello.txt", opts).unwrap();
        writer.write_all(b"world").unwrap();
        writer.finish().unwrap();

        let out = dir.path().join("out");
        extract_zip(&zip_path, &out).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("sub/hello.txt")).unwrap(),
            "world"
        );
    }

    #[test]
    fn seven_z_roundtrip_extracts_files() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("fixture.7z");

        // build a tiny 7z with sevenz-rust2's writer
        let mut writer = sevenz_rust2::SevenZWriter::create(&archive).unwrap();
        writer
            .push_archive_entry(
                sevenz_rust2::SevenZArchiveEntry::new_file("Data/skse64_loader.exe"),
                Some(&b"MZ-fake-skse"[..]),
            )
            .unwrap();
        writer.finish().unwrap();

        let out = dir.path().join("out");
        extract_7z(&archive, &out).unwrap();
        assert_eq!(
            std::fs::read(out.join("Data/skse64_loader.exe")).unwrap(),
            b"MZ-fake-skse"
        );

        // extract_any dispatches on extension to the same result
        let out2 = dir.path().join("out2");
        extract_any(&archive, &out2).unwrap();
        assert!(out2.join("Data/skse64_loader.exe").is_file());
    }

    #[test]
    fn strip_top_level_hoists_wrapping_dir_and_merges() {
        let dir = tempfile::tempdir().unwrap();
        // SKSE-style archive: everything wrapped in one skse64_<ver>/ folder
        let archive = dir.path().join("skse64_2_02_06.7z");
        let mut writer = sevenz_rust2::SevenZWriter::create(&archive).unwrap();
        for (name, body) in [
            ("skse64_2_02_06/skse64_loader.exe", &b"loader"[..]),
            ("skse64_2_02_06/skse64_2_02_06.dll", &b"dll"[..]),
            ("skse64_2_02_06/Data/Scripts/skse.pex", &b"pex"[..]),
        ] {
            writer
                .push_archive_entry(sevenz_rust2::SevenZArchiveEntry::new_file(name), Some(body))
                .unwrap();
        }
        writer.finish().unwrap();

        // out_dir already has the game (incl. a Data/ to merge into)
        let out = dir.path().join("game");
        std::fs::create_dir_all(out.join("Data")).unwrap();
        std::fs::write(out.join("SkyrimSE.exe"), b"game").unwrap();
        std::fs::write(out.join("Data/Skyrim.esm"), b"esm").unwrap();

        extract_archive(&archive, &out, true).unwrap();

        // loader landed at the game root, not under skse64_2_02_06/
        assert!(out.join("skse64_loader.exe").is_file());
        assert!(out.join("skse64_2_02_06.dll").is_file());
        assert!(
            !out.join("skse64_2_02_06").exists(),
            "wrapping dir not hoisted"
        );
        // SKSE's Data merged with the game's existing Data
        assert!(out.join("Data/Scripts/skse.pex").is_file());
        assert!(out.join("Data/Skyrim.esm").is_file());
        // temp dir cleaned up
        assert!(!out.join(".gm-extract-tmp").exists());
    }

    #[test]
    fn extract_any_rejects_unknown_format() {
        let dir = tempfile::tempdir().unwrap();
        let bogus = dir.path().join("file.rar");
        std::fs::write(&bogus, b"x").unwrap();
        let err = extract_any(&bogus, &dir.path().join("o"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unsupported"), "{err}");
    }

    #[test]
    fn tar_gz_roundtrip_extracts_files() {
        let dir = tempfile::tempdir().unwrap();
        let tar_path = dir.path().join("fixture.tar.gz");

        let file = std::fs::File::create(&tar_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let payload = b"binary-here";
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "tool/bin/run", payload.as_slice())
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();

        let out = dir.path().join("out");
        extract_tar_gz(&tar_path, &out).unwrap();
        assert_eq!(std::fs::read(out.join("tool/bin/run")).unwrap(), payload);
    }
}
