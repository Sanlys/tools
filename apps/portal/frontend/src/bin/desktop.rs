//! Desktop build of the unified portal: every panel (Home, Dashboard,
//! Account, and every tool with a compiled-in `ToolPanel`) in a native
//! window, no browser required. The real *deployment* target is still wasm
//! (see `apps/portal/frontend/index.html` and `apps/portal/backend`, which
//! serves the compiled wasm bundle) -- this is the alternate, equally-real
//! way to run the exact same `PortalApp`, shipped as its own downloadable
//! binary via `.github/workflows/release.yml`'s `desktop` job (same
//! treatment `game-mgr-client` already gets).
//!
//! Signing in reuses the *same* "portal" OIDC client_id as the wasm build
//! (see `deploy/idp/values.yaml`'s `IDP_CLIENTS_JSON`, which marks it
//! `native: true` for exactly this) rather than registering a second
//! desktop-only client -- same account, same session semantics, matching
//! how a lot of desktop apps hand the actual login UI to "your browser"
//! and reuse whatever web identity comes back. Concretely that means
//! `auth_adapter::frontend_native`'s RFC 8252 loopback-redirect flow: this
//! binary opens your system browser to the IDP's `/oauth/authorize`, and a
//! short-lived localhost listener catches the redirect back -- see that
//! module's docs. The resulting refresh token is cached in the OS keyring,
//! so signing in once persists across restarts, same as `hello-standalone`.

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // Defaults to the real deployed portal backend rather than a local dev
    // address: this binary is what `release.yml` ships for people to
    // actually run, and a silent-but-wrong default here would be
    // indistinguishable from a real bug (the tool registry/dashboard would
    // just never load, and sign-in would hang against an empty
    // issuer_url) -- see `apps/hello/frontend/src/bin/standalone.rs`'s
    // identical reasoning. Override with PORTAL_API_BASE_URL when
    // developing against a local `portal-backend`.
    let api_base_url = std::env::var("PORTAL_API_BASE_URL")
        .unwrap_or_else(|_| "https://portal.k8s.lysakermoen.com".to_string());

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_title("Tools Platform"),
        ..Default::default()
    };

    eframe::run_native(
        "Tools Platform",
        options,
        Box::new(|_cc| Ok(Box::new(portal_frontend::PortalApp::new(api_base_url)))),
    )
}

// Keeps `cargo build --workspace --target wasm32-unknown-unknown` green even
// though this binary only makes sense natively; the wasm build is produced
// by this same crate's `lib.rs` (`#[wasm_bindgen(start)]`), served by
// `apps/portal/backend`.
#[cfg(target_arch = "wasm32")]
fn main() {
    panic!("portal-desktop is a native-only binary; build apps/portal/frontend's lib with trunk for wasm");
}
