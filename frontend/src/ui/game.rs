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

use egui::{Align2, Color32, RichText};

use crate::i18n::{Lang, Msg};

use super::theme;

/// Gap between the top edge of the window and the badge.
const BADGE_MARGIN: f32 = 12.0;

/// Whether the assistant currently has the pad, and what it last said.
pub struct Assistant<'a> {
    /// True while a command still owes frames: the picture is moving because
    /// the assistant asked for it to.
    pub playing: bool,
    pub says: Option<&'a str>,
}

/// Draw the in-game overlay. Nothing is emitted when the game is simply
/// running, so the pass costs one empty render pass per frame.
pub fn overlay(ctx: &egui::Context, paused: bool, assistant: Option<Assistant>, lang: Lang) {
    // The assistant badge takes the place of the pause one: a session held
    // still between two of its commands is *its* doing, and calling that
    // "PAUSE" would send the player looking for a key they never pressed.
    if let Some(assistant) = assistant {
        let (text, color) = if assistant.playing {
            (Msg::LiveDriving.text(lang), theme::ACCENT)
        } else {
            (Msg::LiveThinking.text(lang), theme::YELLOW)
        };
        return badge(ctx, "prisme-game-assistant", text, assistant.says, color);
    }
    if !paused {
        return;
    }
    badge(ctx, "prisme-game-pause", "PAUSE", None, theme::YELLOW);
}

/// One boxed line at the top of the picture, with an optional second line
/// underneath it in the assistant's own words.
fn badge(ctx: &egui::Context, id: &str, text: &str, says: Option<&str>, color: Color32) {
    egui::Area::new(egui::Id::new(id))
        .anchor(Align2::CENTER_TOP, egui::vec2(0.0, BADGE_MARGIN))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::BG_DEEP.gamma_multiply(0.85))
                .stroke(egui::Stroke::new(1.0, color))
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(12, 6))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        // `RichText::strong` only changes the colour — egui has
                        // no synthetic bold — so the weight comes from the face.
                        ui.label(
                            RichText::new(text)
                                .font(theme::strong(theme::SIZE_SMALL))
                                .color(color),
                        );
                        if let Some(says) = says.map(str::trim).filter(|s| !s.is_empty()) {
                            ui.label(
                                RichText::new(says)
                                    .font(theme::mono(theme::SIZE_SMALL))
                                    .color(theme::TEXT_DIM),
                            );
                        }
                    });
                });
        });
}
