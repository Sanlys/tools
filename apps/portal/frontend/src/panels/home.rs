use platform_config::{JsonResource, ToolRegistry};
use platform_core::{Panel, PanelId};

/// Landing panel: links out to every tool's standalone deployment. Fetches
/// the same runtime `tools.json` the sidebar uses (see
/// `platform_config::ToolRegistry`) rather than duplicating a hard-coded
/// list.
#[derive(Default)]
pub struct HomePanel {
    registry: JsonResource<ToolRegistry>,
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
            self.registry.fetch("/config/tools.json");
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
            }
        }
    }
}
