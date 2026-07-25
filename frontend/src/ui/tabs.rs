//! The tab bar of the home screen: `Bibliothèque · Favoris · Récents ·
//! Réglages`, underlined by the spectral rule.
//!
//! The rule is drawn **once per frame**, under the active tab only: it is the
//! product's signature element and its job here is to say where the player is
//! (see `theme::spectral_rule`). The three first tabs choose what the library
//! shows; `Réglages` opens the settings panel, and while that panel owns the
//! screen the rule sits under it — so the underline always marks whatever is
//! being looked at.
//!
//! Every tab is a focusable widget, so the bar is reachable with the keyboard:
//! Tab walks into it, Space/Enter activates the focused tab (egui turns those
//! into a click on a focused clickable widget), and Left/Right move along the
//! bar and activate as they go, which is what a tab bar is expected to do.

use egui::{Key, Rect, Response, Sense, Vec2};

use crate::i18n::{Lang, Msg};

use super::icons::{self, Icon};
use super::theme;

/// The four entries of the bar, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    /// The whole library.
    #[default]
    Library,
    /// Pinned games only.
    Favorites,
    /// Games that have been launched at least once, most recent first.
    Recent,
    /// Not a view of the library: opens the settings panel.
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Library, Tab::Favorites, Tab::Recent, Tab::Settings];

    pub fn label(self, lang: Lang) -> &'static str {
        match self {
            Tab::Library => Msg::TabLibrary.text(lang),
            Tab::Favorites => Msg::TabFavorites.text(lang),
            Tab::Recent => Msg::TabRecent.text(lang),
            Tab::Settings => Msg::TabSettings.text(lang),
        }
    }

    /// Whether the tab shows a library view. `Réglages` does not: it is an
    /// entry point to the panel, and is never stored as the current view.
    pub fn is_view(self) -> bool {
        self != Tab::Settings
    }

    /// Icon drawn before the label. Only `Réglages` carries one, and that is
    /// the point: it is the entry of the bar that opens a panel instead of
    /// changing what the library shows, and the gear says so before the label
    /// is read.
    pub fn icon(self) -> Option<Icon> {
        match self {
            Tab::Settings => Some(Icon::Gear),
            _ => None,
        }
    }

    /// Value stored in `prefs.library_tab`.
    pub fn as_pref(self) -> &'static str {
        match self {
            Tab::Library => "library",
            Tab::Favorites => "favorites",
            Tab::Recent => "recent",
            // Never stored (see `is_view`); mapped for completeness so the
            // conversion stays total.
            Tab::Settings => "library",
        }
    }

    /// Unknown names read as `Library`, the same lenient rule as
    /// `library::SortMode::from_pref`.
    pub fn from_pref(name: &str) -> Self {
        match name {
            "favorites" => Tab::Favorites,
            "recent" => Tab::Recent,
            _ => Tab::Library,
        }
    }
}

/// Horizontal padding inside one tab.
const PAD_X: f32 = 14.0;
/// Height of a tab, rule excluded.
const TAB_H: f32 = 30.0;
/// Gap between a tab's icon and its label.
const ICON_GAP: f32 = 6.0;
/// Gap between the label and the rule under it.
const RULE_GAP: f32 = 5.0;
/// Length of the hover/focus transition, in seconds — short enough to feel
/// like a response to the pointer rather than an animation.
pub const TRANSITION: f32 = 0.12;

/// Draw the bar and return the tab the player asked for, if any. `active` is
/// the entry the spectral rule is drawn under, which the caller resolves (the
/// settings panel takes the underline while it is open).
pub fn show(ui: &mut egui::Ui, active: Tab, lang: Lang) -> Option<Tab> {
    let mut chosen = None;
    let mut focused = None;
    let mut responses: Vec<Response> = Vec::with_capacity(Tab::ALL.len());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (i, tab) in Tab::ALL.into_iter().enumerate() {
            let response = tab_button(ui, tab, tab == active, lang);
            if response.clicked() {
                chosen = Some(tab);
            }
            if response.has_focus() {
                focused = Some(i);
            }
            responses.push(response);
        }
    });

    // Left/Right walk the bar once it holds the keyboard focus.
    if let Some(i) = focused {
        let step = ui.input(|input| {
            i32::from(input.key_pressed(Key::ArrowRight))
                - i32::from(input.key_pressed(Key::ArrowLeft))
        });
        if step != 0 {
            let count = Tab::ALL.len() as i32;
            let next = ((i as i32 + step) % count + count) % count;
            responses[next as usize].request_focus();
            chosen = Some(Tab::ALL[next as usize]);
        }
    }
    chosen
}

/// Height the bar occupies, so a caller can reserve it before drawing.
pub fn height() -> f32 {
    TAB_H + RULE_GAP + theme::SPECTRAL_RULE_H
}

fn tab_button(ui: &mut egui::Ui, tab: Tab, active: bool, lang: Lang) -> Response {
    let font = if active { theme::strong(theme::SIZE_BODY) } else { theme::font(theme::SIZE_BODY) };
    let galley =
        ui.painter().layout_no_wrap(tab.label(lang).to_owned(), font, egui::Color32::PLACEHOLDER);
    let icon_w = tab.icon().map_or(0.0, |_| icons::SIZE + ICON_GAP);
    let size = Vec2::new(galley.size().x + icon_w + 2.0 * PAD_X, height());
    let (rect, response) = ui.allocate_at_least(size, Sense::CLICK | Sense::FOCUSABLE);
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let lit = ui.ctx().animate_bool_with_time(
        response.id.with("lit"),
        response.hovered() || response.has_focus(),
        TRANSITION,
    );
    let colour = if active {
        theme::TEXT
    } else {
        // Dim at rest, full text colour once the pointer or the focus reaches
        // it: the only feedback a tab needs.
        theme::TEXT_DIM.lerp_to_gamma(theme::TEXT, lit)
    };
    let text_left = rect.center().x - (galley.size().x + icon_w) / 2.0 + icon_w;
    if let Some(icon) = tab.icon() {
        let icon_rect = Rect::from_min_size(
            egui::pos2(text_left - icon_w, rect.top() + (TAB_H - icons::SIZE) / 2.0),
            Vec2::splat(icons::SIZE),
        );
        icon.draw(ui.painter(), icon_rect, colour);
    }
    let text_pos = egui::pos2(text_left, rect.top() + (TAB_H - galley.size().y) / 2.0);
    ui.painter().galley(text_pos, galley, colour);

    if active {
        let rule = Rect::from_min_size(
            egui::pos2(rect.left() + PAD_X / 2.0, rect.bottom() - theme::SPECTRAL_RULE_H),
            Vec2::new(rect.width() - PAD_X, theme::SPECTRAL_RULE_H),
        );
        theme::spectral_rule(ui.painter(), rule);
    } else if lit > 0.0 {
        // An inactive tab under the pointer shows a plain grey underline: the
        // spectral rule stays the mark of the active view alone.
        let rule = Rect::from_min_size(
            egui::pos2(rect.left() + PAD_X / 2.0, rect.bottom() - theme::SPECTRAL_RULE_H),
            Vec2::new((rect.width() - PAD_X) * lit, theme::SPECTRAL_RULE_H),
        );
        ui.painter().rect_filled(rule, 0.0, theme::STROKE);
    }

    if response.has_focus() {
        // Keyboard focus must be visible on its own, not only through the
        // colour change a pointer also produces.
        ui.painter().rect_stroke(
            Rect::from_min_max(rect.min, egui::pos2(rect.right(), rect.top() + TAB_H)).shrink(1.0),
            4.0,
            egui::Stroke::new(1.0, theme::ACCENT),
            egui::StrokeKind::Inside,
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_lists_the_four_entries_the_brief_names() {
        let labels: Vec<&str> = Tab::ALL.iter().map(|t| t.label(Lang::Fr)).collect();
        assert_eq!(labels, vec!["Bibliothèque", "Favoris", "Récents", "Réglages"]);
        let english: Vec<&str> = Tab::ALL.iter().map(|t| t.label(Lang::En)).collect();
        assert_eq!(english, vec!["Library", "Favourites", "Recent", "Settings"]);
        assert_eq!(Tab::default(), Tab::Library);
        // Only the first three are views of the library.
        assert_eq!(Tab::ALL.iter().filter(|t| t.is_view()).count(), 3);
        assert!(!Tab::Settings.is_view());
    }

    #[test]
    fn the_current_tab_round_trips_through_the_preference_string() {
        for tab in Tab::ALL.into_iter().filter(|t| t.is_view()) {
            assert_eq!(Tab::from_pref(tab.as_pref()), tab);
        }
        // An unknown or absent value falls back to the library, never to a
        // filtered view the player did not ask for.
        assert_eq!(Tab::from_pref(""), Tab::Library);
        assert_eq!(Tab::from_pref("zzz"), Tab::Library);
        // …and the string the bar writes must survive the preferences file,
        // or the shell would reopen on a tab nobody chose.
        for tab in Tab::ALL.into_iter().filter(|t| t.is_view()) {
            let json = format!("{{\"library_tab\": {:?}}}", tab.as_pref());
            let back = crate::prefs::Prefs::from_json(&json).expect("parse");
            assert_eq!(Tab::from_pref(&back.library_tab), tab);
        }
        assert_eq!(crate::prefs::Prefs::default().library_tab, Tab::Library.as_pref());
    }

    /// Run one headless UI frame over the bar and return the shapes it painted
    /// plus what it asked for.
    fn painted(active: Tab, events: Vec<egui::Event>) -> (Option<Tab>, Vec<egui::Shape>) {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 200.0))),
            events,
            ..Default::default()
        };
        let mut chosen = None;
        let output = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                chosen = show(ui, active, Lang::Fr);
            });
        });
        fn walk(shape: &egui::Shape, out: &mut Vec<egui::Shape>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                other => out.push(other.clone()),
            }
        }
        let mut shapes = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut shapes);
        }
        (chosen, shapes)
    }

    /// The signature element is spent once and only once: four coloured
    /// segments, under the active tab and under no other.
    #[test]
    fn the_spectral_rule_is_drawn_under_the_active_tab_only() {
        for active in Tab::ALL {
            let (chosen, shapes) = painted(active, Vec::new());
            assert_eq!(chosen, None, "drawing alone must not switch tab");
            let segments: Vec<egui::Color32> = shapes
                .iter()
                .filter_map(|s| match s {
                    egui::Shape::Rect(r) if r.rect.height() <= theme::SPECTRAL_RULE_H => {
                        Some(r.fill)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(segments.len(), theme::ACCENTS.len(), "{active:?}: {segments:?}");
            assert_eq!(segments, theme::ACCENTS.to_vec(), "{active:?}");
        }
    }

    #[test]
    fn every_tab_paints_its_own_label() {
        let (_, shapes) = painted(Tab::Library, Vec::new());
        let mut text = String::new();
        for shape in &shapes {
            if let egui::Shape::Text(t) = shape {
                text.push_str(t.galley.text());
                text.push('\n');
            }
        }
        for tab in Tab::ALL {
            assert!(
                text.contains(tab.label(Lang::Fr)),
                "{:?} is missing: {text}",
                tab.label(Lang::Fr)
            );
        }
    }
}
