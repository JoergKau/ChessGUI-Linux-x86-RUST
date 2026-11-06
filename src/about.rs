//! About panel for Linux (and any platform without the native macOS panel).
//!
//! On macOS the winit application menu provides the standard **About
//! SiliconBoard** panel (`macos_about.rs`). Linux window managers give an
//! immediate-mode app no such menu, so the toolbar carries an **ⓘ** button
//! (MD-Viewer's approach), which opens this modal: app icon, name, version,
//! a short description, the project link and the licence. It closes on OK,
//! the backdrop or Esc.

use std::sync::OnceLock;

use eframe::egui::{self, Align, Color32, Frame, Layout, RichText, Vec2};

use crate::gui::theme;

/// Project page shown in the panel and opened by its button.
const PROJECT_PAGE: &str = "https://github.com/JoergKau";

/// The embedded window icon, shown at the top of the panel.
const ICON_PNG: &[u8] = include_bytes!("../assets/icon/siliconboard-512.png");

/// The app icon as a GPU texture, decoded and uploaded once per run.
fn icon_texture(ctx: &egui::Context) -> egui::TextureHandle {
    static ICON: OnceLock<egui::TextureHandle> = OnceLock::new();
    ICON.get_or_init(|| {
        let image = image::load_from_memory(ICON_PNG)
            .expect("the bundled icon must decode")
            .to_rgba8();
        let (width, height) = image.dimensions();
        ctx.load_texture(
            "about_icon",
            egui::ColorImage::from_rgba_unmultiplied(
                [width as usize, height as usize],
                &image.into_raw(),
            ),
            Default::default(),
        )
    })
    .clone()
}

/// Draw the modal About panel while `open` is true; closing it (OK button,
/// backdrop or Esc) sets the flag to false.
pub fn show(ctx: &egui::Context, open: &mut bool) {
    if !*open {
        return;
    }
    let mut close = false;
    let frame = Frame::new()
        .fill(theme::PANEL_BG)
        .stroke(egui::Stroke::new(1.0, theme::TOOLBAR_STROKE))
        .corner_radius(10.0)
        .inner_margin(20.0);
    let modal = egui::Modal::new(egui::Id::unique("siliconboard_about"))
        .backdrop_color(Color32::from_black_alpha(150))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(420.0);
            ui.vertical_centered(|ui| {
                // App icon (72 pt, from the embedded window icon).
                let icon = icon_texture(ui.ctx());
                ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(
                    icon.id(),
                    Vec2::splat(72.0),
                )));
                ui.add_space(10.0);
                ui.label(
                    RichText::new("SiliconBoard")
                        .size(20.0)
                        .strong()
                        .color(theme::NEON_GREEN),
                );
                ui.label(
                    RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                        .size(13.0)
                        .color(theme::MUTED),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new("A UCI chess GUI written in Rust.")
                        .size(14.0)
                        .color(theme::TEXT),
                );
            });
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(format!("Project Page: {PROJECT_PAGE}"))
                        .size(13.0)
                        .color(theme::NEON_CYAN),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Copyright © 2026 JoKa · MIT License")
                        .size(12.0)
                        .color(theme::MUTED),
                );
            });
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.spacing_mut().button_padding = Vec2::new(14.0, 6.0);
                if ui
                    .add(egui::Button::new(RichText::new("OK").size(14.0)))
                    .clicked()
                {
                    close = true;
                }
                if ui
                    .add(egui::Button::new(
                        RichText::new("Project page").size(14.0),
                    ))
                    .clicked()
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(PROJECT_PAGE));
                }
            });
        });

    if modal.should_close() {
        close = true;
    }
    if close {
        *open = false;
    }
}
