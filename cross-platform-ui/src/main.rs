mod app;
mod commands_model;
mod console_model;
mod debug_model;
mod engine_client;
mod logging;
mod metadata_model;
mod platform;
mod protocol;
mod settings;
mod theme;

use app::Ps7IsePrototypeApp;

fn main() -> eframe::Result<()> {
    let log_path = logging::init();
    logging::log(&format!("starting prototype; log={}", log_path.display()));
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_title("PS7 ISE Cross-Platform Prototype"),
        ..Default::default()
    };
    eframe::run_native(
        "PS7 ISE Prototype",
        options,
        Box::new(|_cc| Box::new(Ps7IsePrototypeApp::new())),
    )
}
