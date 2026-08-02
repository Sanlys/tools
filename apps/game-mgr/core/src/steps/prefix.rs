//! Create/verify a Proton prefix by running wineboot once through umu.

#[cfg(not(windows))]
use anyhow::bail;
use tokio_util::sync::CancellationToken;

#[cfg(not(windows))]
use crate::game::Progress;
use crate::game::{GameCtx, InstallStep, ProgressSink};
#[cfg(not(windows))]
use crate::run::{UmuLaunch, find_umu, resolve_proton_dir};

pub struct EnsurePrefixStep {
    /// umu database id; `None` lets umu use its default.
    pub umu_game_id: Option<String>,
    pub store: String,
    /// Class default GE-Proton version (user override comes from ctx).
    pub proton_default: Option<String>,
}

#[async_trait::async_trait]
impl InstallStep for EnsurePrefixStep {
    fn id(&self) -> String {
        "ensure-prefix".into()
    }

    fn label(&self) -> String {
        "Prepare Wine/Proton prefix".into()
    }

    #[cfg(not(windows))]
    async fn is_done(&self, ctx: &GameCtx) -> anyhow::Result<bool> {
        // a usable prefix has its registry hives in place
        Ok(ctx.dirs.prefix.join("system.reg").is_file()
            || ctx.dirs.prefix.join("pfx/system.reg").is_file())
    }

    /// No Wine prefix concept on Windows -- the game's own Windows build
    /// runs directly (see `crate::classes::gog::GogGame::spawn`'s Windows
    /// half), so this step is always already "done" there.
    #[cfg(windows)]
    async fn is_done(&self, _ctx: &GameCtx) -> anyhow::Result<bool> {
        Ok(true)
    }

    #[cfg(not(windows))]
    async fn run(
        &self,
        ctx: &GameCtx,
        progress: &ProgressSink,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        progress.send(Progress::Message(self.label()));
        tokio::fs::create_dir_all(&ctx.dirs.prefix).await?;
        let umu = find_umu(&ctx.services)?;
        let proton_dir = resolve_proton_dir(
            &ctx.services,
            ctx.proton_override.as_deref(),
            self.proton_default.as_deref(),
        );
        tracing::info!(
            target: "install",
            game = %ctx.game_id,
            prefix = %ctx.dirs.prefix.display(),
            proton = ?proton_dir,
            "preparing prefix",
        );

        let launch = UmuLaunch {
            // umu treats "createprefix" as a no-op exe that just sets up
            // the prefix
            exe: "createprefix".into(),
            prefix: ctx.dirs.prefix.clone(),
            proton_dir,
            game_id: self.umu_game_id.clone(),
            store: self.store.clone(),
        };
        let mut cmd = launch.command(&umu);
        cmd.current_dir(&ctx.dirs.prefix);
        let status =
            crate::steps::run_logged(cmd.into(), "umu createprefix", &ctx.game_id, cancel).await?;
        if !status.success() {
            bail!("umu prefix setup failed with {status}");
        }
        if !self.is_done(ctx).await? {
            bail!(
                "prefix setup ran but {} has no system.reg — check umu/proton installation",
                ctx.dirs.prefix.display()
            );
        }
        Ok(())
    }

    #[cfg(windows)]
    async fn run(
        &self,
        _ctx: &GameCtx,
        _progress: &ProgressSink,
        _cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}
