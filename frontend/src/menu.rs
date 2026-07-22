//! Native macOS menu bar (muda crate), installed once against `NSApp` and
//! polled for click events from the winit loop in `video.rs`.
//!
//! muda's `Menu` is a thin `Rc`-wrapped handle over a platform menu object;
//! on macOS `init_for_nsapp` hands the underlying `NSMenu` to
//! `NSApp.setMainMenu`, which retains it natively, but `AppMenu` is kept
//! alive for the run's duration anyway (stored on `App`) so item state
//! (enabled/text) can be queried or changed later without re-querying AppKit.
//!
//! Only built for `target_os = "macos"`: this is a macOS-specific menu bar,
//! not a cross-platform one (Windows/Linux would need `init_for_hwnd`/GTK
//! wiring this crate doesn't attempt).

#![cfg(target_os = "macos")]

use muda::accelerator::{Accelerator, Code, CMD_OR_CTRL};
use muda::{Menu, MenuItem, PredefinedMenuItem, Submenu};

/// Stable ids for the items `video.rs` dispatches on after a
/// `MenuEvent::receiver().try_recv()`. Predefined items (About, Quit,
/// separators) need no id here: macOS runs their native action
/// (`orderFrontStandardAboutPanel:` / `terminate:`) itself and never routes
/// a click through the `MenuEvent` channel we poll.
pub const OPEN_ROM_ID: &str = "snes-frontend.open-rom";
pub const PAUSE_RESUME_ID: &str = "snes-frontend.pause-resume";
pub const RESET_ID: &str = "snes-frontend.reset";
pub const SAVE_STATE_ID: &str = "snes-frontend.save-state";
pub const LOAD_STATE_ID: &str = "snes-frontend.load-state";
pub const QUIT_ID: &str = "snes-frontend.quit";

/// Live handles for the menu items `video.rs` needs after construction.
/// Held on `App` for the run's lifetime (see module docs).
pub struct AppMenu {
    pub open_rom: MenuItem,
    pub pause_resume: MenuItem,
    pub reset: MenuItem,
    pub save_state: MenuItem,
    pub load_state: MenuItem,
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
pub fn install() -> AppMenu {
    let menu_bar = Menu::new();

    // Application (leftmost) menu: AppKit titles it after the running
    // process itself (from `CFBundleName`/argv0) — muda has no way to
    // rename that from here. `About`/`Quit` are muda's `PredefinedMenuItem`s
    // so they run the OS's own panel/termination actions instead of us
    // reimplementing them.
    let app_menu = Submenu::new("snes-frontend", true);
    // Custom Quit (see AppMenu::quit): routes through our MenuEvent channel so
    // we can flush battery SRAM before exiting, unlike PredefinedMenuItem::quit
    // which invokes AppKit's terminate: directly.
    let quit = MenuItem::with_id(
        QUIT_ID,
        "Quit snes-frontend",
        true,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::KeyQ)),
    );
    let _ = app_menu.append_items(&[
        &PredefinedMenuItem::about(None, None),
        &PredefinedMenuItem::separator(),
        &quit,
    ]);
    let _ = menu_bar.append(&app_menu);

    let file_menu = Submenu::new("File", true);
    let open_rom = MenuItem::with_id(
        OPEN_ROM_ID,
        "Open ROM…",
        true,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::KeyO)),
    );
    let quit_file = MenuItem::with_id(QUIT_ID, "Quit", true, None);
    let _ = file_menu.append_items(&[&open_rom, &PredefinedMenuItem::separator(), &quit_file]);
    let _ = menu_bar.append(&file_menu);

    let emulation_menu = Submenu::new("Emulation", true);
    let pause_resume = MenuItem::with_id(
        PAUSE_RESUME_ID,
        "Pause / Resume",
        true,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::KeyP)),
    );
    let reset = MenuItem::with_id(
        RESET_ID,
        "Reset",
        true,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::KeyR)),
    );
    // Save/Load State act on the `<rom>.state` sidecar (slot 0); also bound to
    // the F5/F9 hotkeys in video.rs. Cmd+S / Cmd+L accelerators.
    let save_state = MenuItem::with_id(
        SAVE_STATE_ID,
        "Save State",
        true,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::KeyS)),
    );
    let load_state = MenuItem::with_id(
        LOAD_STATE_ID,
        "Load State",
        true,
        Some(Accelerator::new(Some(CMD_OR_CTRL), Code::KeyL)),
    );
    let _ = emulation_menu.append_items(&[
        &pause_resume,
        &reset,
        &PredefinedMenuItem::separator(),
        &save_state,
        &load_state,
    ]);
    let _ = menu_bar.append(&emulation_menu);

    menu_bar.init_for_nsapp();

    AppMenu { open_rom, pause_resume, reset, save_state, load_state, quit, quit_file }
}
