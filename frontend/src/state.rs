//! Save-state sidecar files (`.state`/`.stateN`/`.resume`). The serialized
//! blob is produced/consumed by snes-core's `Snes::save_state`/`load_state`;
//! the frontend only chooses the path and does the file I/O (snes-core is
//! I/O-free by design, same split as battery SRAM in `save.rs`).
//!
//! This module owns the *naming* scheme only; where the files land — beside the
//! ROM or in `prefs.save_dir` — is `crate::paths::GamePaths`' job.

use std::path::{Path, PathBuf};

/// Number of manual save-state slots (`prefs.save_slot` cycles 0..SLOT_COUNT).
pub const SLOT_COUNT: u8 = 10;

/// Extension of the automatic session state ("instant resume"). Deliberately
/// outside the `state`/`stateN` series so an automatic write can never
/// overwrite a state the player saved by hand.
pub const RESUME_EXT: &str = "resume";

/// Extension of a manual save state: slot 0 uses `state`, slot N>0 uses
/// `stateN`, so the ten slots coexist as separate files.
pub fn state_ext(slot: u8) -> String {
    if slot == 0 {
        "state".to_string()
    } else {
        format!("state{slot}")
    }
}

/// Picture written beside a save state at the moment it is written:
/// `<game>.state3` → `<game>.state3.png`, the raw 256x224 framebuffer, exactly
/// what `--dump-frame` produces. The suffix is **appended** to the whole file
/// name rather than replacing the extension, so `.state` and `.state1` keep
/// distinct previews (replacing would map both to `<game>.png`) and the preview
/// can never collide with a screenshot or with the state of another slot.
///
/// The preview is optional everywhere it is read: a state saved by an older
/// version has none, and its absence must never stop the state from loading.
pub fn preview_path(state: &Path) -> PathBuf {
    let mut name = state.file_name().unwrap_or_default().to_os_string();
    name.push(".png");
    state.with_file_name(name)
}

/// Write `rgba` (a 256x224 RGBA framebuffer) as the preview of the state at
/// `state`, atomically like the state itself, and return the path it went to.
///
/// Atomic because the picture is written right after a state the player is
/// counting on: a crash between the two must leave the previous picture whole
/// rather than a truncated PNG the sheet would fail to decode.
pub fn write_preview(state: &Path, rgba: &[u8], width: u32, height: u32) -> Result<PathBuf, String> {
    let path = preview_path(state);
    let png = crate::encode_rgba_png(rgba, width, height)?;
    crate::atomic::write(&path, &png)?;
    Ok(path)
}

/// Delete a save state and the preview picture written beside it. The two go
/// together: an orphaned picture would show the frame of a state that no longer
/// exists, on a sheet that no longer lists it.
///
/// Only the state's own failure is an error — the picture is optional, so a
/// missing one is the normal case for a state written before previews existed.
pub fn delete_with_preview(state: &Path) -> Result<PathBuf, String> {
    std::fs::remove_file(state)
        .map_err(|e| format!("could not delete {}: {e}", state.display()))?;
    let preview = preview_path(state);
    if preview.exists() {
        if let Err(e) = std::fs::remove_file(&preview) {
            eprintln!("state: could not delete the preview {}: {e}", preview.display());
        }
    }
    Ok(preview)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch directory per test: no test may share a path with
    /// another test or with another run (they run concurrently).
    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("prisme_state_{}_{tag}", std::process::id()))
    }

    #[test]
    fn slot0_is_dot_state_and_the_others_are_numbered() {
        assert_eq!(state_ext(0), "state");
        assert_eq!(state_ext(2), "state2");
        assert_eq!(state_ext(9), "state9");
    }

    #[test]
    fn every_slot_has_its_own_extension_and_none_is_the_resume_file() {
        let mut exts: Vec<String> = (0..SLOT_COUNT).map(state_ext).collect();
        for ext in &exts {
            assert_ne!(ext, RESUME_EXT, "a manual slot would overwrite the session state");
        }
        let count = exts.len();
        exts.sort();
        exts.dedup();
        assert_eq!(exts.len(), count);
    }

    #[test]
    fn a_preview_sits_beside_its_state_and_keeps_the_slot_in_its_name() {
        assert_eq!(
            preview_path(Path::new("/roms/game.state3")),
            PathBuf::from("/roms/game.state3.png")
        );
        assert_eq!(
            preview_path(Path::new("/saves/SUPER_MARIOWORLD-A0DA.resume")),
            PathBuf::from("/saves/SUPER_MARIOWORLD-A0DA.resume.png")
        );
        // Every slot of one game, plus its resume state, owns a distinct
        // preview: replacing the extension instead of appending would collapse
        // them all onto the same file.
        let mut previews: Vec<PathBuf> = (0..SLOT_COUNT)
            .map(|slot| preview_path(Path::new(&format!("/roms/game.{}", state_ext(slot)))))
            .collect();
        previews.push(preview_path(Path::new("/roms/game.resume")));
        let count = previews.len();
        previews.sort();
        previews.dedup();
        assert_eq!(previews.len(), count);
        // …and none of them is the state file itself.
        assert_ne!(preview_path(Path::new("/roms/game.state")), PathBuf::from("/roms/game.state"));
    }

    /// The picture goes beside the state, as a real PNG, and a second write
    /// replaces it whole (both are atomic: no temp file may survive either).
    #[test]
    fn a_preview_is_written_beside_its_state_and_replaced_whole() {
        let dir = scratch("preview");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let state = dir.join("game.state3");
        std::fs::write(&state, b"state blob").expect("write");

        let rgba = vec![0x40u8; 4 * 4 * 4];
        let path = write_preview(&state, &rgba, 4, 4).expect("preview");
        assert_eq!(path, dir.join("game.state3.png"));
        let first = std::fs::read(&path).expect("read");
        assert_eq!(&first[1..4], b"PNG");

        let rgba = vec![0xF0u8; 8 * 8 * 4];
        write_preview(&state, &rgba, 8, 8).expect("preview");
        let second = std::fs::read(&path).expect("read");
        assert_ne!(first, second, "the picture must be replaced, not kept");
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Deleting a slot deletes its picture with it — and works just as well on
    /// a state that never had one.
    #[test]
    fn deleting_a_state_takes_its_preview_with_it() {
        let dir = scratch("delete");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let state = dir.join("game.state");
        let preview = preview_path(&state);
        std::fs::write(&state, b"blob").expect("write");
        std::fs::write(&preview, b"png").expect("write");
        assert_eq!(delete_with_preview(&state).expect("delete"), preview);
        assert!(!state.exists() && !preview.exists());

        // A state with no picture: still deleted, still no error.
        let bare = dir.join("game.state1");
        std::fs::write(&bare, b"blob").expect("write");
        assert!(delete_with_preview(&bare).is_ok());
        assert!(!bare.exists());
        // …and a state that is not there at all is reported, not ignored.
        assert!(delete_with_preview(&bare).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
