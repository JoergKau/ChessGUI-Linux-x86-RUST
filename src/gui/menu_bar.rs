//! Top bar, one row: Game / File / Engine menus, Start and Abort buttons, and
//! the FEN field with Load and Copy.
//!
//! The bar only reports what the user chose as a [`MenuAction`]; the app
//! performs it. Start and Abort switch analysis on and off; they never quit an
//! engine. The FEN text is owned by the app, so this module only edits it.

use eframe::egui::{self, Color32, FontId, RichText, Ui};

use crate::gui::theme;

/// Something the user asked for in the top bar.
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
    /// File → Copy FEN (also the FEN field's Copy button).
    CopyFen,
    /// File → Save FEN…
    SaveFen,
    /// Engine → Load engine A… / B… (`0` = A, `1` = B).
    LoadEngine(usize),
    /// Engine → Analysis (checkbox).
    ToggleAnalysis,
    /// Engine → Load PolyGlot book…
    LoadBook,
    /// Start: analyse the displayed position with every loaded engine.
    StartAnalysis,
    /// Abort: stop all searches. Engines stay loaded and can be started again.
    StopAnalysis,
    /// FEN field's Load button (or Enter): set the board to the FEN text.
    LoadFen,
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
    /// Number of moves played, shown in the Game menu.
    pub ply_count: usize,
}

/// Draw the top bar. `fen_input` is the FEN text shown in the field; the user
/// may edit it, and Enter loads it into the board. Cut, copy and paste work
/// in the field as in any text box. Returns the action chosen this frame.
pub fn show(ui: &mut Ui, state: &MenuState, fen_input: &mut String) -> Option<MenuAction> {
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

        ui.separator();
        let start = egui::Button::new(RichText::new("Start").color(Color32::BLACK).strong())
            .fill(theme::NEON_GREEN);
        if ui.add(start).on_hover_text("Analyse the position with every loaded engine").clicked() {
            action = Some(MenuAction::StartAnalysis);
        }
        let abort = egui::Button::new(RichText::new("Abort").color(Color32::WHITE).strong())
            .fill(theme::DANGER_RED);
        if ui.add(abort).on_hover_text("Stop all searches (engines stay loaded)").clicked() {
            action = Some(MenuAction::StopAnalysis);
        }

        ui.separator();
        ui.label(RichText::new("FEN").color(theme::MUTED));
        let field = ui
            .add(
                egui::TextEdit::singleline(fen_input)
                    .font(FontId::monospace(15.0))
                    .desired_width(ui.available_width().max(200.0))
                    .hint_text("paste a FEN and press Enter"),
            )
            .on_hover_text("Enter loads this position");
        // A single-line field gives up focus when Enter is pressed.
        if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            action = Some(MenuAction::LoadFen);
        }
    });
    action
}
