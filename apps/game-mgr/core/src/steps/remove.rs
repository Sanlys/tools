//! Uninstall/update support: pause sync folders whose paths live inside the
//! doomed roots *before* deleting, so peers never receive a deletion wave
//! (PLAN.md Risk 2).

use std::path::PathBuf;

use tokio_util::sync::CancellationToken;

use crate::game::{GameCtx, InstallStep, Progress, ProgressSink, SyncFolderSpec};

pub struct RemovePathsStep {
    pub pause_folders: Vec<SyncFolderSpec>,
    pub paths: Vec<PathBuf>,
}

#[async_trait::async_trait]
impl InstallStep for RemovePathsStep {
    fn id(&self) -> String {
        "remove-roots".into()
    }

    fn label(&self) -> String {
        "Remove game files".into()
    }

    async fn is_done(&self, _ctx: &GameCtx) -> anyhow::Result<bool> {
        Ok(self.paths.iter().all(|p| !p.exists()))
    }

    async fn run(
        &self,
        ctx: &GameCtx,
        progress: &ProgressSink,
        _cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        // pause first — deleting under a live folder propagates deletions.
        // Best-effort: if Syncthing is unreachable it isn't replicating these
        // deletions anyway, and a folder that was never created has nothing to
        // pause — neither should abort the uninstall and leave it stuck.
        for folder in &self.pause_folders {
            let inside_doomed = self
                .paths
                .iter()
                .any(|root| folder.local_path.starts_with(root));
            if inside_doomed {
                progress.send(Progress::Message(format!("Pausing {}", folder.folder_id)));
                match ctx.services.syncthing() {
                    Ok(syncthing) => {
                        if let Err(err) = syncthing.set_paused(&folder.folder_id, true).await {
                            tracing::warn!(
                                folder = %folder.folder_id,
                                %err,
                                "could not pause sync folder before removal; continuing",
                            );
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            folder = %folder.folder_id,
                            %err,
                            "Syncthing unavailable; removing without pausing",
                        );
                    }
                }
            }
        }
        for path in &self.paths {
            if path.exists() {
                progress.send(Progress::Message(format!("Removing {}", path.display())));
                tokio::fs::remove_dir_all(path).await?;
            }
        }
        Ok(())
    }
}
