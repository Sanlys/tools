//! Egui panel for {{project-name}} -- {{description}}
//!
//! Generated from `templates/new-tool`. See `apps/hello/frontend` for a
//! more fleshed-out reference (a text field that posts to the backend, a
//! status fetch, etc). Talks to the backend purely over HTTP/websocket, so
//! the exact same code runs natively and compiled to wasm in the portal --
//! and, via `start()` at the bottom of this file plus this crate's
//! `index.html`/`Trunk.toml`, standalone too (built and served by
//! `apps/{{project-name}}/backend`'s Dockerfile, same as `apps/hello`'s).

use platform_config::JsonResource;
use platform_core::{Panel, PanelId};
use serde::Deserialize;

/// Mirrors `apps/{{project-name}}/backend`'s `GET /api/status` response.
#[derive(Debug, Clone, Deserialize)]
struct Status {
    message: String,
    bucket_object_count: u64,
}

pub struct {{project-name | pascal_case}}Panel {
    api_base_url: String,
    status: JsonResource<Status>,
}

impl {{project-name | pascal_case}}Panel {
    pub fn new(api_base_url: impl Into<String>) -> Self {
        Self {
            api_base_url: api_base_url.into(),
            status: JsonResource::new(),
        }
    }

    fn status_url(&self) -> String {
        format!("{}/api/status", self.api_base_url.trim_end_matches('/'))
    }
}

impl Panel for {{project-name | pascal_case}}Panel {
    fn id(&self) -> PanelId {
        "{{project-name}}"
    }

    fn title(&self) -> &str {
        "{{project-name | title_case}}"
    }

    fn tick(&mut self, _ctx: &egui::Context) {
        if !self.status.has_requested() {
            self.status.fetch(&self.status_url());
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("{{project-name | title_case}}");
        ui.label("{{description}}");
        ui.separator();

        match self.status.ready() {
            None => {
                ui.spinner();
            }
            Some(Ok(status)) => {
                ui.label(&status.message);
                ui.label(format!("objects in bucket: {}", status.bucket_object_count));
                if ui.button("Refresh").clicked() {
                    self.status = JsonResource::new();
                }
            }
            Some(Err(err)) => {
                ui.colored_label(egui::Color32::RED, format!("backend error: {err}"));
            }
        }
    }
}

/// Mounts this tool standalone into the `<canvas id="the_canvas_id">` in
/// this crate's `index.html`, talking to `{{project-name}}-backend` on the
/// same origin that serves this bundle -- see `apps/hello/frontend/src/lib.rs`
/// for the reference version of this same wiring.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();

    wasm_bindgen_futures::spawn_local(async {
        platform_core::standalone::run_web("the_canvas_id", {{project-name | pascal_case}}Panel::new(""))
            .await
            .expect("failed to start eframe");
    });
}
