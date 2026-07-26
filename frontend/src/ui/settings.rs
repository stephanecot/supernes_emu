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
use snes_core::JoypadState;

use crate::i18n::{self, Lang, Msg};
use crate::input::{self, Capture, Device};
use crate::pad;
use crate::prefs::{Prefs, FAST_FORWARD_FACTORS};
use crate::render::{Aspect, Filter};
use crate::state::SLOT_COUNT;

use super::pad_art;
use super::tabs::{self, Tab};
use super::theme;
use super::{Action, Setting};

/// Window sizes offered by the display section, labelled by the picture they
/// actually produce rather than by a multiplier: a factor only means something
/// next to the base it multiplies, and that base — 256x224 — is exactly what
/// made the first step of the previous ladder a postage stamp nobody could use.
///
/// The native size is still reachable and comes **last**, named for what it is:
/// an expert entry, no longer the head of the list nor the default (which is
/// resolved from the monitor, see `render::default_zoom`). The window itself
/// stays freely resizable — these only set a size (see `render::zoomed_dims`).
/// Labels are the picture each step produces — a machine value, the same in
/// both languages. Only the native step is named, by `i18n::native_size`.
pub const ZOOM_CHOICES: &[(u8, &str)] = &[
    (2, "512 × 448"),
    (3, "768 × 672"),
    (4, "1024 × 896"),
    (5, "1280 × 1120"),
    (1, "256 × 224"),
];

/// The step whose label is prose rather than a size: the expert entry.
const NATIVE_ZOOM: u8 = 1;

/// What the ladder shows for one step.
fn zoom_label(lang: Lang, zoom: u8, dims: &str) -> String {
    if zoom == NATIVE_ZOOM {
        i18n::native_size(lang, dims)
    } else {
        dims.to_string()
    }
}

/// Sizes `F1`-`F4` set, in that order: the four usable steps of the ladder.
/// The native size deliberately has no hotkey — a key that shrinks the window
/// to a postage stamp is one nobody means to press.
pub const ZOOM_HOTKEYS: [u8; 4] = [2, 3, 4, 5];

/// Display filters, in `render::Filter::next`'s cycle order (the `V` hotkey).
pub const FILTER_CHOICES: &[(Filter, Msg)] = &[
    (Filter::None, Msg::NoneMasculine),
    (Filter::Smooth, Msg::FilterSmooth),
    (Filter::Crt, Msg::FilterCrt),
];

/// Pixel-aspect-ratio modes (the `R` hotkey toggles between the two).
pub const ASPECT_CHOICES: &[(Aspect, Msg)] =
    &[(Aspect::PixelPerfect, Msg::AspectPixel), (Aspect::Tv, Msg::AspectTv)];

/// The three answers the language row offers: follow the host, or one of the
/// two languages named in its own words (`Lang::endonym`).
pub fn language_choices() -> [Option<Lang>; 3] {
    let [first, second] = Lang::ALL;
    [None, Some(first), Some(second)]
}

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
const READING_W: f32 = 800.0;
/// Corner radius of the section rail's own surface, and how far above the
/// first entry that surface starts — the rail is a band of the screen's
/// furniture, like the footer, not a list floating on the page.
const RAIL_RADIUS: f32 = 8.0;
const RAIL_PAD_Y: f32 = 8.0;
/// Height of one entry of the section list.
const NAV_ITEM_H: f32 = 32.0;
/// Left padding of an entry's label, and the width of the accent bar marking
/// the selected one.
const NAV_PAD_X: f32 = 12.0;
const NAV_BAR_W: f32 = 3.0;
/// How far an entry's band is inset from the rail's own edges, so the selected
/// one reads as a piece laid *on* the rail rather than as a slice of it.
const NAV_INSET: f32 = 6.0;
/// Widest a setting's label column is ever drawn. What it *is* drawn at is
/// measured on the labels of the section actually being shown, in the active
/// language (`label_column_w`): `Taille de la fenêtre` and `Window size` are
/// not the same length, and a column sized on either one leaves a hole in the
/// other or clips it.
const LABEL_MAX_W: f32 = 190.0;
/// Gap kept between the longest label and the controls beside it.
const LABEL_GAP: f32 = 16.0;
/// Floor of each column of the bindings list, so a short language does not
/// leave three cramped cells; the widths themselves are measured on the cells
/// actually rendered (`bind_columns`).
const BUTTON_COL_MIN_W: f32 = 54.0;
const BIND_COL_MIN_W: f32 = 96.0;
const PAD_COL_MIN_W: f32 = 150.0;
/// Gap between two columns of the list.
const BIND_COL_GAP: f32 = 16.0;
/// Height of one line of the bindings list. Every cell is drawn in a box of
/// exactly this height: a horizontal layout centres each item against the row
/// height known when it was added, so cells of unequal heights end up
/// staggered — which is what made the button names, the keys and the controller
/// buttons of one line sit on three different baselines, and every line 44
/// points tall instead of 31.
const BIND_ROW_H: f32 = 30.0;

/// Space between the list and the drawing, and the margin kept between the
/// drawing and the right edge of the content area.
const PAD_GAP: f32 = 28.0;
const PAD_INSET: f32 = 10.0;
/// Longest folder path shown before its middle is elided.
const PATH_MAX_CHARS: usize = 52;
/// Narrowest the controls column is ever drawn.
const MIN_CONTENT_W: f32 = 130.0;
/// Length of the volume slider, value box excluded.
const SLIDER_W: f32 = 280.0;


/// The panel's sections, in the display order the brief fixes: Affichage ·
/// Audio · Émulation · Entrées · Dossiers · À propos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    Display,
    Audio,
    Emulation,
    Inputs,
    Assistant,
    Folders,
    About,
}

impl Section {
    pub const ALL: [Section; 7] = [
        Section::Display,
        Section::Audio,
        Section::Emulation,
        Section::Inputs,
        Section::Assistant,
        Section::Folders,
        Section::About,
    ];

    pub fn label(self, lang: Lang) -> &'static str {
        self.msg().text(lang)
    }

    fn msg(self) -> Msg {
        match self {
            Section::Display => Msg::SectionDisplay,
            Section::Audio => Msg::SectionAudio,
            Section::Emulation => Msg::SectionEmulation,
            Section::Inputs => Msg::SectionInputs,
            Section::Assistant => Msg::SectionAssistant,
            Section::Folders => Msg::SectionFolders,
            Section::About => Msg::SectionAbout,
        }
    }
}

/// Name shown for a SNES button in the bindings list. The four directions are
/// named in French; the eight others carry the legend printed on a real SNES
/// pad, which is also what the `--script` contract calls them.
pub fn button_label(lang: Lang, name: &str) -> &'static str {
    match name {
        "Up" => Msg::ButtonUp.text(lang),
        "Down" => Msg::ButtonDown.text(lang),
        "Left" => Msg::ButtonLeft.text(lang),
        "Right" => Msg::ButtonRight.text(lang),
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
pub fn binding_cell(lang: Lang, current: &str, capturing: bool, device: Device) -> String {
    if !capturing {
        return current.to_string();
    }
    match device {
        Device::Keyboard => Msg::PressAKey.text(lang).to_string(),
        Device::Gamepad => Msg::PressAButton.text(lang).to_string(),
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
    /// Edit buffer of the assistant's tool path: a text field needs somewhere
    /// to live between frames, and `prefs` is only written when the field is
    /// left.
    pub assistant_path: String,
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
    /// Button the pointer was on in the controller drawing. Only read when the
    /// drawing sits *under* the list (a narrow window), where its rectangle is
    /// known too late to highlight the matching row in the same frame; beside
    /// the list the answer is recomputed before the rows are laid out.
    pub pad_hover: Option<&'static str>,
}

/// Everything the panel displays, borrowed for one UI frame.
pub struct SettingsModel<'a> {
    pub app_name: &'a str,
    pub version: &'a str,
    /// Read-only: the panel proposes changes, the event loop applies them.
    pub prefs: &'a Prefs,
    /// Live window state, not a preference.
    pub fullscreen: bool,
    /// Window size step in force. Live state too: while the player has picked
    /// none, it is resolved from the monitor at launch rather than read from
    /// `prefs` (see `video::App::zoom`).
    pub zoom: u8,
    /// Folder the library actually scans, already resolved through its
    /// fallbacks (`library::library_dir`).
    pub library_dir: &'a Path,
    /// Path of the `claude` tool, or `None` when it was not found on this
    /// machine — which is what makes the assistant row inert.
    pub claude: Option<&'a Path>,
    /// Where `prefs.json` and the derived caches live, for the About section.
    pub config_dir: Option<&'a Path>,
    /// SNES buttons held right now, keyboard and controllers together. The
    /// `Entrées` section lights them on its drawing, which is what turns the
    /// screen into a controller tester — a half-broken pad is found out here
    /// rather than in the middle of a game.
    pub pressed: JoypadState,
    pub state: &'a mut SettingsUi,
    /// Language every string of the panel is rendered in, and the one the
    /// language row shows as chosen.
    pub lang: Lang,
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
                ui.label(
                    RichText::new(Msg::SettingsFooter.text(model.lang))
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
            egui::Frame::new().fill(theme::BG_PANEL).inner_margin(egui::Margin::symmetric(24, 16)),
        )
        .show(ctx, |ui| {
            if let Some(produced) = header(ui, model.lang) {
                action = produced;
            }
            ui.add_space(10.0);
            // The bar is the one the home screen draws, at the same place: the
            // spectral rule under `Réglages` is what says the settings are a
            // view of the shell and not a window laid over it. Choosing another
            // entry leaves for that library tab.
            if let Some(tab) = tabs::show(ui, Tab::Settings, model.lang) {
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
fn header(ui: &mut egui::Ui, lang: Lang) -> Option<Action> {
    let mut action = None;
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(super::home::MARK_SIDE), Sense::hover());
        theme::mark(ui.painter(), rect);
        ui.add_space(10.0);
        ui.label(RichText::new("Prisme").font(theme::strong(theme::SIZE_TITLE)).color(theme::TEXT));
        ui.add_space(8.0);
        ui.label(
            RichText::new(Msg::AppTagline.text(lang))
                .font(theme::font(theme::SIZE_SMALL))
                .color(theme::TEXT_DIM),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if super::icons::button(ui, super::icons::Icon::ArrowLeft, Msg::BackEsc.text(lang))
                .clicked()
            {
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
        // The gap is claimed here rather than by a separator between the two
        // columns: the rail has a surface of its own, and a hairline floating
        // in the middle of the gap would draw a second, misplaced edge.
        ui.spacing_mut().item_spacing.x = NAV_GAP;
        let rail = Rect::from_min_size(
            egui::pos2(ui.cursor().left(), ui.cursor().top() - RAIL_PAD_Y),
            Vec2::new(nav_w, height + RAIL_PAD_Y),
        );
        ui.painter().rect_filled(rail, RAIL_RADIUS, theme::BG_DEEP);
        ui.allocate_ui_with_layout(Vec2::new(nav_w, height), Layout::top_down(Align::Min), |ui| {
            ui.set_min_width(nav_w);
            for section in Section::ALL {
                if nav_item(ui, section, model.state.section == section, model.lang).clicked() {
                    model.state.section = section;
                    // Leaving the bindings list abandons whatever it was
                    // waiting for: a capture left pending would keep swallowing
                    // keys on a section that does not show it.
                    model.state.capture.cancel();
                }
            }
        });
        ui.allocate_ui_with_layout(
            Vec2::new(content_w, height),
            Layout::top_down(Align::Min),
            |ui| {
                ui.set_min_width(content_w);
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    // `Entrées` is the one section allowed past the reading
                    // column: the controller drawing lives in the space a wide
                    // window leaves free beside it, and bounds its own prose to
                    // the reading width from the inside.
                    let column_w = match model.state.section {
                        Section::Inputs => content_w,
                        _ => reading_w,
                    };
                    ui.allocate_ui_with_layout(
                        Vec2::new(column_w, 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_min_width(column_w);
                            let produced = match model.state.section {
                                Section::Display => display_section(ui, model),
                                Section::Audio => audio_section(ui, model),
                                Section::Inputs => inputs_section(ui, model, reading_w),
                                Section::Emulation => emulation_section(ui, model),
                                Section::Assistant => assistant_section(ui, model),
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
fn nav_item(
    ui: &mut egui::Ui,
    section: Section,
    selected: bool,
    lang: Lang,
) -> egui::Response {
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
    let band = rect.shrink2(Vec2::new(NAV_INSET, 0.0));
    if selected {
        ui.painter().rect_filled(band, 6.0, theme::BG_WIDGET);
        ui.painter().rect_filled(
            Rect::from_min_size(
                egui::pos2(band.left(), band.center().y - NAV_ITEM_H / 4.0),
                Vec2::new(NAV_BAR_W, NAV_ITEM_H / 2.0),
            ),
            1.0,
            theme::ACCENT,
        );
    } else if lit > 0.0 {
        ui.painter().rect_filled(band, 6.0, theme::BG_WIDGET.gamma_multiply(lit));
    }
    let font =
        if selected { theme::strong(theme::SIZE_BODY) } else { theme::font(theme::SIZE_BODY) };
    let colour =
        if selected { theme::TEXT } else { theme::TEXT_DIM.lerp_to_gamma(theme::TEXT, lit) };
    let galley = ui.painter().layout(
        section.label(lang).to_owned(),
        font,
        colour,
        (band.width() - 2.0 * NAV_PAD_X).max(1.0),
    );
    ui.painter().galley(
        egui::pos2(band.left() + NAV_PAD_X, band.center().y - galley.size().y / 2.0),
        galley,
        colour,
    );
    if response.has_focus() {
        // Keyboard focus must be visible on its own, not only through the
        // colour change a pointer also produces.
        ui.painter().rect_stroke(
            band.shrink(1.0),
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
    let lang = model.lang;
    let label_w = label_column_w(
        ui,
        lang,
        &[Msg::Language, Msg::WindowSize, Msg::Filter, Msg::Aspect, Msg::Fullscreen,
          Msg::FrameCounter],
    );

    // First thing in the section, before anything else: this is what someone
    // looks for when they cannot read the screen they are standing on, and a
    // language row further down would already be unreadable by then.
    row(ui, Msg::Language.text(lang), label_w, |ui| {
        for choice in language_choices() {
            // Endonyms: nobody looks for "Anglais" in order to switch to
            // English. The `system` entry is the only one named in the
            // interface's own language, since it names a behaviour.
            let label = match choice {
                None => Msg::LanguageSystem.text(lang),
                Some(other) => other.endonym(),
            };
            let selected = Lang::from_pref(&prefs.language) == choice;
            if ui.selectable_label(selected, label).clicked() {
                action = Action::Set(Setting::Language(choice));
            }
        }
    });

    row(ui, Msg::WindowSize.text(lang), label_w, |ui| {
        for &(zoom, dims) in ZOOM_CHOICES {
            if ui.selectable_label(model.zoom == zoom, zoom_label(lang, zoom, dims)).clicked() {
                action = Action::Set(Setting::Zoom(zoom));
            }
        }
    });
    hint(ui, Msg::WindowSizeHint.text(lang));

    let filter = Filter::from_pref(&prefs.filter);
    row(ui, Msg::Filter.text(lang), label_w, |ui| {
        for &(value, label) in FILTER_CHOICES {
            if ui.selectable_label(filter == value, label.text(lang)).clicked() {
                action = Action::Set(Setting::Filter(value));
            }
        }
    });

    let aspect = Aspect::from_pref(&prefs.aspect);
    row(ui, Msg::Aspect.text(lang), label_w, |ui| {
        for &(value, label) in ASPECT_CHOICES {
            if ui.selectable_label(aspect == value, label.text(lang)).clicked() {
                action = Action::Set(Setting::Aspect(value));
            }
        }
    });
    hint(ui, Msg::AspectHint.text(lang));

    row(ui, Msg::Fullscreen.text(lang), label_w, |ui| {
        let mut on = model.fullscreen;
        if checkbox(ui, &mut on, Msg::FullscreenCheck.text(lang)).changed() {
            action = Action::Set(Setting::Fullscreen(on));
        }
    });
    hint(ui, Msg::FullscreenHint.text(lang));

    row(ui, Msg::FrameCounter.text(lang), label_w, |ui| {
        let mut on = prefs.show_fps;
        if checkbox(ui, &mut on, Msg::ShowFpsCheck.text(lang)).changed() {
            action = Action::Set(Setting::ShowFps(on));
        }
    });

    action
}

fn audio_section(ui: &mut egui::Ui, model: &mut SettingsModel) -> Action {
    let mut action = Action::None;
    let prefs = model.prefs;
    let lang = model.lang;
    let label_w = label_column_w(ui, lang, &[Msg::Mute, Msg::Volume]);

    row(ui, Msg::Mute.text(lang), label_w, |ui| {
        let mut on = prefs.mute;
        if checkbox(ui, &mut on, Msg::MuteCheck.text(lang)).changed() {
            action = Action::Set(Setting::Mute(on));
        }
    });

    row(ui, Msg::Volume.text(lang), label_w, |ui| {
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
    hint(ui, Msg::VolumeHint.text(lang));

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
fn inputs_section(ui: &mut egui::Ui, model: &mut SettingsModel, reading_w: f32) -> Action {
    let mut action = Action::None;
    let lang = model.lang;
    // The three columns are measured on the cells they are about to hold, in
    // the active language: the button names are prose and shrink in English,
    // the key names never change (they are the legends printed on the keys),
    // and the controller labels are the longest of the three in both.
    let columns = bind_columns(ui, model);
    let list_w = columns.total();

    // Where the drawing goes. Beside the list when the window leaves room for
    // it, which is the point of the reading column being bounded; otherwise
    // *under* it — never squeezed into a strip, and never at the cost of the
    // list, which is what the player came here for.
    let available = ui.available_width();
    let beside = available - list_w - PAD_GAP >= pad_art::MIN_W;
    let mut pad_rect = None;
    let mut column_w = available.min(reading_w);
    if beside {
        let size = pad_art::size_for(available - list_w - PAD_GAP);
        // The inset is taken out of the *list's* column, so it only ever costs
        // slack: at the narrowest width where the drawing still fits beside the
        // list there is none to give, and the clamp gives it back.
        column_w = (available - size.x - PAD_GAP - PAD_INSET).clamp(list_w, reading_w.max(list_w));
        // Centred in whatever is left, so the drawing does not drift to the far
        // edge of a very wide window.
        let free = available - column_w - PAD_GAP;
        let left = ui.max_rect().left() + column_w + PAD_GAP + (free - size.x) / 2.0;
        pad_rect = Some(Rect::from_min_size(egui::pos2(left, ui.cursor().top()), size));
    }
    // Interacted with *before* the list is laid out, so pointing at a button on
    // the drawing highlights its row in the very same frame.
    let pad_response =
        pad_rect.map(|rect| ui.interact(rect, ui.id().with("pad-art"), Sense::click()));
    let pad_hover = match (&pad_response, pad_rect) {
        (Some(response), Some(rect)) => response.hover_pos().and_then(|p| pad_art::hit(rect, p)),
        _ => None,
    };
    // Under the list the drawing's rectangle is not known yet: last frame's
    // answer stands in, and egui refreshes it as soon as the pointer moves.
    let mut hovered = if beside { pad_hover } else { model.state.pad_hover };
    if let (Some(response), Some(name)) = (&pad_response, pad_hover) {
        // Clicking the drawing is clicking the row: the primary button rebinds
        // the key, the secondary one the controller button.
        if response.clicked() {
            model.state.capture.start(name, Device::Keyboard);
        } else if response.secondary_clicked() {
            model.state.capture.start(name, Device::Gamepad);
        }
    }

    let mut unbound: Vec<&'static str> = Vec::new();
    ui.allocate_ui_with_layout(Vec2::new(column_w, 0.0), Layout::top_down(Align::Min), |ui| {
        ui.set_min_width(column_w);
        // The prompt, the notice and the reset button sit *above* the list: the
        // twelve rows are taller than the panel's content area at a small window
        // size, and a capture prompt the player has to scroll to find would be
        // useless.
        if let Some((button, device)) = model.state.capture.pending() {
            let prompt = match device {
                Device::Keyboard => i18n::press_a_key_for(lang, button_label(lang, button)),
                Device::Gamepad => i18n::press_a_pad_button_for(lang, button_label(lang, button)),
            };
            ui.label(RichText::new(prompt).size(theme::SIZE_BODY).color(theme::ACCENT));
        } else {
            hint(ui, Msg::RebindHint.text(lang));
        }
        if let Some(notice) = &model.state.capture.notice {
            ui.label(RichText::new(notice).size(theme::SIZE_SMALL).color(theme::RED));
        }
        ui.add_space(4.0);
        if ui.button(Msg::ResetInputs.text(lang)).clicked() {
            action = Action::Set(Setting::ResetInputs);
        }
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            for (width, header) in [
                (columns.button, Msg::ColumnButton),
                (columns.key, Msg::ColumnKeyboard),
                (columns.pad, Msg::ColumnPad),
            ] {
                bind_cell(ui, width, |ui| {
                    ui.label(
                        RichText::new(header.text(lang))
                            .size(theme::SIZE_SMALL)
                            .color(theme::TEXT_DIM),
                    );
                });
            }
        });

        // Tighter than the section's default spacing: twelve rows have to fit
        // beside a controller drawing without pushing the last of them out of view.
        ui.spacing_mut().item_spacing.y = 2.0;
        for name in input::BUTTONS {
            // `shown_key`, not `effective_key`: a binding another button won is
            // shown as a dash, since this one does not answer to it (see
            // `input::shown_key`).
            let bound_key = input::shown_key(&model.prefs.keymap, name);
            let key = bound_key.map(input::key_label).unwrap_or_else(|| "—".to_string());
            let pad_binding = pad::binding_label(lang, &model.prefs.pad_map, name);
            if bound_key.is_none() && pad_binding == "—" {
                unbound.push(name);
            }
            let capturing_key = model.state.capture.waiting_for(Device::Keyboard) == Some(name);
            let capturing_pad = model.state.capture.waiting_for(Device::Gamepad) == Some(name);
            let held = pad_art::is_pressed(&model.pressed, name);
            // Reserved before the row is drawn so the band paints *behind* it;
            // the row's own rectangle is only known once it has been laid out.
            let band = ui.painter().add(egui::Shape::Noop);
            let row = ui
                .horizontal(|ui| {
                    bind_cell(ui, columns.button, |ui| {
                        ui.label(
                            RichText::new(button_label(lang, name))
                                .size(theme::SIZE_BODY)
                                .color(theme::TEXT),
                        );
                    });
                    bind_cell(ui, columns.key, |ui| {
                        // A key name is the legend printed on the key: never
                        // translated, in either language (`input::key_label`).
                        let text = RichText::new(binding_cell(lang, &key, capturing_key, Device::Keyboard))
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
                    bind_cell(ui, columns.pad, |ui| {
                        let text =
                            RichText::new(binding_cell(lang, &pad_binding, capturing_pad, Device::Gamepad))
                                .font(theme::mono(theme::SIZE_MONO));
                        let response = ui.selectable_label(capturing_pad, text);
                        if response.clicked() {
                            model.state.capture.start(name, Device::Gamepad);
                            response.surrender_focus();
                        }
                    });
                })
                .response;
            // The link the drawing earns its space with: the row under the
            // pointer lights its button, and the button under the pointer
            // lights its row.
            if row.contains_pointer() {
                hovered = Some(name);
            }
            let fill = if capturing_key || capturing_pad {
                Some(theme::ACCENT.gamma_multiply(0.28))
            } else if held {
                Some(theme::TEXT.gamma_multiply(0.14))
            } else if hovered == Some(name) {
                Some(theme::BG_WIDGET)
            } else {
                None
            };
            if let Some(fill) = fill {
                let rect = Rect::from_min_size(
                    row.rect.min,
                    Vec2::new(list_w.min(row.rect.width()), row.rect.height()),
                );
                ui.painter().set(
                    band,
                    egui::epaint::RectShape::filled(rect.expand2(Vec2::new(6.0, 1.0)), 5.0, fill),
                );
            }
        }

        ui.add_space(8.0);
        hint(ui, Msg::ConflictHint.text(lang));
        hint(ui, Msg::PlayersHint.text(lang));
    });

    // The drawing goes under the list when there was no room beside it. Same
    // widget, same interaction — only later in the frame, which is why the
    // hover it reports is carried to the next one.
    if !beside {
        ui.add_space(6.0);
        let size = pad_art::size_for(column_w);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let under = response.hover_pos().and_then(|p| pad_art::hit(rect, p));
        if let Some(name) = under {
            if response.clicked() {
                model.state.capture.start(name, Device::Keyboard);
            } else if response.secondary_clicked() {
                model.state.capture.start(name, Device::Gamepad);
            }
        }
        pad_rect = Some(rect);
        model.state.pad_hover = under;
    } else {
        model.state.pad_hover = pad_hover;
    }

    if let Some(rect) = pad_rect {
        let time = ui.input(|i| i.time);
        pad_art::paint(
            ui.painter(),
            rect,
            &pad_art::Pad {
                highlight: hovered,
                capturing: model.state.capture.pending().map(|(button, _)| button),
                pressed: model.pressed,
                unbound: &unbound,
                time,
            },
        );
        if model.state.capture.is_active() {
            // A pulse only exists if the frame it is drawn in is followed by
            // another one.
            ui.ctx().request_repaint();
        }
        let caption = pad_caption(ui, rect, lang);
        if !beside {
            // Under the list, the caption is part of the section's own height.
            ui.add_space(caption + 10.0);
        }
    }

    action
}

/// The line under the drawing. It says what the drawing is for — a controller
/// tester as much as a map of the bindings — because a picture nobody knows is
/// live is a picture nobody presses a button at.
fn pad_caption(ui: &mut egui::Ui, rect: Rect, lang: Lang) -> f32 {
    // A narrow drawing would turn the long form into a six-line paragraph
    // taller than the pad it explains.
    let text = if rect.width() >= pad_art::LEGEND_MIN_W {
        format!("{} {}", Msg::PadArtHint.text(lang), Msg::PadArtClickHint.text(lang))
    } else {
        Msg::PadArtShort.text(lang).to_string()
    };
    let galley = ui.painter().layout(
        text,
        theme::font(theme::SIZE_SMALL),
        theme::TEXT_DIM,
        rect.width(),
    );
    let height = galley.size().y;
    ui.painter().galley(egui::pos2(rect.left(), rect.bottom() + 10.0), galley, theme::TEXT_DIM);
    height
}

/// Width of the tool-path field: long enough for a full home-relative path
/// without pushing the browse button off a narrow window.
const TOOL_PATH_W: f32 = 340.0;

/// The assistant: one switch, and the truth about whether it can be flipped.
///
/// A toggle that promises what the machine cannot do is worse than no toggle,
/// so when `claude` is absent the row is disabled *and* says why — a greyed
/// control with no explanation sends people hunting through preferences for a
/// cause that is not there.
fn assistant_section(ui: &mut egui::Ui, model: &mut SettingsModel) -> Action {
    let mut action = Action::None;
    let lang = model.lang;
    let available = model.claude.is_some();

    hint(ui, Msg::AssistantWhat.text(lang));
    ui.add_space(12.0);

    let label_w = label_column_w(ui, lang, &[Msg::AssistantEnable, Msg::AssistantTool]);
    row(ui, Msg::AssistantEnable.text(lang), label_w, |ui| {
        let mut on = model.prefs.assistant && available;
        let toggle = ui
            .add_enabled(available, egui::Checkbox::new(&mut on, Msg::AssistantOn.text(lang)));
        if toggle.changed() {
            action = Action::Set(Setting::Assistant(on));
        }
    });
    hint(
        ui,
        if available { Msg::AssistantFound.text(lang) } else { Msg::AssistantMissing.text(lang) },
    );
    ui.add_space(14.0);

    // The path is typed, not searched for. See `assistant::find_claude`: the
    // application looks on the `PATH` and nowhere else, so this field is the
    // whole answer when the `PATH` a window inherits does not carry the tool.
    // Seeded from the preference the first time the section is drawn: the
    // buffer only exists to survive between frames, it is not a second source
    // of truth.
    if model.state.assistant_path.is_empty() && !model.prefs.assistant_path.is_empty() {
        model.state.assistant_path = model.prefs.assistant_path.clone();
    }
    row(ui, Msg::AssistantTool.text(lang), label_w, |ui| {
        let mut path = model.state.assistant_path.clone();
        let edit = ui.add(
            egui::TextEdit::singleline(&mut path)
                .desired_width(TOOL_PATH_W)
                .hint_text(Msg::AssistantOnPath.text(lang))
                .font(theme::mono(theme::SIZE_MONO)),
        );
        if edit.changed() {
            model.state.assistant_path = path.clone();
        }
        // Applied when the field is left or Enter is pressed, not on every
        // keystroke: re-resolving the tool for each letter typed would make
        // "not an executable" flash under the fingers of someone halfway
        // through a path.
        if edit.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            if path != model.prefs.assistant_path {
                action = Action::Set(Setting::AssistantPath(path));
            }
        }
        if ui.button(Msg::AssistantLocate.text(lang)).clicked() {
            action = Action::ChooseAssistantTool;
        }
    });

    let typed = !model.state.assistant_path.trim().is_empty();
    if typed && !available {
        ui.label(
            RichText::new(Msg::AssistantBadPath.text(lang))
                .font(theme::font(theme::SIZE_SMALL))
                .color(theme::RED),
        );
    } else if let Some(path) = model.claude.filter(|_| !typed) {
        path_line(ui, &super::home::shorten_path(path, 60));
    }
    hint(ui, Msg::AssistantPathHint.text(lang));
    action
}

fn emulation_section(ui: &mut egui::Ui, model: &mut SettingsModel) -> Action {
    let mut action = Action::None;
    let prefs = model.prefs;
    let lang = model.lang;
    let label_w = label_column_w(
        ui,
        lang,
        &[Msg::FastForward, Msg::InstantResume, Msg::Confirmation, Msg::SaveSlot],
    );

    row(ui, Msg::FastForward.text(lang), label_w, |ui| {
        for &factor in FAST_FORWARD_FACTORS {
            if ui.selectable_label(prefs.fast_forward_factor == factor, format!("×{factor}")).clicked()
            {
                action = Action::Set(Setting::FastForward(factor));
            }
        }
    });
    hint(ui, Msg::FastForwardHint.text(lang));

    row(ui, Msg::InstantResume.text(lang), label_w, |ui| {
        let mut on = prefs.resume_on_launch;
        if checkbox(ui, &mut on, Msg::InstantResumeCheck.text(lang)).changed() {
            action = Action::Set(Setting::ResumeOnLaunch(on));
        }
    });
    hint(ui, Msg::InstantResumeHint.text(lang));

    row(ui, Msg::Confirmation.text(lang), label_w, |ui| {
        let mut on = prefs.confirm_on_quit;
        if checkbox(ui, &mut on, Msg::ConfirmQuitCheck.text(lang)).changed() {
            action = Action::Set(Setting::ConfirmOnQuit(on));
        }
    });

    row(ui, Msg::SaveSlot.text(lang), label_w, |ui| {
        for slot in 0..SLOT_COUNT {
            if ui.selectable_label(prefs.save_slot == slot, slot.to_string()).clicked() {
                action = Action::Set(Setting::Slot(slot));
            }
        }
    });
    hint(ui, Msg::SaveSlotHint.text(lang));

    action
}

fn folders_section(ui: &mut egui::Ui, model: &mut SettingsModel) -> Action {
    let mut action = Action::None;
    let lang = model.lang;

    folder_heading(ui, Msg::RomFolder.text(lang));
    path_line(ui, &super::home::shorten_path(model.library_dir, PATH_MAX_CHARS));
    ui.horizontal(|ui| {
        if ui.button(Msg::Choose.text(lang)).clicked() {
            action = Action::ChooseLibraryDir;
        }
        if ui
            .add_enabled(
                model.prefs.library_dir.is_some(),
                egui::Button::new(Msg::DefaultChoice.text(lang)),
            )
            .clicked()
        {
            action = Action::ResetLibraryDir;
        }
    });
    hint(ui, Msg::RomFolderHint.text(lang));

    ui.add_space(12.0);
    folder_heading(ui, Msg::ScreenshotFolder.text(lang));
    path_line(ui, &screenshot_dir_label(lang, model.prefs.screenshot_dir.as_deref()));
    ui.horizontal(|ui| {
        if ui.button(Msg::Choose.text(lang)).clicked() {
            action = Action::ChooseScreenshotDir;
        }
        if ui
            .add_enabled(
                model.prefs.screenshot_dir.is_some(),
                egui::Button::new(Msg::DefaultChoice.text(lang)),
            )
            .clicked()
        {
            action = Action::ResetScreenshotDir;
        }
    });
    hint(ui, Msg::ScreenshotFolderHint.text(lang));

    ui.add_space(12.0);
    folder_heading(ui, Msg::SaveFolder.text(lang));
    path_line(ui, &save_dir_label(lang, model.prefs.save_dir.as_deref()));
    ui.horizontal(|ui| {
        if ui.button(Msg::Choose.text(lang)).clicked() {
            action = Action::ChooseSaveDir;
        }
        if ui
            .add_enabled(
                model.prefs.save_dir.is_some(),
                egui::Button::new(Msg::DefaultChoice.text(lang)),
            )
            .clicked()
        {
            action = Action::ResetSaveDir;
        }
    });
    hint(ui, Msg::SaveFolderHint.text(lang));
    if let Some(previous) = &model.prefs.previous_save_dir {
        ui.label(
            RichText::new(previous_save_dir_line(lang, previous))
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
    let lang = model.lang;

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
        RichText::new(Msg::AboutBlurb.text(lang))
            .size(theme::SIZE_BODY)
            .color(theme::TEXT_DIM),
    );

    ui.add_space(12.0);
    folder_heading(ui, Msg::Guide.text(lang));
    match &model.state.guide {
        Some(path) => {
            path_line(ui, &super::home::shorten_path(path, PATH_MAX_CHARS));
            if ui.button(Msg::OpenPdf.text(lang)).clicked() {
                action = Action::OpenGuide;
            }
        }
        None => {
            hint(ui, Msg::GuideMissing.text(lang));
        }
    }
    if let Some(notice) = &model.state.notice {
        ui.label(RichText::new(notice).size(theme::SIZE_SMALL).color(theme::RED));
    }

    ui.add_space(12.0);
    folder_heading(ui, Msg::AppFiles.text(lang));
    match model.config_dir {
        Some(dir) => path_line(ui, &super::home::shorten_path(dir, PATH_MAX_CHARS)),
        None => hint(ui, Msg::NoConfigDir.text(lang)),
    }
    hint(ui, Msg::AppFilesHint.text(lang));

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
fn row(ui: &mut egui::Ui, label: &str, label_w: f32, controls: impl FnOnce(&mut egui::Ui)) {
    let label_w = label_w.min(ui.available_width() * 0.5);
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

/// Width of a section's label column: the widest of the labels it is about to
/// render, in the language it is about to render them in, plus a gap — never a
/// constant sized on one language's longest word.
fn label_column_w(ui: &egui::Ui, lang: Lang, labels: &[Msg]) -> f32 {
    let font = theme::font(theme::SIZE_BODY);
    let widest = labels
        .iter()
        .map(|msg| {
            ui.painter()
                .layout_no_wrap(msg.text(lang).to_owned(), font.clone(), theme::TEXT)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max);
    (widest + LABEL_GAP).ceil().min(LABEL_MAX_W)
}

/// Heading of a block inside a section (a folder, the guide): the name of what
/// the lines under it describe.
fn folder_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::strong(theme::SIZE_BODY)).color(theme::TEXT));
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

/// Widths of the three columns of the bindings list, for one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BindColumns {
    button: f32,
    key: f32,
    pad: f32,
}

impl BindColumns {
    /// What the three of them need together. Past it is what the controller
    /// drawing may use.
    fn total(self) -> f32 {
        self.button + self.key + self.pad
    }
}

/// Measure the three columns on the cells they are about to hold rather than
/// on a constant: `Bouton`/`Button` and `Gâchette L2 (LT)`/`L trigger (LT)`
/// are not the same width, and a column sized on the French leaves a gap in
/// English while one sized on the English clips the French.
///
/// The capture prompts are deliberately left out of the measurement: only one
/// row is ever waiting for a press, and widening all twelve for it would push
/// the controller drawing off the section for good.
fn bind_columns(ui: &egui::Ui, model: &SettingsModel) -> BindColumns {
    let lang = model.lang;
    let body = theme::font(theme::SIZE_BODY);
    let small = theme::font(theme::SIZE_SMALL);
    let mono = theme::mono(theme::SIZE_MONO);
    let measure = |text: String, font: egui::FontId| {
        ui.painter().layout_no_wrap(text, font, theme::TEXT).size().x
    };
    let mut columns = BindColumns {
        button: measure(Msg::ColumnButton.text(lang).to_owned(), small.clone()),
        key: measure(Msg::ColumnKeyboard.text(lang).to_owned(), small.clone()),
        pad: measure(Msg::ColumnPad.text(lang).to_owned(), small),
    };
    for name in input::BUTTONS {
        let key = input::shown_key(&model.prefs.keymap, name)
            .map(input::key_label)
            .unwrap_or_else(|| "—".to_string());
        columns.button =
            columns.button.max(measure(button_label(lang, name).to_owned(), body.clone()));
        columns.key = columns.key.max(measure(key, mono.clone()));
        columns.pad = columns
            .pad
            .max(measure(pad::binding_label(lang, &model.prefs.pad_map, name), mono.clone()));
    }
    // The cells are `selectable_label`s, which carry egui's button padding on
    // both sides; without it the widest binding would touch the next column.
    let padding = 2.0 * ui.spacing().button_padding.x + BIND_COL_GAP;
    BindColumns {
        button: (columns.button + BIND_COL_GAP).ceil().max(BUTTON_COL_MIN_W),
        key: (columns.key + padding).ceil().max(BIND_COL_MIN_W),
        pad: (columns.pad + padding).ceil().max(PAD_COL_MIN_W),
    }
}

/// A checkbox with the weight the rest of the shell's controls have.
///
/// Still an `egui::Checkbox` — same semantics, same keyboard behaviour — but
/// egui paints its box from the widget visuals, and those are tuned for a
/// *filled* control: on this palette the resting box came out as a 14-point
/// square of panel grey outlined with a hairline nobody could see, which is
/// what made `Plein écran` and `Compteur d'images` read as blemishes rather
/// than controls. A checked box is filled with the accent, like a selected
/// choice; an unchecked one carries a border in the secondary text colour, the
/// same value as the label next to it.
fn checkbox(ui: &mut egui::Ui, on: &mut bool, label: &str) -> egui::Response {
    let checked = *on;
    {
        let spacing = ui.spacing_mut();
        spacing.icon_width = 18.0;
        spacing.icon_width_inner = 11.0;
        spacing.icon_spacing = 8.0;
    }
    let widgets = &mut ui.visuals_mut().widgets;
    for (state, border) in [
        (&mut widgets.inactive, theme::TEXT_DIM.gamma_multiply(0.8)),
        (&mut widgets.hovered, theme::ACCENT),
        (&mut widgets.active, theme::ACCENT),
    ] {
        state.bg_fill = if checked { theme::ACCENT } else { theme::BG_WIDGET };
        state.weak_bg_fill = state.bg_fill;
        state.bg_stroke =
            egui::Stroke::new(1.5, if checked { theme::ACCENT } else { border });
        state.fg_stroke = egui::Stroke::new(2.0, egui::Color32::WHITE);
    }
    ui.checkbox(on, RichText::new(label).size(theme::SIZE_BODY).color(theme::TEXT))
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
pub fn screenshot_dir_label(lang: Lang, dir: Option<&Path>) -> String {
    match dir {
        Some(dir) => super::home::shorten_path(dir, PATH_MAX_CHARS),
        None => Msg::BesideRomShots.text(lang).to_string(),
    }
}

/// Same for the save folder (`.srm` battery saves, `.state`/`.stateN` slots and
/// the `.resume` session state).
pub fn save_dir_label(lang: Lang, dir: Option<&Path>) -> String {
    match dir {
        Some(dir) => super::home::shorten_path(dir, PATH_MAX_CHARS),
        None => Msg::BesideRom.text(lang).to_string(),
    }
}

/// Line shown under the save folder when a folder was configured before the
/// current setting (`prefs.previous_save_dir`): what it still holds is read,
/// never written to again, so clearing or changing the folder cannot look like
/// lost progress.
pub fn previous_save_dir_line(lang: Lang, previous: &Path) -> String {
    i18n::previous_save_dir(lang, &super::home::shorten_path(previous, PATH_MAX_CHARS))
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
        assert_eq!(Section::ALL.len(), 7);
        let mut labels: Vec<&str> = Section::ALL.iter().map(|s| s.label(Lang::Fr)).collect();
        // The order the brief fixes, top to bottom in the section column.
        assert_eq!(
            labels,
            vec!["Affichage", "Audio", "Émulation", "Entrées", "Assistant", "Dossiers", "À propos"]
        );
        assert_eq!(
            Section::ALL.iter().map(|s| s.label(Lang::En)).collect::<Vec<_>>(),
            vec!["Display", "Audio", "Emulation", "Controls", "Assistant", "Folders", "About"]
        );
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), Section::ALL.len());
        assert_eq!(Section::default(), Section::Display);
    }

    /// The ladder no longer starts at the native picture and no longer speaks
    /// in multipliers: every step is named by the window it produces, the
    /// unusable one comes last, and the label has to be the size the renderer
    /// would actually ask for — a label that drifts from `zoomed_dims` is a
    /// promise the window does not keep.
    #[test]
    fn the_window_sizes_are_labelled_in_pixels_with_the_native_one_last() {
        assert_eq!(
            ZOOM_CHOICES.iter().map(|&(z, _)| z).collect::<Vec<_>>(),
            vec![2, 3, 4, 5, 1],
            "the ladder must not start at 256x224, and must end there"
        );
        let steps: Vec<u8> = ZOOM_CHOICES[..ZOOM_CHOICES.len() - 1].iter().map(|&(z, _)| z).collect();
        for &(zoom, dims) in ZOOM_CHOICES {
            let (w, h) = crate::render::zoomed_dims(zoom, Aspect::PixelPerfect);
            assert!(dims.contains(&w.to_string()), "{dims} does not name its width {w}");
            assert!(dims.contains(&h.to_string()), "{dims} does not name its height {h}");
            assert!(!dims.starts_with('×'), "{dims} still reads as a multiplier");
            // The size itself is a machine value and reads the same in both;
            // only the native step carries a word, and it carries it in both.
            for lang in Lang::ALL {
                let label = zoom_label(lang, zoom, dims);
                assert!(label.contains(dims), "{label} lost its size");
                assert_eq!(zoom != NATIVE_ZOOM, label == dims, "{label}");
            }
        }
        assert!(zoom_label(Lang::Fr, NATIVE_ZOOM, "256 × 224").starts_with("Taille native"));
        assert!(zoom_label(Lang::En, NATIVE_ZOOM, "256 × 224").starts_with("Native size"));
        // F1-F4 land on the four usable steps, in the ladder's own order —
        // never on the native one.
        assert_eq!(ZOOM_HOTKEYS.to_vec(), steps);
        assert!(!ZOOM_HOTKEYS.contains(&1));
        // The adaptive default is always one of the steps the panel offers, so
        // a window nobody sized still shows a selected entry.
        for height in [0u32, 400, 600, 768, 900, 1080, 1440, 2160, 4320] {
            let zoom = crate::render::default_zoom(height);
            assert!(zoom > 1, "{height}: the default must never be the native size");
            assert!(steps.contains(&zoom), "{height}: {zoom} is not on the ladder");
        }
    }

    #[test]
    fn the_display_choices_match_what_the_renderer_and_the_hotkeys_offer() {
        // The steps here must be the ones `set_zoom` can reach.
        assert!(ZOOM_CHOICES.iter().all(|&(z, _)| z <= crate::render::MAX_ZOOM));
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
            screenshot_dir_label(Lang::Fr, None),
            "À côté de la ROM, dans Screenshots/"
        );
        assert_eq!(
            screenshot_dir_label(Lang::En, None),
            "Beside the ROM, in Screenshots/"
        );
        assert_eq!(screenshot_dir_label(Lang::Fr, Some(Path::new("/shots"))), "/shots");
        assert_eq!(save_dir_label(Lang::Fr, None), "À côté de la ROM");
        assert_eq!(save_dir_label(Lang::En, None), "Beside the ROM");
        assert_eq!(save_dir_label(Lang::Fr, Some(Path::new("/saves"))), "/saves");
        // A long folder is elided in its middle rather than widening the panel.
        let long = PathBuf::from("/Volumes/Backup").join("a".repeat(80));
        let label = save_dir_label(Lang::Fr, Some(&long));
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

    /// The same in one named language, which is what the two-language
    /// assertions walk.
    fn draw_in(
        section: Section,
        prefs: &Prefs,
        state: &mut SettingsUi,
        lang: Lang,
    ) -> String {
        draw_full(section, prefs, state, (1280.0, 800.0), JoypadState::default(), lang).1
    }

    /// The same with buttons physically held, which only `Entrées` shows.
    fn draw_pressed(
        prefs: &Prefs,
        state: &mut SettingsUi,
        pressed: JoypadState,
    ) -> (String, Vec<egui::Shape>) {
        let (_, text, shapes) =
            draw_full(Section::Inputs, prefs, state, (1280.0, 800.0), pressed, Lang::Fr);
        (text, shapes)
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
        draw_full(section, prefs, state, size, JoypadState::default(), Lang::Fr)
    }

    fn draw_full(
        section: Section,
        prefs: &Prefs,
        state: &mut SettingsUi,
        size: (f32, f32),
        pressed_buttons: JoypadState,
        lang: Lang,
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
                        claude: Some(Path::new("/usr/local/bin/claude")),
                        app_name: "Prisme",
                        version: "0.0.0",
                        prefs,
                        fullscreen: false,
                        zoom: crate::render::FALLBACK_ZOOM,
                        library_dir: Path::new("roms"),
                        config_dir: Some(Path::new("/config/Prisme")),
                        pressed: pressed_buttons,
                        state,
                        lang,
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
                let label = tab.label(Lang::Fr);
                assert!(text.contains(label), "tab {label:?} is missing: {text}");
            }
            // The rule is spent once: four segments, under the active tab.
            let segments: Vec<egui::Color32> = shapes
                .iter()
                .filter_map(|s| match s {
                    egui::Shape::Rect(r) if r.rect.height() <= theme::SPECTRAL_RULE_H => Some(r.fill),
                    _ => None,
                })
                .collect();
            assert_eq!(segments, theme::ACCENTS.to_vec(), "{:?}", section.label(Lang::Fr));
            assert!(
                !shapes.iter().any(|s| matches!(s, egui::Shape::Rect(r) if r.fill == theme::VEIL)),
                "{:?} still darkens a screen behind it",
                section.label(Lang::Fr)
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
                    section.label(Lang::Fr)
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
                    text.contains(button_label(Lang::Fr, name)),
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
            assert_eq!(produced, Action::None, "section {:?}", section.label(Lang::Fr));
            assert_eq!(state.section, section, "the section must not change by itself");
            assert!(text.contains("Réglages"), "the panel title is missing: {text}");
            assert!(
                text.contains(section.label(Lang::Fr)),
                "section {:?} not listed",
                section.label(Lang::Fr)
            );
        }
        assert_eq!(prefs, Prefs::default(), "the panel must never write a preference");
    }

    /// Each section must actually paint its own controls; an empty section
    /// would still pass the no-action test above.
    #[test]
    fn every_section_paints_the_settings_it_owns() {
        let prefs = Prefs::default();
        let expected: [(Section, &[&str]); 6] = [
            (
                Section::Display,
                &[
                    "Taille de la fenêtre",
                    "Filtre",
                    "Ratio",
                    "Plein écran",
                    "768 × 672",
                    "Taille native (256 × 224)",
                ],
            ),
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
                assert!(
                    text.contains(label),
                    "{:?} is missing {label:?}: {text}",
                    section.label(Lang::Fr)
                );
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
        assert!(previous_save_dir_line(Lang::Fr, Path::new("/old-saves")).contains("toujours relu"));
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
            let json = format!("{{\"zoom\": {zoom}, \"zoom_chosen\": true}}");
            assert_eq!(Prefs::from_json(&json).expect("parse").zoom, Some(zoom));
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
            for lang in Lang::ALL {
                assert_ne!(button_label(lang, name), "?", "no label for {name}");
            }
        }
        assert_eq!(button_label(Lang::Fr, "Turbo"), "?");
        // The eight buttons that carry a printed legend keep it in both.
        for name in ["A", "B", "X", "Y", "L", "R", "Start", "Select"] {
            for lang in Lang::ALL {
                assert_eq!(button_label(lang, name), name);
            }
        }
        assert_eq!(button_label(Lang::En, "Up"), "Up");
        assert_eq!(button_label(Lang::Fr, "Up"), "Haut");
        // Outside a capture the cell shows the binding itself…
        assert_eq!(binding_cell(Lang::Fr, "Z", false, Device::Keyboard), "Z");
        assert_eq!(binding_cell(Lang::Fr, "Bouton bas", false, Device::Gamepad), "Bouton bas");
        // …and during one, the prompt naming the device it waits for.
        assert_eq!(binding_cell(Lang::Fr, "Z", true, Device::Keyboard), "Touche…");
        assert_eq!(binding_cell(Lang::Fr, "Bouton bas", true, Device::Gamepad), "Bouton…");
        assert_eq!(binding_cell(Lang::En, "Z", true, Device::Keyboard), "Key…");
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
        assert!(text.contains("Space"), "the rebound key is missing: {text}");
        assert!(
            text.contains(crate::pad::pad_label(Lang::Fr, crate::pad::Button::North)),
            "{text}"
        );
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
                        claude: Some(Path::new("/usr/local/bin/claude")),
                        app_name: "Prisme",
                        version: "0.0.0",
                        prefs: &prefs,
                        fullscreen: false,
                        zoom: crate::render::FALLBACK_ZOOM,
                        library_dir: Path::new("roms"),
                        config_dir: None,
                        pressed: JoypadState::default(),
                        state: &mut state,
                        lang: Lang::Fr,
                    },
                );
            });
            assert_eq!(produced, Action::None, "a captured key must not act on the panel");
        }
        assert!(state.capture.is_active(), "only the event loop ends a capture");
        assert_eq!(state.section, Section::Inputs);
    }

    /// Circles the panel painted, as (fill, radius).
    fn circles(shapes: &[egui::Shape]) -> Vec<(egui::Color32, f32)> {
        shapes
            .iter()
            .filter_map(|s| match s {
                egui::Shape::Circle(c) => Some((c.fill, c.radius)),
                _ => None,
            })
            .collect()
    }

    /// The controller is drawn in `Entrées` and nowhere else, and it carries
    /// the four prism accents on its face buttons — the legend of the European
    /// pad, which is the whole reason the drawing is there.
    #[test]
    fn the_entrees_section_draws_the_controller_with_its_four_coloured_buttons() {
        let prefs = Prefs::default();
        let mut state = SettingsUi::default();
        let (_, _, shapes) = draw_at(Section::Inputs, &prefs, &mut state, (1280.0, 800.0));
        let fills: Vec<egui::Color32> = circles(&shapes).into_iter().map(|(f, _)| f).collect();
        for accent in theme::ACCENTS {
            assert!(fills.contains(&accent), "no {accent:?} face button on the drawing");
        }
        // …and no other section spends the four accents on a drawing of its own.
        for section in [Section::Display, Section::Audio, Section::Folders] {
            let mut state = SettingsUi::default();
            let (_, _, shapes) = draw_at(section, &prefs, &mut state, (1280.0, 800.0));
            let fills: Vec<egui::Color32> = circles(&shapes).into_iter().map(|(f, _)| f).collect();
            assert!(
                !theme::ACCENTS.iter().all(|a| fills.contains(a)),
                "{:?} draws a controller",
                section.label(Lang::Fr)
            );
        }
    }

    /// The live half: a button held on the keyboard or on a controller must
    /// change what the drawing paints, or the section is not the tester it says
    /// it is.
    #[test]
    fn a_held_button_lights_up_on_the_drawing() {
        let prefs = Prefs::default();
        let mut state = SettingsUi::default();
        let (_, resting) = draw_pressed(&prefs, &mut state, JoypadState::default());
        let mut state = SettingsUi::default();
        let (_, held) =
            draw_pressed(&prefs, &mut state, JoypadState { b: true, ..Default::default() });
        let (resting, held) = (circles(&resting), circles(&held));
        assert!(resting.iter().any(|(f, _)| *f == theme::YELLOW), "B is not yellow at rest");
        assert!(
            !held.iter().any(|(f, _)| *f == theme::YELLOW),
            "B kept its resting colour while it was held"
        );
        // It is still a yellow button, only a lit one — a pressed button must
        // not lose the colour it is identified by.
        let lit = theme::YELLOW.lerp_to_gamma(egui::Color32::WHITE, 0.30);
        assert!(held.iter().any(|(f, _)| *f == lit), "B did not light up: {held:?}");
        // A lit button is also ringed, so it reads as pressed and not merely as
        // a lighter shade of yellow: one more circle than at rest.
        assert_eq!(held.len(), resting.len() + 1, "the pressed button carries no ring");
    }

    /// Clicking a button *on the drawing* must start the same capture clicking
    /// its cell does, and pointing at it must light its row. The pointer is
    /// aimed at whatever the panel actually painted red — the A button — so
    /// this follows the drawing wherever the layout puts it instead of
    /// hard-coding a position that would rot.
    #[test]
    fn clicking_a_button_on_the_drawing_rebinds_it_and_hovering_it_lights_its_row() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let prefs = Prefs::default();
        let mut state = SettingsUi { open: true, section: Section::Inputs, ..Default::default() };
        let size = egui::vec2(1280.0, 800.0);
        let run = |pointer: Option<egui::Pos2>, click: bool, state: &mut SettingsUi| {
            let mut events = Vec::new();
            if let Some(pos) = pointer {
                events.push(egui::Event::PointerMoved(pos));
                if click {
                    events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    });
                    events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: Default::default(),
                    });
                }
            }
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                time: Some(1.0),
                events,
                ..Default::default()
            };
            let output = ctx.run(input, |ctx| {
                show(
                    ctx,
                    &mut SettingsModel {
                        claude: Some(Path::new("/usr/local/bin/claude")),
                        app_name: "Prisme",
                        version: "0.0.0",
                        prefs: &prefs,
                        fullscreen: false,
                        zoom: crate::render::FALLBACK_ZOOM,
                        library_dir: Path::new("roms"),
                        config_dir: None,
                        pressed: JoypadState::default(),
                        state,
                        lang: Lang::Fr,
                    },
                );
            });
            let mut shapes = Vec::new();
            fn walk(shape: &egui::Shape, out: &mut Vec<egui::Shape>) {
                match shape {
                    egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                    other => out.push(other.clone()),
                }
            }
            for clipped in &output.shapes {
                walk(&clipped.shape, &mut shapes);
            }
            shapes
        };

        // Settle the layout, then find the red button the drawing painted.
        let mut shapes = Vec::new();
        for _ in 0..4 {
            shapes = run(None, false, &mut state);
        }
        let a = shapes
            .iter()
            .find_map(|s| match s {
                egui::Shape::Circle(c) if c.fill == theme::RED => Some(c.center),
                _ => None,
            })
            .expect("the drawing painted no red A button");

        // Pointing at it lights the A row of the list…
        let hovered = run(Some(a), false, &mut state);
        assert_eq!(state.pad_hover, Some("A"), "the drawing does not report what it is under");
        let bands = hovered
            .iter()
            .filter(|s| matches!(s, egui::Shape::Rect(r) if r.fill == theme::BG_WIDGET))
            .count();
        assert!(bands > 0, "no row was highlighted while the drawing was hovered");

        // …and clicking it opens the very capture the cell would have opened.
        assert!(!state.capture.is_active());
        let _ = run(Some(a), true, &mut state);
        assert_eq!(
            state.capture.pending(),
            Some(("A", Device::Keyboard)),
            "clicking the drawn A button did not start its capture"
        );

        // A click on the body of the pad binds nothing: only the buttons do.
        state.capture.cancel();
        // Straight below A, clear of its rim and of B, still on the shell.
        let body = a + egui::vec2(0.0, 40.0);
        let _ = run(Some(body), true, &mut state);
        assert_eq!(state.capture.pending(), None, "the shell of the pad is not a button");
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
        assert!(text.contains("Touche…"), "{text}");
        assert!(!text.contains("Arrow Up"), "the row must show the prompt: {text}");

        let mut state = SettingsUi::default();
        state.capture.start("Up", Device::Gamepad);
        state.capture.notice = Some("conflit de test".to_string());
        let (_, text) = draw(Section::Inputs, &prefs, &mut state);
        assert!(text.contains("Bouton…"), "{text}");
        assert!(text.contains("Appuyez sur un bouton de manette pour Haut"), "{text}");
        assert!(text.contains("conflit de test"), "{text}");
    }

    /// The row someone reaches for when they cannot read the screen they are
    /// on: first in the section, three answers, and the two real languages
    /// named in their own words.
    #[test]
    fn the_language_row_leads_the_display_section_and_names_itself_in_endonyms() {
        assert_eq!(language_choices(), [None, Some(Lang::Fr), Some(Lang::En)]);
        let prefs = Prefs::default();
        for lang in Lang::ALL {
            let mut state = SettingsUi::default();
            let text = draw_in(Section::Display, &prefs, &mut state, lang);
            assert!(text.contains(Msg::Language.text(lang)), "no language row: {text}");
            // Endonyms, never "Anglais" or "French".
            assert!(text.contains("Français"), "{text}");
            assert!(text.contains("English"), "{text}");
            assert!(!text.contains("Anglais"), "{text}");
            assert!(text.contains(Msg::LanguageSystem.text(lang)), "{text}");
            // Above the window-size row, which is what "first in the section"
            // means once both are painted.
            let language = text.find(Msg::Language.text(lang)).expect("language row");
            let size = text.find(Msg::WindowSize.text(lang)).expect("window size row");
            assert!(language < size, "the language row is not the first one: {text}");
        }
    }

    /// The stored preference is what the row shows as chosen, and `system` —
    /// the default — selects none of the two languages.
    #[test]
    fn the_language_row_follows_the_stored_preference() {
        let mut prefs = Prefs::default();
        assert_eq!(Lang::from_pref(&prefs.language), None, "no choice is the default");
        for chosen in [Lang::Fr, Lang::En] {
            prefs.language = chosen.as_pref().to_string();
            assert_eq!(Lang::from_pref(&prefs.language), Some(chosen));
        }
    }

    /// Every section, in both languages, with the closing line of each: a
    /// section that fits in French and scrolls its last hint out of sight in
    /// English is a section nobody has looked at.
    #[test]
    fn every_section_is_whole_in_both_languages() {
        let prefs = Prefs::default();
        let tails: [(Section, Msg); 6] = [
            (Section::Display, Msg::ShowFpsCheck),
            (Section::Audio, Msg::VolumeHint),
            (Section::Inputs, Msg::PlayersHint),
            (Section::Emulation, Msg::SaveSlotHint),
            (Section::Folders, Msg::SaveFolderHint),
            (Section::About, Msg::AppFilesHint),
        ];
        for lang in Lang::ALL {
            for (section, tail) in tails {
                let mut state = SettingsUi::default();
                let text = draw_in(section, &prefs, &mut state, lang);
                // Long hints wrap, so the painted text carries them in pieces:
                // the first words are enough to say the line was reached.
                let head: String = tail.text(lang).split_whitespace().take(3).collect::<Vec<_>>().join(" ");
                assert!(
                    text.contains(&head),
                    "{:?} is cut off in {lang}: {head:?} never painted: {text}",
                    section.label(lang)
                );
                assert!(text.contains(section.label(lang)), "{text}");
            }
        }
    }

    /// The English capture must hold no French at all — the failure mode of a
    /// half-threaded language is precisely a screen where three labels out of
    /// twenty stayed behind.
    #[test]
    fn nothing_french_survives_in_an_english_section() {
        let prefs = Prefs::default();
        for section in Section::ALL {
            let mut state = SettingsUi::default();
            let text = draw_in(section, &prefs, &mut state, Lang::En);
            for french in [
                "Réglages",
                "Affichage",
                "Entrées",
                "Émulation",
                "Échap",
                "Retour",
                "Taille",
                "Dossier",
                "Aucun",
                "Manette",
                "Bouton",
            ] {
                assert!(
                    !text.contains(french),
                    "{:?} still says {french:?} in English: {text}",
                    section.label(Lang::En)
                );
            }
        }
    }

    /// The three columns of the bindings list are measured on what they hold,
    /// so they follow the language instead of being sized for French and left
    /// gaping in English.
    #[test]
    fn the_bindings_columns_are_measured_in_the_language_they_render() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let prefs = Prefs::default();
        let mut widths = Vec::new();
        for lang in Lang::ALL {
            let mut state = SettingsUi::default();
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let model = SettingsModel {
                        claude: Some(Path::new("/usr/local/bin/claude")),
                        app_name: "Prisme",
                        version: "0.0.0",
                        prefs: &prefs,
                        fullscreen: false,
                        zoom: crate::render::FALLBACK_ZOOM,
                        library_dir: Path::new("roms"),
                        config_dir: None,
                        pressed: JoypadState::default(),
                        state: &mut state,
                        lang,
                    };
                    widths.push(bind_columns(ui, &model));
                });
            });
        }
        let (fr, en) = (widths[0], widths[1]);
        // `Haut`/`Bas`/`Gauche`/`Droite` are longer than `Up`/`Down`/`Left`/
        // `Right`, so the button column gives ground in English…
        assert!(en.button < fr.button, "{fr:?} vs {en:?}");
        // …while the key column does not move at all: key names are the
        // legends printed on the keys and are never translated.
        assert_eq!(en.key, fr.key, "a key name changed with the language");
        // Every column stays above its floor, and the three of them are the
        // list's width.
        for columns in [fr, en] {
            assert!(columns.button >= BUTTON_COL_MIN_W, "{columns:?}");
            assert!(columns.key >= BIND_COL_MIN_W, "{columns:?}");
            assert!(columns.pad >= PAD_COL_MIN_W, "{columns:?}");
            assert_eq!(columns.total(), columns.button + columns.key + columns.pad);
        }
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
                assistant_path: String::new(),
                open: true,
                section: Section::About,
                guide,
                notice: Some("erreur de test".to_string()),
                folder_notice: None,
                capture: Capture::default(),
                pad_hover: None,
            };
            let mut produced = Action::Quit;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                produced = show(
                    ctx,
                    &mut SettingsModel {
                        claude: Some(Path::new("/usr/local/bin/claude")),
                        app_name: "Prisme",
                        version: "0.0.0",
                        prefs: &prefs,
                        fullscreen: true,
                        zoom: crate::render::FALLBACK_ZOOM,
                        library_dir: Path::new("roms"),
                        config_dir: None,
                        pressed: JoypadState::default(),
                        state: &mut state,
                        lang: Lang::Fr,
                    },
                );
            });
            assert_eq!(produced, Action::None);
        }
    }
}

