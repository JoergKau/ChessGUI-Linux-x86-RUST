//! Top bar, one row: Game / File / Engine menus, Start and Abort buttons, and
//! the FEN field.
//!
//! The bar only reports what the user chose as a [`MenuAction`]; the app
//! performs it. Start and Abort switch analysis on and off for every engine;
//! they never quit an engine. Each engine card has its own toggle. The FEN
//! text is owned by the app, so this module only edits it.

use eframe::egui::{self, Color32, FontId, RichText, Ui};

use crate::chess::players::{GameMode, MODES};
use crate::gui::theme;

/// Something the user asked for in the top bar.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MenuAction {
    /// Game → New game.
    NewGame,
    /// Game → Flip board.
    FlipBoard,
    /// Game → one of the modes (analysis, play a colour, engine vs engine).
    SetMode(GameMode),
    /// Game → Time control…: open the dialog.
    OpenTimeControl,
    /// Game → Book moves in games: switch the book reply on or off.
    ToggleBookInGames,
    /// Game → Resign: the human side gives up the game.
    Resign,
    /// Game → Offer draw: the engine accepts or declines.
    OfferDraw,
    /// Engine → Engine A options… / B options… (`0` = A, `1` = B).
    OpenEngineOptions(usize),
    /// File → Open PGN…
    OpenPgn,
    /// File → Save PGN…
    SavePgn,
    /// File → Copy PGN.
    CopyPgn,
    /// File → Copy FEN.
    CopyFen,
    /// File → Save FEN…
    SaveFen,
    /// Engine → Load engine A… / B… (`0` = A, `1` = B).
    LoadEngine(usize),
    /// An engine card's play/pause toggle (`0` = A, `1` = B).
    ToggleEngine(usize),
    /// Engine → Load PolyGlot book…
    LoadBook,
    /// View → Board theme: switch to the image at this index in the theme list.
    SetBoardTheme(usize),
    /// View → Piece set: switch to the set at this index.
    SetPieceSet(usize),
    /// View → Figurines in move list: toggle `♘f3` there.
    ToggleFigurinesMoveList,
    /// View → Figurines in engine evaluation: toggle `♘f3` in the PV.
    ToggleFigurinesEval,
    /// Start: analyse the displayed position with every loaded engine.
    StartAnalysis,
    /// Abort: stop all searches. Engines stay loaded and can be started again.
    StopAnalysis,
    /// Enter in the FEN field: set the board to the FEN text.
    LoadFen,
}

/// Read-only facts the menu needs to draw its labels.
#[derive(Clone, Debug)]
pub struct MenuState<'a> {
    /// Whether each engine slot holds a running (or starting) engine.
    pub engine_loaded: [bool; 2],
    /// The mode the session is in; its menu entry is marked.
    pub mode: GameMode,
    /// Whether engines play opening-book moves in games.
    pub book_in_games: bool,
    /// `true` while a game against an engine is running, so the human can
    /// resign or offer a draw.
    pub vs_engine_live: bool,
    /// Whether each slot's engine has answered its handshake, so its options are known.
    pub options_ready: [bool; 2],
    /// File name of the loaded opening book, if any.
    pub book_name: Option<&'a str>,
    /// Board images from `public/images/board/`.
    pub board_themes: &'a [crate::assets::BoardTheme],
    /// File name of the board image in use, if one has been chosen.
    pub board_theme: Option<&'a str>,
    /// Piece sets from `public/piece/`.
    pub piece_sets: &'a [crate::assets::PieceSet],
    /// Folder name of the piece set in use, if one has been chosen.
    pub piece_set: Option<&'a str>,
    /// Whether the move list draws piece letters as figurines.
    pub figurines_move_list: bool,
    /// Whether the engine PV draws piece letters as figurines.
    pub figurines_eval: bool,
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
            for mode in MODES {
                if ui.radio(state.mode == mode, mode.label()).clicked() {
                    action = Some(MenuAction::SetMode(mode));
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Time control…").clicked() {
                action = Some(MenuAction::OpenTimeControl);
                ui.close();
            }
            ui.separator();
            let vs_engine = state.vs_engine_live;
            if ui
                .add_enabled(vs_engine, egui::Button::new("Resign"))
                .on_disabled_hover_text("Only in a running game against an engine.")
                .clicked()
            {
                action = Some(MenuAction::Resign);
                ui.close();
            }
            if ui
                .add_enabled(vs_engine, egui::Button::new("Offer draw"))
                .on_disabled_hover_text("Only in a running game against an engine.")
                .clicked()
            {
                action = Some(MenuAction::OfferDraw);
                ui.close();
            }
            ui.separator();
            let book_available = state.book_name.is_some();
            let book = ui
                .add_enabled(
                    book_available,
                    egui::Button::selectable(state.book_in_games, "Book moves in games"),
                )
                .on_disabled_hover_text("Load an opening book first (Engine menu).");
            if book.clicked() {
                action = Some(MenuAction::ToggleBookInGames);
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
            for slot in 0..2 {
                let letter = char::from(b'A' + slot as u8);
                let button = ui
                    .add_enabled(state.options_ready[slot], egui::Button::new(format!("Engine {} options…", letter)))
                    .on_disabled_hover_text("Available once the engine has started.");
                if button.clicked() {
                    action = Some(MenuAction::OpenEngineOptions(slot));
                    ui.close();
                }
            }
            ui.separator();
            let book_label = match state.book_name {
                Some(name) => format!("Opening book: {} (replace…)", name),
                None => "Load opening book…".to_string(),
            };
            if ui.button(book_label).clicked() {
                action = Some(MenuAction::LoadBook);
                ui.close();
            }
        });
        ui.menu_button("View", |ui| {
            ui.menu_button("Board theme", |ui| {
                if state.board_themes.is_empty() {
                    ui.label("No board images found");
                }
                for (index, theme) in state.board_themes.iter().enumerate() {
                    if ui
                        .radio(state.board_theme == Some(theme.file.as_str()), &theme.label)
                        .clicked()
                    {
                        action = Some(MenuAction::SetBoardTheme(index));
                        ui.close();
                    }
                }
            });
            ui.separator();
            ui.menu_button("Piece set", |ui| {
                if state.piece_sets.is_empty() {
                    ui.label("No complete piece sets found");
                }
                for (index, set) in state.piece_sets.iter().enumerate() {
                    if ui
                        .radio(state.piece_set == Some(set.name.as_str()), &set.label)
                        .clicked()
                    {
                        action = Some(MenuAction::SetPieceSet(index));
                        ui.close();
                    }
                }
            });
            ui.separator();
            if ui
                .add(egui::Button::selectable(
                    state.figurines_move_list,
                    "Figurines in move list",
                ))
                .on_hover_text("Show ♘f3 instead of Nf3 in the move list")
                .clicked()
            {
                action = Some(MenuAction::ToggleFigurinesMoveList);
                ui.close();
            }
            if ui
                .add(egui::Button::selectable(
                    state.figurines_eval,
                    "Figurines in engine evaluation",
                ))
                .on_hover_text("Show ♘f3 instead of Nf3 in the principal variation")
                .clicked()
            {
                action = Some(MenuAction::ToggleFigurinesEval);
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
