//! Native standalone entry point for the `hello` tool's egui UI, run outside
//! the unified portal. Every tool gets one of these thin bins for free via
//! `platform_core::standalone::run` -- see `docs/adding-a-tool.md`.

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let api_base_url =
        std::env::var("HELLO_API_BASE_URL").unwrap_or_else(|_| "http://localhost:8081".to_string());
    if let Err(err) = platform_core::standalone::run(hello_frontend::HelloPanel::new(api_base_url))
    {
        eprintln!("hello-standalone exited with error: {err}");
        std::process::exit(1);
    }
}

// Keeps `cargo build --workspace --target wasm32-unknown-unknown` green even
// though this binary only makes sense natively; the wasm build is produced
// by `apps/portal/frontend`, which embeds this crate's `HelloPanel` instead.
#[cfg(target_arch = "wasm32")]
fn main() {
    panic!("hello-standalone is a native-only binary; see apps/portal/frontend for the wasm build");
}
