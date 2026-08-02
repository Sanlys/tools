//! Download + unpack a pinned upstream tool into the managed tools dir
//! (PLAN.md §4.4). Resolution order at use-time: `$PATH` first (NixOS/Arch
//! system installs), managed dir second — this step only runs when neither
//! exists yet.

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::game::{GameCtx, InstallStep, Progress, ProgressSink, ToolSpec};
use crate::services::which;

pub struct InstallToolStep {
    pub spec: ToolSpec,
}

impl InstallToolStep {
    fn install_dir(&self, ctx: &GameCtx) -> std::path::PathBuf {
        ctx.services
            .tools_dir
            .join(self.spec.name)
            .join(self.spec.version)
    }
}

#[async_trait::async_trait]
impl InstallStep for InstallToolStep {
    fn id(&self) -> String {
        format!("tool:{}:{}", self.spec.name, self.spec.version)
    }

    fn label(&self) -> String {
        format!("Install {} {}", self.spec.name, self.spec.version)
    }

    async fn is_done(&self, ctx: &GameCtx) -> anyhow::Result<bool> {
        // a system install satisfies the dependency
        if which(self.spec.name).is_some() {
            return Ok(true);
        }
        Ok(self
            .install_dir(ctx)
            .join(&self.spec.exe_rel_path)
            .is_file())
    }

    async fn run(
        &self,
        ctx: &GameCtx,
        progress: &ProgressSink,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        progress.send(Progress::Message(self.label()));

        // download to cache with hash verification
        let filename = self
            .spec
            .linux_url
            .rsplit('/')
            .next()
            .unwrap_or("tool-archive")
            .to_string();
        let archive = ctx.services.downloads_dir.join("tools").join(&filename);
        tokio::fs::create_dir_all(archive.parent().unwrap()).await?;

        let response = tokio::select! {
            _ = cancel.cancelled() => bail!("tool download cancelled"),
            r = ctx.services.http.get(&self.spec.linux_url).send() => r,
        }
        .with_context(|| format!("downloading {}", self.spec.linux_url))?
        .error_for_status()?;

        let total = response.content_length();
        let mut stream = response;
        let mut hasher = Sha256::new();
        let mut file = tokio::fs::File::create(&archive).await?;
        let mut done = 0u64;
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => bail!("tool download cancelled"),
                c = stream.chunk() => c?,
            };
            let Some(chunk) = chunk else { break };
            hasher.update(&chunk);
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
            done += chunk.len() as u64;
            progress.send(Progress::Bytes { done, total });
        }
        tokio::io::AsyncWriteExt::flush(&mut file).await?;
        drop(file);

        let actual = hex::encode(hasher.finalize());
        if !actual.eq_ignore_ascii_case(&self.spec.sha256) {
            let _ = tokio::fs::remove_file(&archive).await;
            bail!(
                "sha256 mismatch for {} ({}): expected {}, got {actual}",
                self.spec.name,
                self.spec.linux_url,
                self.spec.sha256
            );
        }

        // unpack into the versioned dir
        let install_dir = self.install_dir(ctx);
        tokio::fs::create_dir_all(&install_dir).await?;
        let unpack_to = install_dir.clone();
        let archive_for_unpack = archive.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let name = archive_for_unpack.to_string_lossy();
            if name.ends_with(".zip") {
                crate::steps::extract::extract_zip(&archive_for_unpack, &unpack_to)
            } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
                crate::steps::extract::extract_tar_gz(&archive_for_unpack, &unpack_to)
            } else {
                bail!("unsupported tool archive format: {name}");
            }
        })
        .await??;

        let exe = install_dir.join(&self.spec.exe_rel_path);
        if !exe.is_file() {
            bail!(
                "tool archive for {} did not contain {}",
                self.spec.name,
                self.spec.exe_rel_path
            );
        }
        crate::platform::make_executable(&exe)?;
        Ok(())
    }
}
