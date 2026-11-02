//! Right-hand dashboard: one card per engine, a live evaluation bar, the
//! opening-book suggestions and the outcome of the game.
//!
//! The panel only draws what it is given ([`EvalSummary`]); the app builds the
//! summary from the engine hosts and the game state. Score and number
//! formatting are pure functions with unit tests.

use eframe::egui::{self, Align, Layout, RichText, Sense, Ui, Vec2};

use crate::engine::{AnalysisStatus, EngineView, Score};
use crate::gui::menu_bar::MenuAction;
use crate::gui::theme;

/// Largest evaluation shown on the bar, in centipawns (±10 pawns).
const BAR_CP_LIMIT: i32 = 1000;

/// How many PV moves a card shows before it truncates.
const PV_DISPLAY_LIMIT: usize = 12;

/// One engine card: the slot it fills plus its live view, if an engine is loaded.
#[derive(Clone, Debug)]
pub struct EvalCard {
    /// `0` = Engine A, `1` = Engine B.
    pub slot: usize,
    /// Engine snapshot; `None` when the slot is empty.
    pub view: Option<EngineView>,
    /// Principal variation in SAN, relative to the displayed position.
    pub pv_san: Vec<String>,
}

/// Everything the evaluation panel draws in one frame.
#[derive(Clone, Debug)]
pub struct EvalSummary {
    /// Engine cards, Engine A first.
    pub cards: [EvalCard; 2],
    /// Side to move in the displayed position (scores are relative to it).
    pub white_to_move: bool,
    /// Outcome text, e.g. `"Unfinished"` or `"Black wins by checkmate"`.
    pub outcome: String,
    /// Opening-book moves as `(SAN, weight)`, heaviest first.
    pub book_moves: Vec<(String, u16)>,
    /// Name of the loaded opening book, if any.
    pub book_name: Option<String>,
    /// Last status message of the application.
    pub status: String,
}

/// Draw the dashboard. Returns the menu action requested by a card's
/// "Load engine" button, if one was clicked.
pub fn show(ui: &mut Ui, summary: &EvalSummary) -> Option<MenuAction> {
    let mut requested = None;

    ui.add_space(4.0);
    ui.label(RichText::new(&summary.outcome).color(theme::MUTED));
    ui.add_space(6.0);

    let primary = summary
        .cards
        .iter()
        .find_map(|card| card.view.as_ref().and_then(|v| v.stats.score));
    evaluation_bar(ui, primary, summary.white_to_move);
    ui.add_space(8.0);

    for card in &summary.cards {
        if let Some(action) = engine_card(ui, card, summary.white_to_move) {
            requested = Some(action);
        }
        ui.add_space(6.0);
    }

    book_box(ui, summary);

    ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
        ui.label(RichText::new(&summary.status).small().color(theme::MUTED));
    });
    requested
}

/// Horizontal evaluation bar: White's share of the bar grows with the score.
fn evaluation_bar(ui: &mut Ui, score: Option<Score>, white_to_move: bool) {
    let cp = score.map(|s| white_point_of_view(s, white_to_move).centipawn_equivalent());
    let white_share = eval_bar_white_share(cp);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 14.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::BAR_BLACK);
    let mut white_rect = rect;
    white_rect.set_width(rect.width() * white_share);
    painter.rect_filled(white_rect, 2.0, theme::BAR_WHITE);
    let label = match score {
        Some(s) => format_score_white(white_point_of_view(s, white_to_move)),
        None => "--".to_string(),
    };
    ui.label(RichText::new(format!("Eval (White): {}", label)).color(theme::NEON_GREEN).strong());
}

/// One engine card. Returns a menu action when its "Load engine" button is pressed.
fn engine_card(ui: &mut Ui, card: &EvalCard, white_to_move: bool) -> Option<MenuAction> {
    let mut action = None;
    let title = format!("Engine {}", slot_letter(card.slot));
    egui::Frame::new()
        .fill(theme::CARD_BG)
        .corner_radius(4.0)
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            match &card.view {
                None => {
                    ui.label(RichText::new(format!("{}: no engine loaded", title)).color(theme::MUTED));
                    if ui.button("Load engine…").clicked() {
                        action = Some(MenuAction::LoadEngine(card.slot));
                    }
                }
                Some(view) => {
                    ui.horizontal(|ui| {
                        let name = view
                            .name
                            .clone()
                            .unwrap_or_else(|| view.binary_name.clone());
                        ui.label(RichText::new(format!("{} · {}", title, name)).color(theme::NEON_CYAN).strong());
                        badge(ui, view.status);
                    });
                    let score_text = view
                        .stats
                        .score
                        .map(|s| format_score_white(white_point_of_view(s, white_to_move)))
                        .unwrap_or_else(|| "--".to_string());
                    ui.label(RichText::new(score_text).size(20.0).color(theme::NEON_GREEN).strong());
                    ui.horizontal_wrapped(|ui| {
                        let depth = match (view.stats.depth, view.stats.seldepth) {
                            (Some(d), Some(s)) => format!("depth {}/{}", d, s),
                            (Some(d), None) => format!("depth {}", d),
                            _ => "depth --".to_string(),
                        };
                        metric(ui, &depth);
                        metric(ui, &format!("{} N/s", format_count(view.stats.nps.unwrap_or(0))));
                        metric(ui, &format!("{} nodes", format_count(view.stats.nodes.unwrap_or(0))));
                    });
                    pv_line(ui, &card.pv_san);
                    if view.status == AnalysisStatus::Offline && ui.button("Load engine…").clicked() {
                        action = Some(MenuAction::LoadEngine(card.slot));
                    }
                }
            }
        });
    action
}

/// Status badge: colour + word.
fn badge(ui: &mut Ui, status: AnalysisStatus) {
    let (text, color) = match status {
        AnalysisStatus::Starting => ("Starting", theme::MUTED),
        AnalysisStatus::Ready => ("Ready", theme::NEON_CYAN),
        AnalysisStatus::Thinking => ("Thinking", theme::ALERT_YELLOW),
        AnalysisStatus::Offline => ("Offline", theme::DANGER_RED),
    };
    ui.label(RichText::new(format!("[{}]", text)).color(color).small());
}

/// A search metric shown in neon cyan.
fn metric(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).color(theme::NEON_CYAN).monospace().small());
}

/// The principal variation, first few moves in SAN.
fn pv_line(ui: &mut Ui, pv_san: &[String]) {
    if pv_san.is_empty() {
        ui.label(RichText::new("PV: --").color(theme::MUTED).small());
        return;
    }
    let mut text = String::new();
    for (i, san) in pv_san.iter().take(PV_DISPLAY_LIMIT).enumerate() {
        if i > 0 {
            text.push(' ');
        }
        text.push_str(san);
    }
    if pv_san.len() > PV_DISPLAY_LIMIT {
        text.push_str(" …");
    }
    ui.add(egui::Label::new(RichText::new(text).monospace().small()).wrap());
}

/// Opening-book box: the heaviest book moves with their share of the weight.
fn book_box(ui: &mut Ui, summary: &EvalSummary) {
    egui::Frame::new()
        .fill(theme::CARD_BG)
        .corner_radius(4.0)
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let heading = match &summary.book_name {
                Some(name) => format!("Opening book · {}", name),
                None => "Opening book · none loaded".to_string(),
            };
            ui.label(RichText::new(heading).color(theme::NEON_CYAN).small());
            if summary.book_name.is_some() && summary.book_moves.is_empty() {
                ui.label(RichText::new("not in book").color(theme::MUTED).small());
            }
            let total: u32 = summary.book_moves.iter().map(|(_, w)| u32::from(*w)).sum();
            for (san, weight) in summary.book_moves.iter().take(6) {
                let share = if total == 0 {
                    0.0
                } else {
                    f64::from(*weight) * 100.0 / f64::from(total)
                };
                ui.label(
                    RichText::new(format!("{:<6} {:>5.1}%", san, share))
                        .monospace()
                        .small(),
                );
            }
        });
}

/// Convert a score from the side-to-move's point of view to White's.
pub fn white_point_of_view(score: Score, white_to_move: bool) -> Score {
    if white_to_move {
        return score;
    }
    match score {
        Score::Cp(cp) => Score::Cp(-cp),
        Score::Mate(n) => Score::Mate(-n),
    }
}

/// Display text for a White-point-of-view score: `+0.45`, `-1.20`, `#3`, `-#2`.
pub fn format_score_white(score: Score) -> String {
    match score {
        Score::Cp(cp) => format!("{:+.2}", f64::from(cp) / 100.0),
        Score::Mate(0) => "#0".to_string(),
        Score::Mate(n) if n > 0 => format!("#{}", n),
        Score::Mate(n) => format!("-#{}", n.unsigned_abs()),
    }
}

/// Share of the evaluation bar that belongs to White, in `0.0..=1.0`. Scores
/// are clamped to ±10 pawns; a missing score is an even bar.
pub fn eval_bar_white_share(cp_white: Option<i32>) -> f32 {
    let cp = cp_white.unwrap_or(0).clamp(-BAR_CP_LIMIT, BAR_CP_LIMIT);
    (cp + BAR_CP_LIMIT) as f32 / (2 * BAR_CP_LIMIT) as f32
}

/// Compact count: `512`, `1.2k`, `980k`, `2.45M`, `1.20G`.
pub fn format_count(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.2}M", n as f64 / 1e6),
        _ => format!("{:.2}G", n as f64 / 1e9),
    }
}

/// Letter used in card titles.
fn slot_letter(slot: usize) -> char {
    char::from(b'A' + slot as u8)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_is_shown_from_whites_point_of_view() {
        // Black to move and Black is better by 45 cp: White sees -0.45.
        assert_eq!(
            white_point_of_view(Score::Cp(45), false),
            Score::Cp(-45)
        );
        assert_eq!(white_point_of_view(Score::Cp(45), true), Score::Cp(45));
        assert_eq!(white_point_of_view(Score::Mate(3), false), Score::Mate(-3));
    }

    #[test]
    fn centipawn_scores_format_with_sign_and_two_decimals() {
        assert_eq!(format_score_white(Score::Cp(45)), "+0.45");
        assert_eq!(format_score_white(Score::Cp(-120)), "-1.20");
        assert_eq!(format_score_white(Score::Cp(0)), "+0.00");
    }

    #[test]
    fn mate_scores_format_as_hash_notation() {
        assert_eq!(format_score_white(Score::Mate(3)), "#3");
        assert_eq!(format_score_white(Score::Mate(-2)), "-#2");
        assert_eq!(format_score_white(Score::Mate(0)), "#0");
    }

    #[test]
    fn eval_bar_is_even_without_a_score_and_clamped_at_ten_pawns() {
        assert!((eval_bar_white_share(None) - 0.5).abs() < f32::EPSILON);
        assert!((eval_bar_white_share(Some(0)) - 0.5).abs() < f32::EPSILON);
        assert!((eval_bar_white_share(Some(1_000)) - 1.0).abs() < f32::EPSILON);
        assert!((eval_bar_white_share(Some(-30_000)) - 0.0).abs() < f32::EPSILON);
        assert!(eval_bar_white_share(Some(300)) > 0.5);
    }

    #[test]
    fn counts_use_compact_suffixes() {
        assert_eq!(format_count(512), "512");
        assert_eq!(format_count(1_200), "1.2k");
        assert_eq!(format_count(980_000), "980.0k");
        assert_eq!(format_count(2_450_000), "2.45M");
        assert_eq!(format_count(1_200_000_000), "1.20G");
    }

    #[test]
    fn engine_slots_are_lettered_a_and_b() {
        assert_eq!(slot_letter(0), 'A');
        assert_eq!(slot_letter(1), 'B');
    }
}
