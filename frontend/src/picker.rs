//! Native "open ROM" file dialog (rfd crate), used both at startup (no ROM
//! argument, windowed mode) and for the in-game `O` hotkey.
//!
//! macOS requires `NSOpenPanel` to run on the main thread. Both call sites
//! satisfy this: the startup picker runs in `main()` before the winit event
//! loop is created (so it *is* the main thread, unconditionally), and the
//! hotkey picker is invoked from `ApplicationHandler::window_event`, which
//! winit guarantees is dispatched on the main thread on every platform it
//! supports (required for AppKit/Win32/X11 event pumps in general).

use std::path::PathBuf;

/// Extensions accepted by the ROM filter: raw dumps and zipped dumps
/// (`load_rom_bytes` in `main.rs` already knows how to open both).
const ROM_EXTENSIONS: &[&str] = &["sfc", "smc", "zip"];

/// Open a native file-open dialog filtered to SNES ROM extensions, starting
/// in `roms/` (relative to the current working directory) if that directory
/// exists, else the current directory (rfd's own default). Blocks the
/// calling thread until the user picks a file or cancels; returns `None` on
/// cancel.
pub fn pick_rom() -> Option<PathBuf> {
    let mut dialog =
        rfd::FileDialog::new().set_title("Open SNES ROM").add_filter("SNES ROM", ROM_EXTENSIONS);
    let roms_dir = PathBuf::from("roms");
    if roms_dir.is_dir() {
        dialog = dialog.set_directory(&roms_dir);
    }
    dialog.pick_file()
}
