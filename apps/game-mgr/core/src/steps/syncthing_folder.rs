//! Ensure a game's Syncthing folder exists, is shared with all peers and
//! carries its ignore patterns; optionally wait for the initial sync.

use std::time::Duration;

use anyhow::bail;
use tokio_util::sync::CancellationToken;

use crate::game::{GameCtx, InstallStep, Progress, ProgressSink, SyncFolderSpec};

pub struct EnsureSyncFolderStep {
    pub spec: SyncFolderSpec,
    /// Cap on waiting for initial sync (`required_before_first_launch`).
    pub wait_timeout: Duration,
}

impl EnsureSyncFolderStep {
    pub fn new(spec: SyncFolderSpec) -> Self {
        Self {
            spec,
            wait_timeout: Duration::from_secs(600),
        }
    }
}

#[async_trait::async_trait]
impl InstallStep for EnsureSyncFolderStep {
    fn id(&self) -> String {
        format!("syncfolder:{}", self.spec.folder_id)
    }

    fn label(&self) -> String {
        format!("Sync folder {}", self.spec.folder_id)
    }

    async fn is_done(&self, ctx: &GameCtx) -> anyhow::Result<bool> {
        // cheap config check; ensure_folder is idempotent so re-running on
        // unclear state is fine
        let syncthing = ctx.services.syncthing()?;
        match syncthing.folder(&self.spec.folder_id).await? {
            Some(folder) => {
                Ok(std::path::Path::new(&folder.path) == self.spec.local_path.as_path())
            }
            None => Ok(false),
        }
    }

    async fn run(
        &self,
        ctx: &GameCtx,
        progress: &ProgressSink,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        progress.send(Progress::Message(self.label()));
        let syncthing = ctx.services.syncthing()?;
        syncthing.ensure_folder(&self.spec).await?;

        if self.spec.required_before_first_launch {
            progress.send(Progress::Message(format!(
                "Waiting for {} to finish its initial sync…",
                self.spec.folder_id
            )));
            let deadline = tokio::time::Instant::now() + self.wait_timeout;
            loop {
                if cancel.is_cancelled() {
                    bail!("sync wait cancelled");
                }
                let completion = syncthing
                    .completion(&self.spec.folder_id)
                    .await
                    .unwrap_or(0.0);
                if completion >= 100.0 {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    // PLAN.md Risk 8: don't deadlock when the only peer with
                    // data is offline — surface and continue
                    tracing::warn!(
                        folder = %self.spec.folder_id,
                        completion,
                        "initial sync wait timed out; continuing (peer offline?)"
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
        Ok(())
    }
}
