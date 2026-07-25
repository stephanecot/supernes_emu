//! Locating and opening the pedagogical PDF shipped with the project
//! (`docs/emulateur-snes-explique.pdf`), linked from the settings panel's
//! "À propos" section.
//!
//! The file is part of the repository, not of the binary, so its location
//! depends on how the application was started: from a cargo build the
//! executable sits in `target/<profile>/`, several levels under the repository
//! root; from a working directory that *is* the repository root, `docs/` is
//! right there. Both are covered by walking the ancestors of the executable
//! and of the working directory. When no candidate exists the panel says so
//! and offers no button, rather than a link that would do nothing.

use std::path::{Path, PathBuf};

/// Path of the guide, relative to the repository root.
pub const RELATIVE_PATH: &str = "docs/emulateur-snes-explique.pdf";

/// How far up the executable's / working directory's ancestors are searched.
/// `target/release/prisme` needs three levels to reach the repository root; a
/// couple more cover a `.app` bundle or an installed layout without walking to
/// the filesystem root.
const MAX_ANCESTORS: usize = 6;

/// Every place the guide is looked for, in search order: the executable's own
/// directory and its ancestors first (that is where an installed copy travels
/// with the binary), then the working directory and its ancestors.
/// Deduplicated, so the two lists overlapping costs no extra `exists()` call.
pub fn candidates(exe_dir: Option<&Path>, cwd: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(dir) = exe_dir {
        push_ancestors(&mut out, dir);
    }
    push_ancestors(&mut out, cwd);
    out
}

fn push_ancestors(out: &mut Vec<PathBuf>, dir: &Path) {
    for ancestor in dir.ancestors().take(MAX_ANCESTORS) {
        let candidate = ancestor.join(RELATIVE_PATH);
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    }
}

/// First existing candidate, or `None` when the guide is not next to this
/// build (a copied-out binary, for instance).
pub fn find() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_deref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    candidates(exe_dir.as_deref(), &cwd).into_iter().find(|p| p.is_file())
}

/// Hand `path` to the platform's document opener. Spawned, never waited on:
/// the reader is a separate application and the emulator must keep running its
/// frames.
pub fn open(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = std::process::Command::new("open");
        c.arg(path);
        c
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        // `start` is a `cmd` builtin; the empty string is the window title
        // argument, without which a quoted path would be taken as the title.
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]).arg(path);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path);
        c
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not open {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch directory per test (no shared directory between tests).
    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("prisme_guide_{}_{}", std::process::id(), tag))
    }

    #[test]
    fn the_executable_directory_is_searched_before_the_working_directory() {
        let exe = Path::new("/opt/app/bin");
        let cwd = Path::new("/home/user/repo");
        let list = candidates(Some(exe), cwd);
        // The executable's whole ancestor chain comes first, in order, and only
        // then the working directory's (`/` is shared and appears once).
        let from_exe: Vec<PathBuf> =
            exe.ancestors().take(MAX_ANCESTORS).map(|a| a.join(RELATIVE_PATH)).collect();
        assert_eq!(&list[..from_exe.len()], from_exe.as_slice(), "{list:?}");
        let first_cwd = list
            .iter()
            .position(|p| *p == cwd.join(RELATIVE_PATH))
            .expect("the working directory must be searched too");
        assert!(first_cwd >= from_exe.len(), "{list:?}");
    }

    #[test]
    fn a_cargo_target_layout_reaches_the_repository_root() {
        // `target/release/prisme` -> repository root is three ancestors up.
        let list = candidates(Some(Path::new("/repo/target/release")), Path::new("/elsewhere"));
        assert!(list.contains(&PathBuf::from("/repo").join(RELATIVE_PATH)), "{list:?}");
    }

    #[test]
    fn candidates_are_deduplicated_and_bounded() {
        let dir = Path::new("/a/b/c/d/e/f/g/h");
        let list = candidates(Some(dir), dir);
        assert_eq!(list.len(), MAX_ANCESTORS, "{list:?}");
        let mut sorted = list.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), list.len());
    }

    #[test]
    fn no_candidate_is_reported_when_the_file_is_absent() {
        let dir = scratch("absent");
        let list = candidates(Some(&dir), &dir);
        assert!(list.iter().all(|p| !p.is_file()));
    }

    #[test]
    fn an_existing_guide_is_found_under_a_candidate_root() {
        let root = scratch("found");
        let docs = root.join("docs");
        std::fs::create_dir_all(&docs).expect("create scratch");
        let pdf = root.join(RELATIVE_PATH);
        std::fs::write(&pdf, b"%PDF-1.4\n").expect("write pdf");
        let found = candidates(Some(&root), Path::new("/nowhere"))
            .into_iter()
            .find(|p| p.is_file());
        assert_eq!(found, Some(pdf));
        let _ = std::fs::remove_dir_all(&root);
    }
}
