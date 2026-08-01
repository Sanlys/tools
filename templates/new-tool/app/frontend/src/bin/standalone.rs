//! Native standalone entry point for {{project-name}}'s egui UI, run
//! outside the unified portal -- see `docs/adding-a-tool.md`.

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let api_base_url = std::env::var("{{project-name | shouty_snake_case}}_API_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:{{container_port}}".to_string());
    let panel = {{crate_name}}_frontend::{{project-name | pascal_case}}Panel::new(api_base_url);
    if let Err(err) = platform_core::standalone::run(panel) {
        eprintln!("{{project-name}}-standalone exited with error: {err}");
        std::process::exit(1);
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {
    panic!("{{project-name}}-standalone is a native-only binary; see apps/portal/frontend for the wasm build");
}
