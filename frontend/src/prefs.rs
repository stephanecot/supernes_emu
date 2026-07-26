//! Persisted user preferences (JSON), shared by every optional feature of the
//! frontend so none of them has to invent its own storage.
//!
//! Location — the OS config directory, built from environment variables (no
//! extra crate):
//!   * macOS:   `$HOME/Library/Application Support/Prisme/prefs.json`
//!   * Windows: `%APPDATA%\Prisme\prefs.json`
//!   * other:   `$XDG_CONFIG_HOME/Prisme/prefs.json`, else
//!              `$HOME/.config/Prisme/prefs.json`
//!
//! Robustness rules (a preferences file must never cost a play session):
//!   * a missing, unreadable or malformed file falls back to defaults and only
//!     warns on stderr — never panics;
//!   * unknown fields (written by a newer build) are ignored, missing fields
//!     fall back to the value in `Prefs::default()` (container-level
//!     `#[serde(default)]`), so old files stay readable as fields are added;
//!   * writes are atomic (temp file in the same directory + `rename`, via
//!     `crate::atomic`), so a crash mid-write cannot leave a truncated
//!     `prefs.json` behind;
//!   * `persist` is false in headless/CLI runs: `save()` is then a no-op, so an
//!     automated run never rewrites the user's file.
//!
//! Every field is declared and round-trips through the file from the start
//! (see `docs/ROADMAP.md`'s phases) so the format never needs a breaking
//! migration, but not every field is read by the running emulator yet.
//! Fields the frontend actually *acts on* today: `mute`, `volume`, `show_fps`,
//! `fast_forward_factor`, `confirm_on_quit`, `resume_on_launch`, `save_slot`,
//! `save_dir`, `screenshot_dir`, `zoom`, `filter`, `aspect`, `library_dir`,
//! `library_sort`, `library_tab`, `games`, `keymap`, `pad_map`. Every other field is
//! annotated below with the roadmap phase that wires it up (`parental`
//! already carries this note in its own doc comment) — a value stored there
//! today is preserved for when that phase lands, but changing it has no
//! effect yet.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use winit::keyboard::KeyCode;

use crate::input;

/// Directory created under the OS config directory.
pub const APP_DIR: &str = "Prisme";
/// File name inside `APP_DIR`.
pub const FILE_NAME: &str = "prefs.json";

/// Parental controls (Phase 6). Stored here from the start so an older prefs
/// file written before the feature exists stays readable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Parental {
    pub enabled: bool,
    /// Allowed play time per calendar day, in minutes.
    pub daily_limit_minutes: u32,
    /// Parent password, stored hashed only — never in clear text. `None`
    /// means no password has been set yet.
    pub password_hash: Option<String>,
    /// Minutes already played during `day`.
    pub minutes_today: u32,
    /// Local calendar day the counter belongs to, ISO `YYYY-MM-DD`. `None`
    /// means the counter has never been started.
    pub day: Option<String>,
}

impl Default for Parental {
    fn default() -> Self {
        Self {
            enabled: false,
            daily_limit_minutes: 60,
            password_hash: None,
            minutes_today: 0,
            day: None,
        }
    }
}

/// Per-game persisted state of the library (Phase 8): what the player pinned,
/// how long they have played it, when they last did, and the screenshot they
/// promoted as its thumbnail. Keyed by `library::game_id` (cartridge title +
/// header checksum), so it follows the game rather than its file path.
///
/// `play_seconds` is also what the parental controls of Phase 6 will read, so
/// it is accumulated per game from the start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameStats {
    /// Pinned at the head of the library grid.
    pub favorite: bool,
    /// Cumulated play time, in seconds of *emulated running* wall time (the
    /// home screen and pauses do not count).
    pub play_seconds: u64,
    /// Unix timestamp of the last launch, `None` if never launched from this
    /// build. Drives the "recently played" sort order.
    pub last_played: Option<i64>,
    /// Screenshot promoted as this game's thumbnail, replacing the one the
    /// emulator generated. `None` = use the generated one
    /// (`thumbs::thumb_path`).
    pub thumbnail: Option<PathBuf>,
}

/// All persisted options. Fields for features that are not implemented yet are
/// declared now so their file format is fixed and forward/backward compatible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Audio muted (gain forced to 0, APU keeps running).
    pub mute: bool,
    /// Output gain, 0..=100 percent.
    pub volume: u8,
    /// On-screen FPS overlay (`F` hotkey / `View > Show FPS`).
    pub show_fps: bool,
    /// Integer window-size step the player picked, 1..=8, or `None` while they
    /// never picked one — in which case the starting size is resolved from the
    /// monitor at launch (`render::default_zoom`) instead of from a constant.
    /// This picks the *starting* window size (`render::zoomed_dims`, clamped to
    /// fit the monitor) — the window is then freely resizable by dragging,
    /// independent of this value (see `App::apply_resize`/`render::letterbox`);
    /// resizing by hand does not write a new `zoom` back.
    pub zoom: Option<u8>,
    /// Whether `zoom` was picked on the *current* ladder, whose first usable
    /// step is 512x448 and whose native 256x224 entry sits last. Absent from a
    /// file written before the ladder was shifted, which is precisely what
    /// tells an inherited `zoom: 1` — the head and the default of the old
    /// ladder, i.e. the postage-stamp window — from a deliberate
    /// `Taille native`. See `sanitize`.
    pub zoom_chosen: bool,
    /// Display filter name: `none` (sharp nearest-neighbour, the default),
    /// `smooth` (bilinear), `crt` (bilinear + darkened alternating source
    /// scanlines). Unknown names are preserved as written so a file from a
    /// newer build survives a round trip through an older one, but render as
    /// `none` (`render::Filter::from_pref`). Applied by
    /// `render::compose_frame`, independent of `zoom`/`aspect`.
    pub filter: String,
    /// Aspect handling: `pixel-perfect` (1:1, snapped to the largest whole
    /// zoom that fits the current window) or `tv` (8:7 PAR, ~4:3, continuous
    /// scale). Unknown values render as `pixel-perfect`
    /// (`render::Aspect::from_pref`). Applied by `render::letterbox`/
    /// `render::compose_frame`; letterboxed/pillarboxed with black bars
    /// rather than stretched at any window size.
    pub aspect: String,
    /// Directory for the `.srm`/`.state`/`.stateN`/`.resume` sidecars of every
    /// game; `None` = next to the ROM (the original layout). Files there are
    /// named after the *game* (`library::game_id`), not after the ROM file, so
    /// two homonymous dumps cannot share one save. Applied by
    /// `paths::GamePaths`, which a game's session is built with when it is
    /// loaded — so a change here takes effect at the *next* load, never
    /// retargeting the files the running game read from. A save left beside
    /// the ROM keeps being read while the folder holds none for that game
    /// (`paths::read_sidecar`), so configuring a folder never loses one.
    /// The CLI `--save PATH` overrides it for that run's `.srm`; headless runs
    /// never read this file at all.
    pub save_dir: Option<PathBuf>,
    /// Folder `save_dir` held before the player last changed or cleared it.
    /// Written by `video::App::set_save_dir`, and read by
    /// `paths::GamePaths` as a **read-only** fallback: a save left in the
    /// abandoned folder keeps being loaded (and, with no folder configured,
    /// wins over an older file beside the ROM) instead of the session silently
    /// reverting to a frozen copy. Nothing is ever written there.
    pub previous_save_dir: Option<PathBuf>,
    /// Directory for screenshots; `None` = next to the ROM. Applied by
    /// `App::take_screenshot`, unlike the other directory/display fields on
    /// this struct.
    pub screenshot_dir: Option<PathBuf>,
    /// SNES button name (`A B X Y L R Start Select Up Down Left Right`) ->
    /// physical keyboard key. Entries naming a key winit does not know — or a
    /// key the application handles itself (`input::RESERVED_KEYS`), which would
    /// never reach the console — are dropped with a warning instead of failing
    /// the whole file, so the button falls back to its built-in key. Applied by
    /// `input::resolve_key` on every key event of the game screen: a button
    /// named here uses that key, a button the file omits falls back to
    /// `input::DEFAULT_KEYMAP`, and an explicit binding always wins over a
    /// default one. Written by the `Réglages > Entrées` section
    /// (`input::bind_key`), which also settles conflicts.
    #[serde(deserialize_with = "de_keymap")]
    pub keymap: BTreeMap<String, KeyCode>,
    /// SNES button name -> `gilrs` button name (`South`, `LeftTrigger2`…, see
    /// `pad::PAD_BUTTONS`); empty = the built-in `pad::DEFAULT_PAD_MAP`.
    /// Applied by `pad::PadState::joypad`, with the same rule as `keymap`:
    /// an entry replaces the default binding of that button (including both
    /// halves of L/R, which default to a shoulder button *and* a trigger), a
    /// missing entry keeps the default, and a name no `gilrs` version here
    /// knows is ignored in favour of the default rather than leaving the
    /// button dead.
    pub pad_map: BTreeMap<String, String>,
    pub parental: Parental,
    /// Folder the player last picked a ROM in, written by
    /// `video::App::pump_dialogs` after a successful choice. Read only through
    /// `library::library_dir`, which is what both the library scan and the
    /// in-session ROM picker (`video::App::open_rom_dialog`) resolve their
    /// folder with, so browsing and scanning stay on the same folder. The
    /// startup picker of `--info`/`--disasm` runs before the preferences are
    /// read and still starts in `roms/` (documented CLI behavior).
    pub last_rom_dir: Option<PathBuf>,
    /// Speed multiplier of the fast-forward key, 2..=4 (matches the choices
    /// `menu::FF_FACTORS`/`prefs::FAST_FORWARD_FACTORS` offer; a value from
    /// an older file that allowed up to 8 is clamped down by `sanitize`).
    pub fast_forward_factor: u8,
    /// Ask for confirmation before quitting.
    pub confirm_on_quit: bool,
    /// Restore the automatic session save state (`<rom>.resume`) when the same
    /// game is launched again. On by default: the state is written on every
    /// exit path and never touches a manual slot.
    pub resume_on_launch: bool,
    /// Current save-state slot, 0..=9.
    pub save_slot: u8,
    /// Folder the library screen scans for ROMs; `None` falls back to
    /// `last_rom_dir`, then to `roms/` (see `library::library_dir`).
    pub library_dir: Option<PathBuf>,
    /// Let the assistant run (`assistant::Session`). Off until it is asked
    /// for: it starts a process that reasons about the player's game, and
    /// nothing of that shape should switch itself on.
    ///
    /// Stored independently of whether the tool is installed. The two are
    /// different facts — "I want this" and "it is possible here" — and
    /// conflating them would silently forget the choice of someone whose
    /// `claude` is temporarily missing, then leave the feature off once it
    /// came back.
    pub assistant: bool,
    /// Path of the assistant's command-line tool. Starts at this platform's
    /// usual install location (`assistant::default_path`) rather than empty,
    /// so the field shows what is being looked at instead of leaving someone
    /// to guess; emptying it falls back to searching the `PATH`.
    ///
    /// It exists because a windowed application does not inherit a shell's
    /// `PATH`: on macOS the Finder hands over a bare login environment, so a
    /// tool in `~/.local/bin` is invisible to the bundle while working in a
    /// terminal.
    ///
    /// Deliberately the only way to point at it: no list of likely install
    /// directories is searched. Guessing works until it does not, and it hides
    /// the one fact worth knowing when it fails — where the application looked.
    pub assistant_path: String,
    /// Model the assistant runs on, passed straight to the tool's `--model`.
    /// Defaults to `assistant::DEFAULT_MODEL`; empty leaves the tool its own.
    ///
    /// Free text rather than a fixed list: model names change faster than this
    /// emulator ships, and a hardcoded menu would go stale and start refusing
    /// names that work.
    pub assistant_model: String,
    /// Interface language: `fr`, `en`, or anything else — including the
    /// default `system` — to follow the host. Storing the fallback as an
    /// unrecognised string rather than as an absent key keeps the file
    /// self-explanatory to anyone who opens it.
    pub language: String,
    /// Games added one by one, wherever they live. The scanned folder cannot
    /// hold these: it rebuilds its list from the directory, so a game outside
    /// it would appear once and be gone at the next scan. Listed after the
    /// folder's own games, in the order they were added.
    pub extra_roms: Vec<PathBuf>,
    /// Library sort order: `title` or `recent` (`library::SortMode`). Unknown
    /// values are preserved on a round trip and render as `title`, like
    /// `filter`/`aspect`.
    pub library_sort: String,
    /// Tab the home screen opens on: `library`, `favorites` or `recent`
    /// (`ui::tabs::Tab`). Unknown values are preserved on a round trip and
    /// render as `library`, like `library_sort`.
    pub library_tab: String,
    /// Per-game library state, keyed by `library::game_id`. Games never
    /// launched and never pinned have no entry at all, so the file stays small.
    pub games: BTreeMap<String, GameStats>,
    /// Not serialized: false in headless/CLI runs, where `save()` must do
    /// nothing so automated runs never touch the user's file.
    #[serde(skip)]
    persist: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            mute: false,
            volume: 100,
            show_fps: false,
            zoom: None,
            zoom_chosen: false,
            filter: "none".to_string(),
            aspect: "pixel-perfect".to_string(),
            save_dir: None,
            previous_save_dir: None,
            screenshot_dir: None,
            keymap: default_keymap(),
            pad_map: BTreeMap::new(),
            parental: Parental::default(),
            last_rom_dir: None,
            fast_forward_factor: 2,
            confirm_on_quit: true,
            resume_on_launch: true,
            save_slot: 0,
            library_dir: None,
            assistant: false,
            assistant_path: crate::assistant::default_path(),
            assistant_model: crate::assistant::DEFAULT_MODEL.to_string(),
            language: "system".to_string(),
            extra_roms: Vec::new(),
            library_sort: "title".to_string(),
            library_tab: "library".to_string(),
            games: BTreeMap::new(),
            persist: false,
        }
    }
}

/// Fast-forward factors offered to the player, in order: the keyboard's
/// `[`/`]` hotkeys (`App::adjust_fast_forward_factor`) step through this exact
/// list, and it must stay in sync with the macOS menu's `menu::FF_FACTORS`
/// (checked by a cross-referencing test in `menu.rs`). `Prefs::sanitize`
/// clamps `fast_forward_factor` to this range.
pub const FAST_FORWARD_FACTORS: &[u8] = &[2, 3, 4];

/// Built-in keyboard mapping, taken from `input::DEFAULT_KEYMAP` so the file's
/// defaults and the hard-coded mapping can never drift apart.
pub fn default_keymap() -> BTreeMap<String, KeyCode> {
    input::DEFAULT_KEYMAP.iter().map(|&(name, code)| (name.to_string(), code)).collect()
}

/// Lenient keymap decoding: a key name winit doesn't know (typo in a
/// hand-edited file, key from a newer winit) drops that one entry with a
/// warning instead of invalidating the whole preferences file.
///
/// A key the application acts on itself (`input::RESERVED_KEYS`: Tab, F1-F12,
/// the digits…) is dropped the same way. `video::App::handle_key` dispatches
/// those before the emulated pad and returns, so such a binding could never
/// press anything: the capture already refuses them, and this is the same
/// refusal applied to a file written by hand or by another build.
fn de_keymap<'de, D>(d: D) -> Result<BTreeMap<String, KeyCode>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = BTreeMap::<String, String>::deserialize(d)?;
    let mut map = BTreeMap::new();
    for (button, key) in raw {
        // KeyCode's serde impl encodes unit variants as their name ("KeyZ").
        match serde_json::from_value::<KeyCode>(serde_json::Value::String(key.clone())) {
            Ok(code) => match input::reserved_for(code) {
                // Diagnostic output stays English (see `i18n`), so the
                // shortcut is named in English whatever the interface speaks.
                Some(what) => eprintln!(
                    "prefs: key {key:?} for button {button:?} is an application shortcut ({}); \
                     ignored, the button keeps its default key",
                    what.text(crate::i18n::Lang::En)
                ),
                None => {
                    map.insert(button, code);
                }
            },
            Err(_) => eprintln!("prefs: unknown key name {key:?} for button {button:?}; ignored"),
        }
    }
    Ok(map)
}

impl Prefs {
    /// Load the preferences file, or defaults if it is missing/unreadable/
    /// malformed. `persist` must be false for headless runs (see `save`).
    pub fn load(persist: bool) -> Self {
        let mut prefs = match path() {
            Some(p) => Self::read_from(&p),
            None => {
                eprintln!("prefs: no config directory available; using defaults (not persisted)");
                Self::default()
            }
        };
        prefs.persist = persist;
        prefs
    }

    /// Write the preferences back to the config file. No-op when `persist` is
    /// false. Called after every option change (so a crash cannot lose it) and
    /// once more on exit; failures only warn.
    pub fn save(&self) {
        if !self.persist {
            return;
        }
        let Some(p) = path() else { return };
        if let Err(e) = self.write_to(&p) {
            eprintln!("prefs: {e}");
        }
    }

    /// Parse `path`, falling back to defaults on any error.
    pub fn read_from(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Self::default();
            }
            Err(e) => {
                eprintln!("prefs: could not read {}: {e}; using defaults", path.display());
                return Self::default();
            }
        };
        Self::from_json(&text).unwrap_or_else(|e| {
            eprintln!("prefs: ignoring malformed {}: {e}; using defaults", path.display());
            Self::default()
        })
    }

    /// Parse JSON text; out-of-range values are clamped rather than rejected.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let mut prefs: Self = serde_json::from_str(text).map_err(|e| e.to_string())?;
        prefs.sanitize();
        Ok(prefs)
    }

    /// Atomic write (`crate::atomic::write`: sibling temp file + `rename`,
    /// same directory so the rename stays within one filesystem). A crash
    /// before the rename leaves the previous file intact.
    pub fn write_to(&self, path: &Path) -> Result<(), String> {
        let mut json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("could not serialize preferences: {e}"))?;
        json.push('\n');
        crate::atomic::write(path, json.as_bytes())
    }

    /// Clamp values a hand-edited file could put out of range. Free-form
    /// strings (`filter`, `aspect`) are left alone: an unknown name is the
    /// caller's business and must survive a round trip.
    ///
    /// `zoom` also carries a migration. A file written before the window-size
    /// ladder was shifted holds a bare number and no `zoom_chosen`; `1` there
    /// is the head *and* the default of that old ladder, so it says "nothing
    /// was ever chosen" rather than "give me 256x224", and re-applying it would
    /// reopen the postage-stamp window that made the ladder be shifted in the
    /// first place. It is dropped back to `None`, which resolves to
    /// `render::default_zoom`. Anything above it was a size the player could
    /// only get by asking for it, so it is kept.
    fn sanitize(&mut self) {
        self.volume = self.volume.min(100);
        self.zoom = match self.zoom {
            Some(zoom) if self.zoom_chosen => Some(zoom.clamp(1, 8)),
            Some(zoom) if zoom > 1 => Some(zoom.min(8)),
            _ => None,
        };
        self.zoom_chosen = self.zoom.is_some();
        self.fast_forward_factor = self.fast_forward_factor.clamp(2, 4);
        self.save_slot = self.save_slot.min(9);
        // A path listed twice would draw the same game twice; hand-edited files
        // and older writes can both contain one.
        let mut seen = Vec::with_capacity(self.extra_roms.len());
        self.extra_roms.retain(|p| {
            if seen.contains(p) {
                return false;
            }
            seen.push(p.clone());
            true
        });
    }

    /// The tool path the player named, or `None` to look on the `PATH`. Blank
    /// and whitespace-only both mean "not set" — someone who clears the field
    /// means the same thing as someone who never filled it.
    pub fn assistant_tool(&self) -> Option<&Path> {
        let trimmed = self.assistant_path.trim();
        (!trimmed.is_empty()).then(|| Path::new(trimmed))
    }

    /// The model to run on, or `None` to leave the tool its own default.
    pub fn assistant_model(&self) -> Option<&str> {
        let trimmed = self.assistant_model.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }

    /// Language the interface is drawn in: the stored choice, or the host's
    /// when none was made. Resolved on every call rather than cached, so
    /// changing it applies to the very next frame — the interface is rebuilt
    /// from scratch each time anyway, and making the player restart for a label
    /// swap would be theatre.
    pub fn lang(&self) -> crate::i18n::Lang {
        crate::i18n::Lang::from_pref(&self.language).unwrap_or_else(crate::i18n::system_lang)
    }

    /// Remember a game added by hand. Returns whether the list changed: adding
    /// the same file twice is a no-op, not a second card.
    pub fn add_extra_rom(&mut self, path: &Path) -> bool {
        if self.extra_roms.iter().any(|p| p == path) {
            return false;
        }
        self.extra_roms.push(path.to_path_buf());
        true
    }

    /// Drop a game added by hand. The file itself is never touched — forgetting
    /// a game must not be a way to delete it.
    pub fn forget_extra_rom(&mut self, path: &Path) -> bool {
        let before = self.extra_roms.len();
        self.extra_roms.retain(|p| p != path);
        self.extra_roms.len() != before
    }
}

/// Full path of the preferences file, or `None` when the OS config directory
/// cannot be determined (no `$HOME`/`%APPDATA%`).
pub fn path() -> Option<PathBuf> {
    config_dir().map(|d| d.join(FILE_NAME))
}

/// `<os config dir>/Prisme/<name>`, the same directory `prefs.json` lives in.
/// Used by the library's metadata cache (`library.json`) and by the generated
/// thumbnails (`Thumbnails/`), which are derived data, never player data: both
/// can be deleted at any time and are simply rebuilt.
pub fn data_path(name: &str) -> Option<PathBuf> {
    config_dir().map(|d| d.join(name))
}

/// `<os config dir>/Prisme` (see module docs).
pub fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library").join("Application Support").join(APP_DIR))
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var_os("APPDATA")?;
        Some(PathBuf::from(appdata).join(APP_DIR))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                return Some(PathBuf::from(xdg).join(APP_DIR));
            }
        }
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".config").join(APP_DIR))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch path per test, cleaned up by the caller.
    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("prisme_prefs_{}_{}", std::process::id(), tag))
    }

    #[test]
    fn defaults_are_the_documented_ones() {
        let p = Prefs::default();
        assert!(!p.mute);
        assert_eq!(p.volume, 100);
        assert!(!p.show_fps);
        // No window size until the player picks one: it is resolved from the
        // monitor at launch (`render::default_zoom`), not from a constant.
        assert_eq!(p.zoom, None);
        assert!(!p.zoom_chosen);
        assert_eq!(p.filter, "none");
        assert_eq!(p.aspect, "pixel-perfect");
        assert_eq!(p.save_dir, None);
        assert_eq!(p.previous_save_dir, None);
        assert_eq!(p.screenshot_dir, None);
        assert_eq!(p.fast_forward_factor, 2);
        assert!(p.confirm_on_quit);
        assert!(p.resume_on_launch);
        assert_eq!(p.save_slot, 0);
        assert_eq!(p.library_dir, None);
        assert_eq!(p.library_sort, "title");
        assert!(p.games.is_empty());
        assert!(!p.persist, "loaded prefs must opt into writing explicitly");
        assert_eq!(p.parental, Parental::default());
        assert!(!p.parental.enabled);
        assert_eq!(p.parental.password_hash, None);
    }

    #[test]
    fn default_keymap_matches_the_hard_coded_input_mapping() {
        let map = default_keymap();
        assert_eq!(map.len(), input::DEFAULT_KEYMAP.len());
        for &(name, code) in input::DEFAULT_KEYMAP {
            assert_eq!(map.get(name), Some(&code), "button {name}");
            // The stored defaults and the built-in fallback must resolve the
            // same way, whether the file lists them or not.
            assert_eq!(input::resolve_key(&map, code), Some(name));
            assert_eq!(input::resolve_key(&BTreeMap::new(), code), Some(name));
        }
    }

    /// A remapped keyboard and controller must survive the file and still
    /// resolve afterwards: a binding that only round-trips as text would be a
    /// setting shown but not applied.
    #[test]
    fn a_remapped_keyboard_and_pad_round_trip_and_still_resolve() {
        let mut p = Prefs::default();
        p.keymap.insert("A".to_string(), KeyCode::Space);
        p.keymap.insert("B".to_string(), KeyCode::KeyX);
        p.pad_map.insert("A".to_string(), "North".to_string());
        p.pad_map.insert("L".to_string(), "LeftTrigger2".to_string());
        let json = serde_json::to_string_pretty(&p).expect("serialize");
        let back = Prefs::from_json(&json).expect("parse");
        assert_eq!(back, p);
        assert_eq!(input::resolve_key(&back.keymap, KeyCode::Space), Some("A"));
        assert_eq!(input::resolve_key(&back.keymap, KeyCode::KeyX), Some("B"));
        // The key A used to hold is now free, not shared with B.
        assert_eq!(input::resolve_key(&back.keymap, KeyCode::KeyZ), None);
        assert_eq!(
            crate::pad::resolve_button(&back.pad_map, crate::pad::Button::North),
            Some("A")
        );
        assert_eq!(
            crate::pad::current_buttons(&back.pad_map, "L"),
            vec![crate::pad::Button::LeftTrigger2]
        );
    }

    /// The save folder must survive the file *and* still drive path
    /// resolution afterwards — a folder that only round-trips as text would be
    /// a setting shown but not applied.
    #[test]
    fn a_configured_save_folder_round_trips_and_still_resolves() {
        let mut p = Prefs::default();
        p.save_dir = Some(PathBuf::from("/saves"));
        let json = serde_json::to_string_pretty(&p).expect("serialize");
        let back = Prefs::from_json(&json).expect("parse");
        assert_eq!(back.save_dir, Some(PathBuf::from("/saves")));
        let paths = crate::paths::GamePaths::new(
            Path::new("/roms/game.sfc"),
            "GAME-0001",
            back.save_dir.clone(),
            None,
        );
        // Named after the game, not after the ROM file (see `paths`).
        assert_eq!(paths.srm_write(), PathBuf::from("/saves/GAME-0001.srm"));
        assert_eq!(paths.state_write(1), PathBuf::from("/saves/GAME-0001.state1"));
        assert_eq!(paths.resume_write(), PathBuf::from("/saves/GAME-0001.resume"));
        // Cleared: back beside the ROM, under the ROM file's own name.
        let paths =
            crate::paths::GamePaths::new(Path::new("/roms/game.sfc"), "GAME-0001", None, None);
        assert_eq!(paths.srm_write(), PathBuf::from("/roms/game.srm"));
    }

    #[test]
    fn json_round_trip_preserves_every_field() {
        let mut p = Prefs::default();
        p.mute = true;
        p.volume = 42;
        p.show_fps = true;
        p.zoom = Some(4);
        p.zoom_chosen = true;
        p.filter = "crt".to_string();
        p.aspect = "tv".to_string();
        p.save_dir = Some(PathBuf::from("/tmp/saves"));
        p.previous_save_dir = Some(PathBuf::from("/tmp/old-saves"));
        p.screenshot_dir = Some(PathBuf::from("/tmp/shots"));
        p.keymap.insert("A".to_string(), KeyCode::Space);
        p.pad_map.insert("A".to_string(), "South".to_string());
        p.parental.enabled = true;
        p.parental.daily_limit_minutes = 90;
        p.parental.password_hash = Some("deadbeef".to_string());
        p.parental.minutes_today = 12;
        p.parental.day = Some("2026-07-24".to_string());
        p.last_rom_dir = Some(PathBuf::from("/roms"));
        p.fast_forward_factor = 4;
        p.confirm_on_quit = false;
        p.resume_on_launch = false;
        p.save_slot = 7;
        p.library_dir = Some(PathBuf::from("/library"));
        p.library_sort = "recent".to_string();
        p.games.insert(
            "SECRET_OF_MANA-754F".to_string(),
            GameStats {
                favorite: true,
                play_seconds: 4321,
                last_played: Some(1_700_000_000),
                thumbnail: Some(PathBuf::from("/shots/som.png")),
            },
        );

        let json = serde_json::to_string_pretty(&p).expect("serialize");
        let back = Prefs::from_json(&json).expect("parse");
        assert_eq!(back, p);
        // Keys are written as winit variant names, not numbers.
        assert!(json.contains("\"Space\""), "{json}");
    }

    #[test]
    fn atomic_write_then_read_round_trips_through_a_file() {
        let path = scratch("atomic").join("prefs.json");
        let mut p = Prefs::default();
        p.show_fps = true;
        p.volume = 30;
        p.write_to(&path).expect("write");
        let back = Prefs::read_from(&path);
        assert_eq!(back, p);

        // Overwriting an existing file must succeed and leave no temp file.
        let mut p2 = p.clone();
        p2.volume = 55;
        p2.write_to(&path).expect("rewrite");
        assert_eq!(Prefs::read_from(&path).volume, 55);
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != FILE_NAME)
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
        let _ = std::fs::remove_dir_all(scratch("atomic"));
    }

    #[test]
    fn missing_file_yields_defaults_without_creating_it() {
        let path = scratch("absent").join("prefs.json");
        assert_eq!(Prefs::read_from(&path), Prefs::default());
        assert!(!path.exists());
    }

    #[test]
    fn corrupt_json_falls_back_to_defaults() {
        for text in ["{ not json at all", "", "[1,2,3]", "null", "{\"volume\": \"loud\"}"] {
            assert!(Prefs::from_json(text).is_err(), "expected {text:?} to be rejected");
        }
        let path = scratch("corrupt");
        std::fs::write(&path, b"{ truncated").expect("write");
        assert_eq!(Prefs::read_from(&path), Prefs::default());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn partial_json_keeps_defaults_for_missing_fields() {
        let p = Prefs::from_json("{\"show_fps\": true}").expect("parse");
        assert!(p.show_fps);
        assert_eq!(p.volume, Prefs::default().volume);
        assert_eq!(p.filter, Prefs::default().filter);
        assert_eq!(p.keymap, default_keymap());
        assert_eq!(p.parental, Parental::default());

        // A nested object may be partial too.
        let p = Prefs::from_json("{\"parental\": {\"minutes_today\": 5}}").expect("parse");
        assert_eq!(p.parental.minutes_today, 5);
        assert_eq!(p.parental.daily_limit_minutes, Parental::default().daily_limit_minutes);

        // `{}` is a valid, fully-default file.
        assert_eq!(Prefs::from_json("{}").expect("parse"), Prefs::default());
    }

    #[test]
    fn per_game_library_state_survives_a_partial_file() {
        // A file written before the library existed, and one written by hand
        // with only part of a game's state: both must read back with the
        // documented defaults for what they don't say.
        let p = Prefs::from_json("{\"games\": {\"A-0001\": {\"favorite\": true}}}").expect("parse");
        let stats = &p.games["A-0001"];
        assert!(stats.favorite);
        assert_eq!(stats.play_seconds, 0);
        assert_eq!(stats.last_played, None);
        assert_eq!(stats.thumbnail, None);
        // An unknown sort name is preserved verbatim (same rule as `filter`).
        let p = Prefs::from_json("{\"library_sort\": \"by-colour\"}").expect("parse");
        assert_eq!(p.library_sort, "by-colour");
    }

    #[test]
    fn unknown_fields_from_a_newer_build_are_ignored() {
        let p = Prefs::from_json("{\"show_fps\": true, \"future_option\": [1, 2]}")
            .expect("parse");
        assert!(p.show_fps);
    }

    #[test]
    fn unknown_key_names_drop_only_their_own_entry() {
        let p = Prefs::from_json("{\"keymap\": {\"A\": \"Space\", \"B\": \"NoSuchKey\"}}")
            .expect("parse");
        assert_eq!(p.keymap.get("A"), Some(&KeyCode::Space));
        assert_eq!(p.keymap.get("B"), None);
    }

    #[test]
    fn a_stored_language_wins_and_anything_else_follows_the_host() {
        let mut p = Prefs::default();
        assert_eq!(p.language, "system", "no choice is the default, not French");

        p.language = "en".to_string();
        assert_eq!(p.lang(), crate::i18n::Lang::En);
        p.language = "fr".to_string();
        assert_eq!(p.lang(), crate::i18n::Lang::Fr);

        // A value nobody wrote on purpose — a hand-edit, a file from a future
        // build — must fall back to the host rather than blank the interface.
        for value in ["system", "", "klingon"] {
            p.language = value.to_string();
            assert_eq!(p.lang(), crate::i18n::system_lang(), "{value}");
        }
    }

    #[test]
    fn a_game_is_never_added_to_the_library_twice() {
        let mut p = Prefs::default();
        let rom = PathBuf::from("/Volumes/Sauvegardes/Chrono Trigger (U).sfc");
        assert!(p.add_extra_rom(&rom));
        assert!(!p.add_extra_rom(&rom), "adding the same file again must change nothing");
        assert_eq!(p.extra_roms, vec![rom.clone()]);

        assert!(p.forget_extra_rom(&rom));
        assert!(!p.forget_extra_rom(&rom));
        assert!(p.extra_roms.is_empty());
    }

    #[test]
    fn a_duplicate_in_the_file_draws_one_card_not_two() {
        // Hand-edited files and older writes can both carry a repeat.
        let p = Prefs::from_json(
            "{\"extra_roms\": [\"/jeux/a.sfc\", \"/jeux/b.sfc\", \"/jeux/a.sfc\"]}",
        )
        .expect("parse");
        assert_eq!(p.extra_roms, vec![PathBuf::from("/jeux/a.sfc"), PathBuf::from("/jeux/b.sfc")]);
    }

    /// A key the application handles itself would leave the button dead: the
    /// entry is dropped on read, so the button falls back to its built-in key
    /// and the player can still use it.
    #[test]
    fn a_binding_on_an_application_shortcut_is_dropped_on_read() {
        let p = Prefs::from_json(
            "{\"keymap\": {\"A\": \"F11\", \"B\": \"Tab\", \"X\": \"Digit3\", \"Y\": \"Space\"}}",
        )
        .expect("parse");
        assert_eq!(p.keymap.get("A"), None);
        assert_eq!(p.keymap.get("B"), None);
        assert_eq!(p.keymap.get("X"), None);
        assert_eq!(p.keymap.get("Y"), Some(&KeyCode::Space));
        // Dropped means "back to the built-in key", not "dead".
        assert_eq!(input::resolve_key(&p.keymap, KeyCode::KeyX), Some("A"));
        assert_eq!(input::resolve_key(&p.keymap, KeyCode::KeyZ), Some("B"));
        assert_eq!(input::resolve_key(&p.keymap, KeyCode::KeyS), Some("X"));
        // The built-in mapping itself must survive the filter untouched.
        let json = serde_json::to_string(&Prefs::default()).expect("serialize");
        assert_eq!(Prefs::from_json(&json).expect("parse").keymap, default_keymap());
    }

    /// The abandoned save folder is remembered so a save left there is still
    /// found; it must survive the file like any other path.
    #[test]
    fn the_previous_save_folder_round_trips_and_feeds_the_read_fallback() {
        let mut p = Prefs::default();
        p.save_dir = None;
        p.previous_save_dir = Some(PathBuf::from("/old-saves"));
        let json = serde_json::to_string_pretty(&p).expect("serialize");
        let back = Prefs::from_json(&json).expect("parse");
        assert_eq!(back.previous_save_dir, Some(PathBuf::from("/old-saves")));
        // It is a read fallback only: writes stay beside the ROM.
        let paths = crate::paths::GamePaths::new(
            Path::new("/roms/game.sfc"),
            "GAME-0001",
            back.save_dir.clone(),
            None,
        )
        .with_previous_dir(back.previous_save_dir.clone());
        assert_eq!(paths.srm_write(), PathBuf::from("/roms/game.srm"));
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let p = Prefs::from_json(
            "{\"volume\": 250, \"zoom\": 0, \"fast_forward_factor\": 1, \"save_slot\": 99}",
        )
        .expect("parse");
        assert_eq!(p.volume, 100);
        // A zoom of 0 is no size at all: back to "never chosen".
        assert_eq!(p.zoom, None);
        assert_eq!(p.fast_forward_factor, 2);
        assert_eq!(p.save_slot, 9);
        // …and one above the ladder is kept as written, since the window is
        // freely resizable anyway.
        let p = Prefs::from_json("{\"zoom\": 12, \"zoom_chosen\": true}").expect("parse");
        assert_eq!(p.zoom, Some(8));

        // A file written by an older build (fast_forward_factor allowed up to
        // 8, and the macOS menu only ever offered 2/3/4 — review point G) is
        // clamped down to the now-aligned 2..=4 range instead of leaving a
        // value the menu's radio group can't represent.
        let p = Prefs::from_json("{\"fast_forward_factor\": 8}").expect("parse");
        assert_eq!(p.fast_forward_factor, 4);
    }

    /// The migration the shifted ladder needs. A `zoom` written before it was
    /// shifted carries no `zoom_chosen`, and `1` there is the head *and* the
    /// default of the old ladder — the postage-stamp window the player
    /// complained about. It must be read as "never chosen" so the adaptive
    /// default applies; a deliberate `Taille native` picked on the current
    /// ladder carries the flag and must survive untouched.
    #[test]
    fn an_inherited_native_zoom_is_read_as_never_chosen() {
        // The file the person who reported the problem actually has.
        let p = Prefs::from_json("{\"zoom\": 1}").expect("parse");
        assert_eq!(p.zoom, None, "an inherited zoom of 1 must not size the window");
        assert!(!p.zoom_chosen);

        // The same value, deliberately picked on the current ladder.
        let p = Prefs::from_json("{\"zoom\": 1, \"zoom_chosen\": true}").expect("parse");
        assert_eq!(p.zoom, Some(1), "a chosen native size must be honoured");
        assert!(p.zoom_chosen);

        // A legacy file naming a size only a click could produce is kept: the
        // migration is about the unusable step, not about every old value.
        for zoom in [2u8, 3, 4] {
            let p = Prefs::from_json(&format!("{{\"zoom\": {zoom}}}")).expect("parse");
            assert_eq!(p.zoom, Some(zoom), "a legacy zoom of {zoom} is usable");
            assert!(p.zoom_chosen);
        }

        // A file that never mentioned the field at all, and one written by this
        // build before any choice was made.
        for json in ["{}", "{\"zoom\": null}", "{\"zoom_chosen\": true}"] {
            let p = Prefs::from_json(json).expect("parse");
            assert_eq!(p.zoom, None, "{json}");
            assert!(!p.zoom_chosen, "{json}");
        }

        // Every step of the ladder round-trips once it has been chosen.
        for &(zoom, _) in crate::ui::settings::ZOOM_CHOICES {
            let mut p = Prefs::default();
            p.zoom = Some(zoom);
            p.zoom_chosen = true;
            let back = Prefs::from_json(&serde_json::to_string(&p).expect("serialize"))
                .expect("parse");
            assert_eq!(back.zoom, Some(zoom));
            assert!(back.zoom_chosen);
        }
    }

    #[test]
    fn fast_forward_factors_constant_matches_the_clamp_range() {
        assert_eq!(FAST_FORWARD_FACTORS, &[2, 3, 4]);
        let mut p = Prefs::default();
        for &f in FAST_FORWARD_FACTORS {
            p.fast_forward_factor = f;
            p.sanitize();
            assert_eq!(p.fast_forward_factor, f, "sanitize must not touch an in-range factor");
        }
    }

    #[test]
    fn save_is_a_no_op_without_persist() {
        // Headless runs must never rewrite the user's file; `persist` is only
        // set by `load(true)`, which the windowed path uses.
        let p = Prefs::default();
        assert!(!p.persist);
        p.save(); // must not touch the real config file
        let loaded = Prefs::load(false);
        assert!(!loaded.persist);
    }

    #[test]
    fn config_path_is_under_the_os_config_dir() {
        let Some(p) = path() else { return }; // no HOME in this environment
        assert!(p.ends_with(PathBuf::from(APP_DIR).join(FILE_NAME)), "{}", p.display());
        #[cfg(target_os = "macos")]
        assert!(
            p.to_string_lossy().contains("Library/Application Support/Prisme/prefs.json"),
            "{}",
            p.display()
        );
    }
}
