//! `GogGame`: titles installed from GOG offline installers stored as-is in
//! the bucket, extracted with innoextract, run via umu + GE-Proton
//! (PLAN.md §7.1). Instantiated from a server-stored [`GameDefinition`].
//!
//! Artifacts are grouped by role: the **base** group must contain exactly
//! one `.exe` (the innoextract target — never "whatever sorts first") plus
//! its `.bin` parts; **patch**/**dlc** installers are optional and applied
//! inside the prefix when selected at install time.

use std::path::PathBuf;

use anyhow::Context;
use game_mgr_api_types::{ArtifactRole, GameDefinition};
use serde::{Deserialize, Serialize};

use crate::game::{
    ArtifactRef, GameClass, GameCtx, GameMeta, InstallStep, LaunchedGame, SyncFolderSpec, WatchHint,
};
#[cfg(not(windows))]
use crate::run::{UmuLaunch, find_umu, resolve_proton_dir};
use crate::steps::{
    EnsurePrefixStep, EnsureSyncFolderStep, InnoExtractStep, RunInstallerInPrefixStep, S3FetchStep,
};

/// The class-specific `config` block of a gog definition. This shape is what
/// the Add Game UI fills in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GogConfig {
    /// umu database id, e.g. `umu-1086940`. Optional — not every title has a
    /// umu entry; when absent umu uses its default.
    #[serde(default)]
    pub umu_id: Option<String>,
    /// Class default GE-Proton version; the per-game user override wins.
    #[serde(default)]
    pub proton_default: Option<String>,
    /// Game executable relative to the extracted tree (innoextract output),
    /// e.g. `app/bin/bg3.exe`. Optional — when absent the user picks from the
    /// detected `.exe`s at first launch (the choice is then remembered).
    #[serde(default)]
    pub exe_rel: Option<String>,
    /// Process names that count as "playing" (PLAN.md §6.3).
    #[serde(default)]
    pub watch_exes: Vec<String>,
    /// Save location inside the prefix, relative to the prefix root —
    /// becomes the `gm-<id>-saves` Syncthing folder. Optional — when absent no
    /// saves folder is synced.
    #[serde(default)]
    pub saves_in_prefix: Option<String>,
}

pub struct GogGame {
    pub meta: GameMeta,
    pub artifacts: Vec<ArtifactRef>,
    pub config: GogConfig,
}

fn is_exe(key: &str) -> bool {
    key.to_lowercase().ends_with(".exe")
}

fn is_bin(key: &str) -> bool {
    key.to_lowercase().ends_with(".bin")
}

fn filename(key: &str) -> &str {
    key.rsplit('/').next().unwrap_or(key)
}

impl GogGame {
    /// Build from a server definition (class `gog`).
    pub fn from_definition(def: &GameDefinition) -> anyhow::Result<Self> {
        let version = semver::Version::parse(&def.version)
            .with_context(|| format!("game {}: version '{}' is not semver", def.id, def.version))?;
        let config: GogConfig = serde_json::from_value(def.config.clone())
            .with_context(|| format!("game {}: invalid gog config", def.id))?;
        Ok(GogGame {
            meta: GameMeta {
                id: def.id.clone(),
                title: def.title.clone(),
                class: def.class.clone(),
                version,
            },
            artifacts: def.artifacts.iter().map(Into::into).collect(),
            config,
        })
    }

    fn by_role(&self, role: ArtifactRole) -> Vec<&ArtifactRef> {
        self.artifacts.iter().filter(|a| a.role == role).collect()
    }

    /// The single base `.exe` innoextract runs on. Explicit — a stray `.sh`
    /// or a patch installer can never become the extraction target.
    fn base_exe(&self) -> anyhow::Result<&ArtifactRef> {
        let base = self.by_role(ArtifactRole::Base);
        let exes: Vec<&&ArtifactRef> = base.iter().filter(|a| is_exe(&a.bucket_key)).collect();
        anyhow::ensure!(
            exes.len() == 1,
            "{}: the base group needs exactly one .exe installer, found {} — \
             fix the roles in Edit game",
            self.meta.id,
            exes.len()
        );
        let offenders: Vec<&str> = base
            .iter()
            .filter(|a| !is_exe(&a.bucket_key) && !is_bin(&a.bucket_key))
            .map(|a| filename(&a.bucket_key))
            .collect();
        anyhow::ensure!(
            offenders.is_empty(),
            "{}: base files must be the .exe installer and its .bin parts; \
             set {} to Ignore or another role in Edit game (Linux .sh installers \
             don't work with the Proton pipeline)",
            self.meta.id,
            offenders.join(", ")
        );
        Ok(*exes[0])
    }

    fn game_dir(&self, ctx: &GameCtx) -> PathBuf {
        ctx.dirs.install_root.join("game")
    }

    /// The executable to launch, relative to the extracted tree: a transient
    /// override (favourite / just-picked) wins, then the definition's
    /// `exe_rel`, then the choice remembered from a previous launch
    /// (`ctx.chosen_exe`). `None` ⇒ the user must pick.
    fn resolve_exe_rel(&self, ctx: &GameCtx) -> Option<String> {
        ctx.exe_override
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.config.exe_rel.clone().filter(|s| !s.trim().is_empty()))
            .or_else(|| ctx.chosen_exe.clone().filter(|s| !s.trim().is_empty()))
    }

    /// Recursively list `.exe` candidates under the extracted game dir,
    /// relative to it, skipping uninstallers and common redistributables.
    /// Sorted shortest-path-first so the top-level launcher tends to lead.
    fn detect_exes(&self, ctx: &GameCtx) -> Vec<String> {
        let root = self.game_dir(ctx);
        let mut found = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if is_exe(&path.to_string_lossy()) {
                    let name = filename(&path.to_string_lossy()).to_lowercase();
                    // skip installers/uninstallers and obvious redistributables
                    if name.starts_with("unins")
                        || name.contains("vcredist")
                        || name.contains("dxsetup")
                        || name.contains("directx")
                        || name.contains("dotnet")
                        || name.contains("redist")
                    {
                        continue;
                    }
                    if let Ok(rel) = path.strip_prefix(&root) {
                        found.push(rel.to_string_lossy().replace('\\', "/"));
                    }
                }
            }
        }
        found.sort_by(|a, b| {
            a.matches('/')
                .count()
                .cmp(&b.matches('/').count())
                .then_with(|| a.cmp(b))
        });
        found
    }

    fn exe_path(&self, ctx: &GameCtx, rel: &str) -> PathBuf {
        self.game_dir(ctx).join(rel)
    }

    /// `Some` only when there is a saves path to sync.
    ///
    /// The folder id is scoped to the active profile's UUID
    /// (`gm-<game>-<profile>-saves`) so each profile gets its own globally
    /// unique save set that follows the profile when it's transferred between
    /// users/machines — no rename needed. Falls back to the legacy game-only
    /// id when no profile is active.
    fn saves_spec(&self, ctx: &GameCtx) -> Option<SyncFolderSpec> {
        let saves = self.config.saves_in_prefix.as_deref()?.trim();
        if saves.is_empty() {
            return None;
        }
        let folder_id = match ctx.profile_id {
            Some(profile) => format!("gm-{}-{}-saves", self.meta.id, profile),
            None => format!("gm-{}-saves", self.meta.id),
        };
        Some(SyncFolderSpec {
            // the label doubles as the on-disk folder name on a central
            // Syncthing server (/data/<label>), so it must be globally unique —
            // use the UUID-bearing folder id itself.
            label: folder_id.clone(),
            folder_id,
            local_path: resolve_save_path(ctx, saves),
            stignore: vec![],
            required_before_first_launch: true,
        })
    }

    /// Fetch + apply every `.exe` of a set of artifacts (their `.bin` parts
    /// are fetched alongside), in filename order.
    fn apply_group(
        &self,
        ctx: &GameCtx,
        group: &[&ArtifactRef],
        plan: &mut Vec<Box<dyn InstallStep>>,
    ) {
        let mut group: Vec<&ArtifactRef> = group.to_vec();
        group.sort_by(|a, b| a.bucket_key.cmp(&b.bucket_key));
        for artifact in &group {
            plan.push(Box::new(S3FetchStep::into_downloads(
                (*artifact).clone(),
                ctx,
            )));
        }
        for artifact in group.iter().filter(|a| is_exe(&a.bucket_key)) {
            plan.push(Box::new(RunInstallerInPrefixStep {
                installer: ctx.dirs.downloads.join(filename(&artifact.bucket_key)),
                umu_game_id: self.config.umu_id.clone(),
                store: "gog".into(),
                proton_default: self.config.proton_default.clone(),
            }));
        }
    }

    /// Apply all patch installers (a single group).
    fn patch_steps(&self, ctx: &GameCtx, plan: &mut Vec<Box<dyn InstallStep>>) {
        let group = self.by_role(ArtifactRole::Patch);
        self.apply_group(ctx, &group, plan);
    }

    /// Apply only the DLCs the install selected (`DlcSelection`). Each DLC is
    /// identified by its `dlc_name`; an artifact with no name lands in the
    /// empty-string bucket so `DlcSelection::Named("")` can still target it.
    fn dlc_steps(&self, ctx: &GameCtx, plan: &mut Vec<Box<dyn InstallStep>>) {
        let selected: Vec<&ArtifactRef> = self
            .by_role(ArtifactRole::Dlc)
            .into_iter()
            .filter(|a| ctx.options.dlc.includes(a.dlc_name.as_deref()))
            .collect();
        self.apply_group(ctx, &selected, plan);
    }

    /// Linux: through umu/GE-Proton, same as always.
    #[cfg(not(windows))]
    async fn spawn(&self, ctx: &GameCtx, exe: std::path::PathBuf) -> anyhow::Result<LaunchedGame> {
        let umu = find_umu(&ctx.services)?;
        let proton_dir = resolve_proton_dir(
            &ctx.services,
            ctx.proton_override.as_deref(),
            self.config.proton_default.as_deref(),
        );
        tracing::info!(
            target: "launch",
            game = %ctx.game_id,
            exe = %exe.display(),
            prefix = %ctx.dirs.prefix.display(),
            proton = ?proton_dir,
            umu_id = ?self.config.umu_id,
            mangohud = ctx.launch.mangohud,
            gamescope = ctx.launch.gamescope,
            "launching game",
        );
        let launch = UmuLaunch {
            exe,
            prefix: ctx.dirs.prefix.clone(),
            proton_dir,
            game_id: self.config.umu_id.clone(),
            store: "gog".into(),
        };
        // apply MangoHud/Gamescope wrappers before handing off to tokio
        let wrapped = crate::run::wrap_command(launch.command(&umu), &ctx.launch);
        let mut cmd = tokio::process::Command::from(wrapped);
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().context("spawning umu-run")?;
        // stream the game's output to the terminal + log file; the watcher
        // still owns the Child and wait()s on it for the exit code.
        crate::steps::forward_output(&mut child, "game", &ctx.game_id);
        Ok(LaunchedGame { child })
    }

    /// Windows: the game's own Windows build runs directly, no Proton/umu
    /// translation needed. See `crate::run::NativeLaunch`'s doc comment for
    /// which classes use this.
    #[cfg(windows)]
    async fn spawn(&self, ctx: &GameCtx, exe: std::path::PathBuf) -> anyhow::Result<LaunchedGame> {
        tracing::info!(
            target: "launch",
            game = %ctx.game_id,
            exe = %exe.display(),
            mangohud = ctx.launch.mangohud,
            gamescope = ctx.launch.gamescope,
            "launching game (native)",
        );
        let launch = crate::run::NativeLaunch { exe };
        let wrapped = crate::run::wrap_command(launch.command(), &ctx.launch);
        let mut cmd = tokio::process::Command::from(wrapped);
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().context("spawning game executable")?;
        crate::steps::forward_output(&mut child, "game", &ctx.game_id);
        Ok(LaunchedGame { child })
    }
}

/// Resolve `saves_in_prefix` (a path written in Wine-prefix terms, e.g.
/// `drive_c/users/steamuser/AppData/Local/<Studio>/<Game>/Saves` -- see
/// `GogConfig::saves_in_prefix`'s doc comment) to where the save data
/// actually lives on disk for the platform this is running on.
///
/// On Linux, that's the Proton prefix (Proton/umu's fake `C:` drive) at
/// `ctx.dirs.prefix`, so the path is used exactly as configured. On
/// Windows there is no prefix at all -- `EnsurePrefixStep` is a no-op there
/// (see its doc comment) because the game's own Windows build runs
/// natively and writes straight to the *real* user profile, not a fake
/// Wine one. Naively joining `saves_in_prefix` onto `ctx.dirs.prefix`
/// there points at a directory nothing ever writes to (an empty prefix
/// folder that was never created), so Syncthing ends up faithfully
/// syncing nothing while the actual save data sits untouched at the real
/// path. Translate the same relative structure onto Windows' real special
/// folders instead: strip the `drive_c/users/<wine-username>/` prefix
/// (the username there is Wine/Proton's own fake account and carries no
/// meaning here) and remap the well-known folder that follows --
/// `AppData/Local(Low)?`, `AppData/Roaming`, `Documents`, `Saved Games` --
/// onto its real Windows counterpart via `dirs`. Anything that doesn't
/// match one of those falls back to the same relative path under the
/// real home directory rather than silently dropping the saves folder
/// from sync altogether.
fn resolve_save_path(ctx: &GameCtx, saves_in_prefix: &str) -> PathBuf {
    if cfg!(windows) {
        native_windows_save_path(saves_in_prefix)
            .unwrap_or_else(|| ctx.dirs.prefix.join(saves_in_prefix))
    } else {
        ctx.dirs.prefix.join(saves_in_prefix)
    }
}

fn native_windows_save_path(saves_in_prefix: &str) -> Option<PathBuf> {
    resolve_windows_special_folder(
        saves_in_prefix,
        dirs::data_local_dir(),
        dirs::config_dir(),
        dirs::document_dir(),
        dirs::home_dir(),
    )
}

/// The actual mapping logic, taking the resolved special folders as
/// parameters instead of calling `dirs::*` directly -- not
/// `#[cfg(windows)]`-gated (it's pure path/string logic once its inputs are
/// in hand), so it's exercised by tests on every platform this crate's CI
/// runs rather than only the Windows job, same reasoning as
/// `apps/game-mgr/core/src/services.rs`'s `which_windows`. Taking the
/// folders as parameters rather than calling `dirs::document_dir()` etc.
/// inline additionally keeps tests deterministic: `document_dir()` reads
/// `~/.config/user-dirs.dirs` on Linux, which a bare CI image never has
/// (real Windows always has it, via the OS itself rather than optional
/// config), so a test that called it directly would flake there.
fn resolve_windows_special_folder(
    saves_in_prefix: &str,
    data_local: Option<PathBuf>,
    roaming: Option<PathBuf>,
    documents: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    let normalized = saves_in_prefix.replace('\\', "/");
    let mut parts = normalized.split('/').filter(|s| !s.is_empty());
    if parts.next()? != "drive_c" || parts.next()? != "users" {
        return None;
    }
    let _wine_username = parts.next()?; // Proton's fake account -- irrelevant natively
    let rest: Vec<&str> = parts.collect();
    let (first, after_first) = rest.split_first()?;
    let (base, tail): (PathBuf, &[&str]) = match (*first, after_first.first().copied()) {
        ("AppData", Some("Local")) => (data_local?, &after_first[1..]),
        ("AppData", Some("Roaming")) => (roaming?, &after_first[1..]),
        ("AppData", Some("LocalLow")) => {
            (data_local?.parent()?.join("LocalLow"), &after_first[1..])
        }
        ("Documents" | "My Documents", _) => (documents?, after_first),
        ("Saved Games", _) => (home?.join("Saved Games"), after_first),
        _ => (home?, &rest[..]),
    };
    let mut path = base;
    for component in tail {
        path.push(component);
    }
    Some(path)
}

#[async_trait::async_trait]
impl GameClass for GogGame {
    fn meta(&self) -> &GameMeta {
        &self.meta
    }

    fn artifacts(&self) -> Vec<ArtifactRef> {
        self.artifacts.clone()
    }

    fn sync_folders(&self, ctx: &GameCtx) -> Vec<SyncFolderSpec> {
        self.saves_spec(ctx).into_iter().collect()
    }

    fn install_plan(&self, ctx: &GameCtx) -> anyhow::Result<Vec<Box<dyn InstallStep>>> {
        anyhow::ensure!(
            !self.artifacts.is_empty(),
            "{} has no artifacts — scan its bucket prefix in Edit game \
             (see docs/game-mgr-buckets.md)",
            self.meta.id
        );
        let base_exe = self.base_exe()?;

        let mut plan: Vec<Box<dyn InstallStep>> = Vec::new();
        for artifact in self.by_role(ArtifactRole::Base) {
            plan.push(Box::new(S3FetchStep::into_downloads(artifact.clone(), ctx)));
        }
        plan.push(Box::new(InnoExtractStep {
            installer: ctx.dirs.downloads.join(filename(&base_exe.bucket_key)),
            out_dir: self.game_dir(ctx),
        }));
        plan.push(Box::new(EnsurePrefixStep {
            umu_game_id: self.config.umu_id.clone(),
            store: "gog".into(),
            proton_default: self.config.proton_default.clone(),
        }));
        if ctx.options.include_patches {
            self.patch_steps(ctx, &mut plan);
        }
        // dlc_steps self-limits to the selected DLCs (empty selection ⇒ none).
        self.dlc_steps(ctx, &mut plan);
        if let Some(spec) = self.saves_spec(ctx) {
            plan.push(Box::new(EnsureSyncFolderStep::new(spec)));
        }
        tracing::info!(
            target: "install",
            game = %ctx.game_id,
            steps = ?plan.iter().map(|s| s.id()).collect::<Vec<_>>(),
            "planned install",
        );
        Ok(plan)
    }

    async fn launch(&self, ctx: &GameCtx) -> anyhow::Result<LaunchedGame> {
        let rel = self.resolve_exe_rel(ctx).with_context(|| {
            format!(
                "{}: no executable chosen — pick one from the detected list",
                self.meta.id
            )
        })?;
        let exe = self.exe_path(ctx, &rel);
        anyhow::ensure!(exe.is_file(), "game executable missing: {}", exe.display());
        self.spawn(ctx, exe).await
    }

    fn exe_candidates(&self, ctx: &GameCtx) -> Option<Vec<String>> {
        // a choice is needed only when neither the definition nor a remembered
        // pick gives us an executable.
        if self.resolve_exe_rel(ctx).is_some() {
            return None;
        }
        Some(self.detect_exes(ctx))
    }

    fn list_exes(&self, ctx: &GameCtx) -> Vec<String> {
        self.detect_exes(ctx)
    }

    fn watch_hint(&self) -> Option<WatchHint> {
        if self.config.watch_exes.is_empty() {
            return None;
        }
        Some(WatchHint::ExeNames(self.config.watch_exes.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClientConfig;
    use crate::game::{DlcSelection, GameDirs, InstallOptions};
    use crate::services::Services;
    use game_mgr_api_types::ArtifactDto;
    use std::sync::Arc;

    fn artifact(key: &str, role: ArtifactRole) -> ArtifactDto {
        ArtifactDto {
            bucket_key: key.into(),
            sha256: "aa".repeat(32),
            size: Some(1024),
            role,
            dlc_name: None,
        }
    }

    fn dlc_artifact(key: &str, name: &str) -> ArtifactDto {
        ArtifactDto {
            dlc_name: Some(name.into()),
            ..artifact(key, ArtifactRole::Dlc)
        }
    }

    pub(crate) fn test_definition() -> GameDefinition {
        GameDefinition {
            id: "testgog".into(),
            title: "Test GOG Game".into(),
            class: "gog".into(),
            version: "1.0.0".into(),
            config: serde_json::json!({
                "umu_id": "umu-123",
                "exe_rel": "app/bin/test.exe",
                "watch_exes": ["test.exe"],
                "saves_in_prefix": "drive_c/users/steamuser/AppData/Local/Test/Saves"
            }),
            artifacts: vec![
                artifact("gog/testgog/setup_test_1.0.exe", ArtifactRole::Base),
                artifact("gog/testgog/setup_test_1.0-1.bin", ArtifactRole::Base),
                artifact("gog/testgog/patches/patch_to_1.1.exe", ArtifactRole::Patch),
                dlc_artifact("gog/testgog/dlc/setup_hats_dlc.exe", "Hats"),
                dlc_artifact("gog/testgog/dlc/setup_maps_dlc.exe", "Maps"),
            ],
        }
    }

    fn ctx_with(options: InstallOptions) -> GameCtx {
        let root = std::env::temp_dir().join("gm-gog-test");
        GameCtx {
            game_id: "testgog".into(),
            services: Arc::new(Services {
                config: ClientConfig::default(),
                http: reqwest::Client::new(),
                server: None,
                syncthing: Err("unused".into()),
                library_dir: root.join("lib"),
                tools_dir: root.join("tools"),
                downloads_dir: root.join("dl"),
            }),
            dirs: GameDirs {
                install_root: root.join("lib/testgog"),
                prefix: root.join("prefixes/testgog"),
                downloads: root.join("dl/testgog"),
            },
            proton_override: None,
            profile_id: None,
            chosen_exe: None,
            exe_override: None,
            launch: crate::game::LaunchOpts::default(),
            options,
        }
    }

    fn all_dlc() -> InstallOptions {
        InstallOptions {
            include_patches: true,
            dlc: DlcSelection::All,
        }
    }

    fn named_dlc(names: &[&str]) -> InstallOptions {
        InstallOptions {
            include_patches: false,
            dlc: DlcSelection::Named(names.iter().map(|s| s.to_string()).collect()),
        }
    }

    fn plan_ids(game: &GogGame, options: InstallOptions) -> Vec<String> {
        game.install_plan(&ctx_with(options))
            .unwrap()
            .iter()
            .map(|s| s.id())
            .collect()
    }

    fn expect_err(def: &GameDefinition) -> String {
        match GogGame::from_definition(def) {
            Err(err) => err.to_string(),
            Ok(_) => panic!("definition should have been rejected"),
        }
    }

    #[test]
    fn builds_from_definition_with_roles() {
        let game = GogGame::from_definition(&test_definition()).unwrap();
        assert_eq!(game.meta.id, "testgog");
        assert_eq!(game.artifacts.len(), 5);
        assert_eq!(game.by_role(ArtifactRole::Base).len(), 2);
        assert_eq!(game.by_role(ArtifactRole::Patch).len(), 1);
        assert_eq!(game.by_role(ArtifactRole::Dlc).len(), 2);
    }

    #[test]
    fn rejects_bad_version_and_config() {
        let mut bad_version = test_definition();
        bad_version.version = "latest".into();
        assert!(expect_err(&bad_version).contains("semver"));

        // all fields are optional now, so "bad" means a type mismatch /
        // unknown field (deny_unknown_fields), not a missing one
        let mut bad_config = test_definition();
        bad_config.config = serde_json::json!({ "umu_id": 123 });
        assert!(expect_err(&bad_config).contains("config"));
    }

    #[test]
    fn default_plan_installs_base_only() {
        let game = GogGame::from_definition(&test_definition()).unwrap();
        let ids = plan_ids(&game, InstallOptions::default());
        assert!(ids.iter().any(|id| id.contains("setup_test_1.0.exe")));
        assert!(ids.iter().any(|id| id.contains("setup_test_1.0-1.bin")));
        assert!(
            !ids.iter()
                .any(|id| id.contains("patch") || id.contains("dlc")),
            "optional groups must not install by default: {ids:?}"
        );
        // the innoextract target is the base exe, explicitly
        let extract = ids
            .iter()
            .find(|id| id.starts_with("innoextract:"))
            .unwrap();
        assert!(extract.contains("setup_test_1.0.exe"));
        assert_eq!(ids.last().unwrap(), "syncfolder:gm-testgog-saves");
    }

    #[test]
    fn selected_patches_and_all_dlc_are_fetched_and_applied() {
        let game = GogGame::from_definition(&test_definition()).unwrap();
        let ids = plan_ids(&game, all_dlc());
        assert!(ids.iter().any(|id| id
            == &format!(
            "run-installer:{}",
            ctx_with(InstallOptions::default())
                .dirs
                .downloads
                .join("patch_to_1.1.exe")
                .display()
        )));
        // DlcSelection::All installs every DLC
        assert!(
            ids.iter()
                .any(|id| id.contains("run-installer") && id.contains("setup_hats_dlc.exe"))
        );
        assert!(
            ids.iter()
                .any(|id| id.contains("run-installer") && id.contains("setup_maps_dlc.exe"))
        );
        // patches apply after the prefix exists, before the sync folder
        let prefix_pos = ids.iter().position(|id| id == "ensure-prefix").unwrap();
        let patch_pos = ids
            .iter()
            .position(|id| id.contains("patch_to_1.1.exe") && id.starts_with("run-installer"))
            .unwrap();
        assert!(patch_pos > prefix_pos);
    }

    #[test]
    fn only_selected_dlc_group_installs() {
        let game = GogGame::from_definition(&test_definition()).unwrap();
        let ids = plan_ids(&game, named_dlc(&["Hats"]));
        assert!(
            ids.iter()
                .any(|id| id.contains("run-installer") && id.contains("setup_hats_dlc.exe")),
            "selected DLC must install: {ids:?}"
        );
        assert!(
            !ids.iter().any(|id| id.contains("setup_maps_dlc.exe")),
            "unselected DLC must be omitted entirely: {ids:?}"
        );
    }

    #[test]
    fn empty_dlc_selection_installs_no_dlc() {
        let game = GogGame::from_definition(&test_definition()).unwrap();
        let ids = plan_ids(&game, InstallOptions::default());
        assert!(
            !ids.iter().any(|id| id.contains("dlc")),
            "default (empty) selection installs no DLC: {ids:?}"
        );
    }

    #[test]
    fn optional_config_fields_allow_blank() {
        // umu_id / exe_rel / saves all absent: the plan still builds, has no
        // saves sync folder, and a launch needs an exe choice.
        let mut def = test_definition();
        def.config = serde_json::json!({});
        def.artifacts = vec![artifact(
            "gog/testgog/setup_test_1.0.exe",
            ArtifactRole::Base,
        )];
        let game = GogGame::from_definition(&def).unwrap();
        assert_eq!(game.config.umu_id, None);
        assert_eq!(game.config.exe_rel, None);
        assert_eq!(game.config.saves_in_prefix, None);

        let ctx = ctx_with(InstallOptions::default());
        let ids: Vec<String> = game
            .install_plan(&ctx)
            .unwrap()
            .iter()
            .map(|s| s.id())
            .collect();
        assert!(
            !ids.iter().any(|id| id.starts_with("syncfolder:")),
            "no saves path ⇒ no sync folder: {ids:?}"
        );
        assert!(ids.iter().any(|id| id == "ensure-prefix"));
        // no exe configured and none remembered ⇒ a choice is required
        assert!(game.exe_candidates(&ctx).is_some());
    }

    #[test]
    fn farlanders_case_sh_in_base_fails_with_guidance() {
        // a native Linux installer classified as base must produce a clear
        // error, not an innoextract attempt
        let mut def = test_definition();
        def.artifacts = vec![
            artifact("gog/farlanders/farlanders_prologue.sh", ArtifactRole::Base),
            artifact("gog/farlanders/setup_farlanders.exe", ArtifactRole::Base),
        ];
        let game = GogGame::from_definition(&def).unwrap();
        let err = match game.install_plan(&ctx_with(InstallOptions::default())) {
            Err(err) => err.to_string(),
            Ok(_) => panic!(".sh in base must fail planning"),
        };
        assert!(err.contains(".sh"), "{err}");
        assert!(err.contains("Ignore"), "{err}");
    }

    #[test]
    fn base_group_requires_exactly_one_exe() {
        let mut zero = test_definition();
        zero.artifacts = vec![artifact("gog/x/setup-1.bin", ArtifactRole::Base)];
        let game = GogGame::from_definition(&zero).unwrap();
        assert!(
            game.install_plan(&ctx_with(InstallOptions::default()))
                .is_err()
        );

        let mut two = test_definition();
        two.artifacts = vec![
            artifact("gog/x/setup_a.exe", ArtifactRole::Base),
            artifact("gog/x/setup_b.exe", ArtifactRole::Base),
        ];
        let game = GogGame::from_definition(&two).unwrap();
        let err = match game.install_plan(&ctx_with(InstallOptions::default())) {
            Err(err) => err.to_string(),
            Ok(_) => panic!("two base exes must fail"),
        };
        assert!(err.contains("exactly one"), "{err}");
    }

    /// Expected `local_path` for `test_definition()`'s
    /// `"drive_c/users/steamuser/AppData/Local/Test/Saves"` fixture, on
    /// whichever platform these tests are actually running on -- Windows
    /// resolves onto the real profile (no Proton prefix exists there, see
    /// `resolve_save_path`), everywhere else still nests under the prefix.
    fn expected_saves_path(ctx: &GameCtx) -> PathBuf {
        if cfg!(windows) {
            dirs::data_local_dir().unwrap().join("Test/Saves")
        } else {
            ctx.dirs
                .prefix
                .join("drive_c/users/steamuser/AppData/Local/Test/Saves")
        }
    }

    #[test]
    fn saves_folder_lives_inside_the_prefix() {
        let game = GogGame::from_definition(&test_definition()).unwrap();
        let ctx = ctx_with(InstallOptions::default());
        let folders = game.sync_folders(&ctx);
        assert_eq!(folders.len(), 1);
        // no active profile ⇒ legacy game-only id
        assert_eq!(folders[0].folder_id, "gm-testgog-saves");
        assert_eq!(folders[0].local_path, expected_saves_path(&ctx));
    }

    #[test]
    fn saves_folder_id_is_scoped_to_the_active_profile() {
        let game = GogGame::from_definition(&test_definition()).unwrap();
        let profile = uuid::Uuid::new_v4();
        let mut ctx = ctx_with(InstallOptions::default());
        ctx.profile_id = Some(profile);
        let folders = game.sync_folders(&ctx);
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].folder_id, format!("gm-testgog-{profile}-saves"));
        // path is unchanged — only the synced folder's identity is per-profile
        assert_eq!(folders[0].local_path, expected_saves_path(&ctx));
    }

    /// Synthetic (not the real environment's) special folders, so these
    /// tests are deterministic regardless of what's actually configured on
    /// whatever machine runs them -- see `resolve_windows_special_folder`'s
    /// doc comment on why that matters (`dirs::document_dir()` in
    /// particular needs `~/.config/user-dirs.dirs` on Linux, which a bare
    /// CI image never has).
    fn fake_folders() -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let root = PathBuf::from("C:/Users/fake");
        (
            root.join("AppData/Local"),
            root.join("AppData/Roaming"),
            root.join("Documents"),
            root,
        )
    }

    #[test]
    fn native_windows_save_path_maps_appdata_local() {
        let (data_local, roaming, documents, home) = fake_folders();
        let resolved = resolve_windows_special_folder(
            "drive_c/users/steamuser/AppData/Local/Studio/Game/Saves",
            Some(data_local.clone()),
            Some(roaming),
            Some(documents),
            Some(home),
        )
        .expect("should resolve a well-formed AppData/Local path");
        assert_eq!(resolved, data_local.join("Studio/Game/Saves"));
    }

    #[test]
    fn native_windows_save_path_maps_appdata_roaming() {
        let (data_local, roaming, documents, home) = fake_folders();
        let resolved = resolve_windows_special_folder(
            "drive_c/users/steamuser/AppData/Roaming/Studio/Saves",
            Some(data_local),
            Some(roaming.clone()),
            Some(documents),
            Some(home),
        )
        .expect("should resolve a well-formed AppData/Roaming path");
        assert_eq!(resolved, roaming.join("Studio/Saves"));
    }

    #[test]
    fn native_windows_save_path_maps_appdata_locallow() {
        let (data_local, roaming, documents, home) = fake_folders();
        let resolved = resolve_windows_special_folder(
            "drive_c/users/steamuser/AppData/LocalLow/Studio/Game/saved",
            Some(data_local.clone()),
            Some(roaming),
            Some(documents),
            Some(home),
        )
        .expect("should resolve a well-formed AppData/LocalLow path");
        assert_eq!(
            resolved,
            data_local
                .parent()
                .unwrap()
                .join("LocalLow/Studio/Game/saved")
        );
    }

    #[test]
    fn native_windows_save_path_maps_documents() {
        let (data_local, roaming, documents, home) = fake_folders();
        let resolved = resolve_windows_special_folder(
            "drive_c/users/steamuser/Documents/My Games/Game/Saves",
            Some(data_local),
            Some(roaming),
            Some(documents.clone()),
            Some(home),
        )
        .expect("should resolve a well-formed Documents path");
        assert_eq!(resolved, documents.join("My Games/Game/Saves"));
    }

    #[test]
    fn native_windows_save_path_maps_saved_games() {
        let (data_local, roaming, documents, home) = fake_folders();
        let resolved = resolve_windows_special_folder(
            "drive_c/users/steamuser/Saved Games/Studio/Game",
            Some(data_local),
            Some(roaming),
            Some(documents),
            Some(home.clone()),
        )
        .expect("should resolve a well-formed Saved Games path");
        assert_eq!(resolved, home.join("Saved Games/Studio/Game"));
    }

    #[test]
    fn native_windows_save_path_falls_back_to_home_for_unknown_folders() {
        let (data_local, roaming, documents, home) = fake_folders();
        let resolved = resolve_windows_special_folder(
            "drive_c/users/steamuser/SomeOtherPlace/Saves",
            Some(data_local),
            Some(roaming),
            Some(documents),
            Some(home.clone()),
        )
        .expect("unrecognized folder should still fall back, not fail outright");
        assert_eq!(resolved, home.join("SomeOtherPlace/Saves"));
    }

    #[test]
    fn native_windows_save_path_rejects_non_prefix_shaped_input() {
        assert!(native_windows_save_path("not/a/prefix/path").is_none());
        assert!(native_windows_save_path("drive_c/Program Files/Game/Saves").is_none());
    }
}
