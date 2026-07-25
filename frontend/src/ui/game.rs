//! `Jeu` — what the shell draws *on top of* the emulated picture.
//!
//! The emulated frame itself is not an egui widget: it is composed on the CPU
//! by `render::compose_frame` and blitted by `pixels`' scaling renderer,
//! exactly as before this shell existed. egui only adds an overlay pass on the
//! same target (`LoadOp::Load`, see `ui::egui_layer`), which is where the
//! panels of the next steps (settings, cheats, save-state browser) will go.
//!
//! Today the overlay carries a single indicator: a pause badge, so a session
//! suspended by `P` or by the menu is unambiguous. The transient status
//! messages and the FPS readout stay in the existing bitmap overlay in
//! `video.rs` — they are covered by their own tests and cost nothing.

use egui::{Align2, RichText};

use super::theme;

/// Gap between the top edge of the window and the badge.
const BADGE_MARGIN: f32 = 12.0;

/// Draw the in-game overlay. Nothing is emitted when the game is running, so
/// the pass costs one empty render pass per frame.
pub fn overlay(ctx: &egui::Context, paused: bool) {
    if !paused {
        return;
    }
    egui::Area::new(egui::Id::new("prisme-game-pause"))
        .anchor(Align2::CENTER_TOP, egui::vec2(0.0, BADGE_MARGIN))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::BG_DEEP.gamma_multiply(0.85))
                .stroke(egui::Stroke::new(1.0, theme::YELLOW))
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(12, 6))
                .show(ui, |ui| {
                    // `RichText::strong` only changes the colour — egui has no
                    // synthetic bold — so the weight comes from the face.
                    ui.label(
                        RichText::new("PAUSE")
                            .font(theme::strong(theme::SIZE_SMALL))
                            .color(theme::YELLOW),
                    );
                });
        });
}
