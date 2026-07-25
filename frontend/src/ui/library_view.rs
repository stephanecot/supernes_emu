//! Library grid: search, sort, favourites and the game cards themselves.
//!
//! The screen owns no data: the entries come from `library` (scanned on the
//! background thread), the per-game state from `prefs.games`, and the pictures
//! from `ui::textures`. Only the *view* state — what is typed in the search
//! box, which sort is active, which game's sheet is open — lives here, in
//! `LibraryUi`, because it must survive the immediate-mode rebuild of every
//! frame.
//!
//! A card whose thumbnail has not been generated yet shows a placeholder; the
//! background worker fills them in one game at a time and the grid picks them
//! up on the next frame (`ui::textures::TextureStore::forget`).
//!
//! **The grid is virtualized** (`ScrollArea::show_rows`): only the rows inside
//! the viewport are built, so `TextureStore::get` is called for the cards on
//! screen and no others. An `egui::ScrollArea::show` closure runs for *every*
//! child instead, which on a library larger than `textures::MAX_TEXTURES` would
//! walk the whole cache in eviction order — a 0 % hit rate, i.e. every picture
//! decoded from its PNG and re-uploaded on every frame. `show_rows` needs a
//! constant row height, which `CARD_H` fixes and a unit test pins against the
//! card actually laid out.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use egui::{Align, Layout, RichText, Sense, Stroke, Vec2};

use crate::library::{self, GameEntry, SortMode};
use crate::prefs::GameStats;

use super::textures::TextureStore;
use super::theme;
use super::Action;

/// Card width in points; the picture keeps the SNES 256x224 ratio.
const CARD_W: f32 = 184.0;
const THUMB_W: f32 = 168.0;
const THUMB_H: f32 = THUMB_W * 224.0 / 256.0;
/// Frame margin around a card's content, on every side.
const CARD_MARGIN: f32 = 8.0;
/// Border of the card frame. egui counts a frame's total size as
/// `content + inner_margin + 2 * stroke.width` (see `egui::Frame`'s own
/// diagram), so the border belongs in the card's outer size.
const CARD_STROKE: f32 = 1.0;
/// Width one card occupies in the grid, margins and border included.
const CARD_TOTAL_W: f32 = CARD_W + 2.0 * (CARD_MARGIN + CARD_STROKE);
/// Content height every card is padded to, so all rows are the same height —
/// the constant `ScrollArea::show_rows` needs to place only the visible ones.
/// Must be at least the natural height of the tallest card, which
/// `a_card_is_exactly_one_grid_row_tall` verifies against a real layout pass.
const CARD_INNER_H: f32 = 214.0;
/// Height of one grid row: a card's content plus its frame margins and border.
const CARD_H: f32 = CARD_INNER_H + 2.0 * (CARD_MARGIN + CARD_STROKE);

/// Longest displayed title before it is elided; keeps every card the same
/// width whatever the title length.
const TITLE_MAX_CHARS: usize = 24;

/// View state of the library screen (see module docs).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LibraryUi {
    /// Content of the search box.
    pub query: String,
    pub sort: SortMode,
    /// `game_id` of the open game sheet; `None` shows the grid.
    pub selected: Option<String>,
    /// A scan is in flight on the library thread.
    pub scanning: bool,
    /// Last scan error (unreadable folder), shown in place of the grid.
    pub error: Option<String>,
}

/// Everything the grid draws, borrowed for one UI frame.
pub struct LibraryModel<'a> {
    pub entries: &'a [GameEntry],
    pub games: &'a BTreeMap<String, GameStats>,
    /// Folder currently scanned, shown next to the game count.
    pub dir: &'a Path,
    /// Resolved picture per game id: the promoted screenshot when the player
    /// chose one, else the generated thumbnail. Absent = placeholder.
    pub thumbs: &'a HashMap<String, PathBuf>,
    /// Games whose thumbnail generation has been queued and not answered yet.
    pub pending: &'a HashSet<String>,
    pub state: &'a mut LibraryUi,
    pub textures: &'a mut TextureStore,
}

/// Draw the toolbar and the grid. Returns what the player asked for; changing
/// the search text, the sort order or opening a sheet is handled in place
/// (view state), so those produce no `Action`.
pub fn show(ui: &mut egui::Ui, model: &mut LibraryModel) -> Action {
    let mut action = Action::None;

    ui.horizontal(|ui| {
        ui.label(RichText::new("Rechercher").size(theme::SIZE_SMALL).color(theme::TEXT_DIM));
        ui.add(
            egui::TextEdit::singleline(&mut model.state.query)
                .desired_width(200.0)
                .hint_text("titre ou nom de fichier"),
        );
        if !model.state.query.is_empty() && ui.button("×").on_hover_text("Effacer").clicked() {
            model.state.query.clear();
        }
        ui.add_space(12.0);
        ui.label(RichText::new("Trier par").size(theme::SIZE_SMALL).color(theme::TEXT_DIM));
        for mode in SortMode::ALL {
            let selected = model.state.sort == mode;
            if ui.selectable_label(selected, mode.label()).clicked() {
                model.state.sort = mode;
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Dossier…").on_hover_text("Choisir le dossier de jeux").clicked() {
                action = Action::ChooseLibraryDir;
            }
            if ui.button("Actualiser").clicked() {
                action = Action::Rescan;
            }
        });
    });

    ui.add_space(6.0);
    let shown = library::arrange(model.entries, &model.state.query, model.state.sort, model.games);
    ui.horizontal(|ui| {
        let count = if model.state.scanning {
            "Analyse du dossier…".to_string()
        } else {
            format!("{} jeu(x) · {}", shown.len(), super::home::shorten_path(model.dir, 56))
        };
        ui.label(RichText::new(count).size(theme::SIZE_SMALL).color(theme::TEXT_DIM));
        if !model.pending.is_empty() {
            ui.label(
                RichText::new(format!("· {} miniature(s) en cours", model.pending.len()))
                    .size(theme::SIZE_SMALL)
                    .color(theme::YELLOW),
            );
        }
    });
    ui.add_space(8.0);

    if let Some(error) = &model.state.error {
        ui.label(RichText::new(error).size(theme::SIZE_BODY).color(theme::RED));
        ui.add_space(8.0);
    }

    if shown.is_empty() && !model.state.scanning {
        ui.label(
            RichText::new(empty_hint(model.entries.is_empty()))
                .size(theme::SIZE_BODY)
                .color(theme::TEXT_DIM),
        );
        return action;
    }

    // Fixed-size rows, laid out by hand rather than by `horizontal_wrapped`:
    // the row count is what `show_rows` needs to skip everything off screen.
    let columns = columns_for(
        ui.available_width(),
        ui.spacing().item_spacing.x,
        ui.spacing().scroll.allocated_width(),
    );
    let rows = shown.len().div_ceil(columns);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(
        ui,
        CARD_H,
        rows,
        |ui, range| {
            for row in range {
                let first = row * columns;
                let last = (first + columns).min(shown.len());
                if first >= last {
                    continue;
                }
                ui.horizontal(|ui| {
                    for entry in &shown[first..last] {
                        let stats = model.games.get(&entry.id);
                        let picture = model.thumbs.get(&entry.id).cloned();
                        let pending = model.pending.contains(&entry.id);
                        let hit =
                            card(ui, entry, stats, picture.as_deref(), pending, model.textures);
                        if hit.favorite {
                            action = Action::ToggleFavorite(entry.id.clone());
                        } else if hit.open {
                            model.state.selected = Some(entry.id.clone());
                        }
                    }
                });
            }
        },
    );

    action
}

/// How many cards fit on one grid row, at least one. `bar` is the width the
/// vertical scroll bar reserves, which the cards must not be laid out under (a
/// row wider than the viewport would wrap and break the fixed row height).
pub fn columns_for(available: f32, spacing: f32, bar: f32) -> usize {
    let usable = available - bar;
    if !usable.is_finite() || usable < CARD_TOTAL_W {
        return 1;
    }
    (((usable + spacing) / (CARD_TOTAL_W + spacing)).floor() as usize).max(1)
}

/// What a card was asked to do this frame. A card click opens the game sheet;
/// launching is the sheet's `Jouer` button, so that a game is never started by
/// a stray click on the grid.
#[derive(Default)]
struct CardHit {
    open: bool,
    favorite: bool,
    /// The pointer is over the favourite star: the card-wide click must then
    /// be ignored, or clicking the star would also open the sheet (the card
    /// frame is interacted with *after* its children, so it can otherwise win
    /// the click).
    star_hover: bool,
}

fn card(
    ui: &mut egui::Ui,
    entry: &GameEntry,
    stats: Option<&GameStats>,
    picture: Option<&Path>,
    pending: bool,
    textures: &mut TextureStore,
) -> CardHit {
    let mut hit = CardHit::default();
    let favorite = stats.is_some_and(|s| s.favorite);
    let inner = egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(Stroke::new(CARD_STROKE, if favorite { theme::YELLOW } else { theme::STROKE }))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(CARD_MARGIN as i8))
        .show(ui, |ui| {
            // Every card is the same size, whatever its title or subtitle: the
            // grid's virtualization places rows at a constant `CARD_H`.
            ui.set_width(CARD_W);
            ui.set_min_height(CARD_INNER_H);
            thumbnail(ui, picture, pending, textures, Vec2::new(THUMB_W, THUMB_H));
            ui.add_space(6.0);
            ui.label(
                RichText::new(super::home::elide(&entry.display_title(), TITLE_MAX_CHARS))
                    .size(theme::SIZE_BODY)
                    .color(theme::TEXT),
            );
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(subtitle(entry, stats))
                        .size(theme::SIZE_SMALL)
                        .color(theme::TEXT_DIM),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let star = if favorite { "★" } else { "☆" };
                    let response = ui
                        .add(egui::Button::new(RichText::new(star).color(theme::YELLOW)).frame(false))
                        .on_hover_text(if favorite {
                            "Retirer des favoris"
                        } else {
                            "Épingler en favori"
                        });
                    hit.star_hover = response.hovered();
                    if response.clicked() {
                        hit.favorite = true;
                    }
                });
            });
        });
    if inner.response.interact(Sense::click()).clicked() && !hit.star_hover {
        hit.open = true;
    }
    hit
}

/// One line under the title: region, coprocessor and play time when there is
/// one — what tells two dumps of the same game apart at a glance.
fn subtitle(entry: &GameEntry, stats: Option<&GameStats>) -> String {
    let mut parts = vec![entry.region.clone()];
    if let Some(chip) = &entry.coprocessor {
        parts.push(chip.clone());
    }
    if let Some(played) = stats.map(|s| s.play_seconds).filter(|s| *s > 0) {
        parts.push(library::format_play_time(played));
    }
    parts.join(" · ")
}

/// Draw a game picture at `size`, or a placeholder when there is none yet.
pub fn thumbnail(
    ui: &mut egui::Ui,
    picture: Option<&Path>,
    pending: bool,
    textures: &mut TextureStore,
    size: Vec2,
) {
    if let Some(path) = picture {
        if let Some(handle) = textures.get(ui.ctx(), path) {
            let source = egui::load::SizedTexture::new(handle.id(), size);
            ui.add(egui::Image::new(source).corner_radius(egui::CornerRadius::same(4)));
            return;
        }
    }
    placeholder(ui, size, pending);
}

/// Placeholder picture: the four prism squares on a dark plate, plus a word on
/// what is happening. Deliberately not an empty rectangle — a game whose
/// thumbnail is still being generated must look like it, not like a failure.
fn placeholder(ui: &mut egui::Ui, size: Vec2, pending: bool) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, theme::BG_DEEP);
    let side = (size.y * 0.12).clamp(6.0, 14.0);
    let gap = side * 0.35;
    let block = Vec2::splat(side * 2.0 + gap);
    let origin = rect.center() - block / 2.0 - Vec2::new(0.0, side * 0.8);
    for i in 0..theme::ACCENTS.len() {
        let col = (i % 2) as f32;
        let row = (i / 2) as f32;
        let min = origin + Vec2::new(col * (side + gap), row * (side + gap));
        painter.rect_filled(
            egui::Rect::from_min_size(min, Vec2::splat(side)),
            2.0,
            theme::accent(i),
        );
    }
    painter.text(
        egui::pos2(rect.center().x, rect.max.y - side),
        egui::Align2::CENTER_BOTTOM,
        if pending { "miniature en cours…" } else { "pas de miniature" },
        egui::FontId::proportional(theme::SIZE_SMALL),
        theme::TEXT_DIM,
    );
    // Keeps the placeholder visually part of the card, like the picture it
    // stands in for.
    ui.painter().rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0, theme::STROKE),
        egui::StrokeKind::Inside,
    );
}

/// What to say when the grid has nothing to show: an empty folder and a search
/// that matched nothing are two different problems.
pub fn empty_hint(library_empty: bool) -> &'static str {
    if library_empty {
        "Aucun jeu dans ce dossier. Choisissez-en un autre avec « Dossier… », \
         ou ouvrez directement une ROM."
    } else {
        "Aucun jeu ne correspond à cette recherche."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_empty_hint_tells_an_empty_folder_from_a_failed_search() {
        assert!(empty_hint(true).contains("Dossier"));
        assert!(empty_hint(false).contains("recherche"));
        assert_ne!(empty_hint(true), empty_hint(false));
    }

    fn entry() -> GameEntry {
        GameEntry {
            id: "GAME-0001".to_string(),
            path: PathBuf::from("/roms/game.sfc"),
            file_size: 1024,
            modified: 0,
            title: "GAME".to_string(),
            mapping: "LoROM".to_string(),
            region: "PAL".to_string(),
            rom_bytes: 1024,
            sram_bytes: 0,
            coprocessor: None,
            fastrom: false,
            checksum: 1,
            checksum_valid: true,
        }
    }

    #[test]
    fn the_card_subtitle_lists_region_chip_and_play_time() {
        let mut e = entry();
        assert_eq!(subtitle(&e, None), "PAL");
        e.coprocessor = Some("SuperFX".to_string());
        assert_eq!(subtitle(&e, None), "PAL · SuperFX");
        let stats = GameStats { play_seconds: 3600, ..Default::default() };
        assert_eq!(subtitle(&e, Some(&stats)), "PAL · SuperFX · 1 h 00");
        // A never-played game shows no time at all rather than "0".
        let stats = GameStats::default();
        assert_eq!(subtitle(&e, Some(&stats)), "PAL · SuperFX");
    }

    #[test]
    fn the_default_view_state_is_an_unfiltered_title_sorted_grid() {
        let state = LibraryUi::default();
        assert!(state.query.is_empty());
        assert_eq!(state.sort, SortMode::Title);
        assert_eq!(state.selected, None);
        assert!(!state.scanning);
        assert_eq!(state.error, None);
    }

    #[test]
    fn the_grid_fits_as_many_whole_cards_per_row_as_the_width_allows() {
        let spacing = 10.0;
        // Never zero: a window narrower than one card still shows that card.
        assert_eq!(columns_for(0.0, spacing, 0.0), 1);
        assert_eq!(columns_for(CARD_TOTAL_W - 1.0, spacing, 0.0), 1);
        assert_eq!(columns_for(CARD_TOTAL_W, spacing, 0.0), 1);
        // Two cards need one gap between them, not two.
        assert_eq!(columns_for(2.0 * CARD_TOTAL_W + spacing, spacing, 0.0), 2);
        assert_eq!(columns_for(2.0 * CARD_TOTAL_W + spacing - 1.0, spacing, 0.0), 1);
        // The scroll bar's width is taken out first: a row must never be laid
        // out under it, or it would wrap and break the fixed row height.
        assert_eq!(columns_for(2.0 * CARD_TOTAL_W + spacing + 12.0, spacing, 14.0), 1);
        // Monotonic in the available width.
        let mut previous = 0;
        for w in [200.0, 400.0, 600.0, 1000.0, 2000.0] {
            let columns = columns_for(w, spacing, 0.0);
            assert!(columns >= previous, "{w}: {columns} < {previous}");
            previous = columns;
        }
    }

    fn headless_ctx() -> (egui::Context, egui::RawInput) {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut input = egui::RawInput::default();
        input.screen_rect =
            Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 896.0)));
        (ctx, input)
    }

    /// `ScrollArea::show_rows` places every row at exactly `CARD_H` + spacing:
    /// a card taller than that would overlap the next row and desynchronize the
    /// scroll extent. Measured on a real layout pass rather than assumed.
    #[test]
    fn a_card_is_exactly_one_grid_row_tall() {
        let (ctx, input) = headless_ctx();
        let mut textures = TextureStore::new();
        let mut measured = 0.0;
        let entry = entry();
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = ui.scope(|ui| {
                    card(ui, &entry, None, None, true, &mut textures);
                });
                measured = response.response.rect.height();
            });
        });
        assert!(measured > 0.0, "the card did not lay out");
        assert!(
            measured <= CARD_H,
            "a card is {measured} points tall, more than the {CARD_H} row height"
        );
        // …and not needlessly short either, or the grid would show gaps.
        assert!(CARD_H - measured < 12.0, "row height {CARD_H} is {measured} of card");
    }

    /// The reason the grid is virtualized: `TextureStore::get` must be called
    /// for the visible cards only. Without it a 200-game library decodes 200
    /// PNGs per frame and thrashes a cache capped at `MAX_TEXTURES`.
    #[test]
    fn only_the_visible_cards_ask_for_a_texture() {
        let entries: Vec<GameEntry> = (0..200)
            .map(|i| {
                let mut e = entry();
                e.id = format!("GAME-{i:04}");
                e.title = format!("GAME {i:03}");
                e
            })
            .collect();
        let games = BTreeMap::new();
        let pending = HashSet::new();
        // Every game has a picture, so every card drawn hits the store.
        let thumbs: HashMap<String, PathBuf> = entries
            .iter()
            .map(|e| (e.id.clone(), PathBuf::from(format!("/no/such/{}.png", e.id))))
            .collect();
        let mut state = LibraryUi::default();
        let mut textures = TextureStore::new();
        let (ctx, input) = headless_ctx();
        for _ in 0..2 {
            let _ = ctx.run(input.clone(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &mut LibraryModel {
                            entries: &entries,
                            games: &games,
                            dir: Path::new("/roms"),
                            thumbs: &thumbs,
                            pending: &pending,
                            state: &mut state,
                            textures: &mut textures,
                        },
                    );
                });
            });
        }
        let drawn = textures.len();
        assert!(drawn > 0, "no card was drawn at all");
        // A 1024x896 window shows about 4 columns x 3 rows; the bound is loose
        // on purpose (row heights and margins may be tuned) but must stay far
        // below both the library size and the texture cap.
        assert!(
            drawn <= 40,
            "{drawn} of 200 cards asked for a texture: the grid is not virtualized"
        );
        assert!(drawn < super::super::textures::MAX_TEXTURES, "{drawn}");
    }

    #[test]
    fn the_thumbnail_keeps_the_snes_aspect_ratio() {
        // 256x224 is 8:7; a card picture that drifted from it would letterbox
        // or stretch every game in the grid.
        let ratio = THUMB_W / THUMB_H;
        assert!((ratio - 256.0 / 224.0).abs() < 1e-6, "{ratio}");
        assert!(THUMB_W < CARD_W, "the picture must fit inside the card");
    }
}
