//! Guided manual steps: the engine parks on these and the UI wizard drives
//! confirmation; `run` is never called (PLAN.md §4.2).

use tokio_util::sync::CancellationToken;

use crate::game::{GameCtx, InstallStep, ManualStep, ProgressSink};

pub struct GuidedManualStep {
    pub step_id: String,
    pub title: String,
    pub manual: ManualStep,
}

#[async_trait::async_trait]
impl InstallStep for GuidedManualStep {
    fn id(&self) -> String {
        format!("manual:{}", self.step_id)
    }

    fn label(&self) -> String {
        self.title.clone()
    }

    fn manual(&self) -> Option<ManualStep> {
        Some(self.manual.clone())
    }

    async fn is_done(&self, ctx: &GameCtx) -> anyhow::Result<bool> {
        // already satisfied (e.g. re-run after completion): verify passes
        Ok((self.manual.verify)(ctx).is_ok())
    }

    async fn run(
        &self,
        _ctx: &GameCtx,
        _progress: &ProgressSink,
        _cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        unreachable!("manual steps are confirmed through the wizard, not run")
    }
}
