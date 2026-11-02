//! SiliconBoard — entry point, Phase 4 edition.
//!
//!## Phase 4 scope
//! Boots an eframe 0.37 window with the full Nibbler-style central-panel
//! board view. Pieces are the cburnett SVG set rasterised at startup
//! (Phase 1 BIS); the board texture is `public/images/board/blue.png`
//! (also Phase 1 BIS). The widget supports click-click and drag-and-drop
//! input, highlights legal destinations, plays them via
//! `GameState::apply_user_move`, and surfaces the promotion modal
//! action for the caller to resolve.
//!
//! Engine integration (Phase 5 / 6) will use the same board widget; the
//! Phase-1-BIS readiness preview is now a small status block in the
//! right-hand sidebar (which will host the eval dashboard in Phase 5).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // noop on macOS

use eframe::egui;

mod assets;
mod chess;
mod engine;
mod gui;

use assets::Assets;
use chess::GameState;
use gui::board_view::{BoardAction, BoardView};

/// Application identifier for the eframe persistence layer.
const APP_NAME: &str = "SiliconBoard";

/// Initial window size (Phase 4 board + sidebar layout target).
const INITIAL_WINDOW_SIZE: [f32; 2] = [900.0, 720.0];

/// Top-level eframe application state.
struct SiliconBoardApp {
    /// Loaded asset bundle. `None` until the first frame resolves cleanly
    /// so we can show a "loading…" indicator.
    assets: Option<Result<Assets, String>>,
    /// Live game state — the single source of truth for the board.
    game: GameState,
    /// Widget-side view state (selection, last-move highlight, orientation).
    view: BoardView,
    /// Status line shown in the right-hand sidebar.
    status: String,
}

impl Default for SiliconBoardApp {
    fn default() -> Self {
        Self {
            assets: None,
            game: GameState::new(),
            view: BoardView::default(),
            status: "Welcome — click or drag a piece to make a move.".to_string(),
        }
    }
}

impl eframe::App for SiliconBoardApp {
    /// Background frame — kick off asset loading so the visual pass can
    /// show the board once decoding is finished.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.assets.is_none() {
            match Assets::load(ctx) {
                Ok(a) => self.assets = Some(Ok(a)),
                Err(e) => self.assets = Some(Err(format!("{}", e))),
            }
        }
    }

    /// Visual frame — Phase-4 board layout + sidebar.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // ── Top menu placeholder strip ──
        egui::Panel::top("menu_placeholder").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("SiliconBoard");
                ui.separator();
                ui.label("Phase 4 — Board & Interaction");
            });
        });

        // ── Right sidebar (replaced by the eval dashboard in Phase 5) ──
        egui::Panel::right("side_status").show(ui, |ui| {
            ui.heading("SiliconBoard");
            ui.add_space(8.0);
            ui.label(egui::RichText::new(&self.status).weak());
            ui.add_space(8.0);
            ui.separator();
            if ui.button("Flip board").clicked() {
                self.view.flip();
            }
            if ui.button("Reset board view").clicked() {
                self.view.reset();
            }
            ui.add_space(8.0);
            ui.label("FEN:");
            ui.monospace(self.game.fen());
            if ui.button("Copy FEN").clicked() {
                ui.ctx().copy_text(self.game.fen());
                self.status = "FEN copied to clipboard.".to_string();
            }
            ui.add_space(8.0);
            ui.label("Move history:");
            ui.monospace(self.game.history_string());
        });

        // ── Central board panel ──
        egui::CentralPanel::default().show(ui, |central_ui| {
            match &self.assets {
                None => {
                    central_ui.vertical_centered(|ui| {
                        ui.add_space(24.0);
                        ui.label("Loading piece + board assets…");
                    });
                }
                Some(Err(err)) => {
                    central_ui.vertical_centered(|ui| {
                        ui.add_space(24.0);
                        ui.colored_label(egui::Color32::RED, "Asset load failed:");
                        ui.label(err);
                    });
                }
                Some(Ok(assets)) => {
                    let board_action = gui::board_view::show(
                        central_ui,
                        &self.game,
                        assets,
                        &mut self.view,
                    );
                    apply_interface(board_action, &mut self.game, &mut self.view, &mut self.status);
                }
            }
            // Request a repaint so the engine's status / drag animation /
            // future eval updates flow into the GUI smoothly. Phase 5 / 6
            // replaces this with event-driven repaints when idle.
            central_ui.ctx().request_repaint();
        });
    }
}

/// Apply a [`BoardAction`] to the mutable state (GameState + view + status).
fn apply_interface(
    action: BoardAction,
    game: &mut GameState,
    view: &mut BoardView,
    status: &mut String,
) {
    match action {
        BoardAction::None => {}
        BoardAction::PieceSelected { from } => {
            *status = format!("selected square {}", from);
        }
        BoardAction::RequestMove { m } => match game.apply_move(m) {
            Ok(()) => {
                *status = format!(
                    "played {} | history: {}",
                    m.to_string(),
                    game.history_string()
                );
            }
            Err(e) => {
                *status = format!("move error: {}", e);
            }
        },
        BoardAction::PromotionChoice { .. } => {
            // The promotion modal is drawn on the board itself; this action
            // only fires on the frame the modal opens.
            *status = "promotion — pick Q / R / B / N on the board (Esc cancels)".into();
        }
    }
}

fn main() -> eframe::Result<()> {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();

    log::info!("SiliconBoard starting up — booting eframe on wgpu/Metal");

    let viewport = egui::ViewportBuilder::default().with_inner_size(INITIAL_WINDOW_SIZE);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        options,
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::<SiliconBoardApp>::default())
        }),
    )
}
