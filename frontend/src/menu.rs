//! Native macOS menu bar (muda crate), installed once against `NSApp` and
//! polled for click events from the winit loop in `video.rs`.
//!
//! muda's `Menu` is a thin `Rc`-wrapped handle over a platform menu object;
//! on macOS `init_for_nsapp` hands the underlying `NSMenu` to
//! `NSApp.setMainMenu`, which retains it natively, but `AppMenu` is kept
//! alive for the run's duration anyway (stored on `App`) so item state
//! (enabled/text/checkmark) can be queried or changed later without
//! re-querying AppKit.
//!
//! Layout: App / Fichier / Émulation / Affichage. **Actions only** — every
//! *setting* moved to the egui settings panel (`ui::settings`, opened by the
//! app menu's `Réglages…` / Cmd+, / the `,` hotkey), so an option has exactly
//! one place to be changed and there is no second, retained copy of its state
//! to keep in sync. What is left here either changes nothing persistent (open
//! a ROM, pause, reset, save/load state, screenshot, SPC export, fullscreen,
//! quit) or navigates (Accueil, Réglages).
//!
//! Every item doubles a keyboard hotkey handled in `video.rs::handle_key`
//! (Accueil = Échap, Open ROM = `O`, Pause = `P`, Reset = F6, Save/Load State =
//! F5/F9, Slot suivant = F7, Capture = F12, Export SPC = F8, Plein écran =
//! F11, Réglages = `,`), so the whole feature set stays reachable on platforms
//! without this menu (Windows/Linux).
//!
//! Only built for `target_os = "macos"`: this is a macOS-specific menu bar,
//! not a cross-platform one (Windows/Linux would need `init_for_hwnd`/GTK
//! wiring this crate doesn't attempt).

#![cfg(target_os = "macos")]

use muda::accelerator::{Accelerator, Code, Modifiers, CMD_OR_CTRL};
use muda::{Menu, MenuItem, PredefinedMenuItem, Submenu};

use crate::APP_NAME;

/// Stable ids for the items `video.rs` dispatches on after a
/// `MenuEvent::receiver().try_recv()`. Separators are the only predefined items
/// left and need no id.
pub const HOME_ID: &str = "prisme.home";
pub const SETTINGS_ID: &str = "prisme.settings";
pub const OPEN_ROM_ID: &str = "prisme.open-rom";
pub const SCREENSHOT_ID: &str = "prisme.screenshot";
pub const EXPORT_SPC_ID: &str = "prisme.export-spc";
pub const NEXT_SLOT_ID: &str = "prisme.next-slot";
pub const PAUSE_RESUME_ID: &str = "prisme.pause-resume";
pub const RESET_ID: &str = "prisme.reset";
pub const SAVE_STATE_ID: &str = "prisme.save-state";
pub const LOAD_STATE_ID: &str = "prisme.load-state";
pub const QUIT_ID: &str = "prisme.quit";
pub const FULLSCREEN_ID: &str = "prisme.fullscreen";

/// Live handles for the menu items `video.rs` needs after construction.
/// Held on `App` for the run's lifetime (see module docs). Every one of them
/// is a plain `MenuItem`: with the settings gone there is no `CheckMenuItem`
/// left, hence no AppKit-owned checkmark to re-derive after a click.
pub struct AppMenu {
    /// `Fichier > Accueil` (Échap): suspends the session and shows the home
    /// screen. No accelerator — Escape is handled directly by `video.rs`, and
    /// a menu accelerator would make AppKit swallow it.
    pub home: MenuItem,
    /// App menu `Réglages…` (Cmd+,, the macOS convention): opens the egui
    /// settings panel, which is the only place a setting can be changed by
    /// pointer. Also bound to `,` in `video.rs`.
    pub settings: MenuItem,
    pub open_rom: MenuItem,
    /// `Fichier > Capture d'écran`; also bound to F12 in `video.rs`.
    pub screenshot: MenuItem,
    /// `Fichier > Exporter la musique (.spc)`.
    pub export_spc: MenuItem,
    pub pause_resume: MenuItem,
    pub reset: MenuItem,
    pub save_state: MenuItem,
    pub load_state: MenuItem,
    /// `Émulation > Slot suivant`; also bound to F7. The slot *number* itself
    /// is a setting and lives in the panel; stepping to the next one is an
    /// action, so it stays here.
    pub next_slot: MenuItem,
    /// `Affichage > Plein écran` (F11, also Ctrl+Cmd+F on macOS). A plain
    /// `MenuItem`, not a `CheckMenuItem`: the fullscreen/windowed state is
    /// visually obvious (the whole screen changes), so there is no checkmark
    /// to keep in sync with `Window::fullscreen()`.
    pub fullscreen: MenuItem,
    /// App-menu (leftmost) Quit, Cmd+Q. A custom item rather than
    /// `PredefinedMenuItem::quit` so its click routes through our
    /// `MenuEvent` channel and we exit the winit loop cleanly (which flushes
    /// battery SRAM) instead of AppKit calling `terminate:` and killing the
    /// process before the exit-time save runs.
    pub quit: MenuItem,
    /// File-menu Quit (no accelerator); shares `QUIT_ID` behavior.
    pub quit_file: MenuItem,
}

/// Builds the menu bar and installs it as the process's `NSApp` main menu.
/// Must run after `NSApplication` exists (i.e. after the winit event loop
/// has resumed at least once) — calling this earlier is a silent no-op on
/// macOS, per muda's own documented ordering (see its winit example, which
/// calls `init_for_nsapp` from `resumed`/`new_events` rather than before
/// `run_app`).
///
/// Takes no state: nothing here reflects a preference any more (see module
/// docs), so there is no restored checkmark to feed in at construction time
/// and no menu item that can fall out of step with `prefs.json`.
pub fn install() -> AppMenu {
    let menu_bar = Menu::new();

    // Application (leftmost) menu: AppKit titles it after the running
    // process itself (from `CFBundleName`/argv0) — muda has no way to
    // rename that from here.
    //
    // There is deliberately **no** `PredefinedMenuItem::about`: AppKit's
    // standard about panel opens a nested run loop, which re-enters winit's
    // event handler and aborts the process (crash confirmed in 0.1.0 and
    // 0.2.0 — see `docs/PUNCHLIST.md` and `crate::dialog`). The same
    // information lives in `Réglages… > À propos`, drawn in-app by egui.
    let app_menu = Submenu::new(APP_NAME, true);
    // Custom Quit (see AppMenu::quit): routes through our MenuEvent channel so
    // we can flush battery SRAM before exiting, unlike PredefinedMenuItem::quit
    // which invokes AppKit's terminate: directly.
    let quit = MenuItem::with_id(
        QUIT_ID,
        format!("Quitter {APP_NAME}"),
        true,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::KeyQ)),
    );
    // Cmd+, is macOS's standard shortcut for an application's settings; the
    // bare `,` hotkey in `video.rs` covers the other platforms.
    let settings = MenuItem::with_id(
        SETTINGS_ID,
        "Réglages…",
        true,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::Comma)),
    );
    let _ = app_menu.append_items(&[&settings, &PredefinedMenuItem::separator(), &quit]);
    let _ = menu_bar.append(&app_menu);

    let file_menu = Submenu::new("Fichier", true);
    let home = MenuItem::with_id(HOME_ID, "Accueil (Échap)", true, None);
    let open_rom = MenuItem::with_id(
        OPEN_ROM_ID,
        "Ouvrir une ROM…",
        true,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::KeyO)),
    );
    // No accelerator on these two: F12 is handled directly by `video.rs`, and
    // a menu accelerator would make AppKit swallow the key press and route it
    // here as a second, duplicate activation.
    let screenshot = MenuItem::with_id(SCREENSHOT_ID, "Capture d'écran (F12)", true, None);
    let export_spc =
        MenuItem::with_id(EXPORT_SPC_ID, "Exporter la musique (.spc)…", true, None);
    let quit_file = MenuItem::with_id(QUIT_ID, "Quitter", true, None);
    let _ = file_menu.append_items(&[
        &home,
        &open_rom,
        &PredefinedMenuItem::separator(),
        &screenshot,
        &export_spc,
        &PredefinedMenuItem::separator(),
        &quit_file,
    ]);
    let _ = menu_bar.append(&file_menu);

    let emulation_menu = Submenu::new("Émulation", true);
    let pause_resume = MenuItem::with_id(
        PAUSE_RESUME_ID,
        "Pause / Reprise",
        true,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::KeyP)),
    );
    let reset = MenuItem::with_id(
        RESET_ID,
        "Réinitialiser",
        true,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::KeyR)),
    );
    // Save/Load State act on the sidecar of the *current* slot (`prefs
    // .save_slot`, chosen in the settings panel), exactly like the F5/F9
    // hotkeys in video.rs. Cmd+S / Cmd+L.
    let save_state = MenuItem::with_id(
        SAVE_STATE_ID,
        "Sauvegarder l'état (F5)",
        true,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::KeyS)),
    );
    let load_state = MenuItem::with_id(
        LOAD_STATE_ID,
        "Charger l'état (F9)",
        true,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::KeyL)),
    );
    let next_slot = MenuItem::with_id(NEXT_SLOT_ID, "Slot suivant (F7)", true, None);
    let _ = emulation_menu.append_items(&[
        &pause_resume,
        &reset,
        &PredefinedMenuItem::separator(),
        &save_state,
        &load_state,
        &next_slot,
    ]);
    let _ = menu_bar.append(&emulation_menu);

    let view_menu = Submenu::new("Affichage", true);
    // Ctrl+Cmd+F: macOS's own system convention for toggling fullscreen
    // (distinct from the bare F11 the `video.rs` hotkey also answers to,
    // which some Mac keyboards/OS versions reserve for Mission Control).
    let fullscreen = MenuItem::with_id(
        FULLSCREEN_ID,
        "Plein écran (F11)",
        true,
        Some(Accelerator::new(Some(Modifiers::SUPER | Modifiers::CONTROL), Code::KeyF)),
    );
    // Zoom, filtre, ratio and the FPS readout are settings: they live in
    // `Réglages… > Affichage` (and on their F1-F4 / V / R / F hotkeys).
    let _ = view_menu.append_items(&[&fullscreen]);
    let _ = menu_bar.append(&view_menu);

    menu_bar.init_for_nsapp();

    AppMenu {
        home,
        settings,
        open_rom,
        screenshot,
        export_spc,
        pause_resume,
        reset,
        save_state,
        load_state,
        next_slot,
        fullscreen,
        quit,
        quit_file,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_menu_id_is_distinct() {
        // Two items sharing an id would dispatch to both actions; the only
        // intentional duplicate is Quit (app menu + Fichier).
        let mut ids: Vec<&str> = vec![
            HOME_ID,
            SETTINGS_ID,
            OPEN_ROM_ID,
            SCREENSHOT_ID,
            EXPORT_SPC_ID,
            NEXT_SLOT_ID,
            PAUSE_RESUME_ID,
            RESET_ID,
            SAVE_STATE_ID,
            LOAD_STATE_ID,
            QUIT_ID,
            FULLSCREEN_ID,
        ];
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }

    #[test]
    fn the_menu_carries_actions_only() {
        // Guard for the split introduced with the settings panel: an id whose
        // name reads as a setting would mean two places write the same
        // preference. Settings live in `ui::settings`, which reads `prefs`
        // directly and returns `Action::Set`.
        for id in [
            HOME_ID,
            SETTINGS_ID,
            OPEN_ROM_ID,
            SCREENSHOT_ID,
            EXPORT_SPC_ID,
            NEXT_SLOT_ID,
            PAUSE_RESUME_ID,
            RESET_ID,
            SAVE_STATE_ID,
            LOAD_STATE_ID,
            QUIT_ID,
            FULLSCREEN_ID,
        ] {
            assert!(id.starts_with("prisme."), "{id}");
            for setting in ["zoom", "filter", "aspect", "show-fps", "mute", "volume", "ff-", "slot-", "confirm-quit", "resume-on-launch"] {
                assert!(!id.contains(setting), "{id} looks like a setting; those moved to the panel");
            }
        }
    }
}
