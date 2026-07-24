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

/// Number of manual save-state slots (`prefs.save_slot` cycles 0..SLOT_COUNT).
pub const SLOT_COUNT: u8 = 10;

/// Sidecar path of the automatic session state ("instant resume"): `<rom>
/// .resume`. Deliberately outside the `.state`/`.stateN` series so an
/// automatic write can never overwrite a state the player saved by hand.
pub fn resume_path(rom_path: &Path) -> PathBuf {
    rom_path.with_extension("resume")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_file_is_distinct_from_every_manual_slot() {
        let rom = Path::new("/roms/game.sfc");
        let resume = resume_path(rom);
        assert_eq!(resume, PathBuf::from("/roms/game.resume"));
        for slot in 0..SLOT_COUNT {
            assert_ne!(state_path(rom, slot), resume, "slot {slot} collides with the resume file");
        }
        // Zipped ROMs follow the same base-name rule as `.srm`/`.state`.
        assert_eq!(resume_path(Path::new("/roms/game.zip")), PathBuf::from("/roms/game.resume"));
    }

    #[test]
    fn every_slot_has_its_own_file() {
        let rom = Path::new("/roms/game.sfc");
        let mut paths: Vec<PathBuf> = (0..SLOT_COUNT).map(|s| state_path(rom, s)).collect();
        let count = paths.len();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), count);
    }

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
