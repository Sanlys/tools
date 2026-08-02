//! Egui panel for the `hello` example tool.
//!
//! This crate is the reference implementation of the "every tool has an
//! egui UI that plugs into the unified portal *and* can run standalone"
//! pattern (see `docs/adding-a-tool.md`). It talks to its own backend
//! (`apps/hello/backend`) purely over HTTP -- no direct DB/S3 access from
//! the frontend, which is what lets the exact same code run both natively
//! and compiled to wasm in the browser.
//!
//! The backend's base URL is passed in by whoever hosts this panel (the
//! portal, or `src/bin/standalone.rs`) rather than hard-coded, since the
//! portal resolves it at runtime from `/config/tools.json`
//! ([`platform_config::ToolRegistry`]) instead of baking it into the wasm
//! binary at compile time.
//!
//! This crate compiles to wasm two different ways: embedded as an `rlib`
//! dependency inside `apps/portal/frontend`'s single unified `cdylib` (that
//! wasm build uses an absolute, cross-origin `api_base_url` from the tool
//! registry, since the portal and this tool are served from different
//! subdomains), and standalone as its own `cdylib` via the
//! `#[wasm_bindgen(start)]` below, built by `apps/hello/backend`'s
//! Dockerfile and served at this tool's own ingress host -- that build uses
//! an empty `api_base_url` (`""`), which resolves every request as a
//! same-origin relative path, since `hello-backend` serves both the API and
//! this compiled bundle itself.
//!
//! This is also the reference implementation of "a tool with an auth-gated
//! action": it carries its own [`auth_adapter`] `LoginWidget`, scoped to
//! *this app's own* `client_id` ("hello", per `deploy/idp/values.yaml`) --
//! not the portal's. That's deliberate: a token minted for the portal's own
//! client_id can't carry roles for a different app's client_id (standard
//! OIDC audience scoping), so any tool that wants to check "does this user
//! have role X *for me*" needs its own login/token, even when it's opened
//! as a panel inside the portal. See `docs/adding-a-tool.md`'s auth
//! section for the copy-paste version of this pattern.

use platform_config::JsonResource;
use platform_core::{Panel, PanelId};
use serde::{Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
use auth_adapter::frontend_native::LoginWidget;
#[cfg(target_arch = "wasm32")]
use auth_adapter::frontend_web::LoginWidget;

/// The role this tool declares (see its entry in `IDP_CLIENTS_JSON`) that
/// gates the "reset greetings" action below.
const OPERATOR_ROLE: &str = "operator";

/// Mirrors `apps/hello/backend`'s `GET /api/status` response. Duplicated
/// (rather than shared via a common crate) deliberately: it's two small
/// structs, and keeping backend and frontend decoupled at the type level
/// means the backend never has to compile-depend on egui/eframe.
#[derive(Debug, Clone, Deserialize)]
struct HelloStatus {
    message: String,
    greeting_count: i64,
    bucket_object_count: u64,
}

#[derive(Debug, Serialize)]
struct NewGreeting<'a> {
    name: &'a str,
}

pub struct HelloPanel {
    api_base_url: String,
    /// `true` when hosted as a panel inside the portal, which already shows
    /// the signed-in user elsewhere on the page -- suppresses this panel's
    /// own avatar so it isn't duplicated per panel (`false` for both native
    /// and standalone-wasm builds, where this is the only sign-in indicator
    /// on the page at all). See `LoginWidget::ui_compact`'s doc comment.
    /// Only consulted on wasm -- native has no `ui_compact` (there's no
    /// portal top bar to defer to in a native standalone window either
    /// way).
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    embedded: bool,
    status: JsonResource<HelloStatus>,
    name_input: String,
    last_error: Option<String>,
    login: LoginWidget,
    #[cfg(target_arch = "wasm32")]
    auth_config: JsonResource<auth_adapter::AuthConfig>,
    #[cfg(target_arch = "wasm32")]
    tried_silent_sso: bool,
}

impl HelloPanel {
    /// `embedded` should be `true` only when this panel is hosted inside
    /// the portal (see `apps/portal/frontend/src/lib.rs::open_tool`) --
    /// `false` for both the native and standalone-wasm builds below, which
    /// have no other UI on the page to show the signed-in user.
    pub fn new(api_base_url: impl Into<String>, embedded: bool) -> Self {
        let api_base_url = api_base_url.into();

        #[cfg(target_arch = "wasm32")]
        let login = LoginWidget::new();
        #[cfg(not(target_arch = "wasm32"))]
        let login = match auth_adapter::frontend_native::fetch_auth_config(&api_base_url) {
            Ok(cfg) => LoginWidget::new(cfg),
            Err(err) => {
                let msg = format!("could not fetch auth config from {api_base_url}: {err}");
                eprintln!("hello-frontend: {msg}");
                LoginWidget::with_config_error(msg)
            }
        };

        Self {
            embedded,
            status: JsonResource::new(),
            name_input: String::new(),
            last_error: None,
            login,
            #[cfg(target_arch = "wasm32")]
            auth_config: JsonResource::new(),
            #[cfg(target_arch = "wasm32")]
            tried_silent_sso: false,
            api_base_url,
        }
    }

    fn status_url(&self) -> String {
        format!("{}/api/status", self.api_base_url.trim_end_matches('/'))
    }

    /// Requires being signed in -- `apps/hello/backend`'s `post_greeting`
    /// now takes an `AuthUser` param (any authenticated user, no specific
    /// role), so an unauthenticated request here would just get a 401
    /// anyway; the `bearer_token` check keeps this from firing a doomed
    /// request when the "Say hello" button should already be disabled.
    fn post_greeting(&mut self) {
        if self.name_input.trim().is_empty() {
            return;
        }
        let Some(token) = self.login.bearer_token() else {
            return;
        };
        let url = format!("{}/api/greetings", self.api_base_url.trim_end_matches('/'));
        let body = match serde_json::to_vec(&NewGreeting {
            name: self.name_input.trim(),
        }) {
            Ok(body) => body,
            Err(err) => {
                self.last_error = Some(format!("failed to encode request: {err}"));
                return;
            }
        };
        let mut request = ehttp::Request::post(url, body);
        request.headers = ehttp::Headers::new(&[
            ("Accept", "application/json"),
            ("Content-Type", "application/json"),
            ("Authorization", &format!("Bearer {token}")),
        ]);
        ehttp::fetch(request, |_response| {
            // Fire-and-forget: the next `tick()` re-fetches status, which
            // will reflect the write once the response lands.
        });
        self.name_input.clear();
        // Force a fresh status fetch on the next tick rather than waiting
        // out whatever poll interval `tick()` uses.
        self.status = JsonResource::new();
    }

    fn reset_greetings(&mut self) {
        let Some(token) = self.login.bearer_token() else {
            return;
        };
        let url = format!(
            "{}/api/greetings/reset",
            self.api_base_url.trim_end_matches('/')
        );
        let mut request = ehttp::Request::post(url, Vec::new());
        request.method = "DELETE".to_owned();
        request.headers = ehttp::Headers::new(&[("Authorization", &format!("Bearer {token}"))]);
        ehttp::fetch(request, |_response| {});
        self.status = JsonResource::new();
    }
}

impl Panel for HelloPanel {
    fn id(&self) -> PanelId {
        "hello"
    }

    fn title(&self) -> &str {
        "Hello"
    }

    fn tick(&mut self, ctx: &egui::Context) {
        if !self.status.has_requested() {
            self.status.fetch(&self.status_url());
        }

        #[cfg(target_arch = "wasm32")]
        {
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

        self.login.tick(ctx);

        // First frame after we have a session (or know we don't): try a
        // silent SSO once. If the user already signed in anywhere against
        // this IDP, this picks that session up with no visible passkey
        // prompt; if not, it's a no-op that just leaves the "Sign in"
        // button showing (see `LoginWidget::attempt_silent_sso`'s
        // doc-comment on the one-redirect-flash cost this trades off).
        #[cfg(target_arch = "wasm32")]
        if !self.tried_silent_sso && !self.login.is_authenticated() {
            self.tried_silent_sso = true;
            self.login.attempt_silent_sso();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Hello");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                #[cfg(target_arch = "wasm32")]
                if self.embedded {
                    self.login.ui_compact(ui);
                } else {
                    self.login.ui(ui);
                }
                #[cfg(not(target_arch = "wasm32"))]
                self.login.ui(ui);
            });
        });
        ui.label(
            "Example tool: proves the egui-panel + standalone-backend + \
             Postgres + S3 pattern end to end.",
        );
        ui.separator();

        if self.login.is_authenticated() {
            ui.horizontal(|ui| {
                ui.label("Your name:");
                ui.text_edit_singleline(&mut self.name_input);
                if ui.button("Say hello").clicked() {
                    self.post_greeting();
                }
            });
        } else {
            ui.label(
                egui::RichText::new(
                    "Sign in above to say hello -- posting requires being signed in.",
                )
                .weak(),
            );
        }

        if let Some(err) = &self.last_error {
            ui.colored_label(egui::Color32::RED, err);
        }

        ui.separator();

        match self.status.ready() {
            None => {
                ui.spinner();
                ui.label("contacting backend...");
            }
            Some(Ok(status)) => {
                ui.label(&status.message);
                ui.label(format!(
                    "greetings recorded in Postgres: {}",
                    status.greeting_count
                ));
                ui.label(format!(
                    "objects in this tool's S3 bucket: {}",
                    status.bucket_object_count
                ));
                if ui.button("Refresh").clicked() {
                    self.status = JsonResource::new();
                }
            }
            Some(Err(err)) => {
                ui.colored_label(egui::Color32::RED, format!("backend error: {err}"));
            }
        }

        ui.separator();
        ui.label(egui::RichText::new("Operator actions").strong());
        if self.login.has_role(OPERATOR_ROLE) {
            if ui.button("Reset all greetings").clicked() {
                self.reset_greetings();
            }
        } else {
            ui.label(
                egui::RichText::new(format!(
                    "requires the `{OPERATOR_ROLE}` role -- sign in above"
                ))
                .weak(),
            );
        }
    }
}

/// Mounts this tool standalone into the `<canvas id="the_canvas_id">` in
/// `apps/hello/frontend/index.html`, talking to `hello-backend` on the same
/// origin that serves this bundle. Runs automatically once the wasm module
/// loads (trunk's generated glue calls `init()`, which triggers this) -- see
/// `apps/portal/frontend/src/lib.rs`'s `start()` for the same pattern one
/// level up (the unified portal, hosting many panels instead of just this
/// one).
///
/// Gated behind the `standalone` feature (on by default -- see this
/// crate's Cargo.toml) rather than just `target_arch = "wasm32"`: this
/// crate is *also* linked into `apps/portal/frontend`'s own wasm bundle as
/// a plain rlib dependency (to embed `HelloPanel`), and two
/// `#[wasm_bindgen(start)]` exports in the same wasm module is a linker
/// error, not just a harmless double-init. The portal depends on this
/// crate with `default-features = false` specifically to leave this out.
#[cfg(all(target_arch = "wasm32", feature = "standalone"))]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();

    wasm_bindgen_futures::spawn_local(async {
        platform_core::standalone::run_web("the_canvas_id", HelloPanel::new("", false))
            .await
            .expect("failed to start eframe");
    });
}
