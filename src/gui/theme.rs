//! Dark, high-contrast palette in the spirit of Nibbler: near-black panels,
//! neon green for evaluations, neon cyan for search metrics.

use eframe::egui::{self, Color32, FontId, TextStyle};

/// Panel and window background (`#16161a`).
pub const PANEL_BG: Color32 = Color32::from_rgb(0x16, 0x16, 0x1a);

/// Background of an engine card or the book box.
pub const CARD_BG: Color32 = Color32::from_rgb(0x20, 0x20, 0x28);

/// Primary text.
pub const TEXT: Color32 = Color32::from_rgb(0xd8, 0xde, 0xe9);

/// Secondary text and captions.
pub const MUTED: Color32 = Color32::from_rgb(0x7a, 0x7f, 0x8c);

/// Evaluation scores (`#00ff88`).
pub const NEON_GREEN: Color32 = Color32::from_rgb(0x00, 0xff, 0x88);

/// Search metrics: depth, NPS, nodes (`#8be9fd`).
pub const NEON_CYAN: Color32 = Color32::from_rgb(0x8b, 0xe9, 0xfd);

/// Engine is searching.
pub const ALERT_YELLOW: Color32 = Color32::from_rgb(0xf1, 0xfa, 0x8c);

/// Engine offline or an error.
pub const DANGER_RED: Color32 = Color32::from_rgb(0xff, 0x55, 0x55);

/// Evaluation bar side for White.
pub const BAR_WHITE: Color32 = Color32::from_rgb(0xe8, 0xe8, 0xec);

/// Evaluation bar side for Black.
pub const BAR_BLACK: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x33);

/// Text sizes. egui's defaults (Small = 9 pt) are too small for a desktop
/// analysis window, so every style is set explicitly.
fn text_styles() -> [(TextStyle, FontId); 5] {
    [
        (TextStyle::Heading, FontId::proportional(24.0)),
        (TextStyle::Body, FontId::proportional(17.0)),
        (TextStyle::Button, FontId::proportional(17.0)),
        (TextStyle::Small, FontId::proportional(15.0)),
        (TextStyle::Monospace, FontId::monospace(16.0)),
    ]
}

/// Apply the SiliconBoard dark theme and text sizes to the whole egui context.
pub fn apply(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.text_styles = text_styles().into_iter().collect();
    });
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL_BG;
    visuals.window_fill = CARD_BG;
    visuals.extreme_bg_color = PANEL_BG;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = NEON_GREEN.gamma_multiply(0.35);
    visuals.hyperlink_color = NEON_CYAN;
    ctx.set_visuals(visuals);
}
