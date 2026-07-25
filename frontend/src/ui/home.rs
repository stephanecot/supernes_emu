//! `Accueil` — the landing screen of the application shell.
//!
//! This is the screen the application opens on when it is launched without a
//! ROM path: identity header, the entry points (open a cartridge, return to a
//! suspended session, quit), and the library itself — either the game grid
//! (`ui::library_view`) or, when a game is selected, its sheet
//! (`ui::game_sheet`). Everything it needs comes in through `HomeModel`, so
//! the screen owns no state of its own and can be re-rendered from scratch
//! every frame, as immediate mode expects.

use std::path::Path;

use egui::{Align, Color32, Layout, RichText, Sense, Stroke, Vec2};

use super::game_sheet::{self, SheetData, SheetModel};
use super::library_view::{self, LibraryModel};
use super::theme;
use super::Action;

/// Everything the home screen displays. Borrowed from the event loop for the
/// duration of one UI frame.
pub struct HomeModel<'a> {
    pub app_name: &'a str,
    pub version: &'a str,
    /// Cartridge title of the suspended session, `None` when nothing is loaded.
    pub game_title: Option<&'a str>,
    /// Path of that cartridge, shown under its title.
    pub rom_path: Option<&'a Path>,
    /// The library: entries, per-game state, view state and pictures.
    pub library: LibraryModel<'a>,
    /// Files listed by the open game sheet, gathered by the shell when the
    /// selection changed (never per frame — each field is a directory
    /// listing).
    pub sheet: &'a SheetData,
}

impl HomeModel<'_> {
    pub fn has_session(&self) -> bool {
        self.game_title.is_some()
    }
}

/// Side of the four squares of the prism mark, in points.
const MARK_SQUARE: f32 = 13.0;
/// Gap between two squares of the mark.
const MARK_GAP: f32 = 4.0;
/// Longest path rendered in full before `shorten_path` elides its middle.
const PATH_MAX_CHARS: usize = 64;

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
                ui.label(
                    RichText::new("Échap : revenir au jeu · O : ouvrir une ROM · , : réglages")
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
                .inner_margin(egui::Margin::symmetric(24, 20)),
        )
        .show(ctx, |ui| {
            let has_session = model.has_session();
            header(ui, model.app_name, model.version);
            ui.add_space(14.0);

            if let Some(title) = model.game_title {
                if session_card(ui, title, model.rom_path).clicked() {
                    action = Action::ResumeGame;
                }
                ui.add_space(10.0);
            }

            ui.label(
                RichText::new(library_hint(has_session))
                    .color(theme::TEXT_DIM)
                    .size(theme::SIZE_BODY),
            );
            ui.add_space(12.0);

            ui.horizontal(|ui| {
                if primary_button(ui, "Ouvrir une ROM…").clicked() {
                    action = Action::PickRom;
                }
                if has_session && ui.button("Reprendre la partie").clicked() {
                    action = Action::ResumeGame;
                }
                if ui.button("Réglages…").clicked() {
                    action = Action::OpenSettings;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Quitter").clicked() {
                        action = Action::Quit;
                    }
                });
            });

            ui.add_space(14.0);
            section_rule(ui, "Bibliothèque");
            ui.add_space(10.0);

            let library_action = library(ui, model);
            if library_action != Action::None {
                action = library_action;
            }
        });

    action
}

/// The lower half of the home screen: the game grid, or the sheet of the
/// selected game. A selection naming a game that is no longer in the library
/// (folder changed, file deleted) falls back to the grid.
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
                textures: model.library.textures,
                selected: &mut model.library.state.selected,
            };
            return game_sheet::show(ui, &mut sheet);
        }
        model.library.state.selected = None;
    }
    library_view::show(ui, &mut model.library)
}

/// Identity header: the four-square prism mark, the product name and what the
/// emulated machine is.
fn header(ui: &mut egui::Ui, _app_name: &str, version: &str) {
    ui.horizontal(|ui| {
        prism_mark(ui);
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.label(RichText::new("Prisme").size(theme::SIZE_TITLE).strong().color(theme::TEXT));
            ui.label(
                RichText::new("Émulateur Super Nintendo")
                    .size(theme::SIZE_BODY)
                    .color(theme::TEXT_DIM),
            );
        });
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            ui.label(
                RichText::new(format!("version {version}"))
                    .size(theme::SIZE_SMALL)
                    .color(theme::TEXT_DIM),
            );
        });
    });
}

/// The product mark: the four prism colours as a 2x2 block of squares, in the
/// canonical order of `theme::ACCENTS`.
fn prism_mark(ui: &mut egui::Ui) {
    let side = MARK_SQUARE * 2.0 + MARK_GAP;
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
    let painter = ui.painter();
    for i in 0..theme::ACCENTS.len() {
        let col = (i % 2) as f32;
        let row = (i / 2) as f32;
        let min = rect.min
            + Vec2::new(col * (MARK_SQUARE + MARK_GAP), row * (MARK_SQUARE + MARK_GAP));
        painter.rect_filled(
            egui::Rect::from_min_size(min, Vec2::splat(MARK_SQUARE)),
            2.0,
            theme::accent(i),
        );
    }
}

/// Section title followed by a hairline rule tinted with the primary accent.
fn section_rule(ui: &mut egui::Ui, title: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(theme::SIZE_HEADING).strong().color(theme::TEXT));
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), 1.0),
            Sense::hover(),
        );
        ui.painter().hline(
            rect.x_range(),
            rect.center().y,
            Stroke::new(1.0, theme::STROKE),
        );
    });
}

/// Card for the suspended session: clicking it goes back into the game.
fn session_card(ui: &mut egui::Ui, title: &str, path: Option<&Path>) -> egui::Response {
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(Stroke::new(1.0, theme::STROKE))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(4.0, 40.0), Sense::hover());
                ui.painter().rect_filled(rect, 2.0, theme::GREEN);
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("Partie en cours")
                            .size(theme::SIZE_SMALL)
                            .color(theme::GREEN),
                    );
                    ui.label(RichText::new(title).size(theme::SIZE_HEADING).color(theme::TEXT));
                    if let Some(path) = path {
                        ui.label(
                            RichText::new(shorten_path(path, PATH_MAX_CHARS))
                                .size(theme::SIZE_SMALL)
                                .color(theme::TEXT_DIM),
                        );
                    }
                });
            });
        })
        .response
        .interact(Sense::click())
}

/// Accent-filled call to action, distinct from the neutral secondary buttons.
/// Shared with the game sheet's `Jouer` button.
pub fn primary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).color(Color32::WHITE).size(theme::SIZE_BUTTON))
            .fill(theme::ACCENT)
            .stroke(Stroke::new(1.0, theme::ACCENT)),
    )
}

/// One-line explanation under the library heading, depending on whether a game
/// is already loaded.
pub fn library_hint(has_session: bool) -> &'static str {
    if has_session {
        "La partie est suspendue : la console garde son état, rien n'est perdu."
    } else {
        "Aucune cartouche chargée. Ouvrez un fichier .sfc, .smc ou .zip pour commencer."
    }
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

/// Shorten a label by cutting its tail and appending an ellipsis, so every
/// grid card keeps the same width. Counts characters, not bytes (an accented
/// title must never be cut mid-character).
pub fn elide(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars || max_chars == 0 {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_hint_reflects_whether_a_game_is_loaded() {
        assert!(library_hint(true).contains("suspendue"));
        assert!(library_hint(false).contains(".sfc"));
        assert_ne!(library_hint(true), library_hint(false));
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

    /// The home screen is the only pointer route to the settings panel when no
    /// cartridge is running (the native menu is macOS-only, the `,` hotkey is
    /// not discoverable), so its button must actually be painted.
    #[test]
    fn the_home_screen_offers_the_settings_panel() {
        let entries: Vec<crate::library::GameEntry> = Vec::new();
        let games = std::collections::BTreeMap::new();
        let thumbs = std::collections::HashMap::new();
        let pending = std::collections::HashSet::new();
        let mut state = super::super::library_view::LibraryUi::default();
        let mut textures = super::super::textures::TextureStore::new();
        let sheet = SheetData::default();
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut input = egui::RawInput::default();
        input.screen_rect =
            Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 896.0)));
        let mut produced = Action::Quit;
        let output = ctx.run(input, |ctx| {
            produced = show(
                ctx,
                &mut HomeModel {
                    app_name: "Prisme",
                    version: "0.0.0",
                    game_title: None,
                    rom_path: None,
                    library: LibraryModel {
                        entries: &entries,
                        games: &games,
                        dir: Path::new("roms"),
                        thumbs: &thumbs,
                        pending: &pending,
                        state: &mut state,
                        textures: &mut textures,
                    },
                    sheet: &sheet,
                },
            );
        });
        assert_eq!(produced, Action::None, "drawing alone must ask for nothing");
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
        assert!(text.contains("Réglages…"), "{text}");
        assert!(text.contains("Ouvrir une ROM…"), "{text}");
        assert!(text.contains("Quitter"), "{text}");
    }

    #[test]
    fn the_model_reports_a_session_exactly_when_a_title_is_set() {
        let entries: Vec<crate::library::GameEntry> = Vec::new();
        let games = std::collections::BTreeMap::new();
        let thumbs = std::collections::HashMap::new();
        let pending = std::collections::HashSet::new();
        let mut state = super::super::library_view::LibraryUi::default();
        let mut textures = super::super::textures::TextureStore::new();
        let sheet = SheetData::default();
        let mut m = HomeModel {
            app_name: "Prisme",
            version: "0.0.0",
            game_title: None,
            rom_path: None,
            library: LibraryModel {
                entries: &entries,
                games: &games,
                dir: Path::new("roms"),
                thumbs: &thumbs,
                pending: &pending,
                state: &mut state,
                textures: &mut textures,
            },
            sheet: &sheet,
        };
        assert!(!m.has_session());
        m.game_title = Some("SUPER MARIOWORLD");
        assert!(m.has_session());
    }
}
