//! Game library: ROM-folder scan, on-disk metadata cache, ordering rules and
//! the background worker that feeds both.
//!
//! **What is cached and where.** Parsing a header means reading (and, for a
//! `.zip`, inflating) the whole ROM, so the result is cached as JSON in
//! `<os config dir>/Prisme/library.json` (`prefs::data_path`). One entry per
//! ROM file; an entry is reused only when the file's `(path, size, mtime)`
//! triple still matches what was recorded, so replacing a ROM in place —
//! same name, different dump — invalidates it. Files that disappeared are
//! dropped on the next scan, since the scan rebuilds the list from the
//! directory and only *consults* the cache. A cache written by a newer
//! `CACHE_VERSION` is ignored wholesale rather than migrated: it is derived
//! data, rebuilt in one scan.
//!
//! **Threading.** `Worker` owns the cache and runs on its own thread: the scan
//! (file I/O + header parsing) and the thumbnail generation (a real headless
//! emulation run, see `thumbs`) must never block the UI. The main thread sends
//! `Job`s and polls `Update`s; nothing else crosses the channel, so no
//! emulator type has to be `Send`.
//!
//! Everything in this module except `Worker`/`scan_dir` is pure logic over
//! plain data, which is what makes the ordering, filtering and formatting
//! rules unit-testable on a machine with no display.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use snes_core::{Cartridge, Mapping, Region};

use crate::i18n::{Lang, Msg};
use crate::prefs::{GameStats, Prefs};

/// File extensions the library picks up, matching what `load_rom_bytes` can
/// open. The native picker filters on this very constant (re-exported as
/// `picker::ROM_EXTENSIONS`), so the folder the grid lists and the files the
/// panel offers cannot drift apart.
pub const ROM_EXTENSIONS: &[&str] = &["sfc", "smc", "zip"];

/// Name of the metadata cache inside the application's config directory.
pub const CACHE_FILE: &str = "library.json";

/// Layout version of `library.json`. Bump when a field's meaning changes; a
/// file with any other version is discarded and rebuilt.
pub const CACHE_VERSION: u32 = 1;

/// Directory the library scans, in order of preference: the dedicated
/// `library_dir` preference, then the last directory the ROM picker used
/// (written by `video::App::pump_dialogs` after a ROM is chosen), then `roms/`
/// next to the working directory. The same folder is what the ROM picker opens
/// in, so the two notions of "my ROM folder" stay one.
pub fn library_dir(prefs: &Prefs) -> PathBuf {
    prefs
        .library_dir
        .clone()
        .or_else(|| prefs.last_rom_dir.clone())
        .unwrap_or_else(|| PathBuf::from("roms"))
}

/// How many bytes of the ROM image feed the fallback fingerprint below.
const ID_FINGERPRINT_BYTES: usize = 512;

/// Stable identity of a game: sanitized cartridge title + the header checksum,
/// e.g. `SUPER_MARIOWORLD-A0DA`. Deliberately *not* the file path — a player
/// who moves or renames their ROM keeps their play time, favourite flag and
/// promoted thumbnail; two different dumps of the same title (different
/// revisions/regions) still get their own entry, since the checksum differs.
///
/// A dump whose header carries neither a title nor a checksum (blank title,
/// checksum 0 — homebrew, a bad dump, an unrecognised header) would collapse
/// onto the single id `SNES-0000`, merging the play time of two unrelated games
/// and showing one's thumbnail for the other. Those get a fingerprint of the
/// image itself appended, so distinct dumps keep distinct identities; a
/// well-formed header is untouched, and ids already written in `prefs.json`
/// stay valid.
pub fn game_id(title: &str, checksum: u16, rom: &[u8]) -> String {
    let base = format!("{}-{:04X}", crate::sanitize_file_stem(title), checksum);
    if !title.trim().is_empty() && checksum != 0 {
        return base;
    }
    format!("{base}-{:08X}", fingerprint(rom))
}

/// FNV-1a over the image length followed by its first `ID_FINGERPRINT_BYTES`
/// bytes: cheap (the image is already in memory), stable across runs and
/// machines, and enough to tell two headerless dumps apart. Not a checksum of
/// the whole ROM — this is an identity discriminator, not an integrity check.
fn fingerprint(rom: &[u8]) -> u32 {
    let len = (rom.len() as u64).to_le_bytes();
    let mut hash: u32 = 0x811C_9DC5;
    for byte in len.iter().chain(rom.iter().take(ID_FINGERPRINT_BYTES)) {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Coprocessor detected in the cartridge header by the core, or `None` for a
/// plain LoROM/HiROM cart. The four variants are the ones snes-core actually
/// emulates.
pub fn coprocessor(cart: &Cartridge) -> Option<&'static str> {
    if cart.superfx.is_some() {
        Some("SuperFX")
    } else if cart.sa1.is_some() {
        Some("SA-1")
    } else if cart.dsp1.is_some() {
        Some("DSP-1")
    } else if cart.cx4.is_some() {
        Some("CX4")
    } else {
        None
    }
}

/// One game as the library knows it: what the header says plus the file facts
/// the cache invalidates on. Serialized as-is into `library.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameEntry {
    /// `game_id(title, checksum)`; the key into `prefs.games`.
    pub id: String,
    pub path: PathBuf,
    /// Size of the ROM *file* on disk (a `.zip` is smaller than its content).
    pub file_size: u64,
    /// File mtime, seconds since the Unix epoch.
    pub modified: i64,
    /// Cartridge title, trimmed.
    pub title: String,
    /// `LoROM` / `HiROM`.
    pub mapping: String,
    /// `PAL` / `NTSC`.
    pub region: String,
    /// Size of the ROM image once unpacked and de-headered.
    pub rom_bytes: u64,
    /// Battery SRAM declared by the header, 0 when the cart has none.
    pub sram_bytes: u64,
    /// `SuperFX` / `SA-1` / `DSP-1` / `CX4`, `None` for a plain cart.
    pub coprocessor: Option<String>,
    pub fastrom: bool,
    pub checksum: u16,
    pub checksum_valid: bool,
    /// Set on a game added by hand whose file is no longer where it was. A game
    /// that vanishes from the scanned folder is simply gone — it is not there
    /// any more — but one the player pointed at explicitly must not evaporate
    /// in silence, which reads as a bug. It stays listed, greyed, until they
    /// relocate or forget it. Never true for a folder entry.
    #[serde(default)]
    pub missing: bool,
}

impl GameEntry {
    /// File name of the ROM, used by the search (players often remember the
    /// file name — `Secret of Mana (F).zip` — rather than the header title,
    /// which is uppercase and truncated to 21 characters).
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Header title if it is not blank, else the file name — a handful of
    /// dumps carry an empty or garbage title field.
    pub fn display_title(&self) -> String {
        if self.title.trim().is_empty() {
            self.file_name()
        } else {
            self.title.clone()
        }
    }
}

/// Parse one ROM file into a library entry. Reads the whole file (a `.zip` is
/// inflated) because the header can only be scored against the real image.
pub fn read_entry(path: &Path) -> Result<GameEntry, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("stat {}: {e}", path.display()))?;
    let bytes = crate::load_rom_bytes(path)?;
    let cart = Cartridge::from_bytes(bytes)?;
    Ok(entry_from_cart(path, file_size(&meta), file_mtime(&meta), &cart))
}

/// Build an entry from an already-parsed cartridge; split out so the
/// conversion itself can be exercised without touching a file.
fn entry_from_cart(path: &Path, file_size: u64, modified: i64, cart: &Cartridge) -> GameEntry {
    let title = cart.title.trim().to_string();
    GameEntry {
        id: game_id(&title, cart.header_checksum, &cart.rom),
        path: path.to_path_buf(),
        file_size,
        modified,
        title,
        mapping: match cart.mapping {
            Mapping::LoRom => "LoROM".to_string(),
            Mapping::HiRom => "HiROM".to_string(),
        },
        region: match cart.region {
            Region::Ntsc => "NTSC".to_string(),
            Region::Pal => "PAL".to_string(),
        },
        rom_bytes: cart.rom.len() as u64,
        sram_bytes: cart.sram.len() as u64,
        coprocessor: coprocessor(cart).map(|s| s.to_string()),
        fastrom: cart.fastrom,
        checksum: cart.header_checksum,
        checksum_valid: cart.checksum_valid,
        missing: false,
    }
}

/// Placeholder for an added game whose file is gone: the last known entry if
/// the cache still holds one, else the bare minimum built from the file name.
/// Either way it carries `missing`, so the shell can show it as unreachable
/// rather than pretend it was never added.
fn missing_entry(path: &Path, cache: &Cache) -> GameEntry {
    if let Some(known) = cache.entries.iter().find(|e| e.path == path) {
        return GameEntry { missing: true, ..known.clone() };
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    GameEntry {
        // No header was ever read, so there is no checksum to key on; the path
        // is the only stable identity such an entry can have.
        id: format!("missing-{}", normalize(&name)),
        path: path.to_path_buf(),
        file_size: 0,
        modified: 0,
        title: name,
        mapping: String::new(),
        region: String::new(),
        rom_bytes: 0,
        sram_bytes: 0,
        coprocessor: None,
        fastrom: false,
        checksum: 0,
        checksum_valid: false,
        missing: true,
    }
}

fn file_size(meta: &std::fs::Metadata) -> u64 {
    meta.len()
}

/// File mtime as a Unix timestamp; 0 when the platform/filesystem cannot
/// report one (the entry then simply re-parses on every scan).
fn file_mtime(meta: &std::fs::Metadata) -> i64 {
    meta.modified().ok().map(unix_secs).unwrap_or(0)
}

/// `SystemTime` -> seconds since the Unix epoch, negative before 1970.
pub fn unix_secs(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// Now, as a Unix timestamp.
pub fn now_unix() -> i64 {
    unix_secs(SystemTime::now())
}

/// True when `path`'s extension is one the library scans (case-insensitive).
pub fn is_rom_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ROM_EXTENSIONS.iter().any(|r| e.eq_ignore_ascii_case(r)))
}

/// On-disk metadata cache. `entries` is the last scan's result, in scan order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cache {
    pub version: u32,
    pub entries: Vec<GameEntry>,
}

impl Default for Cache {
    fn default() -> Self {
        Self { version: CACHE_VERSION, entries: Vec::new() }
    }
}

impl Cache {
    /// Read the cache from `path`; a missing, unreadable, malformed or
    /// foreign-version file yields an empty cache (it is derived data — never
    /// an error worth surfacing to the player).
    pub fn read_from(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(c) if c.version == CACHE_VERSION => c,
            Ok(_) => Self::default(),
            Err(e) => {
                eprintln!("library: ignoring malformed {}: {e}", path.display());
                Self::default()
            }
        }
    }

    /// Atomic write (temp file + rename), like every other file this frontend
    /// persists.
    pub fn write_to(&self, path: &Path) -> Result<(), String> {
        let mut json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("could not serialize the library cache: {e}"))?;
        json.push('\n');
        crate::atomic::write(path, json.as_bytes())
    }

    /// Cached entry for `path` whose recorded size and mtime still match
    /// `meta`; `None` when the file is unknown or changed since.
    pub fn valid_entry(&self, path: &Path, size: u64, modified: i64) -> Option<&GameEntry> {
        self.entries
            .iter()
            .find(|e| e.path == path && e.file_size == size && e.modified == modified)
    }
}

/// Full path of `library.json`, or `None` with no config directory.
pub fn cache_path() -> Option<PathBuf> {
    crate::prefs::data_path(CACHE_FILE)
}

/// Scan `dir` for ROM files, reusing `cache` for every file that has not
/// changed and re-parsing the others; `cache` is updated in place to exactly
/// the set of files found (stale entries disappear).
///
/// Sub-directories are not descended into: a library folder is a flat folder
/// of games, and recursing would make an accidentally-pointed-at home
/// directory read gigabytes.
#[cfg(test)]
pub fn scan_dir(dir: &Path, cache: &mut Cache) -> Result<Vec<GameEntry>, String> {
    scan(dir, &[], cache)
}

/// Scan `dir` and fold in `extra` — games the player added one by one, which
/// live wherever they live. A folder scan alone cannot represent those: it
/// rebuilds the list from the directory, so anything outside it would come back
/// only to vanish at the next pass.
///
/// A path listed in both is kept once, as a folder entry: adding a game that
/// already sits in the library folder is a no-op, not a duplicate card.
pub fn scan(dir: &Path, extra: &[PathBuf], cache: &mut Cache) -> Result<Vec<GameEntry>, String> {
    let mut paths = rom_paths_in(dir)?;
    let mut seen: Vec<PathBuf> = paths.iter().map(|p| identity(p)).collect();
    let mut added: Vec<PathBuf> = Vec::new();
    for path in extra {
        let key = identity(path);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        added.push(path.clone());
    }
    paths.sort();

    let mut entries = Vec::with_capacity(paths.len() + added.len());
    for path in paths {
        if let Some(entry) = entry_for(&path, cache) {
            entries.push(entry);
        }
    }
    for path in added {
        // Unlike a folder entry, an added game that will not parse still gets a
        // card: the player asked for this file by name and deserves to be told
        // it cannot be read, rather than watching it silently not appear.
        entries.push(entry_for(&path, cache).unwrap_or_else(|| missing_entry(&path, cache)));
    }
    cache.version = CACHE_VERSION;
    cache.entries = entries.clone();
    Ok(entries)
}

/// ROM files directly inside `dir`, sorted.
fn rom_paths_in(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let read = std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() || !is_rom_file(&path) {
            continue;
        }
        paths.push(path);
    }
    paths.sort();
    Ok(paths)
}

/// Cached or freshly parsed entry for one ROM file; `None` when the file is
/// unreachable or is not a usable ROM (bad dump, unrelated zip) — a folder scan
/// skips those with a warning rather than failing wholesale.
fn entry_for(path: &Path, cache: &Cache) -> Option<GameEntry> {
    let meta = std::fs::metadata(path).ok()?;
    let (size, modified) = (file_size(&meta), file_mtime(&meta));
    if let Some(cached) = cache.valid_entry(path, size, modified) {
        return Some(cached.clone());
    }
    match read_entry(path) {
        Ok(e) => Some(e),
        Err(e) => {
            eprintln!("library: skipping {}: {e}", path.display());
            None
        }
    }
}

/// Key two paths are compared on. `canonicalize` resolves `..`, symlinks and
/// (on macOS) case, so the same file reached by two different spellings is one
/// game; it needs the file to exist, hence the fallback for a game whose file
/// has moved away.
fn identity(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

// --- ordering / filtering -------------------------------------------------

/// Sort order offered by the library screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    /// Alphabetical by displayed title.
    Title,
    /// Most recently played first; never-played games keep the title order
    /// after them.
    Recent,
}

impl Default for SortMode {
    fn default() -> Self {
        SortMode::Title
    }
}

impl SortMode {
    /// Value stored in `prefs.library_sort`.
    pub fn as_pref(self) -> &'static str {
        match self {
            SortMode::Title => "title",
            SortMode::Recent => "recent",
        }
    }

    /// Unknown names read as `Title`, the same lenient rule as
    /// `render::Filter::from_pref` (an unknown value still round-trips
    /// through the preferences file untouched).
    pub fn from_pref(s: &str) -> Self {
        match s {
            "recent" => SortMode::Recent,
            _ => SortMode::Title,
        }
    }

    pub fn label(self, lang: Lang) -> &'static str {
        match self {
            SortMode::Title => Msg::SortTitle.text(lang),
            SortMode::Recent => Msg::SortRecent.text(lang),
        }
    }

    /// The two modes, in the order the UI offers them.
    pub const ALL: [SortMode; 2] = [SortMode::Title, SortMode::Recent];
}

/// Fold a string for searching: lowercase, common accented Latin letters
/// reduced to their base letter, everything that is not alphanumeric dropped.
/// So `Secret of Mana (F).zip` matches `mana`, `secretof` and `SECRET OF`, and
/// `Pokémon` matches `pokemon`.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let c = match c {
            'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' | 'À' | 'Á' | 'Â' | 'Ä' | 'Ã' | 'Å' => 'a',
            'ç' | 'Ç' => 'c',
            'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => 'i',
            'ñ' | 'Ñ' => 'n',
            'ò' | 'ó' | 'ô' | 'ö' | 'õ' | 'Ò' | 'Ó' | 'Ô' | 'Ö' | 'Õ' => 'o',
            'ù' | 'ú' | 'û' | 'ü' | 'Ù' | 'Ú' | 'Û' | 'Ü' => 'u',
            'ý' | 'ÿ' | 'Ý' => 'y',
            other => other,
        };
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// Whether `entry` matches the search box: a normalized substring of either
/// the title or the file name. `query` must already be folded by `normalize` —
/// a filtering pass does that once instead of once per entry. An empty query
/// matches everything.
pub fn matches(entry: &GameEntry, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    normalize(&entry.display_title()).contains(query)
        || normalize(&entry.file_name()).contains(query)
}

/// Order the library for display: favourites first (pinned, in the same order
/// they would otherwise have), then everything else, both according to `sort`.
/// Only entries matching `query` are returned.
///
/// Called on every UI frame, so the folded sort key of each entry is built once
/// up front rather than inside the comparator, where a library of 200 games
/// would allocate two strings per comparison (~3000 allocations per frame).
pub fn arrange<'a>(
    entries: &'a [GameEntry],
    query: &str,
    sort: SortMode,
    games: &BTreeMap<String, GameStats>,
) -> Vec<&'a GameEntry> {
    let query = normalize(query);
    let mut out: Vec<(String, &GameEntry)> = entries
        .iter()
        .filter(|e| matches(e, &query))
        .map(|e| (normalize(&e.display_title()), e))
        .collect();
    out.sort_by(|(key_a, a), (key_b, b)| {
        let fav_a = games.get(&a.id).is_some_and(|s| s.favorite);
        let fav_b = games.get(&b.id).is_some_and(|s| s.favorite);
        // `false < true`, so reverse to pin favourites at the head.
        fav_b.cmp(&fav_a).then_with(|| match sort {
            SortMode::Title => key_a.cmp(key_b),
            SortMode::Recent => {
                let la = games.get(&a.id).and_then(|s| s.last_played);
                let lb = games.get(&b.id).and_then(|s| s.last_played);
                // Never-played games (None) sort after every played one.
                lb.cmp(&la).then_with(|| key_a.cmp(key_b))
            }
        })
    });
    out.into_iter().map(|(_, e)| e).collect()
}

// --- formatting -----------------------------------------------------------

/// Human size, in each language's own units and decimal mark: `2,0 Mo` reads
/// as a typo in English and `2.0 MB` reads as one in French.
pub fn format_size(lang: Lang, bytes: u64) -> String {
    const KO: u64 = 1024;
    const MO: u64 = 1024 * 1024;
    let (mega, kilo, byte, point) = match lang {
        Lang::Fr => ("Mo", "Ko", "o", ","),
        Lang::En => ("MB", "KB", "B", "."),
    };
    if bytes >= MO {
        let mo = bytes as f64 / MO as f64;
        format!("{mo:.1} {mega}").replace('.', point)
    } else if bytes >= KO {
        format!("{} {kilo}", bytes / KO)
    } else {
        format!("{bytes} {byte}")
    }
}

/// Battery SRAM line of the game sheet.
pub fn format_sram(lang: Lang, bytes: u64) -> String {
    if bytes == 0 {
        Msg::NoneFeminine.text(lang).to_string()
    } else {
        format_size(lang, bytes)
    }
}

/// Cumulated play time, rounded down to the minute above one hour. Only the
/// two sentences are prose; `20 min` and `4 h 30` read the same either way.
pub fn format_play_time(lang: Lang, seconds: u64) -> String {
    if seconds == 0 {
        return Msg::NeverPlayed.text(lang).to_string();
    }
    if seconds < 60 {
        return Msg::LessThanAMinute.text(lang).to_string();
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes} min");
    }
    format!("{} h {:02}", minutes / 60, minutes % 60)
}

/// Local calendar date/time of a Unix timestamp: `JJ/MM/AAAA HH:MM` in French,
/// and ISO `AAAA-MM-JJ HH:MM` in English — never `MM/DD`, which the two halves
/// of the English-speaking world read differently.
pub fn format_date(lang: Lang, unix: i64) -> String {
    let t = crate::local_time(unix);
    match lang {
        Lang::Fr => {
            format!("{:02}/{:02}/{:04} {:02}:{:02}", t.day, t.month, t.year, t.hour, t.minute)
        }
        Lang::En => {
            format!("{:04}-{:02}-{:02} {:02}:{:02}", t.year, t.month, t.day, t.hour, t.minute)
        }
    }
}

// --- per-game files -------------------------------------------------------

/// A save state that exists on disk for a game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateFile {
    /// Manual slot number, or `None` for the automatic session state
    /// (`<rom>.resume`, written on every exit — see `state::resume_path`).
    pub slot: Option<u8>,
    pub path: PathBuf,
    pub size: u64,
    pub modified: i64,
    /// Framebuffer picture written beside the state when it was saved
    /// (`crate::state::preview_path`), when there is one. Always optional: a
    /// state written by an earlier version, or one whose picture failed to
    /// write, simply shows a neutral plate in the sheet.
    pub preview: Option<PathBuf>,
}

impl StateFile {
    pub fn label(&self, lang: Lang) -> String {
        crate::i18n::slot_label(lang, self.slot)
    }
}

/// Save states that currently exist for the game `paths` describes: the
/// automatic resume snapshot first, then the manual slots in order.
///
/// Each slot is looked up exactly where loading it would look
/// (`paths::GamePaths::state_read`), so a state still sitting beside the ROM
/// after a folder was configured is listed rather than reported missing. The
/// caller passes the *running* session's own `GamePaths` for the game that is
/// loaded (it is frozen at load time, and F9 reads through it) and builds one
/// from the current preferences for every other entry of the library.
pub fn save_states(paths: &crate::paths::GamePaths) -> Vec<StateFile> {
    let mut out = Vec::new();
    let mut push = |slot: Option<u8>, path: PathBuf| {
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.is_file() {
                let preview = crate::state::preview_path(&path);
                let preview = preview.is_file().then_some(preview);
                out.push(StateFile {
                    slot,
                    path,
                    size: file_size(&meta),
                    modified: file_mtime(&meta),
                    preview,
                });
            }
        }
    };
    push(None, paths.resume_read());
    for slot in 0..crate::state::SLOT_COUNT {
        push(Some(slot), paths.state_read(slot));
    }
    out
}

/// Directory `App::take_screenshot` writes this game's captures to:
/// `prefs.screenshot_dir` when set, else a `Screenshots` folder beside the
/// ROM.
pub fn screenshot_dir(rom_path: &Path, prefs: &Prefs) -> PathBuf {
    prefs.screenshot_dir.clone().unwrap_or_else(|| match rom_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join("Screenshots"),
        _ => PathBuf::from("Screenshots"),
    })
}

/// Captures belonging to `title`, newest first.
///
/// Two layouts are accepted: the flat one the screenshot hotkey writes today
/// (`Screenshots/<TITRE>_<horodatage>.png`, matched on the file-name prefix)
/// and a per-game sub-folder (`Screenshots/<TITRE>/*.png`), so a future
/// re-organisation of the capture folder needs no change here.
pub fn screenshots(dir: &Path, title: &str) -> Vec<PathBuf> {
    let stem = crate::sanitize_file_stem(title);
    let mut found: Vec<(i64, PathBuf)> = Vec::new();
    let mut collect = |dir: &Path, require_prefix: bool| {
        let Ok(read) = std::fs::read_dir(dir) else { return };
        for entry in read.flatten() {
            let path = entry.path();
            let is_png = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("png"));
            if !is_png {
                continue;
            }
            if require_prefix {
                let name = path.file_stem().map(|s| s.to_string_lossy().into_owned());
                let Some(name) = name else { continue };
                // `<stem>_<stamp>` and `<stem>_<stamp>_2` both belong to the
                // game; a different title sharing the prefix would need the
                // exact same sanitized stem, i.e. be the same game.
                if !name.starts_with(&format!("{stem}_")) {
                    continue;
                }
            }
            let modified = std::fs::metadata(&path).map(|m| file_mtime(&m)).unwrap_or(0);
            found.push((modified, path));
        }
    };
    collect(dir, true);
    collect(&dir.join(&stem), false);
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.into_iter().map(|(_, p)| p).collect()
}

/// Picture to display for a game: the screenshot the player promoted when it
/// still exists on disk, else the generated thumbnail when it has been
/// produced. `None` means "placeholder, and a thumbnail still has to be
/// generated" — the single rule both the display and the generation queue
/// read, so the grid can never show a placeholder for a game nobody will ever
/// generate a picture for.
pub fn resolve_picture(custom: Option<&Path>, generated: Option<&Path>) -> Option<PathBuf> {
    if let Some(custom) = custom {
        if custom.is_file() {
            return Some(custom.to_path_buf());
        }
    }
    generated.filter(|p| p.is_file()).map(|p| p.to_path_buf())
}

// --- play-time accounting -------------------------------------------------

/// Accumulates play time in whole seconds, keeping the sub-second remainder so
/// no time is lost to rounding over a long session (a frame is ~20 ms: a naive
/// `as_secs()` per frame would count zero forever).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PlayClock {
    carry: Duration,
}

impl PlayClock {
    /// Add `dt` of running time and return the whole seconds to commit to the
    /// game's counter.
    pub fn add(&mut self, dt: Duration) -> u64 {
        self.carry += dt;
        let whole = self.carry.as_secs();
        self.carry -= Duration::from_secs(whole);
        whole
    }

    /// Drop the pending remainder (session ended / game switched), so time
    /// never leaks from one game onto the next.
    pub fn reset(&mut self) {
        self.carry = Duration::ZERO;
    }
}

// --- background worker ----------------------------------------------------

/// Work the library thread can be asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// Rescan a folder and re-read the individually added games (uses and
    /// updates the on-disk cache).
    Scan { dir: PathBuf, extra: Vec<PathBuf> },
    /// Generate the thumbnail of one game, unless its file already exists.
    Thumb { id: String, rom: PathBuf },
}

/// What the library thread reports back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Scanned { dir: PathBuf, entries: Vec<GameEntry>, error: Option<String> },
    /// `path` is the thumbnail PNG; `None` when generation failed (the game
    /// then keeps the placeholder and is not retried this run).
    Thumb { id: String, path: Option<PathBuf> },
}

/// The worker's job queue. Deliberately **not** a plain FIFO: a scan is what
/// the player is waiting on (they just changed folder or pressed `Actualiser`)
/// while a thumbnail is background work worth seconds of emulation each, so
/// scans are drained first and submitting one drops the thumbnails still
/// queued — they belong to the folder being replaced, and the scan's own
/// `request_thumbnails` re-submits whatever is still missing. A thumbnail run
/// already in progress is not interrupted (it is a plain emulation loop), so a
/// scan waits at most one thumbnail, not the whole backlog.
#[derive(Debug, Default)]
struct Queue {
    scans: VecDeque<(PathBuf, Vec<PathBuf>)>,
    thumbs: VecDeque<(String, PathBuf)>,
    /// Set when the handle is dropped: the thread returns instead of waiting.
    closed: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

/// A poisoned queue lock is recovered rather than propagated: the library is
/// derived data and a panicked worker must not take the session with it.
fn lock(queue: &Mutex<Queue>) -> MutexGuard<'_, Queue> {
    queue.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn new() -> Self {
        Self { queue: Mutex::new(Queue::default()), wake: Condvar::new() }
    }

    fn push(&self, job: Job) {
        {
            let mut queue = lock(&self.queue);
            match job {
                Job::Scan { dir, extra } => {
                    queue.thumbs.clear();
                    queue.scans.push_back((dir, extra));
                }
                Job::Thumb { id, rom } => queue.thumbs.push_back((id, rom)),
            }
        }
        self.wake.notify_one();
    }

    /// Next job, waiting for one to arrive; `None` once the handle is gone.
    fn take(&self) -> Option<Job> {
        let mut queue = lock(&self.queue);
        loop {
            if queue.closed {
                return None;
            }
            if let Some((dir, extra)) = queue.scans.pop_front() {
                return Some(Job::Scan { dir, extra });
            }
            if let Some((id, rom)) = queue.thumbs.pop_front() {
                return Some(Job::Thumb { id, rom });
            }
            queue = match self.wake.wait(queue) {
                Ok(guard) => guard,
                Err(e) => e.into_inner(),
            };
        }
    }

    fn close(&self) {
        lock(&self.queue).closed = true;
        self.wake.notify_all();
    }

    /// Queued (scans, thumbnails). Test-only: the UI reads its own `pending`
    /// set, never the worker's queue.
    #[cfg(test)]
    fn queued(&self) -> (usize, usize) {
        let queue = lock(&self.queue);
        (queue.scans.len(), queue.thumbs.len())
    }
}

/// Handle on the library thread. One thread, one job at a time: thumbnail
/// generation keeps running when the player goes back to a game, so at most
/// one core is ever taken from the emulator, and the queue drains by itself.
///
/// Dropping the handle closes the queue, which ends the thread after its
/// current job; it is deliberately **not** joined — a thumbnail run in progress
/// would otherwise delay quitting by the time it takes to emulate a few hundred
/// frames. Its writes are atomic (temp file + rename), so a thread killed by
/// process exit can at worst leave an unused temp file behind, never a
/// half-written thumbnail or cache.
pub struct Worker {
    shared: Arc<Shared>,
    updates: Receiver<Update>,
}

impl Worker {
    /// Start the library thread. `None` when the OS refuses one (thread limit,
    /// no memory): the library is a convenience, so the shell keeps running
    /// without it — every caller already handles a missing worker — rather than
    /// the application dying on the home screen.
    pub fn spawn() -> Option<Self> {
        let shared = Arc::new(Shared::new());
        let (updates_tx, updates_rx) = channel::<Update>();
        let worker_shared = Arc::clone(&shared);
        if let Err(e) = std::thread::Builder::new()
            .name("prisme-library".to_string())
            .spawn(move || worker_loop(&worker_shared, &updates_tx))
        {
            eprintln!("library: could not start the library thread: {e}");
            return None;
        }
        Some(Self { shared, updates: updates_rx })
    }

    /// Queue a job (see `Queue` for the ordering rules).
    pub fn submit(&self, job: Job) {
        self.shared.push(job);
    }

    /// Take every update produced since the last call. Never blocks.
    pub fn poll(&self) -> Vec<Update> {
        self.updates.try_iter().collect()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.close();
    }
}

fn worker_loop(shared: &Arc<Shared>, updates: &Sender<Update>) {
    let cache_path = cache_path();
    let mut cache = cache_path.as_deref().map(Cache::read_from).unwrap_or_default();
    while let Some(job) = shared.take() {
        match job {
            Job::Scan { dir, extra } => {
                let (entries, error) = match scan(&dir, &extra, &mut cache) {
                    Ok(entries) => {
                        if let Some(path) = &cache_path {
                            if let Err(e) = cache.write_to(path) {
                                eprintln!("library: {e}");
                            }
                        }
                        (entries, None)
                    }
                    Err(e) => (Vec::new(), Some(e)),
                };
                if updates.send(Update::Scanned { dir, entries, error }).is_err() {
                    return;
                }
            }
            Job::Thumb { id, rom } => {
                // With no config directory (no `$HOME`/`%APPDATA%`) there is
                // nowhere to cache a picture: answer "none" like a failed
                // generation, so the card drops its "en cours" marker instead
                // of waiting for an update that can never come.
                let result = match crate::thumbs::thumb_path(&id) {
                    Some(path) if path.is_file() => Ok(Some(path)),
                    Some(path) => crate::thumbs::generate(&rom, &path).map(|()| Some(path)),
                    None => Ok(None),
                };
                let update = match result {
                    Ok(path) => Update::Thumb { id, path },
                    Err(e) => {
                        eprintln!("thumbnail: {} ({e})", rom.display());
                        Update::Thumb { id, path: None }
                    }
                };
                if updates.send(update).is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch directory per test — no test may share a directory with
    /// another (they run concurrently).
    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("prisme_lib_{}_{}", std::process::id(), tag))
    }

    fn entry(id: &str, title: &str) -> GameEntry {
        GameEntry {
            id: id.to_string(),
            path: PathBuf::from(format!("/roms/{title}.sfc")),
            file_size: 1024,
            modified: 42,
            title: title.to_string(),
            mapping: "LoROM".to_string(),
            region: "PAL".to_string(),
            rom_bytes: 1024,
            sram_bytes: 0,
            coprocessor: None,
            fastrom: false,
            checksum: 0x1234,
            checksum_valid: true,
            missing: false,
        }
    }

    #[test]
    fn game_id_combines_the_sanitized_title_and_the_checksum() {
        let rom = [0u8; 1024];
        assert_eq!(game_id("SUPER MARIOWORLD", 0xA0DA, &rom), "SUPER_MARIOWORLD-A0DA");
        // Same title, different dump -> different identity.
        assert_ne!(
            game_id("SECRET OF MANA", 0x0001, &rom),
            game_id("SECRET OF MANA", 0x0002, &rom)
        );
        // Path-independent by construction: the id never mentions the file.
        assert!(!game_id("GAME", 0xBEEF, &rom).contains('/'));
        // The image plays no part as long as the header is usable, so an id
        // already stored in `prefs.json` keeps matching.
        assert_eq!(
            game_id("GAME", 0xBEEF, &rom),
            game_id("GAME", 0xBEEF, &[0xFFu8; 4096])
        );
    }

    #[test]
    fn headerless_dumps_do_not_collide_on_one_identity() {
        // Blank title and no checksum: without a discriminator both dumps would
        // be `SNES-0000` and share play time, favourite flag and thumbnail.
        let a = game_id("   ", 0x0000, &[1u8; 1024]);
        let b = game_id("   ", 0x0000, &[2u8; 1024]);
        assert!(a.starts_with("SNES-0000-"), "{a}");
        assert_ne!(a, b);
        // Same content, same id: the identity must stay stable across runs.
        assert_eq!(a, game_id("", 0x0000, &[1u8; 1024]));
        // Same first bytes but a different image length still differ.
        assert_ne!(game_id("", 0x0000, &[1u8; 512]), game_id("", 0x0000, &[1u8; 1024]));
        // A blank title with a real checksum, or a title with checksum 0, are
        // both ambiguous halves and get the fingerprint too.
        assert!(game_id("", 0x1234, &[3u8; 64]).starts_with("SNES-1234-"));
        assert!(game_id("GAME", 0x0000, &[3u8; 64]).starts_with("GAME-0000-"));
    }

    #[test]
    fn rom_files_are_recognized_case_insensitively() {
        for name in ["a.sfc", "a.SMC", "a.Zip", "a.sMc"] {
            assert!(is_rom_file(Path::new(name)), "{name}");
        }
        for name in ["a.png", "a.srm", "a.state", "a", "a.zipx"] {
            assert!(!is_rom_file(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn cache_entries_are_invalidated_by_size_or_mtime() {
        let mut cache = Cache::default();
        cache.entries.push(entry("A-0001", "A"));
        let path = PathBuf::from("/roms/A.sfc");
        assert!(cache.valid_entry(&path, 1024, 42).is_some());
        assert!(cache.valid_entry(&path, 2048, 42).is_none(), "a resized file must re-parse");
        assert!(cache.valid_entry(&path, 1024, 43).is_none(), "a touched file must re-parse");
        assert!(cache.valid_entry(Path::new("/roms/B.sfc"), 1024, 42).is_none());
    }

    #[test]
    fn a_cache_from_another_version_is_discarded() {
        let dir = scratch("cacheversion");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join(CACHE_FILE);
        let mut cache = Cache::default();
        cache.entries.push(entry("A-0001", "A"));
        cache.write_to(&path).expect("write");
        assert_eq!(Cache::read_from(&path).entries.len(), 1);

        // A future layout is ignored wholesale rather than half-read.
        let text = std::fs::read_to_string(&path).unwrap();
        let bumped = text.replace(
            &format!("\"version\": {CACHE_VERSION}"),
            &format!("\"version\": {}", CACHE_VERSION + 1),
        );
        std::fs::write(&path, bumped).expect("rewrite");
        assert_eq!(Cache::read_from(&path), Cache::default());

        // So is a corrupt one, and a missing file.
        std::fs::write(&path, b"{ not json").expect("rewrite");
        assert_eq!(Cache::read_from(&path), Cache::default());
        assert_eq!(Cache::read_from(&dir.join("absent.json")), Cache::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_round_trips_every_field_through_json() {
        let dir = scratch("roundtrip");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join(CACHE_FILE);
        let mut cache = Cache::default();
        let mut e = entry("MK-BEEF", "SUPER MARIOKART");
        e.coprocessor = Some("DSP-1".to_string());
        e.sram_bytes = 8192;
        e.fastrom = true;
        cache.entries.push(e.clone());
        cache.write_to(&path).expect("write");
        let back = Cache::read_from(&path);
        assert_eq!(back.entries, vec![e]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scanning_a_directory_only_reparses_changed_files() {
        let dir = scratch("scan");
        std::fs::create_dir_all(&dir).expect("mkdir");
        // Not a valid ROM: the scan must skip it, not fail.
        std::fs::write(dir.join("broken.sfc"), b"too small").expect("write");
        std::fs::write(dir.join("notes.txt"), b"ignored").expect("write");
        let mut cache = Cache::default();
        let found = scan_dir(&dir, &mut cache).expect("scan");
        assert!(found.is_empty(), "{found:?}");
        assert!(cache.entries.is_empty());

        // A cached entry for a file that no longer exists disappears.
        cache.entries.push(entry("GONE-0001", "GONE"));
        let found = scan_dir(&dir, &mut cache).expect("scan");
        assert!(found.is_empty());
        assert!(cache.entries.is_empty(), "stale entries must be dropped");

        assert!(scan_dir(&dir.join("absent"), &mut cache).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_added_game_whose_file_is_gone_stays_listed_as_missing() {
        let dir = scratch("extra-missing");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let gone = dir.join("elsewhere").join("Chrono Trigger (U).sfc");
        let mut cache = Cache::default();

        let found = scan(&dir, std::slice::from_ref(&gone), &mut cache).expect("scan");
        // The folder is empty, so the added game is the whole library — and it
        // must be there: a game the player named explicitly disappearing without
        // a word reads as a bug, not as a moved file.
        assert_eq!(found.len(), 1);
        assert!(found[0].missing);
        assert_eq!(found[0].path, gone);
        assert_eq!(found[0].title, "Chrono Trigger (U).sfc");

        // The known facts of a game seen before survive its file going away, so
        // the card still names it rather than reverting to a file name.
        cache.entries = vec![GameEntry { path: gone.clone(), ..entry("CT-0001", "CHRONO TRIGGER") }];
        let found = scan(&dir, std::slice::from_ref(&gone), &mut cache).expect("rescan");
        assert_eq!(found[0].title, "CHRONO TRIGGER");
        assert!(found[0].missing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_game_added_from_the_scanned_folder_is_not_listed_twice() {
        let dir = scratch("extra-dup");
        std::fs::create_dir_all(&dir).expect("mkdir");
        // Not a parseable ROM, so the folder pass drops it; what matters is
        // that the extra pass does not then add it back as a second card.
        std::fs::write(dir.join("game.sfc"), b"too small").expect("write");
        let mut cache = Cache::default();
        let found = scan(&dir, &[dir.join("game.sfc")], &mut cache).expect("scan");
        assert!(found.is_empty(), "{found:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Scan of the project's own ROM folder. Ignored by default: it depends on
    /// files that are not part of the repository's source and reads every one
    /// of them. Run with `cargo test -p prisme -- --ignored scan_of_the_real`.
    #[test]
    #[ignore]
    fn scan_of_the_real_rom_folder_reads_every_header() {
        let dir = Path::new("../roms");
        let dir = if dir.is_dir() { dir } else { Path::new("roms") };
        assert!(dir.is_dir(), "ROM folder missing: {}", dir.display());
        let mut cache = Cache::default();
        let entries = scan_dir(dir, &mut cache).expect("scan");
        assert!(!entries.is_empty(), "no game found in {}", dir.display());
        for e in &entries {
            assert!(!e.title.is_empty(), "{:?}", e.path);
            assert!(e.rom_bytes >= 0x8000);
            assert!(matches!(e.mapping.as_str(), "LoROM" | "HiROM"));
            assert!(matches!(e.region.as_str(), "PAL" | "NTSC"));
            assert!(e.id.starts_with(&format!(
                "{}-{:04X}",
                crate::sanitize_file_stem(&e.title),
                e.checksum
            )));
        }
        // Every entry has its own identity (no two games collide on a key).
        let mut ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate game ids");
        // The coprocessors the core detects on this collection.
        let chips: Vec<&str> =
            entries.iter().filter_map(|e| e.coprocessor.as_deref()).collect();
        assert!(chips.contains(&"SuperFX"), "{chips:?}");
        assert!(chips.contains(&"SA-1"), "{chips:?}");
        assert!(chips.contains(&"DSP-1"), "{chips:?}");
        assert!(chips.contains(&"CX4"), "{chips:?}");

        // A second scan reuses every cached entry (nothing changed on disk).
        let again = scan_dir(dir, &mut cache).expect("rescan");
        assert_eq!(again, entries);
    }

    #[test]
    fn search_ignores_case_accents_and_punctuation() {
        // `matches` takes the query already folded, exactly as `arrange` feeds
        // it (one normalization per search, not one per entry).
        let hit = |e: &GameEntry, q: &str| matches(e, &normalize(q));
        let e = entry("SOM-0001", "SECRET OF MANA");
        assert!(hit(&e, ""));
        assert!(hit(&e, "mana"));
        assert!(hit(&e, "SECRET OF"));
        assert!(hit(&e, "secretof"));
        assert!(!hit(&e, "zelda"));
        // The file name is searched too.
        let mut e = entry("X-0001", "BLANK");
        e.path = PathBuf::from("/roms/Pokémon Édition.zip");
        assert!(hit(&e, "pokemon"));
        assert!(hit(&e, "edition"));
        assert_eq!(normalize("Étoile-42 !"), "etoile42");
    }

    #[test]
    fn favorites_are_pinned_ahead_of_every_other_game() {
        let entries = vec![entry("A-0001", "ALPHA"), entry("B-0001", "BRAVO"), entry("C-0001", "CHARLIE")];
        let mut games = BTreeMap::new();
        games.insert("C-0001".to_string(), GameStats { favorite: true, ..Default::default() });
        let order = arrange(&entries, "", SortMode::Title, &games);
        assert_eq!(order.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["C-0001", "A-0001", "B-0001"]);
    }

    #[test]
    fn recent_sort_puts_the_last_played_first_and_never_played_last() {
        let entries = vec![entry("A-0001", "ALPHA"), entry("B-0001", "BRAVO"), entry("C-0001", "CHARLIE")];
        let mut games = BTreeMap::new();
        games.insert("A-0001".to_string(), GameStats { last_played: Some(100), ..Default::default() });
        games.insert("C-0001".to_string(), GameStats { last_played: Some(500), ..Default::default() });
        let order = arrange(&entries, "", SortMode::Recent, &games);
        assert_eq!(order.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["C-0001", "A-0001", "B-0001"]);
        // Alphabetical order is the tie-break for never-played games.
        let order = arrange(&entries, "", SortMode::Title, &BTreeMap::new());
        assert_eq!(order.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["A-0001", "B-0001", "C-0001"]);
    }

    #[test]
    fn arrange_filters_on_the_query() {
        let entries = vec![entry("A-0001", "ALPHA"), entry("B-0001", "BRAVO")];
        let order = arrange(&entries, "brav", SortMode::Title, &BTreeMap::new());
        assert_eq!(order.len(), 1);
        assert_eq!(order[0].id, "B-0001");
        assert!(arrange(&entries, "nothing", SortMode::Title, &BTreeMap::new()).is_empty());
    }

    /// `arrange` runs on every UI frame, so its cost is a frame cost. The
    /// folded sort key is built once per entry (O(n)) instead of twice per
    /// comparison (O(n log n) allocations), which is what this bound catches if
    /// it ever moves back inside the comparator.
    #[test]
    fn arranging_a_large_library_stays_within_the_frame_budget() {
        let entries: Vec<GameEntry> = (0..200)
            .map(|i| entry(&format!("GAME-{i:04}"), &format!("GAME {:03}", (i * 37) % 200)))
            .collect();
        let mut games = BTreeMap::new();
        for (i, e) in entries.iter().enumerate().filter(|(i, _)| i % 10 == 0) {
            games.insert(
                e.id.clone(),
                GameStats { favorite: true, last_played: Some(i as i64), ..Default::default() },
            );
        }
        const ITERATIONS: u32 = 60;
        let start = std::time::Instant::now();
        let mut kept = 0;
        for _ in 0..ITERATIONS {
            kept += arrange(&entries, "game", SortMode::Recent, &games).len();
        }
        let per_frame = start.elapsed() / ITERATIONS;
        assert_eq!(kept, ITERATIONS as usize * entries.len());
        eprintln!("arrange(200): {:.3} ms/frame", per_frame.as_secs_f64() * 1000.0);
        // Debug builds are ~20x slower than `--release` (see the same note on
        // `render::compose_frame_cost_stays_within_frame_budget`); the bound is
        // loose enough for an unoptimized `cargo test` and still an order of
        // magnitude under one 50 Hz frame.
        let bound = std::time::Duration::from_millis(if cfg!(debug_assertions) { 20 } else { 2 });
        assert!(per_frame < bound, "arrange took {per_frame:?} for 200 games");
    }

    #[test]
    fn sort_mode_round_trips_through_the_preference_string() {
        for mode in SortMode::ALL {
            assert_eq!(SortMode::from_pref(mode.as_pref()), mode);
            for lang in Lang::ALL {
                assert!(!mode.label(lang).is_empty());
            }
        }
        assert_eq!(SortMode::from_pref("unknown-from-a-newer-build"), SortMode::Title);
    }

    #[test]
    fn sizes_and_durations_are_formatted_for_the_sheet() {
        assert_eq!(format_size(Lang::Fr, 0), "0 o");
        assert_eq!(format_size(Lang::Fr, 512), "512 o");
        assert_eq!(format_size(Lang::Fr, 8 * 1024), "8 Ko");
        assert_eq!(format_size(Lang::Fr, 2 * 1024 * 1024), "2,0 Mo");
        assert_eq!(format_size(Lang::Fr, 2_621_440), "2,5 Mo");
        assert_eq!(format_sram(Lang::Fr, 0), "Aucune");
        assert_eq!(format_sram(Lang::Fr, 8192), "8 Ko");
        assert_eq!(format_play_time(Lang::Fr, 0), "Jamais joué");
        assert_eq!(format_play_time(Lang::Fr, 30), "moins d'une minute");
        assert_eq!(format_play_time(Lang::Fr, 60), "1 min");
        assert_eq!(format_play_time(Lang::Fr, 59 * 60), "59 min");
        assert_eq!(format_play_time(Lang::Fr, 3600), "1 h 00");
        assert_eq!(format_play_time(Lang::Fr, 2 * 3600 + 5 * 60 + 59), "2 h 05");
        // English keeps the point and the English units; a French decimal
        // comma in an English size line reads as a thousands separator.
        assert_eq!(format_size(Lang::En, 0), "0 B");
        assert_eq!(format_size(Lang::En, 8 * 1024), "8 KB");
        assert_eq!(format_size(Lang::En, 2_621_440), "2.5 MB");
        assert_eq!(format_sram(Lang::En, 0), "None");
        assert_eq!(format_play_time(Lang::En, 0), "Never played");
        assert_eq!(format_play_time(Lang::En, 30), "less than a minute");
        // The numeric shapes are the same in both: they are not prose.
        assert_eq!(format_play_time(Lang::En, 3600), "1 h 00");
        assert_eq!(format_play_time(Lang::En, 59 * 60), "59 min");
    }

    #[test]
    fn a_promoted_screenshot_wins_over_the_generated_thumbnail() {
        let dir = scratch("picture");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let custom = dir.join("promoted.png");
        let generated = dir.join("generated.png");
        let missing = dir.join("gone.png");

        // Nothing on disk yet: placeholder, and generation is still needed.
        assert_eq!(resolve_picture(None, Some(&generated)), None);
        assert_eq!(resolve_picture(Some(&missing), Some(&missing)), None);
        assert_eq!(resolve_picture(None, None), None);

        std::fs::write(&generated, b"g").expect("write");
        assert_eq!(resolve_picture(None, Some(&generated)), Some(generated.clone()));
        // A promoted capture takes precedence…
        std::fs::write(&custom, b"c").expect("write");
        assert_eq!(resolve_picture(Some(&custom), Some(&generated)), Some(custom.clone()));
        // …unless the player deleted it, in which case the generated one is
        // used again rather than showing nothing.
        std::fs::remove_file(&custom).expect("rm");
        assert_eq!(resolve_picture(Some(&custom), Some(&generated)), Some(generated));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_play_clock_keeps_the_sub_second_remainder() {
        let mut clock = PlayClock::default();
        // 20 ms frames: 49 of them are worth no whole second, the 50th is.
        let mut committed = 0;
        for _ in 0..50 {
            committed += clock.add(Duration::from_millis(20));
        }
        assert_eq!(committed, 1);
        for _ in 0..50 {
            committed += clock.add(Duration::from_millis(20));
        }
        assert_eq!(committed, 2, "no time may be lost to rounding");
        // A long stall commits every whole second at once.
        assert_eq!(clock.add(Duration::from_millis(2500)), 2);
        clock.reset();
        assert_eq!(clock.add(Duration::from_millis(600)), 0);
    }

    #[test]
    fn save_states_lists_the_resume_snapshot_and_the_manual_slots() {
        let dir = scratch("states");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let rom = dir.join("game.sfc");
        std::fs::write(&rom, b"rom").expect("write");
        let beside = crate::paths::GamePaths::new(&rom, "GAME-0001", None, None);
        assert!(save_states(&beside).is_empty());

        std::fs::write(beside.state_write(0), b"s0").expect("write");
        std::fs::write(beside.state_write(3), b"s3").expect("write");
        std::fs::write(beside.resume_write(), b"r").expect("write");
        let states = save_states(&beside);
        assert_eq!(states.len(), 3);
        assert_eq!(states[0].slot, None);
        assert_eq!(states[0].label(Lang::Fr), "Reprise");
        assert_eq!(states[0].label(Lang::En), "Resume");
        assert_eq!(states[1].slot, Some(0));
        assert_eq!(states[2].slot, Some(3));
        assert_eq!(states[2].label(Lang::Fr), "Slot 3");
        assert_eq!(states[1].size, 2);
        // No preview picture was written beside any of them: the sheet must
        // still list the states (the preview is optional by design).
        assert!(states.iter().all(|s| s.preview.is_none()));

        // …and one that has a preview reports its path.
        let preview = crate::state::preview_path(&beside.state_write(3));
        std::fs::write(&preview, b"png").expect("write");
        let states = save_states(&beside);
        assert_eq!(states[2].preview.as_deref(), Some(preview.as_path()));
        assert_eq!(states[1].preview, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With a save folder configured, the sheet must list both what that folder
    /// holds and the states still sitting beside the ROM — exactly the files
    /// F9 would load (`paths::read_sidecar`'s fallback).
    #[test]
    fn save_states_list_the_configured_folder_and_the_legacy_files() {
        let root = scratch("states-dir");
        let roms = root.join("roms");
        let saves = root.join("saves");
        std::fs::create_dir_all(&roms).expect("mkdir");
        std::fs::create_dir_all(&saves).expect("mkdir");
        let rom = roms.join("game.sfc");
        std::fs::write(&rom, b"rom").expect("write");
        // Slot 0 only beside the ROM, slot 1 only in the folder (under the
        // game's own id, the name a save written there takes), slot 2 only in
        // the folder under the ROM file's name, as an older build wrote it.
        std::fs::write(roms.join("game.state"), b"old").expect("write");
        std::fs::write(saves.join("GAME-0001.state1"), b"new").expect("write");
        std::fs::write(saves.join("game.state2"), b"legacy").expect("write");

        let paths =
            crate::paths::GamePaths::new(&rom, "GAME-0001", Some(saves.clone()), None);
        let states = save_states(&paths);
        let listed: Vec<(Option<u8>, PathBuf)> =
            states.iter().map(|s| (s.slot, s.path.clone())).collect();
        assert_eq!(
            listed,
            vec![
                (Some(0), roms.join("game.state")),
                (Some(1), saves.join("GAME-0001.state1")),
                (Some(2), saves.join("game.state2")),
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn screenshots_are_matched_by_title_prefix_and_sub_folder() {
        let dir = scratch("shots");
        std::fs::create_dir_all(&dir).expect("mkdir");
        // Exactly the names `App::take_screenshot` produces.
        std::fs::write(dir.join("SECRET_OF_MANA_20260724-213045.png"), b"a").expect("write");
        std::fs::write(dir.join("SECRET_OF_MANA_20260724-213045_2.png"), b"b").expect("write");
        std::fs::write(dir.join("SUPER_MARIOWORLD_20260724-213045.png"), b"c").expect("write");
        std::fs::write(dir.join("SECRET_OF_MANA_notes.txt"), b"d").expect("write");
        // …and the per-game sub-folder layout.
        let sub = dir.join("SECRET_OF_MANA");
        std::fs::create_dir_all(&sub).expect("mkdir");
        std::fs::write(sub.join("anything.png"), b"e").expect("write");

        let shots = screenshots(&dir, "SECRET OF MANA");
        assert_eq!(shots.len(), 3, "{shots:?}");
        assert!(shots.iter().all(|p| {
            let n = p.to_string_lossy();
            n.contains("SECRET_OF_MANA")
        }));
        assert!(screenshots(&dir, "NO SUCH GAME").is_empty());
        assert!(screenshots(&dir.join("absent"), "SECRET OF MANA").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_screenshot_folder_matches_what_the_capture_hotkey_uses() {
        let mut prefs = Prefs::default();
        assert_eq!(
            screenshot_dir(Path::new("/roms/game.sfc"), &prefs),
            PathBuf::from("/roms/Screenshots")
        );
        assert_eq!(screenshot_dir(Path::new("game.sfc"), &prefs), PathBuf::from("Screenshots"));
        prefs.screenshot_dir = Some(PathBuf::from("/shots"));
        assert_eq!(screenshot_dir(Path::new("/roms/game.sfc"), &prefs), PathBuf::from("/shots"));
    }

    #[test]
    fn the_library_folder_falls_back_from_preference_to_roms() {
        let mut prefs = Prefs::default();
        assert_eq!(library_dir(&prefs), PathBuf::from("roms"));
        prefs.last_rom_dir = Some(PathBuf::from("/games"));
        assert_eq!(library_dir(&prefs), PathBuf::from("/games"));
        prefs.library_dir = Some(PathBuf::from("/library"));
        assert_eq!(library_dir(&prefs), PathBuf::from("/library"));
    }

    #[test]
    fn a_timestamp_formats_as_a_local_date() {
        // Only the shape is asserted: the value depends on the host timezone.
        let s = format_date(Lang::Fr, 1_700_000_000);
        assert_eq!(s.len(), 16, "{s}");
        assert_eq!(s.as_bytes()[2], b'/');
        assert_eq!(s.as_bytes()[5], b'/');
        assert_eq!(s.as_bytes()[13], b':');
        // English is ISO, so a date is never read a month out.
        let s = format_date(Lang::En, 1_700_000_000);
        assert_eq!(s.len(), 16, "{s}");
        assert_eq!(s.as_bytes()[4], b'-');
        assert_eq!(s.as_bytes()[7], b'-');
        assert_eq!(s.as_bytes()[13], b':');
    }

    #[test]
    fn a_scan_jumps_ahead_of_the_queued_thumbnails_and_cancels_them() {
        let shared = Shared::new();
        for id in ["A-0001", "B-0001", "C-0001"] {
            shared.push(Job::Thumb { id: id.to_string(), rom: PathBuf::from("/roms/x.sfc") });
        }
        assert_eq!(shared.queued(), (0, 3));
        // Changing folder must not wait for three emulation runs, and the
        // thumbnails of the *old* folder must not be produced at all.
        shared.push(Job::Scan { dir: PathBuf::from("/games"), extra: Vec::new() });
        assert_eq!(shared.queued(), (1, 0), "queued thumbnails belong to the old folder");
        assert_eq!(shared.take(), Some(Job::Scan { dir: PathBuf::from("/games"), extra: Vec::new() }));
        assert_eq!(shared.queued(), (0, 0));
    }

    #[test]
    fn thumbnails_are_served_in_submission_order_once_no_scan_is_waiting() {
        let shared = Shared::new();
        shared.push(Job::Thumb { id: "A".to_string(), rom: PathBuf::from("/a.sfc") });
        shared.push(Job::Thumb { id: "B".to_string(), rom: PathBuf::from("/b.sfc") });
        assert_eq!(
            shared.take(),
            Some(Job::Thumb { id: "A".to_string(), rom: PathBuf::from("/a.sfc") })
        );
        assert_eq!(
            shared.take(),
            Some(Job::Thumb { id: "B".to_string(), rom: PathBuf::from("/b.sfc") })
        );
    }

    #[test]
    fn closing_the_queue_ends_the_thread_without_waiting() {
        let shared = Shared::new();
        shared.push(Job::Scan { dir: PathBuf::from("/games"), extra: Vec::new() });
        shared.close();
        // Would block forever on a live queue; a closed one returns at once,
        // which is what lets the process quit while a scan is still pending.
        assert_eq!(shared.take(), None);
    }

    #[test]
    fn unix_time_conversions_are_symmetric() {
        assert_eq!(unix_secs(UNIX_EPOCH), 0);
        assert_eq!(unix_secs(UNIX_EPOCH + Duration::from_secs(1_700_000_000)), 1_700_000_000);
        assert_eq!(unix_secs(UNIX_EPOCH - Duration::from_secs(5)), -5);
        assert!(now_unix() > 1_700_000_000, "the host clock should be past 2023");
    }
}
