//! Resumable install pipeline (PLAN.md §4.2): steps run sequentially,
//! `is_done` short-circuits completed work, manual steps park the run.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::game::{GameCtx, InstallStep, ManualStep, Progress, ProgressSink};
use crate::statedb::StateDb;

/// Where a pipeline run stopped.
#[derive(Debug)]
pub enum RunOutcome {
    /// All steps done.
    Complete,
    /// Parked on a guided manual step; resume by confirming it.
    ManualWait {
        step_id: String,
        label: String,
    },
    Cancelled,
}

/// What the engine reports while running (drives the snapshot/UI).
#[derive(Debug, Clone)]
pub enum EngineEvent {
    StepStarted {
        step_id: String,
        label: String,
        index: usize,
        total: usize,
    },
    StepProgress {
        step_id: String,
        progress: ProgressView,
    },
    StepDone {
        step_id: String,
    },
}

#[derive(Debug, Clone)]
pub enum ProgressView {
    Bytes { done: u64, total: Option<u64> },
    Message(String),
}

pub struct Engine {
    pub db: StateDb,
    pub game_id: String,
    pub version: String,
}

impl Engine {
    /// Run the plan from wherever it last stopped. The pending manual step
    /// (if any) is returned via [`RunOutcome::ManualWait`] — call
    /// [`Engine::confirm_manual`] after the user acts, then run again.
    pub async fn run(
        &self,
        plan: &[Box<dyn InstallStep>],
        ctx: &GameCtx,
        cancel: &CancellationToken,
        on_event: impl Fn(EngineEvent) + Send + Sync + 'static,
    ) -> anyhow::Result<RunOutcome> {
        let on_event = Arc::new(on_event);
        let total = plan.len();

        for (index, step) in plan.iter().enumerate() {
            if cancel.is_cancelled() {
                return Ok(RunOutcome::Cancelled);
            }
            let step_id = step.id();

            if step.is_done(ctx).await? {
                self.mark(&step_id, "done", None).await?;
                on_event(EngineEvent::StepDone { step_id });
                continue;
            }

            if step.manual().is_some() {
                self.mark(&step_id, "manual_wait", None).await?;
                return Ok(RunOutcome::ManualWait {
                    step_id,
                    label: step.label(),
                });
            }

            tracing::info!(
                target: "install",
                game = %self.game_id,
                step = %step_id,
                "step {}/{}: {}",
                index + 1,
                total,
                step.label(),
            );
            on_event(EngineEvent::StepStarted {
                step_id: step_id.clone(),
                label: step.label(),
                index,
                total,
            });
            self.mark(&step_id, "running", None).await?;

            let events = on_event.clone();
            let progress_step_id = step_id.clone();
            let sink = ProgressSink::new(move |p| {
                let view = match p {
                    Progress::Bytes { done, total } => ProgressView::Bytes { done, total },
                    Progress::Message(m) => ProgressView::Message(m),
                };
                events(EngineEvent::StepProgress {
                    step_id: progress_step_id.clone(),
                    progress: view,
                });
            });

            match step.run(ctx, &sink, cancel).await {
                Ok(()) => {
                    tracing::info!(target: "install", game = %self.game_id, step = %step_id, "step done");
                    self.mark(&step_id, "done", None).await?;
                    on_event(EngineEvent::StepDone { step_id });
                }
                Err(err) if cancel.is_cancelled() => {
                    self.mark(&step_id, "cancelled", Some(err.to_string()))
                        .await?;
                    return Ok(RunOutcome::Cancelled);
                }
                Err(err) => {
                    tracing::error!(
                        target: "install",
                        game = %self.game_id,
                        step = %step_id,
                        error = format!("{err:#}"),
                        "step failed",
                    );
                    self.mark(&step_id, "failed", Some(format!("{err:#}")))
                        .await?;
                    return Err(err.context(format!("step '{}' failed", step.label())));
                }
            }
        }
        Ok(RunOutcome::Complete)
    }

    /// Find the manual step the run is parked on, if any.
    pub fn pending_manual<'p>(
        plan: &'p [Box<dyn InstallStep>],
        step_id: &str,
    ) -> Option<(&'p dyn InstallStep, ManualStep)> {
        plan.iter()
            .find(|s| s.id() == step_id)
            .and_then(|s| s.manual().map(|m| (s.as_ref(), m)))
    }

    /// Confirm a manual step: run its `verify` hook; mark done on success.
    pub async fn confirm_manual(
        &self,
        plan: &[Box<dyn InstallStep>],
        step_id: &str,
        ctx: &GameCtx,
    ) -> anyhow::Result<()> {
        let Some((_, manual)) = Self::pending_manual(plan, step_id) else {
            anyhow::bail!("no manual step with id {step_id}");
        };
        match (manual.verify)(ctx) {
            Ok(()) => self.mark(step_id, "done", None).await,
            Err(err) => {
                self.mark(step_id, "manual_wait", Some(err.to_string()))
                    .await?;
                Err(err.context("verification failed — complete the instructions and retry"))
            }
        }
    }

    async fn mark(&self, step_id: &str, status: &str, error: Option<String>) -> anyhow::Result<()> {
        self.db
            .step_mark(&self.game_id, &self.version, step_id, status, error)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClientConfig;
    use crate::game::GameDirs;
    use crate::services::Services;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn ctx() -> GameCtx {
        let dir = std::env::temp_dir().join("gm-engine-test");
        GameCtx {
            game_id: "test".into(),
            services: Arc::new(Services {
                config: ClientConfig::default(),
                http: reqwest::Client::new(),
                s3: None,
                syncthing: Err("not configured in engine tests".into()),
                library_dir: dir.join("lib"),
                tools_dir: dir.join("tools"),
                downloads_dir: dir.join("dl"),
            }),
            dirs: GameDirs {
                install_root: dir.join("lib/test"),
                prefix: dir.join("prefix/test"),
                downloads: dir.join("dl/test"),
            },
            proton_override: None,
            profile_id: None,
            chosen_exe: None,
            exe_override: None,
            launch: crate::game::LaunchOpts::default(),
            options: crate::game::InstallOptions::default(),
        }
    }

    /// Mock step driven by shared flags.
    struct Mock {
        id: &'static str,
        done: Arc<AtomicBool>,
        runs: Arc<AtomicUsize>,
        fail: bool,
        manual: Option<ManualStep>,
    }

    impl Mock {
        fn new(id: &'static str) -> Self {
            Self {
                id,
                done: Arc::new(AtomicBool::new(false)),
                runs: Arc::new(AtomicUsize::new(0)),
                fail: false,
                manual: None,
            }
        }
    }

    #[async_trait::async_trait]
    impl InstallStep for Mock {
        fn id(&self) -> String {
            self.id.into()
        }
        fn label(&self) -> String {
            format!("mock {}", self.id)
        }
        fn manual(&self) -> Option<ManualStep> {
            self.manual.clone()
        }
        async fn is_done(&self, ctx: &GameCtx) -> anyhow::Result<bool> {
            // mirror GuidedManualStep: a manual step is done iff verify passes
            if let Some(manual) = &self.manual {
                return Ok((manual.verify)(ctx).is_ok());
            }
            Ok(self.done.load(Ordering::SeqCst))
        }
        async fn run(
            &self,
            _ctx: &GameCtx,
            _p: &ProgressSink,
            _c: &CancellationToken,
        ) -> anyhow::Result<()> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                anyhow::bail!("mock failure");
            }
            self.done.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn engine() -> Engine {
        Engine {
            db: StateDb::open_in_memory().unwrap(),
            game_id: "test".into(),
            version: "1.0.0".into(),
        }
    }

    #[tokio::test]
    async fn completed_steps_short_circuit_on_resume() {
        let (a, b) = (Mock::new("a"), Mock::new("b"));
        let (a_runs, b_runs) = (a.runs.clone(), b.runs.clone());
        let plan: Vec<Box<dyn InstallStep>> = vec![Box::new(a), Box::new(b)];
        let engine = engine();
        let cancel = CancellationToken::new();

        let outcome = engine.run(&plan, &ctx(), &cancel, |_| {}).await.unwrap();
        assert!(matches!(outcome, RunOutcome::Complete));
        // resume = rerun: nothing executes twice
        let outcome = engine.run(&plan, &ctx(), &cancel, |_| {}).await.unwrap();
        assert!(matches!(outcome, RunOutcome::Complete));
        assert_eq!(a_runs.load(Ordering::SeqCst), 1);
        assert_eq!(b_runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failure_stops_the_pipeline_and_resume_retries() {
        let a = Mock::new("a");
        let mut b = Mock::new("b");
        b.fail = true;
        let c = Mock::new("c");
        let (b_done, c_runs) = (b.done.clone(), c.runs.clone());
        let plan: Vec<Box<dyn InstallStep>> = vec![Box::new(a), Box::new(b), Box::new(c)];
        let engine = engine();
        let cancel = CancellationToken::new();

        let err = engine
            .run(&plan, &ctx(), &cancel, |_| {})
            .await
            .unwrap_err();
        assert!(err.to_string().contains("mock b"));
        assert_eq!(c_runs.load(Ordering::SeqCst), 0, "later steps must not run");

        // "fix" the failure out-of-band, resume completes the rest
        b_done.store(true, Ordering::SeqCst);
        let outcome = engine.run(&plan, &ctx(), &cancel, |_| {}).await.unwrap();
        assert!(matches!(outcome, RunOutcome::Complete));
        assert_eq!(c_runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn manual_steps_park_and_confirm() {
        let verified = Arc::new(AtomicBool::new(false));
        let v = verified.clone();
        let mut manual = Mock::new("wizard");
        manual.manual = Some(ManualStep {
            instructions_md: "Do the thing".into(),
            pre_check: None,
            verify: Arc::new(move |_| {
                if v.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    anyhow::bail!("thing not done yet")
                }
            }),
        });
        let after = Mock::new("after");
        let after_runs = after.runs.clone();
        let plan: Vec<Box<dyn InstallStep>> = vec![Box::new(manual), Box::new(after)];
        let engine = engine();
        let cancel = CancellationToken::new();

        let outcome = engine.run(&plan, &ctx(), &cancel, |_| {}).await.unwrap();
        let RunOutcome::ManualWait { step_id, .. } = outcome else {
            panic!("expected manual wait");
        };
        assert_eq!(step_id, "wizard");
        assert_eq!(after_runs.load(Ordering::SeqCst), 0);

        // confirming before the user actually did it fails verification
        assert!(
            engine
                .confirm_manual(&plan, &step_id, &ctx())
                .await
                .is_err()
        );

        verified.store(true, Ordering::SeqCst);
        engine
            .confirm_manual(&plan, &step_id, &ctx())
            .await
            .unwrap();
        // is_done of the manual step now passes (verify == done)
        let outcome = engine.run(&plan, &ctx(), &cancel, |_| {}).await.unwrap();
        assert!(matches!(outcome, RunOutcome::Complete));
        assert_eq!(after_runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancellation_short_circuits() {
        let a = Mock::new("a");
        let runs = a.runs.clone();
        let plan: Vec<Box<dyn InstallStep>> = vec![Box::new(a)];
        let engine = engine();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let outcome = engine.run(&plan, &ctx(), &cancel, |_| {}).await.unwrap();
        assert!(matches!(outcome, RunOutcome::Cancelled));
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn events_are_emitted_in_order() {
        let a = Mock::new("a");
        let plan: Vec<Box<dyn InstallStep>> = vec![Box::new(a)];
        let engine = engine();
        let events: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = events.clone();

        engine
            .run(&plan, &ctx(), &CancellationToken::new(), move |e| {
                let tag = match e {
                    EngineEvent::StepStarted { .. } => "started",
                    EngineEvent::StepProgress { .. } => "progress",
                    EngineEvent::StepDone { .. } => "done",
                };
                sink.lock().unwrap().push(tag.into());
            })
            .await
            .unwrap();
        let seen = events.lock().unwrap().clone();
        assert_eq!(seen.first().map(String::as_str), Some("started"));
        assert_eq!(seen.last().map(String::as_str), Some("done"));
    }
}
