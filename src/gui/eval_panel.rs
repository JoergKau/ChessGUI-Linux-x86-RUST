//! Right-hand dashboard: one card per engine, the opening-book suggestions and
//! the outcome of the game.
//!
//! The panel only draws what it is given ([`EvalSummary`]); the app builds the
//! summary from the engine hosts and the game state. Score, number and move
//! formatting are pure functions with unit tests.

use eframe::egui::{self, Align, Color32, Layout, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, Vec2};

use crate::app::StatusLevel;
use crate::engine::{AnalysisStatus, EngineView, Score};
use crate::gui::menu_bar::MenuAction;
use crate::gui::theme;

/// Height of the principal-variation box: about three lines of text.
const PV_MAX_HEIGHT: f32 = 72.0;

/// Width of one header metric (EVAL, DEPTH, NODES), so the labels never wrap.
const METRIC_WIDTH: f32 = 64.0;

/// One engine card: the slot it fills plus its live view, if an engine is loaded.
#[derive(Clone, Debug)]
pub struct EvalCard {
    /// `0` = Engine A, `1` = Engine B.
    pub slot: usize,
    /// Engine snapshot; `None` when the slot is empty.
    pub view: Option<EngineView>,
    /// Principal variation in SAN, relative to the displayed position.
    pub pv_san: Vec<String>,
    /// Whether analysis is switched on for this slot (the card's toggle).
    pub running: bool,
}

impl EvalCard {
    /// A loaded engine that has not exited.
    fn is_live(&self) -> bool {
        self.view
            .as_ref()
            .is_some_and(|v| v.status != AnalysisStatus::Offline)
    }
}

/// Everything the evaluation panel draws in one frame.
#[derive(Clone, Debug)]
pub struct EvalSummary {
    /// Engine cards, Engine A first.
    pub cards: [EvalCard; 2],
    /// Side to move in the displayed position (scores are relative to it).
    pub white_to_move: bool,
    /// Full-move number of the displayed position.
    pub fullmove: u32,
    /// Outcome text, e.g. `"Unfinished"` or `"Black wins by checkmate"`.
    pub outcome: String,
    /// Opening-book moves as `(SAN, weight)`, heaviest first.
    pub book_moves: Vec<(String, u16)>,
    /// Name of the loaded opening book, if any.
    pub book_name: Option<String>,
    /// Last status message of the application.
    pub status: String,
    /// Severity of the status message.
    pub status_level: StatusLevel,
}

/// Draw the dashboard. Returns the menu action requested by a card's toggle or
/// "Load engine" button, if one was clicked.
pub fn show(ui: &mut Ui, summary: &EvalSummary) -> Option<MenuAction> {
    let mut requested = None;

    ui.add_space(4.0);
    ui.label(RichText::new(&summary.outcome).color(theme::MUTED));
    ui.add_space(6.0);

    for card in &summary.cards {
        if let Some(action) = engine_card(ui, card, summary.white_to_move, summary.fullmove) {
            requested = Some(action);
        }
        ui.add_space(6.0);
    }

    book_box(ui, summary);

    ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
        let color = match summary.status_level {
            StatusLevel::Info => theme::MUTED,
            StatusLevel::Warning => theme::ALERT_YELLOW,
            StatusLevel::Error => theme::DANGER_RED,
        };
        let text = RichText::new(&summary.status).small().color(color);
        let text = if summary.status_level == StatusLevel::Info { text } else { text.strong() };
        ui.label(text);
    });
    requested
}

/// One engine card: a header row (toggle, name, speed, metrics), a blue rule,
/// and the score badge with the principal variation. Returns a menu action
/// when the toggle or the "Load engine" button is pressed.
fn engine_card(ui: &mut Ui, card: &EvalCard, white_to_move: bool, fullmove: u32) -> Option<MenuAction> {
    let mut action = None;
    let live = card.is_live();
    let letter = slot_letter(card.slot);
    egui::Frame::new()
        .fill(theme::CARD_BG)
        .corner_radius(4.0)
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if toggle_button(ui, card.running, live) {
                    action = Some(MenuAction::ToggleEngine(card.slot));
                }
                match card.view.as_ref().filter(|_| live) {
                    Some(view) => {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(letter.to_string()).small().color(theme::MUTED));
                                ui.add(egui::Label::new(RichText::new(engine_name(view)).strong().color(theme::TEXT)).truncate());
                            });
                            speed_pill(ui, view);
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let white_score = view.stats.score.map(|s| white_point_of_view(s, white_to_move));
                            let depth = view
                                .stats
                                .depth
                                .map_or_else(|| "--".to_string(), |d| d.to_string());
                            let nodes = format_count(view.stats.nodes.unwrap_or(0));
                            let eval = white_score.map_or_else(|| "--".to_string(), format_eval_one_digit);
                            metric(ui, "NODES", &nodes, theme::NEON_CYAN);
                            ui.add_space(8.0);
                            metric(ui, "DEPTH", &depth, theme::NEON_CYAN);
                            ui.add_space(8.0);
                            metric(ui, "EVAL", &eval, theme::NEON_GREEN);
                        });
                    }
                    None => {
                        let text = match &card.view {
                            None => format!("Engine {}: no engine loaded", letter),
                            Some(_) => format!("Engine {}: offline", letter),
                        };
                        ui.label(RichText::new(text).color(theme::MUTED));
                    }
                }
            });

            if !live {
                if ui.button("Load engine…").clicked() {
                    action = Some(MenuAction::LoadEngine(card.slot));
                }
                return;
            }
            let Some(view) = card.view.as_ref() else {
                return;
            };

            ui.add_space(6.0);
            let (rule, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 2.0), Sense::hover());
            ui.painter().rect_filled(rule, 1.0, theme::BLUE);
            ui.add_space(6.0);

            let white_score = view.stats.score.map(|s| white_point_of_view(s, white_to_move));
            let badge = white_score.map_or_else(|| "--".to_string(), format_score_white);
            let tokens = number_pv(fullmove, white_to_move, &card.pv_san);
            ui.horizontal_top(|ui| {
                score_badge(ui, &badge);
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt(("pv", card.slot))
                    .max_height(PV_MAX_HEIGHT)
                    .auto_shrink([false, true])
                    .show(ui, |ui| pv_flow(ui, &tokens));
            });
        });
    action
}

/// The engine's name, or its file name when it did not give one.
fn engine_name(view: &EngineView) -> String {
    view.name.clone().unwrap_or_else(|| view.binary_name.clone())
}

/// Second line of the header: the speed as a dark pill while the engine
/// searches, otherwise a word for its state.
fn speed_pill(ui: &mut Ui, view: &EngineView) {
    match view.status {
        AnalysisStatus::Thinking => {
            let nps = format_nps(view.stats.nps.unwrap_or(0));
            egui::Frame::new()
                .fill(theme::BAR_BLACK)
                .corner_radius(4.0)
                .inner_margin(3.0)
                .show(ui, |ui| {
                    ui.label(RichText::new(nps).monospace().small().color(theme::TEXT));
                });
        }
        AnalysisStatus::Starting => {
            ui.label(RichText::new("starting…").small().color(theme::MUTED));
        }
        AnalysisStatus::Ready => {
            ui.label(RichText::new("idle").small().color(theme::MUTED));
        }
        AnalysisStatus::Offline => {}
    }
}

/// Round play/pause button drawn with the painter. Pause while the engine is
/// running, play while it is stopped. Returns `true` when clicked; a disabled
/// button (no live engine) never reports a click.
fn toggle_button(ui: &mut Ui, running: bool, enabled: bool) -> bool {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(30.0), sense);
    let (fill, ink) = match (enabled, running) {
        (false, _) => (theme::BAR_BLACK, theme::MUTED),
        (true, true) => (theme::BLUE, Color32::WHITE),
        (true, false) => (theme::BAR_BLACK, theme::NEON_GREEN),
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, fill);
    let c = rect.center();
    if running {
        for x in [c.x - 5.0, c.x + 1.0] {
            let bar = Rect::from_min_size(Pos2::new(x, c.y - 7.0), Vec2::new(4.0, 14.0));
            painter.rect_filled(bar, 1.0, ink);
        }
    } else {
        let points = vec![
            Pos2::new(c.x - 4.0, c.y - 7.0),
            Pos2::new(c.x - 4.0, c.y + 7.0),
            Pos2::new(c.x + 7.0, c.y),
        ];
        painter.add(Shape::convex_polygon(points, ink, Stroke::NONE));
    }
    let hover = if !enabled {
        "Load an engine first"
    } else if running {
        "Stop this engine's analysis"
    } else {
        "Start this engine's analysis"
    };
    response.on_hover_text(hover).clicked()
}

/// A labelled value, centred over its label, in a fixed-width column.
fn metric(ui: &mut Ui, label: &str, value: &str, color: Color32) {
    ui.allocate_ui_with_layout(Vec2::new(METRIC_WIDTH, 0.0), Layout::top_down(Align::Center), |ui| {
        ui.label(RichText::new(label).small().color(theme::MUTED));
        ui.label(RichText::new(value).strong().color(color));
    });
}

/// The evaluation from White's point of view, on a white badge.
fn score_badge(ui: &mut Ui, text: &str) {
    egui::Frame::new()
        .fill(Color32::WHITE)
        .corner_radius(4.0)
        .inner_margin(6.0)
        .show(ui, |ui| {
            ui.label(RichText::new(text).monospace().strong().color(Color32::BLACK));
        });
}

/// The principal variation as a wrapping flow. Move numbers are muted, moves bold.
fn pv_flow(ui: &mut Ui, tokens: &[String]) {
    if tokens.is_empty() {
        ui.label(RichText::new("PV: --").color(theme::MUTED).small());
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for token in tokens {
            let text = if token.ends_with('.') {
                RichText::new(token).color(theme::MUTED)
            } else {
                RichText::new(token).strong()
            };
            ui.label(text);
        }
    });
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

/// The header's evaluation, one decimal: `+0.2`, `-1.2`, `#3`.
pub fn format_eval_one_digit(score: Score) -> String {
    match score {
        Score::Cp(cp) => format!("{:+.1}", f64::from(cp) / 100.0),
        Score::Mate(_) => format_score_white(score),
    }
}

/// Search speed: `598.7kn/s` below a million nodes per second, `1.25Mn/s` above.
pub fn format_nps(nps: u64) -> String {
    if nps < 1_000_000 {
        format!("{:.1}kn/s", nps as f64 / 1e3)
    } else {
        format!("{:.2}Mn/s", nps as f64 / 1e6)
    }
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

/// The principal variation with move numbers: `3... Nf6 4. O-O Nxe4`. A
/// number is written before each White move, and before the first move when
/// that move is Black's. `first_number` is the displayed full-move number.
pub fn number_pv(first_number: u32, white_to_move: bool, sans: &[String]) -> Vec<String> {
    let black_first = u32::from(!white_to_move);
    let mut out = Vec::with_capacity(sans.len() * 2);
    for (i, san) in sans.iter().enumerate() {
        let i = i as u32;
        let number = first_number + (i + black_first) / 2;
        let is_white = i.is_multiple_of(2) == white_to_move;
        if is_white {
            out.push(format!("{}.", number));
        } else if i == 0 {
            out.push(format!("{}...", number));
        }
        out.push(san.clone());
    }
    out
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

    fn sans(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

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
    fn card_eval_has_one_decimal() {
        assert_eq!(format_eval_one_digit(Score::Cp(17)), "+0.2");
        assert_eq!(format_eval_one_digit(Score::Cp(14)), "+0.1");
        assert_eq!(format_eval_one_digit(Score::Cp(-120)), "-1.2");
        assert_eq!(format_eval_one_digit(Score::Cp(0)), "+0.0");
        assert_eq!(format_eval_one_digit(Score::Mate(3)), "#3");
        assert_eq!(format_eval_one_digit(Score::Mate(-2)), "-#2");
    }

    #[test]
    fn speed_uses_kilo_or_mega_nodes_per_second() {
        assert_eq!(format_nps(598_700), "598.7kn/s");
        assert_eq!(format_nps(1_250_000), "1.25Mn/s");
        assert_eq!(format_nps(0), "0.0kn/s");
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
    fn pv_numbers_start_with_white_when_white_is_to_move() {
        assert_eq!(
            number_pv(1, true, &sans(&["e4", "e5", "Nf3"])),
            sans(&["1.", "e4", "e5", "2.", "Nf3"])
        );
    }

    #[test]
    fn pv_numbers_a_black_first_line_with_dots() {
        assert_eq!(
            number_pv(3, false, &sans(&["Nf6", "O-O", "Nxe4", "d4"])),
            sans(&["3...", "Nf6", "4.", "O-O", "Nxe4", "5.", "d4"])
        );
    }

    #[test]
    fn engine_slots_are_lettered_a_and_b() {
        assert_eq!(slot_letter(0), 'A');
        assert_eq!(slot_letter(1), 'B');
    }
}
