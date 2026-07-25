//! Prisme visual system: palette, embedded typefaces, type scale, the product
//! mark and the spectral rule.
//!
//! **Colour.** A deep, slightly blue slate rather than black (`#16171F` base,
//! `#1E2029` surfaces) and the four colours the product icon refracts: red
//! `#E45C5C`, yellow `#F0C24A`, green `#5BC15B`, blue `#4C86E0`. The four
//! accents carry meaning and nothing else: one colour per **coprocessor**
//! (`chip_color`), yellow for a **favourite**, and the four together only in
//! the mark and in the spectral rule. No accent is ever decorative, and no
//! surface is saturated — on screen the emulated picture must be the only
//! vivid thing.
//!
//! **Type.** Two embedded faces, so the shell does not look like a prototype
//! and renders identically on every machine:
//! * *Space Grotesk* (Regular + Bold), a geometric grotesque, for the
//!   interface and its titles — © 2020 The Space Grotesk Project Authors,
//!   SIL Open Font License 1.1 (`assets/fonts/SpaceGrotesk-OFL.txt`).
//! * *IBM Plex Mono* (Regular) for **machine data** — region, mapping,
//!   checksum, sizes, key bindings, paths — © 2017 IBM Corp., SIL Open Font
//!   License 1.1 (`assets/fonts/IBMPlexMono-OFL.txt`).
//!
//! The OFL allows embedding in a binary and redistribution; both licence texts
//! ship next to the faces and are named in the README. egui's built-in faces
//! are kept *behind* ours as fallbacks, so a glyph neither face has (emoji,
//! box drawing) still draws instead of a tofu box.
//!
//! `apply` is idempotent and needs no window: it only writes into an
//! `egui::Context`, which is why it can be exercised by unit tests on a
//! machine with no display.

use std::sync::Arc;

use egui::{Color32, CornerRadius, FontData, FontFamily, FontId, Rect, Shape, Stroke, TextStyle, Vec2};

/// Prism red.
pub const RED: Color32 = Color32::from_rgb(0xE4, 0x5C, 0x5C);
/// Prism yellow.
pub const YELLOW: Color32 = Color32::from_rgb(0xF0, 0xC2, 0x4A);
/// Prism green.
pub const GREEN: Color32 = Color32::from_rgb(0x5B, 0xC1, 0x5B);
/// Prism blue.
pub const BLUE: Color32 = Color32::from_rgb(0x4C, 0x86, 0xE0);

/// The four accents in the order the prism refracts them in the product icon —
/// red is the least deviated beam, blue the most. The mark, the spectral rule
/// and `accent(i)` all use this order.
pub const ACCENTS: [Color32; 4] = [RED, YELLOW, GREEN, BLUE];

/// Primary interactive accent (selection, focus, primary button): the blue of
/// the prism, the least alarming of the four for a permanent UI role.
pub const ACCENT: Color32 = BLUE;

/// Inset background: the base darkened one step, for what sits *under* a
/// surface (text fields, the plate behind a game picture, the footer).
pub const BG_DEEP: Color32 = Color32::from_rgb(0x10, 0x11, 0x19);
/// Base background, behind every panel.
pub const BG_PANEL: Color32 = Color32::from_rgb(0x16, 0x17, 0x1F);
/// Surface: card, modal, inset panel.
pub const BG_CARD: Color32 = Color32::from_rgb(0x1E, 0x20, 0x29);
/// Resting fill of an interactive widget — the same surface as a card, so a
/// button reads as a raised part of the page rather than a separate material.
pub const BG_WIDGET: Color32 = Color32::from_rgb(0x1E, 0x20, 0x29);
/// Hovered fill.
pub const BG_WIDGET_HOVER: Color32 = Color32::from_rgb(0x27, 0x2A, 0x36);
/// Pressed / active fill.
pub const BG_WIDGET_ACTIVE: Color32 = Color32::from_rgb(0x31, 0x35, 0x45);
/// Hairline borders and separators.
pub const STROKE: Color32 = Color32::from_rgb(0x2A, 0x2D, 0x3A);
/// Body text.
pub const TEXT: Color32 = Color32::from_rgb(0xE8, 0xE9, 0xF0);
/// Secondary text (captions, hints, paths).
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9A, 0xA0, 0xB4);
/// Veil painted behind a modal. Dense enough that the screen underneath stops
/// competing for attention on a background this dark — egui's default
/// (`from_black_alpha(100)`) does not darken a `#16171F` page perceptibly.
pub const VEIL: Color32 = Color32::from_black_alpha(190);

/// Cycle through the four prism accents; `i` may be any index, so a list of
/// arbitrary length can be tinted without bounds checks at the call site.
pub fn accent(i: usize) -> Color32 {
    ACCENTS[i % ACCENTS.len()]
}

/// The accent that names a coprocessor. Each detected chip
/// (`library::coprocessor`) owns one of the four colours for good, so the pill
/// on a card is read by colour before it is read by name. Anything else —
/// a chip this build does not know — stays neutral rather than borrowing a
/// colour that means another chip.
pub fn chip_color(chip: &str) -> Color32 {
    match chip {
        "SuperFX" => GREEN,
        "SA-1" => RED,
        "DSP-1" => BLUE,
        "CX4" => YELLOW,
        _ => TEXT_DIM,
    }
}

// --- typography -----------------------------------------------------------

/// Space Grotesk Regular — SIL OFL 1.1, see the module docs.
const UI_REGULAR: &[u8] = include_bytes!("../../assets/fonts/SpaceGrotesk-Regular.ttf");
/// Space Grotesk Bold — the display weight of the scale.
const UI_BOLD: &[u8] = include_bytes!("../../assets/fonts/SpaceGrotesk-Bold.ttf");
/// IBM Plex Mono Regular — SIL OFL 1.1, machine data only.
const MONO_REGULAR: &[u8] = include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf");

/// Family name of the bold interface face. egui has no synthetic bold: a heavy
/// weight is a family of its own (`RichText::strong` only changes colour).
pub const FAMILY_STRONG: &str = "prisme-strong";

/// Explicit type scale — four steps far enough apart to be told at a glance,
/// plus the size machine data is set at.
/// Screen title (product name, game title, panel title), always bold.
pub const SIZE_TITLE: f32 = 24.0;
/// Section heading.
pub const SIZE_HEADING: f32 = 17.0;
/// Body and controls.
pub const SIZE_BODY: f32 = 14.0;
pub const SIZE_BUTTON: f32 = 14.0;
/// Captions, hints, metadata.
pub const SIZE_SMALL: f32 = 12.0;
/// Machine data. IBM Plex Mono runs optically larger than Space Grotesk, so a
/// point below body size keeps a value and its label on the same line.
pub const SIZE_MONO: f32 = 13.0;

/// Interface face at `size` (Space Grotesk Regular).
pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Interface face at `size`, bold (Space Grotesk Bold).
pub fn strong(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_STRONG.into()))
}

/// Machine-data face at `size` (IBM Plex Mono).
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// Install the two embedded faces on `ctx`, keeping egui's built-ins as
/// fallbacks behind them.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes) in [
        ("prisme-ui", UI_REGULAR),
        ("prisme-ui-bold", UI_BOLD),
        ("prisme-mono", MONO_REGULAR),
    ] {
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    }

    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "prisme-ui".to_owned());
    // The bold family falls back to the regular weight first: a glyph Space
    // Grotesk Bold lacks must not drop straight to egui's built-in face, whose
    // shapes would clash mid-word.
    let mut strong_family = vec!["prisme-ui-bold".to_owned()];
    strong_family.extend(proportional.iter().cloned());
    fonts.families.insert(FontFamily::Name(FAMILY_STRONG.into()), strong_family);
    fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "prisme-mono".to_owned());

    ctx.set_fonts(fonts);
}

/// Install the Prisme theme (fonts, palette, spacing) on `ctx`. Safe to call
/// more than once.
pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG_PANEL;
    visuals.window_fill = BG_CARD;
    visuals.extreme_bg_color = BG_DEEP;
    visuals.faint_bg_color = BG_CARD;
    visuals.code_bg_color = BG_CARD;
    visuals.override_text_color = Some(TEXT);
    visuals.weak_text_color = Some(TEXT_DIM);
    visuals.hyperlink_color = ACCENT;
    visuals.warn_fg_color = YELLOW;
    visuals.error_fg_color = RED;
    visuals.window_stroke = Stroke::new(1.0, STROKE);
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);

    // Sharp-ish corners: the same radius drives a button *and* the 14-point
    // square of a checkbox (`egui::Checkbox` paints its box with the widget's
    // own `corner_radius`), and a radius above 5 turns that square into a
    // circle, which reads as a radio button.
    let radius = CornerRadius::same(4);
    visuals.widgets.noninteractive.bg_fill = BG_CARD;
    visuals.widgets.noninteractive.weak_bg_fill = BG_CARD;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, STROKE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
    visuals.widgets.noninteractive.corner_radius = radius;

    visuals.widgets.inactive.bg_fill = BG_WIDGET;
    visuals.widgets.inactive.weak_bg_fill = BG_WIDGET;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, STROKE);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.inactive.corner_radius = radius;

    visuals.widgets.hovered.bg_fill = BG_WIDGET_HOVER;
    visuals.widgets.hovered.weak_bg_fill = BG_WIDGET_HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.5, TEXT);
    visuals.widgets.hovered.corner_radius = radius;

    visuals.widgets.active.bg_fill = BG_WIDGET_ACTIVE;
    visuals.widgets.active.weak_bg_fill = BG_WIDGET_ACTIVE;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.5, TEXT);
    visuals.widgets.active.corner_radius = radius;

    visuals.widgets.open.bg_fill = BG_WIDGET;
    visuals.widgets.open.weak_bg_fill = BG_WIDGET;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, STROKE);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.open.corner_radius = radius;

    ctx.set_visuals(visuals);

    ctx.style_mut(|style| {
        style.text_styles.insert(TextStyle::Heading, strong(SIZE_HEADING));
        style.text_styles.insert(TextStyle::Body, font(SIZE_BODY));
        style.text_styles.insert(TextStyle::Button, font(SIZE_BUTTON));
        style.text_styles.insert(TextStyle::Small, font(SIZE_SMALL));
        style.text_styles.insert(TextStyle::Monospace, mono(SIZE_MONO));
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        // A scroll bar that takes room of its own instead of floating over the
        // content: egui's floating bars are invisible at rest, so a grid with
        // three more rows below the fold looked like it had none, and the bar
        // covered the last column when it did appear.
        style.spacing.scroll.floating = false;
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style.spacing.window_margin = egui::Margin::same(16);
        style.visuals.button_frame = true;
    });
}

// --- identity -------------------------------------------------------------

/// Draw the product mark inside `rect`: the application's own icon, a prism
/// splitting one white beam into the four accents (red the least deviated,
/// blue the most). Vector-drawn rather than an image so it stays crisp at any
/// size and needs no texture; the stroke widths have a floor in points, which
/// is what keeps the triangle and the four beams legible down to ~16 points.
///
/// The drawing is inscribed in the largest square centred in `rect`, and its
/// proportions are read off `packaging/icon-source.png`: an **equilateral**
/// triangle standing on a horizontal base (base `0.52`, height `0.45`, i.e. the
/// 0.866 ratio of an equilateral one), the white beam arriving **horizontally**
/// at mid-height, and the four beams leaving the right face in a wide fan. The
/// earlier drawing used a tall, narrow triangle and a sloping incoming beam,
/// which is what made the in-app mark read as a different logo from the one on
/// the dock icon.
pub fn mark(painter: &egui::Painter, rect: Rect) {
    let side = rect.width().min(rect.height());
    let origin = rect.center() - Vec2::splat(side / 2.0);
    let p = |x: f32, y: f32| origin + Vec2::new(x * side, y * side);
    let edge = (side * 0.045).max(1.0);
    let beam = (side * 0.06).max(1.0);

    // The prism: the icon's equilateral triangle, apex up, faintly lit like its
    // glass and outlined in the primary text colour. Left of centre so the
    // spectrum has the right half of the box to fan out in.
    let apex = p(0.36, 0.33);
    let left = p(0.10, 0.78);
    let right = p(0.62, 0.78);
    painter.add(Shape::convex_polygon(
        vec![apex, left, right],
        TEXT.gamma_multiply(0.14),
        Stroke::new(edge, TEXT),
    ));

    // White light arriving horizontally and stopping exactly on the left face
    // (the face runs from `left` to `apex`, so at y = 0.50 it is at x = 0.262)…
    painter.line_segment([p(0.0, 0.50), p(0.262, 0.50)], Stroke::new(beam, TEXT));
    // …and the spectrum leaving the right face at x = 0.470, red the least
    // deviated, blue the most.
    let exit = p(0.470, 0.52);
    for (i, y) in [0.58_f32, 0.70, 0.83, 0.96].into_iter().enumerate() {
        painter.line_segment([exit, p(1.0, y)], Stroke::new(beam, accent(i)));
    }
}

/// Height of the spectral rule, in points.
pub const SPECTRAL_RULE_H: f32 = 2.0;

/// The signature element: a hairline broken into the four prism colours, in
/// refraction order. Used **once** in the shell — under the active view's
/// heading — so it reads as a navigation landmark and not as decoration.
pub fn spectral_rule(painter: &egui::Painter, rect: Rect) {
    let segment = rect.width() / ACCENTS.len() as f32;
    for (i, colour) in ACCENTS.iter().enumerate() {
        let x = rect.left() + i as f32 * segment;
        painter.rect_filled(
            Rect::from_min_max(
                egui::pos2(x, rect.top()),
                egui::pos2((x + segment).min(rect.right()), rect.bottom()),
            ),
            0.0,
            *colour,
        );
    }
}

/// `BG_PANEL` as the linear-space clear value wgpu expects for an sRGB surface
/// (IEC 61966-2-1 electro-optical transfer function). Used when egui owns the
/// whole window and there is no emulated picture underneath.
pub fn clear_color() -> [f64; 4] {
    fn to_linear(c: u8) -> f64 {
        let s = c as f64 / 255.0;
        if s <= 0.040_45 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    [to_linear(BG_PANEL.r()), to_linear(BG_PANEL.g()), to_linear(BG_PANEL.b()), 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run one headless UI frame and return the shapes it painted. A
    /// `CentralPanel` rather than an `Area`: an area fades in over
    /// `style.animation_time`, and at `time = 0` every shape it holds is
    /// replaced by a no-op (`egui::Painter::add` short-circuits at zero
    /// opacity), so a one-frame test on an area sees nothing at all.
    fn painted(mut draw: impl FnMut(&egui::Painter)) -> Vec<Shape> {
        let ctx = egui::Context::default();
        apply(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0))),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| draw(ui.painter()));
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
        shapes
    }

    /// `#RRGGBB` of a colour, so a failing assertion names the palette entry
    /// the way the design does.
    fn hex(c: Color32) -> String {
        format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
    }

    #[test]
    fn the_four_prism_accents_are_the_specified_hex_values() {
        assert_eq!(hex(RED), "#E45C5C");
        assert_eq!(hex(YELLOW), "#F0C24A");
        assert_eq!(hex(GREEN), "#5BC15B");
        assert_eq!(hex(BLUE), "#4C86E0");
        assert_eq!(ACCENTS, [RED, YELLOW, GREEN, BLUE]);
    }

    #[test]
    fn the_surfaces_are_the_specified_slate() {
        assert_eq!(hex(BG_PANEL), "#16171F");
        assert_eq!(hex(BG_CARD), "#1E2029");
        assert_eq!(hex(TEXT), "#E8E9F0");
        assert_eq!(hex(TEXT_DIM), "#9AA0B4");
        // Blue-leaning, never a neutral grey: blue is the strongest channel of
        // every surface.
        for c in [BG_DEEP, BG_PANEL, BG_CARD, BG_WIDGET, BG_WIDGET_HOVER, BG_WIDGET_ACTIVE] {
            assert!(c.b() > c.r() && c.b() > c.g(), "{} is not blue-leaning", hex(c));
        }
    }

    #[test]
    fn accents_are_four_distinct_opaque_colours() {
        let mut seen = std::collections::BTreeSet::new();
        for c in ACCENTS {
            assert_eq!(c.a(), 255, "{} must be opaque", hex(c));
            assert!(seen.insert(hex(c)), "duplicate accent {}", hex(c));
        }
        assert_eq!(seen.len(), 4);
    }

    #[test]
    fn accent_cycles_through_the_palette() {
        for i in 0..12 {
            assert_eq!(accent(i), ACCENTS[i % 4]);
        }
    }

    /// The four chips the library detects must each own one accent, and no two
    /// may share one — the pill's colour is what identifies the chip before its
    /// name is read.
    #[test]
    fn every_detected_coprocessor_owns_one_accent_of_its_own() {
        let chips = ["SuperFX", "SA-1", "DSP-1", "CX4"];
        let mut seen = std::collections::BTreeSet::new();
        for chip in chips {
            let c = chip_color(chip);
            assert!(ACCENTS.contains(&c), "{chip} is not tinted with a prism accent");
            assert!(seen.insert(hex(c)), "{chip} shares its colour with another chip");
        }
        assert_eq!(seen.len(), ACCENTS.len());
        // An unknown chip stays neutral rather than claiming another's colour.
        assert_eq!(chip_color("S-DD1"), TEXT_DIM);
        assert_eq!(chip_color(""), TEXT_DIM);
    }

    #[test]
    fn the_theme_is_dark_everywhere() {
        // "Dark" here means every surface colour stays well below mid grey, so
        // the emulated picture is the brightest thing on screen.
        for c in [BG_DEEP, BG_PANEL, BG_CARD, BG_WIDGET, BG_WIDGET_HOVER, BG_WIDGET_ACTIVE] {
            let max = c.r().max(c.g()).max(c.b());
            assert!(max < 96, "{} is not a dark surface", hex(c));
        }
        // …and the text stays clearly above it.
        assert!(TEXT.r() > 200 && TEXT.g() > 200 && TEXT.b() > 200);
        assert!(TEXT_DIM.r() > 128);
    }

    /// Four steps, each far enough from the next to be told apart at a glance
    /// (the brief's complaint was "three sizes that are nearly the same").
    #[test]
    fn the_type_scale_has_four_distinct_steps() {
        let scale = [SIZE_TITLE, SIZE_HEADING, SIZE_BODY, SIZE_SMALL];
        for pair in scale.windows(2) {
            let (big, small) = (pair[0], pair[1]);
            assert!(big > small, "{big} must be above {small}");
            assert!(big / small >= 1.15, "{big} and {small} are too close to be told apart");
        }
        assert_eq!(SIZE_BUTTON, SIZE_BODY, "a control is body text");
        assert!(SIZE_MONO < SIZE_BODY, "machine data is set a point smaller");
    }

    #[test]
    fn apply_installs_the_palette_on_a_context() {
        // Runs without any window or GPU: `egui::Context` is pure CPU state.
        let ctx = egui::Context::default();
        apply(&ctx);
        ctx.style_mut(|_| {}); // force the deferred style write to land
        let style = ctx.style();
        assert!(style.visuals.dark_mode);
        assert_eq!(style.visuals.panel_fill, BG_PANEL);
        assert_eq!(style.visuals.override_text_color, Some(TEXT));
        assert_eq!(style.visuals.selection.stroke.color, ACCENT);
        assert_eq!(style.visuals.error_fg_color, RED);
        assert_eq!(style.visuals.warn_fg_color, YELLOW);
        assert_eq!(
            style.text_styles.get(&TextStyle::Body).map(|f| f.size),
            Some(SIZE_BODY)
        );
        // Idempotent: applying twice yields the same style.
        let before = format!("{:?}", *style);
        drop(style);
        apply(&ctx);
        assert_eq!(format!("{:?}", *ctx.style()), before);
    }

    /// The two embedded faces must actually be installed and reachable, and
    /// egui's built-ins must stay behind them as fallbacks.
    #[test]
    fn the_embedded_faces_are_installed_with_the_builtins_as_fallback() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        ctx.fonts(|fonts| {
            let families = fonts.definitions().families.clone();
            let proportional = &families[&FontFamily::Proportional];
            assert_eq!(proportional[0], "prisme-ui");
            assert!(proportional.len() > 1, "no fallback behind Space Grotesk");
            let bold = &families[&FontFamily::Name(FAMILY_STRONG.into())];
            assert_eq!(bold[0], "prisme-ui-bold");
            assert_eq!(bold[1], "prisme-ui");
            assert_eq!(families[&FontFamily::Monospace][0], "prisme-mono");
        });
    }

    /// Every character the shell actually prints must have a glyph, or it would
    /// paint a tofu box — which is exactly what the default font set did to the
    /// back arrow of the game sheet.
    #[test]
    fn the_faces_cover_the_text_the_shell_prints() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        // French diacritics, the punctuation of the copy, and the units and
        // separators of the machine data.
        const SAMPLE: &str = "ÉÀÇÊÎÔÙéèêàçîôùûïüœ AZaz 0123456789 …·×—–«»'\"()[]{}/\\:;,.?!%+-_@#$&*=<>";
        for family in [
            FontFamily::Proportional,
            FontFamily::Name(FAMILY_STRONG.into()),
            FontFamily::Monospace,
        ] {
            let id = FontId::new(SIZE_BODY, family.clone());
            for c in SAMPLE.chars().filter(|c| !c.is_whitespace()) {
                assert!(
                    ctx.fonts_mut(|f| f.has_glyph(&id, c)),
                    "{family} has no glyph for {c:?}"
                );
            }
        }
    }

    #[test]
    fn clear_color_matches_the_base_background_and_is_opaque() {
        let c = clear_color();
        assert_eq!(c[3], 1.0);
        for v in &c[..3] {
            assert!((0.0..=1.0).contains(v), "{v} out of range");
            assert!(*v < 0.02, "the clear colour must stay very dark, got {v}");
        }
        // Monotonic in the sRGB value it comes from: R(0x16) < G(0x17) < B(0x1F).
        assert!(c[0] < c[1] && c[1] < c[2]);
    }

    /// The mark is the application's icon, not a block of squares: a triangle
    /// (the prism), one white beam in and four coloured beams out. Checked on
    /// the shapes it emits, since there is no other way to look at it here.
    #[test]
    fn the_mark_draws_a_prism_and_four_coloured_beams() {
        // At the header size, then at a size where a fraction of a point would
        // round a stroke away: the mark has to survive both.
        for side in [36.0_f32, 14.0] {
            let rect = Rect::from_min_size(egui::pos2(20.0, 20.0), Vec2::splat(side));
            let mut triangles = 0;
            let mut beams: Vec<(Color32, f32)> = Vec::new();
            for shape in painted(|painter| mark(painter, rect)) {
                match shape {
                    Shape::Path(p) if p.points.len() == 3 => triangles += 1,
                    Shape::LineSegment { stroke, .. } => beams.push((stroke.color, stroke.width)),
                    _ => {}
                }
            }
            assert_eq!(triangles, 1, "the prism itself is missing at {side}");
            assert_eq!(beams.len(), 5, "one beam in and four out at {side}: {beams:?}");
            assert_eq!(beams[0].0, TEXT, "the incoming beam is white light");
            let spectrum: Vec<Color32> = beams[1..].iter().map(|(c, _)| *c).collect();
            assert_eq!(spectrum, ACCENTS, "the spectrum is out of refraction order");
            // Legible small: no stroke ever falls below one point.
            assert!(beams.iter().all(|(_, w)| *w >= 1.0), "{beams:?}");
        }
    }

    /// The mark must be the *same drawing* as the application icon, which the
    /// user reported it was not: an equilateral triangle standing on a
    /// horizontal base, and one horizontal beam of white light entering it.
    #[test]
    fn the_marks_prism_has_the_icons_geometry() {
        let side = 200.0_f32;
        let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::splat(side));
        let mut triangle = None;
        let mut white = None;
        for shape in painted(|painter| mark(painter, rect)) {
            match shape {
                Shape::Path(p) if p.points.len() == 3 => triangle = Some(p.points.clone()),
                Shape::LineSegment { points, stroke } if stroke.color == TEXT => {
                    white = Some(points)
                }
                _ => {}
            }
        }
        let t = triangle.expect("no prism");
        let (apex, left, right) = (t[0], t[1], t[2]);
        // Flat base, apex above its middle.
        assert!((left.y - right.y).abs() < 0.01, "the base is not horizontal: {t:?}");
        assert!((apex.x - (left.x + right.x) / 2.0).abs() < 0.01, "the apex is off-centre");
        assert!(apex.y < left.y);
        // Equilateral: height = base * sin(60°), within 5 %.
        let base = right.x - left.x;
        let height = left.y - apex.y;
        let ratio = height / base;
        assert!((ratio - 0.866).abs() < 0.05, "the prism is not equilateral: h/b = {ratio}");
        // The incoming ray is horizontal and lands on the left face.
        let w = white.expect("no white beam");
        assert!((w[0].y - w[1].y).abs() < 0.01, "the incoming beam is not horizontal: {w:?}");
        assert!(w[0].x <= rect.left() + 0.01, "the beam does not come from outside");
        let t = (left.y - w[1].y) / (left.y - apex.y);
        let face_x = left.x + t * (apex.x - left.x);
        assert!((w[1].x - face_x).abs() < 0.5, "the beam stops at {} not at the face {face_x}", w[1].x);
    }

    /// The spectral rule is the one place the four colours appear together
    /// outside the mark: four equal segments, in refraction order, filling the
    /// rectangle exactly.
    #[test]
    fn the_spectral_rule_is_four_equal_segments_in_order() {
        let rect = Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(120.0, SPECTRAL_RULE_H));
        let segments: Vec<(Color32, Rect)> = painted(|painter| spectral_rule(painter, rect))
            .into_iter()
            .filter_map(|shape| match shape {
                Shape::Rect(r) => Some((r.fill, r.rect)),
                _ => None,
            })
            // The panel paints its own background rectangle first.
            .filter(|(_, r)| r.height() <= SPECTRAL_RULE_H)
            .collect();
        assert_eq!(segments.len(), 4);
        for (i, (colour, seg)) in segments.iter().enumerate() {
            assert_eq!(*colour, ACCENTS[i]);
            assert!((seg.width() - rect.width() / 4.0).abs() < 0.01, "{seg:?}");
            assert!((seg.height() - SPECTRAL_RULE_H).abs() < 0.01);
        }
        assert_eq!(segments[0].1.left(), rect.left());
        assert_eq!(segments[3].1.right(), rect.right());
    }
}
