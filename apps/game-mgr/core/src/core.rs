//! The GUI ⇄ core boundary (PLAN.md §6.1): commands in via a channel, state
//! out via immutable snapshots; the waker triggers repaints. No GUI types.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use game_mgr_api_types::{
    ArtifactDto, ArtifactRole, GameDefinition, ProfileDto, RegisterMachineRequest, SessionDto,
    SessionEndReason, UserDto,
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::ClientConfig;
use crate::engine::{Engine, EngineEvent, ProgressView, RunOutcome};
use crate::game::{GameClass, GameCtx, InstallOptions, LaunchOpts, Progress, ProgressSink};
use crate::oidc::{TokenProvider, provider_from_config};
use crate::registry::{KNOWN_CLASSES, Registry, registry_from_definitions};
use crate::scan::ScannedFile;
use crate::services::Services;
use crate::statedb::{InstallState, StateDb};
use crate::stats::ServerClient;
use crate::syncthing::SyncthingClient;

const CATALOG_CACHE_KEY: &str = "catalog_json";

/// Repaint hook supplied by the GUI shell.
pub type Waker = Box<dyn Fn() + Send + Sync + 'static>;

/// One artifact row of a draft, as classified in the picker. `sha256 = None`
/// means no sidecar was found — submit streams + hashes that file.
#[derive(Debug, Clone)]
pub struct DraftArtifact {
    pub bucket_key: String,
    pub size: Option<i64>,
    pub sha256: Option<String>,
    pub role: ArtifactRole,
    /// Distinct DLC name (only set when `role == Dlc`).
    pub dlc_name: Option<String>,
}

/// A game being defined or edited from the UI: class config assembled by
/// the shell, artifacts picked + classified from a bucket scan.
#[derive(Debug, Clone)]
pub struct GameDraft {
    pub id: String,
    pub title: String,
    pub version: String,
    pub class: String,
    pub config: serde_json::Value,
    pub artifacts: Vec<DraftArtifact>,
}

/// Result of scanning a bucket prefix (sidecar hashes resolved).
#[derive(Debug, Clone)]
pub struct ScanResult {
    pub prefix: String,
    pub files: Vec<ScannedFile>,
}

#[derive(Debug, Clone)]
pub enum CoreCmd {
    /// Refetch the game catalog from the server.
    RefreshLibrary,
    /// List a bucket prefix + read sidecar hashes for the Add/Edit picker.
    ScanPrefix(String),
    Install {
        game_id: String,
        options: InstallOptions,
    },
    CancelInstall(String),
    ConfirmManual {
        game_id: String,
        step_id: String,
    },
    Uninstall(String),
    Launch(String),
    /// User picked an executable for a game whose definition left it blank.
    /// Persists the choice and launches.
    PickExe {
        game_id: String,
        exe_rel: String,
    },
    /// Dismiss the pending executable-choice dialog without launching.
    CancelExeChoice,
    /// Persist per-game launch settings (MangoHud/Gamescope/favourites).
    SaveLaunchOpts {
        game_id: String,
        opts: LaunchOpts,
    },
    /// Detect launchable executables for the Settings favourites list.
    ScanExes(String),
    /// Set (or clear) the per-game Proton/GE-Proton override.
    SetProtonOverride {
        game_id: String,
        value: Option<String>,
    },
    /// Dismiss the current error banner/toast.
    ClearError,
    SelectProfile(Uuid),
    CreateProfile(String),
    DeleteProfile(Uuid),
    /// Create or update a game definition (PUT — id is the upsert key).
    SubmitGame(GameDraft),
    RefreshSessions,
    /// Run an interactive login (opens the system browser) -- see
    /// `crate::oidc::TokenProvider::interactive_login`.
    Login,
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum GameState {
    NotInstalled,
    Installing {
        step_label: String,
        detail: String,
    },
    ManualWait {
        step_id: String,
        instructions_md: String,
    },
    Installed,
    Outdated,
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone)]
pub struct GameView {
    pub id: String,
    pub title: String,
    pub class: String,
    pub version: String,
    pub state: GameState,
    pub playing: bool,
    /// Full server definition — drives Edit prefill and the install-options
    /// dialog (which optional groups exist).
    pub definition: GameDefinition,
    /// Options chosen for the current install, if any.
    pub installed_options: Option<InstallOptions>,
    /// Per-machine launch settings (MangoHud/Gamescope/favourites).
    pub launch_opts: LaunchOpts,
    /// Per-game Proton/GE-Proton version override, if set.
    pub proton_override: Option<String>,
    /// Aggregate download progress across the whole set, while downloading.
    pub download: Option<DownloadView>,
    /// `<library>/<id>` — extracted game files live under `game/`.
    pub install_root: PathBuf,
    /// `<data>/prefixes/<id>` — the Wine/Proton prefix (shown so the user can
    /// inspect it manually).
    pub prefix: PathBuf,
}

/// Aggregate download progress for the whole set being fetched.
#[derive(Debug, Clone, Copy)]
pub struct DownloadView {
    pub done: u64,
    pub total: u64,
    pub speed_bps: f64,
}

/// Pending "pick the executable" dialog: the game left `exe_rel` blank and no
/// choice is remembered yet, so the user must select from the detected list.
#[derive(Debug, Clone)]
pub struct ExeChoiceView {
    pub game_id: String,
    pub title: String,
    pub candidates: Vec<String>,
    pub install_root: PathBuf,
    pub prefix: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct AccountView {
    pub user: Option<UserDto>,
    pub profiles: Vec<ProfileDto>,
    pub active_profile: Option<Uuid>,
    pub auth_description: String,
    /// Whether `TokenProvider::logged_in` currently holds a usable token --
    /// distinct from `server_reachable` below, which can also be `false`
    /// from a plain network outage while still logged in. Refreshed at
    /// startup and after a `Login` command completes.
    pub logged_in: bool,
    pub server_reachable: bool,
}

/// Immutable view of core state, rebuilt on every change.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub games: Vec<GameView>,
    pub account: AccountView,
    pub machine_id: Uuid,
    pub machine_name: String,
    pub server_url: Option<String>,
    pub library_dir: PathBuf,
    pub syncthing_status: Result<(), String>,
    pub recent_sessions: Vec<SessionDto>,
    /// Latest bucket scan for the Add/Edit Game picker.
    pub scan: Option<Arc<ScanResult>>,
    /// Long-running background work (e.g. "hashing gog/bg3/… 42%").
    pub activity: Option<String>,
    pub last_error: Option<String>,
    /// Pending executable-choice dialog, if a launch needs one.
    pub exe_choice: Option<ExeChoiceView>,
    /// Detected executables for the Settings favourites list: (game_id, exes).
    pub exe_list: Option<(String, Vec<String>)>,
    /// Class slugs the Add Game UI can offer.
    pub known_classes: &'static [&'static str],
}

struct CoreState {
    config: ClientConfig,
    registry: Registry,
    definitions: Vec<GameDefinition>,
    services: Arc<Services>,
    db: StateDb,
    server: Option<Arc<ServerClient>>,
    token: Arc<dyn TokenProvider>,
    machine_id: Uuid,
    machine_name: String,
    account: AccountView,
    game_runtime: HashMap<String, GameRuntime>,
    recent_sessions: Vec<SessionDto>,
    scan: Option<Arc<ScanResult>>,
    activity: Option<String>,
    last_error: Option<String>,
    exe_choice: Option<ExeChoiceView>,
    exe_list: Option<(String, Vec<String>)>,
    uploader_poke: mpsc::UnboundedSender<()>,
}

#[derive(Default)]
struct GameRuntime {
    install_progress: Option<(String, String)>, // (label, detail)
    manual: Option<(String, String)>,           // (step_id, instructions)
    cancel: Option<CancellationToken>,
    playing: bool,
    download: Option<DownloadAccum>,
}

/// Running tally of bytes downloaded across all fetch steps of an install.
#[derive(Default)]
struct DownloadAccum {
    total: u64,
    per_step: HashMap<String, u64>,
    speed_bps: f64,
    last_done: u64,
    last_at: Option<std::time::Instant>,
}

impl DownloadAccum {
    fn done(&self) -> u64 {
        self.per_step.values().sum()
    }

    /// Record a step's latest byte count and refresh the EWMA speed.
    fn update(&mut self, step_id: String, done: u64) {
        self.per_step.insert(step_id, done);
        let now = std::time::Instant::now();
        let total_done = self.done();
        if let Some(last) = self.last_at {
            let dt = now.duration_since(last).as_secs_f64();
            if dt > 0.25 {
                let inst = total_done.saturating_sub(self.last_done) as f64 / dt;
                self.speed_bps = if self.speed_bps == 0.0 {
                    inst
                } else {
                    0.7 * self.speed_bps + 0.3 * inst
                };
                self.last_done = total_done;
                self.last_at = Some(now);
            }
        } else {
            self.last_done = total_done;
            self.last_at = Some(now);
        }
    }

    fn view(&self) -> DownloadView {
        DownloadView {
            done: self.done().min(self.total.max(self.done())),
            total: self.total,
            speed_bps: self.speed_bps,
        }
    }
}

pub struct CoreHandle {
    cmd_tx: mpsc::UnboundedSender<CoreCmd>,
    snapshot_rx: watch::Receiver<Arc<Snapshot>>,
}

impl CoreHandle {
    pub fn start(config: ClientConfig, waker: Waker) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<CoreCmd>();
        let (snapshot_tx, snapshot_rx) = watch::channel(Arc::new(Snapshot {
            games: vec![],
            account: AccountView::default(),
            machine_id: Uuid::nil(),
            machine_name: String::new(),
            server_url: config.server_url.clone(),
            library_dir: crate::paths::library_dir(&config),
            syncthing_status: Err("starting…".into()),
            recent_sessions: vec![],
            scan: None,
            activity: None,
            last_error: None,
            exe_choice: None,
            exe_list: None,
            known_classes: KNOWN_CLASSES,
        }));

        std::thread::Builder::new()
            .name("gm-core".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build tokio runtime");
                rt.block_on(core_main(config, cmd_rx, snapshot_tx, Arc::new(waker)));
            })
            .expect("failed to spawn gm-core thread");

        Self {
            cmd_tx,
            snapshot_rx,
        }
    }

    /// Cheap (Arc clone) — called by the GUI every frame.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot_rx.borrow().clone()
    }

    pub fn send(&self, cmd: CoreCmd) {
        let _ = self.cmd_tx.send(cmd);
    }
}

impl Drop for CoreHandle {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(CoreCmd::Shutdown);
    }
}

async fn core_main(
    config: ClientConfig,
    mut cmd_rx: mpsc::UnboundedReceiver<CoreCmd>,
    snapshot_tx: watch::Sender<Arc<Snapshot>>,
    waker: Arc<Waker>,
) {
    let db = match StateDb::open(&crate::paths::state_db()) {
        Ok(db) => db,
        Err(err) => {
            tracing::error!(%err, "cannot open state db");
            return;
        }
    };
    match db.sessions_recover().await {
        Ok(0) => {}
        Ok(n) => tracing::warn!(count = n, "recovered crashed sessions at last tick"),
        Err(err) => tracing::error!(%err, "session recovery failed"),
    }

    let syncthing = match SyncthingClient::connect(&config.syncthing).await {
        Ok(client) => Ok(Arc::new(client)),
        Err(err) => Err(format!("{err:#}")),
    };

    let token = provider_from_config(&config);
    let server =
        config
            .server_url
            .as_deref()
            .and_then(|url| match ServerClient::new(url, token.clone()) {
                Ok(client) => Some(Arc::new(client)),
                Err(err) => {
                    tracing::warn!(%err, "invalid server_url");
                    None
                }
            });

    let services = Arc::new(Services {
        http: reqwest::Client::new(),
        server: server.clone(),
        syncthing,
        library_dir: crate::paths::library_dir(&config),
        tools_dir: crate::paths::tools_dir(),
        downloads_dir: crate::paths::downloads_dir(),
        config: config.clone(),
    });

    let machine_id = machine_id(&db).await;
    let machine_name = machine_name(&db).await;

    let (poke_tx, poke_rx) = mpsc::unbounded_channel();
    if let Some(server) = &server {
        crate::stats::spawn_uploader(db.clone(), server.clone(), poke_rx);
    }

    let mut state = CoreState {
        registry: Registry::default(),
        definitions: vec![],
        services,
        db,
        server,
        token,
        machine_id,
        machine_name,
        account: AccountView::default(),
        game_runtime: HashMap::new(),
        recent_sessions: vec![],
        scan: None,
        activity: None,
        last_error: None,
        exe_choice: None,
        exe_list: None,
        uploader_poke: poke_tx,
        config,
    };
    state.account.auth_description = state.token.describe();
    state.account.logged_in = state.token.logged_in().await;

    // local progress/completion events from spawned tasks
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<CoreEvent>();

    bootstrap_server_session(&mut state).await;
    load_catalog(&mut state).await;
    publish(&state, &snapshot_tx, &waker).await;

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                if matches!(cmd, CoreCmd::Shutdown) {
                    break;
                }
                handle_cmd(cmd, &mut state, &event_tx).await;
            }
            event = event_rx.recv() => {
                let Some(event) = event else { break };
                handle_event(event, &mut state).await;
            }
        }
        publish(&state, &snapshot_tx, &waker).await;
    }
    tracing::info!("core stopped");
}

enum CoreEvent {
    InstallProgress {
        game_id: String,
        label: String,
        detail: String,
    },
    InstallFinished {
        game_id: String,
        outcome: anyhow::Result<RunOutcome>,
        /// True when this run was an uninstall (different terminal handling).
        uninstall: bool,
    },
    /// Per-fetch-step byte progress, aggregated into a download bar.
    DownloadBytes {
        game_id: String,
        step_id: String,
        done: u64,
    },
    SessionEnded {
        game_id: String,
    },
    Activity(Option<String>),
    ScanFinished(ScanResult),
    CatalogFetched(Vec<GameDefinition>),
    /// A definition was just upserted — reconcile derived state (saves folder).
    GameSubmitted(String),
    TaskFailed(String),
    /// Interactive login completed successfully — re-run the same
    /// registration/identity fetch startup already does, now that
    /// `token.bearer()` can actually succeed.
    LoginFinished,
}

async fn handle_cmd(
    cmd: CoreCmd,
    state: &mut CoreState,
    events: &mpsc::UnboundedSender<CoreEvent>,
) {
    match cmd {
        CoreCmd::Shutdown => {}
        CoreCmd::RefreshLibrary => spawn_catalog_fetch(state, events),
        CoreCmd::ScanPrefix(prefix) => spawn_scan(state, prefix, events),
        CoreCmd::Install { game_id, options } => {
            start_install(state, &game_id, options, events, false).await
        }
        CoreCmd::Uninstall(game_id) => {
            let options = stored_options(state, &game_id).await;
            start_install(state, &game_id, options, events, true).await
        }
        CoreCmd::CancelInstall(game_id) => {
            if let Some(rt) = state.game_runtime.get(&game_id)
                && let Some(cancel) = &rt.cancel
            {
                cancel.cancel();
            }
        }
        CoreCmd::ConfirmManual { game_id, step_id } => {
            confirm_manual(state, &game_id, &step_id, events).await;
        }
        CoreCmd::Launch(game_id) => launch(state, &game_id, events).await,
        CoreCmd::PickExe { game_id, exe_rel } => {
            let _ = state
                .db
                .install_chosen_exe_set(&game_id, Some(exe_rel.clone()))
                .await;
            state.exe_choice = None;
            launch_with(state, &game_id, Some(exe_rel), events).await;
        }
        CoreCmd::CancelExeChoice => state.exe_choice = None,
        CoreCmd::SaveLaunchOpts { game_id, opts } => {
            if let Ok(json) = serde_json::to_string(&opts) {
                let _ = state.db.install_launch_opts_set(&game_id, json).await;
            }
        }
        CoreCmd::ScanExes(game_id) => scan_exes(state, &game_id).await,
        CoreCmd::SetProtonOverride { game_id, value } => {
            let _ = state.db.proton_override_set(&game_id, value).await;
        }
        CoreCmd::ClearError => state.last_error = None,
        CoreCmd::SelectProfile(profile_id) => {
            state.account.active_profile = Some(profile_id);
            let _ = state
                .db
                .kv_set("active_profile", &profile_id.to_string())
                .await;
        }
        CoreCmd::CreateProfile(name) => {
            let Some(server) = &state.server else {
                state.last_error = Some("no server configured".into());
                return;
            };
            match server.create_profile(&name).await {
                Ok(profile) => {
                    state.account.active_profile = Some(profile.id);
                    let _ = state
                        .db
                        .kv_set("active_profile", &profile.id.to_string())
                        .await;
                    state.account.profiles.push(profile);
                }
                Err(err) => state.last_error = Some(format!("create profile: {err:#}")),
            }
        }
        CoreCmd::DeleteProfile(profile_id) => {
            let Some(server) = &state.server else {
                state.last_error = Some("no server configured".into());
                return;
            };
            match server.delete_profile(profile_id).await {
                Ok(()) => {
                    state.account.profiles.retain(|p| p.id != profile_id);
                    if state.account.active_profile == Some(profile_id) {
                        state.account.active_profile = None;
                        let _ = state.db.kv_set("active_profile", "").await;
                    }
                }
                Err(err) => state.last_error = Some(format!("delete profile: {err:#}")),
            }
        }
        CoreCmd::SubmitGame(draft) => spawn_submit_game(state, draft, events),
        CoreCmd::RefreshSessions => {
            if let Some(server) = &state.server {
                match server.recent_sessions(50).await {
                    Ok(sessions) => state.recent_sessions = sessions,
                    Err(err) => state.last_error = Some(format!("sessions: {err:#}")),
                }
            }
        }
        CoreCmd::Login => spawn_login(state, events),
    }
}

async fn handle_event(event: CoreEvent, state: &mut CoreState) {
    match event {
        CoreEvent::InstallProgress {
            game_id,
            label,
            detail,
        } => {
            let runtime = state.game_runtime.entry(game_id).or_default();
            let current = runtime.install_progress.take().unwrap_or_default();
            runtime.install_progress =
                Some((if label.is_empty() { current.0 } else { label }, detail));
        }
        CoreEvent::DownloadBytes {
            game_id,
            step_id,
            done,
        } => {
            let runtime = state.game_runtime.entry(game_id).or_default();
            if let Some(accum) = runtime.download.as_mut() {
                accum.update(step_id, done);
            }
        }
        CoreEvent::InstallFinished {
            game_id,
            outcome,
            uninstall,
        } => {
            let version = state
                .registry
                .get(&game_id)
                .map(|c| c.meta().version.to_string())
                .unwrap_or_default();
            let runtime = state.game_runtime.entry(game_id.clone()).or_default();
            runtime.install_progress = None;
            runtime.cancel = None;
            runtime.download = None;
            // Uninstall has its own terminal handling: a clean run *removes* the
            // install row (must not be re-marked Installed); a failed run leaves
            // a Failed row so the user can retry rather than getting stuck.
            if uninstall {
                runtime.manual = None;
                match outcome {
                    Ok(RunOutcome::Complete) => {
                        let _ = state.db.install_remove(&game_id).await;
                        if let Some(server) = &state.server {
                            server
                                .report_install(
                                    state.machine_id,
                                    &game_id,
                                    &version,
                                    "uninstalled",
                                    None,
                                )
                                .await;
                        }
                    }
                    Ok(RunOutcome::Cancelled) => {
                        let _ = state
                            .db
                            .install_set(
                                &game_id,
                                &version,
                                InstallState::Failed,
                                Some("uninstall cancelled".into()),
                            )
                            .await;
                    }
                    Ok(RunOutcome::ManualWait { .. }) => {
                        // uninstall plans have no manual steps; treat as an error
                        let _ = state
                            .db
                            .install_set(
                                &game_id,
                                &version,
                                InstallState::Failed,
                                Some("uninstall parked unexpectedly".into()),
                            )
                            .await;
                    }
                    Err(err) => {
                        let _ = state
                            .db
                            .install_set(
                                &game_id,
                                &version,
                                InstallState::Failed,
                                Some(format!("uninstall failed: {err:#}")),
                            )
                            .await;
                    }
                }
                return;
            }
            match outcome {
                Ok(RunOutcome::Complete) => {
                    runtime.manual = None;
                    let _ = state
                        .db
                        .install_set(&game_id, &version, InstallState::Installed, None)
                        .await;
                    if let Some(server) = &state.server {
                        server
                            .report_install(state.machine_id, &game_id, &version, "installed", None)
                            .await;
                    }
                }
                Ok(RunOutcome::ManualWait { step_id, label }) => {
                    runtime.manual = Some((step_id, label));
                    let _ = state
                        .db
                        .install_set(&game_id, &version, InstallState::ManualWait, None)
                        .await;
                }
                Ok(RunOutcome::Cancelled) => {
                    let _ = state
                        .db
                        .install_set(
                            &game_id,
                            &version,
                            InstallState::Failed,
                            Some("cancelled".into()),
                        )
                        .await;
                }
                Err(err) => {
                    let _ = state
                        .db
                        .install_set(
                            &game_id,
                            &version,
                            InstallState::Failed,
                            Some(format!("{err:#}")),
                        )
                        .await;
                }
            }
        }
        CoreEvent::SessionEnded { game_id } => {
            if let Some(rt) = state.game_runtime.get_mut(&game_id) {
                rt.playing = false;
            }
            let _ = state.uploader_poke.send(());
        }
        CoreEvent::Activity(activity) => state.activity = activity,
        CoreEvent::ScanFinished(result) => {
            state.activity = None;
            state.scan = Some(Arc::new(result));
        }
        CoreEvent::CatalogFetched(defs) => {
            if let Ok(json) = serde_json::to_string(&defs) {
                let _ = state.db.kv_set(CATALOG_CACHE_KEY, &json).await;
            }
            state.registry = registry_from_definitions(&defs);
            state.definitions = defs;
        }
        CoreEvent::GameSubmitted(game_id) => reconcile_saves(state, &game_id).await,
        CoreEvent::TaskFailed(message) => {
            state.activity = None;
            state.last_error = Some(message);
        }
        CoreEvent::LoginFinished => {
            state.activity = None;
            state.account.auth_description = state.token.describe();
            state.account.logged_in = state.token.logged_in().await;
            // Same two calls startup makes right after constructing
            // `token` -- `bearer()` can actually succeed now, so redo the
            // machine-registration + identity fetch it depends on rather
            // than waiting for the next unrelated action to stumble into
            // a fresh attempt.
            bootstrap_server_session(state).await;
            load_catalog(state).await;
        }
    }
}

/// Run an interactive login in the background: opens the system browser
/// and blocks (off the UI/core-loop thread) until the loopback redirect
/// lands or it times out -- see `oidc::OidcPkce::login`.
fn spawn_login(state: &CoreState, events: &mpsc::UnboundedSender<CoreEvent>) {
    let token = state.token.clone();
    let events = events.clone();
    tokio::spawn(async move {
        let _ = events.send(CoreEvent::Activity(Some(
            "waiting for browser sign-in…".into(),
        )));
        match token.interactive_login().await {
            Ok(()) => {
                let _ = events.send(CoreEvent::LoginFinished);
            }
            Err(err) => {
                let _ = events.send(CoreEvent::TaskFailed(format!("sign-in: {err:#}")));
            }
        }
    });
}

/// Fetch the catalog in the background and rebuild the registry.
fn spawn_catalog_fetch(state: &CoreState, events: &mpsc::UnboundedSender<CoreEvent>) {
    let Some(server) = state.server.clone() else {
        return;
    };
    let events = events.clone();
    tokio::spawn(async move {
        match server.games().await {
            Ok(games) => {
                let defs: Vec<GameDefinition> = games.into_iter().map(|g| g.definition).collect();
                let _ = events.send(CoreEvent::CatalogFetched(defs));
            }
            Err(err) => {
                let _ = events.send(CoreEvent::TaskFailed(format!("catalog refresh: {err:#}")));
            }
        }
    });
}

/// List + read sidecars for the Add/Edit Game picker — instant, no
/// streaming of game data. Goes through the backend (`ServerClient::scan`),
/// never the bucket directly (PLAN.md §4.3).
fn spawn_scan(state: &mut CoreState, prefix: String, events: &mpsc::UnboundedSender<CoreEvent>) {
    let Some(server) = state.server.clone() else {
        state.last_error = Some("no server configured".into());
        return;
    };
    let events = events.clone();
    tokio::spawn(async move {
        let _ = events.send(CoreEvent::Activity(Some(format!("scanning {prefix}…"))));
        match crate::scan::scan_prefix(&server, &prefix).await {
            Ok(files) => {
                let _ = events.send(CoreEvent::ScanFinished(ScanResult { prefix, files }));
            }
            Err(err) => {
                let _ = events.send(CoreEvent::TaskFailed(format!("scan {prefix}: {err:#}")));
            }
        }
    });
}

/// Submit a definition: stream-hash only artifacts without sidecars, then
/// PUT and refresh the catalog.
fn spawn_submit_game(
    state: &mut CoreState,
    draft: GameDraft,
    events: &mpsc::UnboundedSender<CoreEvent>,
) {
    let Some(server) = state.server.clone() else {
        state.last_error = Some("no server configured".into());
        return;
    };
    let http = state.services.http.clone();
    let events = events.clone();

    tokio::spawn(async move {
        let result: anyhow::Result<()> = async {
            let mut artifacts = Vec::with_capacity(draft.artifacts.len());
            for artifact in &draft.artifacts {
                let (sha256, size) = match &artifact.sha256 {
                    Some(sha) => (sha.clone(), artifact.size),
                    None => {
                        // no sidecar in the bucket — fall back to streaming
                        // a presigned download and hashing it as it goes
                        let events_for_progress = events.clone();
                        let key = artifact.bucket_key.clone();
                        let progress = ProgressSink::new(move |p| {
                            if let Progress::Bytes { done, total } = p {
                                let detail = match total {
                                    Some(total) if total > 0 => {
                                        format!("{:.0}%", done as f64 / total as f64 * 100.0)
                                    }
                                    _ => format!("{} MiB", done / (1024 * 1024)),
                                };
                                let _ = events_for_progress.send(CoreEvent::Activity(Some(
                                    format!("hashing {key} {detail} (no sidecar found)"),
                                )));
                            }
                        });
                        let (sha, size) = crate::s3::stream_and_hash(
                            &http,
                            &server,
                            &artifact.bucket_key,
                            &progress,
                            &CancellationToken::new(),
                        )
                        .await?;
                        (sha, Some(size as i64))
                    }
                };
                artifacts.push(ArtifactDto {
                    bucket_key: artifact.bucket_key.clone(),
                    sha256,
                    size,
                    role: artifact.role,
                    dlc_name: artifact.dlc_name.clone(),
                });
            }

            let _ = events.send(CoreEvent::Activity(Some(format!(
                "submitting {}…",
                draft.id
            ))));
            server
                .upsert_game(
                    &draft.id,
                    &game_mgr_api_types::UpsertGameRequest {
                        title: draft.title.clone(),
                        class: draft.class.clone(),
                        version: draft.version.clone(),
                        config: draft.config.clone(),
                        artifacts,
                    },
                )
                .await?;

            let games = server.games().await?;
            let defs: Vec<GameDefinition> = games.into_iter().map(|g| g.definition).collect();
            let _ = events.send(CoreEvent::CatalogFetched(defs));
            // an edited definition may have moved the saves path — reconcile
            // the Syncthing folder for installed games (handled after the
            // catalog rebuild so the new config is in the registry).
            let _ = events.send(CoreEvent::GameSubmitted(draft.id.clone()));
            let _ = events.send(CoreEvent::Activity(None));
            Ok(())
        }
        .await;

        if let Err(err) = result {
            let _ = events.send(CoreEvent::TaskFailed(format!("submit game: {err:#}")));
        }
    });
}

async fn stored_options(state: &CoreState, game_id: &str) -> InstallOptions {
    state
        .db
        .install_get(game_id)
        .await
        .ok()
        .flatten()
        .and_then(|row| row.options)
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// Total bytes that will be fetched for an install (base + selected
/// patches/DLC), the denominator for the aggregate download bar.
fn planned_download_bytes(class: &Arc<dyn GameClass>, options: &InstallOptions) -> u64 {
    use game_mgr_api_types::ArtifactRole;
    class
        .artifacts()
        .iter()
        .filter(|a| match a.role {
            ArtifactRole::Base => true,
            ArtifactRole::Patch => options.include_patches,
            ArtifactRole::Dlc => options.dlc.includes(a.dlc_name.as_deref()),
        })
        .filter_map(|a| a.approx_size)
        .sum()
}

async fn start_install(
    state: &mut CoreState,
    game_id: &str,
    options: InstallOptions,
    events: &mpsc::UnboundedSender<CoreEvent>,
    uninstall: bool,
) {
    let Some(class) = state.registry.get(game_id).cloned() else {
        state.last_error = Some(format!("unknown game {game_id}"));
        return;
    };
    let ctx = game_ctx(state, &class, options.clone()).await;
    let plan = if uninstall {
        class.uninstall_plan(&ctx)
    } else {
        class.install_plan(&ctx)
    };
    let plan = match plan {
        Ok(plan) => plan,
        Err(err) => {
            state.last_error = Some(format!("{err:#}"));
            return;
        }
    };

    let version = class.meta().version.to_string();
    let cancel = CancellationToken::new();
    let download_total = if uninstall {
        0
    } else {
        planned_download_bytes(&class, &ctx.options)
    };
    let runtime = state.game_runtime.entry(game_id.to_string()).or_default();
    runtime.cancel = Some(cancel.clone());
    runtime.install_progress = Some(("Starting…".into(), String::new()));
    runtime.download = (!uninstall).then(|| DownloadAccum {
        total: download_total,
        ..Default::default()
    });
    let _ = state
        .db
        .install_set(game_id, &version, InstallState::Installing, None)
        .await;
    if !uninstall && let Ok(json) = serde_json::to_string(&options) {
        let _ = state.db.install_options_set(game_id, json).await;
    }

    let engine = Engine {
        db: state.db.clone(),
        game_id: game_id.to_string(),
        version,
    };
    let events_for_progress = events.clone();
    let events_for_outcome = events.clone();
    let game_for_progress = game_id.to_string();
    let game_for_outcome = game_id.to_string();
    let is_uninstall = uninstall;

    tokio::spawn(async move {
        let progress_game = game_for_progress.clone();
        let outcome = engine
            .run(&plan, &ctx, &cancel, move |event| {
                let (label, detail) = match event {
                    EngineEvent::StepStarted {
                        label,
                        index,
                        total,
                        ..
                    } => (label, format!("step {}/{}", index + 1, total)),
                    EngineEvent::StepProgress { step_id, progress } => match progress {
                        ProgressView::Bytes { done, total } => {
                            // feed the aggregate download bar (fetch steps only)
                            if step_id.starts_with("fetch:") {
                                let _ = events_for_progress.send(CoreEvent::DownloadBytes {
                                    game_id: progress_game.clone(),
                                    step_id: step_id.clone(),
                                    done,
                                });
                            }
                            (
                                String::new(),
                                match total {
                                    Some(total) if total > 0 => format!(
                                        "{:.0}% ({} / {} MiB)",
                                        done as f64 / total as f64 * 100.0,
                                        done / (1024 * 1024),
                                        total / (1024 * 1024)
                                    ),
                                    _ => format!("{} MiB", done / (1024 * 1024)),
                                },
                            )
                        }
                        ProgressView::Message(m) => (String::new(), m),
                    },
                    EngineEvent::StepDone { .. } => return,
                };
                let _ = events_for_progress.send(CoreEvent::InstallProgress {
                    game_id: progress_game.clone(),
                    label,
                    detail,
                });
            })
            .await;
        let _ = events_for_outcome.send(CoreEvent::InstallFinished {
            game_id: game_for_outcome,
            outcome,
            uninstall: is_uninstall,
        });
    });
}

async fn confirm_manual(
    state: &mut CoreState,
    game_id: &str,
    step_id: &str,
    events: &mpsc::UnboundedSender<CoreEvent>,
) {
    let Some(class) = state.registry.get(game_id).cloned() else {
        return;
    };
    let options = stored_options(state, game_id).await;
    let ctx = game_ctx(state, &class, options.clone()).await;
    let plan = match class.install_plan(&ctx) {
        Ok(plan) => plan,
        Err(err) => {
            state.last_error = Some(format!("{err:#}"));
            return;
        }
    };
    let engine = Engine {
        db: state.db.clone(),
        game_id: game_id.to_string(),
        version: class.meta().version.to_string(),
    };
    match engine.confirm_manual(&plan, step_id, &ctx).await {
        Ok(()) => {
            if let Some(rt) = state.game_runtime.get_mut(game_id) {
                rt.manual = None;
            }
            // resume the rest of the plan
            start_install(state, game_id, options, events, false).await;
        }
        Err(err) => state.last_error = Some(format!("{err:#}")),
    }
}

async fn launch(state: &mut CoreState, game_id: &str, events: &mpsc::UnboundedSender<CoreEvent>) {
    launch_with(state, game_id, None, events).await
}

/// Launch a game. `explicit` is a just-picked executable (from the chooser),
/// which bypasses the favourite/prompt logic for this launch.
async fn launch_with(
    state: &mut CoreState,
    game_id: &str,
    explicit: Option<String>,
    events: &mpsc::UnboundedSender<CoreEvent>,
) {
    let Some(class) = state.registry.get(game_id).cloned() else {
        return;
    };
    let Some(profile_id) = state.account.active_profile else {
        state.last_error =
            Some("select or create a profile before playing (stats need one)".into());
        return;
    };
    let installed = matches!(
        state.db.install_get(game_id).await,
        Ok(Some(row)) if row.state == InstallState::Installed
    );
    if !installed {
        state.last_error = Some(format!("{game_id} is not installed"));
        return;
    }

    let options = stored_options(state, game_id).await;
    let mut ctx = game_ctx(state, &class, options).await;

    // Favourites drive which executable runs: an explicit pick wins; exactly
    // one favourite launches directly; more than one asks every time.
    let favorites = ctx.launch.exe_favorites.clone();
    if let Some(rel) = explicit {
        ctx.exe_override = Some(rel);
    } else if favorites.len() == 1 {
        ctx.exe_override = Some(favorites[0].clone());
    } else if favorites.len() > 1 {
        state.exe_choice = Some(ExeChoiceView {
            game_id: game_id.to_string(),
            title: class.meta().title.clone(),
            candidates: favorites,
            install_root: ctx.dirs.install_root.clone(),
            prefix: ctx.dirs.prefix.clone(),
        });
        return;
    }

    // No executable resolved (blank definition, none remembered) — ask the
    // user to pick from the detected list instead of failing.
    if let Some(candidates) = class.exe_candidates(&ctx) {
        tracing::info!(
            game = %game_id,
            count = candidates.len(),
            "executable not set — asking the user to choose"
        );
        state.exe_choice = Some(ExeChoiceView {
            game_id: game_id.to_string(),
            title: class.meta().title.clone(),
            candidates,
            install_root: ctx.dirs.install_root.clone(),
            prefix: ctx.dirs.prefix.clone(),
        });
        return;
    }

    let launched = match class.launch(&ctx).await {
        Ok(launched) => launched,
        Err(err) => {
            state.last_error = Some(format!("launch failed: {err:#}"));
            return;
        }
    };

    let session_id = Uuid::new_v4();
    let started_at = time::OffsetDateTime::now_utc();
    let _ = state
        .db
        .session_start(
            session_id,
            game_id,
            profile_id,
            state.machine_id,
            started_at,
        )
        .await;
    let _ = state
        .db
        .event_record(
            state.machine_id,
            Some(profile_id),
            Some(game_id.to_string()),
            "launch",
            serde_json::json!({}),
        )
        .await;
    let _ = state.uploader_poke.send(());
    state
        .game_runtime
        .entry(game_id.to_string())
        .or_default()
        .playing = true;

    let spec = class.watcher(&ctx);
    let db = state.db.clone();
    let events = events.clone();
    let game_id_owned = game_id.to_string();
    tokio::spawn(async move {
        let tick_db = db.clone();
        let outcome = crate::watcher::watch_session(launched.child, spec, move |at| {
            let tick_db = tick_db.clone();
            tokio::spawn(async move {
                let _ = tick_db.session_tick(session_id, at).await;
            });
        })
        .await;
        let _ = db
            .session_finish(
                session_id,
                outcome.ended_at,
                outcome.exit_code,
                outcome.end_reason,
            )
            .await;
        if outcome.end_reason == SessionEndReason::Exited && outcome.exit_code.unwrap_or(0) != 0 {
            tracing::warn!(
                game = %game_id_owned,
                code = outcome.exit_code,
                "game exited with a non-zero status"
            );
        }
        let _ = events.send(CoreEvent::SessionEnded {
            game_id: game_id_owned,
        });
    });
}

/// Detect launchable executables for the Settings favourites list and stash
/// them in the snapshot for the UI to render.
async fn scan_exes(state: &mut CoreState, game_id: &str) {
    let Some(class) = state.registry.get(game_id).cloned() else {
        return;
    };
    let options = stored_options(state, game_id).await;
    let ctx = game_ctx(state, &class, options).await;
    let exes = class.list_exes(&ctx);
    state.exe_list = Some((game_id.to_string(), exes));
}

/// Re-assert a game's Syncthing folders against its current definition. Run
/// after an edit so changing the saves path actually moves the synced folder
/// (only for installed games — otherwise the install will set it up).
async fn reconcile_saves(state: &mut CoreState, game_id: &str) {
    let Some(class) = state.registry.get(game_id).cloned() else {
        return;
    };
    let installed = matches!(
        state.db.install_get(game_id).await,
        Ok(Some(row)) if row.state == InstallState::Installed
    );
    if !installed {
        return;
    }
    let options = stored_options(state, game_id).await;
    let ctx = game_ctx(state, &class, options).await;
    let folders = class.sync_folders(&ctx);
    let syncthing = match &state.services.syncthing {
        Ok(client) => client.clone(),
        Err(reason) => {
            state.last_error = Some(format!("can't update saves folder — Syncthing: {reason}"));
            return;
        }
    };
    for spec in &folders {
        match syncthing.ensure_folder(spec).await {
            Ok(()) => tracing::info!(
                game = %game_id,
                folder = %spec.folder_id,
                path = %spec.local_path.display(),
                "saves folder reconciled",
            ),
            Err(err) => {
                tracing::error!(game = %game_id, %err, "reconciling saves folder failed");
                state.last_error = Some(format!("update saves folder: {err:#}"));
            }
        }
    }
}

async fn game_ctx(
    state: &CoreState,
    class: &Arc<dyn GameClass>,
    options: InstallOptions,
) -> GameCtx {
    let id = class.meta().id.clone();
    let row = state.db.install_get(&id).await.ok().flatten();
    let proton_override = row.as_ref().and_then(|r| r.proton_override.clone());
    let chosen_exe = row.as_ref().and_then(|r| r.chosen_exe.clone());
    let launch = row
        .as_ref()
        .and_then(|r| r.launch_opts.as_ref())
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();
    GameCtx {
        game_id: id.clone(),
        services: state.services.clone(),
        dirs: state.services.dirs_for(&id),
        proton_override,
        profile_id: state.account.active_profile,
        chosen_exe,
        exe_override: None,
        launch,
        options,
    }
}

/// Register the machine and fetch identity + profiles; surface version skew.
async fn bootstrap_server_session(state: &mut CoreState) {
    let Some(server) = state.server.clone() else {
        return;
    };

    let syncthing_device_id = match &state.services.syncthing {
        Ok(client) => client.status().await.ok().map(|s| s.my_id),
        Err(_) => None,
    };
    let register = server
        .register_machine(
            state.machine_id,
            &RegisterMachineRequest {
                name: state.machine_name.clone(),
                os: Some(std::env::consts::OS.to_string()),
                client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                syncthing_device_id,
            },
        )
        .await;
    if let Err(err) = register {
        tracing::warn!(%err, "machine registration failed (offline?)");
        state.account.server_reachable = false;
        return;
    }
    state.account.server_reachable = true;

    // a stale server image makes newer endpoints 404 with no obvious cause —
    // compare versions and say so explicitly
    match server.server_version().await {
        Ok(Some(version)) if version != env!("CARGO_PKG_VERSION") => {
            state.last_error = Some(format!(
                "server v{version} ≠ client v{} — rebuild/redeploy the server image \
                 (docker compose up --build)",
                env!("CARGO_PKG_VERSION")
            ));
        }
        Ok(Some(_)) => {}
        Ok(None) | Err(_) => {
            state.last_error = Some(
                "server does not report a version — it predates this client; \
                 rebuild the server image (docker compose up --build)"
                    .into(),
            );
        }
    }

    match server.me().await {
        Ok(me) => {
            state.account.user = Some(me.user);
            state.account.profiles = me.profiles;
            // restore the active profile if it is still ours
            if let Ok(Some(stored)) = state.db.kv_get("active_profile").await
                && let Ok(stored) = stored.parse::<Uuid>()
                && state.account.profiles.iter().any(|p| p.id == stored)
            {
                state.account.active_profile = Some(stored);
            }
            if state.account.active_profile.is_none() && state.account.profiles.len() == 1 {
                state.account.active_profile = Some(state.account.profiles[0].id);
            }
        }
        Err(err) => tracing::warn!(%err, "fetching /me failed"),
    }
}

/// Catalog: server when reachable, cached copy otherwise (offline library).
async fn load_catalog(state: &mut CoreState) {
    if let Some(server) = &state.server
        && state.account.server_reachable
    {
        match server.games().await {
            Ok(games) => {
                let defs: Vec<GameDefinition> = games.into_iter().map(|g| g.definition).collect();
                if let Ok(json) = serde_json::to_string(&defs) {
                    let _ = state.db.kv_set(CATALOG_CACHE_KEY, &json).await;
                }
                state.registry = registry_from_definitions(&defs);
                state.definitions = defs;
                return;
            }
            Err(err) => tracing::warn!(%err, "catalog fetch failed; trying cache"),
        }
    }
    if let Ok(Some(json)) = state.db.kv_get(CATALOG_CACHE_KEY).await
        && let Ok(defs) = serde_json::from_str::<Vec<GameDefinition>>(&json)
    {
        tracing::info!(count = defs.len(), "using cached game catalog (offline)");
        state.registry = registry_from_definitions(&defs);
        state.definitions = defs;
    }
}

async fn machine_id(db: &StateDb) -> Uuid {
    if let Ok(Some(stored)) = db.kv_get("machine_id").await
        && let Ok(id) = stored.parse()
    {
        return id;
    }
    let id = Uuid::new_v4();
    let _ = db.kv_set("machine_id", &id.to_string()).await;
    id
}

async fn machine_name(db: &StateDb) -> String {
    if let Ok(Some(stored)) = db.kv_get("machine_name").await {
        return stored;
    }
    let name = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown-machine".into());
    let _ = db.kv_set("machine_name", &name).await;
    name
}

async fn publish(state: &CoreState, tx: &watch::Sender<Arc<Snapshot>>, waker: &Arc<Waker>) {
    let mut games = Vec::new();
    for class in state.registry.iter() {
        let meta = class.meta();
        let id = meta.id.clone();
        let runtime = state.game_runtime.get(&id);
        let install = state.db.install_get(&id).await.ok().flatten();
        let installed_options = install
            .as_ref()
            .and_then(|row| row.options.as_ref())
            .and_then(|json| serde_json::from_str(json).ok());
        let launch_opts = install
            .as_ref()
            .and_then(|row| row.launch_opts.as_ref())
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();
        let proton_override = install.as_ref().and_then(|row| row.proton_override.clone());

        let game_state =
            if let Some((label, detail)) = runtime.and_then(|r| r.install_progress.clone()) {
                GameState::Installing {
                    step_label: label,
                    detail,
                }
            } else if let Some((step_id, instructions)) = runtime.and_then(|r| r.manual.clone()) {
                GameState::ManualWait {
                    step_id,
                    instructions_md: instructions,
                }
            } else {
                match install {
                    None => GameState::NotInstalled,
                    Some(row) => match row.state {
                        InstallState::Installed => {
                            if semver::Version::parse(&row.version)
                                .map(|v| v < meta.version)
                                .unwrap_or(false)
                            {
                                GameState::Outdated
                            } else {
                                GameState::Installed
                            }
                        }
                        InstallState::Installing => GameState::Installing {
                            step_label: "…".into(),
                            detail: String::new(),
                        },
                        InstallState::ManualWait => GameState::ManualWait {
                            step_id: String::new(),
                            instructions_md: "Re-open the wizard from Install".into(),
                        },
                        InstallState::Outdated => GameState::Outdated,
                        InstallState::Failed => GameState::Failed {
                            error: row.error.unwrap_or_else(|| "failed".into()),
                        },
                    },
                }
            };

        let definition = state
            .definitions
            .iter()
            .find(|d| d.id == id)
            .cloned()
            .unwrap_or(GameDefinition {
                id: id.clone(),
                title: meta.title.clone(),
                class: meta.class.clone(),
                version: meta.version.to_string(),
                config: serde_json::Value::Null,
                artifacts: vec![],
            });

        let dirs = state.services.dirs_for(&id);
        games.push(GameView {
            id,
            title: meta.title.clone(),
            class: meta.class.clone(),
            version: meta.version.to_string(),
            state: game_state,
            playing: runtime.map(|r| r.playing).unwrap_or(false),
            definition,
            installed_options,
            launch_opts,
            proton_override,
            download: runtime.and_then(|r| r.download.as_ref()).map(|d| d.view()),
            install_root: dirs.install_root,
            prefix: dirs.prefix,
        });
    }

    let snapshot = Snapshot {
        games,
        account: state.account.clone(),
        machine_id: state.machine_id,
        machine_name: state.machine_name.clone(),
        server_url: state.config.server_url.clone(),
        library_dir: state.services.library_dir.clone(),
        syncthing_status: state
            .services
            .syncthing
            .as_ref()
            .map(|_| ())
            .map_err(|e| e.clone()),
        recent_sessions: state.recent_sessions.clone(),
        scan: state.scan.clone(),
        activity: state.activity.clone(),
        last_error: state.last_error.clone(),
        exe_choice: state.exe_choice.clone(),
        exe_list: state.exe_list.clone(),
        known_classes: KNOWN_CLASSES,
    };
    let _ = tx.send(Arc::new(snapshot));
    (waker.as_ref())();
}
