//! Native build of the portal, mainly for local development without
//! needing `trunk`/a browser. The real deployment target is wasm (see
//! `apps/portal/frontend/index.html` and `apps/portal/backend`, which
//! serves the compiled wasm bundle).

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    eframe::run_native(
        "Tools Platform",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(portal_frontend::PortalApp::default()))),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {
    panic!("portal-native is a native-only binary; build apps/portal/frontend's lib with trunk for wasm");
}
