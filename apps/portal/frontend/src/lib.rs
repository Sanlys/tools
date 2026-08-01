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

/// The closed, compile-time-known set of panels the unified app can host.
/// Adding a tool means: add a variant here, add it to the `dispatch!` match
/// list right below, and add a match arm in [`PortalApp::open_tool`]. See
/// `docs/adding-a-tool.md`.
pub enum ToolPanel {
    Home(HomePanel),
    Dashboard(DashboardPanel),
    Hello(hello_frontend::HelloPanel),
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
    registry: JsonResource<ToolRegistry>,
    open: BTreeMap<String, OpenPanel>,
}

impl Default for PortalApp {
    fn default() -> Self {
        let mut app = Self {
            registry: JsonResource::new(),
            open: BTreeMap::new(),
        };
        app.open_builtin("home");
        app
    }
}

impl PortalApp {
    fn open_builtin(&mut self, id: &str) {
        if let Some(existing) = self.open.get_mut(id) {
            existing.visible = true;
            return;
        }
        let panel: Option<ToolPanel> = match id {
            "home" => Some(ToolPanel::Home(HomePanel::default())),
            "dashboard" => Some(ToolPanel::Dashboard(DashboardPanel::default())),
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

    /// Opens the compiled-in panel for a tool from the runtime registry.
    /// `link.id` must match one of the match arms here -- see
    /// `docs/adding-a-tool.md`.
    fn open_tool(&mut self, link: &ToolLink) {
        if let Some(existing) = self.open.get_mut(&link.id) {
            existing.visible = true;
            return;
        }
        let panel: ToolPanel = match link.id.as_str() {
            "hello" => ToolPanel::Hello(hello_frontend::HelloPanel::new(link.api_base_url.clone())),
            other => {
                tracing::warn!(
                    "no compiled-in panel for tool id `{other}`; add a ToolPanel variant in \
                     apps/portal/frontend/src/lib.rs"
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
            self.registry.fetch("/config/tools.json");
        }

        egui::SidePanel::left("nav")
            .min_width(180.0)
            .show(ctx, |ui| {
                ui.heading("Tools Platform");
                ui.separator();
                if ui.button("Home").clicked() {
                    self.open_builtin("home");
                }
                if ui.button("Dashboard").clicked() {
                    self.open_builtin("dashboard");
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
                            if ui.button(&link.name).clicked() {
                                self.open_tool(link);
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
                Box::new(|_cc| Ok(Box::new(PortalApp::default()))),
            )
            .await
            .expect("failed to start eframe");
    });
}
