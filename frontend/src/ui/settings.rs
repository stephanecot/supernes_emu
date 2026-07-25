//! `Réglages` — the settings panel, reachable from both screens.
//!
//! This is where every user option lives now: the native macOS menu keeps the
//! *actions* (open a ROM, reset, save/load state, screenshot, fullscreen) and
//! no longer carries a single setting, so there is exactly one place to change
//! one and no two widgets to keep in agreement.
//!
//! There is also no mirrored state: the panel is rebuilt from `prefs` on every
//! frame (immediate mode) and never writes into it — a change is returned as
//! `Action::Set(Setting)` and applied by the event loop through the very same
//! `App::set_*` method a keyboard hotkey calls. Preferences are therefore the
//! single source of truth, and the panel cannot drift from the hotkeys, from
//! the menu, or from a value edited by hand in `prefs.json`.
//!
//! Fullscreen is the one exception on purpose: it is a live window state, read
//! from winit each frame rather than from `prefs` (see
//! `video::App::set_fullscreen` for why it is not persisted).

use std::path::{Path, PathBuf};

use egui::{Align, Layout, RichText, Vec2};

use crate::input::{self, Capture, Device};
use crate::pad;
use crate::prefs::{Prefs, FAST_FORWARD_FACTORS};
use crate::render::{Aspect, Filter};
use crate::state::SLOT_COUNT;

use super::theme;
use super::{Action, Setting};

/// Window-size shortcuts offered by the display section, with their label.
/// The window itself stays freely resizable — these only set a size (see
/// `render::zoomed_dims`), which is why the section says so.
pub const ZOOM_CHOICES: &[(u8, &str)] = &[(1, "×1"), (2, "×2"), (3, "×3"), (4, "×4")];

/// Display filters, in `render::Filter::next`'s cycle order (the `V` hotkey).
pub const FILTER_CHOICES: &[(Filter, &str)] =
    &[(Filter::None, "Aucun"), (Filter::Smooth, "Lissé"), (Filter::Crt, "CRT")];

/// Pixel-aspect-ratio modes (the `R` hotkey toggles between the two).
pub const ASPECT_CHOICES: &[(Aspect, &str)] =
    &[(Aspect::PixelPerfect, "Pixel-parfait (1:1)"), (Aspect::Tv, "TV authentique (8:7)")];

/// Width of the panel's content area, in points. Wide enough for a folder path
/// on one line, narrow enough to fit a ×1 window (256 px) side of the screen
/// only partly — the modal is clamped to the window by egui itself.
const PANEL_W: f32 = 560.0;
/// Width of the section list on the left.
const NAV_W: f32 = 150.0;
/// Width reserved for a setting's label, so the controls of a section line up.
const LABEL_W: f32 = 190.0;
/// Width of the button-name column of the bindings list (`Entrées`).
const BUTTON_COL_W: f32 = 70.0;
/// Width of its keyboard column; the controller column takes what is left,
/// since its labels are the longest ("Gâchette L2 (LT)").
const BIND_COL_W: f32 = 130.0;
/// Longest folder path shown before its middle is elided.
const PATH_MAX_CHARS: usize = 52;
/// Narrowest the panel is ever drawn; a window narrower than this shows it
/// clipped rather than an unreadable column of controls.
const MIN_PANEL_W: f32 = 300.0;
/// Narrowest the controls column is ever drawn.
const MIN_CONTENT_W: f32 = 130.0;
/// Height of the content area, enough for the tallest section (Dossiers, whose
/// three folders each carry a path, two buttons and an explanation) not to
/// scroll. Fixed rather than content-driven so the modal keeps one size when the
/// player walks the section list.
const CONTENT_H: f32 = 470.0;
/// Vertical space the panel's own chrome takes outside the content area (title
/// row, separators, frame margins), subtracted from the window height before
/// `CONTENT_H` is clamped to it.
const CHROME_H: f32 = 130.0;
/// Content height below which the panel would be unreadable; a smaller window
/// scrolls instead.
const MIN_CONTENT_H: f32 = 120.0;

/// The panel's sections, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    Display,
    Audio,
    Inputs,
    Emulation,
    Folders,
    About,
}

impl Section {
    pub const ALL: [Section; 6] = [
        Section::Display,
        Section::Audio,
        Section::Inputs,
        Section::Emulation,
        Section::Folders,
        Section::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Display => "Affichage",
            Section::Audio => "Audio",
            Section::Inputs => "Entrées",
            Section::Emulation => "Émulation",
            Section::Folders => "Dossiers",
            Section::About => "À propos",
        }
    }
}

/// Name shown for a SNES button in the bindings list. The four directions are
/// named in French; the eight others carry the legend printed on a real SNES
/// pad, which is also what the `--script` contract calls them.
pub fn button_label(name: &str) -> &'static str {
    match name {
        "Up" => "Haut",
        "Down" => "Bas",
        "Left" => "Gauche",
        "Right" => "Droite",
        "A" => "A",
        "B" => "B",
        "X" => "X",
        "Y" => "Y",
        "L" => "L",
        "R" => "R",
        "Start" => "Start",
        "Select" => "Select",
        _ => "?",
    }
}

/// Text a binding cell shows: the current binding, or the prompt while that
/// very cell is waiting for a press.
pub fn binding_cell(current: &str, capturing: bool, device: Device) -> String {
    if !capturing {
        return current.to_string();
    }
    match device {
        Device::Keyboard => "Appuyez sur une touche…".to_string(),
        Device::Gamepad => "Appuyez sur un bouton…".to_string(),
    }
}

/// A line the `Dossiers` section shows after a folder change. Two kinds, drawn
/// apart: a refusal is a failure the player must act on, a remark is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderNotice {
    /// Nothing was changed and here is why (unusable folder).
    Error(String),
    /// The change was applied; this says when it shows.
    Info(String),
}

impl FolderNotice {
    pub fn text(&self) -> &str {
        match self {
            FolderNotice::Error(t) | FolderNotice::Info(t) => t,
        }
    }
}

/// View state of the panel: what is open and which section is selected. Not
/// persisted — the panel always opens on the first section.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SettingsUi {
    pub open: bool,
    pub section: Section,
    /// Pedagogical PDF located when the panel was opened (`crate::guide`);
    /// `None` when this build has no copy next to it.
    pub guide: Option<PathBuf>,
    /// Last failure worth showing in place (guide that would not open).
    pub notice: Option<String>,
    /// What to say about the last folder change, in the section that made it
    /// rather than only on stderr: a folder that could not be created or
    /// written to (the preference is then left alone), or when a change will
    /// take effect.
    pub folder_notice: Option<FolderNotice>,
    /// "Press a key…" state of the Entrées section. Lives here rather than in
    /// the widget because the press that ends a capture is intercepted by the
    /// event loop, *before* the application's own shortcuts
    /// (`video::App::handle_key`) — otherwise F11 pressed in capture would go
    /// fullscreen instead of being assigned.
    pub capture: Capture,
}

/// Everything the panel displays, borrowed for one UI frame.
pub struct SettingsModel<'a> {
    pub app_name: &'a str,
    pub version: &'a str,
    /// Read-only: the panel proposes changes, the event loop applies them.
    pub prefs: &'a Prefs,
    /// Live window state, not a preference.
    pub fullscreen: bool,
    /// Folder the library actually scans, already resolved through its
    /// fallbacks (`library::library_dir`).
    pub library_dir: &'a Path,
    /// Where `prefs.json` and the derived caches live, for the About section.
    pub config_dir: Option<&'a Path>,
    pub state: &'a mut SettingsUi,
}

/// Panel width and section-list width for a window `window_w` points wide. The
/// section list gives ground first, since the controls are what the player
/// came for.
pub fn panel_dims(window_w: f32) -> (f32, f32) {
    let panel = (window_w - 32.0).clamp(MIN_PANEL_W, PANEL_W);
    let nav = NAV_W.min(panel * 0.34);
    (panel, nav)
}

/// Whether Escape should close the panel rather than act on the screen behind
/// it. Fullscreen keeps precedence, exactly like the game sheet on the home
/// screen: Escape backs out of the window mode first, then out of overlays
/// (see `ui::app_state::escape_action`).
pub fn escape_closes_settings(open: bool, fullscreen: bool) -> bool {
    open && !fullscreen
}

/// Draw the panel as a modal over whichever screen owns the window, and return
/// what the player asked for.
pub fn show(ctx: &egui::Context, model: &mut SettingsModel) -> Action {
    let mut action = Action::None;
    // While the Entrées section waits for a press, the key belongs to the
    // binding and to nothing else: dropping the key events before any widget
    // is built stops a focused button from treating Space/Enter as a click
    // (the event loop routes the press to `input::Capture` instead — see
    // `video::App::handle_key`).
    if model.state.capture.is_active() {
        ctx.input_mut(|input| input.events.retain(|e| !matches!(e, egui::Event::Key { .. })));
    }
    let response = egui::Modal::new(egui::Id::new("prisme-settings"))
        .frame(
            egui::Frame::new()
                .fill(theme::BG_PANEL)
                .stroke(egui::Stroke::new(1.0, theme::STROKE))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::same(18)),
        )
        .show(ctx, |ui| {
            // Narrowed with the window: at ×1/×2 zoom the window is 256/512
            // points wide, which the panel's natural width would overflow on
            // both sides (the modal is centred).
            let (panel_w, nav_w) = panel_dims(ctx.content_rect().width());
            ui.set_width(panel_w);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Réglages").size(theme::SIZE_TITLE).strong().color(theme::TEXT),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Fermer (Échap)").clicked() {
                        action = Action::CloseSettings;
                    }
                });
            });
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(10.0);

            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(nav_w, 0.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        for section in Section::ALL {
                            let selected = model.state.section == section;
                            if ui
                                .selectable_label(selected, RichText::new(section.label()))
                                .clicked()
                            {
                                model.state.section = section;
                                // Leaving the bindings list abandons whatever
                                // it was waiting for: a capture left pending
                                // would keep swallowing keys on a section that
                                // does not show it.
                                model.state.capture.cancel();
                            }
                        }
                    },
                );
                ui.separator();
                // The content area is allocated at an explicit size rather
                // than "whatever is left": a modal is auto-sized from what it
                // laid out on the *previous* frame, so a scroll area that
                // claims the available height keeps re-measuring the height it
                // already has and never grows past it (the tallest sections
                // would stay clipped whatever the window size). Clamped to the
                // window so a ×1 window still shows the title and the section
                // list, and scrolls the rest.
                let content_h =
                    (ctx.content_rect().height() - CHROME_H).clamp(MIN_CONTENT_H, CONTENT_H);
                let content_w = (panel_w - nav_w - 24.0).max(MIN_CONTENT_W);
                ui.allocate_ui_with_layout(
                    Vec2::new(content_w, content_h),
                    Layout::top_down(Align::Min),
                    |ui| {
                        egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                            ui.set_min_width(content_w);
                            let produced = match model.state.section {
                                Section::Display => display_section(ui, model),
                                Section::Audio => audio_section(ui, model),
                                Section::Inputs => inputs_section(ui, model),
                                Section::Emulation => emulation_section(ui, model),
                                Section::Folders => folders_section(ui, model),
                                Section::About => about_section(ui, model),
                            };
                            if produced != Action::None {
                                action = produced;
                            }
                        });
                    },
                );
            });
        });
    // Clicking the darkened backdrop closes the panel, like any modal. Escape
    // is *not* consumed here: it is routed by `video::App::handle_escape` so
    // one rule decides what a press backs out of (fullscreen, panel, sheet,
    // screen).
    if response.backdrop_response.clicked() {
        action = Action::CloseSettings;
    }
    action
}

fn display_section(ui: &mut egui::Ui, model: &mut SettingsModel) -> Action {
    let mut action = Action::None;
    let prefs = model.prefs;

    row(ui, "Taille de la fenêtre", |ui| {
        for &(zoom, label) in ZOOM_CHOICES {
            if ui.selectable_label(prefs.zoom == zoom, label).clicked() {
                action = Action::Set(Setting::Zoom(zoom));
            }
        }
    });
    hint(ui, "La fenêtre reste librement redimensionnable ; ces paliers fixent une taille (F1-F4).");

    let filter = Filter::from_pref(&prefs.filter);
    row(ui, "Filtre", |ui| {
        for &(value, label) in FILTER_CHOICES {
            if ui.selectable_label(filter == value, label).clicked() {
                action = Action::Set(Setting::Filter(value));
            }
        }
    });

    let aspect = Aspect::from_pref(&prefs.aspect);
    row(ui, "Ratio", |ui| {
        for &(value, label) in ASPECT_CHOICES {
            if ui.selectable_label(aspect == value, label).clicked() {
                action = Action::Set(Setting::Aspect(value));
            }
        }
    });
    hint(ui, "L'image n'est jamais déformée : bandes noires si la fenêtre ne tombe pas juste.");

    row(ui, "Plein écran", |ui| {
        let mut on = model.fullscreen;
        if ui.checkbox(&mut on, "Occuper tout l'écran (F11)").changed() {
            action = Action::Set(Setting::Fullscreen(on));
        }
    });
    hint(ui, "Non mémorisé : l'application démarre toujours en fenêtré.");

    row(ui, "Compteur d'images", |ui| {
        let mut on = prefs.show_fps;
        if ui.checkbox(&mut on, "Afficher les FPS (F)").changed() {
            action = Action::Set(Setting::ShowFps(on));
        }
    });

    action
}

fn audio_section(ui: &mut egui::Ui, model: &mut SettingsModel) -> Action {
    let mut action = Action::None;
    let prefs = model.prefs;

    row(ui, "Muet", |ui| {
        let mut on = prefs.mute;
        if ui.checkbox(&mut on, "Couper le son (M)").changed() {
            action = Action::Set(Setting::Mute(on));
        }
    });

    row(ui, "Volume", |ui| {
        let mut volume = prefs.volume;
        if ui
            .add_enabled(!prefs.mute, egui::Slider::new(&mut volume, 0..=100).suffix(" %"))
            .changed()
        {
            action = Action::Set(Setting::Volume(volume));
        }
    });
    hint(ui, "Le son est coupé pendant l'accéléré ; le volume choisi ici revient à sa libération.");

    action
}

/// `Entrées`: the twelve SNES buttons, each with the key and the controller
/// button that press it.
///
/// Clicking a cell starts a capture (`input::Capture`): the very next key —
/// or controller button — is assigned, Escape gives up, and a key that is
/// already an application shortcut is refused with a reason instead of being
/// stored as a binding that would never reach the console. A conflict with
/// another SNES button is settled by swapping the two, so no button is ever
/// left unbound; the swap is announced under the list.
fn inputs_section(ui: &mut egui::Ui, model: &mut SettingsModel) -> Action {
    let mut action = Action::None;

    // The prompt, the notice and the reset button sit *above* the list: the
    // twelve rows are taller than the panel's content area at a small window
    // size, and a capture prompt the player has to scroll to find would be
    // useless.
    if let Some((button, device)) = model.state.capture.pending() {
        let what = match device {
            Device::Keyboard => "une touche",
            Device::Gamepad => "un bouton de manette",
        };
        ui.label(
            RichText::new(format!(
                "Appuyez sur {what} pour {} — Échap pour annuler.",
                button_label(button)
            ))
            .size(theme::SIZE_BODY)
            .color(theme::ACCENT),
        );
    } else {
        hint(ui, "Cliquez sur une case pour réaffecter la touche ou le bouton.");
    }
    if let Some(notice) = &model.state.capture.notice {
        ui.label(RichText::new(notice).size(theme::SIZE_SMALL).color(theme::RED));
    }
    ui.add_space(4.0);
    if ui.button("Rétablir les entrées par défaut").clicked() {
        action = Action::Set(Setting::ResetInputs);
    }
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(BUTTON_COL_W, 0.0),
            Layout::left_to_right(Align::Min),
            |ui| {
                ui.label(RichText::new("Bouton").size(theme::SIZE_SMALL).color(theme::TEXT_DIM));
            },
        );
        ui.allocate_ui_with_layout(
            Vec2::new(BIND_COL_W, 0.0),
            Layout::left_to_right(Align::Min),
            |ui| {
                ui.label(RichText::new("Clavier").size(theme::SIZE_SMALL).color(theme::TEXT_DIM));
            },
        );
        ui.label(RichText::new("Manette").size(theme::SIZE_SMALL).color(theme::TEXT_DIM));
    });

    // Tighter than the panel's default spacing: twelve rows have to fit in the
    // same content area as a five-control section.
    ui.spacing_mut().item_spacing.y = 2.0;
    for name in input::BUTTONS {
        // `shown_key`, not `effective_key`: a binding another button won is
        // shown as a dash, since this one does not answer to it (see
        // `input::shown_key`).
        let key = input::shown_key(&model.prefs.keymap, name)
            .map(input::key_label)
            .unwrap_or_else(|| "—".to_string());
        let pad_binding = pad::binding_label(&model.prefs.pad_map, name);
        let capturing_key = model.state.capture.waiting_for(Device::Keyboard) == Some(name);
        let capturing_pad = model.state.capture.waiting_for(Device::Gamepad) == Some(name);
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(BUTTON_COL_W, 0.0),
                Layout::left_to_right(Align::Min),
                |ui| {
                    ui.label(
                        RichText::new(button_label(name))
                            .size(theme::SIZE_BODY)
                            .color(theme::TEXT),
                    );
                },
            );
            ui.allocate_ui_with_layout(
                Vec2::new(BIND_COL_W, 0.0),
                Layout::left_to_right(Align::Min),
                |ui| {
                    let text = binding_cell(&key, capturing_key, Device::Keyboard);
                    let response = ui.selectable_label(capturing_key, text);
                    if response.clicked() {
                        model.state.capture.start(name, Device::Keyboard);
                        // The clicked cell keeps egui's keyboard focus, where
                        // Space and Enter count as a click: binding either of
                        // them would immediately re-open the capture on the
                        // same row.
                        response.surrender_focus();
                    }
                },
            );
            let text = binding_cell(&pad_binding, capturing_pad, Device::Gamepad);
            let response = ui.selectable_label(capturing_pad, text);
            if response.clicked() {
                model.state.capture.start(name, Device::Gamepad);
                response.surrender_focus();
            }
        });
    }

    ui.add_space(8.0);
    hint(
        ui,
        "Une touche déjà prise par un autre bouton est échangée avec lui ; les raccourcis de l'application (F1-F12, Tab, P, M…) sont refusés.",
    );
    hint(
        ui,
        "Clavier et manette 1 pilotent le joueur 1, manette 2 le joueur 2. Les sticks et la croix restent toujours actifs sur les directions.",
    );

    action
}

fn emulation_section(ui: &mut egui::Ui, model: &mut SettingsModel) -> Action {
    let mut action = Action::None;
    let prefs = model.prefs;

    row(ui, "Accéléré (Tab)", |ui| {
        for &factor in FAST_FORWARD_FACTORS {
            if ui.selectable_label(prefs.fast_forward_factor == factor, format!("×{factor}")).clicked()
            {
                action = Action::Set(Setting::FastForward(factor));
            }
        }
    });
    hint(ui, "Nombre d'images émulées par image affichée tant que Tab est maintenu.");

    row(ui, "Reprise instantanée", |ui| {
        let mut on = prefs.resume_on_launch;
        if ui.checkbox(&mut on, "Reprendre où l'on s'était arrêté (F10)").changed() {
            action = Action::Set(Setting::ResumeOnLaunch(on));
        }
    });
    hint(ui, "L'état de session est écrit à chaque sortie, dans un fichier séparé des slots.");

    row(ui, "Confirmation", |ui| {
        let mut on = prefs.confirm_on_quit;
        if ui.checkbox(&mut on, "Demander avant de quitter (C)").changed() {
            action = Action::Set(Setting::ConfirmOnQuit(on));
        }
    });

    row(ui, "Slot de sauvegarde", |ui| {
        for slot in 0..SLOT_COUNT {
            if ui.selectable_label(prefs.save_slot == slot, slot.to_string()).clicked() {
                action = Action::Set(Setting::Slot(slot));
            }
        }
    });
    hint(ui, "F5 sauvegarde et F9 recharge ce slot ; F7 passe au suivant.");

    action
}

fn folders_section(ui: &mut egui::Ui, model: &mut SettingsModel) -> Action {
    let mut action = Action::None;

    ui.label(RichText::new("Dossier des ROMs").size(theme::SIZE_BODY).color(theme::TEXT));
    path_line(ui, &super::home::shorten_path(model.library_dir, PATH_MAX_CHARS));
    ui.horizontal(|ui| {
        if ui.button("Choisir…").clicked() {
            action = Action::ChooseLibraryDir;
        }
        if ui
            .add_enabled(model.prefs.library_dir.is_some(), egui::Button::new("Par défaut"))
            .clicked()
        {
            action = Action::ResetLibraryDir;
        }
    });
    hint(ui, "Dossier analysé par la bibliothèque de l'accueil.");

    ui.add_space(12.0);
    ui.label(RichText::new("Dossier des captures").size(theme::SIZE_BODY).color(theme::TEXT));
    path_line(ui, &screenshot_dir_label(model.prefs.screenshot_dir.as_deref()));
    ui.horizontal(|ui| {
        if ui.button("Choisir…").clicked() {
            action = Action::ChooseScreenshotDir;
        }
        if ui
            .add_enabled(model.prefs.screenshot_dir.is_some(), egui::Button::new("Par défaut"))
            .clicked()
        {
            action = Action::ResetScreenshotDir;
        }
    });
    hint(ui, "Destination de F12 ; la galerie de la fiche de jeu lit le même dossier.");

    ui.add_space(12.0);
    ui.label(RichText::new("Dossier des sauvegardes").size(theme::SIZE_BODY).color(theme::TEXT));
    path_line(ui, &save_dir_label(model.prefs.save_dir.as_deref()));
    ui.horizontal(|ui| {
        if ui.button("Choisir…").clicked() {
            action = Action::ChooseSaveDir;
        }
        if ui
            .add_enabled(model.prefs.save_dir.is_some(), egui::Button::new("Par défaut"))
            .clicked()
        {
            action = Action::ResetSaveDir;
        }
    });
    hint(ui, SAVE_DIR_HINT);
    if let Some(previous) = &model.prefs.previous_save_dir {
        ui.label(
            RichText::new(previous_save_dir_line(previous))
                .size(theme::SIZE_SMALL)
                .color(theme::TEXT_DIM),
        );
    }
    if let Some(notice) = &model.state.folder_notice {
        let color = match notice {
            FolderNotice::Error(_) => theme::RED,
            FolderNotice::Info(_) => theme::TEXT_DIM,
        };
        ui.label(RichText::new(notice.text()).size(theme::SIZE_SMALL).color(color));
    }

    action
}

fn about_section(ui: &mut egui::Ui, model: &mut SettingsModel) -> Action {
    let mut action = Action::None;

    ui.label(
        RichText::new(model.app_name).size(theme::SIZE_HEADING).strong().color(theme::TEXT),
    );
    ui.label(
        RichText::new(format!("version {}", model.version))
            .size(theme::SIZE_BODY)
            .color(theme::TEXT_DIM),
    );
    ui.add_space(6.0);
    ui.label(
        RichText::new("Émulateur Super Nintendo écrit en Rust : cœur d'émulation sans entrées/sorties, interface séparée.")
            .size(theme::SIZE_BODY)
            .color(theme::TEXT_DIM),
    );

    ui.add_space(12.0);
    ui.label(RichText::new("Guide pédagogique").size(theme::SIZE_BODY).color(theme::TEXT));
    match &model.state.guide {
        Some(path) => {
            path_line(ui, &super::home::shorten_path(path, PATH_MAX_CHARS));
            if ui.button("Ouvrir le PDF").clicked() {
                action = Action::OpenGuide;
            }
        }
        None => {
            hint(
                ui,
                "Introuvable près de cette version : le PDF se trouve dans le dépôt, sous docs/emulateur-snes-explique.pdf.",
            );
        }
    }
    if let Some(notice) = &model.state.notice {
        ui.label(RichText::new(notice).size(theme::SIZE_SMALL).color(theme::RED));
    }

    ui.add_space(12.0);
    ui.label(RichText::new("Fichiers de l'application").size(theme::SIZE_BODY).color(theme::TEXT));
    match model.config_dir {
        Some(dir) => path_line(ui, &super::home::shorten_path(dir, PATH_MAX_CHARS)),
        None => hint(ui, "Aucun répertoire de configuration disponible : rien n'est mémorisé."),
    }
    hint(ui, "Préférences, cache de la bibliothèque et miniatures ; supprimables sans rien perdre.");

    action
}

/// One setting: its name on the left, its controls on the right. The label
/// column never takes more than half the row, so a narrow window keeps the
/// controls usable.
fn row(ui: &mut egui::Ui, label: &str, controls: impl FnOnce(&mut egui::Ui)) {
    let label_w = LABEL_W.min(ui.available_width() * 0.5);
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(Vec2::new(label_w, 0.0), Layout::left_to_right(Align::Min), |ui| {
            ui.label(RichText::new(label).size(theme::SIZE_BODY).color(theme::TEXT));
        });
        controls(ui);
    });
}

/// Secondary line under a setting: what it does, or the limit it has.
fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(theme::SIZE_SMALL).color(theme::TEXT_DIM));
    ui.add_space(8.0);
}

/// A folder path, styled apart from the prose around it.
fn path_line(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(theme::SIZE_SMALL).color(theme::TEXT));
}

/// What the screenshot folder shows: the chosen one, or where captures go
/// without it (`App::take_screenshot`'s own fallback). A long path is elided in
/// its middle so the line never widens the panel.
pub fn screenshot_dir_label(dir: Option<&Path>) -> String {
    match dir {
        Some(dir) => super::home::shorten_path(dir, PATH_MAX_CHARS),
        None => "À côté de la ROM, dans Screenshots/".to_string(),
    }
}

/// Same for the save folder (`.srm` battery saves, `.state`/`.stateN` slots and
/// the `.resume` session state).
pub fn save_dir_label(dir: Option<&Path>) -> String {
    match dir {
        Some(dir) => super::home::shorten_path(dir, PATH_MAX_CHARS),
        None => "À côté de la ROM".to_string(),
    }
}

/// What the save folder does and when. Three facts matter: the folder is read
/// at *load* time (`video::App::switch_rom` freezes it for the session, so the
/// running game keeps writing where it read from), an existing save left beside
/// the ROM is still read when the folder has none (`paths::read_sidecar`) —
/// nothing is moved or deleted — and the files there are named after the game
/// (`library::game_id`), which is what keeps two ROM files of the same name
/// from sharing one save.
pub const SAVE_DIR_HINT: &str =
    "Sauvegardes de cartouche (.srm), slots et reprise. Pris en compte au chargement d'un jeu. \
     Dans un dossier commun, chaque fichier porte le nom du jeu (titre de la cartouche et somme \
     de contrôle), jamais celui du fichier ROM : deux ROMs homonymes gardent des sauvegardes \
     distinctes. Une sauvegarde restée à côté de la ROM est toujours relue tant que le dossier \
     n'en a pas : rien n'est déplacé ni supprimé.";

/// Line shown under the save folder when a folder was configured before the
/// current setting (`prefs.previous_save_dir`): what it still holds is read,
/// never written to again, so clearing or changing the folder cannot look like
/// lost progress.
pub fn previous_save_dir_line(previous: &Path) -> String {
    format!(
        "Dossier précédent, toujours relu : {}",
        super::home::shorten_path(previous, PATH_MAX_CHARS)
    )
}

#[cfg(test)]
mod tests {
    use winit::keyboard::KeyCode;

    use super::*;

    #[test]
    fn the_panel_narrows_with_the_window_and_never_below_its_floor() {
        // ×3/×4 windows (768/1024 points wide): full width.
        assert_eq!(panel_dims(1024.0), (PANEL_W, NAV_W));
        assert_eq!(panel_dims(768.0), (PANEL_W, NAV_W));
        // ×2 (512 points): narrowed, section list shrunk with it.
        let (panel, nav) = panel_dims(512.0);
        assert_eq!(panel, 480.0);
        assert!(nav <= NAV_W && nav > 0.0, "{nav}");
        // ×1 (256 points): floored, so the controls stay readable even though
        // the panel is then wider than the window.
        let (panel, nav) = panel_dims(256.0);
        assert_eq!(panel, MIN_PANEL_W);
        assert!(nav < NAV_W, "{nav}");
        assert!(panel - nav - 24.0 >= MIN_CONTENT_W, "controls column too narrow");
        // Monotonic: a wider window never yields a narrower panel.
        let mut previous = 0.0;
        for w in [200.0, 300.0, 400.0, 600.0, 900.0, 2000.0] {
            let (panel, _) = panel_dims(w);
            assert!(panel >= previous, "{w}: {panel} < {previous}");
            previous = panel;
        }
    }

    #[test]
    fn escape_closes_the_panel_unless_fullscreen_must_be_left_first() {
        assert!(escape_closes_settings(true, false));
        assert!(!escape_closes_settings(true, true), "fullscreen backs out first");
        assert!(!escape_closes_settings(false, false));
        assert!(!escape_closes_settings(false, true));
    }

    #[test]
    fn the_panel_opens_closed_on_the_display_section() {
        let state = SettingsUi::default();
        assert!(!state.open);
        assert_eq!(state.section, Section::Display);
        assert_eq!(state.guide, None);
        assert_eq!(state.notice, None);
    }

    #[test]
    fn every_section_is_listed_once_with_its_own_label() {
        assert_eq!(Section::ALL.len(), 6);
        let mut labels: Vec<&str> = Section::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(
            labels,
            vec!["Affichage", "Audio", "Entrées", "Émulation", "Dossiers", "À propos"]
        );
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), Section::ALL.len());
        assert_eq!(Section::default(), Section::Display);
    }

    #[test]
    fn the_display_choices_match_what_the_renderer_and_the_hotkeys_offer() {
        assert_eq!(ZOOM_CHOICES.iter().map(|&(z, _)| z).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
        // Same order as the `V` hotkey's cycle (`Filter::next`).
        let filters: Vec<Filter> = FILTER_CHOICES.iter().map(|&(f, _)| f).collect();
        assert_eq!(filters, vec![Filter::None, Filter::Smooth, Filter::Crt]);
        assert_eq!(Filter::None.next(), Filter::Smooth);
        assert_eq!(Filter::Smooth.next(), Filter::Crt);
        assert_eq!(Filter::Crt.next(), Filter::None);
        let aspects: Vec<Aspect> = ASPECT_CHOICES.iter().map(|&(a, _)| a).collect();
        assert_eq!(aspects, vec![Aspect::PixelPerfect, Aspect::Tv]);
        assert_eq!(Aspect::PixelPerfect.toggled(), Aspect::Tv);
    }

    #[test]
    fn the_emulation_choices_match_the_preferences_ranges() {
        // The panel must offer exactly the factors `Prefs::sanitize` allows and
        // the `[`/`]` hotkeys step through (this cross-check used to live in
        // `menu.rs`, whose radio group the panel replaced).
        assert_eq!(FAST_FORWARD_FACTORS, &[2, 3, 4]);
        for &factor in FAST_FORWARD_FACTORS {
            let mut p = Prefs::default();
            p.fast_forward_factor = factor;
            let json = serde_json::to_string(&p).expect("serialize");
            assert_eq!(Prefs::from_json(&json).expect("parse").fast_forward_factor, factor);
        }
        // Slots: the panel lists 0..SLOT_COUNT-1, the range `set_slot` clamps to.
        assert_eq!(SLOT_COUNT, 10);
        let p = Prefs::from_json("{\"save_slot\": 99}").expect("parse");
        assert_eq!(p.save_slot, SLOT_COUNT - 1);
    }

    #[test]
    fn folder_labels_name_the_fallback_when_nothing_is_configured() {
        assert_eq!(
            screenshot_dir_label(None),
            "À côté de la ROM, dans Screenshots/"
        );
        assert_eq!(screenshot_dir_label(Some(Path::new("/shots"))), "/shots");
        assert_eq!(save_dir_label(None), "À côté de la ROM");
        assert_eq!(save_dir_label(Some(Path::new("/saves"))), "/saves");
        // A long folder is elided in its middle rather than widening the panel.
        let long = PathBuf::from("/Volumes/Backup").join("a".repeat(80));
        let label = save_dir_label(Some(&long));
        assert!(label.chars().count() <= PATH_MAX_CHARS, "{label}");
        assert!(label.contains('…'), "{label}");
    }

    /// Every string the panel actually painted, one per line. Used to assert
    /// that a section really rendered its controls instead of silently
    /// producing nothing.
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

    /// Draw one section on a headless `egui::Context` (no window, no GPU) and
    /// return what it asked for plus what it painted.
    fn draw(section: Section, prefs: &Prefs, state: &mut SettingsUi) -> (Action, String) {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        state.open = true;
        state.section = section;
        let mut produced = Action::Quit; // must be overwritten by `show`
        let mut input = egui::RawInput::default();
        // A ×4 window (1024x896 points) — the size at which `CONTENT_H` is
        // reached, so no section scrolls and every control is painted (shapes
        // clipped away by a scroll area are culled and would not appear).
        input.screen_rect =
            Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 896.0)));
        // Two frames: egui sizes a modal from the previous frame's measurement,
        // so the first one lays out and the second is the one that paints.
        let mut output = ctx.run(input.clone(), |_| {});
        for _ in 0..5 {
            output = ctx.run(input.clone(), |ctx| {
                produced = show(
                    ctx,
                    &mut SettingsModel {
                        app_name: "Prisme",
                        version: "0.0.0",
                        prefs,
                        fullscreen: false,
                        library_dir: Path::new("roms"),
                        config_dir: Some(Path::new("/config/Prisme")),
                        state,
                    },
                );
            });
        }
        (produced, painted_text(&output))
    }

    /// Build every section on a headless `egui::Context`: exercises the real
    /// widget code and asserts that merely displaying the panel never asks for
    /// a change, and never writes a preference (the model borrows them
    /// immutably, so this also pins that contract).
    #[test]
    fn drawing_the_panel_produces_no_action_without_interaction() {
        let prefs = Prefs::default();
        for section in Section::ALL {
            let mut state = SettingsUi::default();
            let (produced, text) = draw(section, &prefs, &mut state);
            assert_eq!(produced, Action::None, "section {:?}", section.label());
            assert_eq!(state.section, section, "the section must not change by itself");
            assert!(text.contains("Réglages"), "the panel title is missing: {text}");
            assert!(text.contains(section.label()), "section {:?} not listed", section.label());
        }
        assert_eq!(prefs, Prefs::default(), "the panel must never write a preference");
    }

    /// Each section must actually paint its own controls; an empty section
    /// would still pass the no-action test above.
    #[test]
    fn every_section_paints_the_settings_it_owns() {
        let prefs = Prefs::default();
        let expected: [(Section, &[&str]); 6] = [
            (Section::Display, &["Taille de la fenêtre", "Filtre", "Ratio", "Plein écran", "×3"]),
            (Section::Audio, &["Muet", "Volume"]),
            (
                Section::Inputs,
                &[
                    "Bouton",
                    "Clavier",
                    "Manette",
                    "Haut",
                    "Droite",
                    "Rétablir les entrées par défaut",
                ],
            ),
            (
                Section::Emulation,
                &["Accéléré (Tab)", "Reprise instantanée", "Confirmation", "Slot de sauvegarde"],
            ),
            (
                Section::Folders,
                &["Dossier des ROMs", "Dossier des captures", "Dossier des sauvegardes"],
            ),
            (Section::About, &["Prisme", "version 0.0.0", "Guide pédagogique"]),
        ];
        for (section, labels) in expected {
            let mut state = SettingsUi::default();
            let (_, text) = draw(section, &prefs, &mut state);
            for label in labels {
                assert!(text.contains(label), "{:?} is missing {label:?}: {text}", section.label());
            }
        }
    }

    /// The save folder is a live setting now: its picker must be offered, its
    /// current value shown, and the two rules a player has to know about it
    /// (when it takes effect, what happens to saves left beside the ROM)
    /// stated in the section itself.
    #[test]
    fn the_save_folder_offers_a_picker_and_states_its_rules() {
        let mut prefs = Prefs::default();
        let mut state = SettingsUi::default();
        let (_, text) = draw(Section::Folders, &prefs, &mut state);
        assert!(text.contains("À côté de la ROM"), "{text}");
        assert!(!text.contains("phase 4"), "the setting is wired now: {text}");
        assert!(!text.contains("ignoré"), "{text}");
        assert!(text.contains("Pris en compte au chargement"), "{text}");
        assert!(text.contains("toujours relue"), "{text}");

        // A configured folder is shown as the plain path it is.
        prefs.save_dir = Some(PathBuf::from("/saves"));
        let mut state = SettingsUi::default();
        let (_, text) = draw(Section::Folders, &prefs, &mut state);
        assert!(text.contains("/saves"), "{text}");
    }

    /// A shared folder names its files after the *game*, so two homonymous ROM
    /// files keep separate saves. The section has to say so — it is the rule
    /// that decides whether a player can gather every save in one place.
    #[test]
    fn the_shared_folder_states_that_files_are_named_after_the_game() {
        let prefs = Prefs::default();
        let mut state = SettingsUi::default();
        let (_, text) = draw(Section::Folders, &prefs, &mut state);
        assert!(text.contains("nom du jeu"), "{text}");
        assert!(text.contains("homonymes"), "{text}");
    }

    /// The folder the player just left is named, since its saves are still
    /// read: without that line, clearing the setting looks like lost progress.
    #[test]
    fn the_previous_save_folder_is_named_when_there_is_one() {
        let mut prefs = Prefs::default();
        let mut state = SettingsUi::default();
        let (_, text) = draw(Section::Folders, &prefs, &mut state);
        assert!(!text.contains("Dossier précédent"), "there is none yet: {text}");

        prefs.previous_save_dir = Some(PathBuf::from("/old-saves"));
        let mut state = SettingsUi::default();
        let (_, text) = draw(Section::Folders, &prefs, &mut state);
        assert!(text.contains("Dossier précédent"), "{text}");
        assert!(text.contains("old-saves"), "{text}");
        assert!(previous_save_dir_line(Path::new("/old-saves")).contains("toujours relu"));
    }

    /// A binding another button won must not be printed as this one's: the row
    /// shows a dash, which is also what invites the player to rebind it.
    #[test]
    fn a_masked_binding_is_shown_as_a_dash_not_as_the_key_it_lost() {
        let mut prefs = Prefs::default();
        // X takes the key B holds by default, and B's own entry is gone (a
        // hand-edited file, or an entry dropped on read).
        prefs.keymap.remove("B");
        prefs.keymap.insert("X".to_string(), KeyCode::KeyZ);
        let mut state = SettingsUi::default();
        let (_, text) = draw(Section::Inputs, &prefs, &mut state);
        assert!(text.contains('—'), "the masked button must show a dash: {text}");
        assert_eq!(
            text.matches(&input::key_label(KeyCode::KeyZ)).count(),
            1,
            "Z must be printed once, on the button that really answers to it: {text}"
        );
    }

    /// A folder that could not be used is reported in the section that offered
    /// it, not only on stderr.
    #[test]
    fn an_unusable_folder_is_reported_in_the_section() {
        let prefs = Prefs::default();
        let mut state = SettingsUi {
            folder_notice: Some(FolderNotice::Error(
                "Dossier inutilisable, réglage inchangé : test".to_string(),
            )),
            ..Default::default()
        };
        let (_, text) = draw(Section::Folders, &prefs, &mut state);
        assert!(text.contains("Dossier inutilisable"), "{text}");

        // …and a remark about a change that only shows at the next load.
        let mut state = SettingsUi {
            folder_notice: Some(FolderNotice::Info("au prochain chargement".to_string())),
            ..Default::default()
        };
        let (_, text) = draw(Section::Folders, &prefs, &mut state);
        assert!(text.contains("au prochain chargement"), "{text}");
    }

    /// Every choice the panel offers must be representable in `prefs.json`:
    /// what it writes has to survive `Prefs::sanitize` and a round trip
    /// unchanged, or the panel would show a value the file cannot hold.
    #[test]
    fn every_offered_choice_round_trips_through_the_preferences_file() {
        for &(zoom, _) in ZOOM_CHOICES {
            let json = format!("{{\"zoom\": {zoom}}}");
            assert_eq!(Prefs::from_json(&json).expect("parse").zoom, zoom);
        }
        for &(filter, _) in FILTER_CHOICES {
            let json = format!("{{\"filter\": {:?}}}", filter.as_pref());
            let back = Prefs::from_json(&json).expect("parse");
            assert_eq!(Filter::from_pref(&back.filter), filter);
        }
        for &(aspect, _) in ASPECT_CHOICES {
            let json = format!("{{\"aspect\": {:?}}}", aspect.as_pref());
            let back = Prefs::from_json(&json).expect("parse");
            assert_eq!(Aspect::from_pref(&back.aspect), aspect);
        }
        for volume in [0u8, 50, 100] {
            let json = format!("{{\"volume\": {volume}}}");
            assert_eq!(Prefs::from_json(&json).expect("parse").volume, volume);
        }
        for slot in 0..SLOT_COUNT {
            let json = format!("{{\"save_slot\": {slot}}}");
            assert_eq!(Prefs::from_json(&json).expect("parse").save_slot, slot);
        }
    }

    #[test]
    fn every_snes_button_has_a_name_and_a_binding_cell() {
        for name in input::BUTTONS {
            assert_ne!(button_label(name), "?", "no label for {name}");
        }
        assert_eq!(button_label("Turbo"), "?");
        // Outside a capture the cell shows the binding itself…
        assert_eq!(binding_cell("Z", false, Device::Keyboard), "Z");
        assert_eq!(binding_cell("Bouton bas", false, Device::Gamepad), "Bouton bas");
        // …and during one, the prompt naming the device it waits for.
        assert_eq!(binding_cell("Z", true, Device::Keyboard), "Appuyez sur une touche…");
        assert_eq!(
            binding_cell("Bouton bas", true, Device::Gamepad),
            "Appuyez sur un bouton…"
        );
    }

    /// The list must show what the *player* bound, not the built-in table: a
    /// remapped button that still displayed its default key would be a setting
    /// shown but not applied.
    #[test]
    fn the_bindings_list_shows_the_players_own_bindings() {
        let mut prefs = Prefs::default();
        prefs.keymap.insert("A".to_string(), KeyCode::Space);
        prefs.pad_map.insert("A".to_string(), "North".to_string());
        let mut state = SettingsUi::default();
        let (_, text) = draw(Section::Inputs, &prefs, &mut state);
        assert!(text.contains("Espace"), "the rebound key is missing: {text}");
        assert!(text.contains(crate::pad::pad_label(crate::pad::Button::North)), "{text}");
        // A button left alone still shows its built-in key.
        assert!(text.contains(&input::key_label(KeyCode::KeyZ)), "{text}");
    }

    /// A press that belongs to a pending capture must not also drive the
    /// panel: Space and Enter are legitimate bindings, and they are exactly
    /// what egui turns into a click on the focused widget.
    #[test]
    fn a_pending_capture_takes_the_keys_away_from_the_panel() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let prefs = Prefs::default();
        let mut state = SettingsUi { open: true, section: Section::Inputs, ..Default::default() };
        state.capture.start("A", Device::Keyboard);
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1024.0, 896.0),
            )),
            ..Default::default()
        };
        for key in [egui::Key::Space, egui::Key::Enter, egui::Key::Escape, egui::Key::Tab] {
            input.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            });
        }
        let mut produced = Action::Quit;
        for _ in 0..3 {
            let _ = ctx.run(input.clone(), |ctx| {
                produced = show(
                    ctx,
                    &mut SettingsModel {
                        app_name: "Prisme",
                        version: "0.0.0",
                        prefs: &prefs,
                        fullscreen: false,
                        library_dir: Path::new("roms"),
                        config_dir: None,
                        state: &mut state,
                    },
                );
            });
            assert_eq!(produced, Action::None, "a captured key must not act on the panel");
        }
        assert!(state.capture.is_active(), "only the event loop ends a capture");
        assert_eq!(state.section, Section::Inputs);
    }

    /// A pending capture must be visible: the row it waits on shows the prompt
    /// and the panel says which button is being remapped.
    #[test]
    fn a_pending_capture_is_announced_in_the_panel() {
        let prefs = Prefs::default();
        let mut state = SettingsUi::default();
        state.capture.start("Start", Device::Keyboard);
        let (action, text) = draw(Section::Inputs, &prefs, &mut state);
        assert_eq!(action, Action::None, "drawing must not change a binding");
        assert!(text.contains("Appuyez sur une touche pour Start"), "{text}");
        assert!(text.contains("Échap pour annuler"), "{text}");
        assert!(state.capture.is_active(), "drawing must not end the capture");

        // The waiting row itself shows the prompt in place of its binding
        // (checked on the first row, the only one the fixed-height content
        // area is guaranteed to paint without scrolling).
        let mut state = SettingsUi::default();
        state.capture.start("Up", Device::Keyboard);
        let (_, text) = draw(Section::Inputs, &prefs, &mut state);
        assert!(text.contains("Appuyez sur une touche…"), "{text}");
        assert!(!text.contains("Flèche haut"), "the row must show the prompt: {text}");

        let mut state = SettingsUi::default();
        state.capture.start("Up", Device::Gamepad);
        state.capture.notice = Some("conflit de test".to_string());
        let (_, text) = draw(Section::Inputs, &prefs, &mut state);
        assert!(text.contains("Appuyez sur un bouton…"), "{text}");
        assert!(text.contains("Appuyez sur un bouton de manette pour Haut"), "{text}");
        assert!(text.contains("conflit de test"), "{text}");
    }

    /// The About section has two shapes (guide found / not found) and both must
    /// draw; a missing PDF must not offer a button that would do nothing.
    #[test]
    fn the_about_section_draws_with_and_without_the_guide() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let prefs = Prefs::default();
        for guide in [None, Some(PathBuf::from("/repo/docs/emulateur-snes-explique.pdf"))] {
            let mut state = SettingsUi {
                open: true,
                section: Section::About,
                guide,
                notice: Some("erreur de test".to_string()),
                folder_notice: None,
                capture: Capture::default(),
            };
            let mut produced = Action::Quit;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                produced = show(
                    ctx,
                    &mut SettingsModel {
                        app_name: "Prisme",
                        version: "0.0.0",
                        prefs: &prefs,
                        fullscreen: true,
                        library_dir: Path::new("roms"),
                        config_dir: None,
                        state: &mut state,
                    },
                );
            });
            assert_eq!(produced, Action::None);
        }
    }
}

