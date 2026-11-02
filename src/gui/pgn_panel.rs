//! Move list along the bottom: two columns per move (`1. e4 e5`), the current
//! half-move highlighted, click any move to jump to it, plus start / previous /
//! next / latest buttons.
//!
//! The panel navigates the [`GameState`] cursor directly. Clicking a move
//! moves the cursor; the board follows on the next frame.

use eframe::egui::{self, Align, RichText, Ui};

use crate::chess::GameState;
use crate::gui::theme;

/// Draw the move list. `follow` scrolls the current move into view; the app
/// sets it on the frame the cursor moved.
pub fn show(ui: &mut Ui, game: &mut GameState, follow: bool) {
    ui.horizontal(|ui| {
        if ui.button("|<").on_hover_text("Start of game").clicked() {
            game.goto_start();
        }
        if ui.button("<").on_hover_text("Previous move (←)").clicked() {
            game.undo();
        }
        if ui.button(">").on_hover_text("Next move (→)").clicked() {
            game.redo();
        }
        if ui.button(">|").on_hover_text("Latest position (End)").clicked() {
            game.goto_end();
        }
        let browsing = game.cur_ply() != game.total_plies();
        if browsing {
            ui.label(RichText::new("browsing history — board is read-only").color(theme::ALERT_YELLOW));
        }
    });

    let lines = game.pgn_lines();
    let current = game.cur_ply();
    let mut target: Option<usize> = None;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Grid::new("pgn_moves")
                .num_columns(3)
                .spacing([14.0, 2.0])
                .striped(true)
                .show(ui, |ui| {
                    for (index, (number, white, black)) in lines.iter().enumerate() {
                        ui.label(RichText::new(format!("{}.", number)).color(theme::MUTED).monospace());

                        let white_ply = white_ply_for_line(index);
                        if let Some(san) = white {
                            let response = ui.selectable_label(current == white_ply, san);
                            if response.clicked() {
                                target = Some(white_ply);
                            }
                            if follow && current == white_ply {
                                response.scroll_to_me(Some(Align::Center));
                            }
                        } else {
                            ui.label("");
                        }

                        let black_ply = white_ply + 1;
                        if let Some(san) = black {
                            let response = ui.selectable_label(current == black_ply, san);
                            if response.clicked() {
                                target = Some(black_ply);
                            }
                            if follow && current == black_ply {
                                response.scroll_to_me(Some(Align::Center));
                            }
                        } else {
                            ui.label("");
                        }
                        ui.end_row();
                    }
                });
            if lines.is_empty() {
                ui.label(RichText::new("No moves yet — play one on the board.").color(theme::MUTED));
            }
        });

    if let Some(ply) = target {
        game.goto_ply(ply);
    }
}

/// Plies are counted from 1 in the move list (`cur_ply` = number of moves
/// played), so white's move `index` is ply `2 * index + 1`.
pub fn white_ply_for_line(index: usize) -> usize {
    2 * index + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_list_plies_follow_the_cursor_convention() {
        // After 1. e4 e5 the cursor is at ply 2; line 1 holds plies 1 and 2.
        assert_eq!(white_ply_for_line(0), 1);
        assert_eq!(white_ply_for_line(1), 3);
        let mut game = GameState::new();
        game.play_squares(
            shakmaty::Square::from_ascii(b"e2").unwrap(),
            shakmaty::Square::from_ascii(b"e4").unwrap(),
            None,
        )
        .unwrap();
        game.play_squares(
            shakmaty::Square::from_ascii(b"e7").unwrap(),
            shakmaty::Square::from_ascii(b"e5").unwrap(),
            None,
        )
        .unwrap();
        game.goto_ply(white_ply_for_line(0));
        assert_eq!(game.cur_ply(), 1);
        assert_eq!(game.last_move().map(|(from, _)| from.to_string()), Some("e2".into()));
    }
}
