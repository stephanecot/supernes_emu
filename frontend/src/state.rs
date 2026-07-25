//! Save-state sidecar files (`.state`/`.stateN`/`.resume`). The serialized
//! blob is produced/consumed by snes-core's `Snes::save_state`/`load_state`;
//! the frontend only chooses the path and does the file I/O (snes-core is
//! I/O-free by design, same split as battery SRAM in `save.rs`).
//!
//! This module owns the *naming* scheme only; where the files land — beside the
//! ROM or in `prefs.save_dir` — is `crate::paths::GamePaths`' job.

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
