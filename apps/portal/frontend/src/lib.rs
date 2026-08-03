//! The unified "Tools Platform" egui app.
//!
//! Every tool's panel is compiled into this one binary as a variant of
//! [`ToolPanel`] -- there is no runtime plugin loading. Multiple panels can
//! be open (and ticking/polling their own backends) at the same time, each
//! in its own `egui::Window`, which is what "tools can be opened
//! simultaneously" means in practice here.
//!
//! Per-tool backend URLs are **not** compiled in: they're resolved at
//! runtime from `/config/tools.json` (served by `apps/portal/backend`, see
//! [`platform_config::ToolRegistry`]), so the same wasm build works
//! unmodified across environments. See `docs/adding-a-tool.md` for how a
//! new tool gets wired in here.

mod panels;

pub use panels::{DashboardPanel, HomePanel};

use platform_config::{JsonResource, ToolLink, ToolRegistry};
use platform_core::{Panel, PanelId};
use std::collections::BTreeMap;

#[cfg(not(target_arch = "wasm32"))]
use auth_adapter::frontend_native::LoginWidget;
#[cfg(target_arch = "wasm32")]
use auth_adapter::frontend_web::LoginWidget;

/// The closed, compile-time-known set of panels the unified app can host.
/// Adding a tool means: add a variant here, add it to the `dispatch!` match
/// list right below, and add a match arm in [`PortalApp::open_tool`]. See
/// `docs/adding-a-tool.md`.
pub enum ToolPanel {
    Home(HomePanel),
    Dashboard(DashboardPanel),
    Hello(hello_frontend::HelloPanel),
    // Boxed: `GameMgrPanel` carries several `JsonResource`s (one per tab)
    // and is much larger than every other variant here, which clippy's
    // `large_enum_variant` flags -- boxing keeps `ToolPanel` itself small
    // regardless of how large any one tool's panel state gets.
    GameMgr(Box<game_mgr_frontend::GameMgrPanel>),
    /// This panel calls the IDP's own API directly using the portal's own
    /// bearer token (see `panels::idp`'s doc comment) -- works identically
    /// on native and wasm since both platforms now carry a portal-scoped
    /// `LoginWidget` (see `PortalApp::login`). Boxed for the same reason
    /// as `GameMgr` above: `IdpPanel` carries several `JsonResource`s and
    /// clippy's `large_enum_variant` flags it otherwise.
    Idp(Box<panels::IdpPanel>),
}

/// Delegates a `Panel` method call to whichever variant is active. Kept as
/// one macro so every method's match list stays in sync -- see the note on
/// `ToolPanel` above for why this is hand-written instead of using
/// `enum_dispatch` (its trait/enum linkage doesn't work across crates).
macro_rules! dispatch {
    ($self:expr, |$panel:ident| $body:expr) => {
        match $self {
            ToolPanel::Home($panel) => $body,
            ToolPanel::Dashboard($panel) => $body,
            ToolPanel::Hello($panel) => $body,
            ToolPanel::GameMgr($panel) => $body,
            ToolPanel::Idp($panel) => $body,
        }
    };
}

impl Panel for ToolPanel {
    fn id(&self) -> PanelId {
        dispatch!(self, |p| p.id())
    }

    fn title(&self) -> &str {
        dispatch!(self, |p| p.title())
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        dispatch!(self, |p| p.ui(ui))
    }

    fn tick(&mut self, ctx: &egui::Context) {
        dispatch!(self, |p| p.tick(ctx))
    }
}

struct OpenPanel {
    panel: ToolPanel,
    visible: bool,
}

pub struct PortalApp {
    /// This build's own backend to talk to: empty (same-origin relative
    /// paths) for the wasm build served by `apps/portal/backend` itself,
    /// or an absolute URL for the native desktop build, which has no page
    /// origin to resolve a relative path against -- see
    /// [`PortalApp::new`]/`src/bin/desktop.rs`.
    api_base_url: String,
    registry: JsonResource<ToolRegistry>,
    open: BTreeMap<String, OpenPanel>,
    /// The portal's *own* sign-in state (client_id "portal") -- this only
    /// gates portal-native features (and the "Account" panel below). It
    /// can't gate other tools' panels: a token minted for "portal" carries
    /// no roles for "hello" or any other app's own client_id (standard
    /// OIDC audience scoping). Each tool's own standalone panel manages
    /// its own login independently -- see `hello_frontend::HelloPanel` for
    /// that pattern.
    #[cfg(target_arch = "wasm32")]
    auth_config: JsonResource<auth_adapter::AuthConfig>,
    login: LoginWidget,
}

impl PortalApp {
    /// `api_base_url` is this build's own backend (`apps/portal/backend`)
    /// -- pass `""` for the wasm build (served by that same backend, so
    /// every path resolves same-origin), or an absolute URL for the native
    /// desktop build. See `src/bin/desktop.rs`.
    pub fn new(api_base_url: impl Into<String>) -> Self {
        let api_base_url = api_base_url.into();

        #[cfg(not(target_arch = "wasm32"))]
        let login = match auth_adapter::frontend_native::fetch_auth_config(&api_base_url) {
            Ok(cfg) => LoginWidget::new(cfg),
            Err(err) => {
                let msg = format!("could not fetch auth config from {api_base_url}: {err}");
                tracing::error!("portal-desktop: {msg}");
                LoginWidget::with_config_error(msg)
            }
        };
        #[cfg(target_arch = "wasm32")]
        let login = LoginWidget::new();

        let mut app = Self {
            registry: JsonResource::new(),
            open: BTreeMap::new(),
            #[cfg(target_arch = "wasm32")]
            auth_config: JsonResource::new(),
            login,
            api_base_url,
        };
        app.open_builtin("home");
        app
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.api_base_url.trim_end_matches('/'))
    }

    fn open_builtin(&mut self, id: &str) {
        if let Some(existing) = self.open.get_mut(id) {
            existing.visible = true;
            return;
        }
        let panel: Option<ToolPanel> = match id {
            "home" => Some(ToolPanel::Home(HomePanel::new(self.api_base_url.clone()))),
            "dashboard" => Some(ToolPanel::Dashboard(DashboardPanel::new(
                self.api_base_url.clone(),
            ))),
            "idp" => Some(ToolPanel::Idp(Box::new(panels::IdpPanel::new()))),
            _ => None,
        };
        if let Some(panel) = panel {
            self.open.insert(
                id.to_string(),
                OpenPanel {
                    panel,
                    visible: true,
                },
            );
        }
    }

    /// Whether `id` has a compiled-in [`ToolPanel`] variant -- not every
    /// registered tool does (e.g. `webhello`'s frontend is a plain static
    /// page, not egui, so it has no panel to embed here; see
    /// `apps/webhello/backend`). Used to decide whether the sidebar should
    /// offer an inline "open panel" button or just a link out, same
    /// treatment `HomePanel` already gives every tool.
    fn has_panel(id: &str) -> bool {
        matches!(id, "hello" | "game-mgr")
    }

    /// Opens the compiled-in panel for a tool from the runtime registry.
    /// `link.id` must match one of the match arms here -- see
    /// `docs/adding-a-tool.md`. Only call this when [`Self::has_panel`]
    /// returns true for `link.id`.
    fn open_tool(&mut self, link: &ToolLink) {
        if let Some(existing) = self.open.get_mut(&link.id) {
            existing.visible = true;
            return;
        }
        let panel: ToolPanel = match link.id.as_str() {
            "hello" => ToolPanel::Hello(hello_frontend::HelloPanel::new(
                link.api_base_url.clone(),
                true,
            )),
            "game-mgr" => ToolPanel::GameMgr(Box::new(game_mgr_frontend::GameMgrPanel::new(
                link.api_base_url.clone(),
                true,
            ))),
            other => {
                tracing::warn!(
                    "no compiled-in panel for tool id `{other}`; add a ToolPanel variant in \
                     apps/portal/frontend/src/lib.rs, or leave it out of `has_panel` if it's \
                     meant to be link-out only"
                );
                return;
            }
        };
        self.open.insert(
            link.id.clone(),
            OpenPanel {
                panel,
                visible: true,
            },
        );
    }
}

impl eframe::App for PortalApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.registry.has_requested() {
            let url = self.url("/config/tools.json");
            self.registry.fetch(&url);
        }

        #[cfg(target_arch = "wasm32")]
        {
            if !self.auth_config.has_requested() {
                let url = self.url("/config/auth.json");
                self.auth_config.fetch(&url);
            }
            if let Some(Ok(cfg)) = self.auth_config.ready() {
                self.login.set_config(cfg.clone());
            }
        }
        self.login.tick(ctx);

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Tools Platform");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.login.ui(ui);
                });
            });
        });

        egui::SidePanel::left("nav")
            .min_width(180.0)
            .show(ctx, |ui| {
                if ui.button("Home").clicked() {
                    self.open_builtin("home");
                }
                if ui.button("Dashboard").clicked() {
                    self.open_builtin("dashboard");
                }
                if ui.button("Account").clicked() {
                    self.open_builtin("idp");
                }
                ui.separator();
                ui.label("Tools:");
                match self.registry.ready() {
                    None => {
                        ui.spinner();
                    }
                    Some(Ok(links)) => {
                        let links = links.clone();
                        for link in &links {
                            if Self::has_panel(&link.id) {
                                // `requires_role` is purely a cosmetic hint
                                // here (a lock icon) -- the portal can't
                                // itself know whether the signed-in user
                                // holds a role scoped to *this tool's*
                                // client_id (see the note on
                                // `PortalApp::login`); the opened panel
                                // resolves that for real, using its own
                                // login widget.
                                let label = match &link.requires_role {
                                    Some(_) => format!("🔒 {}", link.name),
                                    None => link.name.clone(),
                                };
                                if ui.button(label).clicked() {
                                    self.open_tool(link);
                                }
                            } else {
                                // No compiled-in panel for this tool (e.g.
                                // webhello) -- link out instead of a dead
                                // click, same treatment HomePanel gives it.
                                ui.hyperlink_to(&link.name, &link.standalone_url);
                            }
                        }
                    }
                    Some(Err(err)) => {
                        ui.colored_label(egui::Color32::RED, err);
                    }
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.open.values().all(|p| !p.visible) {
                ui.label(
                    "Open a panel from the sidebar -- panels open in their own \
                     window and can run side by side.",
                );
            }
        });

        let mut newly_closed = Vec::new();
        for (id, open_panel) in self.open.iter_mut() {
            if !open_panel.visible {
                continue;
            }
            match &mut open_panel.panel {
                ToolPanel::Idp(idp) => {
                    idp.set_auth(
                        self.login.bearer_token(),
                        self.login.config().map(|c| c.issuer_url.clone()),
                    );
                }
                // wasm-only: on native, an embedded Hello/GameMgr panel
                // has no portal token to borrow in the first place -- see
                // each panel's own `embedded` field doc comment. It runs
                // its own separate native login instead, same as its
                // standalone build.
                #[cfg(target_arch = "wasm32")]
                ToolPanel::Hello(hello) => {
                    hello.set_portal_token(self.login.bearer_token());
                }
                #[cfg(target_arch = "wasm32")]
                ToolPanel::GameMgr(game_mgr) => {
                    game_mgr.set_portal_token(self.login.bearer_token());
                }
                _ => {}
            }
            open_panel.panel.tick(ctx);
            let mut still_visible = true;
            egui::Window::new(open_panel.panel.title())
                .id(egui::Id::new(id.as_str()))
                .open(&mut still_visible)
                .default_size([440.0, 320.0])
                .show(ctx, |ui| {
                    open_panel.panel.ui(ui);
                });
            if !still_visible {
                newly_closed.push(id.clone());
            }
        }
        for id in newly_closed {
            if let Some(p) = self.open.get_mut(&id) {
                p.visible = false;
            }
        }
    }
}

/// Mounts the app into the `<canvas id="the_canvas_id">` in
/// `apps/portal/frontend/index.html`. Runs automatically once the wasm
/// module loads (`trunk`'s default generated glue just calls `init()`,
/// which triggers this) -- no hand-written JS needed.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();

    wasm_bindgen_futures::spawn_local(async {
        use wasm_bindgen::JsCast as _;

        let canvas = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("the_canvas_id"))
            .expect("index.html must contain a <canvas id=\"the_canvas_id\">")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("#the_canvas_id must be a <canvas> element");

        eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|_cc| Ok(Box::new(PortalApp::new("")))),
            )
            .await
            .expect("failed to start eframe");
    });
}
