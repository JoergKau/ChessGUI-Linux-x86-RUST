//! SiliconBoard — entry point, Phase 1 BIS edition.
//!
//! ## Phase 1 BIS scope
//! Boots an eframe 0.37 window, loads every asset via the `Assets` bundle
//! (board PNG from `public/images/board/blue.png` + twelve cburnett SVG
//! pieces rasterised by `usvg`+`resvg`+`tiny-skia`), and renders a small
//! "Phase 1 BIS readiness check" panel that displays the board thumbnail
//! plus each piece in two horizontal rows.
//!
//! Full UI (drag-and-drop board, eval dashboard, PGN list, menus) lands in
//! Phase 4 / Phase 5.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // noop on macOS

use eframe::egui;

mod assets;
mod chess;
mod engine;
mod gui;

use assets::Assets;
use chess::{PieceColor, PieceKind};

/// Application identifier for the eframe persistence layer.
const APP_NAME: &str = "SiliconBoard";

/// Initial window size for the Phase 1 BIS preview window.
const INITIAL_WINDOW_SIZE: [f32; 2] = [760.0, 480.0];

/// Pixel size of each piece in the preview rows.
const PREVIEW_TILE_SIZE: f32 = 48.0;

/// Side length of the board thumbnail in the readiness panel.
const BOARD_THUMBNAIL_SIZE: f32 = 128.0;

/// Top-level eframe application state.
struct SiliconBoardApp {
    /// Loaded asset bundle. `None` until the first frame so we can show a
    /// "loading…" indicator cleanly.
    assets: Option<Result<Assets, String>>,
}

impl Default for SiliconBoardApp {
    fn default() -> Self {
        Self { assets: None }
    }
}

impl eframe::App for SiliconBoardApp {
    /// Background frame — kick off asset loading so the visual pass can
    /// show thumbs once decoding is finished.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.assets.is_none() {
            match Assets::load(ctx) {
                Ok(a) => self.assets = Some(Ok(a)),
                Err(e) => self.assets = Some(Err(format!("{}", e))),
            }
        }
    }

    /// Visual frame — assemble the Phase-1-BIS preview UI.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Top title strip (replaced by the full menu bar in Phase 5).
        egui::Panel::top("menu_placeholder").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("SiliconBoard");
                ui.separator();
                ui.label("Phase 1 BIS — Public Assets (cburnett / blue board)");
            });
        });

        // Central content panel.
        egui::CentralPanel::default()
            .frame(
                egui::Frame::group(ui.style())
                    .inner_margin(egui::Margin::same(16))
                    .corner_radius(8.0),
            )
            .show(ui, |ui| {
                ui.heading("Phase 1 BIS readiness check");
                ui.add_space(8.0);
                match &self.assets {
                    None => {
                        ui.label("Loading assets…");
                        ui.ctx().request_repaint();
                    }
                    Some(Err(err)) => {
                        ui.colored_label(
                            egui::Color32::RED,
                            "Asset load failed:",
                        );
                        ui.label(err);
                        ui.add_space(8.0);
                        ui.label(
                            "Tip: ensure `public/` exists at the workspace root \
                             with `public/images/board/blue.png` and \
                             `public/piece/cburnett/{wK,wQ,wR,wB,wN,wP,bK,bQ,bR,bB,bN,bP}.svg`.",
                        );
                    }
                    Some(Ok(assets)) => {
                        // ── Status line ──
                        ui.colored_label(
                            egui::Color32::LIGHT_GREEN,
                            format!(
                                "✓ Loaded {} piece sprites from {}",
                                assets.pieces.len(),
                                assets.pieces_root().display()
                            ),
                        );
                        ui.colored_label(
                            egui::Color32::LIGHT_GREEN,
                            format!(
                                "✓ Loaded board {}x{} from {}",
                                assets.board.dimensions.0,
                                assets.board.dimensions.1,
                                assets.board.source().strip_prefix(
                                    assets.asset_root.parent().unwrap_or(&assets.asset_root)
                                ).unwrap_or(assets.board.source()).display()
                            ),
                        );
                        ui.add_space(12.0);

                        // ── Two-column layout: board thumb + piece preview ──
                        ui.horizontal(|ui| {
                            // Board thumbnail column.
                            ui.vertical(|ui| {
                                ui.label("Board (default: blue.png)");
                                let handle = assets.board.handle();
                                ui.add(
                                    egui::Image::new((
                                        handle.id(),
                                        egui::vec2(BOARD_THUMBNAIL_SIZE, BOARD_THUMBNAIL_SIZE),
                                    ))
                                    .alt_text("chess board")
                                    .maintain_aspect_ratio(true),
                                );
                                ui.label(
                                    egui::RichText::new(
                                        format!("{}×{} px", assets.board.dimensions.0, assets.board.dimensions.1),
                                    )
                                    .weak(),
                                );
                            });

                            ui.add_space(16.0);

                            // Piece preview column — two rows.
                            ui.vertical(|ui| {
                                ui.label("Pieces (cburnett → 512×512 RGBA8)");
                                for color in [PieceColor::White, PieceColor::Black] {
                                    ui.horizontal(|ui| {
                                        ui.label(match color {
                                            PieceColor::White => "White:",
                                            PieceColor::Black => "Black:",
                                        });
                                        for kind in [
                                            PieceKind::Pawn,
                                            PieceKind::Knight,
                                            PieceKind::Bishop,
                                            PieceKind::Rook,
                                            PieceKind::Queen,
                                            PieceKind::King,
                                        ] {
                                            let id = assets::PieceId { color, kind };
                                            if let Some(handle) =
                                                assets.pieces.get(id)
                                            {
                                                ui.add(
                                                    egui::Image::new((
                                                        handle.id(),
                                                        egui::vec2(
                                                            PREVIEW_TILE_SIZE,
                                                            PREVIEW_TILE_SIZE,
                                                        ),
                                                    ))
                                                    .alt_text(format!("{:?}", id)),
                                                );
                                            } else {
                                                ui.colored_label(
                                                    egui::Color32::RED,
                                                    format!("missing {:?}", id),
                                                );
                                            }
                                        }
                                    });
                                }
                            });
                        });

                        ui.add_space(8.0);
                        ui.separator();
                        ui.label(
                            "Asset root resolved from workspace `public/` directory. \
                             Override with `SILICONBOARD_PUBLIC_ROOT=/path/to/public`.",
                        );
                        ui.label(
                            "Full board, eval dashboard, PGN viewer and menus land in later phases.",
                        );
                    }
                }
            });
    }
}

fn main() -> eframe::Result<()> {
    // Initialize logging so Phase-1-BIS texture upload messages are visible.
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
