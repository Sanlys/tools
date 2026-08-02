mod app;

fn main() -> eframe::Result {
    // keep the guard alive for the whole process so file logs flush on exit
    let _log_guard = game_mgr_core::init_tracing("gm-client");

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_title("game-mgr"),
        ..Default::default()
    };
    eframe::run_native(
        "game-mgr",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
