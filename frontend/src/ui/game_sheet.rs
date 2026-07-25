//! Game sheet: everything the emulator already knows about one game, on one
//! screen — header facts read by the core (title, region, mapping, sizes,
//! battery SRAM, **detected coprocessor**), the play time accumulated by the
//! shell, the save states that exist on disk for it, and the player's own
//! screenshots.
//!
//! Nothing here is fetched from the network or from a database: every line
//! comes from `library::GameEntry` (cartridge header), `prefs.games` (play
//! time / favourite / promoted thumbnail) or the file system (states,
//! captures), which is what makes the sheet correct for any ROM the player
//! drops in the folder.
//!
//! Clicking one of the screenshots promotes it as the game's thumbnail,
//! replacing the generated one; the generated one is never deleted, so the
//! choice is reversible with one button.

use std::path::{Path, PathBuf};

use egui::{Align, Layout, RichText, Sense, Stroke, Vec2};

use crate::library::{self, GameEntry, StateFile};
use crate::prefs::GameStats;

use super::library_view::thumbnail;
use super::textures::TextureStore;
use super::theme;
use super::Action;

/// Size of the sheet's own big picture.
const HERO_W: f32 = 256.0;
const HERO_H: f32 = HERO_W * 224.0 / 256.0;
/// Size of one capture in the gallery.
const SHOT_W: f32 = 128.0;
const SHOT_H: f32 = SHOT_W * 224.0 / 256.0;

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
}

/// Draw the sheet and return what the player asked for.
pub fn show(ui: &mut egui::Ui, model: &mut SheetModel) -> Action {
    let mut action = Action::None;
    let entry = model.entry;

    ui.horizontal(|ui| {
        if ui.button("← Retour").clicked() {
            *model.selected = None;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let favorite = model.stats.favorite;
            let label = if favorite { "★ Favori" } else { "☆ Ajouter aux favoris" };
            if ui.button(RichText::new(label).color(theme::YELLOW)).clicked() {
                action = Action::ToggleFavorite(entry.id.clone());
            }
        });
    });
    ui.add_space(12.0);

    // One scroll area for the whole sheet: a small window must still reach
    // the gallery at the bottom, and a nested scroll area inside a scrolled
    // page is a well-known usability trap.
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                thumbnail(
                    ui,
                    model.picture,
                    model.pending,
                    model.textures,
                    Vec2::new(HERO_W, HERO_H),
                );
                ui.add_space(8.0);
                if super::home::primary_button(ui, "Jouer").clicked() {
                    action = Action::Launch(entry.path.clone());
                }
                if model.stats.thumbnail.is_some()
                    && ui
                        .button("Vignette générée")
                        .on_hover_text("Revenir à la miniature produite par l'émulateur")
                        .clicked()
                {
                    action = Action::ClearThumbnail(entry.id.clone());
                }
            });
            ui.add_space(20.0);
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(entry.display_title())
                        .size(theme::SIZE_TITLE)
                        .strong()
                        .color(theme::TEXT),
                );
                ui.label(
                    RichText::new(super::home::shorten_path(&entry.path, 60))
                        .size(theme::SIZE_SMALL)
                        .color(theme::TEXT_DIM),
                );
                ui.add_space(10.0);
                for (label, value) in facts(entry, model.stats) {
                    fact_row(ui, &label, &value);
                }
            });
        });

        ui.add_space(16.0);
        section(ui, "Sauvegardes d'état");
        if model.data.states.is_empty() {
            ui.label(
                RichText::new("Aucune sauvegarde d'état pour ce jeu.")
                    .size(theme::SIZE_BODY)
                    .color(theme::TEXT_DIM),
            );
        } else {
            for state in &model.data.states {
                ui.label(
                    RichText::new(format!(
                        "{} · {} · {}",
                        state.label(),
                        library::format_size(state.size),
                        library::format_date(state.modified)
                    ))
                    .size(theme::SIZE_BODY)
                    .color(theme::TEXT_DIM),
                );
            }
        }

        ui.add_space(16.0);
        section(ui, "Captures d'écran");
        if model.data.screenshots.is_empty() {
            ui.label(
                RichText::new("Aucune capture. F12 pendant une partie en enregistre une.")
                    .size(theme::SIZE_BODY)
                    .color(theme::TEXT_DIM),
            );
        } else {
            ui.label(
                RichText::new("Cliquez une capture pour en faire la vignette du jeu.")
                    .size(theme::SIZE_SMALL)
                    .color(theme::TEXT_DIM),
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                for shot in &model.data.screenshots {
                    let promoted = model.stats.thumbnail.as_deref() == Some(shot.as_path());
                    if capture(ui, shot, promoted, model.textures).clicked() {
                        action =
                            Action::SetThumbnail { id: entry.id.clone(), source: shot.clone() };
                    }
                }
            });
        }
    });

    action
}

/// One capture of the gallery; the promoted one is outlined with the accent.
fn capture(
    ui: &mut egui::Ui,
    path: &Path,
    promoted: bool,
    textures: &mut TextureStore,
) -> egui::Response {
    let response = egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(Stroke::new(1.0, if promoted { theme::ACCENT } else { theme::STROKE }))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            thumbnail(ui, Some(path), false, textures, Vec2::new(SHOT_W, SHOT_H));
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            ui.label(
                RichText::new(super::home::elide(&name, 22))
                    .size(theme::SIZE_SMALL)
                    .color(if promoted { theme::ACCENT } else { theme::TEXT_DIM }),
            );
        })
        .response
        .interact(Sense::click());
    response.on_hover_text(if promoted {
        "Vignette actuelle du jeu"
    } else {
        "Utiliser comme vignette"
    })
}

/// The header/technical facts, in display order. Split out from the drawing
/// code so the list itself is unit-testable.
pub fn facts(entry: &GameEntry, stats: &GameStats) -> Vec<(String, String)> {
    let mut out = vec![
        ("Région".to_string(), entry.region.clone()),
        (
            "Mapping".to_string(),
            format!("{}{}", entry.mapping, if entry.fastrom { " (FastROM)" } else { "" }),
        ),
        ("Taille".to_string(), library::format_size(entry.rom_bytes)),
        ("Sauvegarde".to_string(), library::format_sram(entry.sram_bytes)),
        (
            "Coprocesseur".to_string(),
            entry.coprocessor.clone().unwrap_or_else(|| "Aucun".to_string()),
        ),
        (
            "Somme de contrôle".to_string(),
            format!(
                "${:04X} ({})",
                entry.checksum,
                if entry.checksum_valid { "valide" } else { "INVALIDE" }
            ),
        ),
        ("Temps de jeu".to_string(), library::format_play_time(stats.play_seconds)),
    ];
    if let Some(last) = stats.last_played {
        out.push(("Dernière partie".to_string(), library::format_date(last)));
    }
    out
}

fn fact_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(150.0, 0.0),
            Layout::left_to_right(Align::Min),
            |ui| {
                ui.label(RichText::new(label).size(theme::SIZE_BODY).color(theme::TEXT_DIM));
            },
        );
        ui.label(RichText::new(value).size(theme::SIZE_BODY).color(theme::TEXT));
    });
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).size(theme::SIZE_HEADING).strong().color(theme::TEXT));
    ui.add_space(4.0);
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
        }
    }

    #[test]
    fn the_sheet_lists_every_header_fact_including_the_coprocessor() {
        let facts = facts(&entry(), &GameStats::default());
        let map: std::collections::BTreeMap<_, _> = facts.into_iter().collect();
        assert_eq!(map["Région"], "PAL");
        assert_eq!(map["Mapping"], "HiROM (FastROM)");
        assert_eq!(map["Taille"], "2,0 Mo");
        assert_eq!(map["Sauvegarde"], "8 Ko");
        assert_eq!(map["Coprocesseur"], "SA-1");
        assert_eq!(map["Somme de contrôle"], "$ABCD (valide)");
        assert_eq!(map["Temps de jeu"], "Jamais joué");
        // Never launched: no "last played" line at all rather than an epoch.
        assert!(!map.contains_key("Dernière partie"));
    }

    #[test]
    fn a_plain_cartridge_reports_no_coprocessor_and_no_battery() {
        let mut e = entry();
        e.coprocessor = None;
        e.sram_bytes = 0;
        e.fastrom = false;
        e.checksum_valid = false;
        let map: std::collections::BTreeMap<_, _> =
            facts(&e, &GameStats::default()).into_iter().collect();
        assert_eq!(map["Coprocesseur"], "Aucun");
        assert_eq!(map["Sauvegarde"], "Aucune");
        assert_eq!(map["Mapping"], "HiROM");
        assert_eq!(map["Somme de contrôle"], "$ABCD (INVALIDE)");
    }

    #[test]
    fn play_time_and_last_launch_come_from_the_persisted_stats() {
        let stats =
            GameStats { play_seconds: 4 * 3600 + 30 * 60, last_played: Some(1_700_000_000), ..Default::default() };
        let map: std::collections::BTreeMap<_, _> = facts(&entry(), &stats).into_iter().collect();
        assert_eq!(map["Temps de jeu"], "4 h 30");
        assert_eq!(map["Dernière partie"].len(), 16);
    }

    #[test]
    fn the_gallery_pictures_keep_the_snes_aspect_ratio() {
        assert!((HERO_W / HERO_H - 256.0 / 224.0).abs() < 1e-6);
        assert!((SHOT_W / SHOT_H - 256.0 / 224.0).abs() < 1e-6);
    }

    #[test]
    fn sheet_data_defaults_to_an_empty_unnamed_selection() {
        let data = SheetData::default();
        assert!(data.id.is_empty());
        assert!(data.states.is_empty() && data.screenshots.is_empty());
    }
}
