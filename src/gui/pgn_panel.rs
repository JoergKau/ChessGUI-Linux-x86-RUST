//! Move list along the bottom: two columns per move (`1. e4 e5`), the current
//! half-move highlighted, click any move to jump to it, plus start / previous /
//! next / latest buttons.
//!
//! The panel navigates the [`GameState`] cursor directly. Clicking a move
//! moves the cursor; the board follows on the next frame.

use std::sync::Arc;

use eframe::egui::{self, Align, Galley, RichText, Ui, WidgetText};

use crate::chess::game_state::MoveLine;
use crate::chess::GameState;
use crate::gui::{figurines, theme};

/// Draw the move list. `follow` scrolls the current move into view; the app
/// sets it on the frame the cursor moved. `lines` is the cached move list for
/// the current history revision; `galleys[ply]` is its cached SAN layout.
/// `figurines` draws `♘f3` instead of `Nf3` when the View setting is on.
pub fn show(
    ui: &mut Ui,
    game: &mut GameState,
    lines: &[MoveLine],
    galleys: &[Option<Arc<Galley>>],
    follow: bool,
    figurines: bool,
) {
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

    let current = game.cur_ply();
    let mut target: Option<usize> = None;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);
            ui.horizontal_wrapped(|ui| {
                for line in lines {
                    // A line that starts with Black's move is numbered `N...`.
                    let number = if line.white.is_some() {
                        format!("{}.", line.number)
                    } else {
                        format!("{}...", line.number)
                    };
                    ui.label(RichText::new(number).color(theme::MUTED).monospace());
                    let moves = [(&line.white, theme::WHITE_MOVE), (&line.black, theme::BLACK_MOVE)];
                    for (entry, color) in moves {
                        let Some(entry) = entry else { continue };
                        let response = match galleys.get(entry.ply).and_then(|g| g.as_ref()) {
                            Some(galley) => ui.selectable_label(
                                current == entry.ply,
                                WidgetText::Galley(Arc::clone(galley)),
                            ),
                            None => ui.selectable_label(
                                current == entry.ply,
                                figurines::san_job(ui, &entry.san, color, figurines),
                            ),
                        };
                        if response.clicked() {
                            target = Some(entry.ply);
                        }
                        if follow && current == entry.ply {
                            response.scroll_to_me(Some(Align::Center));
                        }
                    }
                    ui.add_space(10.0);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicking_a_move_in_the_list_moves_the_cursor_to_it() {
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
        let white = game.pgn_lines()[0].white.clone().unwrap();
        game.goto_ply(white.ply);
        assert_eq!(game.cur_ply(), 1);
        assert_eq!(game.last_move().map(|(from, _)| from.to_string()), Some("e2".into()));
    }
}
