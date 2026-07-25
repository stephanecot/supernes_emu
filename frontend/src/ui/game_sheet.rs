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
//! Nothing here is fetched from the network or from a database: every line
//! comes from `library::GameEntry` (cartridge header), `prefs.games` (play
//! time / favourite / promoted thumbnail) or the file system (states, their
//! previews, captures), which is what makes the sheet correct for any ROM the
//! player drops in the folder.
//!
//! Clicking one of the screenshots promotes it as the game's thumbnail,
//! replacing the generated one; the generated one is never deleted, so the
//! choice is reversible with one button.

use std::path::{Path, PathBuf};

use egui::{Align, Layout, RichText, Sense, Stroke, Vec2};

use crate::i18n::{Lang, Msg};
use crate::library::{self, GameEntry, StateFile};
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
}

impl SheetData {
    /// The automatic session state, if this game has one. `StateFile::slot` is
    /// `None` for exactly that file, so the list already carries it and no
    /// second listing of the directory is needed.
    pub fn resume(&self) -> Option<&StateFile> {
        self.states.iter().find(|s| s.slot.is_none())
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
    pub textures: &'a mut TextureStore,
    /// Cleared by the `Retour` button; the shell shows the grid again.
    pub selected: &'a mut Option<String>,
    /// Save state whose deletion is armed and waiting for a confirmation.
    pub confirm_delete: &'a mut Option<PathBuf>,
    /// Language every string of the sheet is rendered in.
    pub lang: Lang,
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
                        library_view::thumbnail(
                            ui,
                            model.picture,
                            Placeholder::game_pending(model.pending),
                            model.textures,
                            Vec2::new(HERO_W, HERO_H),
                            lang,
                        );
                        ui.add_space(10.0);
                        // The call to action takes the whole column: it is the one
                        // thing a sheet exists to offer.
                        ui.scope(|ui| {
                            // Measured on the label actually rendered: `Jouer`
                            // and `Play` do not pad out to the same button.
                            ui.spacing_mut().button_padding.x =
                                (HERO_W - play_label_w(ui, lang)) / 2.0;
                            // A game whose file is gone offers the only two
                            // things that can still be done with it, in place of
                            // a `Jouer` that could only fail.
                            if entry.missing {
                                if ui
                                    .button(Msg::RelocateGame.text(lang))
                                    .on_hover_text(Msg::RelocateHint.text(lang))
                                    .clicked()
                                {
                                    action = Action::AddGame { replacing: Some(entry.path.clone()) };
                                }
                                if ui
                                    .button(Msg::ForgetGame.text(lang))
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
                                if ui
                                    .button(Msg::StartOver.text(lang))
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
                    ui.add_space(10.0);
                    if let Some(chip) = &entry.coprocessor {
                        icons::chip_badge(ui, chip);
                        ui.add_space(6.0);
                    }
                    let facts = facts(lang, entry, model.stats);
                    let label_w = fact_label_w(ui, &facts);
                    for (label, value) in &facts {
                        fact_row(ui, label, value, label_w);
                    }
                });
            });

            ui.add_space(20.0);
            super::home::heading(ui, Msg::SaveStates.text(lang));
            ui.add_space(8.0);
            if model.data.states.is_empty() {
                note(ui, Msg::NoSaveStates.text(lang));
            } else {
                for state in &model.data.states {
                    if let Some(produced) =
                        slot_row(ui, state, model.confirm_delete, model.textures, lang)
                    {
                        action = produced;
                    }
                    ui.add_space(6.0);
                }
            }

            ui.add_space(14.0);
            super::home::heading(ui, Msg::Screenshots.text(lang));
            ui.add_space(8.0);
            if model.data.screenshots.is_empty() {
                note(ui, Msg::NoScreenshots.text(lang));
            } else {
                note(ui, Msg::PromoteHint.text(lang));
                ui.add_space(6.0);
                // Laid out by hand rather than with `horizontal_wrapped`: the row
                // is what decides where a capture goes, and a cell wider than the
                // space left must start the next row instead of being clipped at
                // the right edge.
                let cell = SHOT_W + 2.0 * (SHOT_MARGIN + 1.0);
                let spacing = ui.spacing().item_spacing.x;
                let columns =
                    (((ui.available_width() + spacing) / (cell + spacing)).floor() as usize).max(1);
                for row in model.data.screenshots.chunks(columns) {
                    ui.horizontal(|ui| {
                        for shot in row {
                            let promoted = model.stats.thumbnail.as_deref() == Some(shot.as_path());
                            if capture(ui, shot, promoted, model.textures, lang).clicked() {
                                action = Action::SetThumbnail {
                                    id: entry.id.clone(),
                                    source: shot.clone(),
                                };
                            }
                        }
                    });
                }
            }
        });
    });

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

/// Width of the `Jouer`/`Play` button's own content (icon, gap and label),
/// used to pad it out to the full width of the left column.
fn play_label_w(ui: &egui::Ui, lang: Lang) -> f32 {
    let galley = ui.painter().layout_no_wrap(
        Msg::Play.text(lang).to_owned(),
        theme::font(theme::SIZE_BUTTON),
        theme::TEXT,
    );
    galley.size().x + icons::SIZE + icons::GAP
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
        data: &SheetData,
        stats: &GameStats,
        confirm: &mut Option<PathBuf>,
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
        let output = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                produced = show(
                    ui,
                    &mut SheetModel {
                        entry: &entry,
                        stats,
                        data,
                        picture: None,
                        pending: false,
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
        };
        let mut confirm = None;
        let (produced, text) = draw(&data, &GameStats::default(), &mut confirm, Lang::Fr);
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
        };
        let mut confirm = None;
        let (_, text) = draw(&data, &GameStats::default(), &mut confirm, Lang::Fr);
        assert!(text.contains("Supprimer…"), "{text}");
        assert!(!text.contains("Supprimer définitivement"), "{text}");

        // Armed (what the first click does): the row now offers the real
        // deletion and a way out of it.
        let mut confirm = Some(PathBuf::from("/roms/game.state3"));
        let (_, text) = draw(&data, &GameStats::default(), &mut confirm, Lang::Fr);
        assert!(text.contains("Supprimer définitivement"), "{text}");
        assert!(text.contains("Annuler"), "{text}");
    }

    /// An empty section is never a void: it says what would fill it.
    #[test]
    fn the_empty_sections_say_what_fills_them() {
        let mut confirm = None;
        let (_, text) = draw(&SheetData::default(), &GameStats::default(), &mut confirm, Lang::Fr);
        assert!(text.contains("Aucune sauvegarde d'état"), "{text}");
        assert!(text.contains("F5"), "{text}");
        assert!(text.contains("Aucune capture"), "{text}");
        assert!(text.contains("F12"), "{text}");
    }
}
