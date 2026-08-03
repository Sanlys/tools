use platform_config::{JsonResource, ToolRegistry};
use platform_core::{Panel, PanelId};

/// Landing panel: links out to every tool's standalone deployment. Fetches
/// the same runtime `tools.json` the sidebar uses (see
/// `platform_config::ToolRegistry`) rather than duplicating a hard-coded
/// list.
#[derive(Default)]
pub struct HomePanel {
    /// The portal backend to fetch `tools.json` from -- `""` resolves as a
    /// same-origin relative path (the wasm build, served by that same
    /// backend); the native desktop build passes an absolute URL instead,
    /// since it has no page origin to resolve a relative path against. See
    /// `PortalApp::new`.
    api_base_url: String,
    registry: JsonResource<ToolRegistry>,
}

impl HomePanel {
    pub fn new(api_base_url: impl Into<String>) -> Self {
        Self {
            api_base_url: api_base_url.into(),
            registry: JsonResource::new(),
        }
    }
}

impl Panel for HomePanel {
    fn id(&self) -> PanelId {
        "home"
    }

    fn title(&self) -> &str {
        "Home"
    }

    fn tick(&mut self, _ctx: &egui::Context) {
        if !self.registry.has_requested() {
            let url = format!(
                "{}/config/tools.json",
                self.api_base_url.trim_end_matches('/')
            );
            self.registry.fetch(&url);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Internal Tools Platform");
        ui.label(
            "Every tool below can be opened inline here, or visited at its \
             own standalone deployment.",
        );
        ui.separator();

        match self.registry.ready() {
            None => {
                ui.spinner();
            }
            Some(Ok(links)) if links.is_empty() => {
                ui.label("No tools registered yet.");
            }
            Some(Ok(links)) => {
                egui::Grid::new("home_tool_grid")
                    .num_columns(3)
                    .striped(true)
                    .show(ui, |ui| {
                        for link in links {
                            ui.strong(&link.name);
                            ui.label(&link.description);
                            ui.hyperlink_to("open standalone \u{2197}", &link.standalone_url);
                            ui.end_row();
                        }
                    });
            }
            Some(Err(err)) => {
                ui.colored_label(
                    egui::Color32::RED,
                    format!("failed to load tools.json: {err}"),
                );
                // Without this, a transient failure (backend still
                // starting up, a network blip) was permanent: `tick`
                // only ever fetches once (`!has_requested()`), unlike
                // `DashboardPanel`'s own periodic retry, so there was no
                // way to recover short of reloading/restarting the whole
                // app.
                if ui.button("Retry").clicked() {
                    self.registry = JsonResource::new();
                }
            }
        }
    }
}
