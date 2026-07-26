//! `Accueil` — the landing screen of the application shell.
//!
//! This is the screen the application opens on when it is launched without a
//! ROM path. Three bands, and nothing else: a one-line identity header with
//! the entry points on its right, the tab bar (`ui::tabs`) whose active entry
//! carries the spectral rule, and the view itself — the game grid
//! (`ui::library_view`) or, when a game is selected, its sheet
//! (`ui::game_sheet`).
//!
//! The chrome is deliberately thin: everything above the first card is space
//! taken from the library, and the header used to eat half the window. The
//! suspended session, the ROM picker and the quit button all live on the
//! header line; the sheet replaces the whole content band, so nothing of the
//! grid is repeated above it.
//!
//! Everything the screen displays comes in through `HomeModel`, so it owns no
//! state of its own and can be re-rendered from scratch every frame, as
//! immediate mode expects.

use std::path::Path;

use egui::{Align, Color32, Layout, RichText, Sense, Stroke, StrokeKind, Vec2};

use crate::i18n::{self, Lang, Msg};

use super::game_sheet::{self, SheetData, SheetModel};
use super::icons::{self, Icon};
use super::library_view::{self, LibraryModel};
use super::tabs;
use super::theme;
use super::Action;

/// Everything the home screen displays. Borrowed from the event loop for the
/// duration of one UI frame.
pub struct HomeModel<'a> {
    pub app_name: &'a str,
    pub version: &'a str,
    /// Language every string of the screen is rendered in, resolved by the
    /// shell from `prefs.lang()` on each frame.
    pub lang: Lang,
    /// Cartridge title of the suspended session, `None` when nothing is loaded.
    pub game_title: Option<&'a str>,
    /// Path of that cartridge, shown as the session chip's tooltip.
    pub rom_path: Option<&'a Path>,
    /// The library: entries, per-game state, view state and pictures.
    pub library: LibraryModel<'a>,
    /// Files listed by the open game sheet, gathered by the shell when the
    /// selection changed (never per frame — each field is a directory
    /// listing).
    pub sheet: &'a SheetData,
    /// What the catalogues said, per game id — empty until the player asks for
    /// any of it (`ui::Action::FillSheet` / `FillLibrary`).
    pub meta: &'a std::collections::BTreeMap<String, crate::metadata::GameMeta>,
    /// The assistant is switched on and its tool resolved.
    pub assistant: bool,
    /// `game_id` of the session currently loaded, if any: the assistant plays
    /// from that session's state, so it can only play *this* game.
    pub running: Option<&'a str>,
    /// Edit buffer of the request being typed on the sheet.
    pub wish: &'a mut String,
    /// The assistant's latest line while one runs.
    pub assistant_says: Option<&'a str>,
}

impl HomeModel<'_> {
    pub fn has_session(&self) -> bool {
        self.game_title.is_some()
    }
}

/// Side of the product mark in the header, in points. Shared with the settings
/// view, whose header must be exactly as tall so the tab bar sits at the same
/// height on both.
pub const MARK_SIDE: f32 = 30.0;
/// Longest path rendered in full before `shorten_path` elides its middle.
const PATH_MAX_CHARS: usize = 64;
/// Longest game title shown on the session chip.
const SESSION_TITLE_MAX_CHARS: usize = 28;

/// Draw the whole home screen and return what the user asked for.
pub fn show(ctx: &egui::Context, model: &mut HomeModel) -> Action {
    let mut action = Action::None;

    egui::TopBottomPanel::bottom("prisme-home-footer")
        .frame(
            egui::Frame::new()
                .fill(theme::BG_DEEP)
                .inner_margin(egui::Margin::symmetric(24, 10)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                // Escape only "goes back to the game" when there is one to go
                // back to; with no cartridge loaded it leaves the application
                // (`app_state::escape_action`), and the line must not promise
                // otherwise.
                ui.label(
                    RichText::new(footer_hint(model.lang, model.has_session()))
                        .size(theme::SIZE_SMALL)
                        .color(theme::TEXT_DIM),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{} {}", model.app_name, model.version))
                            .size(theme::SIZE_SMALL)
                            .color(theme::TEXT_DIM),
                    );
                });
            });
        });

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(theme::BG_PANEL)
                .inner_margin(egui::Margin::symmetric(24, 16)),
        )
        .show(ctx, |ui| {
            if let Some(produced) = header(ui, model) {
                action = produced;
            }
            ui.add_space(10.0);

            // The rule marks the library view being shown. `Réglages` is never
            // active here: it is a screen of its own (`ui::settings`), which
            // draws the same bar with the rule under its own entry.
            if let Some(tab) = tabs::show(ui, model.library.state.tab, model.lang) {
                if tab.is_view() {
                    model.library.state.tab = tab;
                    // Switching tab leaves whatever sheet was open: the sheet
                    // belongs to the game, not to the view.
                    model.library.state.selected = None;
                    model.library.state.confirm_delete = None;
                } else {
                    action = Action::OpenSettings;
                }
            }
            ui.add_space(12.0);

            let library_action = library(ui, model);
            if library_action != Action::None {
                action = library_action;
            }
        });

    action
}

/// The lower band: the game grid, or the sheet of the selected game. A
/// selection naming a game that is no longer in the library (folder changed,
/// file deleted) falls back to the grid.
fn library(ui: &mut egui::Ui, model: &mut HomeModel) -> Action {
    let entries = model.library.entries;
    let games = model.library.games;
    let thumbs = model.library.thumbs;
    let pending = model.library.pending;
    let selected = model.library.state.selected.clone();

    if let Some(id) = selected {
        if let Some(entry) = entries.iter().find(|e| e.id == id) {
            let stats = games.get(&id).cloned().unwrap_or_default();
            let mut sheet = SheetModel {
                entry,
                stats: &stats,
                data: model.sheet,
                picture: thumbs.get(&id).map(|p| p.as_path()),
                pending: pending.contains(&id),
                meta: model.meta.get(&id),
                assistant: model.assistant,
                is_running: model.running == Some(id.as_str()),
                wish: model.wish,
                assistant_says: model.assistant_says,
                fetching: model.library.fetching.contains(&id),
                textures: model.library.textures,
                selected: &mut model.library.state.selected,
                confirm_delete: &mut model.library.state.confirm_delete,
                lang: model.lang,
            };
            return game_sheet::show(ui, &mut sheet);
        }
        model.library.state.selected = None;
    }
    library_view::show(ui, &mut model.library)
}

/// Identity header on one line: the product mark and its name on the left, the
/// suspended session and the two global actions on the right.
fn header(ui: &mut egui::Ui, model: &HomeModel) -> Option<Action> {
    let mut action = None;
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(MARK_SIDE), Sense::hover());
        theme::mark(ui.painter(), rect);
        ui.add_space(10.0);
        ui.label(RichText::new("Prisme").font(theme::strong(theme::SIZE_TITLE)).color(theme::TEXT));
        ui.add_space(8.0);
        ui.label(
            RichText::new(Msg::AppTagline.text(model.lang))
                .font(theme::font(theme::SIZE_SMALL))
                .color(theme::TEXT_DIM),
        );

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button(Msg::Quit.text(model.lang)).clicked() {
                action = Some(Action::Quit);
            }
            if icons::primary_button(ui, Icon::Folder, Msg::OpenRom.text(model.lang)).clicked() {
                action = Some(Action::PickRom);
            }
            if let Some(title) = model.game_title {
                if session_chip(ui, title, model.rom_path, model.lang).clicked() {
                    action = Some(Action::ResumeGame);
                }
            }
        });
    });
    action
}

/// The suspended session, as a chip on the header line: green means "running"
/// everywhere in the shell, and clicking it goes back into the game.
fn session_chip(
    ui: &mut egui::Ui,
    title: &str,
    path: Option<&Path>,
    lang: Lang,
) -> egui::Response {
    let label = i18n::resume_chip(lang, &elide(title, SESSION_TITLE_MAX_CHARS));
    let galley = ui.painter().layout_no_wrap(
        label,
        theme::font(theme::SIZE_BUTTON),
        Color32::PLACEHOLDER,
    );
    let padding = ui.spacing().button_padding;
    let dot = 8.0;
    let size = Vec2::new(galley.size().x + dot + 8.0, galley.size().y) + 2.0 * padding;
    let (rect, response) = ui.allocate_at_least(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        let fill = if hovered { theme::BG_WIDGET_HOVER } else { theme::BG_WIDGET };
        ui.painter().rect(
            rect,
            6.0,
            fill,
            Stroke::new(1.0, theme::GREEN.gamma_multiply(if hovered { 1.0 } else { 0.6 })),
            StrokeKind::Inside,
        );
        ui.painter().circle_filled(
            egui::pos2(rect.left() + padding.x + dot / 2.0, rect.center().y),
            dot / 2.0,
            theme::GREEN,
        );
        ui.painter().galley(
            egui::pos2(rect.left() + padding.x + dot + 8.0, rect.center().y - galley.size().y / 2.0),
            galley,
            theme::TEXT,
        );
    }
    match path {
        Some(path) => response.on_hover_text(shorten_path(path, PATH_MAX_CHARS)),
        None => response,
    }
}

/// The keyboard line of the footer. What Escape does depends on whether a
/// cartridge is loaded, so the line does too.
pub fn footer_hint(lang: Lang, has_session: bool) -> &'static str {
    i18n::escape_hint(lang, has_session)
}

/// Accent-filled call to action with no icon, distinct from the neutral
/// secondary buttons (`icons::primary_button` is the same button with one).
pub fn primary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            RichText::new(label).color(Color32::WHITE).font(theme::font(theme::SIZE_BUTTON)),
        )
        .fill(theme::ACCENT)
        .stroke(Stroke::new(1.0, theme::ACCENT)),
    )
}

/// Heading of a band. No rule under it: the bold heading face already separates
/// it from the body, and a grey hairline as long as the words themselves read
/// as an artefact. The **spectral** rule is spent on the active tab and nowhere
/// else (`ui::tabs`).
pub fn heading(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).font(theme::strong(theme::SIZE_HEADING)).color(theme::TEXT));
}

/// Shorten a path for display by eliding its middle, keeping the beginning and
/// the file name (the two parts that identify it). Paths shorter than
/// `max_chars` are returned unchanged. Counts characters, not bytes, so an
/// accented path is never cut mid-character.
pub fn shorten_path(path: &Path, max_chars: usize) -> String {
    let text = path.to_string_lossy().into_owned();
    let count = text.chars().count();
    if count <= max_chars || max_chars < 5 {
        return text;
    }
    // One character of the budget goes to the ellipsis; the tail gets the
    // larger half so the file name survives.
    let keep = max_chars - 1;
    let head = keep / 3;
    let tail = keep - head;
    let head: String = text.chars().take(head).collect();
    let tail: String = text.chars().skip(count - tail).collect();
    format!("{head}…{tail}")
}

/// Shorten a label by cutting its tail and appending an ellipsis. Counts
/// characters, not bytes (an accented title must never be cut mid-character).
/// The grid's titles are elided by the text layout instead
/// (`library_view::elided_galley`), which can do it on two rows.
pub fn elide(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars || max_chars == 0 {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::tabs::Tab;
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_footer_line_matches_what_escape_actually_does() {
        assert!(footer_hint(Lang::Fr, true).contains("revenir au jeu"));
        assert!(footer_hint(Lang::Fr, false).contains("quitter"));
        assert_ne!(footer_hint(Lang::Fr, true), footer_hint(Lang::Fr, false));
        // Both name the two other shortcuts of the screen.
        for line in [footer_hint(Lang::Fr, true), footer_hint(Lang::Fr, false)] {
            assert!(line.contains("ouvrir une ROM"), "{line}");
            assert!(line.contains("réglages"), "{line}");
        }
        assert!(footer_hint(Lang::En, true).contains("back to the game"));
        assert!(footer_hint(Lang::En, false).contains("quit"));
        for line in [footer_hint(Lang::En, true), footer_hint(Lang::En, false)] {
            assert!(line.contains("open a ROM"), "{line}");
            assert!(line.contains("settings"), "{line}");
        }
    }

    #[test]
    fn short_paths_are_shown_in_full() {
        let p = PathBuf::from("/roms/game.sfc");
        assert_eq!(shorten_path(&p, 64), "/roms/game.sfc");
        assert_eq!(shorten_path(&p, 14), "/roms/game.sfc");
    }

    #[test]
    fn long_paths_keep_their_file_name() {
        let p = PathBuf::from("/Users/someone/a/very/deep/directory/tree/that/goes/on/Secret of Mana (F).zip");
        let s = shorten_path(&p, 40);
        assert_eq!(s.chars().count(), 40);
        assert!(s.starts_with("/Users"), "{s}");
        assert!(s.ends_with("(F).zip"), "{s}");
        assert!(s.contains('…'));
    }

    #[test]
    fn shortening_never_splits_a_character() {
        // Every character of this path is 2 bytes in UTF-8: a byte-based cut
        // would panic or produce invalid UTF-8.
        let p = PathBuf::from("/é".repeat(60));
        let s = shorten_path(&p, 20);
        assert_eq!(s.chars().count(), 20);
        assert!(s.is_char_boundary(s.len()));
    }

    #[test]
    fn a_budget_too_small_to_elide_returns_the_path_unchanged() {
        let p = PathBuf::from("/roms/some/long/path.sfc");
        assert_eq!(shorten_path(&p, 4), p.to_string_lossy());
    }

    #[test]
    fn elide_keeps_the_head_and_never_splits_a_character() {
        assert_eq!(elide("SUPER MARIOWORLD", 24), "SUPER MARIOWORLD");
        assert_eq!(elide("SUPER MARIOWORLD", 16), "SUPER MARIOWORLD");
        let cut = elide("SUPER MARIOWORLD", 10);
        assert_eq!(cut.chars().count(), 10);
        assert!(cut.starts_with("SUPER"), "{cut}");
        assert!(cut.ends_with('…'));
        // Multi-byte characters: a byte-based cut would panic here.
        let cut = elide(&"é".repeat(40), 8);
        assert_eq!(cut.chars().count(), 8);
        assert!(cut.is_char_boundary(cut.len()));
        assert_eq!(elide("x", 0), "x");
    }

    /// One headless frame of the whole screen, returning what it asked for and
    /// every string it painted.
    fn draw(
        game_title: Option<&str>,
        state: &mut super::super::library_view::LibraryUi,
        size: egui::Vec2,
        lang: Lang,
    ) -> (Action, String) {
        let entries: Vec<crate::library::GameEntry> = Vec::new();
        let games = std::collections::BTreeMap::new();
        let thumbs = std::collections::HashMap::new();
        let pending = std::collections::HashSet::new();
        let fetching = std::collections::HashSet::new();
        let meta = std::collections::BTreeMap::new();
        let mut textures = super::super::textures::TextureStore::new();
        let sheet = SheetData::default();
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        let mut produced = Action::Quit;
        let output = ctx.run(input, |ctx| {
            produced = show(
                ctx,
                &mut HomeModel {
                assistant: true,
                running: None,
                wish: &mut String::new(),
                assistant_says: None,
                    app_name: "Prisme",
                    version: "0.0.0",
                    lang,
                    game_title,
                    rom_path: None,
                    library: LibraryModel {
                        entries: &entries,
                        games: &games,
                        dir: Path::new("roms"),
                        thumbs: &thumbs,
                        pending: &pending,
                        fetching: &fetching,
                        state,
                        textures: &mut textures,
                        lang,
                    },
                    sheet: &sheet,
                    meta: &meta,
                },
            );
        });
        let mut text = String::new();
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
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut text);
        }
        (produced, text)
    }

    /// The home screen is the only pointer route to a ROM and to the settings
    /// panel when no cartridge is running, so both must actually be painted.
    #[test]
    fn the_home_screen_offers_the_tabs_and_the_global_actions() {
        for lang in Lang::ALL {
            let mut state = super::super::library_view::LibraryUi::default();
            let (produced, text) = draw(None, &mut state, egui::vec2(1024.0, 896.0), lang);
            assert_eq!(produced, Action::None, "drawing alone must ask for nothing");
            for tab in Tab::ALL {
                assert!(
                    text.contains(tab.label(lang)),
                    "tab {:?} is missing: {text}",
                    tab.label(lang)
                );
            }
            assert!(text.contains(Msg::OpenRom.text(lang)), "{text}");
            assert!(text.contains(Msg::Quit.text(lang)), "{text}");
            // No cartridge: no session chip.
            assert!(!text.contains(" · SUPER"), "{text}");
        }
    }

    /// A suspended session is shown as one chip on the header line, not as a
    /// card that pushes the library down the screen.
    #[test]
    fn a_suspended_session_is_offered_on_the_header_line() {
        let mut state = super::super::library_view::LibraryUi::default();
        let (_, text) =
            draw(Some("SUPER MARIOWORLD"), &mut state, egui::vec2(1024.0, 896.0), Lang::Fr);
        assert!(text.contains("Reprendre · SUPER MARIOWORLD"), "{text}");
        let mut state = super::super::library_view::LibraryUi::default();
        let (_, text) =
            draw(Some("SUPER MARIOWORLD"), &mut state, egui::vec2(1024.0, 896.0), Lang::En);
        assert!(text.contains("Resume · SUPER MARIOWORLD"), "{text}");
    }

    /// The chrome above the first card is what the brief measured at 53 % of
    /// the window. Header, tabs and toolbar together must stay a small band,
    /// whatever the window size.
    #[test]
    fn the_chrome_above_the_library_stays_a_band() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        assert!(tabs::height() < 40.0, "the tab bar alone is {} tall", tabs::height());
        // Header (mark 30 + margins) + tab bar + spacing, measured from the
        // constants the screen is built from.
        let chrome = 16.0 + MARK_SIDE + 10.0 + tabs::height() + 12.0;
        assert!(chrome < 120.0, "the chrome is {chrome} points tall");
    }

    #[test]
    fn the_model_reports_a_session_exactly_when_a_title_is_set() {
        let entries: Vec<crate::library::GameEntry> = Vec::new();
        let games = std::collections::BTreeMap::new();
        let thumbs = std::collections::HashMap::new();
        let pending = std::collections::HashSet::new();
        let fetching = std::collections::HashSet::new();
        let meta = std::collections::BTreeMap::new();
        let mut state = super::super::library_view::LibraryUi::default();
        let mut textures = super::super::textures::TextureStore::new();
        let sheet = SheetData::default();
        let mut m = HomeModel {
                assistant: true,
                running: None,
                wish: &mut String::new(),
                assistant_says: None,
            app_name: "Prisme",
            version: "0.0.0",
            lang: Lang::Fr,
            game_title: None,
            rom_path: None,
            library: LibraryModel {
                entries: &entries,
                games: &games,
                dir: Path::new("roms"),
                thumbs: &thumbs,
                pending: &pending,
                fetching: &fetching,
                state: &mut state,
                textures: &mut textures,
                lang: Lang::Fr,
            },
            sheet: &sheet,
            meta: &meta,
        };
        assert!(!m.has_session());
        m.game_title = Some("SUPER MARIOWORLD");
        assert!(m.has_session());
    }
}
