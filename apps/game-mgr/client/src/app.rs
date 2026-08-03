//! egui shell. Deliberately thin: all state lives in `gm-core`, this module
//! only renders the latest snapshot and forwards commands (PLAN.md §6.1).

use eframe::egui::{self, Color32, RichText};
use game_mgr_api_types::ArtifactRole;
use game_mgr_core::config::ClientConfig;
use game_mgr_core::core::{
    CoreCmd, CoreHandle, DraftArtifact, GameDraft, GameState, GameView, Snapshot,
};
use game_mgr_core::game::{DlcSelection, InstallOptions, LaunchOpts};
use game_mgr_core::scan::SuggestedRole;

/// Role choice in the picker — `Ignore` rows are not submitted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum RoleChoice {
    #[default]
    Base,
    Patch,
    Dlc,
    Ignore,
}

impl RoleChoice {
    fn label(self) -> &'static str {
        match self {
            RoleChoice::Base => "base",
            RoleChoice::Patch => "patch",
            RoleChoice::Dlc => "dlc",
            RoleChoice::Ignore => "ignore",
        }
    }

    fn from_suggested(s: SuggestedRole) -> Self {
        match s {
            SuggestedRole::Base => RoleChoice::Base,
            SuggestedRole::Patch => RoleChoice::Patch,
            SuggestedRole::Dlc => RoleChoice::Dlc,
            SuggestedRole::Ignore => RoleChoice::Ignore,
        }
    }

    fn from_role(r: ArtifactRole) -> Self {
        match r {
            ArtifactRole::Base => RoleChoice::Base,
            ArtifactRole::Patch => RoleChoice::Patch,
            ArtifactRole::Dlc => RoleChoice::Dlc,
        }
    }

    fn as_role(self) -> Option<ArtifactRole> {
        match self {
            RoleChoice::Base => Some(ArtifactRole::Base),
            RoleChoice::Patch => Some(ArtifactRole::Patch),
            RoleChoice::Dlc => Some(ArtifactRole::Dlc),
            RoleChoice::Ignore => None,
        }
    }
}

const ALL_ROLES: [RoleChoice; 4] = [
    RoleChoice::Base,
    RoleChoice::Patch,
    RoleChoice::Dlc,
    RoleChoice::Ignore,
];

/// Human-readable byte size (binary units).
fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Compact ETA from seconds, e.g. `5m 20s`.
fn fmt_eta(secs: f64) -> String {
    let secs = secs.max(0.0) as u64;
    if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Last path segment of a bucket key, for compact display in pickers.
fn filename(bucket_key: &str) -> String {
    bucket_key
        .rsplit('/')
        .next()
        .unwrap_or(bucket_key)
        .to_string()
}

/// Best-effort DLC name to pre-fill the picker: the folder after a `dlc/`
/// segment (`…/dlc/<name>/setup.exe`), else the installer's file stem. The
/// user can always override it.
fn derive_dlc_name(bucket_key: &str) -> String {
    let segments: Vec<&str> = bucket_key.split('/').filter(|s| !s.is_empty()).collect();
    if let Some(pos) = segments.iter().position(|s| s.eq_ignore_ascii_case("dlc"))
        && pos + 1 < segments.len().saturating_sub(1)
    {
        return segments[pos + 1].to_string();
    }
    let file = segments.last().copied().unwrap_or(bucket_key);
    file.rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(file)
        .to_string()
}

/// One file row in the picker.
struct ArtifactRow {
    bucket_key: String,
    size: Option<i64>,
    sha256: Option<String>,
    role: RoleChoice,
    /// DLC name, editable only when `role == Dlc`.
    dlc_name: String,
    /// Selected for a bulk role assignment.
    selected: bool,
}

/// Form state for the Add/Edit Game window.
#[derive(Default)]
struct GameDraftForm {
    editing: bool, // true = id locked (editing an existing definition)
    id: String,
    title: String,
    version: String,
    class: String,
    bucket_prefix: String,
    artifacts: Vec<ArtifactRow>,
    /// Marks which scan result the artifact rows came from.
    scan_seen: Option<String>,
    /// Role applied by the "set selected to…" bulk control.
    bulk_role: RoleChoice,
    /// Anchor row for shift-range selection.
    last_clicked: Option<usize>,
    /// Show rows whose role is `ignore` (hidden by default).
    show_ignored: bool,
    // gog fields
    umu_id: String,
    exe_rel: String,
    watch_exes: String, // comma separated
    saves_in_prefix: String,
    proton_default: String,
    // skyrim-modded fields
    skse_key: String, // bucket key of the SKSE archive (picked from the scan)
    game_path_in_prefix: String, // game install path on the prefix's C: drive
    sync_root_in_prefix: String, // C: base path holding the synced folders
    mo2_exe_rel: String, // ModOrganizer.exe relative to the C: sync root
}

impl GameDraftForm {
    fn new() -> Self {
        Self {
            version: "1.0.0".into(),
            class: "gog".into(),
            ..Self::default()
        }
    }

    fn for_edit(game: &GameView) -> Self {
        let def = &game.definition;
        let cfg = &def.config;
        let join = |key: &str| -> String {
            cfg.get(key)
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default()
        };
        let text = |key: &str| -> String {
            cfg.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        Self {
            editing: true,
            id: def.id.clone(),
            title: def.title.clone(),
            version: def.version.clone(),
            class: def.class.clone(),
            bucket_prefix: String::new(),
            artifacts: def
                .artifacts
                .iter()
                .map(|a| ArtifactRow {
                    bucket_key: a.bucket_key.clone(),
                    size: a.size,
                    sha256: Some(a.sha256.clone()),
                    role: RoleChoice::from_role(a.role),
                    dlc_name: a.dlc_name.clone().unwrap_or_default(),
                    selected: false,
                })
                .collect(),
            scan_seen: None,
            bulk_role: RoleChoice::Base,
            last_clicked: None,
            show_ignored: false,
            umu_id: text("umu_id"),
            exe_rel: text("exe_rel"),
            watch_exes: join("watch_exes"),
            saves_in_prefix: text("saves_in_prefix"),
            proton_default: text("proton_default"),
            skse_key: text("skse_key"),
            game_path_in_prefix: text("game_path_in_prefix"),
            sync_root_in_prefix: text("sync_root_in_prefix"),
            mo2_exe_rel: text("mo2_exe_rel"),
        }
    }

    /// Pull freshly scanned files into the rows (replacing them).
    fn absorb_scan(&mut self, scan: &game_mgr_core::core::ScanResult) {
        if self.scan_seen.as_deref() == Some(scan.prefix.as_str()) {
            return;
        }
        self.scan_seen = Some(scan.prefix.clone());
        self.artifacts = scan
            .files
            .iter()
            .map(|f| {
                let role = RoleChoice::from_suggested(f.suggested);
                ArtifactRow {
                    dlc_name: if role == RoleChoice::Dlc {
                        derive_dlc_name(&f.bucket_key)
                    } else {
                        String::new()
                    },
                    bucket_key: f.bucket_key.clone(),
                    size: Some(f.size),
                    sha256: f.sha256.clone(),
                    role,
                    selected: false,
                }
            })
            .collect();
    }

    fn validate(&self) -> Result<GameDraft, String> {
        let id = self.id.trim();
        if id.is_empty()
            || id.len() > 64
            || !id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(
                "id must be a slug: lowercase letters, digits, dashes (e.g. \
                        baldurs-gate-3)"
                    .into(),
            );
        }
        if self.title.trim().is_empty() {
            return Err("title is required".into());
        }
        if semver::Version::parse(self.version.trim()).is_err() {
            return Err("version must be semver, e.g. 1.0.0".into());
        }
        // every DLC row must carry a name so installs can list it distinctly
        for row in &self.artifacts {
            if row.role == RoleChoice::Dlc && row.dlc_name.trim().is_empty() {
                return Err(format!(
                    "{} is marked DLC but has no name — give each DLC a name",
                    row.bucket_key
                ));
            }
        }
        let artifacts: Vec<DraftArtifact> = self
            .artifacts
            .iter()
            .filter_map(|row| {
                row.role.as_role().map(|role| DraftArtifact {
                    bucket_key: row.bucket_key.clone(),
                    size: row.size,
                    sha256: row.sha256.clone(),
                    role,
                    dlc_name: (role == ArtifactRole::Dlc).then(|| row.dlc_name.trim().to_string()),
                })
            })
            .collect();
        if artifacts.is_empty() {
            return Err("no files selected — scan a bucket prefix and assign roles".into());
        }

        let config = match self.class.as_str() {
            "gog" => {
                let base_exes = artifacts
                    .iter()
                    .filter(|a| {
                        a.role == ArtifactRole::Base
                            && a.bucket_key.to_lowercase().ends_with(".exe")
                    })
                    .count();
                if base_exes != 1 {
                    return Err(format!(
                        "the base group needs exactly one .exe installer (found {base_exes}) — \
                         adjust the roles"
                    ));
                }
                if let Some(bad) = artifacts.iter().find(|a| {
                    a.role == ArtifactRole::Base && {
                        let k = a.bucket_key.to_lowercase();
                        !k.ends_with(".exe") && !k.ends_with(".bin")
                    }
                }) {
                    return Err(format!(
                        "{} can't be a base file (only .exe/.bin work with the Proton \
                         pipeline) — set it to ignore",
                        bad.bucket_key
                    ));
                }
                let watch_exes: Vec<String> = self
                    .watch_exes
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                let mut config = serde_json::json!({ "watch_exes": watch_exes });
                // optional fields: omit when blank so they deserialize to None
                for (key, value) in [
                    ("umu_id", &self.umu_id),
                    ("exe_rel", &self.exe_rel),
                    ("saves_in_prefix", &self.saves_in_prefix),
                    ("proton_default", &self.proton_default),
                ] {
                    if !value.trim().is_empty() {
                        config[key] = serde_json::json!(value.trim());
                    }
                }
                config
            }
            "skyrim-modded" => {
                let skse = self.skse_key.trim();
                // exactly one GOG .exe among the base parts that aren't the SKSE archive
                let gog_base_exes = artifacts
                    .iter()
                    .filter(|a| {
                        a.role == ArtifactRole::Base
                            && a.bucket_key != skse
                            && a.bucket_key.to_lowercase().ends_with(".exe")
                    })
                    .count();
                if gog_base_exes != 1 {
                    return Err(format!(
                        "the base group needs exactly one GOG .exe installer (found \
                         {gog_base_exes}) — adjust the roles or pick the SKSE archive separately"
                    ));
                }
                if let Some(bad) = artifacts.iter().find(|a| {
                    a.role == ArtifactRole::Base && a.bucket_key != skse && {
                        let k = a.bucket_key.to_lowercase();
                        !k.ends_with(".exe") && !k.ends_with(".bin")
                    }
                }) {
                    return Err(format!(
                        "{} can't be a GOG base file (only .exe/.bin work) — set it to ignore \
                         or pick it as the SKSE archive",
                        bad.bucket_key
                    ));
                }
                if !skse.is_empty() && !artifacts.iter().any(|a| a.bucket_key == skse) {
                    return Err(
                        "the selected SKSE file isn't in the artifact list — give it a role \
                         (e.g. base) so it's included"
                            .into(),
                    );
                }
                let watch_exes: Vec<String> = self
                    .watch_exes
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                let mut config = serde_json::json!({ "watch_exes": watch_exes });
                for (key, value) in [
                    ("umu_id", &self.umu_id),
                    ("proton_default", &self.proton_default),
                    ("skse_key", &self.skse_key),
                    ("game_path_in_prefix", &self.game_path_in_prefix),
                    ("sync_root_in_prefix", &self.sync_root_in_prefix),
                    ("mo2_exe_rel", &self.mo2_exe_rel),
                ] {
                    if !value.trim().is_empty() {
                        config[key] = serde_json::json!(value.trim());
                    }
                }
                config
            }
            other => return Err(format!("unknown class {other}")),
        };
        Ok(GameDraft {
            id: id.to_string(),
            title: self.title.trim().to_string(),
            version: self.version.trim().to_string(),
            class: self.class.clone(),
            config,
            artifacts,
        })
    }
}

pub struct App {
    core: CoreHandle,
    show_profiles: bool,
    new_profile_name: String,
    confirm_delete_profile: Option<uuid::Uuid>,
    show_sessions: bool,
    show_game_form: bool,
    draft: GameDraftForm,
    draft_error: Option<String>,
    /// Pending install-options dialog for a game with optional groups.
    install_dialog: Option<(String, InstallOptions)>,
    /// Last error shown in the prominent dialog; `None` once dismissed.
    shown_error: Option<String>,
    /// Per-game Settings window: the game id it's open for, plus edit buffers.
    settings_for: Option<String>,
    settings_draft: LaunchOpts,
    settings_proton: String,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = ClientConfig::load().unwrap_or_else(|err| {
            // `?err` (Debug), not `%err` (Display): `ClientConfig::load`'s
            // error is an `anyhow::Error` built from stacked `.context()`
            // calls (`reading <path>` / `parsing <path>`) wrapping the
            // real `toml::de::Error` -- anyhow's `Display` only shows the
            // outermost context ("parsing <path>"), silently swallowing
            // the actual cause (e.g. which field, line, and column
            // `deny_unknown_fields` rejected). `Debug` prints the whole
            // chain, which is the only way this warning is ever
            // actionable.
            tracing::warn!(?err, "failed to load config, using defaults");
            ClientConfig::default()
        });
        let ctx = cc.egui_ctx.clone();
        let core = CoreHandle::start(config, Box::new(move || ctx.request_repaint()));
        Self {
            core,
            show_profiles: false,
            new_profile_name: String::new(),
            confirm_delete_profile: None,
            show_sessions: false,
            show_game_form: false,
            draft: GameDraftForm::new(),
            draft_error: None,
            install_dialog: None,
            shown_error: None,
            settings_for: None,
            settings_draft: LaunchOpts::default(),
            settings_proton: String::new(),
        }
    }

    /// Open `path` in the system file manager (best-effort, Linux `xdg-open`).
    fn open_path(path: &std::path::Path) {
        if let Err(err) = std::process::Command::new("xdg-open").arg(path).spawn() {
            tracing::warn!(%err, path = %path.display(), "failed to open path");
        }
    }

    /// Native folder picker for the saves directory, starting inside the game's
    /// prefix; returns the choice relative to the prefix when possible.
    fn pick_saves_dir(snapshot: &Snapshot, game_id: &str) -> Option<String> {
        let prefix = snapshot
            .games
            .iter()
            .find(|g| g.id == game_id)
            .map(|g| g.prefix.clone());
        let mut dialog = rfd::FileDialog::new();
        if let Some(p) = &prefix
            && p.is_dir()
        {
            dialog = dialog.set_directory(p);
        }
        let chosen = dialog.pick_folder()?;
        match &prefix {
            Some(p) => match chosen.strip_prefix(p) {
                Ok(rel) => Some(rel.to_string_lossy().replace('\\', "/")),
                Err(_) => Some(chosen.to_string_lossy().into_owned()),
            },
            None => Some(chosen.to_string_lossy().into_owned()),
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        ui.horizontal(|ui| {
            ui.heading("game-mgr");
            ui.separator();

            // pure selection — management lives in its own window, because
            // egui combo popups close on any interior click
            let account = &snapshot.account;
            let active_name = account
                .active_profile
                .and_then(|id| account.profiles.iter().find(|p| p.id == id))
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "no profile".into());
            egui::ComboBox::from_id_salt("profile-picker")
                .selected_text(format!("👤 {active_name}"))
                .show_ui(ui, |ui| {
                    for profile in &account.profiles {
                        let selected = account.active_profile == Some(profile.id);
                        if ui.selectable_label(selected, &profile.name).clicked() {
                            self.core.send(CoreCmd::SelectProfile(profile.id));
                        }
                    }
                });
            if ui
                .button("Profiles…")
                .on_hover_text("create, delete or switch profiles")
                .clicked()
            {
                self.show_profiles = !self.show_profiles;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match &snapshot.server_url {
                    Some(url) => {
                        let color = if snapshot.account.server_reachable {
                            Color32::DARK_GREEN
                        } else {
                            Color32::DARK_RED
                        };
                        ui.label(RichText::new(url).color(color));
                    }
                    None => {
                        ui.label(RichText::new("server not configured").weak());
                    }
                }
                if !snapshot.account.logged_in {
                    let signing_in = snapshot.activity.is_some();
                    if ui
                        .add_enabled(!signing_in, egui::Button::new("Sign in"))
                        .on_hover_text("opens your browser to sign in")
                        .clicked()
                    {
                        self.core.send(CoreCmd::Login);
                    }
                }
                if ui.button("Sessions").clicked() {
                    self.show_sessions = !self.show_sessions;
                    self.core.send(CoreCmd::RefreshSessions);
                }
                if ui.button("➕ Add game").clicked() {
                    self.draft = GameDraftForm::new();
                    self.draft_error = None;
                    self.show_game_form = true;
                }
                if ui
                    .button("⟳")
                    .on_hover_text("refresh game catalog")
                    .clicked()
                {
                    self.core.send(CoreCmd::RefreshLibrary);
                }
            });
        });
    }

    fn profiles_window(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        if !self.show_profiles {
            return;
        }
        let mut open = self.show_profiles;
        egui::Window::new("Profiles")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                let account = &snapshot.account;
                if account.profiles.is_empty() {
                    ui.label(
                        RichText::new("No profiles yet — create one below to start playing.")
                            .weak(),
                    );
                }
                for profile in &account.profiles {
                    ui.horizontal(|ui| {
                        let active = account.active_profile == Some(profile.id);
                        if ui
                            .selectable_label(active, format!("👤 {}", profile.name))
                            .on_hover_text("make active")
                            .clicked()
                        {
                            self.core.send(CoreCmd::SelectProfile(profile.id));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if self.confirm_delete_profile == Some(profile.id) {
                                if ui
                                    .button(RichText::new("Really delete").color(Color32::WHITE))
                                    .on_hover_text(
                                        "deletes the profile AND all sessions recorded on it",
                                    )
                                    .clicked()
                                {
                                    self.core.send(CoreCmd::DeleteProfile(profile.id));
                                    self.confirm_delete_profile = None;
                                }
                                if ui.button("Cancel").clicked() {
                                    self.confirm_delete_profile = None;
                                }
                            } else if ui
                                .button("🗑")
                                .on_hover_text("delete this profile and its sessions")
                                .clicked()
                            {
                                self.confirm_delete_profile = Some(profile.id);
                            }
                        });
                    });
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.new_profile_name)
                            .hint_text("new profile name"),
                    );
                    let submitted =
                        edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (ui.button("Create").clicked() || submitted)
                        && !self.new_profile_name.trim().is_empty()
                    {
                        self.core
                            .send(CoreCmd::CreateProfile(self.new_profile_name.trim().into()));
                        self.new_profile_name.clear();
                    }
                });
            });
        self.show_profiles = open;
    }

    fn game_form_window(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        if !self.show_game_form {
            return;
        }
        // pull in scan results that arrived since the last frame
        if let Some(scan) = &snapshot.scan {
            self.draft.absorb_scan(scan);
        }

        let mut open = self.show_game_form;
        let title = if self.draft.editing {
            format!("Edit game — {}", self.draft.id)
        } else {
            "Add game".to_string()
        };
        egui::Window::new(title)
            .open(&mut open)
            .default_width(620.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Upload files (with .sha256 sidecars) to the bucket, scan their \
                         prefix, classify each file, submit. Hashes come from the sidecars \
                         — registration is instant (docs/game-mgr-buckets.md).",
                    )
                    .weak(),
                );
                ui.add_space(6.0);

                egui::Grid::new("draft-grid").num_columns(4).show(ui, |ui| {
                    ui.label("class");
                    egui::ComboBox::from_id_salt("draft-class")
                        .selected_text(&self.draft.class)
                        .show_ui(ui, |ui| {
                            for class in snapshot.known_classes {
                                ui.selectable_value(
                                    &mut self.draft.class,
                                    class.to_string(),
                                    *class,
                                );
                            }
                        });
                    ui.label("id (slug)");
                    ui.add_enabled(
                        !self.draft.editing,
                        egui::TextEdit::singleline(&mut self.draft.id).hint_text("baldurs-gate-3"),
                    );
                    ui.end_row();

                    ui.label("title");
                    ui.text_edit_singleline(&mut self.draft.title);
                    ui.label("version");
                    ui.text_edit_singleline(&mut self.draft.version);
                    ui.end_row();
                });

                if self.draft.class == "gog" {
                    egui::Grid::new("gog-grid").num_columns(4).show(ui, |ui| {
                        ui.label("umu id (opt.)");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.umu_id)
                                .hint_text("umu-1086940")
                                .desired_width(220.0),
                        )
                        .on_hover_text("optional — leave blank if the title has no umu entry");
                        ui.label("executable (opt.)");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.exe_rel)
                                .hint_text("app/bin/bg3.exe")
                                .desired_width(220.0),
                        )
                        .on_hover_text(
                            "optional — leave blank to pick from the detected .exe list at \
                             first launch",
                        );
                        ui.end_row();
                        ui.label("watch exes");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.watch_exes)
                                .hint_text("bg3.exe, bg3_dx11.exe")
                                .desired_width(220.0),
                        );
                        ui.label("proton (opt.)");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.proton_default)
                                .hint_text("GE-Proton9-20")
                                .desired_width(220.0),
                        );
                        ui.end_row();
                        ui.label("saves in prefix (opt.)");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.saves_in_prefix)
                                .hint_text("drive_c/users/steamuser/AppData/…")
                                .desired_width(420.0),
                        )
                        .on_hover_text("optional — leave blank to skip saves syncing");
                        if ui
                            .button("Browse…")
                            .on_hover_text(
                                "pick the saves folder on disk (e.g. inside the prefix) instead \
                                 of typing the path",
                            )
                            .clicked()
                            && let Some(dir) = Self::pick_saves_dir(snapshot, &self.draft.id)
                        {
                            self.draft.saves_in_prefix = dir;
                        }
                        ui.end_row();
                    });
                }

                if self.draft.class == "skyrim-modded" {
                    ui.label(
                        RichText::new(
                            "GOG Skyrim + SKSE install per-machine on the prefix's C: drive; \
                             the Archive / Data / 'Skyrim MO2' folders sync via Syncthing under \
                             a C: base path. Everything is on C: so MO2's paths stay portable \
                             (point MO2 at the C:\\ locations). Tag the GOG installer parts as \
                             base and pick the SKSE archive below.",
                        )
                        .small()
                        .weak(),
                    );
                    egui::Grid::new("skyrim-grid")
                        .num_columns(4)
                        .show(ui, |ui| {
                            ui.label("umu id (opt.)");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.draft.umu_id)
                                    .hint_text("umu-489830")
                                    .desired_width(220.0),
                            );
                            ui.label("proton (opt.)");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.draft.proton_default)
                                    .hint_text("GE-Proton9-20")
                                    .desired_width(220.0),
                            );
                            ui.end_row();

                            ui.label("SKSE archive");
                            let skse_label = if self.draft.skse_key.is_empty() {
                                "(none)".to_string()
                            } else {
                                filename(&self.draft.skse_key)
                            };
                            egui::ComboBox::from_id_salt("skyrim-skse")
                                .selected_text(skse_label)
                                .width(220.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.draft.skse_key,
                                        String::new(),
                                        "(none)",
                                    );
                                    for row in &self.draft.artifacts {
                                        let name = filename(&row.bucket_key);
                                        ui.selectable_value(
                                            &mut self.draft.skse_key,
                                            row.bucket_key.clone(),
                                            name,
                                        );
                                    }
                                });
                            ui.label("watch exes");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.draft.watch_exes)
                                    .hint_text("SkyrimSE.exe, skse64_loader.exe")
                                    .desired_width(220.0),
                            )
                            .on_hover_text(
                                "optional — sensible Skyrim/SKSE defaults are used when blank",
                            );
                            ui.end_row();

                            ui.label("game path (C:)");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.draft.game_path_in_prefix)
                                    .hint_text("GOG Games/Skyrim Anniversary Edition")
                                    .desired_width(220.0),
                            )
                            .on_hover_text(
                                "where GOG + SKSE install on the prefix's C: drive — set it to \
                             match MO2's gamePath (default GOG Games/Skyrim Anniversary Edition)",
                            );
                            ui.label("sync root (C:)");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.draft.sync_root_in_prefix)
                                    .hint_text("game-mgr/skyrim")
                                    .desired_width(220.0),
                            )
                            .on_hover_text(
                                "C: base path holding Archive/Data/Skyrim MO2 — point MO2 here \
                             (default game-mgr/<id>)",
                            );
                            ui.label("MO2 exe (opt.)");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.draft.mo2_exe_rel)
                                    .hint_text("Skyrim MO2/ModOrganizer.exe")
                                    .desired_width(220.0),
                            )
                            .on_hover_text(
                                "relative to the C: sync root — launcher path \
                             (default Skyrim MO2/ModOrganizer.exe)",
                            );
                            ui.end_row();
                        });
                }

                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("bucket prefix");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.bucket_prefix)
                            .hint_text("gog/baldurs_gate_iii/"),
                    );
                    if ui.button("Scan").clicked() && !self.draft.bucket_prefix.trim().is_empty() {
                        self.draft.scan_seen = None;
                        self.core
                            .send(CoreCmd::ScanPrefix(self.draft.bucket_prefix.trim().into()));
                    }
                });

                if !self.draft.artifacts.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "base installs always; patch/dlc are chosen at install time; \
                             ignore = not part of this game. Tick rows (shift-click to select a \
                             range) and bulk-assign a role.",
                        )
                        .small()
                        .weak(),
                    );

                    let show_ignored = self.draft.show_ignored;
                    let selected_count = self.draft.artifacts.iter().filter(|r| r.selected).count();
                    let hidden_count = self
                        .draft
                        .artifacts
                        .iter()
                        .filter(|r| r.role == RoleChoice::Ignore && !show_ignored)
                        .count();

                    // bulk-assignment bar
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{selected_count} selected")).weak());
                        if ui.button("Select all").clicked() {
                            for row in &mut self.draft.artifacts {
                                if show_ignored || row.role != RoleChoice::Ignore {
                                    row.selected = true;
                                }
                            }
                        }
                        if ui.button("Select none").clicked() {
                            for row in &mut self.draft.artifacts {
                                row.selected = false;
                            }
                        }
                        ui.separator();
                        egui::ComboBox::from_id_salt("bulk-role")
                            .selected_text(self.draft.bulk_role.label())
                            .show_ui(ui, |ui| {
                                for choice in ALL_ROLES {
                                    ui.selectable_value(
                                        &mut self.draft.bulk_role,
                                        choice,
                                        choice.label(),
                                    );
                                }
                            });
                        if ui
                            .button("Set selected")
                            .on_hover_text("apply the role to every ticked file")
                            .clicked()
                        {
                            let bulk = self.draft.bulk_role;
                            for row in &mut self.draft.artifacts {
                                if row.selected {
                                    row.role = bulk;
                                    if bulk == RoleChoice::Dlc && row.dlc_name.is_empty() {
                                        row.dlc_name = derive_dlc_name(&row.bucket_key);
                                    }
                                }
                            }
                        }
                        ui.separator();
                        ui.checkbox(&mut self.draft.show_ignored, "show ignored");
                        if hidden_count > 0 {
                            ui.label(RichText::new(format!("({hidden_count} hidden)")).weak());
                        }
                    });

                    let mut pending_click: Option<(usize, bool)> = None;
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .show(ui, |ui| {
                            egui::Grid::new("artifact-grid")
                                .striped(true)
                                .num_columns(5)
                                .show(ui, |ui| {
                                    for (index, row) in self.draft.artifacts.iter_mut().enumerate()
                                    {
                                        if row.role == RoleChoice::Ignore && !show_ignored {
                                            continue;
                                        }
                                        let resp = ui.checkbox(&mut row.selected, "");
                                        if resp.clicked() {
                                            pending_click =
                                                Some((index, ui.input(|i| i.modifiers.shift)));
                                        }
                                        let name = row
                                            .bucket_key
                                            .rsplit('/')
                                            .next()
                                            .unwrap_or(&row.bucket_key);
                                        ui.label(name).on_hover_text(&row.bucket_key);
                                        ui.label(
                                            RichText::new(match row.size {
                                                Some(size) => {
                                                    format!(
                                                        "{:.1} GiB",
                                                        size as f64 / (1 << 30) as f64
                                                    )
                                                }
                                                None => "?".into(),
                                            })
                                            .weak(),
                                        );
                                        if row.sha256.is_some() {
                                            ui.label(RichText::new("sidecar ✓").weak());
                                        } else {
                                            ui.label(
                                                RichText::new("no sidecar — will stream-hash")
                                                    .color(Color32::ORANGE),
                                            )
                                            .on_hover_text(
                                                "upload a <file>.sha256 next to it for instant \
                                                 registration",
                                            );
                                        }
                                        ui.horizontal(|ui| {
                                            egui::ComboBox::from_id_salt(("role", index))
                                                .selected_text(row.role.label())
                                                .show_ui(ui, |ui| {
                                                    for choice in ALL_ROLES {
                                                        ui.selectable_value(
                                                            &mut row.role,
                                                            choice,
                                                            choice.label(),
                                                        );
                                                    }
                                                });
                                            if row.role == RoleChoice::Dlc {
                                                ui.add(
                                                    egui::TextEdit::singleline(&mut row.dlc_name)
                                                        .hint_text("DLC name")
                                                        .desired_width(150.0),
                                                )
                                                .on_hover_text(
                                                    "names this DLC so it gets its own \
                                                     install checkbox",
                                                );
                                            }
                                        });
                                        ui.end_row();
                                    }
                                });
                        });

                    // apply shift-range selection after the row loop (can't
                    // touch sibling rows from inside `iter_mut`)
                    if let Some((index, shift)) = pending_click {
                        if shift && let Some(anchor) = self.draft.last_clicked {
                            let (lo, hi) = (anchor.min(index), anchor.max(index));
                            for row in &mut self.draft.artifacts[lo..=hi] {
                                row.selected = true;
                            }
                        } else {
                            self.draft.last_clicked = Some(index);
                        }
                    }
                }

                ui.add_space(8.0);
                if let Some(activity) = &snapshot.activity {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(activity);
                    });
                } else {
                    let submit_label = if self.draft.editing {
                        "Save changes"
                    } else {
                        "Submit game"
                    };
                    if ui.button(submit_label).clicked() {
                        match self.draft.validate() {
                            Ok(draft) => {
                                self.draft_error = None;
                                self.core.send(CoreCmd::SubmitGame(draft));
                            }
                            Err(err) => self.draft_error = Some(err),
                        }
                    }
                }
                if let Some(err) = &self.draft_error {
                    ui.label(RichText::new(err).color(Color32::DARK_RED));
                }
            });
        self.show_game_form = open;
    }

    fn install_options_window(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        let Some((game_id, mut options)) = self.install_dialog.clone() else {
            return;
        };
        let Some(game) = snapshot.games.iter().find(|g| g.id == game_id) else {
            self.install_dialog = None;
            return;
        };
        let has_patches = game
            .definition
            .artifacts
            .iter()
            .any(|a| a.role == ArtifactRole::Patch);
        // distinct DLC names, in first-seen order
        let mut dlc_names: Vec<String> = Vec::new();
        for a in &game.definition.artifacts {
            if a.role == ArtifactRole::Dlc {
                let name = a
                    .dlc_name
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| derive_dlc_name(&a.bucket_key));
                if !dlc_names.contains(&name) {
                    dlc_names.push(name);
                }
            }
        }
        // working set of selected DLC names (All ⇒ everything checked)
        let mut selected: std::collections::BTreeSet<String> = match &options.dlc {
            DlcSelection::All => dlc_names.iter().cloned().collect(),
            DlcSelection::Named(set) => set.clone(),
        };

        let mut close = false;
        egui::Window::new(format!("Install {}", game.title))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("The base game always installs. Optional groups:");
                if has_patches {
                    ui.checkbox(&mut options.include_patches, "apply patch installers");
                }
                for name in &dlc_names {
                    let mut on = selected.contains(name);
                    if ui.checkbox(&mut on, format!("DLC: {name}")).changed() {
                        if on {
                            selected.insert(name.clone());
                        } else {
                            selected.remove(name);
                        }
                    }
                }
                options.dlc = DlcSelection::Named(selected);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Install").clicked() {
                        self.core.send(CoreCmd::Install {
                            game_id: game_id.clone(),
                            options: options.clone(),
                        });
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        if close {
            self.install_dialog = None;
        } else {
            self.install_dialog = Some((game_id, options));
        }
    }

    fn request_install(&mut self, game: &GameView) {
        let has_optional = game
            .definition
            .artifacts
            .iter()
            .any(|a| a.role != ArtifactRole::Base);
        if has_optional {
            self.install_dialog = Some((
                game.id.clone(),
                game.installed_options.clone().unwrap_or_default(),
            ));
        } else {
            self.core.send(CoreCmd::Install {
                game_id: game.id.clone(),
                options: InstallOptions::default(),
            });
        }
    }

    fn game_row(&mut self, ui: &mut egui::Ui, game: &GameView) {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&game.title).strong().size(16.0));
                        ui.label(
                            RichText::new(format!("({}, v{})", game.class, game.version)).weak(),
                        );
                        if game.playing {
                            ui.label(RichText::new("▶ playing").color(Color32::DARK_GREEN));
                        }
                    });
                    match &game.state {
                        GameState::NotInstalled => {
                            ui.label(RichText::new("not installed").weak());
                        }
                        GameState::Installing { step_label, detail } => {
                            ui.label(format!("⏳ {step_label} {detail}"));
                            if let Some(dl) = &game.download
                                && dl.total > 0
                            {
                                let frac = (dl.done as f32 / dl.total as f32).clamp(0.0, 1.0);
                                let speed = if dl.speed_bps > 0.0 {
                                    format!(" · {}/s", fmt_bytes(dl.speed_bps as u64))
                                } else {
                                    String::new()
                                };
                                let eta = if dl.speed_bps > 1.0 && dl.done < dl.total {
                                    let secs = (dl.total - dl.done) as f64 / dl.speed_bps;
                                    format!(" · ETA {}", fmt_eta(secs))
                                } else {
                                    String::new()
                                };
                                ui.add(egui::ProgressBar::new(frac).text(format!(
                                    "{} / {}{speed}{eta}",
                                    fmt_bytes(dl.done),
                                    fmt_bytes(dl.total),
                                )));
                            }
                        }
                        GameState::ManualWait { .. } => {
                            ui.label(
                                RichText::new("waiting for you — open the wizard")
                                    .color(Color32::ORANGE),
                            );
                        }
                        GameState::Installed => {
                            ui.label(RichText::new("installed").color(Color32::DARK_GREEN));
                        }
                        GameState::Outdated => {
                            ui.label(RichText::new("update available").color(Color32::ORANGE));
                        }
                        GameState::Failed { error } => {
                            ui.label(
                                RichText::new(format!("failed: {error}")).color(Color32::DARK_RED),
                            );
                        }
                    }
                    // surface where the files live so they can be inspected by
                    // hand — the prefix is under XDG data, not the install dir.
                    if matches!(
                        game.state,
                        GameState::Installed | GameState::Outdated | GameState::Failed { .. }
                    ) {
                        ui.horizontal(|ui| {
                            if ui
                                .small_button("📁 prefix")
                                .on_hover_text(game.prefix.display().to_string())
                                .clicked()
                            {
                                Self::open_path(&game.prefix);
                            }
                            if ui
                                .small_button("📁 install")
                                .on_hover_text(game.install_root.display().to_string())
                                .clicked()
                            {
                                Self::open_path(&game.install_root);
                            }
                            if ui
                                .small_button("⧉")
                                .on_hover_text("copy prefix path")
                                .clicked()
                            {
                                ui.ctx().copy_text(game.prefix.display().to_string());
                            }
                        });
                        ui.label(
                            RichText::new(format!("prefix: {}", game.prefix.display()))
                                .small()
                                .weak(),
                        );
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button("✏")
                        .on_hover_text("edit this game's definition")
                        .clicked()
                    {
                        self.draft = GameDraftForm::for_edit(game);
                        self.draft_error = None;
                        self.show_game_form = true;
                    }
                    if matches!(
                        game.state,
                        GameState::Installed | GameState::Outdated | GameState::Failed { .. }
                    ) && ui
                        .button("⚙")
                        .on_hover_text("MangoHud, Gamescope, exe favourites, Proton")
                        .clicked()
                    {
                        self.settings_for = Some(game.id.clone());
                        self.settings_draft = game.launch_opts.clone();
                        self.settings_proton = game.proton_override.clone().unwrap_or_default();
                        self.core.send(CoreCmd::ScanExes(game.id.clone()));
                    }
                    match &game.state {
                        GameState::NotInstalled | GameState::Failed { .. } => {
                            if ui.button("Install").clicked() {
                                self.request_install(game);
                            }
                        }
                        GameState::Installing { .. } => {
                            if ui.button("Cancel").clicked() {
                                self.core.send(CoreCmd::CancelInstall(game.id.clone()));
                            }
                        }
                        GameState::ManualWait { .. } => { /* wizard window below */ }
                        GameState::Installed => {
                            if !game.playing && ui.button("Play").clicked() {
                                self.core.send(CoreCmd::Launch(game.id.clone()));
                            }
                            if ui.button("Uninstall").clicked() {
                                self.core.send(CoreCmd::Uninstall(game.id.clone()));
                            }
                        }
                        GameState::Outdated => {
                            if ui.button("Update (reinstall)").clicked() {
                                self.core.send(CoreCmd::Uninstall(game.id.clone()));
                                self.request_install(game);
                            }
                        }
                    }
                });
            });
        });
    }

    fn wizard_windows(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        for game in &snapshot.games {
            if let GameState::ManualWait {
                step_id,
                instructions_md,
            } = &game.state
            {
                egui::Window::new(format!("{} — manual step", game.title))
                    .collapsible(false)
                    .show(ctx, |ui| {
                        ui.label(instructions_md);
                        ui.add_space(8.0);
                        if ui.button("I did this — verify").clicked() {
                            self.core.send(CoreCmd::ConfirmManual {
                                game_id: game.id.clone(),
                                step_id: step_id.clone(),
                            });
                        }
                    });
            }
        }
    }

    fn sessions_window(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        if !self.show_sessions {
            return;
        }
        egui::Window::new("Recent sessions")
            .default_width(560.0)
            .open(&mut self.show_sessions)
            .show(ctx, |ui| {
                if snapshot.recent_sessions.is_empty() {
                    ui.label(RichText::new("no sessions on the server yet").weak());
                    return;
                }
                egui::Grid::new("sessions-grid")
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label(RichText::new("game").strong());
                        ui.label(RichText::new("started").strong());
                        ui.label(RichText::new("duration").strong());
                        ui.label(RichText::new("exit").strong());
                        ui.end_row();
                        for session in &snapshot.recent_sessions {
                            ui.label(&session.game_id);
                            ui.label(
                                session
                                    .started_at
                                    .format(&time::format_description::well_known::Rfc3339)
                                    .unwrap_or_default(),
                            );
                            let mins = session.duration_s / 60;
                            ui.label(format!("{}h {:02}m", mins / 60, mins % 60));
                            match session.exit_code {
                                Some(0) => ui.label(RichText::new("ok").color(Color32::DARK_GREEN)),
                                Some(code) => ui.label(
                                    RichText::new(format!("crash? ({code})"))
                                        .color(Color32::DARK_RED),
                                ),
                                None => ui.label(RichText::new(session.end_reason.as_str()).weak()),
                            };
                            ui.end_row();
                        }
                    });
            });
    }

    /// Modal-ish dialog to pick the executable when a game's definition left
    /// it blank (the user chose "always pick from a list").
    fn exe_choice_window(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        let Some(choice) = &snapshot.exe_choice else {
            return;
        };
        egui::Window::new(format!("Choose an executable — {}", choice.title))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                if choice.candidates.is_empty() {
                    ui.label(
                        RichText::new(
                            "No .exe found under the install folder. Check the install \
                             completed, or open the folder to inspect.",
                        )
                        .color(Color32::ORANGE),
                    );
                } else {
                    ui.label("Pick the game's main executable:");
                }
                for rel in &choice.candidates {
                    if ui.button(rel).clicked() {
                        self.core.send(CoreCmd::PickExe {
                            game_id: choice.game_id.clone(),
                            exe_rel: rel.clone(),
                        });
                    }
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("install: {}", choice.install_root.display()))
                        .small()
                        .weak(),
                );
                ui.label(
                    RichText::new(format!("prefix:  {}", choice.prefix.display()))
                        .small()
                        .weak(),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("📁 Open install folder").clicked() {
                        Self::open_path(&choice.install_root);
                    }
                    if ui.button("Cancel").clicked() {
                        self.core.send(CoreCmd::CancelExeChoice);
                    }
                });
            });
    }

    /// Per-game launch settings: MangoHud, Gamescope, exe favourites, Proton.
    fn settings_window(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        let Some(game_id) = self.settings_for.clone() else {
            return;
        };
        let title = snapshot
            .games
            .iter()
            .find(|g| g.id == game_id)
            .map(|g| g.title.clone())
            .unwrap_or_else(|| game_id.clone());
        let detected: Vec<String> = match &snapshot.exe_list {
            Some((id, exes)) if *id == game_id => exes.clone(),
            _ => Vec::new(),
        };

        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        egui::Window::new(format!("Settings — {title}"))
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.checkbox(&mut self.settings_draft.mangohud, "MangoHud overlay")
                    .on_hover_text("wraps the launch in `mangohud -- …` (assumes it's installed)");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.settings_draft.gamescope, "Gamescope");
                    ui.add_enabled(
                        self.settings_draft.gamescope,
                        egui::TextEdit::singleline(&mut self.settings_draft.gamescope_args)
                            .hint_text("-W 2560 -H 1440 -f")
                            .desired_width(260.0),
                    )
                    .on_hover_text("raw gamescope args; composes with MangoHud");
                });

                ui.separator();
                ui.label("Custom launch options (Steam-style):");
                ui.add(
                    egui::TextEdit::singleline(&mut self.settings_draft.custom_args)
                        .hint_text("MANGOHUD_CONFIG=fps_limit=60 gamemoderun %command% -novid")
                        .desired_width(400.0),
                )
                .on_hover_text(
                    "leading KEY=VALUE tokens become env vars; %command% is replaced by \
                     the launch command (MangoHud/Gamescope included); without %command% \
                     the whole string prefixes it, like Gamescope above",
                );

                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Proton override (opt.)");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.settings_proton)
                            .hint_text("GE-Proton9-20")
                            .desired_width(220.0),
                    );
                });

                ui.separator();
                ui.label("Launch favourites (star the exes you launch):");
                ui.label(
                    RichText::new(
                        "0 ⇒ use the definition's exe; 1 ⇒ launch it directly; 2+ ⇒ asks \
                         every launch which to run (e.g. game vs. its launcher).",
                    )
                    .small()
                    .weak(),
                );
                if detected.is_empty() {
                    ui.label(RichText::new("no executables detected yet").weak());
                }
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for exe in &detected {
                            let mut fav = self.settings_draft.exe_favorites.contains(exe);
                            if ui.checkbox(&mut fav, exe.as_str()).changed() {
                                if fav {
                                    if !self.settings_draft.exe_favorites.contains(exe) {
                                        self.settings_draft.exe_favorites.push(exe.clone());
                                    }
                                } else {
                                    self.settings_draft.exe_favorites.retain(|e| e != exe);
                                }
                            }
                        }
                    });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if save {
            self.core.send(CoreCmd::SaveLaunchOpts {
                game_id: game_id.clone(),
                opts: self.settings_draft.clone(),
            });
            let proton = self.settings_proton.trim();
            self.core.send(CoreCmd::SetProtonOverride {
                game_id: game_id.clone(),
                value: (!proton.is_empty()).then(|| proton.to_string()),
            });
            self.settings_for = None;
        } else if !open || cancel {
            self.settings_for = None;
        }
    }

    /// Prominent error dialog — launch/install failures and the "no profile"
    /// case otherwise only show as tiny status-bar text.
    fn error_window(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        // adopt the latest error from the core snapshot
        if let Some(err) = &snapshot.last_error
            && self.shown_error.as_deref() != Some(err.as_str())
        {
            self.shown_error = Some(err.clone());
        }
        let Some(err) = self.shown_error.clone() else {
            return;
        };
        let mut open = true;
        let mut dismiss = false;
        egui::Window::new(RichText::new("⚠ Problem").color(Color32::DARK_RED))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(&err);
                ui.add_space(8.0);
                if ui.button("Dismiss").clicked() {
                    dismiss = true;
                }
            });
        if !open || dismiss {
            self.shown_error = None;
            self.core.send(CoreCmd::ClearError);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let snapshot = self.core.snapshot();

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            self.top_bar(ui, &snapshot);
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "machine: {} · library: {}",
                        snapshot.machine_name,
                        snapshot.library_dir.display()
                    ))
                    .small()
                    .weak(),
                );
                if let Err(reason) = &snapshot.syncthing_status {
                    ui.label(
                        RichText::new(format!("syncthing: {reason}"))
                            .small()
                            .color(Color32::DARK_RED),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(error) = &snapshot.last_error {
                        ui.label(RichText::new(error).small().color(Color32::DARK_RED));
                    } else if let Some(activity) = &snapshot.activity {
                        ui.label(RichText::new(activity).small());
                    }
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if snapshot.games.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.35);
                    ui.heading("No games yet");
                    ui.label(
                        RichText::new(
                            "Upload game files to the bucket, then define the game with \
                             “➕ Add game” — it lives on the server from then on.",
                        )
                        .weak(),
                    );
                });
            } else {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let games = snapshot.games.clone();
                    for game in &games {
                        self.game_row(ui, game);
                        ui.add_space(6.0);
                    }
                });
            }
        });

        self.profiles_window(ctx, &snapshot);
        self.game_form_window(ctx, &snapshot);
        self.install_options_window(ctx, &snapshot);
        self.wizard_windows(ctx, &snapshot);
        self.sessions_window(ctx, &snapshot);
        self.exe_choice_window(ctx, &snapshot);
        self.settings_window(ctx, &snapshot);
        self.error_window(ctx, &snapshot);
    }
}
