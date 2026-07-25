//! Battery-backed SRAM persistence (.srm sidecar files). `snes-core` is
//! I/O-free by design (see `core/src/cartridge/sram.rs`); loading/saving
//! the sidecar file is the frontend's job.

use std::path::Path;

use snes_core::Cartridge;

use crate::atomic;

/// Extension of a battery-save sidecar. Where the file lands — beside the ROM
/// or in `prefs.save_dir` — is `crate::paths::GamePaths`' job; this module only
/// reads and writes whatever path it is handed.
pub const SRM_EXT: &str = "srm";

/// Load a sidecar save into `cart.sram` if the cart has battery SRAM and a
/// save file exists at `save_path`. Returns the post-load SRAM bytes as a
/// baseline snapshot: `save_if_dirty` compares the final SRAM against this
/// baseline at exit, so an untouched (all-0xFF, freshly-initialized) cart or
/// an untouched loaded save is never rewritten.
pub fn load_sram(cart: &mut Cartridge, save_path: &Path) -> Vec<u8> {
    if cart.sram.is_empty() {
        return Vec::new();
    }
    match std::fs::read(save_path) {
        Ok(bytes) if bytes.len() == cart.sram.len() => {
            cart.sram.load(&bytes);
            eprintln!(
                "save: loaded {} ({} bytes) into cart SRAM",
                save_path.display(),
                bytes.len()
            );
        }
        Ok(bytes) => {
            // Anything other than an exact size match is not a plausible
            // save for this ROM: a leftover sidecar from a different game, or
            // — the case that matters most — a `.srm` truncated by a writer
            // that was interrupted mid-write (see `save_if_dirty`'s atomic
            // write, which exists precisely so this can't happen from *this*
            // emulator, but a save copied in from elsewhere could still be
            // partial). Loading a short file would silently leave the tail of
            // SRAM at its power-on pattern instead of the game's real data,
            // which is worse than not loading it at all — refuse it and
            // start from fresh (all-0xFF) SRAM instead, exactly as if this
            // were the game's first boot.
            eprintln!(
                "save: ignoring {} ({} bytes, expected exactly {} for this cart's SRAM); starting from fresh SRAM",
                save_path.display(),
                bytes.len(),
                cart.sram.len()
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("save: no save found at {}, starting fresh SRAM", save_path.display());
        }
        Err(e) => {
            eprintln!("save: could not read {}: {e}", save_path.display());
        }
    }
    cart.sram.as_bytes().to_vec()
}

/// Write `cart.sram` to `save_path` if the cart has battery SRAM and its
/// contents differ from `baseline` (the state captured by `load_sram` at
/// startup). Skipping an unchanged write avoids clobbering a good save with
/// an all-0xFF buffer when the game never touched SRAM this session, and
/// avoids needless disk writes when nothing changed.
pub fn save_if_dirty(cart: &Cartridge, save_path: &Path, baseline: &[u8]) {
    if cart.sram.is_empty() {
        return;
    }
    let current = cart.sram.as_bytes();
    if current == baseline {
        return;
    }
    // Atomic (temp file + rename): a crash or power loss mid-write must never
    // leave a truncated `.srm` on disk, since `load_sram` above would then
    // refuse it outright on the next launch and the player would lose the
    // save entirely rather than just this session's changes.
    match atomic::write(save_path, current) {
        Ok(()) => {
            eprintln!("save: wrote {} ({} bytes)", save_path.display(), current.len())
        }
        Err(e) => eprintln!("save: could not write {}: {e}", save_path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snes_core::cartridge::sram::Sram;
    use snes_core::Mapping;
    use std::path::PathBuf;

    /// Minimal cart with `sram_len` bytes of battery SRAM; every other field
    /// is irrelevant to `load_sram`/`save_if_dirty`.
    fn cart_with_sram(sram_len: usize) -> Cartridge {
        Cartridge {
            rom: Vec::new(),
            sram: Sram::new(sram_len),
            mapping: Mapping::LoRom,
            region: snes_core::Region::Ntsc,
            title: "TEST".to_string(),
            fastrom: false,
            header_checksum: 0,
            checksum_valid: true,
            superfx: None,
            sa1: None,
            dsp1: None,
            dsp1_mapping: None,
            cx4: None,
        }
    }

    /// A private directory per test. `save_if_dirty_writes_atomically_and_skips_unchanged_sram`
    /// scans the parent directory for leftover `.tmp` files; pointing that at the
    /// shared system temp dir made it fail whenever any other process happened to
    /// have a `.tmp` file there.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prisme_save_{}_{}", std::process::id(), tag));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir.join("game.srm")
    }

    #[test]
    fn exact_size_save_is_loaded() {
        let path = scratch("exact");
        std::fs::write(&path, [0xAAu8; 8]).expect("write fixture");
        let mut cart = cart_with_sram(8);
        let baseline = load_sram(&mut cart, &path);
        assert_eq!(cart.sram.as_bytes(), &[0xAAu8; 8]);
        assert_eq!(baseline, vec![0xAAu8; 8]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn truncated_save_is_rejected_and_sram_stays_fresh() {
        let path = scratch("short");
        // Half the declared SRAM size: e.g. a `.srm` left behind by a writer
        // that was killed mid-write before the (now atomic) fix.
        std::fs::write(&path, [0x11u8; 4]).expect("write fixture");
        let mut cart = cart_with_sram(8);
        let baseline = load_sram(&mut cart, &path);
        // Fresh SRAM is all-0xFF (see `Sram::new`), not spliced with the
        // partial file.
        assert_eq!(cart.sram.as_bytes(), &[0xFFu8; 8]);
        assert_eq!(baseline, vec![0xFFu8; 8]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn oversized_save_is_rejected_and_sram_stays_fresh() {
        let path = scratch("long");
        std::fs::write(&path, [0x22u8; 16]).expect("write fixture");
        let mut cart = cart_with_sram(8);
        let baseline = load_sram(&mut cart, &path);
        assert_eq!(cart.sram.as_bytes(), &[0xFFu8; 8]);
        assert_eq!(baseline, vec![0xFFu8; 8]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_save_file_yields_fresh_sram_without_creating_it() {
        let path = scratch("missing");
        let _ = std::fs::remove_file(&path);
        let mut cart = cart_with_sram(4);
        let baseline = load_sram(&mut cart, &path);
        assert_eq!(baseline, vec![0xFFu8; 4]);
        assert!(!path.exists());
    }

    #[test]
    fn cart_without_sram_is_a_no_op_on_both_paths() {
        let mut cart = cart_with_sram(0);
        let path = scratch("empty-cart");
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_sram(&mut cart, &path), Vec::<u8>::new());
        cart.sram.set(0, 0xAA); // no-op: `Sram::set` on an empty buffer does nothing
        save_if_dirty(&cart, &path, &[]);
        assert!(!path.exists(), "a cart with no SRAM must never create a .srm file");
    }

    /// A save folder the player configured may not exist yet (they typed a new
    /// name in the panel, or moved it since): the write creates it rather than
    /// dropping the save.
    #[test]
    fn a_missing_save_folder_is_created_by_the_write() {
        // Own directory, and one that does *not* exist yet — that is the point.
        let root =
            std::env::temp_dir().join(format!("prisme_save_{}_mkdir", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let path = root.join("nested").join("game.srm");
        let mut cart = cart_with_sram(4);
        let baseline = load_sram(&mut cart, &path); // nothing there yet
        cart.sram.set(0, 0x42);
        save_if_dirty(&cart, &path, &baseline);
        assert!(path.is_file(), "the folder must be created on demand");
        assert_eq!(std::fs::read(&path).unwrap()[0], 0x42);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn save_if_dirty_writes_atomically_and_skips_unchanged_sram() {
        let path = scratch("dirty");
        let _ = std::fs::remove_file(&path);
        let dir = path.parent().unwrap().to_path_buf();
        let mut cart = cart_with_sram(4);
        let baseline = load_sram(&mut cart, &path); // fresh, all-0xFF
        save_if_dirty(&cart, &path, &baseline);
        assert!(!path.exists(), "unchanged SRAM must not be written");

        cart.sram.set(0, 0x42);
        save_if_dirty(&cart, &path, &baseline);
        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap()[0], 0x42);

        // No leftover atomic-write temp file in the same directory.
        let stray = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|n| n.contains(".tmp"));
        assert_eq!(stray, None, "atomic write left a temp file: {stray:?}");
        let _ = std::fs::remove_file(&path);
    }
}
