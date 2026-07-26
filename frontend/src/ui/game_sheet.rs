//! Game sheet: everything the emulator already knows about one game, on one
//! screen — header facts read by the core (title, region, mapping, sizes,
//! battery SRAM, **detected coprocessor**), the play time accumulated by the
//! shell, the save states that exist on disk for it *with the picture of what
//! each one holds*, and the player's own screenshots.
//!
//! Two columns: the picture and the actions on the left, the data on the
//! right, machine values in the monospace face — the distinction that says at a
//! glance which strings the cartridge wrote and which ones the application did.
//!
//! The right column is **tabbed** (`SheetTab`). The sheet had grown into one
//! long scroll — facts, catalogue, description, assistant, states, cheats,
//! captures stacked head to tail — where reaching the captures meant scrolling
//! past everything else and nothing said they were there. The four tabs answer
//! four different questions (what this game *is*, where you are in it, what you
//! can change about it, what you made of it), the bar is the shell's own
//! (`ui::tabs::bar`, spectral rule included), and the identity of the sheet —
//! picture, actions, title — sits outside them so that changing tab never looks
//! like loading another screen.
//!
//! Everything above the `Catalogue` heading comes from the machine itself:
//! `library::GameEntry` (cartridge header), `prefs.games` (play time /
//! favourite / promoted thumbnail) and the file system (states, their
//! previews, captures) — which is what makes that half of the sheet correct
//! for any ROM the player drops in the folder, with no network at all.
//!
//! Below it sits what a catalogue says (`metadata`), and the two are kept
//! visibly apart: the header facts are read off the cartridge, the catalogue
//! facts are a claim by a third party, and the description is a claim matched
//! **by title** on Wikipedia, credited and linked so a wrong match reads as a
//! wrong match rather than as an assertion by this application. That section
//! is empty until the player presses `Compléter la fiche`, which is one of the
//! two places in the whole application that opens a socket.
//!
//! Clicking one of the screenshots promotes it as the game's thumbnail,
//! replacing the generated one; the generated one is never deleted, so the
//! choice is reversible with one button.

use std::path::{Path, PathBuf};

use egui::{Align, Layout, RichText, Sense, Stroke, Vec2};

use crate::cheats::{Cheat, Kind};
use crate::i18n::{self, Lang, Msg};
use crate::library::{self, GameEntry, StateFile};
use crate::metadata::GameMeta;
use crate::prefs::GameStats;

use super::icons::{self, Icon};
use super::library_view::{self, Placeholder};
use super::textures::TextureStore;
use super::theme;
use super::Action;

/// Width of the sheet's left column: the big picture and the actions under it.
const HERO_W: f32 = 272.0;
const HERO_H: f32 = HERO_W / library_view::PICTURE_RATIO;
/// Gap between the two columns.
const COLUMN_GAP: f32 = 24.0;
/// Size of one capture in the gallery.
const SHOT_W: f32 = 128.0;
const SHOT_H: f32 = SHOT_W / library_view::PICTURE_RATIO;
/// Frame around one capture of the gallery.
const SHOT_MARGIN: f32 = 6.0;
/// Size of the picture of one save state.
const SLOT_W: f32 = 104.0;
const SLOT_H: f32 = SLOT_W / library_view::PICTURE_RATIO;
/// Frame margin of a save-state row.
const SLOT_MARGIN: f32 = 8.0;
/// Gap kept between the longest fact label and the column of values, and the
/// widest that column is ever drawn (`fact_label_w` measures the rest).
const FACT_LABEL_GAP: f32 = 24.0;
const FACT_LABEL_MAX_W: f32 = 200.0;
/// Rows the game's title may take before it is elided.
const TITLE_ROWS: usize = 2;

/// Widest the sheet is ever laid out, whatever the window. Past it the facts
/// column and the save-state rows stretched across a 1600-point window with
/// nothing but void between a label and its value: a page of text has a
/// reading width, and a wider window gives it margins instead of longer lines.
const SHEET_MAX_W: f32 = 1040.0;

/// Tooltip of the `Reprendre` button: when the session it would pick up was
/// left. A date rather than "a session exists" — the useful question is
/// whether it is the one they remember.
fn resume_hover(lang: crate::i18n::Lang, resume: &StateFile) -> String {
    crate::i18n::resume_from(lang, &library::format_date(lang, resume.modified))
}

/// Run `body` inside the sheet's reading column: at most `SHEET_MAX_W` points,
/// centred in whatever the window affords.
fn column<R>(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let available = ui.available_width();
    let width = available.min(SHEET_MAX_W);
    let indent = ((available - width) / 2.0).max(0.0).floor();
    let mut out = None;
    ui.horizontal_top(|ui| {
        ui.add_space(indent);
        ui.allocate_ui_with_layout(
            Vec2::new(width, 0.0),
            Layout::top_down(Align::Min),
            |ui| {
                ui.set_min_width(width);
                ui.set_max_width(width);
                out = Some(body(ui));
            },
        );
    });
    out.expect("the sheet column never ran its body")
}

/// Files and pictures the sheet lists, gathered once when the sheet opens
/// rather than every frame (each field is a directory listing).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SheetData {
    /// Game the data was gathered for; a mismatch with the selection is what
    /// tells the shell to refresh it.
    pub id: String,
    pub states: Vec<StateFile>,
    pub screenshots: Vec<PathBuf>,
    /// This game's `<game>.cheats.json`, read from disk with the states — the
    /// sheet is shown for games that are not running, and an agent may have
    /// written the file while the application was on the home screen.
    pub cheats: Vec<Cheat>,
}

impl SheetData {
    /// The automatic session state, if this game has one. `StateFile::slot` is
    /// `None` for exactly that file, so the list already carries it and no
    /// second listing of the directory is needed.
    pub fn resume(&self) -> Option<&StateFile> {
        self.states.iter().find(|s| s.slot.is_none())
    }
}

/// The four tabs of the sheet, in display order.
///
/// None of them ever disappears, however empty it is: a bar whose entries come
/// and go cannot be learnt, and "there is no save state yet" is a sentence
/// worth reading — it says which key writes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SheetTab {
    /// What the game *is*: the cartridge's own facts, what a catalogue claims
    /// about it, and the description. First, because someone opening a sheet
    /// wants to know what the game is before what they saved in it.
    #[default]
    About,
    /// Where the player is in it.
    States,
    /// What they can change about it: the assistant, and the cheats it found.
    Cheats,
    /// What they made of it.
    Shots,
}

impl SheetTab {
    pub const ALL: [SheetTab; 4] =
        [SheetTab::About, SheetTab::States, SheetTab::Cheats, SheetTab::Shots];

    pub fn label(self, lang: Lang) -> &'static str {
        match self {
            SheetTab::About => Msg::SheetTabAbout.text(lang),
            SheetTab::States => Msg::SaveStates.text(lang),
            SheetTab::Cheats => Msg::Cheats.text(lang),
            SheetTab::Shots => Msg::Screenshots.text(lang),
        }
    }

    /// How many things the tab leads to — what makes it worth clicking before
    /// it is clicked. `None` where a number would say nothing: the descriptive
    /// tab is prose, and an empty tab shows no `0` because the sentence waiting
    /// inside it says more than a zero could.
    pub fn count(self, data: &SheetData) -> Option<usize> {
        let held = match self {
            SheetTab::About => return None,
            SheetTab::States => data.states.len(),
            SheetTab::Cheats => data.cheats.len(),
            SheetTab::Shots => data.screenshots.len(),
        };
        (held > 0).then_some(held)
    }
}

/// Everything the sheet draws, borrowed for one UI frame.
pub struct SheetModel<'a> {
    pub entry: &'a GameEntry,
    pub stats: &'a GameStats,
    pub data: &'a SheetData,
    /// Resolved picture: the promoted screenshot if there is one, else the
    /// generated thumbnail.
    pub picture: Option<&'a Path>,
    /// The thumbnail of this game is still being generated.
    pub pending: bool,
    /// What the catalogue said about this game, `None` while nobody has asked
    /// for it. The sheet never fetches anything itself — it produces an
    /// `Action` and the shell's background thread does the work.
    pub meta: Option<&'a GameMeta>,
    /// A fetch for this game is in flight.
    pub fetching: bool,
    pub textures: &'a mut TextureStore,
    /// Cleared by the `Retour` button; the shell shows the grid again.
    pub selected: &'a mut Option<String>,
    /// Save state whose deletion is armed and waiting for a confirmation.
    pub confirm_delete: &'a mut Option<PathBuf>,
    /// Tab the sheet is open on. Lives in the shell, so it survives the frame;
    /// it is reset when a sheet is opened on another game, which is why it is
    /// not remembered per game (see `library_view::LibraryUi`).
    pub tab: &'a mut SheetTab,
    /// The frame this game's suspended session stopped on, when it *is* the
    /// running one. Shown in place of the thumbnail: someone opening the sheet
    /// of the game they are playing wants to see where they are, not a picture
    /// of its title screen.
    pub session_frame: Option<&'a [u8]>,
    /// Language every string of the sheet is rendered in.
    pub lang: Lang,
    /// The assistant may be summoned: switched on *and* its tool resolved.
    pub assistant: bool,
    /// This game is the one currently loaded — the assistant starts from the
    /// live session's state, so it has nothing to work from otherwise.
    pub is_running: bool,
    /// What the player is typing. Lives in the shell so it survives the
    /// immediate-mode rebuild of every frame.
    pub wish: &'a mut String,
    /// The assistant's latest line, while one is running.
    pub assistant_says: Option<&'a str>,
}

/// Draw the sheet and return what the player asked for.
pub fn show(ui: &mut egui::Ui, model: &mut SheetModel) -> Action {
    let mut action = Action::None;
    let entry = model.entry;
    let lang = model.lang;

    column(ui, |ui| {
        ui.horizontal(|ui| {
            // Drawn arrow, not the character `←`: no bundled face carries that
            // glyph, and the button printed a tofu box.
            if icons::button(ui, Icon::ArrowLeft, Msg::Back.text(lang)).clicked() {
                *model.selected = None;
                *model.confirm_delete = None;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let favorite = model.stats.favorite;
                let (icon, label) = if favorite {
                    (Icon::StarFilled, Msg::Favorite.text(lang))
                } else {
                    (Icon::Star, Msg::AddToFavorites.text(lang))
                };
                if icons::button_tinted(ui, icon, label, theme::YELLOW).clicked() {
                    action = Action::ToggleFavorite(entry.id.clone());
                }
            });
        });
    });
    ui.add_space(12.0);

    // One scroll area for the whole sheet: a small window must still reach
    // the gallery at the bottom, and a nested scroll area inside a scrolled
    // page is a well-known usability trap.
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        column(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(HERO_W, 0.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_min_width(HERO_W);
                        match model.session_frame {
                            Some(frame) => {
                                session_hero(ui, frame, model.textures, lang);
                            }
                            None => {
                                library_view::thumbnail(
                                    ui,
                                    model.picture,
                                    Placeholder::game_pending(model.pending),
                                    model.textures,
                                    Vec2::new(HERO_W, HERO_H),
                                    lang,
                                );
                            }
                        }
                        ui.add_space(10.0);
                        // The call to action takes the whole column: it is the one
                        // thing a sheet exists to offer.
                        ui.scope(|ui| {
                            // Every control of this column is laid out at
                            // exactly `HERO_W`, whatever its label and whatever
                            // the language: a stack of buttons of three
                            // different widths reads as an accident. The
                            // padding is the floor, `set_width` does the rest.
                            ui.spacing_mut().button_padding.x =
                                column_button_padding(ui, lang);
                            ui.set_width(HERO_W);
                            ui.style_mut().spacing.button_padding.y = 6.0;
                            // A game whose file is gone offers the only two
                            // things that can still be done with it, in place of
                            // a `Jouer` that could only fail.
                            if entry.missing {
                                if wide_button(ui, Msg::RelocateGame.text(lang))
                                    .on_hover_text(Msg::RelocateHint.text(lang))
                                    .clicked()
                                {
                                    action = Action::AddGame { replacing: Some(entry.path.clone()) };
                                }
                                if wide_button(ui, Msg::ForgetGame.text(lang))
                                    .on_hover_text(Msg::ForgetHint.text(lang))
                                    .clicked()
                                {
                                    action = Action::ForgetGame(entry.path.clone());
                                }
                            } else if let Some(resume) = model.data.resume() {
                                // A suspended session exists, so the two ways
                                // in are genuinely different and the sheet
                                // must not choose for the player: resuming is
                                // offered first (it is what they left), and
                                // starting over is a plain button beside it —
                                // never a hidden preference three screens away.
                                if icons::primary_button(ui, Icon::Play, Msg::Resume.text(lang))
                                    .on_hover_text(resume_hover(lang, resume))
                                    .clicked()
                                {
                                    action =
                                        Action::Launch { path: entry.path.clone(), resume: true };
                                }
                                if wide_button(ui, Msg::StartOver.text(lang))
                                    .on_hover_text(Msg::StartOverHint.text(lang))
                                    .clicked()
                                {
                                    action =
                                        Action::Launch { path: entry.path.clone(), resume: false };
                                }
                            } else if icons::primary_button(ui, Icon::Play, Msg::Play.text(lang))
                                .clicked()
                            {
                                action = Action::Launch { path: entry.path.clone(), resume: true };
                            }
                        });
                        if model.stats.thumbnail.is_some()
                            && ui
                                .button(Msg::GeneratedThumbnail.text(lang))
                                .on_hover_text(Msg::GeneratedThumbnailHint.text(lang))
                                .clicked()
                        {
                            action = Action::ClearThumbnail(entry.id.clone());
                        }
                        // The one control of this screen that reaches the
                        // network. Never offered for a game whose file is
                        // gone: there is no image to fingerprint, so there is
                        // nothing to look up.
                        if !entry.missing {
                            if model.fetching {
                                ui.add_space(2.0);
                                note(ui, Msg::Filling.text(lang));
                            } else {
                                let (label, hint) = if model.meta.is_some() {
                                    (Msg::CatalogRefetch, Msg::CatalogRefetchHint)
                                } else {
                                    (Msg::FillSheet, Msg::FillSheetHint)
                                };
                                if wide_button(ui, label.text(lang))
                                    .on_hover_text(hint.text(lang))
                                    .clicked()
                                {
                                    action = Action::FillSheet(entry.id.clone());
                                }
                            }
                        }
                    },
                );
                ui.add_space(COLUMN_GAP);
                ui.vertical(|ui| {
                    title(ui, &entry.display_title());
                    ui.label(
                        RichText::new(super::home::shorten_path(&entry.path, 60))
                            .font(theme::mono(theme::SIZE_SMALL))
                            .color(theme::TEXT_DIM),
                    );
                    if let Some(chip) = &entry.coprocessor {
                        ui.add_space(8.0);
                        icons::chip_badge(ui, chip);
                    }
                    ui.add_space(12.0);
                    // The bar sits beside the picture rather than under the
                    // whole identity block: the left column is 350 points tall
                    // and a bar below it would leave a tab's content two
                    // hundred points of window at 800.
                    tab_bar(ui, model);
                    ui.add_space(10.0);
                    if let Some(produced) = match *model.tab {
                        SheetTab::About => about_tab(ui, model),
                        SheetTab::States => states_tab(ui, model),
                        SheetTab::Cheats => cheats_tab(ui, model),
                        SheetTab::Shots => shots_tab(ui, model),
                    } {
                        action = produced;
                    }
                });
            });
        });
    });

    action
}

/// The sheet's own bar, drawn by the shell's widget so that it carries the
/// same spectral rule, the same hover and the same keyboard behaviour: a
/// second strip of a different shape would read as a second application.
fn tab_bar(ui: &mut egui::Ui, model: &mut SheetModel) {
    let entries: Vec<super::tabs::Entry> = SheetTab::ALL
        .iter()
        .map(|tab| {
            super::tabs::Entry::new(tab.label(model.lang)).with_count(tab.count(model.data))
        })
        .collect();
    let current = SheetTab::ALL.iter().position(|tab| *tab == *model.tab).unwrap_or(0);
    if let Some(chosen) = super::tabs::bar(ui, &entries, current) {
        *model.tab = SheetTab::ALL[chosen];
        // Leaving the list also disarms whatever deletion was armed on it: a
        // confirmation the player cannot see must not be waiting for them.
        *model.confirm_delete = None;
    }
}

/// What the game is: the cartridge's own facts, then the catalogue's claim and
/// the description.
fn about_tab(ui: &mut egui::Ui, model: &mut SheetModel) -> Option<Action> {
    let lang = model.lang;
    let cartridge = facts(lang, model.entry, model.stats);
    let catalogue = model
        .meta
        .filter(|meta| meta.matched())
        .map(|meta| crate::metadata::facts(lang, meta));

    // Side by side wherever the two fit: what the cartridge says and what a
    // catalogue claims about the same game are read against each other, and
    // stacking them made this tab a scroll of its own. Measured on the lists
    // actually rendered — the labels are not the same length in the two
    // languages, and the values are not the same length in any.
    let room = ui.available_width();
    let side_by_side = catalogue.as_ref().is_some_and(|catalogue| {
        !catalogue.is_empty()
            && facts_w(ui, &cartridge) + COLUMN_GAP + facts_w(ui, catalogue) <= room
    });
    if side_by_side {
        ui.columns(2, |columns| {
            fact_block(&mut columns[0], Msg::Cartridge.text(lang), &cartridge);
            catalog_block(&mut columns[1], model.meta, lang);
        });
    } else {
        fact_block(ui, Msg::Cartridge.text(lang), &cartridge);
        ui.add_space(18.0);
        catalog_block(ui, model.meta, lang);
    }
    ui.add_space(18.0);
    description_block(ui, model.meta, lang)
}

/// A heading and the fact list under it.
fn fact_block(ui: &mut egui::Ui, heading: &str, facts: &[(String, String)]) {
    super::home::heading(ui, heading);
    ui.add_space(8.0);
    let label_w = fact_label_w(ui, facts);
    for (label, value) in facts {
        fact_row(ui, label, value, label_w);
    }
}

/// Width a fact list needs to be drawn without wrapping: its measured label
/// column plus its widest value.
fn facts_w(ui: &egui::Ui, facts: &[(String, String)]) -> f32 {
    let font = theme::mono(theme::SIZE_MONO);
    let widest = facts
        .iter()
        .map(|(_, value)| {
            ui.painter().layout_no_wrap(value.clone(), font.clone(), theme::TEXT).size().x
        })
        .fold(0.0_f32, f32::max);
    fact_label_w(ui, facts) + widest
}

/// Where the player is in the game.
fn states_tab(ui: &mut egui::Ui, model: &mut SheetModel) -> Option<Action> {
    let lang = model.lang;
    // Copied out of the model, not borrowed from it: the rows want the texture
    // store and the armed deletion at the same time, and both live in there.
    let data = model.data;
    if data.states.is_empty() {
        note(ui, Msg::NoSaveStates.text(lang));
        return None;
    }
    let mut action = None;
    for state in &data.states {
        if let Some(produced) = slot_row(ui, state, model.confirm_delete, model.textures, lang) {
            action = Some(produced);
        }
        ui.add_space(6.0);
    }
    action
}

/// What the player can change about the game.
///
/// The request comes first and the list second, because the request is what
/// produces the list — and each keeps its own heading, so nothing here says
/// that asking for a passage to be played is cheating.
fn cheats_tab(ui: &mut egui::Ui, model: &mut SheetModel) -> Option<Action> {
    let lang = model.lang;
    let (data, entry) = (model.data, model.entry);
    let mut action = ask_section(ui, model);
    ui.add_space(18.0);
    super::home::heading(ui, Msg::Cheats.text(lang));
    ui.add_space(8.0);
    if data.cheats.is_empty() {
        note(ui, Msg::NoCheats.text(lang));
        return action;
    }
    note(ui, Msg::CheatsHint.text(lang));
    ui.add_space(6.0);
    for cheat in &data.cheats {
        if let Some(produced) = cheat_row(ui, &entry.id, cheat, lang) {
            action = Some(produced);
        }
        ui.add_space(6.0);
    }
    action
}

/// What the player made of the game.
fn shots_tab(ui: &mut egui::Ui, model: &mut SheetModel) -> Option<Action> {
    let lang = model.lang;
    let (data, entry, stats) = (model.data, model.entry, model.stats);
    if data.screenshots.is_empty() {
        note(ui, Msg::NoScreenshots.text(lang));
        return None;
    }
    let mut action = None;
    note(ui, Msg::PromoteHint.text(lang));
    ui.add_space(8.0);
    // Laid out by hand rather than with `horizontal_wrapped`: the row is what
    // decides where a capture goes, and a cell wider than the space left must
    // start the next row instead of being clipped at the right edge.
    let cell = SHOT_W + 2.0 * (SHOT_MARGIN + 1.0);
    let spacing = ui.spacing().item_spacing.x;
    let columns = (((ui.available_width() + spacing) / (cell + spacing)).floor() as usize).max(1);
    for row in data.screenshots.chunks(columns) {
        ui.horizontal(|ui| {
            for shot in row {
                let promoted = stats.thumbnail.as_deref() == Some(shot.as_path());
                if capture(ui, shot, promoted, model.textures, lang).clicked() {
                    action = Some(Action::SetThumbnail {
                        id: entry.id.clone(),
                        source: shot.clone(),
                    });
                }
            }
        });
    }
    action
}

/// The game's title, elided over at most two rows: a 60-character file name
/// must not run off the right edge of the sheet.
fn title(ui: &mut egui::Ui, text: &str) {
    let galley = library_view::elided_galley(
        ui.painter(),
        text,
        theme::strong(theme::SIZE_TITLE),
        ui.available_width(),
        TITLE_ROWS,
    );
    let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
    ui.painter().galley(rect.min, galley, theme::TEXT);
}

/// A secondary sentence of the sheet: what a section holds, or what to do when
/// it holds nothing.
fn note(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::font(theme::SIZE_BODY)).color(theme::TEXT_DIM));
}

/// What a catalogue says about this game: how it was identified and the facts
/// that came back, provenance attached.
///
/// Three states, all of them deliberate: nothing has been fetched yet, the
/// fingerprint matched nothing, or there is a sheet. None of them is a hole.
fn catalog_block(ui: &mut egui::Ui, meta: Option<&GameMeta>, lang: Lang) {
    super::home::heading(ui, Msg::Catalog.text(lang));
    ui.add_space(8.0);
    let Some(meta) = meta else {
        note(ui, Msg::NoCatalogEntry.text(lang));
        return;
    };
    if !meta.matched() {
        note(ui, Msg::NotInNoIntro.text(lang));
        return;
    }
    // The canonical name first, in the machine-data face: it is the answer the
    // fingerprint gave, and the one line that shows at a glance whether the
    // rest belongs to this cartridge.
    ui.label(RichText::new(&meta.name).font(theme::mono(theme::SIZE_MONO)).color(theme::TEXT));
    ui.add_space(8.0);
    let facts = crate::metadata::facts(lang, meta);
    if facts.is_empty() {
        note(ui, Msg::NoCatalogEntry.text(lang));
    } else {
        let label_w = fact_label_w(ui, &facts);
        for (label, value) in &facts {
            fact_row(ui, label, value, label_w);
        }
    }
    ui.add_space(6.0);
    ui.label(
        RichText::new(Msg::CatalogSource.text(lang))
            .font(theme::font(theme::SIZE_SMALL))
            .color(theme::TEXT_DIM),
    );
    if meta.boxart.is_none() {
        ui.label(
            RichText::new(Msg::NoBoxart.text(lang))
                .font(theme::font(theme::SIZE_SMALL))
                .color(theme::TEXT_DIM),
        );
    }
}

/// The paragraph a catalogue matched **by title**, credited and linked. Its own
/// block, under both fact columns: prose has a reading width that a half column
/// does not give it.
fn description_block(ui: &mut egui::Ui, meta: Option<&GameMeta>, lang: Lang) -> Option<Action> {
    let meta = meta.filter(|meta| meta.matched())?;
    super::home::heading(ui, Msg::Description.text(lang));
    ui.add_space(8.0);
    let Some(description) = &meta.description else {
        note(ui, Msg::NoDescription.text(lang));
        return None;
    };
    let mut action = None;
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(Stroke::new(1.0, theme::STROKE))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(SLOT_MARGIN as i8))
        .show(ui, |ui| {
            // The credit comes *before* the text, not as a footnote under it:
            // whoever reads the paragraph has already been told who wrote it,
            // in which language, and that the match was made on a title.
            ui.label(
                RichText::new(Msg::WikipediaCredit.text(lang))
                    .font(theme::font(theme::SIZE_SMALL))
                    .color(theme::TEXT_DIM),
            );
            if ui
                .link(
                    RichText::new(i18n::wikipedia_article(lang, &description.title))
                        .font(theme::font(theme::SIZE_SMALL)),
                )
                .on_hover_text(format!(
                    "{} · {}",
                    Msg::OpenArticle.text(lang),
                    description.url
                ))
                .clicked()
            {
                action = Some(Action::OpenUrl(description.url.clone()));
            }
            ui.add_space(8.0);
            // The paragraph is English inside a bilingual interface. Marked as
            // such rather than left to read as a translation nobody finished.
            ui.label(
                RichText::new(&description.text)
                    .font(theme::font(theme::SIZE_BODY))
                    .color(theme::TEXT),
            );
            if lang != Lang::En {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(Msg::EnglishOnly.text(lang))
                        .font(theme::font(theme::SIZE_SMALL))
                        .color(theme::TEXT_DIM),
                );
            }
        });
    action
}

/// One save state: the picture written beside it when it was saved, its slot,
/// its size and its date — and the only irreversible action of the screen,
/// which therefore takes two clicks.
fn slot_row(
    ui: &mut egui::Ui,
    state: &StateFile,
    confirm: &mut Option<PathBuf>,
    textures: &mut TextureStore,
    lang: Lang,
) -> Option<Action> {
    let mut action = None;
    let armed = confirm.as_deref() == Some(state.path.as_path());
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(Stroke::new(1.0, if armed { theme::RED } else { theme::STROKE }))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(SLOT_MARGIN as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // A slot with no picture (saved before previews existed, or
                // whose picture could not be written) shows a plate of exactly
                // the same size, never a hole of a different one.
                library_view::thumbnail(
                    ui,
                    state.preview.as_deref(),
                    Placeholder::NoPreview,
                    textures,
                    Vec2::new(SLOT_W, SLOT_H),
                    lang,
                );
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(state.label(lang))
                            .font(theme::strong(theme::SIZE_BODY))
                            .color(theme::TEXT),
                    );
                    // Size and date are machine-written: monospace and dim, so
                    // the slot's own name stays the thing that is read first.
                    ui.label(
                        RichText::new(format!(
                            "{} · {}",
                            library::format_size(lang, state.size),
                            library::format_date(lang, state.modified)
                        ))
                        .font(theme::mono(theme::SIZE_MONO))
                        .color(theme::TEXT_DIM),
                    );
                    if state.preview.is_none() {
                        ui.label(
                            RichText::new(Msg::SavedWithoutPreview.text(lang))
                                .font(theme::font(theme::SIZE_SMALL))
                                .color(theme::TEXT_DIM),
                        );
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if armed {
                        if ui.button(Msg::Cancel.text(lang)).clicked() {
                            *confirm = None;
                        }
                        let sure = ui.add(
                            egui::Button::new(
                                RichText::new(Msg::DeleteForever.text(lang))
                                    .color(theme::RED)
                                    .font(theme::font(theme::SIZE_BUTTON)),
                            )
                            .stroke(Stroke::new(1.0, theme::RED)),
                        );
                        if sure.clicked() {
                            action = Some(Action::DeleteState(state.path.clone()));
                            *confirm = None;
                        }
                    } else if ui
                        .button(Msg::Delete.text(lang))
                        .on_hover_text(Msg::DeleteHint.text(lang))
                        .clicked()
                    {
                        *confirm = Some(state.path.clone());
                    }
                });
            });
        });
    action
}

/// One cheat: a tick box carrying its name, the address and bytes it writes in
/// the machine-data face, how long it holds, and the way out.
///
/// Written for someone who ran no agent at all: the row says in words what the
/// cheat does to the game (`figée` / `une fois`), and the raw address is there
/// for whoever wants it without being the thing that is read first.
fn cheat_row(ui: &mut egui::Ui, id: &str, cheat: &Cheat, lang: Lang) -> Option<Action> {
    let mut action = None;
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(Stroke::new(1.0, theme::STROKE))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(SLOT_MARGIN as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let mut enabled = cheat.enabled;
                let toggled = ui
                    .checkbox(&mut enabled, "")
                    .on_hover_text(Msg::CheatEnabledHint.text(lang))
                    .changed();
                if toggled {
                    action = Some(Action::ToggleCheat {
                        id: id.to_string(),
                        name: cheat.name.clone(),
                        enabled,
                    });
                }
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(&cheat.name)
                            .font(theme::strong(theme::SIZE_BODY))
                            .color(if cheat.enabled { theme::TEXT } else { theme::TEXT_DIM }),
                    );
                    let (kind, kind_hint) = match cheat.kind {
                        Kind::Freeze => (Msg::CheatFrozen, Msg::CheatFrozenHint),
                        Kind::Once => (Msg::CheatOnce, Msg::CheatOnceHint),
                    };
                    ui.label(
                        RichText::new(format!(
                            "{} = {} · {}",
                            cheat.addr_text(),
                            cheat.hex(),
                            kind.text(lang)
                        ))
                        .font(theme::mono(theme::SIZE_MONO))
                        .color(theme::TEXT_DIM),
                    )
                    .on_hover_text(kind_hint.text(lang));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .button(Msg::CheatRemove.text(lang))
                        .on_hover_text(Msg::CheatRemoveHint.text(lang))
                        .clicked()
                    {
                        action =
                            Some(Action::RemoveCheat { id: id.to_string(), name: cheat.name.clone() });
                    }
                });
            });
        });
    action
}

/// One capture of the gallery; the promoted one is outlined with the accent.
fn capture(
    ui: &mut egui::Ui,
    path: &Path,
    promoted: bool,
    textures: &mut TextureStore,
    lang: Lang,
) -> egui::Response {
    let response = egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(Stroke::new(1.0, if promoted { theme::ACCENT } else { theme::STROKE }))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::same(SHOT_MARGIN as i8))
        .show(ui, |ui| {
            // The gallery is a horizontal layout, which the frame inherits:
            // without this the picture and its name would sit side by side.
            ui.vertical(|ui| {
                ui.set_width(SHOT_W);
                library_view::thumbnail(
                    ui,
                    Some(path),
                    Placeholder::NoPicture,
                    textures,
                    Vec2::new(SHOT_W, SHOT_H),
                    lang,
                );
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.label(
                    RichText::new(super::home::elide(&name, 16))
                        .font(theme::mono(theme::SIZE_SMALL))
                        .color(if promoted { theme::ACCENT } else { theme::TEXT_DIM }),
                );
            });
        })
        .response
        .interact(Sense::click());
    response.on_hover_text(if promoted {
        Msg::CurrentThumbnail.text(lang)
    } else {
        Msg::UseAsThumbnail.text(lang)
    })
}

/// The header/technical facts, in display order. Split out from the drawing
/// code so the list itself is unit-testable.
pub fn facts(lang: Lang, entry: &GameEntry, stats: &GameStats) -> Vec<(String, String)> {
    let mut out = vec![
        (Msg::FactRegion.text(lang).to_string(), entry.region.clone()),
        (
            Msg::FactMapping.text(lang).to_string(),
            format!("{}{}", entry.mapping, if entry.fastrom { " (FastROM)" } else { "" }),
        ),
        (Msg::FactSize.text(lang).to_string(), library::format_size(lang, entry.rom_bytes)),
        (Msg::FactSave.text(lang).to_string(), library::format_sram(lang, entry.sram_bytes)),
        (
            Msg::FactCoprocessor.text(lang).to_string(),
            entry
                .coprocessor
                .clone()
                .unwrap_or_else(|| Msg::NoneMasculine.text(lang).to_string()),
        ),
        (
            Msg::FactChecksum.text(lang).to_string(),
            format!(
                "${:04X} ({})",
                entry.checksum,
                if entry.checksum_valid {
                    Msg::ChecksumValid.text(lang)
                } else {
                    Msg::ChecksumInvalid.text(lang)
                }
            ),
        ),
        (
            Msg::FactPlayTime.text(lang).to_string(),
            library::format_play_time(lang, stats.play_seconds),
        ),
    ];
    if let Some(last) = stats.last_played {
        out.push((
            Msg::FactLastPlayed.text(lang).to_string(),
            library::format_date(lang, last),
        ));
    }
    out
}

/// Width of the fact column: the widest label of the list actually being
/// rendered, plus a gap. Measured rather than fixed — `Somme de contrôle` and
/// `Checksum` are not the same length, and a column sized on either of them
/// leaves a hole in one language or clips the other.
fn fact_label_w(ui: &egui::Ui, facts: &[(String, String)]) -> f32 {
    let font = theme::font(theme::SIZE_BODY);
    let widest = facts
        .iter()
        .map(|(label, _)| {
            ui.painter().layout_no_wrap(label.clone(), font.clone(), theme::TEXT_DIM).size().x
        })
        .fold(0.0_f32, f32::max);
    (widest + FACT_LABEL_GAP).ceil().min(FACT_LABEL_MAX_W)
}

/// Where the player actually talks to the assistant.
///
/// It sits immediately above `Triches` on purpose: asking for infinite lives is
/// what *produces* the list below it, and putting the request next to its
/// result is what makes the pair legible without a word of explanation.
fn ask_section(ui: &mut egui::Ui, model: &mut SheetModel) -> Option<Action> {
    let lang = model.lang;
    let mut action = None;
    super::home::heading(ui, Msg::AskHeading.text(lang));
    ui.add_space(4.0);
    note(ui, Msg::AskIntro.text(lang));
    ui.add_space(8.0);

    if !model.assistant {
        note(ui, Msg::AskDisabled.text(lang));
        return None;
    }

    // While one is running, the field is replaced by what it is saying: two
    // requests at once is not a state this can be in, and a live line is worth
    // more than a spinner — it says what the assistant is actually doing.
    if let Some(said) = model.assistant_says {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(Msg::AskWorking.text(lang))
                    .font(theme::font(theme::SIZE_BODY))
                    .color(theme::ACCENT),
            );
            if ui.button(Msg::AskStop.text(lang)).clicked() {
                action = Some(Action::StopAssistant);
            }
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new(said).font(theme::mono(theme::SIZE_SMALL)).color(theme::TEXT_DIM),
        );
        return action;
    }

    ui.add(
        egui::TextEdit::singleline(model.wish)
            .desired_width(f32::INFINITY)
            .hint_text(Msg::AskPlaceholder.text(lang))
            .font(theme::font(theme::SIZE_BODY)),
    );
    ui.add_space(8.0);

    let asked = !model.wish.trim().is_empty();
    ui.horizontal(|ui| {
        // The search starts from the session's own state: it wants the game
        // already where the thing to find happens, not at a title screen it
        // would have to navigate on its own.
        if ui
            .add_enabled(asked && model.is_running, egui::Button::new(Msg::AskFindCheat.text(lang)))
            .on_hover_text(Msg::AskCheatHint.text(lang))
            .clicked()
        {
            action = Some(Action::AskAssistant { id: model.entry.id.clone() });
        }
    });
    if asked && !model.is_running {
        ui.add_space(4.0);
        note(ui, Msg::AskNeedsSession.text(lang));
    }
    action
}

/// The running session's own frame, in the sheet's picture box, with a line
/// saying what it is.
///
/// It replaces the thumbnail rather than sitting beside it: a sheet has one
/// picture box, and of the two pictures the one showing where the player
/// actually is beats a generated shot of the title screen.
fn session_hero(
    ui: &mut egui::Ui,
    frame: &[u8],
    textures: &mut TextureStore,
    lang: Lang,
) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(HERO_W, HERO_H), Sense::hover());
    let key = Path::new(TextureStore::SESSION_FRAME);
    let size = [crate::SCREEN_WIDTH, crate::SCREEN_HEIGHT];
    let texture = textures.frame(ui.ctx(), key, frame, size).map(|t| t.id());
    match texture {
        Some(id) => {
            ui.painter().image(
                id,
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        None => {
            ui.painter().rect_filled(rect, 4.0, theme::BG_DEEP);
        }
    }
    ui.add_space(6.0);
    ui.label(
        RichText::new(Msg::SessionStillOn.text(lang))
            .font(theme::font(theme::SIZE_SMALL))
            .color(theme::GREEN),
    );
}

/// A plain button spanning the whole left column.
fn wide_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let height = ui.spacing().interact_size.y.max(28.0);
    ui.add_sized(Vec2::new(HERO_W, height), egui::Button::new(label))
}

/// Padding that makes every button of the left column exactly `HERO_W` wide.
///
/// Measured on the **widest** label the column can show, not on `Jouer`: one
/// padding is set for the whole scope, so sizing it on the shortest label
/// pushed `Nouvelle partie` past the column and wrapped it onto two lines.
/// English hid the bug — `New game` fits where `Nouvelle partie` does not.
fn column_button_padding(ui: &egui::Ui, lang: Lang) -> f32 {
    let widest = [
        (Msg::Play, true),
        (Msg::Resume, true),
        (Msg::StartOver, false),
        (Msg::RelocateGame, false),
        (Msg::ForgetGame, false),
        (Msg::GeneratedThumbnail, false),
    ]
    .into_iter()
    .map(|(msg, icon)| {
        let galley = ui.painter().layout_no_wrap(
            msg.text(lang).to_owned(),
            theme::font(theme::SIZE_BUTTON),
            theme::TEXT,
        );
        galley.size().x + if icon { icons::SIZE + icons::GAP } else { 0.0 }
    })
    .fold(0.0_f32, f32::max);
    // A floor rather than a negative padding on a very narrow column.
    ((HERO_W - widest) / 2.0).max(6.0)
}

/// One header fact: its name in the interface face, its value in the
/// machine-data face.
fn fact_row(ui: &mut egui::Ui, label: &str, value: &str, label_w: f32) {
    ui.horizontal(|ui| {
        // Both cells are centred on the row: the two faces have different
        // ascents, and top-aligning them would print the value a few points
        // above its own label.
        ui.allocate_ui_with_layout(
            Vec2::new(label_w, 0.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                // `allocate_ui_with_layout` shrinks back to what its content
                // used, so the column width has to be claimed from inside —
                // without this every value would start right after its own
                // label and no two rows would line up.
                ui.set_min_width(label_w);
                ui.label(
                    RichText::new(label)
                        .font(theme::font(theme::SIZE_BODY))
                        .color(theme::TEXT_DIM),
                );
            },
        );
        ui.label(RichText::new(value).font(theme::mono(theme::SIZE_MONO)).color(theme::TEXT));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The request block was once drawn twice, because a move left the first
    /// call behind. A section that appears twice is not a layout quibble: it
    /// makes the reader wonder which of the two is the real one.
    #[test]
    fn the_request_block_is_drawn_exactly_once() {
        let source = include_str!("game_sheet.rs");
        let body = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert_eq!(body.matches("ask_section(ui, model)").count(), 1, "{}", "drawn twice");
    }

    fn entry() -> GameEntry {
        GameEntry {
            id: "GAME-1234".to_string(),
            path: PathBuf::from("/roms/game.sfc"),
            file_size: 1024,
            modified: 0,
            title: "SUPER GAME".to_string(),
            mapping: "HiROM".to_string(),
            region: "PAL".to_string(),
            rom_bytes: 2 * 1024 * 1024,
            sram_bytes: 8192,
            coprocessor: Some("SA-1".to_string()),
            fastrom: true,
            checksum: 0xABCD,
            checksum_valid: true,
            crc32: 0x5641_0E5E,
            missing: false,
        }
    }

    fn state_file(slot: Option<u8>, preview: Option<PathBuf>) -> StateFile {
        StateFile {
            slot,
            path: PathBuf::from("/roms/game.state3"),
            size: 541_312,
            modified: 1_768_478_400,
            preview,
        }
    }

    #[test]
    fn the_sheet_lists_every_header_fact_including_the_coprocessor() {
        let map: std::collections::BTreeMap<_, _> =
            facts(Lang::Fr, &entry(), &GameStats::default()).into_iter().collect();
        assert_eq!(map["Région"], "PAL");
        assert_eq!(map["Mapping"], "HiROM (FastROM)");
        assert_eq!(map["Taille"], "2,0 Mo");
        assert_eq!(map["Sauvegarde"], "8 Ko");
        assert_eq!(map["Coprocesseur"], "SA-1");
        assert_eq!(map["Somme de contrôle"], "$ABCD (valide)");
        assert_eq!(map["Temps de jeu"], "Jamais joué");
        // Never launched: no "last played" line at all rather than an epoch.
        assert!(!map.contains_key("Dernière partie"));

        // The same facts in English: the chip name is a chip name, the region
        // a machine value, and everything around them is translated.
        let map: std::collections::BTreeMap<_, _> =
            facts(Lang::En, &entry(), &GameStats::default()).into_iter().collect();
        assert_eq!(map["Region"], "PAL");
        assert_eq!(map["Size"], "2.0 MB");
        assert_eq!(map["Battery save"], "8 KB");
        assert_eq!(map["Coprocessor"], "SA-1");
        assert_eq!(map["Checksum"], "$ABCD (valid)");
        assert_eq!(map["Play time"], "Never played");
    }

    #[test]
    fn a_plain_cartridge_reports_no_coprocessor_and_no_battery() {
        let mut e = entry();
        e.coprocessor = None;
        e.sram_bytes = 0;
        e.fastrom = false;
        e.checksum_valid = false;
        let map: std::collections::BTreeMap<_, _> =
            facts(Lang::Fr, &e, &GameStats::default()).into_iter().collect();
        assert_eq!(map["Coprocesseur"], "Aucun");
        assert_eq!(map["Sauvegarde"], "Aucune");
        assert_eq!(map["Mapping"], "HiROM");
        assert_eq!(map["Somme de contrôle"], "$ABCD (INVALIDE)");
        let map: std::collections::BTreeMap<_, _> =
            facts(Lang::En, &e, &GameStats::default()).into_iter().collect();
        assert_eq!(map["Coprocessor"], "None");
        assert_eq!(map["Battery save"], "None");
        assert_eq!(map["Checksum"], "$ABCD (INVALID)");
    }

    #[test]
    fn play_time_and_last_launch_come_from_the_persisted_stats() {
        let stats =
            GameStats { play_seconds: 4 * 3600 + 30 * 60, last_played: Some(1_700_000_000), ..Default::default() };
        let map: std::collections::BTreeMap<_, _> =
            facts(Lang::Fr, &entry(), &stats).into_iter().collect();
        assert_eq!(map["Temps de jeu"], "4 h 30");
        assert_eq!(map["Dernière partie"].len(), 16);
    }

    #[test]
    fn every_picture_of_the_sheet_keeps_the_snes_aspect_ratio() {
        for (w, h) in [(HERO_W, HERO_H), (SHOT_W, SHOT_H), (SLOT_W, SLOT_H)] {
            assert!((w / h - library_view::PICTURE_RATIO).abs() < 1e-4, "{w}x{h}");
        }
    }

    /// A sheet laid out across a 1900-point window put a label at x = 330 and
    /// its value at x = 490, with a thousand points of nothing to their right.
    /// The page has a reading width, and a wider window gives it margins.
    #[test]
    fn the_sheet_is_laid_out_in_a_centred_reading_column() {
        for window in [700.0_f32, 1280.0, 1900.0] {
            let ctx = egui::Context::default();
            theme::apply(&ctx);
            let mut measured = None;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(window, 800.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ctx, |ui| {
                            column(ui, |ui| {
                                measured = Some((ui.max_rect().left(), ui.max_rect().width()));
                            });
                        });
                },
            );
            let (left, width) = measured.expect("the column never ran");
            assert!(width <= SHEET_MAX_W + 0.5, "{window}: {width} points wide");
            assert!(width <= window + 0.5, "{window}: {width} points wide");
            // Centred once the window is wider than the column.
            let expected_left = ((window - width) / 2.0).max(0.0).floor();
            assert!((left - expected_left).abs() < 1.5, "{window}: left {left}");
        }
    }

    #[test]
    fn sheet_data_defaults_to_an_empty_unnamed_selection() {
        let data = SheetData::default();
        assert!(data.id.is_empty());
        assert!(data.states.is_empty() && data.screenshots.is_empty());
    }

    /// One headless frame of the sheet, returning what it asked for and every
    /// string it painted.
    fn draw(
        tab: SheetTab,
        data: &SheetData,
        stats: &GameStats,
        confirm: &mut Option<PathBuf>,
        lang: Lang,
    ) -> (Action, String) {
        draw_with(tab, data, stats, confirm, None, lang)
    }

    /// The same, with a catalogue sheet in place.
    fn draw_with(
        tab: SheetTab,
        data: &SheetData,
        stats: &GameStats,
        confirm: &mut Option<PathBuf>,
        meta: Option<&GameMeta>,
        lang: Lang,
    ) -> (Action, String) {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let entry = entry();
        let mut textures = TextureStore::new();
        let mut selected = Some(entry.id.clone());
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 1400.0),
            )),
            ..Default::default()
        };
        let mut produced = Action::Quit;
        let mut tab = tab;
        let output = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                produced = show(
                    ui,
                    &mut SheetModel {
                tab: &mut tab,
                session_frame: None,
                assistant: true,
                is_running: true,
                wish: &mut String::new(),
                assistant_says: None,
                        entry: &entry,
                        stats,
                        data,
                        picture: None,
                        pending: false,
                        meta,
                        fetching: false,
                        textures: &mut textures,
                        selected: &mut selected,
                        confirm_delete: confirm,
                        lang,
                    },
                );
            });
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

    /// The slot list is the sheet's own screen: every state must show its
    /// name, its size, its date — and say so when it carries no picture.
    #[test]
    fn every_save_state_is_listed_with_its_facts() {
        let data = SheetData {
            id: "GAME-1234".to_string(),
            states: vec![
                state_file(None, Some(PathBuf::from("/roms/game.resume.png"))),
                state_file(Some(3), None),
            ],
            screenshots: Vec::new(),
            cheats: Vec::new(),
        };
        let mut confirm = None;
        let (produced, text) = draw(SheetTab::States, &data, &GameStats::default(), &mut confirm, Lang::Fr);
        assert_eq!(produced, Action::None, "drawing alone must delete nothing");
        assert!(text.contains("Reprise"), "{text}");
        assert!(text.contains("Slot 3"), "{text}");
        assert!(text.contains("528 Ko"), "{text}");
        // The state with no picture says so rather than showing a hole.
        assert!(text.contains("Sauvegardé sans aperçu"), "{text}");
        assert_eq!(confirm, None, "nothing may be armed by a plain draw");
    }

    /// Deleting a state is irreversible, so it is never one click: the first
    /// arms the row, and only the armed row offers the destructive button.
    #[test]
    fn deleting_a_state_asks_for_a_confirmation_first() {
        let data = SheetData {
            id: "GAME-1234".to_string(),
            states: vec![state_file(Some(3), None)],
            screenshots: Vec::new(),
            cheats: Vec::new(),
        };
        let mut confirm = None;
        let (_, text) = draw(SheetTab::States, &data, &GameStats::default(), &mut confirm, Lang::Fr);
        assert!(text.contains("Supprimer…"), "{text}");
        assert!(!text.contains("Supprimer définitivement"), "{text}");

        // Armed (what the first click does): the row now offers the real
        // deletion and a way out of it.
        let mut confirm = Some(PathBuf::from("/roms/game.state3"));
        let (_, text) = draw(SheetTab::States, &data, &GameStats::default(), &mut confirm, Lang::Fr);
        assert!(text.contains("Supprimer définitivement"), "{text}");
        assert!(text.contains("Annuler"), "{text}");
    }

    /// The cheats a search found are the player's to keep, switch off or drop —
    /// and the row has to be readable by someone who never ran an agent.
    #[test]
    fn every_cheat_is_listed_with_what_it_does_to_the_game() {
        let data = SheetData {
            id: "GAME-1234".to_string(),
            cheats: vec![
                Cheat::new("Vies infinies".into(), "7E:0DBE", "63", Kind::Freeze, true).unwrap(),
                Cheat::new("Pièces".into(), "7E:0DBF", "63", Kind::Once, false).unwrap(),
            ],
            ..Default::default()
        };
        let mut confirm = None;
        let (produced, text) = draw(SheetTab::Cheats, &data, &GameStats::default(), &mut confirm, Lang::Fr);
        assert_eq!(produced, Action::None, "drawing alone must change nothing");
        assert!(text.contains("Triches"), "{text}");
        assert!(text.contains("Vies infinies"), "{text}");
        // The address and the payload are there, in the machine-data face…
        assert!(text.contains("7E:0DBE = 63"), "{text}");
        // …and so is the word that says what it *does*, which is the part a
        // player who ran no agent needs.
        assert!(text.contains("figée"), "{text}");
        assert!(text.contains("une fois"), "{text}");
        assert!(text.contains("Retirer"), "{text}");

        let (_, text) = draw(SheetTab::Cheats, &data, &GameStats::default(), &mut confirm, Lang::En);
        assert!(text.contains("Cheats"), "{text}");
        assert!(text.contains("frozen"), "{text}");
        assert!(text.contains("once"), "{text}");
        assert!(text.contains("Remove"), "{text}");
    }

    fn filled_meta() -> GameMeta {
        GameMeta {
            crc32: 0x5641_0E5E,
            name: "Super Mario Kart (Europe)".to_string(),
            genre: Some("Racing".to_string()),
            developer: Some("Nintendo".to_string()),
            players: Some("2".to_string()),
            year: Some("1993".to_string()),
            month: Some("1".to_string()),
            description: Some(crate::metadata::Description {
                text: "Super Mario Kart is a 1992 kart racing game developed and published by \
                       Nintendo for the Super Nintendo Entertainment System."
                    .to_string(),
                title: "Super Mario Kart".to_string(),
                url: "https://en.wikipedia.org/wiki/Super_Mario_Kart".to_string(),
            }),
            boxart: Some(PathBuf::from("/box/GAME-1234.png")),
            fetched: 1_768_478_400,
            ..Default::default()
        }
    }

    /// The catalogue block, when there is one: the facts, the source of the
    /// facts, and — separately — the description with its attribution.
    #[test]
    fn a_fetched_sheet_shows_its_facts_and_credits_the_description() {
        let meta = filled_meta();
        let mut confirm = None;
        let (produced, text) =
            draw_with(SheetTab::About, &SheetData::default(), &GameStats::default(), &mut confirm, Some(&meta), Lang::Fr);
        assert_eq!(produced, Action::None, "drawing alone must fetch nothing");
        // The canonical name the fingerprint resolved to is on screen: it is
        // what makes a wrong match visible at a glance.
        assert!(text.contains("Super Mario Kart (Europe)"), "{text}");
        assert!(text.contains("Racing"), "{text}");
        assert!(text.contains("01/1993"), "{text}");
        assert!(text.contains("No-Intro"), "{text}");
        // The attribution is not optional and not a footnote: the licence
        // asks for it, and a title-matched description has to say so.
        assert!(text.contains("Wikipédia"), "{text}");
        assert!(text.contains("CC BY-SA"), "{text}");
        assert!(text.contains("Super Mario Kart"), "{text}");
        // …and the French interface says the paragraph is in English rather
        // than letting it read as a translation nobody finished.
        assert!(text.contains("en anglais"), "{text}");
        // The refresh button replaces the "fill it in" one once there is a
        // sheet, so the network button never reads as "nothing happened".
        assert!(text.contains("Actualiser la fiche"), "{text}");

        let (_, text) =
            draw_with(SheetTab::About, &SheetData::default(), &GameStats::default(), &mut confirm, Some(&meta), Lang::En);
        assert!(text.contains("Wikipedia"), "{text}");
        assert!(text.contains("1993-01"), "{text}");
        assert!(text.contains("Refresh the sheet"), "{text}");
    }

    /// A dump no catalogue knows is a normal outcome, and it has to read as a
    /// decision rather than as a blank.
    #[test]
    fn a_dump_that_is_in_no_catalogue_says_so_instead_of_showing_a_hole() {
        let unmatched = GameMeta { crc32: 0xDEAD_BEEF, ..Default::default() };
        let mut confirm = None;
        let (_, text) = draw_with(
            SheetTab::About,
            &SheetData::default(),
            &GameStats::default(),
            &mut confirm,
            Some(&unmatched),
            Lang::Fr,
        );
        assert!(text.contains("No-Intro"), "{text}");
        assert!(text.contains("traduction amateur"), "{text}");
        // No empty facts, and no description block promising a description.
        assert!(!text.contains("Wikipédia"), "{text}");
    }

    /// Before anybody asks, the section says what would fill it — and the
    /// button that would is the one thing on the screen that reaches the
    /// network, so it is named plainly.
    #[test]
    fn an_unfetched_sheet_offers_the_button_and_nothing_else() {
        let mut confirm = None;
        let (produced, text) =
            draw_with(SheetTab::About, &SheetData::default(), &GameStats::default(), &mut confirm, None, Lang::Fr);
        assert_eq!(produced, Action::None);
        assert!(text.contains("Catalogue"), "{text}");
        assert!(text.contains("Compléter la fiche…"), "{text}");
        assert!(!text.contains("Actualiser la fiche"), "{text}");
        let (_, text) =
            draw_with(SheetTab::About, &SheetData::default(), &GameStats::default(), &mut confirm, None, Lang::En);
        assert!(text.contains("Fill in the sheet…"), "{text}");
    }

    /// An empty tab is never a void and never absent: the bar keeps all four
    /// entries, and the one that is empty says what would fill it.
    #[test]
    fn an_empty_tab_stays_in_the_bar_and_says_what_fills_it() {
        let empty = SheetData::default();
        let mut confirm = None;
        let expected = [
            (SheetTab::States, "Aucune sauvegarde d'état", "F5"),
            (SheetTab::Shots, "Aucune capture", "F12"),
            (SheetTab::Cheats, "Aucune triche", "assistant"),
        ];
        for (tab, sentence, hint) in expected {
            let (_, text) = draw(tab, &empty, &GameStats::default(), &mut confirm, Lang::Fr);
            assert!(text.contains(sentence), "{tab:?}: {text}");
            assert!(text.contains(hint), "{tab:?}: {text}");
            // …and every other tab is still one click away from it.
            for other in SheetTab::ALL {
                assert!(text.contains(other.label(Lang::Fr)), "{other:?} left the bar: {text}");
            }
        }
    }

    /// The bar the sheet draws is the shell's own, so its four entries have to
    /// be reachable, distinct, and counted where a count means something.
    #[test]
    fn every_tab_of_the_sheet_is_reachable_and_named_in_both_languages() {
        assert_eq!(SheetTab::default(), SheetTab::About, "a sheet opens on what the game is");
        for lang in Lang::ALL {
            let labels: Vec<&str> = SheetTab::ALL.iter().map(|t| t.label(lang)).collect();
            let mut unique = labels.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(unique.len(), labels.len(), "two tabs read alike in {lang}: {labels:?}");
            assert!(labels.iter().all(|l| !l.trim().is_empty()));

            // Each tab actually draws its own content, and the bar under it.
            let mut confirm = None;
            for tab in SheetTab::ALL {
                let (produced, text) =
                    draw(tab, &populated(), &GameStats::default(), &mut confirm, lang);
                assert_eq!(produced, Action::None, "{tab:?}: drawing alone must ask nothing");
                for entry in SheetTab::ALL {
                    assert!(text.contains(entry.label(lang)), "{tab:?}: {entry:?} missing");
                }
                let own = match tab {
                    SheetTab::About => Msg::Catalog.text(lang),
                    SheetTab::States => "Slot 3",
                    SheetTab::Cheats => Msg::AskHeading.text(lang),
                    SheetTab::Shots => Msg::PromoteHint.text(lang),
                };
                assert!(text.contains(own), "{tab:?} drew none of its own content: {text}");
            }
        }
    }

    /// The count is what makes a tab worth clicking before it is clicked — and
    /// a tab holding nothing shows no `0`: the sentence inside it says more.
    #[test]
    fn a_tab_counts_what_it_holds_and_an_empty_one_counts_nothing() {
        let data = populated();
        assert_eq!(SheetTab::About.count(&data), None, "prose is not counted");
        assert_eq!(SheetTab::States.count(&data), Some(2));
        assert_eq!(SheetTab::Cheats.count(&data), Some(1));
        assert_eq!(SheetTab::Shots.count(&data), Some(3));
        for tab in SheetTab::ALL {
            assert_eq!(tab.count(&SheetData::default()), None, "{tab:?} counts an empty sheet");
        }
        // The numbers reach the bar: a count kept in the model and never drawn
        // would pass every assertion above.
        let mut confirm = None;
        let (_, text) = draw(SheetTab::About, &data, &GameStats::default(), &mut confirm, Lang::Fr);
        for n in ["2", "1", "3"] {
            assert!(text.contains(n), "the bar shows no count: {text}");
        }
    }

    /// Two states, one cheat, three captures — counts that cannot be confused
    /// with one another.
    fn populated() -> SheetData {
        SheetData {
            id: "GAME-1234".to_string(),
            states: vec![state_file(None, None), state_file(Some(3), None)],
            screenshots: (0..3).map(|i| PathBuf::from(format!("/roms/shot{i}.png"))).collect(),
            cheats: vec![
                Cheat::new("Vies infinies".into(), "7E:0DBE", "63", Kind::Freeze, true).unwrap(),
            ],
        }
    }
}
