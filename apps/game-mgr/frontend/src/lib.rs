//! Egui panel for `game-mgr` -- replaces the originally-planned React/Vite
//! stats web UI (see `apps/game-mgr/backend`'s module docs and the repo-wide
//! port plan). Talks to `apps/game-mgr/backend` purely over HTTP, the same
//! `platform_core::Panel` + standalone pattern `apps/hello/frontend` is the
//! reference implementation of -- see that crate's module doc for the full
//! embedded-vs-standalone auth explanation this panel copies verbatim.
//!
//! Views, matching the page list the dropped web UI was scoped for:
//! **Dashboard** (top games by playtime, machine liveness), **Games**
//! (catalog + per-game session history), **Machines** (registration +
//! last-seen), **Profiles** (list/create/rename/transfer), **Settings**
//! (signed-in identity). There is no separate "Sync health" view: the
//! `sync_status` table exists in the backend's schema
//! (`apps/game-mgr/backend/migrations/0001_init.sql`) and
//! `game_mgr_core::syncthing::folder_health` can already compute the data
//! for it, but no route (ingest or read) has actually been wired up yet on
//! either side -- so there is nothing yet for a view to display; the
//! Machines tab notes this rather than inventing a client-side aggregate
//! the API can't back.

use std::collections::BTreeMap;

use game_mgr_api_types::{
    GameDto, MachineDto, MeResponse, PingResponse, ProfileDto, SessionDto, UserDto,
};
use platform_config::JsonResource;
use platform_core::{Panel, PanelId};
use uuid::Uuid;

#[cfg(not(target_arch = "wasm32"))]
use auth_adapter::frontend_native::LoginWidget;
#[cfg(target_arch = "wasm32")]
use auth_adapter::frontend_web::LoginWidget;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Dashboard,
    Games,
    Machines,
    Profiles,
    Settings,
}

/// Which cached [`JsonResource`] a [`GameMgrPanel::mutate`] call should
/// drop, forcing a re-fetch on the next tick. Explicit per call site so
/// adding a mutation for a resource other than profiles can't silently
/// keep serving stale data (`mutate` used to always reset `profiles`
/// regardless of what was actually mutated).
#[derive(Debug, Clone, Copy)]
enum Invalidate {
    Profiles,
}

pub struct GameMgrPanel {
    api_base_url: String,
    /// See `apps/hello/frontend/src/lib.rs`'s module doc -- `true` only when
    /// hosted inside the portal.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    embedded: bool,
    #[cfg(target_arch = "wasm32")]
    portal_token: Option<String>,
    login: LoginWidget,
    #[cfg(target_arch = "wasm32")]
    auth_config: JsonResource<auth_adapter::AuthConfig>,
    #[cfg(target_arch = "wasm32")]
    tried_silent_sso: bool,

    tab: Tab,
    last_error: Option<String>,

    /// `GET /api/v1/ping` -- unauthenticated, fetched unconditionally in
    /// `tick` (not gated on sign-in) so a build can be confirmed by eye
    /// even before logging in. See [`PingResponse`]'s doc comment.
    ping: JsonResource<PingResponse>,
    me: JsonResource<MeResponse>,
    games: JsonResource<Vec<GameDto>>,
    machines: JsonResource<Vec<MachineDto>>,
    profiles: JsonResource<Vec<ProfileDto>>,
    users: JsonResource<Vec<UserDto>>,

    selected_game: Option<String>,
    game_sessions: JsonResource<Vec<SessionDto>>,

    new_profile_name: String,
    renaming: Option<(Uuid, String)>,
    transfer_target: BTreeMap<Uuid, Uuid>,
    confirm_delete: Option<Uuid>,
}

impl GameMgrPanel {
    /// `embedded` should be `true` only when hosted inside the portal (see
    /// `apps/portal/frontend/src/lib.rs::open_tool`).
    pub fn new(api_base_url: impl Into<String>, embedded: bool) -> Self {
        let api_base_url = api_base_url.into();

        #[cfg(target_arch = "wasm32")]
        let login = LoginWidget::new();
        #[cfg(not(target_arch = "wasm32"))]
        let login = match auth_adapter::frontend_native::fetch_auth_config(&api_base_url) {
            Ok(cfg) => LoginWidget::new(cfg),
            Err(err) => {
                let msg = format!("could not fetch auth config from {api_base_url}: {err}");
                eprintln!("game-mgr-frontend: {msg}");
                LoginWidget::with_config_error(msg)
            }
        };

        Self {
            embedded,
            #[cfg(target_arch = "wasm32")]
            portal_token: None,
            login,
            #[cfg(target_arch = "wasm32")]
            auth_config: JsonResource::new(),
            #[cfg(target_arch = "wasm32")]
            tried_silent_sso: false,
            tab: Tab::default(),
            last_error: None,
            ping: JsonResource::new(),
            me: JsonResource::new(),
            games: JsonResource::new(),
            machines: JsonResource::new(),
            profiles: JsonResource::new(),
            users: JsonResource::new(),
            selected_game: None,
            game_sessions: JsonResource::new(),
            new_profile_name: String::new(),
            renaming: None,
            transfer_target: BTreeMap::new(),
            confirm_delete: None,
            api_base_url,
        }
    }

    /// Called once per frame by the portal (only when `embedded`) -- see
    /// `apps/hello/frontend`'s identical method.
    #[cfg(target_arch = "wasm32")]
    pub fn set_portal_token(&mut self, token: Option<String>) {
        if self.embedded {
            self.portal_token = token;
        }
    }

    fn bearer_token(&self) -> Option<String> {
        #[cfg(target_arch = "wasm32")]
        {
            if self.embedded {
                return self.portal_token.clone();
            }
        }
        self.login.bearer_token()
    }

    fn is_authenticated(&self) -> bool {
        self.bearer_token().is_some()
    }

    /// Short label for the deployed build, e.g. `"d4956ea88f2d"` -- `None`
    /// while `/ping` hasn't resolved yet or failed. Reads `build` (the git
    /// commit), *not* `version` (the compatibility-check semver) -- see
    /// [`PingResponse`]'s doc comment on why those are different fields.
    fn version_label(&self) -> Option<String> {
        match self.ping.ready() {
            Some(Ok(ping)) => Some(ping.build.chars().take(12).collect()),
            _ => None,
        }
    }

    fn api(&self, path: &str) -> String {
        format!("{}{}", self.api_base_url.trim_end_matches('/'), path)
    }

    fn get<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        resource: &mut JsonResource<T>,
        path: &str,
    ) {
        let Some(token) = self.bearer_token() else {
            return;
        };
        let auth = format!("Bearer {token}");
        resource.fetch_with_headers(&self.api(path), &[("Authorization", &auth)]);
    }

    /// Fire-and-forget POST/PATCH/DELETE, same pattern as
    /// `apps/hello/frontend`'s `post_greeting`/`reset_greetings`: the next
    /// tick's re-fetch is the only feedback, and a missing role/ownership
    /// surfaces as a rejected status the backend already enforces.
    /// `invalidate` names the resource this call actually touched so only
    /// that cache gets dropped -- see [`Invalidate`]'s doc comment.
    fn mutate(
        &mut self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
        invalidate: Invalidate,
    ) {
        let Some(token) = self.bearer_token() else {
            return;
        };
        let mut request = match &body {
            Some(v) => ehttp::Request::post(self.api(path), v.to_string().into_bytes()),
            None => ehttp::Request::post(self.api(path), Vec::new()),
        };
        request.method = method.to_owned();
        let mut headers = vec![("Authorization".to_string(), format!("Bearer {token}"))];
        if body.is_some() {
            headers.push(("Content-Type".to_string(), "application/json".to_string()));
        }
        request.headers = ehttp::Headers::new(
            &headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect::<Vec<_>>(),
        );
        ehttp::fetch(request, |_response| {});
        match invalidate {
            Invalidate::Profiles => self.profiles = JsonResource::new(),
        }
    }
}

impl Panel for GameMgrPanel {
    fn id(&self) -> PanelId {
        "game-mgr"
    }

    fn title(&self) -> &str {
        "game-mgr"
    }

    fn tick(&mut self, ctx: &egui::Context) {
        // Unauthenticated and unconditional -- deliberately ahead of the
        // `is_authenticated` gate below, so the version is visible even on
        // the pre-sign-in screen.
        if !self.ping.has_requested() {
            self.ping.fetch(&self.api("/api/v1/ping"));
        }

        #[cfg(target_arch = "wasm32")]
        if !self.embedded {
            if !self.auth_config.has_requested() {
                self.auth_config.fetch(&format!(
                    "{}/config/auth.json",
                    self.api_base_url.trim_end_matches('/')
                ));
            }
            if let Some(Ok(cfg)) = self.auth_config.ready() {
                self.login.set_config(cfg.clone());
            }
        }

        #[cfg(target_arch = "wasm32")]
        if !self.embedded {
            self.login.tick(ctx);
            if !self.tried_silent_sso && !self.login.is_authenticated() {
                self.tried_silent_sso = true;
                self.login.attempt_silent_sso();
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.login.tick(ctx);

        if !self.is_authenticated() {
            return;
        }

        match self.tab {
            Tab::Dashboard | Tab::Games => {
                if !self.games.has_requested() {
                    let mut r = JsonResource::new();
                    self.get(&mut r, "/api/v1/games");
                    self.games = r;
                }
                if !self.machines.has_requested() && matches!(self.tab, Tab::Dashboard) {
                    let mut r = JsonResource::new();
                    self.get(&mut r, "/api/v1/machines");
                    self.machines = r;
                }
                if let Some(game_id) = self.selected_game.clone() {
                    if !self.game_sessions.has_requested() {
                        let mut r = JsonResource::new();
                        self.get(
                            &mut r,
                            &format!("/api/v1/sessions?game_id={game_id}&limit=20"),
                        );
                        self.game_sessions = r;
                    }
                }
            }
            Tab::Machines => {
                if !self.machines.has_requested() {
                    let mut r = JsonResource::new();
                    self.get(&mut r, "/api/v1/machines");
                    self.machines = r;
                }
            }
            Tab::Profiles => {
                if !self.profiles.has_requested() {
                    let mut r = JsonResource::new();
                    self.get(&mut r, "/api/v1/profiles");
                    self.profiles = r;
                }
                if !self.users.has_requested() {
                    let mut r = JsonResource::new();
                    self.get(&mut r, "/api/v1/users");
                    self.users = r;
                }
                if !self.me.has_requested() {
                    let mut r = JsonResource::new();
                    self.get(&mut r, "/api/v1/me");
                    self.me = r;
                }
            }
            Tab::Settings => {
                if !self.me.has_requested() {
                    let mut r = JsonResource::new();
                    self.get(&mut r, "/api/v1/me");
                    self.me = r;
                }
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("game-mgr");
            // Visible whether or not you're signed in -- lets a deploy be
            // confirmed by eye without an extra click. See `version_label`.
            match self.version_label() {
                Some(v) => {
                    ui.label(egui::RichText::new(format!("build {v}")).weak().small())
                        .on_hover_text("GET /api/v1/ping -- the deployed backend's git commit");
                }
                None => {
                    ui.label(egui::RichText::new("build ?").weak().small());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                #[cfg(target_arch = "wasm32")]
                if !self.embedded {
                    self.login.ui(ui);
                }
                #[cfg(not(target_arch = "wasm32"))]
                self.login.ui(ui);
            });
        });

        if !self.is_authenticated() {
            ui.label(egui::RichText::new("Sign in above to view stats.").weak());
            return;
        }

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::Dashboard, "Dashboard");
            ui.selectable_value(&mut self.tab, Tab::Games, "Games");
            ui.selectable_value(&mut self.tab, Tab::Machines, "Machines");
            ui.selectable_value(&mut self.tab, Tab::Profiles, "Profiles");
            ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
        });
        ui.separator();

        if let Some(err) = &self.last_error {
            ui.colored_label(egui::Color32::RED, err);
        }

        egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
            Tab::Dashboard => self.ui_dashboard(ui),
            Tab::Games => self.ui_games(ui),
            Tab::Machines => self.ui_machines(ui),
            Tab::Profiles => self.ui_profiles(ui),
            Tab::Settings => self.ui_settings(ui),
        });
    }
}

mod panels;

/// Mounts this tool standalone into the `<canvas id="the_canvas_id">` in
/// `apps/game-mgr/frontend/index.html` -- see `apps/hello/frontend`'s
/// identical `start()` for the full explanation of the `standalone` feature
/// gate below.
#[cfg(all(target_arch = "wasm32", feature = "standalone"))]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();

    wasm_bindgen_futures::spawn_local(async {
        platform_core::standalone::run_web("the_canvas_id", GameMgrPanel::new("", false))
            .await
            .expect("failed to start eframe");
    });
}
