//! Native "open ROM" file dialog (rfd crate), used both at startup (no ROM
//! argument, `--info`/`--disasm` with no path) and by the shell's `Ouvrir une
//! ROM…` entry points.
//!
//! macOS requires `NSOpenPanel` to run on the main thread, and winit's own
//! callbacks must not be on the stack when it opens (see `crate::dialog`).
//! Both call sites satisfy this: the startup picker runs in `main()` before the
//! winit event loop is created, and every in-session picker is posted to the
//! main dispatch queue by `crate::dialog`, which runs it on the main thread
//! between winit callbacks.

use std::path::{Path, PathBuf};

/// Extensions accepted by the ROM filter. Single source of truth, shared with
/// the library scan (`library::is_rom_file`) so the folder the grid lists and
/// the files the native panel offers can never drift apart.
pub use crate::library::ROM_EXTENSIONS;

/// Open a native file-open dialog filtered to SNES ROM extensions, starting in
/// `start` if that directory exists, else the current directory (rfd's own
/// default). Blocks the calling thread until the user picks a file or cancels;
/// returns `None` on cancel.
pub fn pick_rom(start: &Path) -> Option<PathBuf> {
    let mut dialog =
        rfd::FileDialog::new().set_title("Open SNES ROM").add_filter("SNES ROM", ROM_EXTENSIONS);
    if start.is_dir() {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_file()
}

/// Open a native folder-open dialog titled `title`, starting in `current` when
/// that directory exists. Used by the settings panel's `Dossiers` section (ROM
/// folder, screenshot folder). Same main-thread constraint as `pick_rom`,
/// satisfied the same way.
pub fn pick_dir(title: &str, current: &Path) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title(title);
    if current.is_dir() {
        dialog = dialog.set_directory(current);
    }
    dialog.pick_folder()
}
