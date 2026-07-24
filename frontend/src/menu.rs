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
//! Layout: App / Fichier / Émulation / Audio / Affichage. Every item here has
//! a keyboard equivalent handled directly in `video.rs::handle_key`, so the
//! whole feature set stays reachable on platforms without this menu.
//!
//! Only built for `target_os = "macos"`: this is a macOS-specific menu bar,
//! not a cross-platform one (Windows/Linux would need `init_for_hwnd`/GTK
//! wiring this crate doesn't attempt).

#![cfg(target_os = "macos")]

use muda::accelerator::{Accelerator, Code, CMD_OR_CTRL};
use muda::{AboutMetadata, CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

use crate::prefs::Prefs;
use crate::{APP_NAME, VERSION};

/// Stable ids for the items `video.rs` dispatches on after a
/// `MenuEvent::receiver().try_recv()`. Predefined items (About, separators)
/// need no id here: macOS runs their native action
/// (`orderFrontStandardAboutPanel:`) itself and never routes a click through
/// the `MenuEvent` channel we poll.
pub const OPEN_ROM_ID: &str = "prisme.open-rom";
pub const SCREENSHOT_ID: &str = "prisme.screenshot";
pub const EXPORT_SPC_ID: &str = "prisme.export-spc";
pub const NEXT_SLOT_ID: &str = "prisme.next-slot";
pub const RESUME_ID: &str = "prisme.resume-on-launch";
pub const PAUSE_RESUME_ID: &str = "prisme.pause-resume";
pub const RESET_ID: &str = "prisme.reset";
pub const SAVE_STATE_ID: &str = "prisme.save-state";
pub const LOAD_STATE_ID: &str = "prisme.load-state";
pub const SHOW_FPS_ID: &str = "prisme.show-fps";
pub const MUTE_ID: &str = "prisme.mute";
pub const VOLUME_UP_ID: &str = "prisme.volume-up";
pub const VOLUME_DOWN_ID: &str = "prisme.volume-down";
pub const FF_X2_ID: &str = "prisme.ff-x2";
pub const FF_X3_ID: &str = "prisme.ff-x3";
pub const FF_X4_ID: &str = "prisme.ff-x4";
pub const CONFIRM_QUIT_ID: &str = "prisme.confirm-quit";
pub const QUIT_ID: &str = "prisme.quit";

/// Fast-forward factors offered in `Émulation > Accéléré`, paired with the id
/// of their menu item. `video.rs` maps a click back to the factor with this
/// table, and `AppMenu::sync_fast_forward` re-derives the checkmarks from it.
pub const FF_FACTORS: &[(u8, &str)] = &[(2, FF_X2_ID), (3, FF_X3_ID), (4, FF_X4_ID)];

/// Save-state slots offered in `Émulation > Slot`, paired with their menu-item
/// id. Slot 0 is `<rom>.state`, slot N `<rom>.stateN` (see `state::state_path`).
pub const SLOT_IDS: &[&str] = &[
    "prisme.slot-0",
    "prisme.slot-1",
    "prisme.slot-2",
    "prisme.slot-3",
    "prisme.slot-4",
    "prisme.slot-5",
    "prisme.slot-6",
    "prisme.slot-7",
    "prisme.slot-8",
    "prisme.slot-9",
];

/// Live handles for the menu items `video.rs` needs after construction.
/// Held on `App` for the run's lifetime (see module docs).
pub struct AppMenu {
    pub open_rom: MenuItem,
    /// `Fichier > Capture d'écran`; also bound to F12 in `video.rs`.
    pub screenshot: MenuItem,
    /// `Fichier > Exporter la musique (.spc)`.
    pub export_spc: MenuItem,
    pub pause_resume: MenuItem,
    pub reset: MenuItem,
    pub save_state: MenuItem,
    pub load_state: MenuItem,
    /// `Émulation > Slot` choices, in `SLOT_IDS` order (slot = index). Radio
    /// group, re-derived after every click like `ff_items`.
    pub slot_items: Vec<CheckMenuItem>,
    /// `Émulation > Slot suivant`; also bound to F7.
    pub next_slot: MenuItem,
    /// `Émulation > Reprise instantanée`.
    pub resume_on_launch: CheckMenuItem,
    /// `Affichage > Afficher les FPS` (Cmd+F). AppKit toggles a
    /// `CheckMenuItem`'s own checked state itself before the click reaches our
    /// `MenuEvent` channel (see `poll_menu_events`), so `video.rs` reads
    /// `is_checked()` after the event rather than flipping a bool — the same
    /// item is also toggled programmatically from the `F` hotkey via
    /// `set_checked` to keep the two in sync.
    pub show_fps: CheckMenuItem,
    /// `Audio > Muet` (Cmd+M); same AppKit checkmark ownership as `show_fps`.
    pub mute: CheckMenuItem,
    pub volume_up: MenuItem,
    pub volume_down: MenuItem,
    /// Disabled item whose text shows the current volume percentage; updated
    /// through `set_volume_label`.
    pub volume_label: MenuItem,
    /// `Émulation > Accéléré` factor choices, in `FF_FACTORS` order. Used as a
    /// radio group: exactly one is checked, re-derived after every click.
    pub ff_items: Vec<CheckMenuItem>,
    /// `Fichier > Demander confirmation avant de quitter`.
    pub confirm_quit: CheckMenuItem,
    /// App-menu (leftmost) Quit, Cmd+Q. A custom item rather than
    /// `PredefinedMenuItem::quit` so its click routes through our
    /// `MenuEvent` channel and we exit the winit loop cleanly (which flushes
    /// battery SRAM) instead of AppKit calling `terminate:` and killing the
    /// process before the exit-time save runs.
    pub quit: MenuItem,
    /// File-menu Quit (no accelerator); shares `QUIT_ID` behavior.
    pub quit_file: MenuItem,
}

impl AppMenu {
    /// Re-derive the `Accéléré` radio group from `factor`. Needed after every
    /// click there: AppKit flips only the clicked item's own checkmark, and
    /// clicking the already-selected factor would otherwise leave the group
    /// with nothing checked.
    pub fn sync_fast_forward(&self, factor: u8) {
        for (item, &(value, _)) in self.ff_items.iter().zip(FF_FACTORS) {
            item.set_checked(value == factor);
        }
    }

    /// Re-derive the `Slot` radio group from `slot`, for the same reason as
    /// `sync_fast_forward`: AppKit only flips the clicked item's checkmark.
    pub fn sync_slot(&self, slot: u8) {
        for (i, item) in self.slot_items.iter().enumerate() {
            item.set_checked(i as u8 == slot);
        }
    }

    /// Show the current volume in the disabled `Audio` menu label.
    pub fn set_volume_label(&self, volume: u8) {
        self.volume_label.set_text(volume_label_text(volume));
    }
}

/// Text of the disabled volume indicator in the Audio menu.
fn volume_label_text(volume: u8) -> String {
    format!("Volume : {volume} %")
}

/// Builds the menu bar and installs it as the process's `NSApp` main menu.
/// Must run after `NSApplication` exists (i.e. after the winit event loop
/// has resumed at least once) — calling this earlier is a silent no-op on
/// macOS, per muda's own documented ordering (see its winit example, which
/// calls `init_for_nsapp` from `resumed`/`new_events` rather than before
/// `run_app`).
///
/// `prefs` supplies the restored state of every checkable item: AppKit owns a
/// `CheckMenuItem`'s checkmark, so those values have to be passed in at
/// construction time rather than applied afterwards.
pub fn install(prefs: &Prefs) -> AppMenu {
    let menu_bar = Menu::new();

    // Application (leftmost) menu: AppKit titles it after the running
    // process itself (from `CFBundleName`/argv0) — muda has no way to
    // rename that from here. `About` is muda's `PredefinedMenuItem`, so it
    // opens the OS's own standard about panel, fed with the app name and the
    // package version.
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
    let about = PredefinedMenuItem::about(
        Some(&format!("À propos de {APP_NAME}")),
        Some(AboutMetadata {
            name: Some(APP_NAME.to_string()),
            version: Some(VERSION.to_string()),
            ..Default::default()
        }),
    );
    let _ = app_menu.append_items(&[&about, &PredefinedMenuItem::separator(), &quit]);
    let _ = menu_bar.append(&app_menu);

    let file_menu = Submenu::new("Fichier", true);
    let open_rom = MenuItem::with_id(
        OPEN_ROM_ID,
        "Ouvrir une ROM…",
        true,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::KeyO)),
    );
    let confirm_quit = CheckMenuItem::with_id(
        CONFIRM_QUIT_ID,
        "Demander confirmation avant de quitter",
        true,
        prefs.confirm_on_quit,
        None,
    );
    // No accelerator on these two: F12 is handled directly by `video.rs`, and
    // a menu accelerator would make AppKit swallow the key press and route it
    // here as a second, duplicate activation.
    let screenshot = MenuItem::with_id(SCREENSHOT_ID, "Capture d'écran (F12)", true, None);
    let export_spc =
        MenuItem::with_id(EXPORT_SPC_ID, "Exporter la musique (.spc)…", true, None);
    let quit_file = MenuItem::with_id(QUIT_ID, "Quitter", true, None);
    let _ = file_menu.append_items(&[
        &open_rom,
        &PredefinedMenuItem::separator(),
        &screenshot,
        &export_spc,
        &PredefinedMenuItem::separator(),
        &confirm_quit,
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
    // .save_slot`), exactly like the F5/F9 hotkeys in video.rs. Cmd+S / Cmd+L.
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
    let slot_menu = Submenu::new("Slot", true);
    let slot_items: Vec<CheckMenuItem> = SLOT_IDS
        .iter()
        .enumerate()
        .map(|(slot, &id)| {
            CheckMenuItem::with_id(
                id,
                format!("Slot {slot}"),
                true,
                prefs.save_slot as usize == slot,
                None,
            )
        })
        .collect();
    for item in &slot_items {
        let _ = slot_menu.append(item);
    }
    let next_slot = MenuItem::with_id(NEXT_SLOT_ID, "Slot suivant (F7)", true, None);
    // Session save state, written to `<rom>.resume` on every exit path and
    // restored at launch; never touches the manual slots above.
    let resume_on_launch = CheckMenuItem::with_id(
        RESUME_ID,
        "Reprise instantanée",
        true,
        prefs.resume_on_launch,
        None,
    );
    // Factor picker for the held Tab fast-forward; the key itself is handled
    // in video.rs, this only selects how many frames one press runs.
    let ff_menu = Submenu::new("Accéléré (Tab)", true);
    let ff_items: Vec<CheckMenuItem> = FF_FACTORS
        .iter()
        .map(|&(factor, id)| {
            CheckMenuItem::with_id(
                id,
                format!("×{factor}"),
                true,
                prefs.fast_forward_factor == factor,
                None,
            )
        })
        .collect();
    for item in &ff_items {
        let _ = ff_menu.append(item);
    }
    let _ = emulation_menu.append_items(&[
        &pause_resume,
        &reset,
        &PredefinedMenuItem::separator(),
        &save_state,
        &load_state,
        &slot_menu,
        &next_slot,
        &PredefinedMenuItem::separator(),
        &resume_on_launch,
        &PredefinedMenuItem::separator(),
        &ff_menu,
    ]);
    let _ = menu_bar.append(&emulation_menu);

    let audio_menu = Submenu::new("Audio", true);
    let mute = CheckMenuItem::with_id(
        MUTE_ID,
        "Muet",
        true,
        prefs.mute,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::KeyM)),
    );
    let volume_up = MenuItem::with_id(
        VOLUME_UP_ID,
        "Volume +",
        true,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::Equal)),
    );
    let volume_down = MenuItem::with_id(
        VOLUME_DOWN_ID,
        "Volume −",
        true,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::Minus)),
    );
    // Indicator only: disabled so it can't be clicked, its text is refreshed
    // by `set_volume_label` on every volume change.
    let volume_label = MenuItem::new(volume_label_text(prefs.volume), false, None);
    let _ = audio_menu.append_items(&[
        &mute,
        &PredefinedMenuItem::separator(),
        &volume_up,
        &volume_down,
        &volume_label,
    ]);
    let _ = menu_bar.append(&audio_menu);

    let view_menu = Submenu::new("Affichage", true);
    // Off on a first launch (the overlay is an opt-in debug aid, not something
    // that should appear over a game by default); afterwards it reflects the
    // persisted `prefs.show_fps`.
    let show_fps = CheckMenuItem::with_id(
        SHOW_FPS_ID,
        "Afficher les FPS",
        true,
        prefs.show_fps,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::KeyF)),
    );
    let _ = view_menu.append_items(&[&show_fps]);
    let _ = menu_bar.append(&view_menu);

    menu_bar.init_for_nsapp();

    AppMenu {
        open_rom,
        screenshot,
        export_spc,
        pause_resume,
        reset,
        save_state,
        load_state,
        slot_items,
        next_slot,
        resume_on_launch,
        show_fps,
        mute,
        volume_up,
        volume_down,
        volume_label,
        ff_items,
        confirm_quit,
        quit,
        quit_file,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_forward_factors_cover_the_documented_range() {
        assert_eq!(FF_FACTORS.iter().map(|&(f, _)| f).collect::<Vec<_>>(), vec![2, 3, 4]);
        // Ids must be unique, or a click would dispatch to two factors.
        let mut ids: Vec<&str> = FF_FACTORS.iter().map(|&(_, id)| id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), FF_FACTORS.len());
    }

    #[test]
    fn slot_ids_cover_the_ten_slots_and_are_unique() {
        assert_eq!(SLOT_IDS.len(), 10);
        let mut ids: Vec<&str> = SLOT_IDS.to_vec();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), SLOT_IDS.len());
        // A click is mapped back to a slot by index; the ids must stay in
        // slot order for that to hold.
        for (slot, id) in SLOT_IDS.iter().enumerate() {
            assert!(id.ends_with(&slot.to_string()), "{id} is not slot {slot}");
        }
    }

    #[test]
    fn every_menu_id_is_distinct() {
        // Two items sharing an id would dispatch to both actions; the only
        // intentional duplicate is Quit (app menu + Fichier).
        let mut ids: Vec<&str> = vec![
            OPEN_ROM_ID,
            SCREENSHOT_ID,
            EXPORT_SPC_ID,
            NEXT_SLOT_ID,
            RESUME_ID,
            PAUSE_RESUME_ID,
            RESET_ID,
            SAVE_STATE_ID,
            LOAD_STATE_ID,
            SHOW_FPS_ID,
            MUTE_ID,
            VOLUME_UP_ID,
            VOLUME_DOWN_ID,
            CONFIRM_QUIT_ID,
            QUIT_ID,
        ];
        ids.extend(FF_FACTORS.iter().map(|&(_, id)| id));
        ids.extend(SLOT_IDS);
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }

    #[test]
    fn volume_label_shows_percent() {
        assert_eq!(volume_label_text(0), "Volume : 0 %");
        assert_eq!(volume_label_text(100), "Volume : 100 %");
    }
}
