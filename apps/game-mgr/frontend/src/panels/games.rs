//! Games tab: catalog list + per-game session history
//! (`GET /games`, `GET /sessions?game_id=`).

use super::format_playtime;
use crate::GameMgrPanel;

impl GameMgrPanel {
    pub(crate) fn ui_games(&mut self, ui: &mut egui::Ui) {
        let games = match self.games.ready() {
            None => {
                ui.spinner();
                ui.label("loading games...");
                return;
            }
            Some(Err(err)) => {
                ui.colored_label(egui::Color32::RED, format!("failed to load games: {err}"));
                return;
            }
            Some(Ok(games)) => games.clone(),
        };

        if games.is_empty() {
            ui.label(egui::RichText::new("No games registered yet -- add one from the desktop client's \"Add Game\" flow.").weak());
            return;
        }

        egui::Grid::new("games_grid")
            .striped(true)
            .num_columns(6)
            .show(ui, |ui| {
                ui.strong("Title");
                ui.strong("Class");
                ui.strong("Version");
                ui.strong("Playtime");
                ui.strong("Sessions");
                ui.strong("Artifacts");
                ui.end_row();

                for g in &games {
                    let selected = self.selected_game.as_deref() == Some(g.definition.id.as_str());
                    if ui.selectable_label(selected, &g.definition.title).clicked() {
                        if selected {
                            self.selected_game = None;
                        } else {
                            self.selected_game = Some(g.definition.id.clone());
                            self.game_sessions = platform_config::JsonResource::new();
                        }
                    }
                    ui.label(&g.definition.class);
                    ui.label(&g.definition.version);
                    ui.label(format_playtime(g.total_playtime_s));
                    ui.label(g.session_count.to_string());
                    ui.label(g.definition.artifacts.len().to_string());
                    ui.end_row();
                }
            });

        let Some(game_id) = self.selected_game.clone() else {
            return;
        };
        ui.add_space(8.0);
        ui.separator();
        ui.label(egui::RichText::new(format!("Recent sessions: {game_id}")).strong());
        match self.game_sessions.ready() {
            None => {
                ui.spinner();
            }
            Some(Err(err)) => {
                ui.colored_label(
                    egui::Color32::RED,
                    format!("failed to load sessions: {err}"),
                );
            }
            Some(Ok(sessions)) if sessions.is_empty() => {
                ui.label(egui::RichText::new("No sessions recorded for this game yet.").weak());
            }
            Some(Ok(sessions)) => {
                egui::Grid::new("sessions_grid")
                    .striped(true)
                    .num_columns(4)
                    .show(ui, |ui| {
                        ui.strong("Started");
                        ui.strong("Duration");
                        ui.strong("Exit code");
                        ui.strong("End reason");
                        ui.end_row();
                        for s in sessions {
                            ui.label(
                                s.started_at
                                    .format(&time::format_description::well_known::Rfc3339)
                                    .unwrap_or_else(|_| s.started_at.to_string()),
                            );
                            ui.label(format_playtime(s.duration_s as i64));
                            ui.label(
                                s.exit_code
                                    .map(|c| c.to_string())
                                    .unwrap_or_else(|| "--".to_string()),
                            );
                            ui.label(s.end_reason.as_str());
                            ui.end_row();
                        }
                    });
            }
        }
    }
}
