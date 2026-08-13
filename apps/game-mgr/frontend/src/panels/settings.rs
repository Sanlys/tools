//! Settings/About tab: signed-in identity (`GET /me`).

use crate::GameMgrPanel;

impl GameMgrPanel {
    pub(crate) fn ui_settings(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Account").strong());
        match self.me.ready() {
            None => {
                ui.spinner();
            }
            Some(Err(err)) => {
                ui.colored_label(
                    egui::Color32::RED,
                    format!("failed to load identity: {err}"),
                );
            }
            Some(Ok(me)) => {
                ui.label(format!(
                    "Signed in as: {}",
                    me.user.display_name.as_deref().unwrap_or(&me.user.sub)
                ));
                ui.label(format!("Owned profiles: {}", me.profiles.len()));
            }
        }

        ui.add_space(12.0);
        ui.separator();
        ui.label(egui::RichText::new("About").strong());
        ui.label(format!("API base: {}", self.api_base_url));
        match self.ping.ready() {
            None => {
                ui.label(egui::RichText::new("Backend build: loading...").weak());
            }
            Some(Err(err)) => {
                ui.colored_label(
                    egui::Color32::RED,
                    format!("failed to load backend build info: {err}"),
                );
            }
            Some(Ok(ping)) => {
                ui.label(format!(
                    "Backend build: {} -- v{} ({})",
                    ping.build, ping.version, ping.status
                ));
            }
        }
        ui.label(
            egui::RichText::new(
                "Ported from the standalone game-mgr project -- see that repo's PLAN.md for \
                 the desktop client's design (installs, launch, save sync).",
            )
            .weak(),
        );
    }
}
