//! Quit confirmation, drawn as an in-app egui modal.
//!
//! It used to be an `rfd::MessageDialog` (a native `NSAlert`), which is exactly
//! the class of crash `docs/PUNCHLIST.md` records: a native modal opened from a
//! winit callback spins a nested AppKit run loop and re-enters winit's event
//! handler, which panics on purpose. An egui modal draws inside the frame the
//! shell is already building, so no nested loop exists at all.
//!
//! The panel owns no state: `video::App` holds whether it is up and which pause
//! flag to restore, and this module only turns a click into an `Action`.

use egui::{Align, Layout, RichText};

use super::theme;
use super::Action;

/// Draw the modal over whichever screen owns the window and return what the
/// player answered. `Action::None` while they have not answered.
pub fn show(ctx: &egui::Context, app_name: &str) -> Action {
    let mut action = Action::None;
    let response = egui::Modal::new(egui::Id::new("prisme-quit-confirm"))
        .frame(
            egui::Frame::new()
                .fill(theme::BG_PANEL)
                .stroke(egui::Stroke::new(1.0, theme::STROKE))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::same(18)),
        )
        .show(ctx, |ui| {
            ui.set_max_width(380.0);
            ui.label(
                RichText::new(title(app_name))
                    .size(theme::SIZE_HEADING)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(8.0);
            ui.label(RichText::new(DESCRIPTION).size(theme::SIZE_BODY).color(theme::TEXT_DIM));
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.button("Annuler (Échap)").clicked() {
                    action = Action::CancelQuit;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if super::home::primary_button(ui, "Quitter (Entrée)").clicked() {
                        action = Action::ConfirmQuit;
                    }
                });
            });
        });
    // Clicking the darkened backdrop is a dismissal, like any modal: the safe
    // answer is to stay in the application.
    if response.backdrop_response.clicked() {
        action = Action::CancelQuit;
    }
    action
}

/// What the modal promises before it lets the process go; the battery save is
/// written by `video::App::persist_all` on the way out.
const DESCRIPTION: &str =
    "La sauvegarde de la cartouche et l'état de session seront écrits avant de quitter.";

pub fn title(app_name: &str) -> String {
    format!("Quitter {app_name} ?")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every string the modal painted, one per line.
    fn painted(output: &egui::FullOutput) -> String {
        fn walk(shape: &egui::Shape, out: &mut String) {
            match shape {
                egui::Shape::Text(t) => {
                    out.push_str(t.galley.text());
                    out.push('\n');
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = String::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    #[test]
    fn the_modal_names_the_application_and_both_answers() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut input = egui::RawInput::default();
        input.screen_rect =
            Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 896.0)));
        let mut produced = Action::Quit; // must be overwritten
        let mut output = ctx.run(input.clone(), |_| {});
        // A modal is sized from the previous frame's measurement, so the first
        // pass lays out and a later one paints.
        for _ in 0..3 {
            output = ctx.run(input.clone(), |ctx| {
                produced = show(ctx, "Prisme");
            });
        }
        assert_eq!(produced, Action::None, "drawing alone must answer nothing");
        let text = painted(&output);
        assert!(text.contains("Quitter Prisme ?"), "{text}");
        assert!(text.contains("Annuler"), "{text}");
        assert!(text.contains("Quitter (Entrée)"), "{text}");
        // The promise made to the player must match what the exit path does.
        assert!(text.contains("sauvegarde de la cartouche"), "{text}");
    }

    #[test]
    fn the_title_carries_the_product_name() {
        assert_eq!(title("Prisme"), "Quitter Prisme ?");
        assert_ne!(title("Prisme"), title("Autre"));
    }
}
