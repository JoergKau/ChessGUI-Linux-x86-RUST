//! SiliconBoard — entry point.
//!
//! Boots eframe on wgpu/Metal, creates the Tokio runtime that serves engines
//! and background I/O, and hands both to [`app::SiliconBoardApp`]. All the
//! behaviour lives in the modules below.

use eframe::egui;

mod app;
mod assets;
mod chess;
mod engine;
mod gui;

use app::SiliconBoardApp;

/// Application identifier for the eframe window.
const APP_NAME: &str = "SiliconBoard";

/// Initial window size (board, evaluation dashboard and move list).
const INITIAL_WINDOW_SIZE: [f32; 2] = [1280.0, 800.0];

/// Smallest window that still shows the top bar, the board and both engine cards.
const MIN_WINDOW_SIZE: [f32; 2] = [960.0, 640.0];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();

    log::info!("SiliconBoard starting up — booting eframe on wgpu/Metal");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("siliconboard-io")
        .build()?;

    let viewport = egui::ViewportBuilder::default()
        .with_inner_size(INITIAL_WINDOW_SIZE)
        .with_min_inner_size(MIN_WINDOW_SIZE);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            gui::theme::apply(&cc.egui_ctx);
            Ok(Box::new(SiliconBoardApp::new(runtime)))
        }),
    )?;
    Ok(())
}
