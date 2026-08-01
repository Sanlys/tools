//! Egui panel for the `hello` example tool.
//!
//! This crate is the reference implementation of the "every tool has an
//! egui UI that plugs into the unified portal *and* can run standalone"
//! pattern (see `docs/adding-a-tool.md`). It talks to its own backend
//! (`apps/hello/backend`) purely over HTTP -- no direct DB/S3 access from
//! the frontend, which is what lets the exact same code run both natively
//! and compiled to wasm in the browser.
//!
//! The backend's base URL is passed in by whoever hosts this panel (the
//! portal, or `src/bin/standalone.rs`) rather than hard-coded, since the
//! portal resolves it at runtime from `/config/tools.json`
//! ([`platform_config::ToolRegistry`]) instead of baking it into the wasm
//! binary at compile time.

use platform_config::JsonResource;
use platform_core::{Panel, PanelId};
use serde::{Deserialize, Serialize};

/// Mirrors `apps/hello/backend`'s `GET /api/status` response. Duplicated
/// (rather than shared via a common crate) deliberately: it's two small
/// structs, and keeping backend and frontend decoupled at the type level
/// means the backend never has to compile-depend on egui/eframe.
#[derive(Debug, Clone, Deserialize)]
struct HelloStatus {
    message: String,
    greeting_count: i64,
    bucket_object_count: u64,
}

#[derive(Debug, Serialize)]
struct NewGreeting<'a> {
    name: &'a str,
}

pub struct HelloPanel {
    api_base_url: String,
    status: JsonResource<HelloStatus>,
    name_input: String,
    last_error: Option<String>,
}

impl HelloPanel {
    pub fn new(api_base_url: impl Into<String>) -> Self {
        Self {
            api_base_url: api_base_url.into(),
            status: JsonResource::new(),
            name_input: String::new(),
            last_error: None,
        }
    }

    fn status_url(&self) -> String {
        format!("{}/api/status", self.api_base_url.trim_end_matches('/'))
    }

    fn post_greeting(&mut self) {
        if self.name_input.trim().is_empty() {
            return;
        }
        let url = format!("{}/api/greetings", self.api_base_url.trim_end_matches('/'));
        let body = match serde_json::to_vec(&NewGreeting {
            name: self.name_input.trim(),
        }) {
            Ok(body) => body,
            Err(err) => {
                self.last_error = Some(format!("failed to encode request: {err}"));
                return;
            }
        };
        let mut request = ehttp::Request::post(url, body);
        request.headers = ehttp::Headers::new(&[
            ("Accept", "application/json"),
            ("Content-Type", "application/json"),
        ]);
        ehttp::fetch(request, |_response| {
            // Fire-and-forget: the next `tick()` re-fetches status, which
            // will reflect the write once the response lands.
        });
        self.name_input.clear();
        // Force a fresh status fetch on the next tick rather than waiting
        // out whatever poll interval `tick()` uses.
        self.status = JsonResource::new();
    }
}

impl Panel for HelloPanel {
    fn id(&self) -> PanelId {
        "hello"
    }

    fn title(&self) -> &str {
        "Hello"
    }

    fn tick(&mut self, _ctx: &egui::Context) {
        if !self.status.has_requested() {
            self.status.fetch(&self.status_url());
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Hello");
        ui.label(
            "Example tool: proves the egui-panel + standalone-backend + \
             Postgres + S3 pattern end to end.",
        );
        ui.separator();

        ui.horizontal(|ui| {
            ui.label("Your name:");
            ui.text_edit_singleline(&mut self.name_input);
            if ui.button("Say hello").clicked() {
                self.post_greeting();
            }
        });

        if let Some(err) = &self.last_error {
            ui.colored_label(egui::Color32::RED, err);
        }

        ui.separator();

        match self.status.ready() {
            None => {
                ui.spinner();
                ui.label("contacting backend...");
            }
            Some(Ok(status)) => {
                ui.label(&status.message);
                ui.label(format!(
                    "greetings recorded in Postgres: {}",
                    status.greeting_count
                ));
                ui.label(format!(
                    "objects in this tool's S3 bucket: {}",
                    status.bucket_object_count
                ));
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
