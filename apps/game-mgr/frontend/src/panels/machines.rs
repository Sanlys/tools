//! Machines tab: registration/liveness (`GET /machines`). No sync-folder
//! health here -- see `crate`'s module doc on why: the API only has a write
//! side for that (`PUT /machines/{id}/sync-status`), nothing to read back.

use super::format_relative;
use crate::GameMgrPanel;

impl GameMgrPanel {
    pub(crate) fn ui_machines(&mut self, ui: &mut egui::Ui) {
        match self.machines.ready() {
            None => {
                ui.spinner();
                ui.label("loading machines...");
            }
            Some(Err(err)) => {
                ui.colored_label(
                    egui::Color32::RED,
                    format!("failed to load machines: {err}"),
                );
            }
            Some(Ok(machines)) if machines.is_empty() => {
                ui.label(egui::RichText::new("No machines registered yet.").weak());
            }
            Some(Ok(machines)) => {
                egui::Grid::new("machines_grid")
                    .striped(true)
                    .num_columns(4)
                    .show(ui, |ui| {
                        ui.strong("Name");
                        ui.strong("OS");
                        ui.strong("Client version");
                        ui.strong("Last seen");
                        ui.end_row();
                        for m in machines {
                            ui.label(&m.name);
                            ui.label(m.os.as_deref().unwrap_or("--"));
                            ui.label(m.client_version.as_deref().unwrap_or("--"));
                            ui.label(format_relative(m.last_seen_at));
                            ui.end_row();
                        }
                    });
            }
        }

        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "Per-folder sync health isn't queryable from the API yet -- \
                 PUT /machines/{id}/sync-status is ingest-only today.",
            )
            .weak()
            .italics(),
        );
    }
}
