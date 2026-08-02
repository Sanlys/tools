//! Native standalone entry point for `game-mgr`'s egui UI, run outside the
//! unified portal. Every tool gets one of these thin bins for free via
//! `platform_core::standalone::run` -- see `docs/adding-a-tool.md`.

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    // Defaults to the real deployed backend rather than a local dev
    // address -- see apps/hello/frontend/src/bin/standalone.rs's comment on
    // why. Override with GAME_MGR_API_BASE_URL when developing against a
    // local `game-mgr-backend`.
    let api_base_url = std::env::var("GAME_MGR_API_BASE_URL")
        .unwrap_or_else(|_| "https://games.lysakermoen.com".to_string());
    if let Err(err) =
        platform_core::standalone::run(game_mgr_frontend::GameMgrPanel::new(api_base_url, false))
    {
        eprintln!("game-mgr-standalone exited with error: {err}");
        std::process::exit(1);
    }
}

// Keeps `cargo build --workspace --target wasm32-unknown-unknown` green even
// though this binary only makes sense natively; the wasm build is produced
// by `apps/portal/frontend`, which embeds this crate's `GameMgrPanel`
// instead.
#[cfg(target_arch = "wasm32")]
fn main() {
    panic!(
        "game-mgr-standalone is a native-only binary; see apps/portal/frontend for the wasm build"
    );
}
