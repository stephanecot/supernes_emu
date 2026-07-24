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
//!   * writes are atomic (temp file in the same directory + `rename`), so a
//!     crash mid-write cannot leave a truncated `prefs.json` behind;
//!   * `persist` is false in headless/CLI runs: `save()` is then a no-op, so an
//!     automated run never rewrites the user's file.

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
    /// Integer window upscale factor, 1..=8.
    pub zoom: u8,
    /// Display filter name: `none` (sharp nearest-neighbour, the default),
    /// `smooth`, `crt`. Unknown names are preserved as written so a file from
    /// a newer build survives a round trip through an older one.
    pub filter: String,
    /// Aspect handling: `pixel-perfect` (1:1) or `tv` (8:7 PAR, ~4:3).
    pub aspect: String,
    /// Directory for `.srm`/`.state` sidecars; `None` = next to the ROM.
    pub save_dir: Option<PathBuf>,
    /// Directory for screenshots; `None` = next to the ROM.
    pub screenshot_dir: Option<PathBuf>,
    /// SNES button name (`A B X Y L R Start Select Up Down Left Right`) ->
    /// physical keyboard key. Entries naming a key winit does not know are
    /// dropped with a warning instead of failing the whole file.
    #[serde(deserialize_with = "de_keymap")]
    pub keymap: BTreeMap<String, KeyCode>,
    /// SNES button name -> gamepad button name (Phase 3; empty = built-in
    /// default mapping).
    pub pad_map: BTreeMap<String, String>,
    pub parental: Parental,
    /// Directory the ROM picker opens in; `None` = `roms/` if present.
    pub last_rom_dir: Option<PathBuf>,
    /// Speed multiplier of the fast-forward key, 2..=8.
    pub fast_forward_factor: u8,
    /// Ask for confirmation before quitting.
    pub confirm_on_quit: bool,
    /// Restore the automatic session save state (`<rom>.resume`) when the same
    /// game is launched again. On by default: the state is written on every
    /// exit path and never touches a manual slot.
    pub resume_on_launch: bool,
    /// Current save-state slot, 0..=9.
    pub save_slot: u8,
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
            zoom: crate::video::WINDOW_SCALE as u8,
            filter: "none".to_string(),
            aspect: "pixel-perfect".to_string(),
            save_dir: None,
            screenshot_dir: None,
            keymap: default_keymap(),
            pad_map: BTreeMap::new(),
            parental: Parental::default(),
            last_rom_dir: None,
            fast_forward_factor: 2,
            confirm_on_quit: true,
            resume_on_launch: true,
            save_slot: 0,
            persist: false,
        }
    }
}

/// Built-in keyboard mapping, taken from `input::DEFAULT_KEYMAP` so the file's
/// defaults and the hard-coded mapping can never drift apart.
pub fn default_keymap() -> BTreeMap<String, KeyCode> {
    input::DEFAULT_KEYMAP.iter().map(|&(name, code)| (name.to_string(), code)).collect()
}

/// Lenient keymap decoding: a key name winit doesn't know (typo in a
/// hand-edited file, key from a newer winit) drops that one entry with a
/// warning instead of invalidating the whole preferences file.
fn de_keymap<'de, D>(d: D) -> Result<BTreeMap<String, KeyCode>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = BTreeMap::<String, String>::deserialize(d)?;
    let mut map = BTreeMap::new();
    for (button, key) in raw {
        // KeyCode's serde impl encodes unit variants as their name ("KeyZ").
        match serde_json::from_value::<KeyCode>(serde_json::Value::String(key.clone())) {
            Ok(code) => {
                map.insert(button, code);
            }
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

    /// Atomic write: serialize to a sibling temp file, then `rename` over the
    /// target (same directory, so the rename stays within one filesystem and
    /// is atomic). A crash before the rename leaves the previous file intact.
    pub fn write_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
            }
        }
        let mut json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("could not serialize preferences: {e}"))?;
        json.push('\n');

        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let tmp = path.with_file_name(format!(".{name}.tmp{}", std::process::id()));
        std::fs::write(&tmp, json.as_bytes())
            .map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("could not replace {}: {e}", path.display()));
        }
        Ok(())
    }

    /// Clamp values a hand-edited file could put out of range. Free-form
    /// strings (`filter`, `aspect`) are left alone: an unknown name is the
    /// caller's business and must survive a round trip.
    fn sanitize(&mut self) {
        self.volume = self.volume.min(100);
        self.zoom = self.zoom.clamp(1, 8);
        self.fast_forward_factor = self.fast_forward_factor.clamp(2, 8);
        self.save_slot = self.save_slot.min(9);
    }
}

/// Full path of the preferences file, or `None` when the OS config directory
/// cannot be determined (no `$HOME`/`%APPDATA%`).
pub fn path() -> Option<PathBuf> {
    config_dir().map(|d| d.join(FILE_NAME))
}

/// `<os config dir>/Prisme` (see module docs).
fn config_dir() -> Option<PathBuf> {
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
        assert_eq!(p.zoom, crate::video::WINDOW_SCALE as u8);
        assert_eq!(p.filter, "none");
        assert_eq!(p.aspect, "pixel-perfect");
        assert_eq!(p.save_dir, None);
        assert_eq!(p.screenshot_dir, None);
        assert_eq!(p.fast_forward_factor, 2);
        assert!(p.confirm_on_quit);
        assert!(p.resume_on_launch);
        assert_eq!(p.save_slot, 0);
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
            assert_eq!(input::keycode_to_button(code), Some(name));
        }
    }

    #[test]
    fn json_round_trip_preserves_every_field() {
        let mut p = Prefs::default();
        p.mute = true;
        p.volume = 42;
        p.show_fps = true;
        p.zoom = 4;
        p.filter = "crt".to_string();
        p.aspect = "tv".to_string();
        p.save_dir = Some(PathBuf::from("/tmp/saves"));
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
    fn unknown_fields_from_a_newer_build_are_ignored() {
        let p = Prefs::from_json("{\"show_fps\": true, \"future_option\": [1, 2]}")
            .expect("parse");
        assert!(p.show_fps);
    }

    #[test]
    fn unknown_key_names_drop_only_their_own_entry() {
        let p = Prefs::from_json("{\"keymap\": {\"A\": \"KeyM\", \"B\": \"NoSuchKey\"}}")
            .expect("parse");
        assert_eq!(p.keymap.get("A"), Some(&KeyCode::KeyM));
        assert_eq!(p.keymap.get("B"), None);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let p = Prefs::from_json(
            "{\"volume\": 250, \"zoom\": 0, \"fast_forward_factor\": 1, \"save_slot\": 99}",
        )
        .expect("parse");
        assert_eq!(p.volume, 100);
        assert_eq!(p.zoom, 1);
        assert_eq!(p.fast_forward_factor, 2);
        assert_eq!(p.save_slot, 9);
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
