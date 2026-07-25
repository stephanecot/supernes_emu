//! `Réglages` — the settings screen, reachable from both screens.
//!
//! It is a **full-width view**, at the same rank as the library's own tabs and
//! reached by the `Réglages` entry of the very same tab bar (`ui::tabs`), which
//! keeps its spectral rule visible: a centred panel over a darkened library was
//! both too small for the sections it holds and hid the rule that says where
//! the player is. Sections on the left, the settings themselves on the right in
//! a bounded reading column, and the whole thing scrolls vertically — never
//! horizontally.
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

use egui::{Align, Layout, Rect, RichText, Sense, Vec2};

use crate::input::{self, Capture, Device};
use crate::pad;
use crate::prefs::{Prefs, FAST_FORWARD_FACTORS};
use crate::render::{Aspect, Filter};
use crate::state::SLOT_COUNT;

use super::tabs::{self, Tab};
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

/// Width of the section column on the left, in points. It is a sidebar of the
/// screen now, so it carries the longest section name ("À propos", "Émulation")
/// on one line with room around it.
const NAV_W: f32 = 180.0;
/// Narrowest that column is ever drawn: below this the names wrap.
const MIN_NAV_W: f32 = 120.0;
/// Horizontal space between the two columns, separator included.
const NAV_GAP: f32 = 24.0;
/// Longest line a setting is laid out on, whatever the window width. The view
/// is full width, but a checkbox stretched over 1200 points is unreadable and a
/// hint line that long cannot be scanned: the controls stay inside this reading
/// column, left-aligned, and the space beyond it is left free (it is where the
/// `Entrées` section draws the controller).
const READING_W: f32 = 720.0;
/// Height of one entry of the section list.
const NAV_ITEM_H: f32 = 32.0;
/// Left padding of an entry's label, and the width of the accent bar marking
/// the selected one.
const NAV_PAD_X: f32 = 12.0;
const NAV_BAR_W: f32 = 3.0;
/// Width reserved for a setting's label, so the controls of a section line up.
const LABEL_W: f32 = 190.0;
/// Width of the button-name column of the bindings list (`Entrées`).
const BUTTON_COL_W: f32 = 70.0;
/// Width of its keyboard column.
const BIND_COL_W: f32 = 130.0;
/// Width of its controller column — the widest of the three, since its labels
/// are the longest ("Gâchette L (LB) / Gâchette L2 (LT)").
const PAD_COL_W: f32 = 250.0;
/// Height of one line of the bindings list. Every cell is drawn in a box of
/// exactly this height: a horizontal layout centres each item against the row
/// height known when it was added, so cells of unequal heights end up
/// staggered — which is what made the button names, the keys and the controller
/// buttons of one line sit on three different baselines, and every line 44
/// points tall instead of 31.
const BIND_ROW_H: f32 = 30.0;
/// Longest folder path shown before its middle is elided.
const PATH_MAX_CHARS: usize = 52;
/// Narrowest the controls column is ever drawn.
const MIN_CONTENT_W: f32 = 130.0;
/// Length of the volume slider, value box excluded.
const SLIDER_W: f32 = 280.0;
/// Keyboard line of the footer. Escape leaves the view for whatever it was
/// opened from — the library tab that was showing, or the running game — and a
/// change is written to `prefs.json` as soon as it is made, not on the way out.
pub const FOOTER_HINT: &str = "Échap : revenir · chaque changement est enregistré aussitôt";

/// The panel's sections, in the display order the brief fixes: Affichage ·
/// Audio · Émulation · Entrées · Dossiers · À propos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    Display,
    Audio,
    Emulation,
    Inputs,
    Folders,
    About,
}

impl Section {
    pub const ALL: [Section; 6] = [
        Section::Display,
        Section::Audio,
        Section::Emulation,
        Section::Inputs,
        Section::Folders,
        Section::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Display => "Affichage",
            Section::Audio => "Audio",
            Section::Emulation => "Émulation",
            Section::Inputs => "Entrées",
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

/// Widths of the three columns the view is built from — section list, content
/// area, reading column inside it — for a body `inner_w` points wide (the
/// window minus the screen's own margins).
///
/// The section list gives ground first, since the controls are what the player
/// came for; the reading column is the content area capped at `READING_W`, so a
/// wide window adds free space on the right rather than stretching a checkbox
/// across it.
pub fn layout_dims(inner_w: f32) -> (f32, f32, f32) {
    let nav = NAV_W.min(inner_w * 0.30).max(MIN_NAV_W.min(inner_w * 0.5));
    let content = (inner_w - nav - NAV_GAP).max(MIN_CONTENT_W.min(inner_w.max(0.0)));
    (nav, content, content.min(READING_W))
}

/// Whether Escape should leave the settings view rather than act on the screen
/// it was opened from. Fullscreen keeps precedence, exactly like the game sheet
/// on the home screen: Escape backs out of the window mode first, then out of
/// the view (see `ui::app_state::escape_action`).
pub fn escape_closes_settings(open: bool, fullscreen: bool) -> bool {
    open && !fullscreen
}

/// Draw the whole screen — it owns the window while it is up — and return what
/// the player asked for.
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

    // Same footer band as the home screen, so the two views are at the same
    // rank and the content area starts and ends at the same place on both.
    egui::TopBottomPanel::bottom("prisme-settings-footer")
        .frame(
            egui::Frame::new().fill(theme::BG_DEEP).inner_margin(egui::Margin::symmetric(24, 10)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(FOOTER_HINT).size(theme::SIZE_SMALL).color(theme::TEXT_DIM));
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
            egui::Frame::new().fill(theme::BG_PANEL).inner_margin(egui::Margin::symmetric(24, 16)),
        )
        .show(ctx, |ui| {
            if let Some(produced) = header(ui) {
                action = produced;
            }
            ui.add_space(10.0);
            // The bar is the one the home screen draws, at the same place: the
            // spectral rule under `Réglages` is what says the settings are a
            // view of the shell and not a window laid over it. Choosing another
            // entry leaves for that library tab.
            if let Some(tab) = tabs::show(ui, Tab::Settings) {
                if tab.is_view() {
                    model.state.capture.cancel();
                    action = Action::ShowLibrary(tab);
                }
            }
            ui.add_space(16.0);
            let produced = body(ui, model);
            if produced != Action::None {
                action = produced;
            }
        });
    action
}

/// Identity header, laid out exactly like the home screen's (same mark, same
/// type, same heights) so switching between the two moves nothing but the band
/// below the tabs.
fn header(ui: &mut egui::Ui) -> Option<Action> {
    let mut action = None;
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(super::home::MARK_SIDE), Sense::hover());
        theme::mark(ui.painter(), rect);
        ui.add_space(10.0);
        ui.label(RichText::new("Prisme").font(theme::strong(theme::SIZE_TITLE)).color(theme::TEXT));
        ui.add_space(8.0);
        ui.label(
            RichText::new("Émulateur Super Nintendo")
                .font(theme::font(theme::SIZE_SMALL))
                .color(theme::TEXT_DIM),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if super::icons::button(ui, super::icons::Icon::ArrowLeft, "Retour (Échap)").clicked() {
                action = Some(Action::CloseSettings);
            }
        });
    });
    action
}

/// The two columns: the section list, then the selected section inside a
/// vertical scroll area. Both are allocated at an explicit width computed from
/// the window (`layout_dims`) so neither can be squeezed by the other, and the
/// scroll area is the only thing that ever scrolls — `ScrollArea::vertical`
/// carries no horizontal bar, whatever the window width.
fn body(ui: &mut egui::Ui, model: &mut SettingsModel) -> Action {
    let mut action = Action::None;
    let (nav_w, content_w, reading_w) = layout_dims(ui.available_width());
    let height = ui.available_height();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(Vec2::new(nav_w, height), Layout::top_down(Align::Min), |ui| {
            ui.set_min_width(nav_w);
            for section in Section::ALL {
                if nav_item(ui, section, model.state.section == section).clicked() {
                    model.state.section = section;
                    // Leaving the bindings list abandons whatever it was
                    // waiting for: a capture left pending would keep swallowing
                    // keys on a section that does not show it.
                    model.state.capture.cancel();
                }
            }
        });
        ui.separator();
        ui.allocate_ui_with_layout(
            Vec2::new(content_w, height),
            Layout::top_down(Align::Min),
            |ui| {
                ui.set_min_width(content_w);
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(reading_w, 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_min_width(reading_w);
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
                        },
                    );
                });
            },
        );
    });
    action
}

/// One entry of the section list: a full-width band rather than a text-sized
/// label, so the whole column answers the pointer and the selected section
/// reads as a place rather than as a highlighted word. The accent bar marks it;
/// the spectral rule stays spent on the active tab alone.
fn nav_item(ui: &mut egui::Ui, section: Section, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_at_least(
        Vec2::new(ui.available_width(), NAV_ITEM_H),
        Sense::CLICK | Sense::FOCUSABLE,
    );
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let lit = ui.ctx().animate_bool_with_time(
        response.id.with("lit"),
        response.hovered() || response.has_focus(),
        tabs::TRANSITION,
    );
    if selected {
        ui.painter().rect_filled(rect, 6.0, theme::BG_WIDGET);
        ui.painter().rect_filled(
            Rect::from_min_size(
                egui::pos2(rect.left(), rect.center().y - NAV_ITEM_H / 4.0),
                Vec2::new(NAV_BAR_W, NAV_ITEM_H / 2.0),
            ),
            1.0,
            theme::ACCENT,
        );
    } else if lit > 0.0 {
        ui.painter().rect_filled(rect, 6.0, theme::BG_WIDGET.gamma_multiply(lit));
    }
    let font =
        if selected { theme::strong(theme::SIZE_BODY) } else { theme::font(theme::SIZE_BODY) };
    let colour =
        if selected { theme::TEXT } else { theme::TEXT_DIM.lerp_to_gamma(theme::TEXT, lit) };
    let galley = ui.painter().layout(
        section.label().to_owned(),
        font,
        colour,
        (rect.width() - 2.0 * NAV_PAD_X).max(1.0),
    );
    ui.painter().galley(
        egui::pos2(rect.left() + NAV_PAD_X, rect.center().y - galley.size().y / 2.0),
        galley,
        colour,
    );
    if response.has_focus() {
        // Keyboard focus must be visible on its own, not only through the
        // colour change a pointer also produces.
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            6.0,
            egui::Stroke::new(1.0, theme::ACCENT),
            egui::StrokeKind::Inside,
        );
    }
    response
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
        // egui's default slider is 100 points long, which in a reading column
        // this wide reads as a stub and gives one point of gain per point of
        // travel; this is the only control of the panel that needs a size of
        // its own.
        ui.spacing_mut().slider_width = SLIDER_W.min(ui.available_width() - 60.0);
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
        bind_cell(ui, BUTTON_COL_W, |ui| {
            ui.label(RichText::new("Bouton").size(theme::SIZE_SMALL).color(theme::TEXT_DIM));
        });
        bind_cell(ui, BIND_COL_W, |ui| {
            ui.label(RichText::new("Clavier").size(theme::SIZE_SMALL).color(theme::TEXT_DIM));
        });
        bind_cell(ui, PAD_COL_W, |ui| {
            ui.label(RichText::new("Manette").size(theme::SIZE_SMALL).color(theme::TEXT_DIM));
        });
    });

    // Tighter than the section's default spacing: twelve rows have to fit
    // beside a controller drawing without pushing the last of them out of view.
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
            bind_cell(ui, BUTTON_COL_W, |ui| {
                ui.label(
                    RichText::new(button_label(name)).size(theme::SIZE_BODY).color(theme::TEXT),
                );
            });
            bind_cell(ui, BIND_COL_W, |ui| {
                // A key name is what the hardware reports, not prose.
                let text = RichText::new(binding_cell(&key, capturing_key, Device::Keyboard))
                    .font(theme::mono(theme::SIZE_MONO));
                let response = ui.selectable_label(capturing_key, text);
                if response.clicked() {
                    model.state.capture.start(name, Device::Keyboard);
                    // The clicked cell keeps egui's keyboard focus, where
                    // Space and Enter count as a click: binding either of
                    // them would immediately re-open the capture on the
                    // same row.
                    response.surrender_focus();
                }
            });
            bind_cell(ui, PAD_COL_W, |ui| {
                let text = RichText::new(binding_cell(&pad_binding, capturing_pad, Device::Gamepad))
                    .font(theme::mono(theme::SIZE_MONO));
                let response = ui.selectable_label(capturing_pad, text);
                if response.clicked() {
                    model.state.capture.start(name, Device::Gamepad);
                    response.surrender_focus();
                }
            });
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

    ui.label(
        RichText::new("Dossier des ROMs")
            .font(theme::strong(theme::SIZE_BODY))
            .color(theme::TEXT),
    );
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
    ui.label(
        RichText::new("Dossier des captures")
            .font(theme::strong(theme::SIZE_BODY))
            .color(theme::TEXT),
    );
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
    ui.label(
        RichText::new("Dossier des sauvegardes")
            .font(theme::strong(theme::SIZE_BODY))
            .color(theme::TEXT),
    );
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
        RichText::new(model.app_name)
            .font(theme::strong(theme::SIZE_HEADING))
            .color(theme::TEXT),
    );
    ui.label(
        RichText::new(format!("version {}", model.version))
            .font(theme::mono(theme::SIZE_MONO))
            .color(theme::TEXT_DIM),
    );
    ui.add_space(6.0);
    ui.label(
        RichText::new("Émulateur Super Nintendo écrit en Rust : cœur d'émulation sans entrées/sorties, interface séparée.")
            .size(theme::SIZE_BODY)
            .color(theme::TEXT_DIM),
    );

    ui.add_space(12.0);
    ui.label(
        RichText::new("Guide pédagogique")
            .font(theme::strong(theme::SIZE_BODY))
            .color(theme::TEXT),
    );
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
    ui.label(
        RichText::new("Fichiers de l'application")
            .font(theme::strong(theme::SIZE_BODY))
            .color(theme::TEXT),
    );
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
///
/// Every row of a section starts its controls on the **same** vertical line:
/// `allocate_ui_with_layout` shrinks back to what its content used, so the
/// column has to be claimed from inside it (`set_min_width`) — without that,
/// each row started right after its own label and no two lined up. The
/// controls then wrap inside their own column rather than overflowing the
/// panel: `Ratio` offers two choices that are together wider than what is left
/// of a 560-point panel, and a clipped choice is a setting that cannot be
/// clicked.
fn row(ui: &mut egui::Ui, label: &str, controls: impl FnOnce(&mut egui::Ui)) {
    let label_w = LABEL_W.min(ui.available_width() * 0.5);
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(Vec2::new(label_w, 0.0), Layout::left_to_right(Align::Min), |ui| {
            ui.set_min_width(label_w);
            ui.label(RichText::new(label).size(theme::SIZE_BODY).color(theme::TEXT));
        });
        let controls_w = ui.available_width().max(MIN_CONTENT_W);
        ui.allocate_ui_with_layout(
            Vec2::new(controls_w, 0.0),
            Layout::left_to_right(Align::Min).with_main_wrap(true),
            |ui| {
                ui.set_max_width(controls_w);
                controls(ui);
            },
        );
    });
}

/// One cell of the bindings list: a box of fixed width and height, its content
/// aligned on the middle. The width is what makes the three columns start on
/// the same vertical line whatever a row contains (`allocate_ui_with_layout`
/// shrinks back to its content, so the column has to be claimed from inside
/// with `set_min_size`); the height is what puts the three labels of a row on
/// one baseline.
fn bind_cell(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, BIND_ROW_H),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_size(Vec2::new(width, BIND_ROW_H));
            add(ui);
        },
    );
}

/// Secondary line under a setting: what it does, or the limit it has.
fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::font(theme::SIZE_SMALL)).color(theme::TEXT_DIM));
    ui.add_space(8.0);
}

/// A folder path: machine data, set in the monospace face like every other
/// string the application did not write itself.
fn path_line(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::mono(theme::SIZE_MONO)).color(theme::TEXT));
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

    /// The view takes the whole width, but the settings themselves stay inside
    /// a reading column: the section list keeps its width, the content area
    /// takes everything else, and a wider window adds free space instead of
    /// stretching a checkbox over it.
    #[test]
    fn the_view_splits_the_width_between_the_sections_and_a_reading_column() {
        // The two widths the brief names, minus the screen's 24-point margins.
        for window in [900.0_f32, 1280.0, 1600.0] {
            let inner = window - 48.0;
            let (nav, content, reading) = layout_dims(inner);
            assert_eq!(nav, NAV_W, "at {window} the section list is {nav}");
            assert!((nav + content + NAV_GAP - inner).abs() < 0.5, "{window}: {content}");
            assert!(reading <= READING_W, "{window}: reading column {reading}");
            assert!(reading >= 600.0, "{window}: reading column {reading} is too narrow");
        }
        // 1600 is wide enough that the reading column is capped and free space
        // is left beside it (where `Entrées` draws the controller).
        let (_, content, reading) = layout_dims(1600.0 - 48.0);
        assert_eq!(reading, READING_W);
        assert!(content - reading > 300.0, "{content} vs {reading}");
        // A ×1 window (256 points) is still laid out rather than clamped to
        // something wider than the window: nothing may overflow sideways.
        let (nav, content, reading) = layout_dims(256.0 - 48.0);
        assert!(nav >= MIN_NAV_W.min(208.0 * 0.5) && nav < NAV_W, "{nav}");
        assert!(content > 0.0 && reading == content, "{content} {reading}");
        assert!(nav + content + NAV_GAP <= 208.0 + MIN_CONTENT_W, "{nav} {content}");
        // Monotonic: a wider window never yields a narrower content area.
        let mut previous = 0.0;
        for w in [200.0, 300.0, 400.0, 600.0, 900.0, 2000.0] {
            let (_, content, _) = layout_dims(w);
            assert!(content >= previous, "{w}: {content} < {previous}");
            previous = content;
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
        // The order the brief fixes, top to bottom in the section column.
        assert_eq!(
            labels,
            vec!["Affichage", "Audio", "Émulation", "Entrées", "Dossiers", "À propos"]
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
    /// return what it asked for plus what it painted, at a ×4 window
    /// (1024x896 points).
    fn draw(section: Section, prefs: &Prefs, state: &mut SettingsUi) -> (Action, String) {
        let (action, text, _) = draw_at(section, prefs, state, (1024.0, 896.0));
        (action, text)
    }

    /// The same at an explicit window size, also returning the shapes: a
    /// section that scrolls out of view is *not* painted (egui skips a widget
    /// whose rectangle is outside the clip rectangle), which is what makes this
    /// able to tell a complete section from a truncated one.
    fn draw_at(
        section: Section,
        prefs: &Prefs,
        state: &mut SettingsUi,
        size: (f32, f32),
    ) -> (Action, String, Vec<egui::Shape>) {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        state.open = true;
        state.section = section;
        let mut produced = Action::Quit; // must be overwritten by `show`
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(size.0, size.1),
            )),
            ..Default::default()
        };
        let mut output = ctx.run(input.clone(), |_| {});
        // Several passes: a scroll area's extent and the tab bar's animations
        // are known only from the previous one, so the last pass is the one
        // that paints the resting state.
        for pass in 0..5 {
            input.time = Some(pass as f64 * 0.5);
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
        (produced, painted_text(&output), shapes)
    }

    /// The settings are a view of the shell, not a panel laid over it: the tab
    /// bar is drawn, `Réglages` carries the spectral rule, and nothing paints
    /// the modal veil that used to darken the library behind it.
    #[test]
    fn the_view_carries_the_tab_bar_with_the_spectral_rule_and_no_backdrop() {
        let prefs = Prefs::default();
        for section in Section::ALL {
            let mut state = SettingsUi::default();
            let (_, text, shapes) = draw_at(section, &prefs, &mut state, (1280.0, 800.0));
            for tab in Tab::ALL {
                assert!(text.contains(tab.label()), "tab {:?} is missing: {text}", tab.label());
            }
            // The rule is spent once: four segments, under the active tab.
            let segments: Vec<egui::Color32> = shapes
                .iter()
                .filter_map(|s| match s {
                    egui::Shape::Rect(r) if r.rect.height() <= theme::SPECTRAL_RULE_H => Some(r.fill),
                    _ => None,
                })
                .collect();
            assert_eq!(segments, theme::ACCENTS.to_vec(), "{:?}", section.label());
            assert!(
                !shapes.iter().any(|s| matches!(s, egui::Shape::Rect(r) if r.fill == theme::VEIL)),
                "{:?} still darkens a screen behind it",
                section.label()
            );
            // …and the footer names the way out.
            assert!(text.contains("Échap"), "{text}");
        }
    }

    /// Nothing may be cut off at either width the brief names. Each section is
    /// asserted on its **last** line, the one a content area too short would
    /// scroll out of sight.
    #[test]
    fn every_section_fits_at_900_and_at_1280_points() {
        let prefs = Prefs::default();
        let last: [(Section, &str); 6] = [
            (Section::Display, "Afficher les FPS (F)"),
            (Section::Audio, "Le son est coupé pendant l'accéléré"),
            // The twelfth binding row, then the two closing hints.
            (Section::Inputs, "Select"),
            (Section::Emulation, "F5 sauvegarde et F9 recharge ce slot"),
            (Section::Folders, "Sauvegardes de cartouche"),
            (Section::About, "supprimables sans rien perdre"),
        ];
        for size in [(900.0, 700.0), (1280.0, 800.0)] {
            for (section, tail) in last {
                let mut state = SettingsUi::default();
                let (_, text, _) = draw_at(section, &prefs, &mut state, size);
                assert!(
                    text.contains(tail),
                    "{:?} is cut off at {size:?}, {tail:?} never painted: {text}",
                    section.label()
                );
            }
        }
        // The bindings list is the tallest section: all twelve rows must show
        // at both sizes, not eight of them.
        for size in [(900.0, 700.0), (1280.0, 800.0)] {
            let mut state = SettingsUi::default();
            let (_, text, _) = draw_at(Section::Inputs, &prefs, &mut state, size);
            for name in input::BUTTONS {
                assert!(
                    text.contains(button_label(name)),
                    "{name} is not visible at {size:?}: {text}"
                );
            }
        }
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

