//! Prisme visual identity: dark surfaces, the four prism accent colours, sober
//! typography.
//!
//! The four accents are the ones of the product icon and the pedagogical PDF
//! (`docs/ROADMAP.md`, Phase 8 "Identité visuelle"): red `#E45C5C`, yellow
//! `#F0C24A`, green `#5BC15B`, blue `#4C86E0`. They are used as highlights
//! (title mark, section rules, focus/selection), never as large fills — the
//! surfaces stay neutral so the emulated picture, when it is on screen, is the
//! only saturated thing.
//!
//! `apply` is idempotent and needs no window: it only writes into an
//! `egui::Context`, which is why it can be exercised by unit tests on a
//! machine with no display.

use egui::{Color32, CornerRadius, FontId, Stroke, TextStyle};

/// Prism red.
pub const RED: Color32 = Color32::from_rgb(0xE4, 0x5C, 0x5C);
/// Prism yellow.
pub const YELLOW: Color32 = Color32::from_rgb(0xF0, 0xC2, 0x4A);
/// Prism green.
pub const GREEN: Color32 = Color32::from_rgb(0x5B, 0xC1, 0x5B);
/// Prism blue.
pub const BLUE: Color32 = Color32::from_rgb(0x4C, 0x86, 0xE0);

/// The four accents in their canonical order (red, yellow, green, blue) — the
/// order of the icon's four squares, reused by the title mark and by
/// `accent(i)` for anything that needs to cycle through them.
pub const ACCENTS: [Color32; 4] = [RED, YELLOW, GREEN, BLUE];

/// Primary interactive accent (selection, focus, primary button): the blue of
/// the prism, the least alarming of the four for a permanent UI role.
pub const ACCENT: Color32 = BLUE;

/// Window background, behind every panel.
pub const BG_DEEP: Color32 = Color32::from_rgb(0x0F, 0x11, 0x16);
/// Panel background (the home screen's central panel).
pub const BG_PANEL: Color32 = Color32::from_rgb(0x16, 0x19, 0x20);
/// Card / inset background.
pub const BG_CARD: Color32 = Color32::from_rgb(0x1B, 0x1F, 0x27);
/// Resting fill of an interactive widget.
pub const BG_WIDGET: Color32 = Color32::from_rgb(0x22, 0x27, 0x31);
/// Hovered fill.
pub const BG_WIDGET_HOVER: Color32 = Color32::from_rgb(0x2C, 0x32, 0x3F);
/// Pressed / active fill.
pub const BG_WIDGET_ACTIVE: Color32 = Color32::from_rgb(0x38, 0x40, 0x50);
/// Hairline borders and separators.
pub const STROKE: Color32 = Color32::from_rgb(0x2E, 0x34, 0x40);
/// Body text.
pub const TEXT: Color32 = Color32::from_rgb(0xE6, 0xE8, 0xEC);
/// Secondary text (captions, hints, paths).
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9A, 0xA1, 0xAD);

/// Cycle through the four prism accents; `i` may be any index, so a list of
/// arbitrary length can be tinted without bounds checks at the call site.
pub fn accent(i: usize) -> Color32 {
    ACCENTS[i % ACCENTS.len()]
}

/// Named text roles of the shell. egui's built-in proportional face is used
/// as-is (no font asset): the identity comes from the sizes and the palette,
/// not from a custom typeface.
pub const SIZE_TITLE: f32 = 34.0;
pub const SIZE_HEADING: f32 = 19.0;
pub const SIZE_BODY: f32 = 15.0;
pub const SIZE_BUTTON: f32 = 15.0;
pub const SIZE_SMALL: f32 = 12.0;

/// Install the Prisme dark theme on `ctx`. Safe to call more than once.
pub fn apply(ctx: &egui::Context) {
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

    let radius = CornerRadius::same(6);
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
        style.text_styles.insert(TextStyle::Heading, FontId::proportional(SIZE_HEADING));
        style.text_styles.insert(TextStyle::Body, FontId::proportional(SIZE_BODY));
        style.text_styles.insert(TextStyle::Button, FontId::proportional(SIZE_BUTTON));
        style.text_styles.insert(TextStyle::Small, FontId::proportional(SIZE_SMALL));
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(14.0, 8.0);
        style.spacing.window_margin = egui::Margin::same(16);
        style.visuals.button_frame = true;
    });
}

/// `BG_DEEP` as the linear-space clear value wgpu expects for an sRGB surface
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
    [to_linear(BG_DEEP.r()), to_linear(BG_DEEP.g()), to_linear(BG_DEEP.b()), 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn clear_color_matches_the_deep_background_and_is_opaque() {
        let c = clear_color();
        assert_eq!(c[3], 1.0);
        for v in &c[..3] {
            assert!((0.0..=1.0).contains(v), "{v} out of range");
            assert!(*v < 0.02, "the clear colour must stay very dark, got {v}");
        }
        // Monotonic in the sRGB value it comes from: R(0x0F) < G(0x11) < B(0x16).
        assert!(c[0] < c[1] && c[1] < c[2]);
    }
}
