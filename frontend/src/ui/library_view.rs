//! Library grid: search, sort, favourites and the game cards themselves.
//!
//! The screen owns no data: the entries come from `library` (scanned on the
//! background thread), the per-game state from `prefs.games`, and the pictures
//! from `ui::textures`. Only the *view* state — what is typed in the search
//! box, which sort is active, which tab is open, which game's sheet is open —
//! lives here, in `LibraryUi`, because it must survive the immediate-mode
//! rebuild of every frame.
//!
//! **Cards are strictly uniform.** A card is not laid out from its content: the
//! grid computes one geometry per frame (`grid_metrics`) and every card is
//! painted into a rectangle of exactly that size. The picture is drawn
//! letterboxed inside a fixed 256:224 box, a missing picture draws a
//! placeholder of the same size, and the title is a two-row galley with an
//! ellipsis — so no title, no picture ratio and no missing thumbnail can change
//! a card's size.
//!
//! **The grid never scrolls sideways.** `grid_metrics` computes the column
//! count from the available width *minus the vertical scroll bar*, then shrinks
//! the cards so that `columns * card + gaps` is never wider than that; the
//! scroll area itself is `ScrollArea::vertical`, which has horizontal scrolling
//! disabled outright.
//!
//! **The grid is virtualized** (`ScrollArea::show_rows`): only the rows inside
//! the viewport are built, so `TextureStore::get` is called for the cards on
//! screen and no others. An `egui::ScrollArea::show` closure runs for *every*
//! child instead, which on a library larger than `textures::MAX_TEXTURES` would
//! walk the whole cache in eviction order — a 0 % hit rate, i.e. every picture
//! decoded from its PNG and re-uploaded on every frame. `show_rows` needs a
//! constant row height, which `GridMetrics::row_h` provides for the frame.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use egui::{Align, Color32, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};

use crate::i18n::{self, Lang, Msg};
use crate::library::{self, GameEntry, SortMode};
use crate::prefs::GameStats;

use super::icons::{self, Icon};
use super::tabs::{Tab, TRANSITION};
use super::textures::TextureStore;
use super::theme;
use super::Action;

/// The picture box of every card and of the sheet's hero: the SNES frame ratio.
/// A picture that is not 8:7 is letterboxed inside it rather than stretched.
pub const PICTURE_RATIO: f32 = 256.0 / 224.0;

/// Narrowest a card is ever drawn. Below this the picture stops carrying
/// enough of the game to be recognised.
const CARD_MIN_W: f32 = 168.0;
/// Widest a card is ever drawn: past it a wide window would show four huge
/// tiles instead of a library.
const CARD_MAX_W: f32 = 228.0;
/// Frame margin around a card's content, on every side.
const CARD_MARGIN: f32 = 8.0;
/// Border of the card frame. egui counts a frame's total size as
/// `content + inner_margin + 2 * stroke.width` (see `egui::Frame`'s own
/// diagram), so the border belongs in the card's outer size.
const CARD_STROKE: f32 = 1.0;
/// What one card adds to its content width: margins and border, both sides.
const CARD_CHROME: f32 = 2.0 * (CARD_MARGIN + CARD_STROKE);
/// Gap between the picture and the title.
const TITLE_GAP: f32 = 8.0;
/// Gap between the title block and the subtitle line.
const SUBTITLE_GAP: f32 = 4.0;
/// Rows the title is allowed to take before it is elided.
const TITLE_ROWS: usize = 2;
/// Text width of the search field.
const SEARCH_W: f32 = 220.0;
/// Space between the left edge of the search field and its magnifier.
const SEARCH_ICON_PAD: f32 = 8.0;

/// View state of the library screen (see module docs).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LibraryUi {
    /// Content of the search box.
    pub query: String,
    pub sort: SortMode,
    /// Which of the three library tabs is shown.
    pub tab: Tab,
    /// `game_id` of the open game sheet; `None` shows the grid.
    pub selected: Option<String>,
    /// Save state the sheet is waiting for a confirmation to delete. Armed by
    /// the first click on `Supprimer`, cleared by anything else — deleting a
    /// slot is irreversible, so it never happens on a single click.
    pub confirm_delete: Option<PathBuf>,
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
    /// Games whose sheet is being fetched from the catalogues.
    pub fetching: &'a HashSet<String>,
    pub state: &'a mut LibraryUi,
    pub textures: &'a mut TextureStore,
    /// Language every string of the grid is rendered in.
    pub lang: Lang,
}

/// The games one tab shows, in display order.
///
/// `library::arrange` does the searching and the sorting (and pins favourites
/// at the head, in every view); the tab only decides which of its results are
/// kept. `Récents` forces the recency order, since that is what the tab means.
pub fn visible<'a>(
    tab: Tab,
    entries: &'a [GameEntry],
    query: &str,
    sort: SortMode,
    games: &BTreeMap<String, GameStats>,
) -> Vec<&'a GameEntry> {
    let sort = if tab == Tab::Recent { SortMode::Recent } else { sort };
    library::arrange(entries, query, sort, games)
        .into_iter()
        .filter(|entry| match tab {
            Tab::Favorites => games.get(&entry.id).is_some_and(|s| s.favorite),
            Tab::Recent => games.get(&entry.id).is_some_and(|s| s.last_played.is_some()),
            _ => true,
        })
        .collect()
}

/// Geometry of the grid for one frame: how many columns fit, how wide a card
/// is and how tall one row is. Every card of the frame is painted at exactly
/// this size, which is what makes them uniform and what
/// `ScrollArea::show_rows` needs to place only the visible rows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridMetrics {
    pub columns: usize,
    /// Content width of a card (picture and text), chrome excluded.
    pub card_w: f32,
    /// Total width one card occupies, margins and border included.
    pub outer_w: f32,
    /// Height of one row, spacing excluded.
    pub row_h: f32,
}

/// Compute the grid for an area `available` points wide, with `spacing`
/// between two cards and `bar` reserved for the vertical scroll bar. `text_h`
/// is the height of a card's text block, measured from the fonts in use.
///
/// The card width gives way before the column count does: the columns are what
/// the window width affords, and the cards then shrink to fit inside it exactly
/// — a row is never laid out wider than the viewport, which is the only way a
/// horizontal scroll bar could appear.
pub fn grid_metrics(available: f32, spacing: f32, bar: f32, text_h: f32) -> GridMetrics {
    let min_outer = CARD_MIN_W + CARD_CHROME;
    let max_outer = CARD_MAX_W + CARD_CHROME;
    let usable = if available.is_finite() { (available - bar).max(min_outer) } else { min_outer };
    let columns = (((usable + spacing) / (min_outer + spacing)).floor() as usize).max(1);
    // Floored to a whole point, and the cards are then placed at whole-point
    // offsets (`show`): a fractional card width rasterizes to 209 pixels in one
    // column and 210 in the next, which is exactly the "the tiles are not the
    // same size" the brief opens with, seen at the pixel level.
    let outer_w = ((usable - (columns as f32 - 1.0) * spacing) / columns as f32)
        .clamp(min_outer, max_outer)
        .floor();
    let card_w = outer_w - CARD_CHROME;
    let row_h = (card_w / PICTURE_RATIO + TITLE_GAP + text_h + CARD_CHROME).ceil();
    GridMetrics { columns, card_w, outer_w, row_h }
}

/// Height of a card's text block: the title, at most `TITLE_ROWS` rows of it,
/// plus the subtitle line. Measured from the fonts actually installed rather
/// than assumed, so a change of type scale moves the card with it.
fn text_block_height(ctx: &egui::Context) -> f32 {
    let title = ctx.fonts_mut(|f| f.row_height(&theme::strong(theme::SIZE_BODY)));
    let subtitle = ctx.fonts_mut(|f| f.row_height(&theme::mono(theme::SIZE_SMALL)));
    TITLE_ROWS as f32 * title + SUBTITLE_GAP + subtitle
}

/// Draw the toolbar and the grid. Returns what the player asked for; changing
/// the search text, the sort order or opening a sheet is handled in place
/// (view state), so those produce no `Action`.
pub fn show(ui: &mut egui::Ui, model: &mut LibraryModel) -> Action {
    let mut action = Action::None;
    let tab = model.state.tab;
    let lang = model.lang;

    ui.horizontal(|ui| {
        // The magnifier belongs *in* the field, not floating beside it: outside
        // it read as a separate button and left the box looking unlabelled.
        let field = ui.add(
            egui::TextEdit::singleline(&mut model.state.query)
                .desired_width(SEARCH_W)
                .margin(egui::Margin {
                    left: (SEARCH_ICON_PAD + icons::SIZE + 7.0) as i8,
                    right: 10,
                    top: 5,
                    bottom: 5,
                })
                .hint_text(Msg::SearchPlaceholder.text(lang)),
        );
        Icon::Search.draw(
            ui.painter(),
            Rect::from_center_size(
                egui::pos2(
                    field.rect.left() + SEARCH_ICON_PAD + icons::SIZE / 2.0,
                    field.rect.center().y,
                ),
                Vec2::splat(icons::SIZE),
            ),
            theme::TEXT_DIM,
        );
        if !model.state.query.is_empty()
            && icons::ghost_button(ui, Icon::Close, icons::SIZE, theme::TEXT_DIM)
                .on_hover_text(Msg::Clear.text(lang))
                .clicked()
        {
            model.state.query.clear();
        }
        // The `Récents` tab *is* a sort order; offering another one there
        // would be a control that contradicts the tab it sits under.
        if tab != Tab::Recent {
            ui.add_space(12.0);
            ui.label(
                RichText::new(Msg::SortBy.text(lang)).size(theme::SIZE_SMALL).color(theme::TEXT_DIM),
            );
            for mode in SortMode::ALL {
                let selected = model.state.sort == mode;
                if ui.selectable_label(selected, mode.label(lang)).clicked() {
                    model.state.sort = mode;
                }
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // The folder the grid is showing is named here, where it can be
            // changed, rather than on a line of its own above the first card.
            // A game living outside the scanned folder can only get here by
            // being named explicitly — or dropped on the window, which the
            // hover text advertises since nothing on screen could suggest it.
            if icons::button(ui, Icon::Plus, Msg::AddGame.text(lang))
                .on_hover_text(Msg::AddGameHint.text(lang))
                .clicked()
            {
                action = Action::AddGame { replacing: None };
            }
            if icons::button(ui, Icon::Folder, Msg::ChooseFolder.text(lang))
                .on_hover_text(super::home::shorten_path(model.dir, 72))
                .clicked()
            {
                action = Action::ChooseLibraryDir;
            }
            // The library-wide catch-up. Deliberately a plain button next to
            // the others and never automatic: it is the only thing on this
            // screen that talks to the internet, and it says so on hover.
            if !model.entries.is_empty()
                && ui
                    .button(Msg::FillLibrary.text(lang))
                    .on_hover_text(Msg::FillLibraryHint.text(lang))
                    .clicked()
            {
                action = Action::FillLibrary;
            }
            if ui.button(Msg::Refresh.text(lang)).clicked() {
                action = Action::Rescan;
            }
        });
    });

    let shown = visible(tab, model.entries, &model.state.query, model.state.sort, model.games);
    // A status line only while something is actually happening. The game count
    // and the folder path used to sit here on every frame: a third band of
    // chrome, above every card, saying what the grid itself already shows.
    if let Some(status) =
        activity(lang, model.state.scanning, model.fetching.len(), model.pending.len())
    {
        ui.add_space(6.0);
        ui.label(RichText::new(status).font(theme::font(theme::SIZE_SMALL)).color(theme::TEXT_DIM));
    }
    ui.add_space(8.0);

    if let Some(error) = &model.state.error {
        ui.label(RichText::new(error).size(theme::SIZE_BODY).color(theme::RED));
        ui.add_space(8.0);
    }

    if shown.is_empty() && !model.state.scanning {
        let state = empty_state(tab, model.entries.is_empty(), !model.state.query.is_empty());
        return empty_screen(ui, state, &mut model.state.query, lang);
    }

    let metrics = grid_metrics(
        ui.available_width(),
        ui.spacing().item_spacing.x,
        ui.spacing().scroll.allocated_width(),
        text_block_height(ui.ctx()),
    );
    let spacing = ui.spacing().item_spacing.x;
    let rows = shown.len().div_ceil(metrics.columns);
    egui::ScrollArea::vertical()
        // The bar is what tells the player there is more below. It is drawn
        // only when there is (egui's floating bars, invisible at rest, read as
        // "nothing more"), and its width is reserved either way by
        // `theme::apply`, so the last column never ends up under it.
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
        .auto_shrink([false, false])
        .show_rows(ui, metrics.row_h, rows, |ui, range| {
            for row in range {
                let first = row * metrics.columns;
                let last = (first + metrics.columns).min(shown.len());
                if first >= last {
                    continue;
                }
                let (strip, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), metrics.row_h),
                    Sense::hover(),
                );
                for (column, entry) in shown[first..last].iter().enumerate() {
                    let rect = Rect::from_min_size(
                        egui::pos2(
                            (strip.left() + column as f32 * (metrics.outer_w + spacing)).round(),
                            strip.top().round(),
                        ),
                        Vec2::new(metrics.outer_w, metrics.row_h),
                    );
                    let stats = model.games.get(&entry.id);
                    let picture = model.thumbs.get(&entry.id).map(|p| p.as_path());
                    let pending = model.pending.contains(&entry.id);
                    let hit =
                        card(ui, rect, entry, stats, picture, pending, model.textures, lang);
                    if hit.favorite {
                        action = Action::ToggleFavorite(entry.id.clone());
                    } else if hit.play {
                        action = Action::Launch { path: entry.path.clone(), resume: true };
                    } else if hit.open {
                        model.state.selected = Some(entry.id.clone());
                    }
                }
            }
        });

    action
}

/// The one line the toolbar shows while the library is working, or `None` when
/// it is not: a scan in flight, sheets being fetched, or thumbnails still being
/// emulated. Written out properly rather than with a parenthesised plural —
/// this is prose, and the grid says how many games there are by showing them.
///
/// Fetching is announced before thumbnail generation because it is the one the
/// player asked for by pressing a button, and because it is the one that is
/// talking to somebody else's server.
pub fn activity(lang: Lang, scanning: bool, fetching: usize, pending: usize) -> Option<String> {
    if scanning {
        return Some(Msg::ScanningFolder.text(lang).to_string());
    }
    if fetching > 0 {
        return Some(i18n::sheets_pending(lang, fetching));
    }
    match pending {
        0 => None,
        n => Some(i18n::thumbnails_pending(lang, n)),
    }
}

/// What a card was asked to do this frame. A card click opens the game sheet;
/// launching is the `Jouer` button that appears on it, so that a game is never
/// started by a stray click on the grid.
#[derive(Default)]
struct CardHit {
    open: bool,
    favorite: bool,
    play: bool,
}

/// One game card, painted into exactly `rect` — no layout, no content-driven
/// size: that is what makes every card of the grid identical.
///
/// At rest it shows the picture, the title and one line of facts. Under the
/// pointer (or under the keyboard focus, which must be as visible) it lifts:
/// a shadow appears, the border takes the accent, the picture brightens and
/// the two actions of a tile — `Jouer` and the favourite star — fade in over
/// `tabs::TRANSITION`.
fn card(
    ui: &mut egui::Ui,
    rect: Rect,
    entry: &GameEntry,
    stats: Option<&GameStats>,
    picture: Option<&Path>,
    pending: bool,
    textures: &mut TextureStore,
    lang: Lang,
) -> CardHit {
    let mut hit = CardHit::default();
    let favorite = stats.is_some_and(|s| s.favorite);
    let id = egui::Id::new(("prisme-card", &entry.id));
    let response = ui.interact(rect, id, Sense::CLICK | Sense::FOCUSABLE);
    let focused = response.has_focus();
    // `rect_contains_pointer`, not `response.hovered()`: the star and the
    // `Jouer` button are widgets of their own *inside* the card, and egui gives
    // the hover to the topmost one — the card would go dark the moment the
    // pointer reached the button that only exists while it is lit, which flips
    // the state back and forth on the spot.
    let over = ui.rect_contains_pointer(rect);
    let lit = ui.ctx().animate_bool_with_time(id.with("lit"), over || focused, TRANSITION);

    let inner = rect.shrink(CARD_STROKE);
    let content = inner.shrink(CARD_MARGIN);
    let picture_rect = Rect::from_min_size(
        content.min,
        Vec2::new(content.width(), content.width() / PICTURE_RATIO),
    );

    if !ui.is_rect_visible(rect) {
        return hit;
    }
    let painter = ui.painter();

    // Elevation: a soft plate under the card, growing with the transition.
    if lit > 0.0 {
        painter.rect_filled(
            rect.translate(Vec2::new(0.0, 2.0 * lit)).expand(2.0 * lit),
            8.0,
            Color32::from_black_alpha((70.0 * lit) as u8),
        );
    }
    let fill = theme::BG_CARD.lerp_to_gamma(theme::BG_WIDGET_HOVER, lit);
    // Every card carries the same border, favourite or not: a yellow frame on
    // three tiles of a row made them read as a different kind of object — and
    // the state is already said, once, by the star stamped on the picture.
    let border = theme::STROKE.lerp_to_gamma(theme::ACCENT, lit);
    painter.rect(rect, 8.0, fill, Stroke::new(CARD_STROKE, border), StrokeKind::Inside);

    paint_picture(
        painter,
        picture_rect,
        picture,
        textures,
        ui.ctx(),
        Placeholder::game_pending(pending),
        lang,
    );
    if lit > 0.0 {
        // The picture brightens rather than the card only: it is the part of
        // the tile the pointer is aiming at.
        painter.rect_filled(picture_rect, 4.0, Color32::from_white_alpha((26.0 * lit) as u8));
    }

    // The coprocessor pill is stamped on the corner of the picture rather than
    // laid out on the subtitle line: its width depends on the chip's name, and
    // on the line it would push the play time out of a card of fixed width.
    if let Some(chip) = &entry.coprocessor {
        icons::paint_chip_badge(painter, picture_rect.min + Vec2::splat(6.0), chip);
    }

    let title_font = theme::strong(theme::SIZE_BODY);
    let title_row = ui.ctx().fonts_mut(|f| f.row_height(&title_font));
    let title_top = picture_rect.bottom() + TITLE_GAP;
    let galley = elided_galley(
        painter,
        &entry.display_title(),
        title_font,
        content.width(),
        TITLE_ROWS,
    );
    painter.galley(egui::pos2(content.left(), title_top), galley, theme::TEXT);

    let subtitle_top = title_top + TITLE_ROWS as f32 * title_row + SUBTITLE_GAP;
    painter.text(
        egui::pos2(content.left(), subtitle_top),
        egui::Align2::LEFT_TOP,
        subtitle(entry, stats, lang),
        theme::mono(theme::SIZE_SMALL),
        if entry.missing { theme::RED } else { theme::TEXT_DIM },
    );

    // The favourite star sits on the picture: always there once it is one (it
    // is the state the yellow announces), otherwise only while the card is lit.
    let star_rect = Rect::from_min_size(
        egui::pos2(picture_rect.right() - icons::SIZE - 8.0, picture_rect.top() + 8.0),
        Vec2::splat(icons::SIZE),
    );
    let star_visible = favorite || lit > 0.0;
    let star = ui.interact(star_rect.expand(4.0), id.with("star"), Sense::CLICK);
    if star_visible {
        let alpha = if favorite { 1.0 } else { lit };
        let colour = theme::YELLOW.gamma_multiply(if star.hovered() { alpha } else { 0.85 * alpha });
        // An opaque plate under it, like the chip pill: the star is drawn over
        // a game picture whose colours are unknown.
        ui.painter().circle_filled(
            star_rect.center(),
            icons::SIZE * 0.8,
            Color32::from_black_alpha((170.0 * alpha) as u8),
        );
        let icon = if favorite { Icon::StarFilled } else { Icon::Star };
        icon.draw(ui.painter(), star_rect, colour);
    }

    // …and `Jouer`, which only exists while the card is lit: the grid must not
    // launch a game on a stray click.
    let play_size = Vec2::new(88.0, 30.0);
    let play_rect = Rect::from_center_size(picture_rect.center(), play_size);
    // A game whose file is gone cannot be started, so it is not offered: a
    // `Jouer` that can only fail is worse than no button at all. Its card still
    // opens, which is where it can be relocated or forgotten.
    let play = ui.interact(play_rect, id.with("play"), Sense::CLICK);
    if lit > 0.0 && !entry.missing {
        let fill = theme::ACCENT.gamma_multiply(if play.hovered() { lit } else { 0.9 * lit });
        ui.painter().rect(
            play_rect,
            6.0,
            fill,
            Stroke::new(1.0, fill),
            StrokeKind::Inside,
        );
        let label_colour = Color32::WHITE.gamma_multiply(lit);
        let icon_rect = Rect::from_min_size(
            egui::pos2(play_rect.left() + 12.0, play_rect.center().y - icons::SIZE / 2.0),
            Vec2::splat(icons::SIZE),
        );
        Icon::Play.draw(ui.painter(), icon_rect, label_colour);
        ui.painter().text(
            egui::pos2(icon_rect.right() + 6.0, play_rect.center().y),
            egui::Align2::LEFT_CENTER,
            Msg::Play.text(lang),
            theme::font(theme::SIZE_BUTTON),
            label_colour,
        );
    }

    if focused {
        // Keyboard focus is drawn on its own ring, outside the card: the lit
        // state alone could be mistaken for a hover the player is not causing.
        ui.painter().rect_stroke(
            rect.expand(2.0),
            10.0,
            Stroke::new(2.0, theme::ACCENT),
            StrokeKind::Outside,
        );
    }

    // The star and the play button sit *inside* the card's own rectangle, so a
    // click on either must not also open the sheet.
    if star.clicked() {
        hit.favorite = true;
    } else if lit > 0.0 && !entry.missing && play.clicked() {
        hit.play = true;
    } else if response.clicked() && !star.hovered() && !(lit > 0.0 && play.hovered()) {
        hit.open = true;
    }
    hit
}

/// Lay `text` out in at most `max_rows` rows of `max_width`, eliding what does
/// not fit. This is what keeps a long title from changing a card's height: the
/// galley has a bounded number of rows by construction.
pub fn elided_galley(
    painter: &egui::Painter,
    text: &str,
    font: egui::FontId,
    max_width: f32,
    max_rows: usize,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat { font_id: font, color: Color32::PLACEHOLDER, ..Default::default() },
    );
    job.wrap = egui::text::TextWrapping {
        max_width,
        max_rows,
        // Break between words: a title cut in the middle of one ("A Li / nk to
        // the Past") reads as a bug. A single word longer than the row is
        // broken anyway, since there is nowhere else to break it.
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    painter.layout_job(job)
}

/// One line under the title: region and play time when there is one — what
/// tells two dumps of the same game apart at a glance. The coprocessor is not
/// in it: it is the coloured pill stamped on the picture
/// (`icons::paint_chip_badge`), where its accent identifies it before its name
/// is read.
fn subtitle(entry: &GameEntry, stats: Option<&GameStats>, lang: Lang) -> String {
    if entry.missing {
        // Region and play time describe a cartridge we can no longer read; the
        // only fact worth the line is that the file is not where it was.
        return Msg::FileMissing.text(lang).to_string();
    }
    let mut parts = vec![entry.region.clone()];
    if let Some(played) = stats.map(|s| s.play_seconds).filter(|s| *s > 0) {
        parts.push(library::format_play_time(lang, played));
    }
    parts.join(" · ")
}

/// What a picture box shows when there is no picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placeholder {
    /// No thumbnail for this game, and none coming.
    NoPicture,
    /// Its thumbnail is being emulated right now.
    Pending,
    /// A save state written before previews existed, or one whose picture
    /// could not be written: the slot is there, its picture is not.
    NoPreview,
}

impl Placeholder {
    /// What a game's own picture box shows when it has none: a thumbnail that
    /// is still being emulated is not the same state as one that will never
    /// come.
    pub fn game_pending(pending: bool) -> Self {
        if pending {
            Placeholder::Pending
        } else {
            Placeholder::NoPicture
        }
    }

    fn caption(self, lang: Lang) -> &'static str {
        match self {
            Placeholder::NoPicture => Msg::NoPicture.text(lang),
            Placeholder::Pending => Msg::PictureRunning.text(lang),
            Placeholder::NoPreview => Msg::NoPreview.text(lang),
        }
    }
}

/// Draw a game picture inside `rect`, letterboxed on the SNES ratio, or a
/// placeholder of the very same size when there is none.
///
/// Letterboxing is what makes the tiles uniform: the box is fixed, and a
/// picture whose ratio differs (a promoted screenshot from another source) is
/// centred inside it on the inset background instead of being stretched or
/// changing the card's height.
pub fn paint_picture(
    painter: &egui::Painter,
    rect: Rect,
    path: Option<&Path>,
    textures: &mut TextureStore,
    ctx: &egui::Context,
    placeholder: Placeholder,
    lang: Lang,
) {
    if let Some(path) = path {
        if let Some(handle) = textures.get(ctx, path) {
            let [w, h] = handle.size();
            let (id, size) = (handle.id(), Vec2::new(w as f32, h as f32));
            painter.rect_filled(rect, 4.0, theme::BG_DEEP);
            painter.image(
                id,
                letterbox(rect, size),
                Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            return;
        }
    }
    paint_placeholder(painter, rect, placeholder, lang);
}

/// The largest rectangle of `source`'s ratio that fits inside `box_rect`,
/// centred in it.
pub fn letterbox(box_rect: Rect, source: Vec2) -> Rect {
    if source.x <= 0.0 || source.y <= 0.0 {
        return box_rect;
    }
    let scale = (box_rect.width() / source.x).min(box_rect.height() / source.y);
    Rect::from_center_size(box_rect.center(), source * scale)
}

/// Placeholder picture: the silhouette of a cartridge with the product mark on
/// its label, on a dark plate, plus a word on what is happening. Deliberately
/// not an empty rectangle — a game whose thumbnail is still being generated
/// must look like it, not like a failure — and deliberately the same size as
/// the picture it stands in for.
fn paint_placeholder(painter: &egui::Painter, rect: Rect, kind: Placeholder, lang: Lang) {
    painter.rect_filled(rect, 4.0, theme::BG_DEEP);
    let size = rect.size();

    // Cartridge: the landscape shell of a SNES cartridge — wider than it is
    // tall, the grip ridge across the top, and the label taking its lower two
    // thirds with the product mark printed on it. A portrait box with a rule
    // near the top, which is what stood here, reads as a sheet of paper.
    let body_h = size.y * 0.44;
    let body_w = body_h * 1.15;
    let body = Rect::from_center_size(
        rect.center() - Vec2::new(0.0, size.y * 0.07),
        Vec2::new(body_w, body_h),
    );
    let stroke = Stroke::new(1.5, theme::TEXT_DIM);
    painter.rect_stroke(body, 3.0, stroke, StrokeKind::Inside);
    // The ridge: the moulded step across the top of the shell.
    let ridge = body.top() + body_h * 0.20;
    painter.line_segment(
        [egui::pos2(body.left(), ridge), egui::pos2(body.right(), ridge)],
        Stroke::new(1.0, theme::TEXT_DIM),
    );
    let label = Rect::from_min_max(
        egui::pos2(body.left() + body_w * 0.13, ridge + body_h * 0.13),
        egui::pos2(body.right() - body_w * 0.13, body.bottom() - body_h * 0.10),
    );
    painter.rect_stroke(label, 2.0, Stroke::new(1.0, theme::TEXT_DIM), StrokeKind::Inside);
    theme::mark(painter, label.shrink2(Vec2::new(label.width() * 0.16, label.height() * 0.16)));

    painter.text(
        egui::pos2(rect.center().x, rect.max.y - size.y * 0.08),
        egui::Align2::CENTER_BOTTOM,
        kind.caption(lang),
        theme::font(theme::SIZE_SMALL),
        theme::TEXT_DIM,
    );
    // Keeps the placeholder visually part of the card, like the picture it
    // stands in for.
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0, theme::STROKE), StrokeKind::Inside);
}

/// Draw a picture inside a `Ui`, allocating `size` for it. The sheet's hero and
/// its gallery lay their pictures out; the grid paints them at computed
/// rectangles instead (`paint_picture`).
pub fn thumbnail(
    ui: &mut egui::Ui,
    path: Option<&Path>,
    placeholder: Placeholder,
    textures: &mut TextureStore,
    size: Vec2,
    lang: Lang,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let ctx = ui.ctx().clone();
        paint_picture(ui.painter(), rect, path, textures, &ctx, placeholder, lang);
    }
    response
}

// --- empty screens --------------------------------------------------------

/// The call to action an empty screen offers, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyCall {
    /// Pick the folder that holds the ROMs.
    ChooseFolder,
    /// Drop the search that matched nothing.
    ClearSearch,
    /// Nothing to offer: the tab fills itself as the player uses the shell.
    None,
}

/// What an empty grid says. Never a mute void: it names what is missing and,
/// when the player can do something about it, what to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmptyState {
    pub title: Msg,
    pub hint: Msg,
    pub call: EmptyCall,
}

/// Resolve the empty screen from the tab and why it is empty. An empty folder,
/// a search that matched nothing and a tab that fills itself with use are three
/// different problems, and only the first two can be acted on.
pub fn empty_state(tab: Tab, library_empty: bool, searching: bool) -> EmptyState {
    if library_empty {
        return EmptyState {
            title: Msg::EmptyLibrary,
            hint: Msg::EmptyLibraryHint,
            call: EmptyCall::ChooseFolder,
        };
    }
    if searching {
        return EmptyState {
            title: Msg::EmptySearch,
            hint: Msg::EmptySearchHint,
            call: EmptyCall::ClearSearch,
        };
    }
    match tab {
        Tab::Favorites => EmptyState {
            title: Msg::EmptyFavorites,
            hint: Msg::EmptyFavoritesHint,
            call: EmptyCall::None,
        },
        Tab::Recent => EmptyState {
            title: Msg::EmptyRecent,
            hint: Msg::EmptyRecentHint,
            call: EmptyCall::None,
        },
        _ => EmptyState {
            title: Msg::EmptyLibrary,
            hint: Msg::EmptyLibraryHint,
            call: EmptyCall::ChooseFolder,
        },
    }
}

/// Draw the empty screen and its call to action.
fn empty_screen(
    ui: &mut egui::Ui,
    state: EmptyState,
    query: &mut String,
    lang: Lang,
) -> Action {
    let mut action = Action::None;
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        let icon = match state.call {
            EmptyCall::ChooseFolder => Icon::Folder,
            EmptyCall::ClearSearch => Icon::Search,
            EmptyCall::None => Icon::Star,
        };
        icons::show(ui, icon, 56.0, theme::TEXT_DIM);
        ui.add_space(12.0);
        ui.label(
            RichText::new(state.title.text(lang))
                .font(theme::strong(theme::SIZE_HEADING))
                .color(theme::TEXT),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(state.hint.text(lang))
                .font(theme::font(theme::SIZE_BODY))
                .color(theme::TEXT_DIM),
        );
        ui.add_space(14.0);
        match state.call {
            EmptyCall::ChooseFolder => {
                if icons::primary_button(ui, Icon::Folder, Msg::ChooseRomFolder.text(lang))
                    .clicked()
                {
                    action = Action::ChooseLibraryDir;
                }
            }
            EmptyCall::ClearSearch => {
                if icons::button(ui, Icon::Close, Msg::ClearSearch.text(lang)).clicked() {
                    query.clear();
                }
            }
            EmptyCall::None => {}
        }
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

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
            crc32: 0x0000_0001,
            missing: false,
        }
    }

    #[test]
    fn an_empty_screen_names_what_is_missing_and_what_to_do() {
        // The brief's own words, and the button that goes with them.
        let empty = empty_state(Tab::Library, true, false);
        assert_eq!(empty.title.text(Lang::Fr), "Aucun jeu ici.");
        assert_eq!(empty.hint.text(Lang::Fr), "Choisissez le dossier qui contient vos ROMs.");
        assert_eq!(empty.title.text(Lang::En), "No game here.");
        assert_eq!(empty.call, EmptyCall::ChooseFolder);
        // An empty folder wins over every other reason, on every tab.
        for tab in [Tab::Library, Tab::Favorites, Tab::Recent] {
            assert_eq!(empty_state(tab, true, true).call, EmptyCall::ChooseFolder);
        }
        // A search that matched nothing offers to drop the search.
        let none = empty_state(Tab::Library, false, true);
        assert!(none.title.text(Lang::Fr).contains("recherche"));
        assert!(none.title.text(Lang::En).contains("search"));
        assert_eq!(none.call, EmptyCall::ClearSearch);
        // The two filtered tabs fill themselves: nothing to click, but never a
        // mute void either.
        for (tab, word) in [(Tab::Favorites, "favori"), (Tab::Recent, "récente")] {
            let state = empty_state(tab, false, false);
            let title = state.title.text(Lang::Fr);
            assert!(title.to_lowercase().contains(word), "{tab:?}: {title}");
            for lang in Lang::ALL {
                assert!(!state.hint.text(lang).is_empty());
            }
            assert_eq!(state.call, EmptyCall::None);
        }
    }

    #[test]
    fn the_card_subtitle_lists_region_and_play_time() {
        let mut e = entry();
        assert_eq!(subtitle(&e, None, Lang::Fr), "PAL");
        // The coprocessor is not in the line: it is the coloured pill stamped
        // on the picture, so the subtitle stays the same length whatever the
        // chip.
        e.coprocessor = Some("SuperFX".to_string());
        assert_eq!(subtitle(&e, None, Lang::Fr), "PAL");
        let stats = GameStats { play_seconds: 3600, ..Default::default() };
        assert_eq!(subtitle(&e, Some(&stats), Lang::Fr), "PAL · 1 h 00");
        // A never-played game shows no time at all rather than "0".
        let stats = GameStats::default();
        assert_eq!(subtitle(&e, Some(&stats), Lang::Fr), "PAL");
    }

    /// The toolbar says something only while the library is working, and says
    /// it in proper French. At rest the line does not exist at all — it used to
    /// carry the game count and the folder path above every single card.
    #[test]
    fn the_toolbar_speaks_only_while_the_library_is_working() {
        assert_eq!(activity(Lang::Fr, false, 0, 0), None);
        assert_eq!(activity(Lang::Fr, true, 0, 0).as_deref(), Some("Analyse du dossier…"));
        // A scan in flight wins: the count of queued thumbnails is not settled
        // until it ends.
        assert_eq!(activity(Lang::Fr, true, 0, 3).as_deref(), Some("Analyse du dossier…"));
        assert_eq!(activity(Lang::Fr, false, 0, 1).as_deref(), Some("1 miniature en cours…"));
        assert_eq!(activity(Lang::Fr, false, 0, 7).as_deref(), Some("7 miniatures en cours…"));
        assert_eq!(activity(Lang::En, false, 0, 1).as_deref(), Some("1 thumbnail being built…"));
        assert_eq!(activity(Lang::En, false, 0, 7).as_deref(), Some("7 thumbnails being built…"));
        // A sheet the player asked for is announced ahead of the background
        // thumbnails: it is the one that is talking to somebody else's server.
        assert_eq!(activity(Lang::Fr, false, 2, 7).as_deref(), Some("2 fiches en cours…"));
        assert_eq!(activity(Lang::En, false, 1, 7).as_deref(), Some("1 sheet being filled in…"));
        assert_eq!(activity(Lang::Fr, true, 2, 7).as_deref(), Some("Analyse du dossier…"));
        // No parenthesised plural anywhere in the line, in either language.
        for lang in Lang::ALL {
            for n in 0..12 {
                if let Some(line) = activity(lang, false, n, n) {
                    assert!(!line.contains("(s)"), "{line}");
                }
            }
        }
    }

    #[test]
    fn the_default_view_state_is_an_unfiltered_title_sorted_grid() {
        let state = LibraryUi::default();
        assert!(state.query.is_empty());
        assert_eq!(state.sort, SortMode::Title);
        assert_eq!(state.tab, Tab::Library);
        assert_eq!(state.selected, None);
        assert_eq!(state.confirm_delete, None);
        assert!(!state.scanning);
        assert_eq!(state.error, None);
    }

    /// The defect the brief opens with: a row must never be wider than the
    /// area it is laid out in, at any window width — that overflow is what
    /// produces a horizontal scroll bar.
    #[test]
    fn a_row_never_exceeds_the_width_it_was_given() {
        let spacing = 10.0;
        let bar = 14.0;
        let text_h = 60.0;
        let mut width = 320.0;
        while width <= 3000.0 {
            let m = grid_metrics(width, spacing, bar, text_h);
            let row = m.columns as f32 * m.outer_w + (m.columns as f32 - 1.0) * spacing;
            assert!(m.columns >= 1, "{width}: no column at all");
            assert!(
                row <= width - bar + 0.01,
                "{width}: a {}-column row is {row} wide, more than {} usable",
                m.columns,
                width - bar
            );
            // Cards stay inside the size range whatever the window.
            assert!(m.card_w >= CARD_MIN_W - 0.01, "{width}: card {} too narrow", m.card_w);
            assert!(m.card_w <= CARD_MAX_W + 0.01, "{width}: card {} too wide", m.card_w);
            // The row is the picture box plus the text block plus the chrome,
            // rounded up to a whole point (see `grid_metrics`).
            let expected = (m.card_w / PICTURE_RATIO + TITLE_GAP + text_h + CARD_CHROME).ceil();
            assert!((m.row_h - expected).abs() < 0.01, "{width}: {} vs {expected}", m.row_h);
            // Whole-point geometry: two cards of the same frame must rasterize
            // to the same number of pixels, at every window width.
            assert_eq!(m.outer_w, m.outer_w.floor(), "{width}: fractional card width");
            assert_eq!(m.row_h, m.row_h.floor(), "{width}: fractional row height");
            width += 7.0;
        }
    }

    #[test]
    fn the_column_count_grows_with_the_window_and_never_falls_below_one() {
        let (spacing, text_h) = (10.0, 60.0);
        // A window narrower than one card still shows that card.
        assert_eq!(grid_metrics(0.0, spacing, 0.0, text_h).columns, 1);
        assert_eq!(grid_metrics(100.0, spacing, 0.0, text_h).columns, 1);
        // The three widths the brief asks to be checked on a capture.
        let columns: Vec<usize> = [900.0_f32, 1280.0, 1600.0]
            .into_iter()
            .map(|w| grid_metrics(w - 48.0, spacing, 14.0, text_h).columns)
            .collect();
        assert!(columns[0] >= 3, "900 px shows only {} columns", columns[0]);
        assert!(columns[1] > columns[0] && columns[2] > columns[1], "{columns:?}");
        // Monotonic in the available width.
        let mut previous = 0;
        for w in [200.0, 400.0, 600.0, 1000.0, 2000.0, 4000.0] {
            let columns = grid_metrics(w, spacing, 0.0, text_h).columns;
            assert!(columns >= previous, "{w}: {columns} < {previous}");
            previous = columns;
        }
    }

    /// The picture box is the SNES frame ratio and a picture of any other
    /// shape is letterboxed inside it, never stretched and never resizing the
    /// box — which is what "the tiles are not the same size" was about.
    #[test]
    fn a_picture_is_letterboxed_inside_a_fixed_box() {
        let box_rect = Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(160.0, 140.0));
        // Native 256x224: fills the box exactly.
        let fitted = letterbox(box_rect, egui::vec2(256.0, 224.0));
        assert!((fitted.width() - 160.0).abs() < 0.01, "{fitted:?}");
        assert!((fitted.height() - 140.0).abs() < 0.01, "{fitted:?}");
        // A square picture: bars left and right, same box.
        let square = letterbox(box_rect, egui::vec2(200.0, 200.0));
        assert!((square.width() - 140.0).abs() < 0.01, "{square:?}");
        assert!(box_rect.contains_rect(square));
        assert_eq!(square.center(), box_rect.center());
        // A very wide picture: bars above and below.
        let wide = letterbox(box_rect, egui::vec2(640.0, 100.0));
        assert!(box_rect.contains_rect(wide), "{wide:?}");
        assert!((wide.width() - 160.0).abs() < 0.01);
        // Degenerate sizes never produce a NaN rectangle.
        assert_eq!(letterbox(box_rect, egui::vec2(0.0, 0.0)), box_rect);
        assert!((PICTURE_RATIO - 256.0 / 224.0).abs() < 1e-6);
    }

    /// A long title must be elided, not wrapped forever: the card's height is
    /// fixed, so a third row would spill onto the row below.
    #[test]
    fn a_long_title_is_elided_to_two_rows() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut rows = (0, 0);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let long = elided_galley(
                    ui.painter(),
                    "The Legend of Zelda - A Link to the Past (Europe) (Rev 1) [!]",
                    theme::strong(theme::SIZE_BODY),
                    160.0,
                    TITLE_ROWS,
                );
                let short = elided_galley(
                    ui.painter(),
                    "F-ZERO",
                    theme::strong(theme::SIZE_BODY),
                    160.0,
                    TITLE_ROWS,
                );
                rows = (long.rows.len(), short.rows.len());
                assert!(long.elided, "a 60-character title must be elided at 160 points");
                assert!(!short.elided);
                assert!(long.size().x <= 160.01, "{}", long.size().x);
            });
        });
        assert_eq!(rows.0, TITLE_ROWS);
        assert_eq!(rows.1, 1);
    }

    fn headless_ctx() -> (egui::Context, egui::RawInput) {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut input = egui::RawInput::default();
        input.screen_rect =
            Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 896.0)));
        (ctx, input)
    }

    fn model_frame(
        ctx: &egui::Context,
        input: egui::RawInput,
        entries: &[GameEntry],
        games: &BTreeMap<String, GameStats>,
        thumbs: &HashMap<String, PathBuf>,
        state: &mut LibraryUi,
        textures: &mut TextureStore,
        lang: Lang,
    ) -> egui::FullOutput {
        let pending = HashSet::new();
        let fetching = HashSet::new();
        ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(
                    ui,
                    &mut LibraryModel {
                        entries,
                        games,
                        dir: Path::new("/roms"),
                        thumbs,
                        pending: &pending,
                        fetching: &fetching,
                        state,
                        textures,
                        lang,
                    },
                );
            });
        })
    }

    fn painted_text(output: &egui::FullOutput) -> String {
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

    /// The chip is still on the card, as the pill only the coprocessor games
    /// carry.
    #[test]
    fn a_coprocessor_game_still_names_its_chip_on_the_card() {
        let mut e = entry();
        e.coprocessor = Some("SuperFX".to_string());
        let (ctx, input) = headless_ctx();
        let mut textures = TextureStore::new();
        let mut state = LibraryUi::default();
        let output = model_frame(
            &ctx,
            input,
            std::slice::from_ref(&e),
            &BTreeMap::new(),
            &HashMap::new(),
            &mut state,
            &mut textures,
            Lang::Fr,
        );
        assert!(painted_text(&output).contains("SuperFX"), "{}", painted_text(&output));
    }

    /// Each tab shows what its name says, and only that.
    #[test]
    fn each_tab_filters_the_library_the_way_its_name_says() {
        let mut entries = Vec::new();
        for i in 0..4 {
            let mut e = entry();
            e.id = format!("GAME-{i:04}");
            e.title = format!("GAME {i}");
            entries.push(e);
        }
        let mut games = BTreeMap::new();
        games.insert(
            "GAME-0001".to_string(),
            GameStats { favorite: true, ..Default::default() },
        );
        games.insert(
            "GAME-0002".to_string(),
            GameStats { last_played: Some(1_700_000_000), ..Default::default() },
        );
        let ids = |tab| -> Vec<String> {
            visible(tab, &entries, "", SortMode::Title, &games)
                .into_iter()
                .map(|e| e.id.clone())
                .collect()
        };
        assert_eq!(ids(Tab::Library).len(), 4);
        assert_eq!(ids(Tab::Favorites), vec!["GAME-0001".to_string()]);
        assert_eq!(ids(Tab::Recent), vec!["GAME-0002".to_string()]);
        // The search still applies inside a tab.
        let found = visible(Tab::Library, &entries, "game 3", SortMode::Title, &games);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "GAME-0003");
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
        // Every game has a picture, so every card drawn hits the store.
        let thumbs: HashMap<String, PathBuf> = entries
            .iter()
            .map(|e| (e.id.clone(), PathBuf::from(format!("/no/such/{}.png", e.id))))
            .collect();
        let mut state = LibraryUi::default();
        let mut textures = TextureStore::new();
        let (ctx, input) = headless_ctx();
        for _ in 0..2 {
            let _ = model_frame(
                &ctx,
                input.clone(),
                &entries,
                &games,
                &thumbs,
                &mut state,
                &mut textures,
                Lang::Fr,
            );
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

    /// One frame of a one-card grid, with the pointer where the caller says.
    /// Four passes: egui resolves hovering from the widget list of the
    /// previous pass, and the card's own transition then has to settle.
    fn draw_with_pointer(pointer: Option<egui::Pos2>) -> (String, Vec<egui::Shape>) {
        let e = entry();
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut textures = TextureStore::new();
        let mut state = LibraryUi::default();
        let mut output = None;
        for pass in 0..4 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1024.0, 896.0),
                )),
                time: Some(pass as f64 * 0.5),
                events: pointer.map(egui::Event::PointerMoved).into_iter().collect(),
                ..Default::default()
            };
            output = Some(model_frame(
                &ctx,
                input,
                std::slice::from_ref(&e),
                &BTreeMap::new(),
                &HashMap::new(),
                &mut state,
                &mut textures,
                Lang::Fr,
            ));
        }
        let output = output.expect("no frame");
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
        (painted_text(&output), shapes)
    }

    /// The hover state the brief asks for: the tile lifts, and the two actions
    /// it carries appear. At rest neither exists — the grid must not launch a
    /// game on a stray click, so `Jouer` only exists under the pointer.
    #[test]
    fn a_tile_under_the_pointer_offers_to_play_and_lifts() {
        let (resting, resting_shapes) = draw_with_pointer(None);
        assert!(!resting.contains("Jouer"), "the resting tile must offer nothing: {resting}");

        // Over the picture of the only card of the grid, which starts just
        // under the toolbar row.
        let (hovered, shapes) = draw_with_pointer(Some(egui::pos2(120.0, 150.0)));
        assert!(hovered.contains("Jouer"), "{hovered}");
        // The star of a game that is not a favourite also appears only then.
        let stars = |shapes: &[egui::Shape]| {
            shapes
                .iter()
                .filter(|s| matches!(s, egui::Shape::Path(p) if p.points.len() == 10))
                .count()
        };
        assert_eq!(stars(&resting_shapes), 0, "a plain game shows no star at rest");
        assert_eq!(stars(&shapes), 1);
        // …and the card lifts: a shadow plate is painted under it.
        let shadow = shapes.iter().any(|s| matches!(s, egui::Shape::Rect(r) if r.fill.a() > 0 && r.fill.r() == 0 && r.fill.g() == 0 && r.fill.b() == 0));
        assert!(shadow, "no elevation under the hovered tile");
    }

    /// Keyboard focus must be as visible as the pointer's hover, or the grid
    /// cannot be used without a mouse.
    #[test]
    fn a_focused_tile_is_lit_and_ringed() {
        let e = entry();
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut textures = TextureStore::new();
        let mut state = LibraryUi::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1024.0, 896.0),
            )),
            ..Default::default()
        };
        let card_id = egui::Id::new(("prisme-card", &e.id));
        let mut output = None;
        for pass in 0..4 {
            let mut input = input.clone();
            input.time = Some(pass as f64 * 0.5);
            // The card is focusable: asking for its focus is exactly what the
            // Tab key does, and it must stick.
            ctx.memory_mut(|memory| memory.request_focus(card_id));
            output = Some(model_frame(
                &ctx,
                input,
                std::slice::from_ref(&e),
                &BTreeMap::new(),
                &HashMap::new(),
                &mut state,
                &mut textures,
                Lang::Fr,
            ));
        }
        assert!(ctx.memory(|m| m.has_focus(card_id)), "the card refused the keyboard focus");
        let output = output.expect("no frame");
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
        let ring = shapes.iter().any(
            |s| matches!(s, egui::Shape::Rect(r) if r.stroke.color == theme::ACCENT && r.stroke.width >= 2.0),
        );
        assert!(ring, "a focused tile draws no focus ring");
    }

    /// An empty library must draw the call to action rather than nothing at
    /// all — checked on the real widget code, not only on `empty_state`.
    #[test]
    fn an_empty_library_paints_its_call_to_action() {
        let (ctx, input) = headless_ctx();
        let mut textures = TextureStore::new();
        let mut state = LibraryUi::default();
        let output = model_frame(
            &ctx,
            input,
            &[],
            &BTreeMap::new(),
            &HashMap::new(),
            &mut state,
            &mut textures,
            Lang::Fr,
        );
        let text = painted_text(&output);
        assert!(text.contains("Aucun jeu ici."), "{text}");
        assert!(text.contains("Choisir un dossier de ROMs…"), "{text}");
    }
}
