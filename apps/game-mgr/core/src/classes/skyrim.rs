//! `SkyrimModded`: a bespoke class for modded Skyrim through Mod Organizer 2
//! (PLAN.md §7.2), adapted to a portable MO2 instance that lives on the mesh.
//!
//! Split of responsibilities:
//! - **Per-machine, not synced:** the GOG Skyrim game (innoextract) plus SKSE
//!   dropped on top, installed **inside the prefix's C: drive** at a fixed
//!   Windows path (e.g. `C:\GOG Games\Skyrim Anniversary Edition`).
//! - **Synced via Syncthing:** three sibling folders — `Archive`, `Data`,
//!   `Skyrim MO2` — that hold the MO2 portable instance, its mods and saves.
//!   game-mgr never writes into them; their contents are MO2's domain. They
//!   also live on the prefix's **C: drive**, under a fixed base path
//!   (`C:\game-mgr\<id>` by default).
//!
//! Everything is reached through the **C: drive** on purpose: the prefix is
//! per-machine but `C:\…` paths are identical everywhere, so MO2's config is
//! portable. We deliberately avoid a custom drive letter (X:, …) — Wine's
//! mount manager auto-assigns letters to detected volumes (e.g. a separate
//! `/home` mount) and would clobber a hand-mapped drive on every launch; only
//! `C:` (always `drive_c`) is safe.
//!
//! Launch is `umu-run` on `…/Skyrim MO2/ModOrganizer.exe`.

use std::path::PathBuf;

use anyhow::Context;
use game_mgr_api_types::{ArtifactRole, GameDefinition};
use serde::{Deserialize, Serialize};

use crate::game::{
    ArtifactRef, GameClass, GameCtx, GameMeta, InstallStep, LaunchedGame, SyncFolderSpec,
    WatchHint, WatchScope, WatcherSpec,
};
use crate::run::{UmuLaunch, find_umu, resolve_proton_dir};
use crate::steps::{
    EnsurePrefixStep, EnsureSyncFolderStep, ExtractArchiveStep, InnoExtractStep, S3FetchStep,
};

/// Subdirectory names of the three synced folders, under the C: sync root.
const ARCHIVE_DIR: &str = "Archive";
const DATA_DIR: &str = "Data";
const MO2_DIR: &str = "Skyrim MO2";

/// Default game install path on the prefix's C: drive — matches a common GOG
/// install location so MO2's `gamePath` lines up out of the box.
const DEFAULT_GAME_PATH_IN_PREFIX: &str = "GOG Games/Skyrim Anniversary Edition";
/// Default launcher path, relative to the C: sync root.
const DEFAULT_MO2_EXE_REL: &str = "Skyrim MO2/ModOrganizer.exe";
/// Default processes that count as "playing" (the game, not MO2 browsing).
const DEFAULT_WATCH_EXES: &[&str] = &["SkyrimSE.exe", "SkyrimSELauncher.exe", "skse64_loader.exe"];

/// The class-specific `config` block of a skyrim-modded definition. Every
/// field is optional; the Add Game UI fills in what the instance needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SkyrimConfig {
    /// umu database id for Skyrim SE; `None` lets umu use its default.
    #[serde(default)]
    pub umu_id: Option<String>,
    /// Class default GE-Proton version; the per-game user override wins.
    #[serde(default)]
    pub proton_default: Option<String>,
    /// Processes that count as playtime; defaults applied when empty.
    #[serde(default)]
    pub watch_exes: Vec<String>,
    /// Bucket key of the SKSE archive (7z/zip), picked in the Add Game UI.
    /// `None` ⇒ no SKSE is installed.
    #[serde(default)]
    pub skse_key: Option<String>,
    /// Game install path on the prefix's C: drive (a Windows-style relative
    /// path under `C:\`, `/` or `\` separated); defaults to
    /// `GOG Games/Skyrim Anniversary Edition`. Set this to match MO2's
    /// configured `gamePath`.
    #[serde(default)]
    pub game_path_in_prefix: Option<String>,
    /// Base path on the prefix's C: drive that holds the three synced folders
    /// (`/` or `\` separated, under `C:\`); defaults to `game-mgr/<id>`. Set
    /// this to match where MO2 expects Archive/Data/Skyrim MO2.
    #[serde(default)]
    pub sync_root_in_prefix: Option<String>,
    /// Launcher path relative to the C: sync root; defaults to
    /// `Skyrim MO2/ModOrganizer.exe`.
    #[serde(default)]
    pub mo2_exe_rel: Option<String>,
}

pub struct SkyrimModded {
    pub meta: GameMeta,
    pub artifacts: Vec<ArtifactRef>,
    pub config: SkyrimConfig,
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

impl SkyrimModded {
    /// Build from a server definition (class `skyrim-modded`).
    pub fn from_definition(def: &GameDefinition) -> anyhow::Result<Self> {
        let version = semver::Version::parse(&def.version)
            .with_context(|| format!("game {}: version '{}' is not semver", def.id, def.version))?;
        let config: SkyrimConfig = serde_json::from_value(def.config.clone())
            .with_context(|| format!("game {}: invalid skyrim-modded config", def.id))?;
        Ok(SkyrimModded {
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

    /// The SKSE archive artifact, identified by its configured bucket key.
    fn skse_artifact(&self) -> Option<&ArtifactRef> {
        let key = self.config.skse_key.as_deref()?;
        self.artifacts.iter().find(|a| a.bucket_key == key)
    }

    /// Artifacts of a role, excluding the SKSE archive (which the user may have
    /// tagged with any role — it's handled by its own step).
    fn by_role(&self, role: ArtifactRole) -> Vec<&ArtifactRef> {
        let skse = self.config.skse_key.as_deref();
        self.artifacts
            .iter()
            .filter(|a| a.role == role && Some(a.bucket_key.as_str()) != skse)
            .collect()
    }

    /// The GOG installer parts (base role, minus SKSE).
    fn gog_base(&self) -> Vec<&ArtifactRef> {
        self.by_role(ArtifactRole::Base)
    }

    /// Fetch a group of GOG installer artifacts (.exe + its .bin parts) and
    /// innoextract each `.exe` into the game dir, overlaying onto the base.
    /// Used for patches and DLC (e.g. the Anniversary Edition upgrade), which
    /// are separate GOG offline installers extracted on top of the SE base.
    fn extract_group(
        &self,
        ctx: &GameCtx,
        group: &[&ArtifactRef],
        plan: &mut Vec<Box<dyn InstallStep>>,
    ) {
        let mut group = group.to_vec();
        group.sort_by(|a, b| a.bucket_key.cmp(&b.bucket_key));
        for artifact in &group {
            plan.push(Box::new(S3FetchStep::into_downloads(
                (*artifact).clone(),
                ctx,
            )));
        }
        for artifact in group.iter().filter(|a| is_exe(&a.bucket_key)) {
            plan.push(Box::new(InnoExtractStep {
                installer: ctx.dirs.downloads.join(filename(&artifact.bucket_key)),
                out_dir: self.game_dir(ctx),
            }));
        }
    }

    /// The single GOG `.exe` innoextract runs on — explicit, never "whatever
    /// sorts first".
    fn base_exe(&self) -> anyhow::Result<&ArtifactRef> {
        let base = self.gog_base();
        let exes: Vec<&&ArtifactRef> = base.iter().filter(|a| is_exe(&a.bucket_key)).collect();
        anyhow::ensure!(
            exes.len() == 1,
            "{}: the base group needs exactly one GOG .exe installer, found {} — \
             fix the roles in Edit game (the SKSE archive must be picked as SKSE, \
             not left as a base file)",
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
            "{}: base files must be the GOG .exe installer and its .bin parts; \
             set {} to Ignore or pick it as the SKSE archive in Edit game",
            self.meta.id,
            offenders.join(", ")
        );
        Ok(*exes[0])
    }

    /// The prefix's `C:` drive root (`drive_c`). umu/Proton place the wine
    /// prefix at `WINEPREFIX` directly, so `drive_c` sits at the prefix root
    /// (same assumption the GOG class uses for in-prefix save paths).
    fn drive_c(&self, ctx: &GameCtx) -> PathBuf {
        ctx.dirs.prefix.join("drive_c")
    }

    /// Join a Windows-style relative path (`/` or `\` separated) onto a base,
    /// component by component, so it nests correctly on the host filesystem.
    fn join_win(base: PathBuf, rel: &str) -> PathBuf {
        let mut dir = base;
        for component in rel.replace('\\', "/").split('/').filter(|s| !s.is_empty()) {
            dir.push(component);
        }
        dir
    }

    /// Where the GOG game + SKSE are installed: inside the prefix's C: drive at
    /// the configured Windows path. Per-machine (the prefix is per-machine) but
    /// at an identical `C:\…` path everywhere, so MO2's gamePath is portable.
    fn game_dir(&self, ctx: &GameCtx) -> PathBuf {
        let rel = self
            .config
            .game_path_in_prefix
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_GAME_PATH_IN_PREFIX);
        Self::join_win(self.drive_c(ctx), rel)
    }

    /// Base path on the C: drive holding the three synced folders. Defaults to
    /// `game-mgr/<id>` so two skyrim-modded titles never collide.
    fn sync_root(&self, ctx: &GameCtx) -> PathBuf {
        let rel = self
            .config
            .sync_root_in_prefix
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("game-mgr/{}", self.meta.id));
        Self::join_win(self.drive_c(ctx), &rel)
    }

    /// The ModOrganizer.exe to launch (under the C: sync root).
    fn mo2_exe(&self, ctx: &GameCtx) -> PathBuf {
        let rel = self
            .config
            .mo2_exe_rel
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_MO2_EXE_REL);
        Self::join_win(self.sync_root(ctx), rel)
    }

    fn watch_exes(&self) -> Vec<String> {
        if self.config.watch_exes.is_empty() {
            DEFAULT_WATCH_EXES.iter().map(|s| s.to_string()).collect()
        } else {
            self.config.watch_exes.clone()
        }
    }
}

#[async_trait::async_trait]
impl GameClass for SkyrimModded {
    fn meta(&self) -> &GameMeta {
        &self.meta
    }

    fn artifacts(&self) -> Vec<ArtifactRef> {
        self.artifacts.clone()
    }

    /// Three sibling Syncthing folders under the C: sync root. Sibling (not
    /// nested) layout sidesteps the nested-folder `.stignore` discipline
    /// (PLAN.md Risk 1) entirely — no folder's path is inside another's.
    fn sync_folders(&self, ctx: &GameCtx) -> Vec<SyncFolderSpec> {
        let root = self.sync_root(ctx);
        let mk = |dir: &str, suffix: &str, required: bool| SyncFolderSpec {
            folder_id: format!("gm-{}-{}", self.meta.id, suffix),
            label: format!("gm-{}-{}", self.meta.id, suffix),
            local_path: root.join(dir),
            stignore: vec![],
            required_before_first_launch: required,
        };
        vec![
            // the launcher + mods config live here — needed to start at all
            mk(MO2_DIR, "mo2", true),
            // game data / mods — needed for a meaningful launch
            mk(DATA_DIR, "data", true),
            // downloaded mod archives — nice to have, don't block launch
            mk(ARCHIVE_DIR, "archive", false),
        ]
    }

    fn install_plan(&self, ctx: &GameCtx) -> anyhow::Result<Vec<Box<dyn InstallStep>>> {
        anyhow::ensure!(
            !self.artifacts.is_empty(),
            "{} has no artifacts — scan its bucket prefix in Edit game \
             (see docs/uploading-game-data.md)",
            self.meta.id
        );
        let base_exe = self.base_exe()?;

        let mut plan: Vec<Box<dyn InstallStep>> = Vec::new();

        // 1. prefix first (gives us drive_c for everything below)
        plan.push(Box::new(EnsurePrefixStep {
            umu_game_id: self.config.umu_id.clone(),
            store: "gog".into(),
            proton_default: self.config.proton_default.clone(),
        }));

        // 2. GOG game → innoextract into the C: game dir (per-machine)
        for artifact in self.gog_base() {
            plan.push(Box::new(S3FetchStep::into_downloads(artifact.clone(), ctx)));
        }
        plan.push(Box::new(InnoExtractStep {
            installer: ctx.dirs.downloads.join(filename(&base_exe.bucket_key)),
            out_dir: self.game_dir(ctx),
        }));

        // 3. patches + selected DLC (e.g. Anniversary Edition) → innoextract on
        //    top of the base, same as GOG, picking up the player's selection.
        if ctx.options.include_patches {
            let patches = self.by_role(ArtifactRole::Patch);
            self.extract_group(ctx, &patches, &mut plan);
        }
        let dlc: Vec<&ArtifactRef> = self
            .by_role(ArtifactRole::Dlc)
            .into_iter()
            .filter(|a| ctx.options.dlc.includes(a.dlc_name.as_deref()))
            .collect();
        self.extract_group(ctx, &dlc, &mut plan);

        // 4. SKSE on top of the game dir (strip its wrapping skse64_<ver>/ dir
        //    so the loader/DLLs land next to the game exe).
        if let Some(skse) = self.skse_artifact() {
            plan.push(Box::new(S3FetchStep::into_downloads(skse.clone(), ctx)));
            plan.push(Box::new(ExtractArchiveStep {
                archive: ctx.dirs.downloads.join(filename(&skse.bucket_key)),
                out_dir: self.game_dir(ctx),
                strip_top_level: true,
            }));
        }

        // 5. the three synced folders under the C: sync root (creates the dirs)
        for spec in self.sync_folders(ctx) {
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
        let umu = find_umu(&ctx.services)?;
        let exe = self.mo2_exe(ctx);
        anyhow::ensure!(
            exe.is_file(),
            "Mod Organizer not found at {} — has the 'Skyrim MO2' folder finished syncing?",
            exe.display()
        );
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
            "launching modded skyrim via MO2",
        );
        let launch = UmuLaunch {
            exe,
            prefix: ctx.dirs.prefix.clone(),
            proton_dir,
            game_id: self.config.umu_id.clone(),
            store: "gog".into(),
        };
        let wrapped = crate::run::wrap_command(launch.command(&umu), &ctx.launch);
        let mut cmd = tokio::process::Command::from(wrapped);
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().context("spawning umu-run (MO2)")?;
        crate::steps::forward_output(&mut child, "mo2", &ctx.game_id);
        Ok(LaunchedGame { child })
    }

    fn watch_hint(&self) -> Option<WatchHint> {
        Some(WatchHint::ExeNames(self.watch_exes()))
    }

    /// Scope playtime detection to this game's **prefix**: the game (in
    /// `drive_c`) and MO2 (in the C: sync root) both run under it. The default
    /// scope is `install_root`, which no longer contains the running processes.
    fn watcher(&self, ctx: &GameCtx) -> WatcherSpec {
        WatcherSpec {
            hint: self.watch_hint(),
            scope: WatchScope {
                under_path: Some(ctx.dirs.prefix.clone()),
            },
            ..WatcherSpec::default()
        }
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

    fn test_definition() -> GameDefinition {
        GameDefinition {
            id: "skyrim".into(),
            title: "Skyrim (modded)".into(),
            class: "skyrim-modded".into(),
            version: "1.0.0".into(),
            config: serde_json::json!({
                "umu_id": "umu-489830",
                "skse_key": "gog/skyrim/skse/skse64_2_02_06.7z",
            }),
            artifacts: vec![
                artifact("gog/skyrim/setup_skyrim_se_1.0.exe", ArtifactRole::Base),
                artifact("gog/skyrim/setup_skyrim_se_1.0-1.bin", ArtifactRole::Base),
                artifact("gog/skyrim/skse/skse64_2_02_06.7z", ArtifactRole::Base),
            ],
        }
    }

    fn ctx() -> GameCtx {
        let root = std::env::temp_dir().join("gm-skyrim-test");
        GameCtx {
            game_id: "skyrim".into(),
            services: Arc::new(Services {
                config: ClientConfig::default(),
                http: reqwest::Client::new(),
                s3: None,
                syncthing: Err("unused".into()),
                library_dir: root.join("lib"),
                tools_dir: root.join("tools"),
                downloads_dir: root.join("dl"),
            }),
            dirs: GameDirs {
                install_root: root.join("lib/skyrim"),
                prefix: root.join("prefixes/skyrim"),
                downloads: root.join("dl/skyrim"),
            },
            proton_override: None,
            profile_id: None,
            chosen_exe: None,
            exe_override: None,
            launch: crate::game::LaunchOpts::default(),
            options: InstallOptions::default(),
        }
    }

    fn ctx_with(options: InstallOptions) -> GameCtx {
        GameCtx { options, ..ctx() }
    }

    fn plan_ids(game: &SkyrimModded) -> Vec<String> {
        game.install_plan(&ctx())
            .unwrap()
            .iter()
            .map(|s| s.id())
            .collect()
    }

    fn plan_ids_with(game: &SkyrimModded, options: InstallOptions) -> Vec<String> {
        game.install_plan(&ctx_with(options))
            .unwrap()
            .iter()
            .map(|s| s.id())
            .collect()
    }

    #[test]
    fn builds_from_definition() {
        let game = SkyrimModded::from_definition(&test_definition()).unwrap();
        assert_eq!(game.meta.id, "skyrim");
        assert_eq!(game.config.umu_id.as_deref(), Some("umu-489830"));
        assert_eq!(game.artifacts.len(), 3);
    }

    fn expect_err(def: &GameDefinition) -> String {
        match SkyrimModded::from_definition(def) {
            Err(err) => err.to_string(),
            Ok(_) => panic!("definition should have been rejected"),
        }
    }

    #[test]
    fn rejects_bad_version_and_config() {
        let mut bad_version = test_definition();
        bad_version.version = "latest".into();
        assert!(expect_err(&bad_version).contains("semver"));

        let mut bad_config = test_definition();
        bad_config.config = serde_json::json!({ "umu_id": 123 });
        assert!(expect_err(&bad_config).contains("config"));
    }

    #[test]
    fn skse_is_excluded_from_the_gog_base_group() {
        let game = SkyrimModded::from_definition(&test_definition()).unwrap();
        // the .7z lives in the base role but must not be treated as a GOG part
        let base = game.gog_base();
        assert_eq!(base.len(), 2, "only the .exe + .bin are GOG base parts");
        assert!(base.iter().all(|a| !a.bucket_key.ends_with(".7z")));
        assert!(game.base_exe().is_ok());
        assert_eq!(
            game.skse_artifact().unwrap().bucket_key,
            "gog/skyrim/skse/skse64_2_02_06.7z"
        );
    }

    #[test]
    fn base_group_must_have_exactly_one_exe() {
        // SKSE archive left untagged would otherwise look like a stray base file
        let mut def = test_definition();
        def.config = serde_json::json!({}); // no skse_key
        let game = SkyrimModded::from_definition(&def).unwrap();
        let err = match game.install_plan(&ctx()) {
            Err(err) => err.to_string(),
            Ok(_) => panic!("untagged .7z in base must fail planning"),
        };
        assert!(err.contains("SKSE"), "{err}");
    }

    #[test]
    fn selected_dlc_is_fetched_and_innoextracted() {
        let mut def = test_definition();
        def.artifacts.push(dlc_artifact(
            "gog/skyrim/ae/setup_anniversary_edition.exe",
            "Anniversary Edition",
        ));
        def.artifacts.push(ArtifactDto {
            dlc_name: Some("Anniversary Edition".into()),
            ..artifact(
                "gog/skyrim/ae/setup_anniversary_edition-1.bin",
                ArtifactRole::Dlc,
            )
        });
        let game = SkyrimModded::from_definition(&def).unwrap();

        // not selected ⇒ no AE steps at all
        let none = plan_ids_with(&game, InstallOptions::default());
        assert!(
            !none.iter().any(|id| id.contains("anniversary")),
            "unselected DLC must be omitted: {none:?}"
        );

        // selected ⇒ fetch (.exe + .bin) and innoextract the AE installer
        let mut set = std::collections::BTreeSet::new();
        set.insert("Anniversary Edition".to_string());
        let opts = InstallOptions {
            include_patches: false,
            dlc: DlcSelection::Named(set),
        };
        let ids = plan_ids_with(&game, opts);
        assert!(
            ids.iter()
                .any(|id| id.starts_with("fetch:") && id.contains("setup_anniversary_edition.exe")),
            "{ids:?}"
        );
        assert!(
            ids.iter()
                .any(|id| id.contains("setup_anniversary_edition-1.bin")),
            "the DLC .bin part must be fetched too: {ids:?}"
        );
        assert!(
            ids.iter()
                .any(|id| id.starts_with("innoextract:")
                    && id.contains("setup_anniversary_edition.exe")),
            "the AE installer must be innoextracted onto the base: {ids:?}"
        );
    }

    #[test]
    fn plan_order_prefix_game_skse_then_folders() {
        let game = SkyrimModded::from_definition(&test_definition()).unwrap();
        let ids = plan_ids(&game);
        let pos = |needle: &str| ids.iter().position(|id| id.starts_with(needle)).unwrap();
        assert!(pos("ensure-prefix") < pos("innoextract:"), "{ids:?}");
        assert!(
            pos("innoextract:") < pos("extract:"),
            "skse after game: {ids:?}"
        );
        assert!(pos("extract:") < pos("syncfolder:"), "{ids:?}");
        // no custom drive mapping any more — everything lives on C:
        assert!(!ids.iter().any(|id| id.starts_with("drive-map")), "{ids:?}");
    }

    #[test]
    fn without_skse_no_extract_step() {
        let mut def = test_definition();
        def.config = serde_json::json!({}); // no skse_key
        def.artifacts = vec![
            artifact("gog/skyrim/setup_skyrim_se_1.0.exe", ArtifactRole::Base),
            artifact("gog/skyrim/setup_skyrim_se_1.0-1.bin", ArtifactRole::Base),
        ];
        let game = SkyrimModded::from_definition(&def).unwrap();
        let ids = plan_ids(&game);
        assert!(!ids.iter().any(|id| id.starts_with("extract:")), "{ids:?}");
    }

    #[test]
    fn three_sibling_sync_folders_none_nested() {
        let game = SkyrimModded::from_definition(&test_definition()).unwrap();
        let ctx = ctx();
        let folders = game.sync_folders(&ctx);
        assert_eq!(folders.len(), 3);
        let ids: Vec<&str> = folders.iter().map(|f| f.folder_id.as_str()).collect();
        assert!(ids.contains(&"gm-skyrim-mo2"));
        assert!(ids.contains(&"gm-skyrim-data"));
        assert!(ids.contains(&"gm-skyrim-archive"));
        // crucial invariant: no folder path lies inside another (Risk 1) — the
        // sibling layout makes nested-folder .stignore discipline unnecessary.
        for a in &folders {
            for b in &folders {
                if a.folder_id != b.folder_id {
                    assert!(
                        !a.local_path.starts_with(&b.local_path),
                        "{} is nested inside {}",
                        a.local_path.display(),
                        b.local_path.display()
                    );
                }
            }
            // every folder lives under the C: sync root, on the C: drive
            assert!(a.local_path.starts_with(game.sync_root(&ctx)));
            assert!(a.local_path.starts_with(ctx.dirs.prefix.join("drive_c")));
        }
        // default sync root is game-mgr/<id> on C:
        assert!(
            game.sync_root(&ctx).ends_with("drive_c/game-mgr/skyrim"),
            "{:?}",
            game.sync_root(&ctx)
        );
    }

    #[test]
    fn game_and_mo2_live_on_the_c_drive_by_default() {
        let game = SkyrimModded::from_definition(&test_definition()).unwrap();
        let ctx = ctx();
        let drive_c = ctx.dirs.prefix.join("drive_c");
        let game_dir = game.game_dir(&ctx);
        assert!(game_dir.starts_with(&drive_c), "{game_dir:?}");
        assert!(
            game_dir.ends_with("drive_c/GOG Games/Skyrim Anniversary Edition"),
            "{game_dir:?}"
        );
        // MO2 lives under the C: sync root, not a custom drive
        let mo2 = game.mo2_exe(&ctx);
        assert!(mo2.starts_with(&drive_c), "{mo2:?}");
        assert!(
            mo2.ends_with("drive_c/game-mgr/skyrim/Skyrim MO2/ModOrganizer.exe"),
            "{mo2:?}"
        );
    }

    #[test]
    fn paths_accept_windows_separators_and_overrides() {
        let mut def = test_definition();
        def.config = serde_json::json!({
            "game_path_in_prefix": r"Games\Skyrim SE",
            "sync_root_in_prefix": r"Modding\Skyrim",
            "mo2_exe_rel": "MO2/ModOrganizer.exe",
        });
        let game = SkyrimModded::from_definition(&def).unwrap();
        let ctx = ctx();
        assert!(
            game.game_dir(&ctx).ends_with("drive_c/Games/Skyrim SE"),
            "{:?}",
            game.game_dir(&ctx)
        );
        assert!(
            game.sync_root(&ctx).ends_with("drive_c/Modding/Skyrim"),
            "{:?}",
            game.sync_root(&ctx)
        );
        assert!(
            game.mo2_exe(&ctx)
                .ends_with("drive_c/Modding/Skyrim/MO2/ModOrganizer.exe"),
            "{:?}",
            game.mo2_exe(&ctx)
        );
    }

    #[test]
    fn watcher_is_scoped_to_the_prefix() {
        let game = SkyrimModded::from_definition(&test_definition()).unwrap();
        let ctx = ctx();
        let spec = game.watcher(&ctx);
        assert_eq!(
            spec.scope.under_path.as_deref(),
            Some(ctx.dirs.prefix.as_path())
        );
    }

    #[test]
    fn watch_exes_default_when_empty() {
        let game = SkyrimModded::from_definition(&test_definition()).unwrap();
        let hint = game.watch_hint().unwrap();
        match hint {
            WatchHint::ExeNames(names) => {
                assert!(names.iter().any(|n| n == "skse64_loader.exe"));
                assert!(names.iter().any(|n| n == "SkyrimSE.exe"));
            }
            _ => panic!("expected ExeNames"),
        }
    }
}
