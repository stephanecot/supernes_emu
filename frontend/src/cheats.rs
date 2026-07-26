//! Cheats found by an agent, stored per game.
//!
//! There is no code format here — no Game Genie, no decoder, no database. A
//! cheat is the *result* of a memory search an external agent ran through the
//! `--agent` channel (`docs/CHEATS.md`): one bus address, the bytes to put
//! there, and how long they should stay.
//!
//! **Freeze or once.** A value written a single time is taken straight back by
//! the game's own logic — set the lives counter to 99 and the next death
//! writes 98 over it. `Kind::Freeze` therefore rewrites the bytes after *every*
//! emulated frame, which is what "infinite lives" actually means; `Kind::Once`
//! is the other half (refill a bar, hand over an item) and fires a single time.
//!
//! **Why a sidecar and not `prefs.json`.** Two reasons, both load-bearing:
//!   * a headless run must never write the player's preferences
//!     (`Prefs::load`'s `persist` flag), yet `cheat-add` on the agent channel
//!     is exactly a headless run that has to persist something;
//!   * a file that sits beside the save is portable and inspectable — it moves
//!     with the game, and a human can read it without the application.
//!
//! It follows `GamePaths` like every other sidecar, so a configured save folder
//! is honoured and an older file left beside the ROM is still read.
//!
//! **Robustness**, same rules as the preferences: a missing, unreadable or
//! malformed file yields an empty list and a warning on stderr rather than
//! costing a play session; one unusable entry (bad address, bad hex) is dropped
//! on its own instead of invalidating the file; writes are atomic.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use snes_core::Snes;

use crate::paths::GamePaths;

/// Extension of the per-game sidecar: `<game>.cheats.json`.
pub const CHEATS_EXT: &str = "cheats.json";

/// Longest payload one cheat may write. A cheat patches a counter or a flag;
/// anything longer is a memory editor, which is not what this is.
pub const MAX_BYTES: usize = 64;

/// How long a cheat's bytes stay in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Rewritten after every emulated frame, so the game cannot take it back.
    Freeze,
    /// Written a single time, then left alone.
    Once,
}

impl Kind {
    /// The spelling used by the file and by the agent channel.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Freeze => "freeze",
            Kind::Once => "once",
        }
    }

    pub fn parse(s: &str) -> Result<Kind, String> {
        match s.trim() {
            "freeze" => Ok(Kind::Freeze),
            "once" => Ok(Kind::Once),
            other => Err(format!("\"kind\" must be \"freeze\" or \"once\", got {other:?}")),
        }
    }
}

/// One cheat, as the rest of the application uses it: the address already
/// parsed, the payload already decoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Wire", into = "Wire")]
pub struct Cheat {
    /// What the player sees. Also the cheat's identity: `cheat-add` under an
    /// existing name replaces that cheat rather than adding a second one, which
    /// is what a re-run of the search wants.
    pub name: String,
    /// Bus address, 24 bits (`7E:0DBE`).
    pub addr: u32,
    pub bytes: Vec<u8>,
    pub kind: Kind,
    pub enabled: bool,
}

/// The cheat as it is written down: an address in the spelling the rest of the
/// CLI uses, and a hex payload. Text on both sides so the file stays readable
/// by a human and by an agent that has just computed an address.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Wire {
    name: String,
    addr: String,
    hex: String,
    kind: Kind,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

impl TryFrom<Wire> for Cheat {
    type Error = String;

    fn try_from(w: Wire) -> Result<Cheat, String> {
        Cheat::new(w.name, &w.addr, &w.hex, w.kind, w.enabled)
    }
}

impl From<Cheat> for Wire {
    fn from(c: Cheat) -> Wire {
        Wire {
            name: c.name,
            addr: format_addr(c.addr),
            hex: to_hex(&c.bytes),
            kind: c.kind,
            enabled: c.enabled,
        }
    }
}

impl Cheat {
    /// Build one from the text an agent (or the file) provides, validating
    /// everything before it can ever reach the bus.
    pub fn new(
        name: String,
        addr: &str,
        hex: &str,
        kind: Kind,
        enabled: bool,
    ) -> Result<Cheat, String> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err("\"name\" must not be empty".into());
        }
        let addr = crate::agent::parse_addr(addr)?;
        let bytes = parse_hex(hex)?;
        if bytes.is_empty() || bytes.len() > MAX_BYTES {
            return Err(format!(
                "a cheat writes 1..={MAX_BYTES} bytes, got {}",
                bytes.len()
            ));
        }
        // The same refusal the agent channel applies to `write-mem`: rewriting
        // an I/O register behind the game's back every frame would latch
        // counters and move the WRAM port, which is not a cheat but a crash.
        for i in 0..bytes.len() as u32 {
            crate::agent::check_addr(step_addr(addr, i))?;
        }
        Ok(Cheat { name, addr, bytes, kind, enabled })
    }

    /// `7E:0DBE`, the spelling `--watch` and the agent channel use.
    pub fn addr_text(&self) -> String {
        format_addr(self.addr)
    }

    pub fn hex(&self) -> String {
        to_hex(&self.bytes)
    }

    /// Put the bytes on the bus. `write_no_tick` is the same door the agent's
    /// `write-mem` uses: it changes memory without running a console cycle, so
    /// applying a cheat cannot perturb the emulation's timing.
    fn write(&self, snes: &mut Snes) {
        for (i, b) in self.bytes.iter().enumerate() {
            snes.bus.write_no_tick(step_addr(self.addr, i as u32), *b);
        }
    }
}

/// Every cheat of one game, plus where the list came from.
#[derive(Debug, Clone, Default)]
pub struct Cheats {
    list: Vec<Cheat>,
    /// Names of the `once` cheats already written this session. Cleared
    /// whenever the list changes, so re-enabling a cheat fires it again.
    fired: HashSet<String>,
}

impl Cheats {
    /// Read this game's sidecar, following `GamePaths`' read order (configured
    /// folder, then the legacy names, then beside the ROM).
    pub fn load(paths: &GamePaths) -> Cheats {
        Cheats::read_from(&paths.cheats_read())
    }

    /// Read one file. A missing one is an empty list, not an error: most games
    /// have no cheats and must not print anything.
    pub fn read_from(path: &Path) -> Cheats {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Cheats::default(),
            Err(e) => {
                eprintln!("cheats: could not read {}: {e}; ignoring", path.display());
                return Cheats::default();
            }
        };
        match Cheats::from_json(&text) {
            Ok(list) => Cheats { list, fired: HashSet::new() },
            Err(e) => {
                eprintln!("cheats: ignoring malformed {}: {e}", path.display());
                Cheats::default()
            }
        }
    }

    /// Parse the file's text. Entries that cannot be used are dropped one by
    /// one with a warning — a single bad address must not disable the cheats
    /// that do work.
    pub fn from_json(text: &str) -> Result<Vec<Cheat>, String> {
        #[derive(Deserialize)]
        #[serde(default)]
        struct File {
            cheats: Vec<serde_json::Value>,
        }
        impl Default for File {
            fn default() -> File {
                File { cheats: Vec::new() }
            }
        }
        let file: File = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for raw in file.cheats {
            match serde_json::from_value::<Cheat>(raw.clone()) {
                Ok(cheat) if seen.insert(cheat.name.clone()) => out.push(cheat),
                Ok(cheat) => {
                    eprintln!("cheats: {:?} listed twice; keeping the first", cheat.name)
                }
                Err(e) => eprintln!("cheats: ignoring unusable entry {raw}: {e}"),
            }
        }
        Ok(out)
    }

    /// Write the list where this game's sidecars go. Atomic, like every other
    /// file this frontend persists: a crash mid-write cannot truncate it.
    pub fn save(&self, paths: &GamePaths) -> Result<(), String> {
        self.write_to(&paths.cheats_write())
    }

    pub fn write_to(&self, path: &Path) -> Result<(), String> {
        let mut json = serde_json::to_string_pretty(&serde_json::json!({
            "cheats": self.list.clone(),
        }))
        .map_err(|e| format!("could not serialize the cheats: {e}"))?;
        json.push('\n');
        crate::atomic::write(path, json.as_bytes())
    }

    pub fn list(&self) -> &[Cheat] {
        &self.list
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Add one, or replace the cheat of the same name. Returns whether an
    /// existing one was replaced.
    pub fn add(&mut self, cheat: Cheat) -> bool {
        self.fired.clear();
        match self.list.iter_mut().find(|c| c.name == cheat.name) {
            Some(slot) => {
                *slot = cheat;
                true
            }
            None => {
                self.list.push(cheat);
                false
            }
        }
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.list.len();
        self.list.retain(|c| c.name != name);
        self.fired.clear();
        self.list.len() != before
    }

    /// Turn one on or off. `None` when no cheat carries that name.
    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> Option<&Cheat> {
        self.fired.clear();
        let slot = self.list.iter_mut().find(|c| c.name == name)?;
        slot.enabled = enabled;
        Some(slot)
    }

    /// Every name the list holds, for an error message that has to say what it
    /// *would* have accepted.
    pub fn names(&self) -> Vec<String> {
        self.list.iter().map(|c| c.name.clone()).collect()
    }

    /// Write the enabled cheats onto the bus. Called after every emulated
    /// frame, in the windowed loop and on the agent channel alike, so it stays
    /// a walk over a list that is empty for almost every game.
    pub fn apply(&mut self, snes: &mut Snes) {
        if self.is_empty() {
            return;
        }
        for cheat in &self.list {
            if !cheat.enabled {
                continue;
            }
            match cheat.kind {
                Kind::Freeze => cheat.write(snes),
                Kind::Once => {
                    if !self.fired.contains(&cheat.name) {
                        cheat.write(snes);
                        self.fired.insert(cheat.name.clone());
                    }
                }
            }
        }
    }

    /// Let the `once` cheats fire again — after a save state is loaded, the
    /// console no longer holds what they wrote.
    pub fn rearm(&mut self) {
        self.fired.clear();
    }
}

/// Bytes are consecutive on the 24-bit bus and wrap at the top of the address
/// space, exactly as `agent`'s reads and writes do.
fn step_addr(addr: u32, i: u32) -> u32 {
    addr.wrapping_add(i) & 0x00FF_FFFF
}

fn format_addr(addr: u32) -> String {
    format!("{:02X}:{:04X}", (addr >> 16) as u8, addr as u16)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.len() % 2 != 0 {
        return Err(format!("\"hex\" needs an even number of digits, got {}", clean.len()));
    }
    (0..clean.len() / 2)
        .map(|i| {
            u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16)
                .map_err(|_| format!("bad hex byte in {s:?}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use snes_core::{Cartridge, JoypadState};
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("prisme_cheats_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn lives() -> Cheat {
        Cheat::new("Vies infinies".into(), "7E:0DBE", "63", Kind::Freeze, true).expect("cheat")
    }

    /// Minimal PAL LoROM cart whose reset vector jumps to a `STP`-free loop of
    /// $00 opcodes; enough for `run_frame` to terminate.
    fn test_snes() -> Snes {
        let mut rom = vec![0u8; 0x10000];
        rom[0x7FC0..0x7FC0 + 21].copy_from_slice(b"CHEAT TEST           ");
        rom[0x7FC0 + 0x15] = 0x20;
        rom[0x7FC0 + 0x19] = 2; // PAL
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        Snes::new(Cartridge::from_bytes(rom).expect("cart"))
    }

    #[test]
    fn an_address_is_written_the_way_the_rest_of_the_cli_writes_it() {
        for (given, expect) in [
            ("7E:0DBE", 0x7E_0DBE),
            ("7e:0dbe", 0x7E_0DBE),
            ("7E0DBE", 0x7E_0DBE),
            ("$7E:0DBE", 0x7E_0DBE),
            ("0x7E0DBE", 0x7E_0DBE),
            (" 7E:0DBE ", 0x7E_0DBE),
        ] {
            let c = Cheat::new("n".into(), given, "01", Kind::Freeze, true).expect(given);
            assert_eq!(c.addr, expect, "{given}");
            assert_eq!(c.addr_text(), "7E:0DBE");
        }
    }

    #[test]
    fn every_unusable_cheat_is_refused_with_a_reason() {
        for (addr, hex, why) in [
            ("", "01", "empty address"),
            ("nonsense", "01", "not an address"),
            ("7E:ZZZZ", "01", "bad hex in the address"),
            ("7E00190", "01", "too many digits"),
            ("7E:0000", "", "no payload"),
            ("7E:0000", "0", "odd number of digits"),
            ("7E:0000", "GG", "not hex"),
            // The MMIO window: rewriting it every frame would latch counters
            // and move the WRAM port.
            ("00:2100", "01", "MMIO"),
            ("80:4210", "01", "MMIO through the mirror"),
            ("00:1FFF", "0102", "a span that runs into MMIO"),
        ] {
            let e = Cheat::new("n".into(), addr, hex, Kind::Freeze, true).unwrap_err();
            assert!(!e.is_empty(), "{why}: {addr} {hex}");
        }
        // A payload longer than a counter is refused as a whole.
        let long = "00".repeat(MAX_BYTES + 1);
        assert!(Cheat::new("n".into(), "7E:0000", &long, Kind::Freeze, true).is_err());
        // …and a nameless cheat could never be turned off again.
        assert!(Cheat::new("  ".into(), "7E:0000", "01", Kind::Freeze, true).is_err());
        // WRAM, SRAM and ROM addresses are all fine.
        for addr in ["7E:0000", "7F:FFFF", "00:1FFE", "70:0000", "C0:0000"] {
            assert!(Cheat::new("n".into(), addr, "01", Kind::Once, true).is_ok(), "{addr}");
        }
    }

    #[test]
    fn a_kind_reads_and_writes_the_same_two_words() {
        assert_eq!(Kind::parse("freeze").unwrap(), Kind::Freeze);
        assert_eq!(Kind::parse(" once ").unwrap(), Kind::Once);
        assert_eq!(Kind::Freeze.as_str(), "freeze");
        assert_eq!(Kind::Once.as_str(), "once");
        for bad in ["", "frozen", "FREEZE", "always"] {
            assert!(Kind::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_sidecar_round_trips_through_a_file() {
        let dir = scratch("roundtrip");
        let path = dir.join("game.cheats.json");
        let mut cheats = Cheats::default();
        cheats.add(lives());
        cheats.add(
            Cheat::new("Énergie au max".into(), "7E:0DB4", "0A0A", Kind::Once, false)
                .expect("cheat"),
        );
        cheats.write_to(&path).expect("write");

        let back = Cheats::read_from(&path);
        assert_eq!(back.list(), cheats.list());
        let first = &back.list()[0];
        assert_eq!(first.name, "Vies infinies");
        assert_eq!(first.addr_text(), "7E:0DBE");
        assert_eq!(first.hex(), "63");
        assert_eq!(first.kind, Kind::Freeze);
        assert!(first.enabled);
        assert!(!back.list()[1].enabled);

        // The file is meant to be read by a human: address and payload are
        // text, not numbers.
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("\"7E:0DBE\""), "{text}");
        assert!(text.contains("\"freeze\""), "{text}");
        assert!(text.contains("Énergie au max"), "{text}");

        // Rewriting leaves no temp file behind (atomic write).
        cheats.write_to(&path).expect("rewrite");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "game.cheats.json")
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_broken_file_is_an_empty_list_not_a_failure() {
        let dir = scratch("broken");
        assert!(Cheats::read_from(&dir.join("absent.json")).is_empty());
        for text in ["{ not json", "[1,2,3]", "null"] {
            std::fs::write(dir.join("x.json"), text).expect("write");
            assert!(Cheats::read_from(&dir.join("x.json")).is_empty(), "{text}");
        }
        // A file with no `cheats` key at all is simply empty.
        assert_eq!(Cheats::from_json("{}").expect("parse").len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_unusable_entry_does_not_take_the_others_with_it() {
        let list = Cheats::from_json(
            r#"{"cheats":[
                {"name":"bonne","addr":"7E:0DBE","hex":"63","kind":"freeze"},
                {"name":"adresse cassée","addr":"pas une adresse","hex":"63","kind":"freeze"},
                {"name":"mmio","addr":"00:2100","hex":"01","kind":"freeze"},
                {"name":"doublon","addr":"7E:0001","hex":"01","kind":"once","enabled":false},
                {"name":"doublon","addr":"7E:0002","hex":"02","kind":"once"}
            ]}"#,
        )
        .expect("parse");
        let names: Vec<_> = list.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["bonne", "doublon"]);
        // `enabled` defaults to true when the file omits it, and the first of
        // two homonyms is the one kept.
        assert!(list[0].enabled);
        assert!(!list[1].enabled);
        assert_eq!(list[1].addr_text(), "7E:0001");
    }

    #[test]
    fn a_name_is_the_identity_of_a_cheat() {
        let mut cheats = Cheats::default();
        assert!(!cheats.add(lives()));
        assert_eq!(cheats.list().len(), 1);
        // The same name again replaces, so re-running the search updates the
        // address instead of leaving two entries the player has to pick from.
        let moved = Cheat::new("Vies infinies".into(), "7E:0DB4", "05", Kind::Once, true).unwrap();
        assert!(cheats.add(moved));
        assert_eq!(cheats.list().len(), 1);
        assert_eq!(cheats.list()[0].addr_text(), "7E:0DB4");

        assert!(cheats.set_enabled("Vies infinies", false).is_some());
        assert!(!cheats.list()[0].enabled);
        assert!(cheats.set_enabled("inconnue", true).is_none());
        assert!(cheats.remove("Vies infinies"));
        assert!(!cheats.remove("Vies infinies"));
        assert!(cheats.is_empty());
    }

    /// The whole point of `freeze`: the game writes over the value, and the
    /// cheat puts it back — after the frame, every frame.
    #[test]
    fn a_frozen_cheat_is_rewritten_after_every_frame() {
        let mut snes = test_snes();
        let mut cheats = Cheats::default();
        cheats.add(lives());
        for _ in 0..3 {
            snes.run_frame([JoypadState::default(); 2]);
            // Whatever ran during the frame, the game's own value is there…
            snes.bus.write_no_tick(0x7E_0DBE, 0x04);
            assert_eq!(snes.bus.read_no_tick(0x7E_0DBE), 0x04);
            // …until the cheat is applied, which is what the loops do next.
            cheats.apply(&mut snes);
            assert_eq!(snes.bus.read_no_tick(0x7E_0DBE), 0x63);
        }
    }

    #[test]
    fn a_once_cheat_fires_exactly_once_and_a_disabled_one_never_does() {
        let mut snes = test_snes();
        let mut cheats = Cheats::default();
        cheats.add(Cheat::new("Pièces".into(), "7E:0DBF", "63", Kind::Once, true).unwrap());
        cheats.add(Cheat::new("Éteinte".into(), "7E:0DC0", "63", Kind::Freeze, false).unwrap());

        cheats.apply(&mut snes);
        assert_eq!(snes.bus.read_no_tick(0x7E_0DBF), 0x63);
        assert_eq!(snes.bus.read_no_tick(0x7E_0DC0), 0x00, "a disabled cheat writes nothing");

        // The game spends them; the cheat does not hand them back.
        snes.bus.write_no_tick(0x7E_0DBF, 0x00);
        cheats.apply(&mut snes);
        assert_eq!(snes.bus.read_no_tick(0x7E_0DBF), 0x00);

        // …until something rearms it (a save state was loaded, the list
        // changed), which is the only way a `once` cheat fires again.
        cheats.rearm();
        cheats.apply(&mut snes);
        assert_eq!(snes.bus.read_no_tick(0x7E_0DBF), 0x63);
    }

    #[test]
    fn a_multi_byte_cheat_writes_consecutive_addresses() {
        let mut snes = test_snes();
        let mut cheats = Cheats::default();
        cheats.add(Cheat::new("Or".into(), "7E:1F00", "E80300".into(), Kind::Once, true).unwrap());
        cheats.apply(&mut snes);
        assert_eq!(snes.bus.read_no_tick(0x7E_1F00), 0xE8);
        assert_eq!(snes.bus.read_no_tick(0x7E_1F01), 0x03);
        assert_eq!(snes.bus.read_no_tick(0x7E_1F02), 0x00);
    }

    /// The sidecar follows the same layout rules as the `.srm` and the states:
    /// a configured folder names it after the game, none puts it beside the ROM.
    #[test]
    fn the_sidecar_sits_where_every_other_sidecar_of_the_game_sits() {
        let beside = GamePaths::new(Path::new("/roms/game.sfc"), "GAME-0001", None, None);
        assert_eq!(beside.cheats_write(), PathBuf::from("/roms/game.cheats.json"));
        let folder = GamePaths::new(
            Path::new("/roms/game.sfc"),
            "GAME-0001",
            Some(PathBuf::from("/saves")),
            None,
        );
        assert_eq!(folder.cheats_write(), PathBuf::from("/saves/GAME-0001.cheats.json"));
        // `--save PATH` names one `.srm` and must not drag the cheats with it.
        let overridden = GamePaths::new(
            Path::new("/roms/game.sfc"),
            "GAME-0001",
            None,
            Some(PathBuf::from("/elsewhere/custom.srm")),
        );
        assert_eq!(overridden.cheats_write(), PathBuf::from("/roms/game.cheats.json"));
    }

    #[test]
    fn loading_and_saving_go_through_the_game_paths() {
        let dir = scratch("paths");
        let rom = dir.join("game.sfc");
        std::fs::write(&rom, b"rom").expect("write rom");
        let paths = GamePaths::new(&rom, "GAME-0001", None, None);
        assert!(Cheats::load(&paths).is_empty());
        let mut cheats = Cheats::default();
        cheats.add(lives());
        cheats.save(&paths).expect("save");
        assert!(dir.join("game.cheats.json").is_file());
        assert_eq!(Cheats::load(&paths).list(), cheats.list());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
