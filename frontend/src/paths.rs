//! Where a game's sidecar files live: battery SRAM (`.srm`), manual save
//! states (`.state`/`.stateN`) and the automatic session state (`.resume`).
//!
//! Two layouts coexist:
//!   * **beside the ROM** — `/roms/game.sfc` -> `/roms/game.srm`. The original
//!     layout and still the default (`prefs.save_dir` unset).
//!   * **one folder for every game** — `prefs.save_dir`, where each game's
//!     sidecars are named after the game itself: `library::game_id`, i.e. the
//!     sanitized cartridge title plus the header checksum
//!     (`<save_dir>/SUPER_MARIOWORLD-A0DA.srm`), which is also the key
//!     `prefs.games` uses.
//!
//! Naming a shared folder's files after the *game* rather than after the ROM
//! *file* is what keeps two homonymous dumps apart: `/roms/eu/Zelda.sfc` and
//! `/roms/us/Zelda.sfc` used to land on one `<save_dir>/Zelda.srm`, so the
//! second one loaded would read a save that is not its own, judge it invalid
//! and overwrite it. Two files of the same game (`Zelda.sfc` and `Zelda.smc`)
//! do share one sidecar, which is the intended behavior — it is the same
//! cartridge.
//!
//! Resolution order for a **write** (`GamePaths::*_write`):
//!   1. the CLI `--save PATH` override, for the `.srm` of the cartridge the run
//!      started with — and only that one: the flag names a single file, so
//!      switching ROM inside the window drops it (`GamePaths::new` is called
//!      again without an override);
//!   2. `prefs.save_dir` when set, under the game's id;
//!   3. beside the ROM.
//!
//! **No existing save is ever left behind** (`GamePaths::*_read`). A read walks,
//! in this order:
//!   1. `<save_dir>/<game id>.<ext>`, the current naming;
//!   2. `<save_dir>/<ROM file stem>.<ext>`, what builds before this one wrote
//!      there — a game keeps loading its save without anything being moved,
//!      and migrates lazily the first time it writes;
//!   3. the same two names inside `prefs.previous_save_dir`, the folder that was
//!      configured before the current setting (see below);
//!   4. the file beside the ROM.
//!
//! Nothing is moved or deleted — the old file stays where it is, and the next
//! *write* lands in the configured folder. Two consequences, both deliberate:
//!   * the legacy file is left as-is and becomes a frozen backup;
//!   * when a file exists in *both* places, the configured folder wins — that
//!     is where writes go, so preferring the older legacy copy would resurrect
//!     stale data.
//!
//! With **no** folder configured the beside-the-ROM file is the target, and the
//! folder the player just stopped using (`previous_save_dir`) is consulted too:
//! it is picked when it holds a *strictly newer* sidecar for this game, since
//! it is where the recent sessions wrote. Clearing `Réglages > Dossiers >
//! Dossier des sauvegardes` would otherwise silently hand back the frozen
//! beside-the-ROM file and lose hours of progress from view. The comparison is
//! by file mtime and only ever runs when the current target is older or absent.
//!
//! A missing folder is created by the writer itself (`crate::atomic::write`
//! does `mkdir -p` on the parent); `prepare_dir` below additionally checks it
//! is writable at the moment the player picks it, so an unusable folder is
//! reported then rather than at the next save.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::cheats::CHEATS_EXT;
use crate::save::SRM_EXT;
use crate::state::{state_ext, RESUME_EXT};

/// File name a game's sidecar takes in a shared folder: `<game id>.<ext>`.
/// `None` when the id cannot be one path component — no cartridge loaded
/// (empty id), or an id from a hand-edited preferences file carrying a
/// separator or a leading dot. The caller then falls back to the ROM file's
/// own base name, which is what earlier builds always used.
fn id_file_name(id: &str, ext: &str) -> Option<OsString> {
    if id.is_empty() || id.starts_with('.') || id.contains('/') || id.contains('\\') {
        return None;
    }
    Some(OsString::from(format!("{id}.{ext}")))
}

/// `<dir>/<ROM file stem>.<ext>`: the name a shared folder used before sidecars
/// were keyed on the game id. Read-only fallback (see the module docs); a ROM
/// path with no file name keeps the beside-the-ROM behavior, which yields the
/// path unchanged.
pub fn legacy_sidecar(rom: &Path, dir: &Path, ext: &str) -> PathBuf {
    match rom.file_stem() {
        Some(stem) => {
            let mut name = OsString::from(stem);
            name.push(".");
            name.push(ext);
            dir.join(name)
        }
        None => rom.with_extension(ext),
    }
}

/// Sidecar path for the game `id` loaded from `rom`, with extension `ext`:
/// inside `dir` under the game's id when one is configured, else beside the
/// ROM.
pub fn sidecar(rom: &Path, dir: Option<&Path>, id: &str, ext: &str) -> PathBuf {
    match dir {
        Some(dir) => match id_file_name(id, ext) {
            Some(name) => dir.join(name),
            None => legacy_sidecar(rom, dir, ext),
        },
        None => rom.with_extension(ext),
    }
}

/// First existing sidecar of this game inside `dir`: the id-named file, else
/// the one named after the ROM file. `None` when `dir` is unset or holds
/// neither.
fn found_in(rom: &Path, dir: Option<&Path>, id: &str, ext: &str) -> Option<PathBuf> {
    let dir = dir?;
    let by_id = sidecar(rom, Some(dir), id, ext);
    if by_id.is_file() {
        return Some(by_id);
    }
    let by_stem = legacy_sidecar(rom, dir, ext);
    by_stem.is_file().then_some(by_stem)
}

/// Last-modified time of `path`, or `None` when it does not exist or the
/// platform/filesystem cannot report one.
fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// `a` exists and is strictly more recent than `b` (an absent `b` counts as
/// older). Unreadable timestamps answer `false`, so an undecidable comparison
/// leaves the caller on its normal target.
fn newer(a: &Path, b: &Path) -> bool {
    match (mtime(a), mtime(b)) {
        (Some(a), Some(b)) => a > b,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Where that sidecar is *read* from, following the order documented at the top
/// of this module. When nothing exists anywhere, the configured location is
/// returned, so a "not found" message names the place the player chose.
pub fn read_sidecar(
    rom: &Path,
    dir: Option<&Path>,
    previous: Option<&Path>,
    id: &str,
    ext: &str,
) -> PathBuf {
    let target = sidecar(rom, dir, id, ext);
    if dir.is_some() {
        if target.is_file() {
            return target;
        }
        if let Some(found) = found_in(rom, dir, id, ext) {
            return found; // legacy, ROM-file-named entry of the same folder
        }
        if let Some(found) = found_in(rom, previous, id, ext) {
            return found;
        }
        let beside = rom.with_extension(ext);
        if beside.is_file() {
            return beside;
        }
        return target;
    }
    // No folder configured: `target` is the beside-the-ROM file. A folder the
    // player has just stopped using still holds the recent sessions' saves.
    match found_in(rom, previous, id, ext) {
        Some(left_behind) if newer(&left_behind, &target) => left_behind,
        _ => target,
    }
}

/// Every sidecar file of the game currently loaded. Built once per loaded
/// cartridge (`video::App::switch_rom`, `main::run`) rather than resolved per
/// call, so a change of `prefs.save_dir` made mid-session cannot retarget the
/// running game's files behind its back: the session keeps the layout it
/// started with, and the new folder applies to the next game loaded. Writing
/// this session's SRAM into a folder that already holds a save for the same
/// game would otherwise destroy that save.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GamePaths {
    rom: PathBuf,
    /// `library::game_id` of the cartridge: the base name its sidecars take in
    /// a shared folder. Empty while no cartridge is loaded.
    id: String,
    /// `prefs.save_dir` as it was when the cartridge was loaded.
    dir: Option<PathBuf>,
    /// `prefs.previous_save_dir`: the folder configured before the current
    /// setting. Consulted by reads only — nothing is ever written there.
    previous_dir: Option<PathBuf>,
    /// CLI `--save PATH`: the `.srm` of this run, whatever `dir` says.
    srm_override: Option<PathBuf>,
}

impl GamePaths {
    pub fn new(rom: &Path, id: &str, dir: Option<PathBuf>, srm_override: Option<PathBuf>) -> Self {
        Self {
            rom: rom.to_path_buf(),
            id: id.to_string(),
            dir,
            previous_dir: None,
            srm_override,
        }
    }

    /// Add the read-only fallback folder (`prefs.previous_save_dir`).
    pub fn with_previous_dir(mut self, previous: Option<PathBuf>) -> Self {
        self.previous_dir = previous;
        self
    }

    /// Folder this session writes into, `None` for beside the ROM.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// `library::game_id` this session's sidecars are named after.
    pub fn id(&self) -> &str {
        &self.id
    }

    fn write_path(&self, ext: &str) -> PathBuf {
        sidecar(&self.rom, self.dir(), &self.id, ext)
    }

    fn read_path(&self, ext: &str) -> PathBuf {
        read_sidecar(&self.rom, self.dir(), self.previous_dir.as_deref(), &self.id, ext)
    }

    /// Battery SRAM, write target.
    pub fn srm_write(&self) -> PathBuf {
        match &self.srm_override {
            Some(path) => path.clone(),
            None => self.write_path(SRM_EXT),
        }
    }

    /// Battery SRAM, read target (legacy fallbacks).
    pub fn srm_read(&self) -> PathBuf {
        match &self.srm_override {
            Some(path) => path.clone(),
            None => self.read_path(SRM_EXT),
        }
    }

    /// Manual save state of `slot`, write target.
    pub fn state_write(&self, slot: u8) -> PathBuf {
        self.write_path(&state_ext(slot))
    }

    /// Manual save state of `slot`, read target (legacy fallbacks).
    pub fn state_read(&self, slot: u8) -> PathBuf {
        self.read_path(&state_ext(slot))
    }

    /// Automatic session state, write target.
    pub fn resume_write(&self) -> PathBuf {
        self.write_path(RESUME_EXT)
    }

    /// Automatic session state, read target (legacy fallbacks).
    pub fn resume_read(&self) -> PathBuf {
        self.read_path(RESUME_EXT)
    }

    /// Cheats found for this game, write target. Deliberately not covered by
    /// `--save`: that flag names one `.srm`, not the game's whole sidecar set.
    pub fn cheats_write(&self) -> PathBuf {
        self.write_path(CHEATS_EXT)
    }

    /// Cheats found for this game, read target (legacy fallbacks).
    pub fn cheats_read(&self) -> PathBuf {
        self.read_path(CHEATS_EXT)
    }
}

/// Create `dir` if it does not exist and check it can actually be written to,
/// by creating and removing a probe file. Called when the player picks a save
/// folder: a folder that only fails at the next save (read-only volume,
/// permissions, a *file* by that name) would silently cost a save.
pub fn prepare_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let probe = dir.join(format!(".prisme-write-test-{}", std::process::id()));
    std::fs::write(&probe, b"")
        .map_err(|e| format!("could not write in {}: {e}", dir.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SLOT_COUNT;

    const ID: &str = "SUPER_MARIOWORLD-A0DA";

    /// A private directory per test: a shared one made two earlier tests flaky
    /// when they raced on the same files.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prisme_paths_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// Write `bytes` to `path` and stamp it `secs` seconds in the past, so two
    /// fixtures can be compared by age without sleeping.
    fn write_aged(path: &Path, bytes: &[u8], secs: u64) {
        std::fs::write(path, bytes).expect("write fixture");
        let when = SystemTime::now() - std::time::Duration::from_secs(secs);
        let file = std::fs::File::options().write(true).open(path).expect("open fixture");
        file.set_modified(when).expect("set mtime");
    }

    #[test]
    fn without_a_folder_a_sidecar_sits_beside_the_rom() {
        assert_eq!(
            sidecar(Path::new("/roms/game.sfc"), None, ID, "srm"),
            PathBuf::from("/roms/game.srm")
        );
        // Only the last extension is replaced, so a zipped ROM keeps the zip's
        // base name.
        assert_eq!(
            sidecar(Path::new("/roms/game.zip"), None, ID, "srm"),
            PathBuf::from("/roms/game.srm")
        );
        assert_eq!(
            sidecar(Path::new("/roms/game.sfc"), None, ID, "state3"),
            PathBuf::from("/roms/game.state3")
        );
    }

    #[test]
    fn a_configured_folder_names_each_sidecar_after_the_game() {
        let dir = Path::new("/saves");
        assert_eq!(
            sidecar(Path::new("/roms/game.sfc"), Some(dir), ID, "srm"),
            PathBuf::from("/saves/SUPER_MARIOWORLD-A0DA.srm")
        );
        assert_eq!(
            sidecar(Path::new("/roms/sub/Super Mario (E) [!].zip"), Some(dir), ID, "state2"),
            PathBuf::from("/saves/SUPER_MARIOWORLD-A0DA.state2")
        );
        // No id at all (no cartridge loaded): the ROM file's own base name,
        // which is what earlier builds always used.
        assert_eq!(
            sidecar(Path::new("/roms/game.sfc"), Some(dir), "", "srm"),
            PathBuf::from("/saves/game.srm")
        );
        // An id that is not a single path component cannot name a file.
        assert_eq!(
            sidecar(Path::new("/roms/game.sfc"), Some(dir), "../escape-0000", "srm"),
            PathBuf::from("/saves/game.srm")
        );
        // No file name at all: unchanged, never `<dir>/.srm`.
        assert_eq!(sidecar(Path::new(""), Some(dir), "", "srm"), PathBuf::from(""));
    }

    /// The save-destroying case this naming exists for: two ROM files of the
    /// same name, from different folders, holding *different* games.
    #[test]
    fn two_homonymous_roms_never_share_a_sidecar_in_a_shared_folder() {
        let dir = Path::new("/saves");
        let eu = sidecar(Path::new("/roms/eu/Zelda.sfc"), Some(dir), "ZELDA_EU-1234", "srm");
        let us = sidecar(Path::new("/roms/us/Zelda.sfc"), Some(dir), "ZELDA_US-5678", "srm");
        assert_ne!(eu, us);
        // …and two files of the *same* game do share one, which is intended:
        // it is the same cartridge.
        let smc = sidecar(Path::new("/roms/Zelda.smc"), Some(dir), "ZELDA_EU-1234", "srm");
        assert_eq!(eu, smc);
    }

    #[test]
    fn a_read_falls_back_to_the_legacy_files_and_a_write_never_does() {
        let root = scratch("fallback");
        let roms = root.join("roms");
        let saves = root.join("saves");
        std::fs::create_dir_all(&roms).expect("mkdir");
        std::fs::create_dir_all(&saves).expect("mkdir");
        let rom = roms.join("game.sfc");
        std::fs::write(&rom, b"rom").expect("write rom");
        let beside = roms.join("game.srm");
        let by_stem = saves.join("game.srm");
        let by_id = saves.join(format!("{ID}.srm"));

        // Nothing anywhere: the read names the configured folder, so a
        // "not found" message points at the place the player chose.
        assert_eq!(read_sidecar(&rom, Some(&saves), None, ID, "srm"), by_id);

        // Only the old save exists: it must be read, not ignored.
        std::fs::write(&beside, b"old").expect("write beside");
        assert_eq!(read_sidecar(&rom, Some(&saves), None, ID, "srm"), beside);
        // …but the write still goes to the configured folder (lazy migration).
        assert_eq!(sidecar(&rom, Some(&saves), ID, "srm"), by_id);

        // A file written there by an earlier build, under the ROM file's name,
        // is preferred over the beside-the-ROM one.
        std::fs::write(&by_stem, b"folder-legacy").expect("write legacy");
        assert_eq!(read_sidecar(&rom, Some(&saves), None, ID, "srm"), by_stem);

        // Once written under the game's id, that one wins over both.
        std::fs::write(&by_id, b"new").expect("write id");
        assert_eq!(read_sidecar(&rom, Some(&saves), None, ID, "srm"), by_id);

        // With no folder configured, nothing is looked up at all.
        assert_eq!(read_sidecar(&rom, None, None, ID, "srm"), beside);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Clearing the save folder must not hand back a frozen beside-the-ROM
    /// file while the folder just abandoned holds the recent progress.
    #[test]
    fn clearing_the_folder_still_reads_the_newer_save_left_in_it() {
        let root = scratch("previous");
        let roms = root.join("roms");
        let saves = root.join("saves");
        std::fs::create_dir_all(&roms).expect("mkdir");
        std::fs::create_dir_all(&saves).expect("mkdir");
        let rom = roms.join("game.sfc");
        std::fs::write(&rom, b"rom").expect("write rom");
        let beside = roms.join("game.srm");
        let by_id = saves.join(format!("{ID}.srm"));

        // The old file beside the ROM is frozen; the folder holds the recent one.
        write_aged(&beside, b"old", 3600);
        write_aged(&by_id, b"recent", 10);
        assert_eq!(read_sidecar(&rom, None, Some(&saves), ID, "srm"), by_id);
        // The write still goes beside the ROM: that is the setting now.
        assert_eq!(sidecar(&rom, None, ID, "srm"), beside);

        // Once the beside-the-ROM file is the more recent one, it wins again.
        write_aged(&beside, b"newest", 0);
        assert_eq!(read_sidecar(&rom, None, Some(&saves), ID, "srm"), beside);

        // A previous folder holding nothing for this game changes nothing.
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).expect("mkdir");
        assert_eq!(read_sidecar(&rom, None, Some(&empty), ID, "srm"), beside);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Moving from one folder to another keeps reading the previous one until
    /// the game writes into the new folder.
    #[test]
    fn a_new_folder_reads_the_previous_one_before_the_beside_the_rom_file() {
        let root = scratch("moved");
        let roms = root.join("roms");
        let old = root.join("old-saves");
        let new = root.join("new-saves");
        for d in [&roms, &old, &new] {
            std::fs::create_dir_all(d).expect("mkdir");
        }
        let rom = roms.join("game.sfc");
        std::fs::write(&rom, b"rom").expect("write rom");
        std::fs::write(roms.join("game.srm"), b"ancient").expect("write beside");
        let in_old = old.join(format!("{ID}.srm"));
        std::fs::write(&in_old, b"previous").expect("write old");

        assert_eq!(read_sidecar(&rom, Some(&new), Some(&old), ID, "srm"), in_old);
        // As soon as the new folder holds one, it wins.
        let in_new = new.join(format!("{ID}.srm"));
        std::fs::write(&in_new, b"current").expect("write new");
        assert_eq!(read_sidecar(&rom, Some(&new), Some(&old), ID, "srm"), in_new);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_directory_named_like_the_save_is_not_mistaken_for_one() {
        let root = scratch("isdir");
        let roms = root.join("roms");
        let saves = root.join("saves");
        std::fs::create_dir_all(&roms).expect("mkdir");
        std::fs::create_dir_all(saves.join(format!("{ID}.srm"))).expect("mkdir decoy");
        std::fs::create_dir_all(saves.join("game.srm")).expect("mkdir decoy");
        let rom = roms.join("game.sfc");
        let beside = roms.join("game.srm");
        std::fs::write(&beside, b"old").expect("write beside");
        assert_eq!(read_sidecar(&rom, Some(&saves), None, ID, "srm"), beside);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_cli_override_wins_over_the_configured_folder() {
        let paths = GamePaths::new(
            Path::new("/roms/game.sfc"),
            ID,
            Some(PathBuf::from("/saves")),
            Some(PathBuf::from("/elsewhere/custom.srm")),
        );
        assert_eq!(paths.srm_write(), PathBuf::from("/elsewhere/custom.srm"));
        assert_eq!(paths.srm_read(), PathBuf::from("/elsewhere/custom.srm"));
        // States are not covered by `--save` (it names one file), so they
        // follow the configured folder.
        assert_eq!(paths.state_write(0), PathBuf::from("/saves/SUPER_MARIOWORLD-A0DA.state"));
        assert_eq!(paths.resume_write(), PathBuf::from("/saves/SUPER_MARIOWORLD-A0DA.resume"));
    }

    #[test]
    fn every_sidecar_of_a_session_has_its_own_name() {
        for dir in [None, Some(PathBuf::from("/saves"))] {
            let paths = GamePaths::new(Path::new("/roms/game.sfc"), ID, dir.clone(), None);
            let mut all = vec![paths.srm_write(), paths.resume_write(), paths.cheats_write()];
            all.extend((0..SLOT_COUNT).map(|s| paths.state_write(s)));
            let count = all.len();
            all.sort();
            all.dedup();
            assert_eq!(all.len(), count, "{dir:?}: two sidecars share a file name");
        }
    }

    #[test]
    fn a_session_keeps_the_layout_it_was_built_with() {
        // The whole point of holding the folder in the struct: `prefs.save_dir`
        // changing later cannot retarget these paths.
        let beside = GamePaths::new(Path::new("/roms/game.sfc"), ID, None, None);
        assert_eq!(beside.srm_write(), PathBuf::from("/roms/game.srm"));
        assert_eq!(beside.dir(), None);
        assert_eq!(beside.id(), ID);
        let folder =
            GamePaths::new(Path::new("/roms/game.sfc"), ID, Some(PathBuf::from("/saves")), None);
        assert_eq!(folder.srm_write(), PathBuf::from("/saves/SUPER_MARIOWORLD-A0DA.srm"));
        assert_eq!(folder.dir(), Some(Path::new("/saves")));
    }

    /// Nothing is ever written into the previous folder: it is a read-only
    /// fallback.
    #[test]
    fn the_previous_folder_is_never_a_write_target() {
        let paths = GamePaths::new(Path::new("/roms/game.sfc"), ID, None, None)
            .with_previous_dir(Some(PathBuf::from("/old-saves")));
        assert_eq!(paths.srm_write(), PathBuf::from("/roms/game.srm"));
        assert_eq!(paths.resume_write(), PathBuf::from("/roms/game.resume"));
        for slot in 0..SLOT_COUNT {
            assert!(!paths.state_write(slot).starts_with("/old-saves"));
        }
    }

    #[test]
    fn preparing_a_folder_creates_it_and_reports_an_unusable_one() {
        let root = scratch("prepare");
        let nested = root.join("a").join("b");
        assert!(prepare_dir(&nested).is_ok());
        assert!(nested.is_dir());
        // No probe file survives the check.
        let leftovers: Vec<_> = std::fs::read_dir(&nested)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(leftovers.is_empty(), "probe file left behind: {leftovers:?}");

        // A *file* where the folder should be: reported, never a panic.
        let as_file = root.join("not-a-dir");
        std::fs::write(&as_file, b"x").expect("write");
        assert!(prepare_dir(&as_file).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
