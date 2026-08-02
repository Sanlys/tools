//! Dashboard tab: total/top playtime and machine liveness. Built from
//! `GET /games` (which already carries `total_playtime_s`/`session_count`/
//! `last_played` per title, computed server-side) and `GET /machines` --
//! there is no `/stats/overview` endpoint in the ported backend (that was
//! design-doc scope that was never implemented, see `crate`'s module doc),
//! so this tab aggregates client-side from data the API actually serves.

use egui_plot::{Bar, BarChart, Plot};

use super::{format_playtime, format_relative};
use crate::GameMgrPanel;

impl GameMgrPanel {
    pub(crate) fn ui_dashboard(&mut self, ui: &mut egui::Ui) {
        match self.games.ready() {
            None => {
                ui.spinner();
                ui.label("loading games...");
                return;
            }
            Some(Err(err)) => {
                ui.colored_label(egui::Color32::RED, format!("failed to load games: {err}"));
                return;
            }
            Some(Ok(games)) => {
                let total: i64 = games.iter().map(|g| g.total_playtime_s).sum();
                ui.label(
                    egui::RichText::new(format!("Total playtime: {}", format_playtime(total)))
                        .strong(),
                );

                let mut by_playtime = games.clone();
                by_playtime.sort_by_key(|g| std::cmp::Reverse(g.total_playtime_s));
                by_playtime.truncate(8);

                if !by_playtime.is_empty() {
                    ui.add_space(8.0);
                    ui.label("Top games by playtime:");
                    let bars: Vec<Bar> = by_playtime
                        .iter()
                        .enumerate()
                        .map(|(i, g)| {
                            Bar::new(i as f64, g.total_playtime_s as f64 / 3600.0)
                                .name(g.definition.title.clone())
                        })
                        .collect();
                    Plot::new("top_games_playtime")
                        .height(180.0)
                        .show_x(false)
                        .allow_scroll(false)
                        .show(ui, |plot_ui| {
                            plot_ui.bar_chart(BarChart::new(bars).name("hours played"));
                        });
                    for (i, g) in by_playtime.iter().enumerate() {
                        ui.label(format!(
                            "{}. {} -- {} ({} sessions)",
                            i + 1,
                            g.definition.title,
                            format_playtime(g.total_playtime_s),
                            g.session_count
                        ));
                    }
                } else {
                    ui.label(egui::RichText::new("No games recorded yet.").weak());
                }

                let mut recent: Vec<_> = games.iter().filter(|g| g.last_played.is_some()).collect();
                recent.sort_by_key(|g| std::cmp::Reverse(g.last_played));
                if !recent.is_empty() {
                    ui.add_space(8.0);
                    ui.label("Recently played:");
                    for g in recent.into_iter().take(5) {
                        ui.label(format!(
                            "{} -- {}",
                            g.definition.title,
                            format_relative(g.last_played)
                        ));
                    }
                }
            }
        }

        ui.add_space(12.0);
        ui.separator();
        ui.label(egui::RichText::new("Machines").strong());
        match self.machines.ready() {
            None => {
                ui.spinner();
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
                for m in machines {
                    ui.label(format!(
                        "{} -- last seen {}",
                        m.name,
                        format_relative(m.last_seen_at)
                    ));
                }
            }
        }
    }
}
