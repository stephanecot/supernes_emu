//! The Super Nintendo controller, drawn with egui's painter beside the bindings
//! list of `Réglages > Entrées`.
//!
//! Vector art, exactly like the icon set (`ui::icons`): described in a unit box
//! and mapped onto whatever rectangle it is given, one uniform stroke family, no
//! bitmap and no external asset. It is a **diagram**, not a photograph of a
//! controller — flat shapes in the shell's own palette, no fake plastic and no
//! specular highlight.
//!
//! The four face buttons carry the four prism accents, which is the whole point
//! of drawing them: X blue, A red, B yellow, Y green is the real legend of the
//! European / Super Famicom pad *and* the palette the application already speaks
//! in (`theme::ACCENTS`).
//!
//! The drawing is alive: it says which button the panel is waiting a press for
//! (a pulsing ring), which one the pointer is on, and — the useful part — which
//! ones are **held right now** on the keyboard or on a real controller, which
//! turns the settings screen into a pad tester. `hit` maps a point back to a
//! button name so the drawing is also clickable.

use egui::{pos2, vec2, Align2, Color32, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2};
use snes_core::JoypadState;

use super::theme;

// --- geometry, in unit coordinates ----------------------------------------
//
// `x` runs from 0 (left tip of the body) to 1 (right tip); `y` is measured from
// the body's centre line, in the same units, so the drawing keeps its
// proportions at any size.

/// The two ends of the body are half-ellipses, far wider than they are tall —
/// a circle there would turn the silhouette into a peanut instead of a
/// controller. `LOBE_C` is both the centre of the ellipse and its horizontal
/// semi-axis, `LOBE_R` its vertical one.
const LOBE_C: f32 = 0.28;
const LOBE_R: f32 = 0.20;
/// Half-height of the waist between them. The top edge dips more than the
/// bottom one, as it does on the console's own pad — a symmetric waist reads as
/// a bone, not as a controller.
const WAIST_TOP: f32 = 0.158;
const WAIST_BOTTOM: f32 = 0.183;
/// How far the shoulders stick out above the body.
const SHOULDER_RISE: f32 = 0.062;

/// Height of the whole drawing as a fraction of its width.
pub const ASPECT: f32 = 2.0 * LOBE_R + SHOULDER_RISE;

/// Narrowest the pad is worth drawing: below this the letters on the face
/// buttons stop being legible, and an illegible diagram is only clutter.
pub const MIN_W: f32 = 170.0;
/// Widest: past this the drawing would dwarf the list it explains.
pub const MAX_W: f32 = 370.0;
/// Below this width the `SELECT` / `START` legends are dropped: the two words
/// would overlap, and the lozenges alone still read as the two menu buttons.
pub const LEGEND_MIN_W: f32 = 240.0;

const DPAD_C: (f32, f32) = (0.225, 0.010);
/// Half-length and half-width of one arm of the cross.
const DPAD_ARM: f32 = 0.088;
const DPAD_W: f32 = 0.029;

const FACE_C: (f32, f32) = (0.755, 0.0);
/// Distance from the centre of the diamond to the centre of a button.
const FACE_RING: f32 = 0.093;
const FACE_R: f32 = 0.049;

/// Centre of the two menu buttons, their spacing and their size.
const MENU_X: f32 = 0.465;
const MENU_Y: f32 = 0.035;
const MENU_DX: f32 = 0.062;
const MENU_HALF: (f32, f32) = (0.040, 0.013);
/// Slant of the two lozenges, in radians (the SNES prints them tilted).
const MENU_TILT: f32 = -0.30;

/// Half-width of a shoulder, and the middle of the band of it that the body
/// does not cover — the only part of a trigger that is visible, and the only
/// part it makes sense to click.
const SHOULDER_HALF_X: f32 = 0.115;
const SHOULDER_TOP: f32 = -(LOBE_R + SHOULDER_RISE / 2.0);
const SHOULDER_L_X: f32 = 0.26;
const SHOULDER_R_X: f32 = 0.74;

/// Grey of the shell. Lighter than the page so the pad reads as an object laid
/// on it, and blue-leaning like every other surface of the theme.
const BODY: Color32 = Color32::from_rgb(0x33, 0x37, 0x45);
/// Hairline around the body and around the moulded parts.
const EDGE: Color32 = Color32::from_rgb(0x4A, 0x4F, 0x62);
/// The shoulders: one step behind the body, but the same shell — the pair are
/// read as one moulded object, not as tabs stuck on the back.
const BACK: Color32 = Color32::from_rgb(0x2C, 0x30, 0x3E);
/// Dark plastic: the cross and the two menu buttons.
const DARK: Color32 = theme::BG_DEEP;

/// A clickable part of the drawing.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Zone {
    Disc { c: (f32, f32), r: f32 },
    /// A rectangle, possibly slanted about its own centre.
    Bar { c: (f32, f32), half: (f32, f32), tilt: f32 },
}

impl Zone {
    /// Whether the unit-space point `p` falls inside the zone.
    fn contains(self, p: (f32, f32)) -> bool {
        match self {
            Zone::Disc { c, r } => {
                let (dx, dy) = (p.0 - c.0, p.1 - c.1);
                dx * dx + dy * dy <= r * r
            }
            Zone::Bar { c, half, tilt } => {
                let (dx, dy) = (p.0 - c.0, p.1 - c.1);
                let (sin, cos) = (-tilt).sin_cos();
                let x = dx * cos - dy * sin;
                let y = dx * sin + dy * cos;
                x.abs() <= half.0 && y.abs() <= half.1
            }
        }
    }
}

/// The twelve buttons and the part of the drawing each one owns, in
/// `input::BUTTONS` order. The four arms of the cross stop short of its centre,
/// so the dead middle of a D-pad answers to nothing — as it does on the console.
fn zones() -> [(&'static str, Zone); 12] {
    let (cx, cy) = DPAD_C;
    let arm = |dx: f32, dy: f32| {
        let along = (DPAD_ARM - DPAD_W) / 2.0;
        let mid = DPAD_W + along;
        Zone::Bar {
            c: (cx + dx * mid, cy + dy * mid),
            half: if dx == 0.0 { (DPAD_W, along) } else { (along, DPAD_W) },
            tilt: 0.0,
        }
    };
    let face = |dx: f32, dy: f32| Zone::Disc {
        c: (FACE_C.0 + dx * FACE_RING, FACE_C.1 + dy * FACE_RING),
        r: FACE_R,
    };
    let menu = |dx: f32| Zone::Bar {
        c: (MENU_X + dx * MENU_DX, MENU_Y),
        half: MENU_HALF,
        tilt: MENU_TILT,
    };
    // Only the exposed band of a shoulder is clickable: the rest of it is drawn
    // behind the body, where a click means the body and not the trigger.
    let shoulder = |x: f32| Zone::Bar {
        c: (x, SHOULDER_TOP),
        half: (SHOULDER_HALF_X, SHOULDER_RISE / 2.0),
        tilt: 0.0,
    };
    [
        ("Up", arm(0.0, -1.0)),
        ("Down", arm(0.0, 1.0)),
        ("Left", arm(-1.0, 0.0)),
        ("Right", arm(1.0, 0.0)),
        ("B", face(0.0, 1.0)),
        ("A", face(1.0, 0.0)),
        ("Y", face(-1.0, 0.0)),
        ("X", face(0.0, -1.0)),
        ("L", shoulder(SHOULDER_L_X)),
        ("R", shoulder(SHOULDER_R_X)),
        ("Start", menu(1.0)),
        ("Select", menu(-1.0)),
    ]
}

/// The prism accent a face button carries — the legend of the European pad.
/// `None` for everything that is moulded in plastic on the real controller.
pub fn accent_of(name: &str) -> Option<Color32> {
    match name {
        "X" => Some(theme::BLUE),
        "A" => Some(theme::RED),
        "B" => Some(theme::YELLOW),
        "Y" => Some(theme::GREEN),
        _ => None,
    }
}

/// Whether `name` is held in `state`. Unknown names are not held.
pub fn is_pressed(state: &JoypadState, name: &str) -> bool {
    match name {
        "Up" => state.up,
        "Down" => state.down,
        "Left" => state.left,
        "Right" => state.right,
        "A" => state.a,
        "B" => state.b,
        "X" => state.x,
        "Y" => state.y,
        "L" => state.l,
        "R" => state.r,
        "Start" => state.start,
        "Select" => state.select,
        _ => false,
    }
}

/// Size the drawing takes for an available width, clamped to the range it stays
/// legible in.
pub fn size_for(width: f32) -> Vec2 {
    let w = width.clamp(MIN_W, MAX_W);
    vec2(w, w * ASPECT)
}

/// The button under `pos`, or `None` for the body and everything around it.
pub fn hit(rect: Rect, pos: Pos2) -> Option<&'static str> {
    let f = Frame::new(rect);
    let p = f.unit(pos);
    zones().into_iter().find(|(_, zone)| zone.contains(p)).map(|(name, _)| name)
}

/// Everything the drawing shows beyond the pad itself.
pub struct Pad<'a> {
    /// Button the rest of the panel is pointing at — the row under the pointer.
    pub highlight: Option<&'a str>,
    /// Button the panel is waiting a press for: it pulses, so the player always
    /// knows *which* binding they are about to set.
    pub capturing: Option<&'a str>,
    /// Buttons held right now, on any device.
    pub pressed: JoypadState,
    /// Buttons with no binding at all: drawn faded, since they answer to
    /// nothing.
    pub unbound: &'a [&'static str],
    /// Context time, in seconds, for the capture pulse.
    pub time: f64,
}

/// Unit space -> points.
struct Frame {
    /// Left tip of the body, on its centre line.
    origin: Pos2,
    /// Width of the drawing in points: one unit.
    s: f32,
}

impl Frame {
    fn new(rect: Rect) -> Self {
        let s = rect.width().min(rect.height() / ASPECT);
        let height = s * ASPECT;
        let left = rect.center().x - s / 2.0;
        let top = rect.center().y - height / 2.0;
        Self { origin: pos2(left, top + s * (LOBE_R + SHOULDER_RISE)), s }
    }

    fn p(&self, x: f32, y: f32) -> Pos2 {
        pos2(self.origin.x + x * self.s, self.origin.y + y * self.s)
    }

    fn l(&self, v: f32) -> f32 {
        v * self.s
    }

    fn unit(&self, p: Pos2) -> (f32, f32) {
        ((p.x - self.origin.x) / self.s, (p.y - self.origin.y) / self.s)
    }
}

/// How one part of the drawing is painted right now.
struct Vis {
    fill: Color32,
    /// Ring drawn around the shape: hover, and the capture pulse.
    ring: Option<Stroke>,
    label: Color32,
}

/// Resolve a part's state. `base` is its resting colour, `lit` what it becomes
/// under a finger.
fn vis(name: &str, pad: &Pad, f: &Frame, base: Color32, lit: Color32, label: Color32) -> Vis {
    let held = is_pressed(&pad.pressed, name);
    let hot = pad.highlight == Some(name);
    let waiting = pad.capturing == Some(name);
    let unbound = pad.unbound.iter().any(|b| *b == name);

    // The awaited button pulses in two ways at once — it brightens *and* it is
    // ringed — because "which button am I setting" is the one question this
    // drawing must never leave open. Neither ever goes fully off: a pulse that
    // blinks out stops saying "this one" between two beats.
    let k = 0.55 + 0.45 * (pad.time * 5.0).sin().abs() as f32;
    let (mut fill, ring) = if waiting {
        (
            base.lerp_to_gamma(theme::TEXT, 0.10 + 0.32 * k),
            Some(Stroke::new(f.l(0.012).max(1.8), theme::TEXT.gamma_multiply(k))),
        )
    } else if held {
        (lit, Some(Stroke::new(f.l(0.007).max(1.2), theme::TEXT)))
    } else if hot {
        (
            base.lerp_to_gamma(theme::TEXT, 0.22),
            Some(Stroke::new(f.l(0.006).max(1.0), theme::TEXT.gamma_multiply(0.85))),
        )
    } else {
        (base, None)
    };
    if unbound && !held && !waiting {
        fill = fill.gamma_multiply(0.42);
    }
    let label = if held || waiting { theme::BG_DEEP } else { label };
    Vis { fill, ring, label }
}

/// Draw the controller inside `rect`.
pub fn paint(painter: &egui::Painter, rect: Rect, pad: &Pad) {
    let f = Frame::new(rect);
    shoulders(painter, &f, pad);
    body(painter, &f);
    dpad(painter, &f, pad);
    menu(painter, &f, pad);
    faces(painter, &f, pad);
}

/// Half-height of the body at `x`: a circular end on each side, a cosine waist
/// between them.
fn body_half(x: f32, top: bool) -> f32 {
    let waist = if top { WAIST_TOP } else { WAIST_BOTTOM };
    let x = if x > 0.5 { 1.0 - x } else { x };
    if x <= LOBE_C {
        LOBE_R * (1.0 - ((x - LOBE_C) / LOBE_C).powi(2)).max(0.0).sqrt()
    } else {
        let t = ((x - LOBE_C) / (0.5 - LOBE_C)).clamp(0.0, 1.0);
        waist + (LOBE_R - waist) * (1.0 + (std::f32::consts::PI * t).cos()) / 2.0
    }
}

/// How many points the outline is sampled at, per edge.
const OUTLINE_STEPS: usize = 56;

fn body(painter: &egui::Painter, f: &Frame) {
    let mut points = Vec::with_capacity(2 * OUTLINE_STEPS + 2);
    for i in 0..=OUTLINE_STEPS {
        let x = i as f32 / OUTLINE_STEPS as f32;
        points.push(f.p(x, -body_half(x, true)));
    }
    for i in (0..=OUTLINE_STEPS).rev() {
        let x = i as f32 / OUTLINE_STEPS as f32;
        points.push(f.p(x, body_half(x, false)));
    }
    painter.add(Shape::Path(egui::epaint::PathShape {
        points,
        closed: true,
        fill: BODY,
        stroke: Stroke::new(f.l(0.006).max(1.0), EDGE).into(),
    }));
}

fn shoulders(painter: &egui::Painter, f: &Frame, pad: &Pad) {
    for (name, x) in [("L", SHOULDER_L_X), ("R", SHOULDER_R_X)] {
        let v = vis(name, pad, f, BACK, theme::TEXT.gamma_multiply(0.85), theme::TEXT);
        // Drawn from above the body down *into* it: the hidden half is what
        // makes the trigger look moulded into the shell rather than stuck on
        // top of it.
        let rect = Rect::from_min_max(
            f.p(x - SHOULDER_HALF_X, SHOULDER_TOP - SHOULDER_RISE / 2.0),
            f.p(x + SHOULDER_HALF_X, 0.0),
        );
        // Rounded on top only: the square half runs down inside the body, where
        // nothing of it shows.
        let r = f.l(0.026).round().max(1.0) as u8;
        let radius = egui::CornerRadius { nw: r, ne: r, sw: 0, se: 0 };
        painter.rect(rect, radius, v.fill, Stroke::new(f.l(0.005).max(1.0), EDGE), StrokeKind::Inside);
        if let Some(ring) = v.ring {
            painter.rect_stroke(rect, radius, ring, StrokeKind::Outside);
        }
        painter.text(
            f.p(x, SHOULDER_TOP),
            Align2::CENTER_CENTER,
            name,
            theme::strong(f.l(0.042).max(9.0)),
            v.label,
        );
    }
}

fn dpad(painter: &egui::Painter, f: &Frame, pad: &Pad) {
    let (cx, cy) = DPAD_C;
    let radius = f.l(0.010);
    // The cross itself, in one piece: two bars of the same dark plastic, so no
    // seam shows where they meet.
    for half in [(DPAD_W, DPAD_ARM), (DPAD_ARM, DPAD_W)] {
        painter.rect_filled(
            Rect::from_min_max(f.p(cx - half.0, cy - half.1), f.p(cx + half.0, cy + half.1)),
            radius,
            DARK,
        );
    }
    for (name, dx, dy) in
        [("Up", 0.0, -1.0), ("Down", 0.0, 1.0), ("Left", -1.0, 0.0), ("Right", 1.0, 0.0)]
    {
        let v = vis(name, pad, f, DARK, theme::TEXT.gamma_multiply(0.9), theme::TEXT_DIM);
        // One arm, drawn from the middle of the cross out to its tip, so a lit
        // direction stays joined to the rest of the cross.
        let reach = |d: f32| if d == 0.0 { DPAD_W } else { DPAD_ARM };
        let rect = Rect::from_min_max(
            f.p(cx - if dx < 0.0 { reach(dx) } else { DPAD_W }, cy - if dy < 0.0 { reach(dy) } else { DPAD_W }),
            f.p(cx + if dx > 0.0 { reach(dx) } else { DPAD_W }, cy + if dy > 0.0 { reach(dy) } else { DPAD_W }),
        );
        if v.fill != DARK {
            painter.rect_filled(rect, radius, v.fill);
        }
        if let Some(ring) = v.ring {
            painter.rect_stroke(rect, radius, ring, StrokeKind::Outside);
        }
    }
}

fn menu(painter: &egui::Painter, f: &Frame, pad: &Pad) {
    for (name, dx) in [("Select", -1.0), ("Start", 1.0)] {
        let v = vis(name, pad, f, DARK, theme::TEXT.gamma_multiply(0.9), theme::TEXT_DIM);
        let centre = f.p(MENU_X + dx * MENU_DX, MENU_Y);
        let (sin, cos) = MENU_TILT.sin_cos();
        let (hx, hy) = (f.l(MENU_HALF.0), f.l(MENU_HALF.1));
        let corner = |sx: f32, sy: f32| {
            let (x, y) = (sx * hx, sy * hy);
            centre + vec2(x * cos - y * sin, x * sin + y * cos)
        };
        let points = vec![corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)];
        painter.add(Shape::convex_polygon(points.clone(), v.fill, Stroke::NONE));
        if let Some(ring) = v.ring {
            painter.add(Shape::closed_line(
                points.iter().map(|p| *p + (*p - centre).normalized() * f.l(0.008)).collect(),
                ring,
            ));
        }
        // The legend the console prints under the two buttons — but only while
        // there is room for it: at the smallest size the two words run into
        // each other, and "SELECSTART" is worse than no legend at all.
        if f.s >= LEGEND_MIN_W {
            painter.text(
                f.p(MENU_X + dx * MENU_DX, MENU_Y + 0.060),
                Align2::CENTER_CENTER,
                if name == "Start" { "START" } else { "SELECT" },
                theme::font(f.l(0.030).max(7.5)),
                if is_pressed(&pad.pressed, name) { theme::TEXT } else { theme::TEXT_DIM },
            );
        }
    }
}

fn faces(painter: &egui::Painter, f: &Frame, pad: &Pad) {
    for (name, dx, dy) in [("X", 0.0, -1.0), ("A", 1.0, 0.0), ("B", 0.0, 1.0), ("Y", -1.0, 0.0)] {
        let accent = accent_of(name).expect("a face button carries an accent");
        // A pressed face button keeps its colour and gains light: washing it out
        // to white would cost the very thing the four buttons are drawn for.
        let v = vis(name, pad, f, accent, accent.lerp_to_gamma(Color32::WHITE, 0.30), theme::BG_DEEP);
        let centre = f.p(FACE_C.0 + dx * FACE_RING, FACE_C.1 + dy * FACE_RING);
        let r = f.l(FACE_R);
        painter.circle_filled(centre, r, v.fill);
        if let Some(ring) = v.ring {
            painter.circle_stroke(centre, r + ring.width, ring);
        }
        painter.text(
            centre,
            Align2::CENTER_CENTER,
            name,
            theme::strong(f.l(FACE_R * 1.15).max(8.0)),
            v.label,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOX: Rect = Rect::from_min_max(pos2(100.0, 50.0), pos2(400.0, 50.0 + 300.0 * ASPECT));

    fn frame() -> Frame {
        Frame::new(BOX)
    }

    /// The twelve buttons of the console, each with a part of the drawing of
    /// its own — the list the bindings table walks.
    #[test]
    fn every_snes_button_owns_a_zone_of_the_drawing() {
        let named: Vec<&str> = zones().iter().map(|(n, _)| *n).collect();
        assert_eq!(named, crate::input::BUTTONS.to_vec());
    }

    /// Hit-testing is what makes the drawing clickable; this geometry rots
    /// silently otherwise.
    #[test]
    fn a_point_inside_a_button_names_it_and_the_body_names_nothing() {
        let f = frame();
        // Dead centre of each face button.
        for (name, dx, dy) in
            [("X", 0.0, -1.0), ("A", 1.0, 0.0), ("B", 0.0, 1.0), ("Y", -1.0, 0.0)]
        {
            let p = f.p(FACE_C.0 + dx * FACE_RING, FACE_C.1 + dy * FACE_RING);
            assert_eq!(hit(BOX, p), Some(name), "{name} at {p:?}");
        }
        // The middle of the diamond, between the four of them, is not one.
        assert_eq!(hit(BOX, f.p(FACE_C.0, FACE_C.1)), None);
        // The four arms of the cross, near their tips.
        for (name, dx, dy) in
            [("Up", 0.0, -1.0), ("Down", 0.0, 1.0), ("Left", -1.0, 0.0), ("Right", 1.0, 0.0)]
        {
            let p = f.p(DPAD_C.0 + dx * DPAD_ARM * 0.9, DPAD_C.1 + dy * DPAD_ARM * 0.9);
            assert_eq!(hit(BOX, p), Some(name), "{name}");
        }
        // The dead middle of the cross, the two menu buttons, the shoulders.
        assert_eq!(hit(BOX, f.p(DPAD_C.0, DPAD_C.1)), None, "the middle of the cross");
        assert_eq!(hit(BOX, f.p(MENU_X - MENU_DX, MENU_Y)), Some("Select"));
        assert_eq!(hit(BOX, f.p(MENU_X + MENU_DX, MENU_Y)), Some("Start"));
        assert_eq!(hit(BOX, f.p(SHOULDER_L_X, SHOULDER_TOP)), Some("L"));
        assert_eq!(hit(BOX, f.p(SHOULDER_R_X, SHOULDER_TOP)), Some("R"));
        // Plain body: between the cross and the menu buttons, under the
        // diamond, and outside the drawing altogether.
        for (x, y) in [(0.36_f32, 0.0_f32), (0.5, -0.09), (0.775, 0.165), (0.02, 0.0), (0.5, 0.30)] {
            assert_eq!(hit(BOX, f.p(x, y)), None, "({x}, {y}) is not a button");
        }
        assert_eq!(hit(BOX, pos2(0.0, 0.0)), None);
    }

    /// No two buttons may claim the same point, or a click would rebind
    /// whichever one happened to be listed first.
    #[test]
    fn no_two_zones_overlap() {
        let zones = zones();
        for x in 0..120 {
            for y in 0..60 {
                let p = (x as f32 / 120.0, y as f32 / 60.0 * 0.6 - 0.30);
                let claims: Vec<&str> =
                    zones.iter().filter(|(_, z)| z.contains(p)).map(|(n, _)| *n).collect();
                assert!(claims.len() <= 1, "{p:?} is claimed by {claims:?}");
            }
        }
    }

    /// Every part of the drawing stays inside the rectangle it was given: the
    /// pad sits next to a list of bindings and must never paint over it.
    #[test]
    fn the_drawing_stays_inside_its_rectangle() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(600.0, 400.0))),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                paint(
                    ui.painter(),
                    BOX,
                    &Pad {
                        highlight: Some("A"),
                        capturing: Some("Start"),
                        pressed: JoypadState { up: true, l: true, ..Default::default() },
                        unbound: &["Select"],
                        time: 0.4,
                    },
                );
            });
        });
        fn walk(shape: &Shape, out: &mut Vec<Shape>) {
            match shape {
                Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                other => out.push(other.clone()),
            }
        }
        let mut shapes = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut shapes);
        }
        let allowed = BOX.expand(3.0);
        let mut painted = 0;
        for shape in &shapes {
            let bounds = shape.visual_bounding_rect();
            // The panel paints its own background, far larger than the pad.
            if !bounds.is_finite() || bounds.contains_rect(allowed) {
                continue;
            }
            painted += 1;
            assert!(allowed.contains_rect(bounds), "{shape:?} paints {bounds:?} outside {allowed:?}");
        }
        assert!(painted > 12, "the pad drew almost nothing: {painted} shapes");
    }

    /// The colours are the point of the drawing: the European legend, taken
    /// from the four accents the application already owns.
    #[test]
    fn the_face_buttons_carry_the_four_prism_accents() {
        assert_eq!(accent_of("X"), Some(theme::BLUE));
        assert_eq!(accent_of("A"), Some(theme::RED));
        assert_eq!(accent_of("B"), Some(theme::YELLOW));
        assert_eq!(accent_of("Y"), Some(theme::GREEN));
        assert_eq!(accent_of("Start"), None);
        assert_eq!(accent_of("Up"), None);
        // X is above A, which is right of B, which is above… the diamond of the
        // real pad, checked on the zones themselves.
        let f = frame();
        let centre = |name: &str| {
            zones()
                .into_iter()
                .find(|(n, _)| *n == name)
                .map(|(_, z)| match z {
                    Zone::Disc { c, .. } => f.p(c.0, c.1),
                    Zone::Bar { c, .. } => f.p(c.0, c.1),
                })
                .unwrap()
        };
        let (x, a, b, y) = (centre("X"), centre("A"), centre("B"), centre("Y"));
        assert!(x.y < a.y && x.y < y.y && a.y < b.y, "the diamond is upside down");
        assert!(y.x < x.x && x.x < a.x, "the diamond is back to front");
        // …and the whole diamond is on the right, the cross on the left.
        assert!(centre("Left").x < y.x);
    }

    /// Every state the drawing has must change what it paints, or the feedback
    /// the section promises is not there.
    #[test]
    fn press_hover_and_capture_each_change_the_drawing() {
        let f = frame();
        let quiet = Pad {
            highlight: None,
            capturing: None,
            pressed: JoypadState::default(),
            unbound: &[],
            time: 0.0,
        };
        let resting = vis("A", &quiet, &f, theme::RED, Color32::WHITE, theme::BG_DEEP);
        assert_eq!(resting.fill, theme::RED);
        assert!(resting.ring.is_none());

        let held = Pad { pressed: JoypadState { a: true, ..Default::default() }, ..quiet };
        assert_eq!(vis("A", &held, &f, theme::RED, Color32::WHITE, theme::BG_DEEP).fill, Color32::WHITE);

        let hot = Pad { highlight: Some("A"), ..quiet };
        let v = vis("A", &hot, &f, theme::RED, Color32::WHITE, theme::BG_DEEP);
        assert_ne!(v.fill, theme::RED, "a hovered button must light up");
        assert!(v.ring.is_some(), "a hovered button must be ringed");

        // The capture ring pulses but never disappears, at any instant.
        for t in 0..40 {
            let waiting = Pad { capturing: Some("A"), time: t as f64 * 0.05, ..quiet };
            let ring = vis("A", &waiting, &f, theme::RED, Color32::WHITE, theme::BG_DEEP)
                .ring
                .expect("the awaited button must be ringed");
            assert!(ring.color.a() > 90, "the pulse went out at t={t}: {ring:?}");
            assert!(ring.width >= 1.5);
        }

        // A button with no binding at all is faded, and lights up all the same
        // when it is physically pressed — that is how a pad tester says the
        // hardware works even though nothing is mapped to it.
        let none = Pad { unbound: &["A"], ..quiet };
        let faded = vis("A", &none, &f, theme::RED, Color32::WHITE, theme::BG_DEEP).fill;
        assert!(faded.a() < 255 || faded != theme::RED, "an unbound button must be faded");
        let none_held =
            Pad { unbound: &["A"], pressed: JoypadState { a: true, ..Default::default() }, ..quiet };
        assert_eq!(
            vis("A", &none_held, &f, theme::RED, Color32::WHITE, theme::BG_DEEP).fill,
            Color32::WHITE
        );
    }

    #[test]
    fn the_pressed_state_is_read_button_by_button() {
        for name in crate::input::BUTTONS {
            let mut state = JoypadState::default();
            crate::input::set_button(&mut state, name, true).expect("known button");
            for other in crate::input::BUTTONS {
                assert_eq!(is_pressed(&state, other), other == name, "{name} vs {other}");
            }
        }
        assert!(!is_pressed(&JoypadState::default(), "Turbo"));
    }

    /// The drawing keeps its proportions and stays legible whatever width it is
    /// given.
    #[test]
    fn the_size_is_clamped_to_the_range_it_stays_legible_in() {
        for width in [0.0_f32, 120.0, MIN_W, 260.0, MAX_W, 900.0] {
            let size = size_for(width);
            assert!((MIN_W..=MAX_W).contains(&size.x), "{width} -> {size:?}");
            assert!((size.y - size.x * ASPECT).abs() < 0.01);
        }
        // A pad drawn in a box taller than it needs is centred in it, not
        // stretched: the same drawing, same aspect.
        let tall = Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 400.0));
        let f = Frame::new(tall);
        assert!((f.s - 300.0).abs() < 0.01);
        assert!((f.p(0.5, 0.0).y - tall.center().y).abs() < 0.01 + 300.0 * SHOULDER_RISE / 2.0);
    }
}
