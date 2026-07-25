//! Atomic file writes: serialize to a temp file in the same directory as the
//! target, then `rename` over it. A crash or power loss mid-write then always
//! leaves either the previous file intact or the new one complete — never a
//! truncated hybrid of both, unlike `std::fs::write` (which opens the target
//! with `O_TRUNC`, so a crash between the truncate and the last byte written
//! leaves a zero-length or partial file in place).
//!
//! `rename` is atomic only within a single filesystem, which putting the temp
//! file in the target's own directory guarantees (as opposed to e.g. the OS
//! temp directory, which can be a different mount).
//!
//! Used for every file whose corruption would lose player data or settings:
//! `.srm` battery saves (`save.rs`), `.state`/`.stateN`/`.resume` snapshots
//! (`video.rs`, `main.rs --save-state-at`), and `prefs.json` (`prefs.rs`).

use std::path::Path;

/// `mkdir -p` on `path`'s parent, skipping bare file names (no directory
/// component to create).
fn ensure_parent_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
    }
    Ok(())
}

/// Write `data` to `path` atomically (see module docs). The temp name embeds
/// the process id, so two processes racing on the same target — unlikely for
/// this app, but cheap to guard — never collide on the same temp file; on any
/// failure the temp file is removed rather than left behind.
pub fn write(path: &Path, data: &[u8]) -> Result<(), String> {
    ensure_parent_dir(path)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp{}", std::process::id()));
    std::fs::write(&tmp, data).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("could not replace {}: {e}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Unique scratch directory per test, cleaned up by the caller.
    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("prisme_atomic_{}_{}", std::process::id(), tag))
    }

    #[test]
    fn writes_and_overwrites_leaving_no_temp_file() {
        let dir = scratch("basic");
        let path = dir.join("f.bin");
        write(&path, b"first").expect("write");
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        // Overwrite with different (longer) content: must fully replace, not
        // append or leave a mix of old/new bytes.
        write(&path, b"second, longer").expect("rewrite");
        assert_eq!(std::fs::read(&path).unwrap(), b"second, longer");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "f.bin")
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn creates_missing_parent_directories() {
        let dir = scratch("nested").join("a").join("b");
        let path = dir.join("f.bin");
        write(&path, b"x").expect("write");
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(scratch("nested"));
    }

    #[test]
    fn a_failed_write_never_replaces_the_original_file() {
        // Point the target at a directory (so the final rename target is
        // unusable) to force the rename step to fail, then check the
        // original — nonexistent, in this case — file state is unaffected
        // and no temp file survives.
        let dir = scratch("fail");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let bad_target = dir.join("is-a-dir");
        std::fs::create_dir_all(&bad_target).expect("mkdir target");
        assert!(write(&bad_target, b"data").is_err());
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "is-a-dir")
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind after a failed write: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
