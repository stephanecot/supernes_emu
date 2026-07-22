//! Battery-backed SRAM persistence (.srm sidecar files). `snes-core` is
//! I/O-free by design (see `core/src/cartridge/sram.rs`); loading/saving
//! the sidecar file is the frontend's job.

use std::path::{Path, PathBuf};

use snes_core::Cartridge;

/// Default save path for a ROM: same directory and base name, `.srm`
/// extension. `PathBuf::with_extension` replaces whatever extension is
/// present (`.sfc`/`.smc`/`.zip`), so a zipped ROM's sidecar is named after
/// the zip itself (e.g. `game.zip` -> `game.srm`), matching the spec.
pub fn default_save_path(rom_path: &Path) -> PathBuf {
    rom_path.with_extension("srm")
}

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
        Ok(bytes) if !bytes.is_empty() && bytes.len() <= cart.sram.len() => {
            cart.sram.load(&bytes);
            eprintln!(
                "save: loaded {} ({} bytes) into {} bytes of cart SRAM",
                save_path.display(),
                bytes.len(),
                cart.sram.len()
            );
        }
        Ok(bytes) => {
            // Empty file or larger than the cart declares: not a plausible
            // save for this ROM (e.g. leftover sidecar from a different
            // game). Refuse to load garbage into SRAM.
            eprintln!(
                "save: ignoring {} ({} bytes, expected 1..={} for this cart's SRAM)",
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
    if let Some(parent) = save_path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("save: could not create {}: {e}", parent.display());
                return;
            }
        }
    }
    match std::fs::write(save_path, current) {
        Ok(()) => {
            eprintln!("save: wrote {} ({} bytes)", save_path.display(), current.len())
        }
        Err(e) => eprintln!("save: could not write {}: {e}", save_path.display()),
    }
}
