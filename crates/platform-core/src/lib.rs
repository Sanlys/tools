//! Shared UI contract every tool implements to plug into the unified `portal` app.
//!
//! There is no runtime plugin loading: the set of panels compiled into the
//! unified wasm binary is a closed, compile-time-known list. Each tool's
//! frontend crate implements [`Panel`] on its own state type; `apps/portal`
//! then lists every tool's panel type as a variant of its `ToolPanel` enum
//! (see `apps/portal/frontend/src/lib.rs`) and adding a tool means adding a
//! line there, not registering anything at runtime.
//!
//! `ToolPanel` dispatches to each variant by hand (a small `match`-based
//! macro) rather than via the `enum_dispatch` crate: that crate's
//! trait/enum linkage is cached per-compilation-unit, so it silently
//! produces no impl when the trait (here) and the enum (in `apps/portal`)
//! live in different crates. A hand-written match compiles to the same
//! static dispatch without that footgun.

/// Stable identifier for a panel: used in navigation, URL fragments (for the
/// wasm build) and persisted UI state. Keep these short and kebab-case.
pub type PanelId = &'static str;

/// A single feature/tool surface hostable inside the unified egui app.
///
/// Implement this on your tool's panel state, then add it as a variant to
/// `apps/portal/frontend`'s `ToolPanel` enum. The same type is reused by a
/// tool's standalone binary via [`standalone::run`]/[`standalone::run_web`].
pub trait Panel {
    /// Stable id, e.g. `"hello"`. Must be unique across all registered tools.
    fn id(&self) -> PanelId;

    /// Human-readable name shown in the portal's navigation sidebar.
    fn title(&self) -> &str;

    /// Draw this panel's contents. Called every frame while the panel is the
    /// active/open one in the host (portal or standalone).
    fn ui(&mut self, ui: &mut egui::Ui);

    /// Called once per frame the panel is mounted in its host, independently
    /// of whether [`ui`](Self::ui) also runs that frame -- for background
    /// work such as polling a backend that should keep running even while
    /// the user isn't looking at this panel's contents (e.g. it's one of
    /// several open at once and a different one currently has focus). This
    /// is `standalone::run`/`run_web`'s host loop's contract exactly: there
    /// is only ever one panel, so it always ticks.
    ///
    /// The portal, which can host several panels at once each in its own
    /// closable `egui::Window`, additionally pauses ticking a panel a user
    /// has explicitly closed (not just defocused) -- see
    /// `apps/portal/frontend/src/lib.rs`'s panel loop. That's a deliberate
    /// choice by that particular host (no point polling a backend for a
    /// panel nobody asked to see anymore), not a violation of this
    /// contract: reopening the same panel resumes ticking it.
    ///
    /// Default is a no-op.
    fn tick(&mut self, ctx: &egui::Context) {
        let _ = ctx;
    }
}

/// Helpers for running a single [`Panel`] as its own standalone native/wasm
/// binary, outside of the unified portal host. Every tool gets this for free
/// by depending on `platform-core` and calling `standalone::run` (native) or
/// `standalone::run_web` (wasm, from a `#[wasm_bindgen(start)]` entry point
/// -- see `apps/hello/frontend/src/lib.rs` for the reference wiring, along
/// with its `index.html`/`Trunk.toml` and `apps/hello/backend`'s Dockerfile,
/// which builds and serves that wasm bundle so the tool's own ingress host
/// renders its panel directly instead of just exposing a bare API).
pub mod standalone {
    use crate::Panel;

    struct StandaloneApp<P: Panel> {
        panel: P,
    }

    impl<P: Panel> eframe::App for StandaloneApp<P> {
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            self.panel.tick(ctx);
            egui::CentralPanel::default().show(ctx, |ui| {
                self.panel.ui(ui);
            });
        }
    }

    /// Run `panel` as a native standalone window. Call this from the tool's
    /// own `src/bin/*.rs` (native only -- for the wasm build, see
    /// [`run_web`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn run<P: Panel + 'static>(panel: P) -> eframe::Result<()> {
        let title = panel.title().to_string();
        let options = eframe::NativeOptions::default();
        eframe::run_native(
            &title,
            options,
            Box::new(|_cc| Ok(Box::new(StandaloneApp { panel }))),
        )
    }

    /// Mount `panel` into a `<canvas id="{canvas_id}">` element in the
    /// hosting page, standalone (not embedded in the portal's unified wasm
    /// bundle). Call this from a `#[wasm_bindgen(start)]` function in the
    /// tool's own frontend crate -- panic-hook/tracing setup and the
    /// `wasm_bindgen_futures::spawn_local` wrapper stay in that crate (same
    /// as `apps/portal/frontend` does today) since they're one-time,
    /// per-binary concerns, not something worth threading through here.
    #[cfg(target_arch = "wasm32")]
    pub async fn run_web<P: Panel + 'static>(
        canvas_id: &str,
        panel: P,
    ) -> Result<(), wasm_bindgen::JsValue> {
        use wasm_bindgen::JsCast as _;

        let canvas = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id(canvas_id))
            .unwrap_or_else(|| panic!("index.html must contain a <canvas id=\"{canvas_id}\">"))
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .unwrap_or_else(|_| panic!("#{canvas_id} must be a <canvas> element"));

        eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|_cc| Ok(Box::new(StandaloneApp { panel }))),
            )
            .await
    }
}
