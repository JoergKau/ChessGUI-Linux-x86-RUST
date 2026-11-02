//! Top menu bar (Game, File, Engine, Abort) and the FEN bar under it.
//!
//! The menu only reports what the user chose as a [`MenuAction`]; the app
//! performs it. Enabled/checked state comes from [`MenuState`].

use eframe::egui::{self, RichText, Ui};

use crate::gui::theme;

/// Something the user asked for in the menu bar or FEN bar.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MenuAction {
    /// Game → New game.
    NewGame,
    /// Game → Flip board.
    FlipBoard,
    /// File → Open PGN…
    OpenPgn,
    /// File → Save PGN…
    SavePgn,
    /// File → Copy PGN.
    CopyPgn,
    /// File → Copy FEN (also the FEN bar's Copy button).
    CopyFen,
    /// File → Save FEN…
    SaveFen,
    /// Engine → Load engine A… / B… (`0` = A, `1` = B).
    LoadEngine(usize),
    /// Engine → Analysis (checkbox).
    ToggleAnalysis,
    /// Engine → Load PolyGlot book…
    LoadBook,
    /// Abort: stop and quit every running engine.
    AbortAll,
}

/// Read-only facts the menu needs to draw its checkmarks and labels.
#[derive(Clone, Debug)]
pub struct MenuState<'a> {
    /// Whether analysis is switched on.
    pub analysis_enabled: bool,
    /// Whether each engine slot holds a running (or starting) engine.
    pub engine_loaded: [bool; 2],
    /// File name of the loaded opening book, if any.
    pub book_name: Option<&'a str>,
    /// FEN of the displayed position.
    pub fen: &'a str,
    /// Number of moves played, shown in the Game menu.
    pub ply_count: usize,
}

/// Draw the menu bar and the FEN bar. Returns the action chosen this frame.
pub fn show(ui: &mut Ui, state: &MenuState) -> Option<MenuAction> {
    let mut action = None;
    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button("Game", |ui| {
            if ui.button("New game").clicked() {
                action = Some(MenuAction::NewGame);
                ui.close();
            }
            if ui.button("Flip board").clicked() {
                action = Some(MenuAction::FlipBoard);
                ui.close();
            }
            ui.separator();
            ui.add_enabled(false, egui::Button::new(format!("{} half-moves played", state.ply_count)));
        });
        ui.menu_button("File", |ui| {
            if ui.button("Open PGN…").clicked() {
                action = Some(MenuAction::OpenPgn);
                ui.close();
            }
            if ui.button("Save PGN…").clicked() {
                action = Some(MenuAction::SavePgn);
                ui.close();
            }
            if ui.button("Copy PGN").clicked() {
                action = Some(MenuAction::CopyPgn);
                ui.close();
            }
            ui.separator();
            if ui.button("Copy FEN").clicked() {
                action = Some(MenuAction::CopyFen);
                ui.close();
            }
            if ui.button("Save FEN…").clicked() {
                action = Some(MenuAction::SaveFen);
                ui.close();
            }
        });
        ui.menu_button("Engine", |ui| {
            for slot in 0..2 {
                let letter = char::from(b'A' + slot as u8);
                let label = if state.engine_loaded[slot] {
                    format!("Replace engine {}…", letter)
                } else {
                    format!("Load engine {}…", letter)
                };
                if ui.button(label).clicked() {
                    action = Some(MenuAction::LoadEngine(slot));
                    ui.close();
                }
            }
            ui.separator();
            let mut analysis = state.analysis_enabled;
            if ui.checkbox(&mut analysis, "Analysis").changed() {
                action = Some(MenuAction::ToggleAnalysis);
                ui.close();
            }
            let book_label = match state.book_name {
                Some(name) => format!("Opening book: {} (replace…)", name),
                None => "Load opening book…".to_string(),
            };
            if ui.button(book_label).clicked() {
                action = Some(MenuAction::LoadBook);
                ui.close();
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let abort = egui::Button::new(RichText::new("Abort").color(egui::Color32::WHITE))
                .fill(theme::DANGER_RED);
            if ui
                .add(abort)
                .on_hover_text("Stop and quit all engines now")
                .clicked()
            {
                action = Some(MenuAction::AbortAll);
            }
        });
    });

    ui.horizontal(|ui| {
        ui.label(RichText::new("FEN").color(theme::MUTED).monospace());
        ui.add(egui::Label::new(RichText::new(state.fen).monospace()).truncate());
        if ui.button("Copy").on_hover_text("Copy FEN to the clipboard").clicked() {
            action = Some(MenuAction::CopyFen);
        }
    });
    action
}
