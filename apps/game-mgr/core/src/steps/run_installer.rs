//! Run an InnoSetup-style installer inside the game's prefix via umu —
//! used for optional GOG patch/DLC installers selected at install time
//! (and as the Risk-6 fallback for installers innoextract can't handle).

use std::path::PathBuf;

use anyhow::bail;
use tokio_util::sync::CancellationToken;

use crate::game::{GameCtx, InstallStep, Progress, ProgressSink};
use crate::run::{UmuLaunch, find_umu, resolve_proton_dir};

pub struct RunInstallerInPrefixStep {
    pub installer: PathBuf,
    /// umu database id; `None` lets umu use its default.
    pub umu_game_id: Option<String>,
    pub store: String,
    pub proton_default: Option<String>,
}

impl RunInstallerInPrefixStep {
    fn sentinel(&self, ctx: &GameCtx) -> PathBuf {
        let name = self
            .installer
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        ctx.dirs.prefix.join(format!(".gm-applied-{name}"))
    }
}

#[async_trait::async_trait]
impl InstallStep for RunInstallerInPrefixStep {
    fn id(&self) -> String {
        format!("run-installer:{}", self.installer.display())
    }

    fn label(&self) -> String {
        format!(
            "Apply {}",
            self.installer
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        )
    }

    async fn is_done(&self, ctx: &GameCtx) -> anyhow::Result<bool> {
        Ok(self.sentinel(ctx).is_file())
    }

    async fn run(
        &self,
        ctx: &GameCtx,
        progress: &ProgressSink,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        progress.send(Progress::Message(self.label()));
        anyhow::ensure!(
            self.installer.is_file(),
            "installer missing: {}",
            self.installer.display()
        );
        let umu = find_umu(&ctx.services)?;
        let launch = UmuLaunch {
            exe: self.installer.clone(),
            prefix: ctx.dirs.prefix.clone(),
            proton_dir: resolve_proton_dir(
                &ctx.services,
                ctx.proton_override.as_deref(),
                self.proton_default.as_deref(),
            ),
            game_id: self.umu_game_id.clone(),
            store: self.store.clone(),
        };
        let mut cmd = launch.command(&umu);
        // InnoSetup silent flags; GOG patches detect the install themselves
        cmd.arg("/VERYSILENT")
            .arg("/SUPPRESSMSGBOXES")
            .arg("/NORESTART");
        let status =
            crate::steps::run_logged(cmd.into(), "umu installer", &ctx.game_id, cancel).await?;
        if !status.success() {
            bail!(
                "{} exited with {status} — run it manually in the prefix to inspect",
                self.installer.display()
            );
        }
        crate::steps::write_sentinel(&self.sentinel(ctx), "applied")
    }
}
