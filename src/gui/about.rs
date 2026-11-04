//! The About window: the product name and version, the project's page and the
//! licence. Opened from Help → About SiliconBoard.

use eframe::egui;

/// The title shown in the window.
pub const TITLE: &str = "SiliconBoard 1.0";

/// The project's page.
pub const PROJECT_URL: &str = "https://github.com/JoergKau";

/// Show the About window while `open` is set. Closing the window clears `open`.
pub fn show(ctx: &egui::Context, open: &mut bool) {
    egui::Window::new("About SiliconBoard")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.heading(TITLE);
            ui.label("A UCI chess GUI for Apple Silicon, written in Rust.");
            ui.add_space(6.0);
            ui.hyperlink_to(PROJECT_URL, PROJECT_URL);
            ui.add_space(6.0);
            ui.label("Licence: MIT");
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_about_window_names_the_version_and_the_project_page() {
        assert_eq!(TITLE, "SiliconBoard 1.0");
        assert_eq!(PROJECT_URL, "https://github.com/JoergKau");
    }
}
