//! The game model: classes (gog, switch, …) are code; titles are
//! server-stored definitions instantiated at runtime (PLAN.md §4).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::services::Services;

/// Title metadata, sourced from the server-side [`game_mgr_api_types::GameDefinition`].
/// The id is a stable slug (`bg3`, `skyrim`, …) used in Syncthing folder IDs,
/// bucket prefixes, local state and stats — never renamed once shipped.
#[derive(Debug, Clone)]
pub struct GameMeta {
    pub id: String,
    pub title: String,
    /// Class slug: `"gog"`, `"switch"`, `"skyrim-modded"`, ...
    pub class: String,
    /// Bumping this marks existing installs `outdated` (versioned reinstall,
    /// PLAN.md §4.2).
    pub version: semver::Version,
}

/// Something to fetch from the bucket; the sha256 comes from the `.sha256`
/// sidecar read at definition time (streamed hash as fallback).
#[derive(Debug, Clone)]
pub struct ArtifactRef {
    pub bucket_key: String,
    /// Lowercase hex sha256 of the object.
    pub sha256: String,
    pub approx_size: Option<u64>,
    pub role: game_mgr_api_types::ArtifactRole,
    /// Distinct DLC name (only meaningful when `role == Dlc`).
    pub dlc_name: Option<String>,
}

impl From<&game_mgr_api_types::ArtifactDto> for ArtifactRef {
    fn from(a: &game_mgr_api_types::ArtifactDto) -> Self {
        ArtifactRef {
            bucket_key: a.bucket_key.clone(),
            sha256: a.sha256.clone(),
            approx_size: a.size.and_then(|s| u64::try_from(s).ok()),
            role: a.role,
            dlc_name: a.dlc_name.clone(),
        }
    }
}

/// Per-game, per-machine launch settings (not part of the shared server
/// definition). Edited in the per-game Settings window, persisted in statedb.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LaunchOpts {
    /// Wrap the launch in `mangohud -- …`.
    #[serde(default)]
    pub mangohud: bool,
    /// Wrap the launch in `gamescope <args> -- …`.
    #[serde(default)]
    pub gamescope: bool,
    /// Raw gamescope arguments (whitespace-split), e.g. `-W 2560 -H 1440 -f`.
    #[serde(default)]
    pub gamescope_args: String,
    /// Steam-style custom launch options, e.g. `MANGOHUD_CONFIG=fps_limit=60
    /// gamemoderun %command% -novid`. Shell-quoted (so quoted args with
    /// spaces work); leading `KEY=VALUE` tokens become env vars; the rest
    /// before `%command%` prefixes the (possibly MangoHud/Gamescope-wrapped)
    /// launch command and the rest after it is appended as extra arguments.
    /// Without a `%command%` token the whole string is treated as a prefix,
    /// same as the dedicated Gamescope field. Wraps outermost, around
    /// MangoHud/Gamescope.
    #[serde(default)]
    pub custom_args: String,
    /// Executables (relative to the extracted tree) the user starred. With one
    /// favourite it launches directly; with more than one the launcher asks
    /// which to run every time.
    #[serde(default)]
    pub exe_favorites: Vec<String>,
}

/// Which DLCs an install includes. `All` is the legacy "install every DLC"
/// behavior; `Named` selects DLCs by their picker-assigned name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DlcSelection {
    All,
    Named(std::collections::BTreeSet<String>),
}

impl Default for DlcSelection {
    fn default() -> Self {
        DlcSelection::Named(std::collections::BTreeSet::new())
    }
}

impl DlcSelection {
    /// True when nothing is selected (no DLC installs).
    pub fn is_empty(&self) -> bool {
        matches!(self, DlcSelection::Named(set) if set.is_empty())
    }

    /// Should an artifact with this DLC name be installed?
    pub fn includes(&self, name: Option<&str>) -> bool {
        match self {
            DlcSelection::All => true,
            // `None`-named DLC artifacts use the empty-string bucket so the
            // dialog (which lists them under a derived label) can select them.
            DlcSelection::Named(set) => set.contains(name.unwrap_or("")),
        }
    }
}

/// Which optional artifact groups an install includes. The base group is
/// always installed; patches/DLC are chosen at install time and remembered
/// per install for updates/reinstalls.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(from = "InstallOptionsRepr", into = "InstallOptionsRepr")]
pub struct InstallOptions {
    pub include_patches: bool,
    pub dlc: DlcSelection,
}

/// On-disk/wire shape for [`InstallOptions`], kept back-compatible with the
/// old `{"include_patches":bool,"include_dlc":bool}` JSON stored in statedb.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct InstallOptionsRepr {
    #[serde(default)]
    include_patches: bool,
    /// Legacy "install all DLC" flag — honored only when `dlc_names` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    include_dlc: Option<bool>,
    /// Named DLC selection (new shape).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dlc_names: Option<Vec<String>>,
}

impl From<InstallOptionsRepr> for InstallOptions {
    fn from(r: InstallOptionsRepr) -> Self {
        let dlc = match r.dlc_names {
            Some(names) => DlcSelection::Named(names.into_iter().collect()),
            None if r.include_dlc == Some(true) => DlcSelection::All,
            None => DlcSelection::default(),
        };
        InstallOptions {
            include_patches: r.include_patches,
            dlc,
        }
    }
}

impl From<InstallOptions> for InstallOptionsRepr {
    fn from(o: InstallOptions) -> Self {
        let (include_dlc, dlc_names) = match o.dlc {
            DlcSelection::All => (Some(true), None),
            DlcSelection::Named(set) if set.is_empty() => (None, None),
            DlcSelection::Named(set) => (None, Some(set.into_iter().collect())),
        };
        InstallOptionsRepr {
            include_patches: o.include_patches,
            include_dlc,
            dlc_names,
        }
    }
}

/// An external tool pinned in code, downloaded from upstream (PLAN.md §4.4).
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub version: &'static str,
    /// Linux download URL (archive). Windows URL joins for the port.
    pub linux_url: String,
    pub sha256: String,
    /// Path of the main executable inside the unpacked archive.
    pub exe_rel_path: String,
}

/// A Syncthing folder owned by a game. `folder_id` is identical on every
/// device; `local_path` is this machine's resolution (PLAN.md §5).
#[derive(Debug, Clone)]
pub struct SyncFolderSpec {
    pub folder_id: String,
    pub label: String,
    pub local_path: PathBuf,
    /// Lines for the folder's ignore patterns (nested-folder discipline).
    pub stignore: Vec<String>,
    /// Wait for initial sync before the first launch (timeout + override).
    pub required_before_first_launch: bool,
}

#[derive(Debug, Clone)]
pub enum WatchHint {
    /// Match process executable file names (case-insensitive).
    ExeNames(Vec<String>),
    /// Match a substring of the process command line.
    CmdlineContains(String),
}

/// Restrict hint matches to processes whose exe/cwd lives under a path —
/// prevents cross-game matches (PLAN.md §6.3).
#[derive(Debug, Clone, Default)]
pub struct WatchScope {
    pub under_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct WatcherSpec {
    pub hint: Option<WatchHint>,
    pub scope: WatchScope,
    pub poll: Duration,
    pub grace: Duration,
}

impl Default for WatcherSpec {
    fn default() -> Self {
        Self {
            hint: None,
            scope: WatchScope::default(),
            poll: Duration::from_secs(2),
            grace: Duration::from_secs(10),
        }
    }
}

/// Per-game filesystem locations, derived from config (PLAN.md §6.6).
#[derive(Debug, Clone)]
pub struct GameDirs {
    /// `<library>/<game-id>` — game files live underneath.
    pub install_root: PathBuf,
    /// `<data>/prefixes/<game-id>` — Wine/Proton prefix (when applicable).
    pub prefix: PathBuf,
    /// `<cache>/downloads/<game-id>` — fetched artifacts.
    pub downloads: PathBuf,
}

/// Everything a class needs to plan, install and launch on this machine.
#[derive(Clone)]
pub struct GameCtx {
    /// Stable game id — used to tag subprocess log lines (PLAN.md §6.6).
    pub game_id: String,
    pub services: Arc<Services>,
    pub dirs: GameDirs,
    /// User-selected Proton/tool version override for this game, if any.
    pub proton_override: Option<String>,
    /// Active profile — its stable UUID scopes the saves Syncthing folder so
    /// each profile gets its own save set that follows the profile on transfer.
    pub profile_id: Option<uuid::Uuid>,
    /// Executable chosen at first launch when the definition leaves it blank
    /// (relative to the extracted tree), persisted per install.
    pub chosen_exe: Option<String>,
    /// Transient highest-priority executable for *this* launch (a favourite or
    /// a just-picked exe). Wins over `exe_rel`/`chosen_exe`.
    pub exe_override: Option<String>,
    /// Per-machine launch wrappers/favourites (MangoHud, Gamescope, …).
    pub launch: LaunchOpts,
    /// Optional-group selection for installs (PLAN.md §4.2).
    pub options: InstallOptions,
}

/// Byte/step progress reported by running steps.
#[derive(Debug, Clone)]
pub enum Progress {
    Bytes { done: u64, total: Option<u64> },
    Message(String),
}

#[derive(Clone)]
pub struct ProgressSink(Arc<dyn Fn(Progress) + Send + Sync>);

impl ProgressSink {
    pub fn new(f: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    pub fn noop() -> Self {
        Self(Arc::new(|_| {}))
    }

    pub fn send(&self, progress: Progress) {
        (self.0)(progress);
    }
}

/// Verification hook for guided manual steps. Runs synchronously (file
/// checks); must pass before the step is marked done.
pub type CheckFn = Arc<dyn Fn(&GameCtx) -> anyhow::Result<()> + Send + Sync>;

/// A wizard step the user performs by hand (PLAN.md §4.2): `pre_check` must
/// pass *before* the user is asked to act; `verify` must pass after they
/// confirm.
#[derive(Clone)]
pub struct ManualStep {
    pub instructions_md: String,
    pub pre_check: Option<CheckFn>,
    pub verify: CheckFn,
}

/// One resumable unit of an install plan. `is_done` is the idempotency
/// probe: resume = rerun the plan, finished steps short-circuit.
#[async_trait::async_trait]
pub trait InstallStep: Send + Sync {
    /// Stable within a plan — the resume key.
    fn id(&self) -> String;
    fn label(&self) -> String;
    /// `Some` marks a guided manual step: the engine parks and the UI runs
    /// the wizard; `run` is never called for manual steps.
    fn manual(&self) -> Option<ManualStep> {
        None
    }
    async fn is_done(&self, ctx: &GameCtx) -> anyhow::Result<bool>;
    async fn run(
        &self,
        ctx: &GameCtx,
        progress: &ProgressSink,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()>;
}

/// A launched game: the spawned root process (owned by the watcher for
/// exit-code capture) plus the watcher configuration to use.
pub struct LaunchedGame {
    pub child: tokio::process::Child,
}

/// Implemented once per class of game (`GogGame`, `SwitchGame`, bespoke
/// classes for complex titles). A *title* is one constructed value of a
/// class, registered in [`crate::registry::build_registry`].
#[async_trait::async_trait]
pub trait GameClass: Send + Sync + 'static {
    fn meta(&self) -> &GameMeta;
    /// Bucket artifacts needed for install (preflight + UI display).
    fn artifacts(&self) -> Vec<ArtifactRef> {
        vec![]
    }
    /// Pinned upstream tools this game needs.
    fn tools(&self) -> Vec<ToolSpec> {
        vec![]
    }
    fn sync_folders(&self, ctx: &GameCtx) -> Vec<SyncFolderSpec>;
    fn install_plan(&self, ctx: &GameCtx) -> anyhow::Result<Vec<Box<dyn InstallStep>>>;
    /// Default uninstall: pause this game's sync folders, then remove the
    /// managed roots (engine-provided steps).
    fn uninstall_plan(&self, ctx: &GameCtx) -> anyhow::Result<Vec<Box<dyn InstallStep>>> {
        Ok(crate::steps::default_uninstall(
            self.sync_folders(ctx),
            &ctx.dirs,
        ))
    }
    async fn launch(&self, ctx: &GameCtx) -> anyhow::Result<LaunchedGame>;
    /// If launching needs the user to choose an executable first (the
    /// definition left it blank and none is remembered yet), return the
    /// detected candidates (paths relative to the extracted tree). Default:
    /// no choice ever needed.
    fn exe_candidates(&self, _ctx: &GameCtx) -> Option<Vec<String>> {
        None
    }
    /// All launchable executables under the install (for the favourites UI).
    fn list_exes(&self, _ctx: &GameCtx) -> Vec<String> {
        vec![]
    }
    /// Playtime measurement, overridable per class (PLAN.md §6.3).
    fn watcher(&self, ctx: &GameCtx) -> WatcherSpec {
        WatcherSpec {
            hint: self.watch_hint(),
            scope: WatchScope {
                under_path: Some(ctx.dirs.install_root.clone()),
            },
            ..WatcherSpec::default()
        }
    }
    fn watch_hint(&self) -> Option<WatchHint> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_options_legacy_include_dlc_means_all() {
        let o: InstallOptions =
            serde_json::from_str(r#"{"include_patches":false,"include_dlc":true}"#).unwrap();
        assert_eq!(o.dlc, DlcSelection::All);
        assert!(o.dlc.includes(Some("anything")));
    }

    #[test]
    fn install_options_legacy_no_dlc_means_empty() {
        let o: InstallOptions =
            serde_json::from_str(r#"{"include_patches":true,"include_dlc":false}"#).unwrap();
        assert!(o.include_patches);
        assert!(o.dlc.is_empty());
        assert!(!o.dlc.includes(Some("anything")));
    }

    #[test]
    fn install_options_named_selection_roundtrips() {
        let mut set = std::collections::BTreeSet::new();
        set.insert("Hats".to_string());
        set.insert("Maps".to_string());
        let o = InstallOptions {
            include_patches: true,
            dlc: DlcSelection::Named(set),
        };
        let json = serde_json::to_string(&o).unwrap();
        assert!(json.contains("dlc_names"));
        let back: InstallOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(back, o);
        assert!(back.dlc.includes(Some("Hats")));
        assert!(!back.dlc.includes(Some("Capes")));
    }

    #[test]
    fn install_options_all_roundtrips() {
        let o = InstallOptions {
            include_patches: false,
            dlc: DlcSelection::All,
        };
        let back: InstallOptions =
            serde_json::from_str(&serde_json::to_string(&o).unwrap()).unwrap();
        assert_eq!(back, o);
    }

    #[test]
    fn install_options_default_is_empty() {
        let o = InstallOptions::default();
        assert!(!o.include_patches);
        assert!(o.dlc.is_empty());
    }
}
