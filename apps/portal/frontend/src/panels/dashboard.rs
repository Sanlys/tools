use api_types::Health;
use platform_config::{DashboardStatus, JsonResource};
use platform_core::{Panel, PanelId};

const REFRESH_INTERVAL_SECS: f64 = 10.0;

/// Shows the health of every tool's backend, combining an HTTP health-check
/// hit and Kubernetes Deployment readiness -- both computed server-side by
/// `apps/portal/backend`'s `GET /api/status` (see `docs/observability.md`
/// for how that in turn feeds Prometheus/Grafana).
#[derive(Default)]
pub struct DashboardPanel {
    status: JsonResource<DashboardStatus>,
    last_value: Option<DashboardStatus>,
    last_error: Option<String>,
    last_fetch_at: Option<f64>,
}

impl Panel for DashboardPanel {
    fn id(&self) -> PanelId {
        "dashboard"
    }

    fn title(&self) -> &str {
        "Dashboard"
    }

    fn tick(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let due = match self.last_fetch_at {
            None => true,
            Some(last) => now - last > REFRESH_INTERVAL_SECS,
        };
        if due && !self.status.is_loading() {
            self.status.fetch("/api/status");
            self.last_fetch_at = Some(now);
        }
        // Keep the UI ticking even when this panel isn't being interacted
        // with, so the periodic refetch above actually fires.
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Backend status");

        if let Some(result) = self.status.ready() {
            match result {
                Ok(value) => {
                    self.last_value = Some(value.clone());
                    self.last_error = None;
                }
                Err(err) => self.last_error = Some(err.clone()),
            }
        }

        match &self.last_value {
            None => {
                if let Some(err) = &self.last_error {
                    ui.colored_label(egui::Color32::RED, err);
                } else {
                    ui.spinner();
                }
            }
            Some(statuses) => {
                if let Some(err) = &self.last_error {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        format!("last refresh failed, showing stale data: {err}"),
                    );
                }
                egui::Grid::new("dashboard_grid")
                    .num_columns(4)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong("tool");
                        ui.strong("status");
                        ui.strong("http");
                        ui.strong("k8s");
                        ui.end_row();

                        for status in statuses {
                            let (color, label) = match status.overall {
                                Health::Healthy => (egui::Color32::from_rgb(0, 200, 0), "healthy"),
                                Health::Degraded => {
                                    (egui::Color32::from_rgb(220, 170, 0), "degraded")
                                }
                                Health::Down => (egui::Color32::from_rgb(220, 0, 0), "down"),
                                Health::Unknown => (egui::Color32::GRAY, "unknown"),
                            };
                            ui.label(&status.name);
                            ui.colored_label(color, label);
                            match &status.http_check {
                                Some(check) => ui.label(format!(
                                    "{} ({}ms)",
                                    if check.healthy { "ok" } else { "fail" },
                                    check.latency_ms.unwrap_or(0)
                                )),
                                None => ui.label("-"),
                            };
                            match &status.k8s_readiness {
                                Some(k8s) => ui.label(format!(
                                    "{}/{}",
                                    k8s.ready_replicas, k8s.desired_replicas
                                )),
                                None => ui.label("-"),
                            };
                            ui.end_row();
                        }
                    });
            }
        }
    }
}
