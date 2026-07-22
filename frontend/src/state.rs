//! Save-state sidecar files (`.state` next to the ROM). The serialized blob
//! is produced/consumed by snes-core's `Snes::save_state`/`load_state`; the
//! frontend only chooses the path and does the file I/O (snes-core is
//! I/O-free by design, same split as battery SRAM in `save.rs`).

use std::path::{Path, PathBuf};

/// Sidecar save-state path for a ROM and slot. Slot 0 uses `<rom>.state`;
/// slot N>0 uses `<rom>.stateN`, so multiple states can coexist. Like the
/// `.srm` sidecar, the extension is replaced, so a zipped ROM's state is
/// named after the zip's base name (`game.zip` -> `game.state`).
pub fn state_path(rom_path: &Path, slot: u8) -> PathBuf {
    if slot == 0 {
        rom_path.with_extension("state")
    } else {
        rom_path.with_extension(format!("state{slot}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot0_is_dot_state() {
        assert_eq!(state_path(Path::new("/roms/game.sfc"), 0), PathBuf::from("/roms/game.state"));
        assert_eq!(state_path(Path::new("/roms/game.zip"), 0), PathBuf::from("/roms/game.state"));
    }

    #[test]
    fn slotn_is_numbered() {
        assert_eq!(state_path(Path::new("/roms/game.sfc"), 2), PathBuf::from("/roms/game.state2"));
    }
}
