// Hide the console window in release builds (Windows-only attribute).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod git;
mod theme;
mod ui;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("GITGUD")
            .with_inner_size([1380.0, 860.0])
            .with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "gitgud",
        options,
        Box::new(|cc| Ok(Box::new(app::GitGudApp::new(cc)))),
    )
}
