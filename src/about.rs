//! About window for Linux (and any platform without the native macOS panel).
//!
//! On macOS the winit application menu provides the standard **About
//! SiliconBoard** panel (`macos_about.rs`). Linux window managers give an
//! immediate-mode app no such menu, so the View menu carries **About
//! SiliconBoard…**, which opens this small movable window: name, version,
//! project page and licence — the same facts the macOS panel shows.

use eframe::egui::{self, Color32, RichText};

/// Project page shown in the window.
const PROJECT_PAGE: &str = "https://github.com/JoergKau";

/// Copyright line shown under the project page.
const COPYRIGHT: &str = "Copyright © 2026 JoKa. MIT License.";

/// The accent green of the evaluation dashboard, for the title.
const ACCENT: Color32 = Color32::from_rgb(0x00, 0xff, 0x88);

/// Draw the About window. The `open` flag is owned by the app: the window's
/// close button sets it to `false`, and `show` returns the new value.
pub fn show(ctx: &egui::Context, open: &mut bool) {
    egui::Window::new("About SiliconBoard")
        .open(open)
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label(RichText::new("SiliconBoard").size(22.0).color(ACCENT).strong());
            ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
            ui.add_space(8.0);
            ui.label("A UCI chess GUI written in Rust.");
            ui.add_space(8.0);
            ui.hyperlink_to(PROJECT_PAGE, PROJECT_PAGE);
            ui.add_space(8.0);
            ui.label(COPYRIGHT);
            ui.add_space(4.0);
        });
}
