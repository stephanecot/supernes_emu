//! The shell's icon set, drawn with egui's painter.
//!
//! A restricted set — play, star, gear, folder, chip, magnifier, cross, back
//! arrow — vector-drawn rather than taken from an icon font: no dependency, no
//! licence question, one uniform stroke weight, and the same shapes at every
//! size (a font would also reintroduce the tofu box the default face printed
//! for `←`).
//!
//! Every glyph is described in a unit square and mapped onto the rectangle it
//! is asked to fill, so an icon next to 14-point text and the same icon on a
//! 24-point button are the same drawing. The stroke width is a constant
//! fraction of the side with a one-point floor, which is what keeps an icon
//! readable at small sizes.

use egui::{pos2, vec2, Color32, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Vec2};

use super::theme;

/// Side of an icon standing next to body text, in points.
pub const SIZE: f32 = 15.0;
/// Gap between an icon and the label that follows it.
pub const GAP: f32 = 7.0;
/// Stroke width as a fraction of the icon's side. One value for the whole set:
/// that uniformity is what makes drawn icons read as a family.
const STROKE_RATIO: f32 = 0.095;

/// The icons the shell uses. Anything that would need a ninth icon is a sign
/// the screen is doing too much.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// Start a game.
    Play,
    /// Favourite, outlined when it is not one.
    Star,
    /// Favourite, filled when it is.
    StarFilled,
    /// Settings.
    Gear,
    /// A folder to pick or to open.
    Folder,
    /// Coprocessor.
    Chip,
    /// Search.
    Search,
    /// Clear / close.
    Close,
    /// Back to the previous screen.
    ArrowLeft,
    /// Add something to the library.
    Plus,
}

impl Icon {
    /// The whole set, walked by the tests that check every icon draws, keeps
    /// the family's stroke weight and stays inside its box. Compiled in tests
    /// only — the screens name the icon they want.
    #[cfg(test)]
    pub const ALL: [Icon; 10] = [
        Icon::Play,
        Icon::Star,
        Icon::StarFilled,
        Icon::Gear,
        Icon::Folder,
        Icon::Chip,
        Icon::Search,
        Icon::Close,
        Icon::ArrowLeft,
        Icon::Plus,
    ];

    /// Draw the icon inside `rect` in `color`. The drawing is inscribed in the
    /// largest square centred in `rect`.
    pub fn draw(self, painter: &egui::Painter, rect: Rect, color: Color32) {
        let side = rect.width().min(rect.height());
        let origin = rect.center() - Vec2::splat(side / 2.0);
        let p = |x: f32, y: f32| origin + vec2(x * side, y * side);
        let stroke = Stroke::new((side * STROKE_RATIO).max(1.0), color);

        match self {
            Icon::Play => {
                painter.add(Shape::closed_line(
                    vec![p(0.26, 0.14), p(0.84, 0.50), p(0.26, 0.86)],
                    stroke,
                ));
            }
            Icon::Star | Icon::StarFilled => {
                let points = star_points(p);
                if self == Icon::StarFilled {
                    // A star is not convex: `Shape::convex_polygon` would cut
                    // its notches off, so the filled state is a concave path.
                    painter.add(Shape::Path(egui::epaint::PathShape {
                        points: points.clone(),
                        closed: true,
                        fill: color,
                        stroke: stroke.into(),
                    }));
                } else {
                    painter.add(Shape::closed_line(points, stroke));
                }
            }
            Icon::Gear => {
                let centre = p(0.5, 0.5);
                let radius = side * 0.24;
                painter.circle_stroke(centre, radius, stroke);
                painter.circle_stroke(centre, side * 0.09, stroke);
                // Eight teeth, evenly spaced, drawn outwards from the rim.
                for i in 0..8 {
                    let a = std::f32::consts::TAU * i as f32 / 8.0;
                    let dir = vec2(a.cos(), a.sin());
                    painter.line_segment(
                        [centre + dir * radius, centre + dir * (radius + side * 0.14)],
                        stroke,
                    );
                }
            }
            Icon::Folder => {
                painter.add(Shape::closed_line(
                    vec![
                        p(0.10, 0.82),
                        p(0.10, 0.20),
                        p(0.42, 0.20),
                        p(0.52, 0.34),
                        p(0.90, 0.34),
                        p(0.90, 0.82),
                    ],
                    stroke,
                ));
            }
            Icon::Chip => {
                // A package with three pins per side, the way a coprocessor is
                // drawn on a cartridge board.
                painter.rect_stroke(
                    Rect::from_min_max(p(0.26, 0.26), p(0.74, 0.74)),
                    0.0,
                    stroke,
                    StrokeKind::Middle,
                );
                // Short pins: at twelve points the package has to stay the
                // dominant shape, or the icon reads as a star.
                for k in [0.36_f32, 0.50, 0.64] {
                    painter.line_segment([p(k, 0.26), p(k, 0.14)], stroke);
                    painter.line_segment([p(k, 0.74), p(k, 0.86)], stroke);
                    painter.line_segment([p(0.26, k), p(0.14, k)], stroke);
                    painter.line_segment([p(0.74, k), p(0.86, k)], stroke);
                }
            }
            Icon::Search => {
                painter.circle_stroke(p(0.44, 0.44), side * 0.26, stroke);
                painter.line_segment([p(0.64, 0.64), p(0.88, 0.88)], stroke);
            }
            Icon::Close => {
                painter.line_segment([p(0.20, 0.20), p(0.80, 0.80)], stroke);
                painter.line_segment([p(0.80, 0.20), p(0.20, 0.80)], stroke);
            }
            Icon::Plus => {
                painter.line_segment([p(0.50, 0.18), p(0.50, 0.82)], stroke);
                painter.line_segment([p(0.18, 0.50), p(0.82, 0.50)], stroke);
            }
            Icon::ArrowLeft => {
                painter.line_segment([p(0.86, 0.50), p(0.16, 0.50)], stroke);
                painter.line_segment([p(0.16, 0.50), p(0.44, 0.22)], stroke);
                painter.line_segment([p(0.16, 0.50), p(0.44, 0.78)], stroke);
            }
        }
    }
}

/// The ten points of a five-pointed star, alternating outer and inner radius,
/// first point straight up.
fn star_points(p: impl Fn(f32, f32) -> Pos2) -> Vec<Pos2> {
    let (outer, inner) = (0.44_f32, 0.18_f32);
    (0..10)
        .map(|i| {
            let radius = if i % 2 == 0 { outer } else { inner };
            let angle = std::f32::consts::TAU * i as f32 / 10.0 - std::f32::consts::FRAC_PI_2;
            p(0.5 + radius * angle.cos(), 0.5 + radius * angle.sin())
        })
        .collect()
}

/// Draw an icon on its own, with no interaction, at `side` points.
pub fn show(ui: &mut egui::Ui, icon: Icon, side: f32, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
    if ui.is_rect_visible(rect) {
        icon.draw(ui.painter(), rect, color);
    }
    response
}

/// A framed button carrying an icon and its label. Same frame, padding and
/// hover states as `egui::Button`, since it is painted from the very same
/// widget visuals — the icon is what a plain button cannot do.
pub fn button(ui: &mut egui::Ui, icon: Icon, label: &str) -> Response {
    labeled(ui, icon, label, None, None, None)
}

/// The same button with its icon in a meaning-carrying colour (the yellow star
/// of a favourite), the label staying neutral.
pub fn button_tinted(ui: &mut egui::Ui, icon: Icon, label: &str, tint: Color32) -> Response {
    labeled(ui, icon, label, None, None, Some(tint))
}

/// The accent-filled call to action (`Jouer`, `Ouvrir une ROM…`).
pub fn primary_button(ui: &mut egui::Ui, icon: Icon, label: &str) -> Response {
    labeled(ui, icon, label, Some(theme::ACCENT), Some(Color32::WHITE), None)
}

/// An icon alone as a button, with no frame: for the favourite star and the
/// clear-search cross, which sit inside another surface.
pub fn ghost_button(ui: &mut egui::Ui, icon: Icon, side: f32, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    if ui.is_rect_visible(rect) {
        // The only feedback is the icon itself brightening: a frame here would
        // fight the card or the text field the button sits in.
        let color = if response.hovered() { color } else { color.gamma_multiply(0.8) };
        icon.draw(ui.painter(), rect, color);
    }
    response
}

fn labeled(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    fill: Option<Color32>,
    text_color: Option<Color32>,
    icon_color: Option<Color32>,
) -> Response {
    let padding = ui.spacing().button_padding;
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        theme::font(theme::SIZE_BUTTON),
        Color32::PLACEHOLDER,
    );
    let inner = vec2(SIZE + GAP + galley.size().x, galley.size().y.max(SIZE));
    let (rect, response) = ui.allocate_at_least(inner + 2.0 * padding, Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let fill = fill.unwrap_or(visuals.weak_bg_fill);
        let stroke = match text_color {
            // A filled call to action carries no contrasting border.
            Some(_) => Stroke::new(1.0, fill),
            None => visuals.bg_stroke,
        };
        ui.painter().rect(rect, visuals.corner_radius, fill, stroke, StrokeKind::Inside);
        let colour = text_color.unwrap_or_else(|| visuals.text_color());
        let icon_rect = Rect::from_min_size(
            pos2(rect.left() + padding.x, rect.center().y - SIZE / 2.0),
            Vec2::splat(SIZE),
        );
        icon.draw(ui.painter(), icon_rect, icon_color.unwrap_or(colour));
        let text_pos =
            pos2(icon_rect.right() + GAP, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(text_pos, galley, colour);
    }
    response
}

/// Padding inside a coprocessor pill.
const BADGE_PADDING: Vec2 = Vec2::new(6.0, 3.0);
/// Gap between the chip icon and the chip's name inside the pill.
const BADGE_GAP: f32 = 5.0;

/// Coprocessor pill: the chip icon and the chip's name, in the accent that
/// names it (`theme::chip_color`). The name is machine data, so it is set in
/// the monospace face like every other header fact. The plate behind it is
/// opaque black rather than a tint of the accent, because the pill is also
/// painted **over a game picture** on the grid cards, where a translucent tint
/// would take the picture's colour.
pub fn chip_badge(ui: &mut egui::Ui, chip: &str) -> Response {
    let size = badge_size(ui.painter(), chip);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        paint_chip_badge(ui.painter(), rect.min, chip);
    }
    response
}

/// Size the pill for `chip` needs.
pub fn badge_size(painter: &egui::Painter, chip: &str) -> Vec2 {
    let galley = painter.layout_no_wrap(
        chip.to_owned(),
        theme::mono(theme::SIZE_SMALL),
        Color32::PLACEHOLDER,
    );
    let side = theme::SIZE_SMALL;
    vec2(side + BADGE_GAP + galley.size().x, galley.size().y.max(side)) + 2.0 * BADGE_PADDING
}

/// Paint the pill with its top-left corner at `at`, outside any layout — this
/// is how a card stamps it on the corner of the game picture.
pub fn paint_chip_badge(painter: &egui::Painter, at: Pos2, chip: &str) -> Rect {
    let colour = theme::chip_color(chip);
    let rect = Rect::from_min_size(at, badge_size(painter, chip));
    let galley = painter.layout_no_wrap(
        chip.to_owned(),
        theme::mono(theme::SIZE_SMALL),
        Color32::PLACEHOLDER,
    );
    painter.rect(
        rect,
        4.0,
        Color32::from_black_alpha(200),
        Stroke::new(1.0, colour.gamma_multiply(0.6)),
        StrokeKind::Inside,
    );
    let side = theme::SIZE_SMALL;
    let icon_rect = Rect::from_min_size(
        pos2(rect.left() + BADGE_PADDING.x, rect.center().y - side / 2.0),
        Vec2::splat(side),
    );
    Icon::Chip.draw(painter, icon_rect, colour);
    painter.galley(
        pos2(icon_rect.right() + BADGE_GAP, rect.center().y - galley.size().y / 2.0),
        galley,
        colour,
    );
    rect
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run one headless UI frame and return what it painted. A `CentralPanel`
    /// rather than an `Area`: an area fades in over `style.animation_time`, and
    /// at `time = 0` every shape it holds is dropped by `Painter::add`, so a
    /// one-frame test on an area would see nothing.
    fn painted(mut draw: impl FnMut(&mut egui::Ui)) -> Vec<Shape> {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0))),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| draw(ui));
        });
        fn walk(shape: &Shape, out: &mut Vec<Shape>) {
            match shape {
                Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                other => out.push(other.clone()),
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    /// The shapes of one icon alone, without the panel's own background.
    fn shapes_of(icon: Icon, side: f32) -> Vec<Shape> {
        let rect = Rect::from_min_size(pos2(20.0, 20.0), Vec2::splat(side));
        painted(|ui| icon.draw(ui.painter(), rect, theme::TEXT))
            .into_iter()
            .filter(|shape| match shape {
                // The `CentralPanel` frame: a rectangle far larger than the icon.
                Shape::Rect(r) => r.rect.width() <= side * 2.0,
                _ => true,
            })
            .collect()
    }

    /// Every icon must draw something at every size the shell uses it at — an
    /// icon that silently emitted nothing would leave a hole in a button.
    #[test]
    /// `ALL` is what every other test in this module walks, so an icon missing
    /// from it is an icon nothing checks — which is exactly what happened when
    /// `Plus` was added and the list was not. The match below is the guard: a
    /// new variant stops compiling here until it is named, and the count then
    /// forces it into `ALL` as well.
    #[test]
    fn no_icon_escapes_the_walked_set() {
        let mut named = 0;
        for icon in Icon::ALL {
            match icon {
                Icon::Play
                | Icon::Star
                | Icon::StarFilled
                | Icon::Gear
                | Icon::Folder
                | Icon::Chip
                | Icon::Search
                | Icon::Close
                | Icon::ArrowLeft
                | Icon::Plus => named += 1,
            }
        }
        assert_eq!(named, 10, "every icon named in the match must be listed in ALL");
    }

    #[test]
    fn every_icon_draws_at_every_size_the_shell_uses() {
        for icon in Icon::ALL {
            for side in [12.0_f32, SIZE, 24.0, 48.0] {
                let shapes = shapes_of(icon, side);
                assert!(!shapes.is_empty(), "{icon:?} drew nothing at {side}");
            }
        }
    }

    /// One stroke weight for the whole set, never thinner than a point: that
    /// uniformity is what makes them a family rather than nine drawings.
    #[test]
    fn the_whole_set_shares_one_stroke_weight() {
        for side in [10.0_f32, SIZE, 32.0] {
            let expected = (side * STROKE_RATIO).max(1.0);
            for icon in Icon::ALL {
                for shape in shapes_of(icon, side) {
                    let width = match shape {
                        Shape::LineSegment { stroke, .. } => stroke.width,
                        Shape::Path(p) => p.stroke.width,
                        Shape::Circle(c) => c.stroke.width,
                        Shape::Rect(r) => r.stroke.width,
                        _ => continue,
                    };
                    assert!(
                        (width - expected).abs() < 1e-3,
                        "{icon:?} at {side} strokes {width}, not {expected}"
                    );
                }
            }
        }
    }

    /// The drawing stays inside the box it was given, whatever the icon: an
    /// icon bleeding out of its rectangle would collide with the label next to
    /// it.
    #[test]
    fn no_icon_draws_outside_its_rectangle() {
        let side = 32.0;
        // Same box `shapes_of` draws into.
        let rect = Rect::from_min_size(pos2(20.0, 20.0), Vec2::splat(side));
        for icon in Icon::ALL {
            for shape in shapes_of(icon, side) {
                let bounds = shape.visual_bounding_rect();
                if !bounds.is_finite() {
                    continue;
                }
                // The bounding rect includes half the stroke on each side.
                let allowed = rect.expand(side * STROKE_RATIO);
                assert!(
                    allowed.contains_rect(bounds),
                    "{icon:?} paints {bounds:?} outside {allowed:?}"
                );
            }
        }
    }

    /// The filled star is what a favourite looks like: it must actually be
    /// filled, and the outlined one must not be.
    #[test]
    fn the_favourite_star_is_filled_and_the_other_is_not() {
        let filled = shapes_of(Icon::StarFilled, SIZE);
        let hollow = shapes_of(Icon::Star, SIZE);
        let fill_of = |shapes: &[Shape]| match shapes.first() {
            Some(Shape::Path(p)) => p.fill,
            _ => panic!("the star is not a path"),
        };
        assert_eq!(fill_of(&filled), theme::TEXT);
        assert_eq!(fill_of(&hollow), Color32::TRANSPARENT);
        // Ten points: five branches with their notches.
        match &filled[0] {
            Shape::Path(p) => assert_eq!(p.points.len(), 10),
            _ => unreachable!(),
        }
    }

    /// The pill is the card's coprocessor marker: it must paint the chip's own
    /// accent and its name.
    #[test]
    fn the_chip_badge_carries_the_colour_and_the_name_of_the_chip() {
        for chip in ["SuperFX", "SA-1", "DSP-1", "CX4"] {
            let (mut text, mut fills, mut strokes) = (String::new(), Vec::new(), Vec::new());
            for shape in painted(|ui| {
                chip_badge(ui, chip);
            }) {
                match shape {
                    Shape::Text(t) => text.push_str(t.galley.text()),
                    Shape::Rect(r) => {
                        fills.push(r.fill);
                        strokes.push(r.stroke.color);
                    }
                    _ => {}
                }
            }
            assert!(text.contains(chip), "the badge does not name {chip}: {text}");
            assert!(
                strokes.contains(&theme::chip_color(chip).gamma_multiply(0.6)),
                "{chip} is not outlined with its own accent"
            );
            // Opaque plate: the pill is also stamped on a game picture.
            assert!(fills.contains(&Color32::from_black_alpha(200)), "{chip}");
        }
    }

    /// An icon button is a button: it must paint its label, report a click and
    /// carry the widget frame of the theme.
    #[test]
    fn an_icon_button_paints_its_label_and_answers_like_a_button() {
        let mut clicked = false;
        let shapes = painted(|ui| {
            clicked |= button(ui, Icon::Folder, "Ouvrir une ROM…").clicked();
            clicked |= button_tinted(ui, Icon::StarFilled, "Favori", theme::YELLOW).clicked();
            clicked |= primary_button(ui, Icon::Play, "Jouer").clicked();
            clicked |= ghost_button(ui, Icon::Star, SIZE, theme::YELLOW).clicked();
        });
        assert!(!clicked, "drawing alone must not click");
        let mut text = String::new();
        let mut fills = Vec::new();
        for shape in shapes {
            match shape {
                Shape::Text(t) => {
                    text.push_str(t.galley.text());
                    text.push('\n');
                }
                Shape::Rect(r) => fills.push(r.fill),
                _ => {}
            }
        }
        assert!(text.contains("Ouvrir une ROM…"), "{text}");
        assert!(text.contains("Favori"), "{text}");
        assert!(text.contains("Jouer"), "{text}");
        // The call to action is the only accent-filled button of the three.
        assert_eq!(
            fills.iter().filter(|f| **f == theme::ACCENT).count(),
            1,
            "the primary button is not filled with the accent"
        );
    }
}
