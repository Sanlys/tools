//! Fetch one bucket artifact with resume + sha256 verification.

use std::path::PathBuf;

use tokio_util::sync::CancellationToken;

use crate::game::{ArtifactRef, GameCtx, InstallStep, Progress, ProgressSink};

pub struct S3FetchStep {
    pub artifact: ArtifactRef,
    /// Final location, e.g. `<downloads>/<game>/<filename>`.
    pub dest: PathBuf,
}

impl S3FetchStep {
    pub fn into_downloads(artifact: ArtifactRef, ctx: &GameCtx) -> Self {
        let filename = artifact
            .bucket_key
            .rsplit('/')
            .next()
            .unwrap_or(&artifact.bucket_key)
            .to_string();
        let dest = ctx.dirs.downloads.join(filename);
        Self { artifact, dest }
    }
}

#[async_trait::async_trait]
impl InstallStep for S3FetchStep {
    fn id(&self) -> String {
        format!("fetch:{}", self.artifact.bucket_key)
    }

    fn label(&self) -> String {
        format!("Download {}", self.artifact.bucket_key)
    }

    async fn is_done(&self, _ctx: &GameCtx) -> anyhow::Result<bool> {
        // a finished download was verified and renamed into place; the
        // sentinel records which hash it satisfied
        Ok(crate::steps::sentinel_matches(
            &self.dest.with_extension("gm-ok"),
            &self.artifact.sha256,
        ) && self.dest.is_file())
    }

    async fn run(
        &self,
        ctx: &GameCtx,
        progress: &ProgressSink,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        progress.send(Progress::Message(self.label()));
        crate::s3::download(
            &ctx.services.http,
            ctx.services.server()?,
            &self.artifact.bucket_key,
            &self.dest,
            &self.artifact.sha256,
            progress,
            cancel,
        )
        .await?;
        crate::steps::write_sentinel(&self.dest.with_extension("gm-ok"), &self.artifact.sha256)
    }
}
