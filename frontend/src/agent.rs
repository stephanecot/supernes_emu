//! `--agent`: a control channel for an external program (in practice a local
//! Claude Code instance) driving the emulator — one JSON object per line on
//! stdin, one JSON object per line on stdout.
//!
//! This is a developer/tool interface, not a player-facing one: it stays in
//! English and is never translated, because scripts anchor on its wording.
//!
//! Two rules shape everything below.
//!
//! *One request, one response, always.* Every line that arrives — including a
//! blank one, a malformed one or an unknown command — produces exactly one
//! answer, carrying back the request's `id` when it had one. An agent that
//! gets no answer cannot tell a crash from a slow frame, so errors are values
//! (`{"error": "..."}`) and the channel stays open; the process exits only on
//! `quit` or on stdin closing.
//!
//! *Determinism is the feature.* The emulator is byte-identical on replay,
//! which is what makes an agent's memory search reproducible. Nothing here
//! reads a clock, and the channel's own latency cannot change what the
//! emulation does: frames advance only when a `step`/`press` asks for them,
//! and observation (`read-mem`, `screenshot`) never advances the console.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use snes_core::{JoypadState, Mapping, Region, Snes, SCREEN_HEIGHT, SCREEN_WIDTH};

use crate::input;

/// Bumped on any incompatible change to the shapes below; reported by the
/// `ready` greeting and by `state`.
pub const PROTOCOL_VERSION: u32 = 1;

/// Where `screenshot` and `save-state` write when the caller names no path.
/// Under `target/` so a session leaves nothing in the repo root.
pub const DEFAULT_OUT_DIR: &str = "target/debug-out/agent";

/// Largest `read-mem`/`write-mem` payload. A whole WRAM bank in one line would
/// be 128 KB of hex; a cheat search works on windows far smaller than this.
const MAX_MEM_LEN: u32 = 4096;

/// Largest `step`/`press` burst. A typo (`"frames": 10000000`) would otherwise
/// wedge the channel for hours with no way to interrupt it.
const MAX_FRAMES: u32 = 100_000;

/// The button names the rest of the codebase uses (`--script`, the keymap).
const BUTTONS: [&str; 12] =
    ["A", "B", "X", "Y", "L", "R", "Start", "Select", "Up", "Down", "Left", "Right"];

/// One line of the protocol, as understood after parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Step { frames: u32 },
    Press { buttons: Vec<String>, frames: u32, release: u32 },
    Screenshot { path: Option<PathBuf> },
    ReadMem { addr: u32, len: u32 },
    WriteMem { addr: u32, bytes: Vec<u8> },
    SaveState { path: Option<PathBuf> },
    LoadState { path: Option<PathBuf> },
    State,
    Ping,
    Help,
    Quit,
}

/// What the loop does with a handled line.
pub enum Reply {
    Send(Value),
    /// Answer, then close the channel (`quit`).
    Quit(Value),
}

/// One command per line, with its accepted fields. Returned by `help`, and
/// listed in the greeting so an agent needs no out-of-band documentation.
pub const HELP: [&str; 12] = [
    "{\"cmd\":\"step\",\"frames\":N} - emulate N frames (default 1) with no button held",
    "{\"cmd\":\"press\",\"button\":\"Start\"|\"buttons\":[\"B\",\"Right\"],\"frames\":N,\"release\":M} - hold for N frames (default 1), then M frames released (default 0)",
    "{\"cmd\":\"screenshot\",\"path\":\"out.png\"} - write the 256x224 framebuffer as PNG (default: target/debug-out/agent/frame_NNNNN.png) and return its path",
    "{\"cmd\":\"read-mem\",\"addr\":\"7E:0019\",\"len\":N} - read N bytes (1..=4096) from the bus, returned as hex",
    "{\"cmd\":\"write-mem\",\"addr\":\"7E:0019\",\"hex\":\"05\"|\"bytes\":[5]} - write bytes to the bus",
    "{\"cmd\":\"save-state\",\"path\":\"s.state\"} - write a save state (default: target/debug-out/agent/state_NNNNN.state) and return its path",
    "{\"cmd\":\"load-state\",\"path\":\"s.state\"} - restore a save state (default: the last one taken this session)",
    "{\"cmd\":\"state\"} - frame count, ROM title/region/mapping, last save state (alias: info)",
    "{\"cmd\":\"ping\"} - liveness check",
    "{\"cmd\":\"help\"} - this list",
    "{\"cmd\":\"quit\"} - answer, then exit 0",
    "every request may carry an \"id\" of any JSON type; it is echoed on the response, success or error",
];

/// Emulator plus the bookkeeping the channel needs: how far the console has
/// been driven, and which save states this session took.
pub struct Session {
    snes: Snes,
    rom_path: PathBuf,
    /// Frames emulated since the session started. This is the agent's only
    /// clock — save states restore it, so a `load-state` really does put the
    /// session back where it was.
    frame: u64,
    out_dir: PathBuf,
    /// Frame each state written this session was taken at, so `load-state`
    /// can restore the counter along with the console.
    states: HashMap<PathBuf, u64>,
    last_state: Option<PathBuf>,
    /// True while the console still holds exactly what the last `save-state`
    /// or `load-state` left there (no frame stepped, no memory written since).
    at_saved_state: bool,
    /// The APU queues ~640 stereo samples per frame for a frontend that never
    /// consumes them here; drained (and dropped) each frame so a long session
    /// does not grow without bound. Draining alone runs no SPC700 cycles, so
    /// it cannot affect emulation.
    audio_sink: Vec<(i16, i16)>,
}

impl Session {
    pub fn new(snes: Snes, rom_path: PathBuf) -> Session {
        Session {
            snes,
            rom_path,
            frame: 0,
            out_dir: PathBuf::from(DEFAULT_OUT_DIR),
            states: HashMap::new(),
            last_state: None,
            at_saved_state: false,
            audio_sink: Vec::new(),
        }
    }

    /// Unsolicited first line: the channel is up and the cartridge is loaded.
    /// Tagged `"event": "ready"` rather than answering an `id`, so a client can
    /// tell it apart from a response to a request it made.
    pub fn greeting(&self) -> Value {
        let mut v = self.describe();
        let obj = v.as_object_mut().expect("describe returns an object");
        obj.insert("ok".into(), Value::Bool(true));
        obj.insert("event".into(), json!("ready"));
        obj.insert("buttons".into(), json!(BUTTONS));
        obj.insert("commands".into(), json!(HELP));
        v
    }

    /// Parse one line, run it, and produce the single answer it is owed.
    pub fn handle(&mut self, line: &str) -> Reply {
        let (id, parsed) = parse(line);
        match parsed {
            Err(e) => Reply::Send(error(&id, e)),
            Ok(Request::Quit) => Reply::Quit(reply(&id, json!({ "frame": self.frame }))),
            Ok(req) => match self.exec(req) {
                Ok(body) => Reply::Send(reply(&id, body)),
                Err(e) => Reply::Send(error(&id, e)),
            },
        }
    }

    fn exec(&mut self, req: Request) -> Result<Value, String> {
        match req {
            Request::Step { frames } => {
                self.run_frames(frames, JoypadState::default());
                Ok(json!({ "frame": self.frame, "frames": frames }))
            }
            Request::Press { buttons, frames, release } => {
                let mut pad = JoypadState::default();
                for b in &buttons {
                    input::set_button(&mut pad, b, true)?;
                }
                self.run_frames(frames, pad);
                self.run_frames(release, JoypadState::default());
                Ok(json!({
                    "frame": self.frame,
                    "buttons": buttons,
                    "frames": frames,
                    "release": release,
                }))
            }
            Request::Screenshot { path } => {
                let path = match path {
                    Some(p) => {
                        crate::create_parent_dir(&p)?;
                        p
                    }
                    None => self.default_path(&format!("frame_{:05}", self.frame), "png")?,
                };
                crate::write_frame_png(&self.snes, &path)?;
                Ok(json!({
                    "path": display_path(&path),
                    "frame": self.frame,
                    "width": SCREEN_WIDTH,
                    "height": SCREEN_HEIGHT,
                }))
            }
            Request::ReadMem { addr, len } => {
                check_range(addr, len)?;
                let mut hex = String::with_capacity(len as usize * 2);
                for i in 0..len {
                    let b = self.snes.bus.read_no_tick(step_addr(addr, i));
                    hex.push_str(&format!("{b:02X}"));
                }
                Ok(json!({ "addr": format_addr(addr), "len": len, "hex": hex, "frame": self.frame }))
            }
            Request::WriteMem { addr, bytes } => {
                check_range(addr, bytes.len() as u32)?;
                for (i, b) in bytes.iter().enumerate() {
                    self.snes.bus.write_no_tick(step_addr(addr, i as u32), *b);
                }
                self.at_saved_state = false;
                Ok(json!({
                    "addr": format_addr(addr),
                    "len": bytes.len(),
                    "frame": self.frame,
                }))
            }
            Request::SaveState { path } => {
                let path = match path {
                    Some(p) => {
                        crate::create_parent_dir(&p)?;
                        p
                    }
                    None => self.default_path(&format!("state_{:05}", self.frame), "state")?,
                };
                let bytes = self.snes.save_state();
                crate::atomic::write(&path, &bytes)?;
                // Keyed by the resolved path, so a state saved as `s.state`
                // and reloaded as `./s.state` is recognized as the same one.
                let path = resolve(&path);
                self.states.insert(path.clone(), self.frame);
                self.last_state = Some(path.clone());
                self.at_saved_state = true;
                Ok(json!({
                    "path": display_path(&path),
                    "bytes": bytes.len(),
                    "frame": self.frame,
                }))
            }
            Request::LoadState { path } => {
                let path = match path.or_else(|| self.last_state.clone()) {
                    Some(p) => p,
                    None => {
                        return Err(
                            "no save state taken this session; pass \"path\"".to_string()
                        )
                    }
                };
                let bytes = std::fs::read(&path)
                    .map_err(|e| format!("read {}: {e}", path.display()))?;
                self.snes.load_state(&bytes)?;
                // A state written by an earlier process carries no frame
                // count of ours, so the counter keeps running instead of
                // silently claiming a frame number it cannot know.
                let path = resolve(&path);
                let restored = self.states.get(&path).copied();
                if let Some(f) = restored {
                    self.frame = f;
                }
                self.last_state = Some(path.clone());
                self.at_saved_state = true;
                Ok(json!({
                    "path": display_path(&path),
                    "frame": self.frame,
                    "frame_restored": restored.is_some(),
                }))
            }
            Request::State => Ok(self.describe()),
            Request::Ping => Ok(json!({ "frame": self.frame })),
            Request::Help => Ok(json!({ "commands": HELP, "buttons": BUTTONS })),
            // `handle` answers `quit` itself: it must close the loop, which
            // `exec` has no way to say.
            Request::Quit => unreachable!("quit is handled by the caller"),
        }
    }

    /// Where the emulator is. Shared by `state`, `info` and the greeting.
    fn describe(&self) -> Value {
        let cart = &self.snes.bus.cart;
        json!({
            "protocol": PROTOCOL_VERSION,
            "frame": self.frame,
            "rom": display_path(&self.rom_path),
            "title": cart.title.trim(),
            "region": match cart.region { Region::Pal => "PAL", Region::Ntsc => "NTSC" },
            "mapping": match cart.mapping { Mapping::LoRom => "LoROM", Mapping::HiRom => "HiROM" },
            "rom_bytes": cart.rom.len(),
            "sram_bytes": cart.sram.len(),
            "at_saved_state": self.at_saved_state,
            "last_state": match &self.last_state {
                Some(p) => json!({
                    "path": display_path(p),
                    "frame": self.states.get(p),
                }),
                None => Value::Null,
            },
            "out_dir": display_path(&self.out_dir),
        })
    }

    /// Advance the console `frames` frames with `pad` held on player 1 (player
    /// 2 is left idle, exactly as `--script` drives a headless run).
    fn run_frames(&mut self, frames: u32, pad: JoypadState) {
        for _ in 0..frames {
            self.snes.run_frame([pad, JoypadState::default()]);
            self.snes.bus.apu.drain_samples(&mut self.audio_sink);
            self.audio_sink.clear();
            self.frame += 1;
        }
        if frames > 0 {
            self.at_saved_state = false;
        }
    }

    /// A fresh, never-used name under `out_dir` for an unnamed capture.
    /// `unique_path` reserves it atomically, so two captures taken at the same
    /// frame cannot overwrite each other.
    fn default_path(&self, stem: &str, ext: &str) -> Result<PathBuf, String> {
        std::fs::create_dir_all(&self.out_dir)
            .map_err(|e| format!("create {}: {e}", self.out_dir.display()))?;
        Ok(crate::unique_path(&self.out_dir, stem, ext))
    }
}

/// Serve the channel until `quit` or EOF on stdin. Returns `Ok` on both: a
/// closed stdin is how a client detaches, not a failure.
pub fn run(snes: Snes, rom_path: &Path) -> Result<(), String> {
    let mut session = Session::new(snes, rom_path.to_path_buf());
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    emit(&mut out, &session.greeting())?;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| format!("read stdin: {e}"))?;
        match session.handle(&line) {
            Reply::Send(v) => emit(&mut out, &v)?,
            Reply::Quit(v) => return emit(&mut out, &v),
        }
    }
    Ok(())
}

/// One JSON object, one line, flushed: a client blocked on a read must see the
/// answer as soon as it exists.
fn emit(out: &mut impl Write, v: &Value) -> Result<(), String> {
    writeln!(out, "{v}").map_err(|e| format!("write stdout: {e}"))?;
    out.flush().map_err(|e| format!("flush stdout: {e}"))
}

/// Split a line into its echoed `id` and the request it holds. The id is
/// recovered even from a request that fails to parse, so an error can still be
/// matched to the line that caused it.
pub fn parse(line: &str) -> (Value, Result<Request, String>) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return (Value::Null, Err("empty request line".into()));
    }
    let v: Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(e) => return (Value::Null, Err(format!("invalid JSON: {e}"))),
    };
    let Some(obj) = v.as_object() else {
        return (Value::Null, Err("request must be a JSON object".into()));
    };
    let id = obj.get("id").cloned().unwrap_or(Value::Null);
    (id, Request::from_object(obj))
}

impl Request {
    fn from_object(obj: &Map<String, Value>) -> Result<Request, String> {
        let cmd = obj
            .get("cmd")
            .or_else(|| obj.get("command"))
            .ok_or("missing \"cmd\"")?
            .as_str()
            .ok_or("\"cmd\" must be a string")?;
        match cmd {
            "step" => Ok(Request::Step { frames: frame_count(obj, "frames", 1)? }),
            "press" => Ok(Request::Press {
                buttons: buttons(obj)?,
                frames: frame_count(obj, "frames", 1)?,
                release: frame_count(obj, "release", 0)?,
            }),
            "screenshot" => Ok(Request::Screenshot { path: opt_path(obj, "path")? }),
            "read-mem" => {
                let len = u32_field(obj, "len")?.unwrap_or(1);
                if len == 0 || len > MAX_MEM_LEN {
                    return Err(format!("\"len\" must be 1..={MAX_MEM_LEN}, got {len}"));
                }
                Ok(Request::ReadMem { addr: addr_field(obj)?, len })
            }
            "write-mem" => {
                let bytes = write_bytes(obj)?;
                if bytes.is_empty() || bytes.len() as u32 > MAX_MEM_LEN {
                    return Err(format!(
                        "write payload must be 1..={MAX_MEM_LEN} bytes, got {}",
                        bytes.len()
                    ));
                }
                Ok(Request::WriteMem { addr: addr_field(obj)?, bytes })
            }
            "save-state" => Ok(Request::SaveState { path: opt_path(obj, "path")? }),
            "load-state" => Ok(Request::LoadState { path: opt_path(obj, "path")? }),
            "state" | "info" => Ok(Request::State),
            "ping" => Ok(Request::Ping),
            "help" => Ok(Request::Help),
            "quit" | "exit" => Ok(Request::Quit),
            other => Err(format!(
                "unknown command: {other} (try {{\"cmd\":\"help\"}})"
            )),
        }
    }
}

fn u32_field(obj: &Map<String, Value>, key: &str) -> Result<Option<u32>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| {
                format!("\"{key}\" must be a non-negative whole number, got {v}")
            })?;
            u32::try_from(n).map(Some).map_err(|_| format!("\"{key}\" is too large: {n}"))
        }
    }
}

fn frame_count(obj: &Map<String, Value>, key: &str, default: u32) -> Result<u32, String> {
    let n = u32_field(obj, key)?.unwrap_or(default);
    if n > MAX_FRAMES {
        return Err(format!("\"{key}\" must be <= {MAX_FRAMES}, got {n}"));
    }
    Ok(n)
}

fn opt_path(obj: &Map<String, Value>, key: &str) -> Result<Option<PathBuf>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => {
            Err(format!("\"{key}\" must not be empty"))
        }
        Some(Value::String(s)) => Ok(Some(PathBuf::from(s))),
        Some(v) => Err(format!("\"{key}\" must be a string, got {v}")),
    }
}

/// `button` (one name) or `buttons` (several, for a running jump). Names are
/// validated here so a typo is refused before any frame is emulated.
fn buttons(obj: &Map<String, Value>) -> Result<Vec<String>, String> {
    let names: Vec<String> = match (obj.get("button"), obj.get("buttons")) {
        (Some(Value::String(s)), None) => vec![s.clone()],
        (None, Some(Value::Array(a))) => a
            .iter()
            .map(|v| {
                v.as_str().map(str::to_string).ok_or_else(|| "\"buttons\" holds strings".to_string())
            })
            .collect::<Result<_, _>>()?,
        (None, None) => return Err(format!("press needs \"button\" or \"buttons\" ({})", BUTTONS.join(" "))),
        _ => return Err("give either \"button\" or \"buttons\", not both".into()),
    };
    if names.is_empty() {
        return Err("\"buttons\" must not be empty".into());
    }
    let mut scratch = JoypadState::default();
    for n in &names {
        input::set_button(&mut scratch, n, true)
            .map_err(|e| format!("{e} (known: {})", BUTTONS.join(" ")))?;
    }
    Ok(names)
}

/// `hex` (an even-length hex string) or `bytes` (an array of 0..=255).
fn write_bytes(obj: &Map<String, Value>) -> Result<Vec<u8>, String> {
    match (obj.get("hex"), obj.get("bytes")) {
        (Some(Value::String(s)), None) => parse_hex(s),
        (None, Some(Value::Array(a))) => a
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or_else(|| format!("\"bytes\" holds 0..=255, got {v}"))
            })
            .collect(),
        (None, None) => Err("write-mem needs \"hex\" or \"bytes\"".into()),
        _ => Err("give either \"hex\" or \"bytes\", not both".into()),
    }
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

/// `addr` is a bus address written like the rest of the CLI (`--watch BB:AAAA`),
/// with a bare 24-bit hex form (`7E0019`) accepted as well since an agent
/// computing addresses rarely has the colon handy.
fn addr_field(obj: &Map<String, Value>) -> Result<u32, String> {
    let v = obj.get("addr").or_else(|| obj.get("address")).ok_or("missing \"addr\"")?;
    let s = v.as_str().ok_or_else(|| format!("\"addr\" must be a string like \"7E:0019\", got {v}"))?;
    parse_addr(s)
}

pub fn parse_addr(s: &str) -> Result<u32, String> {
    let s = s.trim();
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("$")).unwrap_or(s);
    if s.contains(':') {
        let (bank, off) = crate::parse_bus_addr(s)?;
        return Ok(((bank as u32) << 16) | off as u32);
    }
    if s.is_empty() || s.len() > 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("expected BB:AAAA or up to 6 hex digits, got {s:?}"));
    }
    u32::from_str_radix(s, 16).map_err(|_| format!("bad address: {s}"))
}

fn format_addr(addr: u32) -> String {
    format!("{:02X}:{:04X}", (addr >> 16) as u8, addr as u16)
}

/// Bytes are consecutive on the 24-bit bus, so a read that starts near the end
/// of a bank continues into the next one (`7E:FFFF` then `7F:0000` really is
/// contiguous WRAM), wrapping at the top of the address space.
fn step_addr(addr: u32, i: u32) -> u32 {
    addr.wrapping_add(i) & 0x00FF_FFFF
}

/// Refuse the I/O window of the system banks. Touching `$2100-$5FFF` behind
/// the game's back latches counters, clears interrupt flags and moves the WRAM
/// port: a read there would change what the console does next, which is
/// exactly the property this channel exists to preserve. WRAM, SRAM and ROM —
/// everything a cheat search needs — are unaffected.
fn check_addr(addr: u32) -> Result<(), String> {
    let bank = (addr >> 16) as u8;
    let off = addr as u16;
    let system_bank = matches!(bank, 0x00..=0x3F | 0x80..=0xBF);
    if system_bank && (0x2000..=0x5FFF).contains(&off) {
        return Err(format!(
            "{} is MMIO: reading or writing it from the agent channel would perturb the console (use WRAM 7E:0000-7F:FFFF)",
            format_addr(addr)
        ));
    }
    Ok(())
}

/// Validate the whole span before touching any of it, so a rejected
/// `write-mem` leaves nothing half-written.
fn check_range(addr: u32, len: u32) -> Result<(), String> {
    for i in 0..len {
        check_addr(step_addr(addr, i))?;
    }
    Ok(())
}

/// Absolute when it can be (an agent's file reader wants absolute paths), the
/// path as given otherwise — a path that does not exist yet cannot be
/// canonicalized.
fn resolve(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn display_path(p: &Path) -> String {
    resolve(p).display().to_string()
}

fn reply(id: &Value, mut fields: Value) -> Value {
    let obj = fields.as_object_mut().expect("command bodies are JSON objects");
    obj.insert("ok".into(), Value::Bool(true));
    if !id.is_null() {
        obj.insert("id".into(), id.clone());
    }
    fields
}

fn error(id: &Value, msg: impl Into<String>) -> Value {
    let mut obj = Map::new();
    obj.insert("error".into(), Value::String(msg.into()));
    if !id.is_null() {
        obj.insert("id".into(), id.clone());
    }
    Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;
    use snes_core::Cartridge;

    fn req(line: &str) -> Result<Request, String> {
        parse(line).1
    }

    /// Minimal PAL LoROM cart: enough header for `Cartridge::from_bytes` to
    /// map it, and 64 KB of $00 so `run_frame` still terminates.
    fn test_session() -> Session {
        let mut rom = vec![0u8; 0x10000];
        rom[0x7FC0..0x7FC0 + 21].copy_from_slice(b"AGENT TEST           ");
        rom[0x7FC0 + 0x15] = 0x20;
        rom[0x7FC0 + 0x19] = 2; // PAL
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        let cart = Cartridge::from_bytes(rom).expect("cart");
        let mut s = Session::new(Snes::new(cart), PathBuf::from("agent-test.sfc"));
        s.out_dir = std::env::temp_dir().join(format!("prisme_agent_{}", std::process::id()));
        s
    }

    fn send(s: &mut Session, line: &str) -> Value {
        match s.handle(line) {
            Reply::Send(v) | Reply::Quit(v) => v,
        }
    }

    #[test]
    fn every_command_shape_parses() {
        assert_eq!(req(r#"{"cmd":"step"}"#).unwrap(), Request::Step { frames: 1 });
        assert_eq!(req(r#"{"cmd":"step","frames":60}"#).unwrap(), Request::Step { frames: 60 });
        assert_eq!(
            req(r#"{"cmd":"press","button":"Start","frames":4,"release":2}"#).unwrap(),
            Request::Press { buttons: vec!["Start".into()], frames: 4, release: 2 }
        );
        assert_eq!(
            req(r#"{"cmd":"press","buttons":["B","Right"]}"#).unwrap(),
            Request::Press { buttons: vec!["B".into(), "Right".into()], frames: 1, release: 0 }
        );
        assert_eq!(req(r#"{"cmd":"screenshot"}"#).unwrap(), Request::Screenshot { path: None });
        assert_eq!(
            req(r#"{"cmd":"screenshot","path":"a/b.png"}"#).unwrap(),
            Request::Screenshot { path: Some(PathBuf::from("a/b.png")) }
        );
        assert_eq!(
            req(r#"{"cmd":"read-mem","addr":"7E:0019","len":16}"#).unwrap(),
            Request::ReadMem { addr: 0x7E_0019, len: 16 }
        );
        assert_eq!(
            req(r#"{"cmd":"write-mem","addr":"7E0019","hex":"0A ff"}"#).unwrap(),
            Request::WriteMem { addr: 0x7E_0019, bytes: vec![0x0A, 0xFF] }
        );
        assert_eq!(
            req(r#"{"cmd":"write-mem","addr":"$7E:0019","bytes":[5]}"#).unwrap(),
            Request::WriteMem { addr: 0x7E_0019, bytes: vec![5] }
        );
        assert_eq!(req(r#"{"cmd":"save-state"}"#).unwrap(), Request::SaveState { path: None });
        assert_eq!(
            req(r#"{"cmd":"load-state","path":"s.state"}"#).unwrap(),
            Request::LoadState { path: Some(PathBuf::from("s.state")) }
        );
        // `info` is an accepted alias of `state`, `exit` of `quit`.
        assert_eq!(req(r#"{"cmd":"state"}"#).unwrap(), Request::State);
        assert_eq!(req(r#"{"cmd":"info"}"#).unwrap(), Request::State);
        assert_eq!(req(r#"{"cmd":"ping"}"#).unwrap(), Request::Ping);
        assert_eq!(req(r#"{"cmd":"help"}"#).unwrap(), Request::Help);
        assert_eq!(req(r#"{"cmd":"quit"}"#).unwrap(), Request::Quit);
        assert_eq!(req(r#"{"cmd":"exit"}"#).unwrap(), Request::Quit);
    }

    #[test]
    fn a_malformed_line_is_an_error_value_not_a_panic() {
        for line in [
            "",
            "   ",
            "not json",
            "[1,2]",
            "\"a string\"",
            r#"{"frames":3}"#,          // no cmd
            r#"{"cmd":42}"#,            // cmd is not a string
            r#"{"cmd":"fly"}"#,         // unknown command
            r#"{"cmd":"step","frames":-1}"#,
            r#"{"cmd":"step","frames":999999999}"#,
            r#"{"cmd":"press"}"#,       // no button
            r#"{"cmd":"press","button":"Turbo"}"#,
            r#"{"cmd":"press","button":"A","buttons":["B"]}"#,
            r#"{"cmd":"read-mem"}"#,    // no addr
            r#"{"cmd":"read-mem","addr":"nonsense","len":1}"#,
            r#"{"cmd":"read-mem","addr":"7E:0000","len":0}"#,
            r#"{"cmd":"read-mem","addr":"7E:0000","len":99999}"#,
            r#"{"cmd":"write-mem","addr":"7E:0000"}"#,
            r#"{"cmd":"write-mem","addr":"7E:0000","hex":"abc"}"#,
            r#"{"cmd":"write-mem","addr":"7E:0000","bytes":[300]}"#,
            r#"{"cmd":"screenshot","path":""}"#,
            r#"{"cmd":"screenshot","path":7}"#,
        ] {
            assert!(req(line).is_err(), "{line} should not parse");
        }
    }

    #[test]
    fn the_unknown_command_error_names_the_command() {
        let e = req(r#"{"cmd":"fly"}"#).unwrap_err();
        assert!(e.contains("unknown command: fly"), "{e}");
    }

    #[test]
    fn addresses_take_the_cli_form_and_bare_hex() {
        assert_eq!(parse_addr("7E:0019").unwrap(), 0x7E_0019);
        assert_eq!(parse_addr("00:8000").unwrap(), 0x00_8000);
        assert_eq!(parse_addr("7E0019").unwrap(), 0x7E_0019);
        assert_eq!(parse_addr("0x7E0019").unwrap(), 0x7E_0019);
        assert_eq!(parse_addr("$7E:0019").unwrap(), 0x7E_0019);
        assert_eq!(format_addr(0x7E_0019), "7E:0019");
        for bad in ["", "ZZ:0000", "7E:ZZZZ", "7E00190", "hello", "7E:"] {
            assert!(parse_addr(bad).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn a_read_crossing_a_bank_boundary_stays_contiguous() {
        assert_eq!(step_addr(0x7E_FFFF, 1), 0x7F_0000);
        assert_eq!(step_addr(0xFF_FFFF, 1), 0x00_0000);
    }

    #[test]
    fn the_mmio_window_is_refused_in_both_directions() {
        let mut s = test_session();
        for addr in ["00:2100", "80:4210", "3F:5FFF", "00:2000"] {
            let v = send(&mut s, &format!(r#"{{"cmd":"read-mem","addr":"{addr}","len":1}}"#));
            assert!(v.get("error").is_some(), "{addr} should be refused: {v}");
            let v = send(&mut s, &format!(r#"{{"cmd":"write-mem","addr":"{addr}","hex":"00"}}"#));
            assert!(v.get("error").is_some(), "{addr} should be refused: {v}");
        }
        // A span that only *ends* in MMIO is refused as a whole, and writes
        // nothing: the check runs before the first byte.
        let v = send(&mut s, r#"{"cmd":"read-mem","addr":"00:1FF0","len":32}"#);
        assert!(v.get("error").is_some(), "{v}");
        // WRAM, SRAM and ROM are all fine.
        for addr in ["7E:0000", "7F:FFFF", "00:1FFF", "00:8000", "C0:0000"] {
            let v = send(&mut s, &format!(r#"{{"cmd":"read-mem","addr":"{addr}","len":1}}"#));
            assert!(v.get("ok").is_some(), "{addr} should be readable: {v}");
        }
    }

    #[test]
    fn the_id_is_echoed_on_success_and_on_error() {
        let mut s = test_session();
        let v = send(&mut s, r#"{"id":7,"cmd":"ping"}"#);
        assert_eq!(v["id"], json!(7));
        assert_eq!(v["ok"], json!(true));
        let v = send(&mut s, r#"{"id":"abc","cmd":"fly"}"#);
        assert_eq!(v["id"], json!("abc"));
        assert!(v["error"].is_string());
        // An id of any JSON type comes back verbatim, and a request without
        // one gets an answer without one (never a null id).
        let v = send(&mut s, r#"{"id":{"seq":1},"cmd":"ping"}"#);
        assert_eq!(v["id"], json!({"seq": 1}));
        let v = send(&mut s, r#"{"cmd":"ping"}"#);
        assert!(v.get("id").is_none(), "{v}");
        // Even a line that is not JSON at all gets exactly one answer.
        let v = send(&mut s, "}{");
        assert!(v["error"].as_str().unwrap().contains("invalid JSON"), "{v}");
    }

    #[test]
    fn quit_is_the_only_reply_that_closes_the_channel() {
        let mut s = test_session();
        assert!(matches!(s.handle(r#"{"cmd":"ping"}"#), Reply::Send(_)));
        assert!(matches!(s.handle(r#"{"cmd":"fly"}"#), Reply::Send(_)));
        match s.handle(r#"{"id":9,"cmd":"quit"}"#) {
            Reply::Quit(v) => assert_eq!(v["id"], json!(9)),
            Reply::Send(v) => panic!("quit should close the channel: {v}"),
        }
    }

    #[test]
    fn write_mem_then_read_mem_round_trips_through_the_bus() {
        let mut s = test_session();
        let v = send(&mut s, r#"{"cmd":"write-mem","addr":"7E:0100","hex":"DEADBEEF"}"#);
        assert_eq!(v["len"], json!(4));
        let v = send(&mut s, r#"{"cmd":"read-mem","addr":"7E:0100","len":4}"#);
        assert_eq!(v["hex"], json!("DEADBEEF"));
        assert_eq!(v["addr"], json!("7E:0100"));
        // The low 8 KB of WRAM is mirrored in every system bank, so the same
        // byte is visible through bank $00 — proof this really goes to the bus.
        let v = send(&mut s, r#"{"cmd":"read-mem","addr":"00:0100","len":1}"#);
        assert_eq!(v["hex"], json!("DE"));
    }

    #[test]
    fn a_save_state_round_trip_restores_memory_and_the_frame_count() {
        let mut s = test_session();
        send(&mut s, r#"{"cmd":"write-mem","addr":"7E:0200","hex":"11"}"#);
        send(&mut s, r#"{"cmd":"step","frames":2}"#);
        let saved = send(&mut s, r#"{"cmd":"save-state"}"#);
        let path = saved["path"].as_str().expect("path").to_string();
        assert_eq!(saved["frame"], json!(2));

        // Move on: change memory and the frame count.
        send(&mut s, r#"{"cmd":"write-mem","addr":"7E:0200","hex":"22"}"#);
        send(&mut s, r#"{"cmd":"step","frames":3}"#);
        let v = send(&mut s, r#"{"cmd":"read-mem","addr":"7E:0200","len":1}"#);
        assert_eq!(v["hex"], json!("22"));
        let v = send(&mut s, r#"{"cmd":"state"}"#);
        assert_eq!(v["frame"], json!(5));
        assert_eq!(v["at_saved_state"], json!(false));

        // …and come back.
        let v = send(&mut s, &format!(r#"{{"cmd":"load-state","path":"{path}"}}"#));
        assert_eq!(v["frame"], json!(2), "{v}");
        assert_eq!(v["frame_restored"], json!(true));
        let v = send(&mut s, r#"{"cmd":"read-mem","addr":"7E:0200","len":1}"#);
        assert_eq!(v["hex"], json!("11"));
        let v = send(&mut s, r#"{"cmd":"state"}"#);
        assert_eq!(v["at_saved_state"], json!(true));
        assert_eq!(v["title"], json!("AGENT TEST"));
        assert_eq!(v["region"], json!("PAL"));
        assert_eq!(v["mapping"], json!("LoROM"));
        assert_eq!(v["protocol"], json!(PROTOCOL_VERSION));

        // `load-state` with no path reuses the last one taken.
        send(&mut s, r#"{"cmd":"step","frames":1}"#);
        let v = send(&mut s, r#"{"cmd":"load-state"}"#);
        assert_eq!(v["frame"], json!(2), "{v}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn loading_a_state_that_does_not_exist_is_an_error_value() {
        let mut s = test_session();
        // No state taken yet and no path: refused rather than guessing.
        let v = send(&mut s, r#"{"cmd":"load-state"}"#);
        assert!(v["error"].is_string(), "{v}");
        let v = send(&mut s, r#"{"cmd":"load-state","path":"/nonexistent/x.state"}"#);
        assert!(v["error"].is_string(), "{v}");
        // …and the channel is still usable afterwards.
        assert_eq!(send(&mut s, r#"{"cmd":"ping"}"#)["ok"], json!(true));
    }

    #[test]
    fn stepping_and_pressing_advance_the_frame_count_and_nothing_else() {
        let mut s = test_session();
        assert_eq!(send(&mut s, r#"{"cmd":"state"}"#)["frame"], json!(0));
        assert_eq!(send(&mut s, r#"{"cmd":"step","frames":3}"#)["frame"], json!(3));
        // A press of N frames plus M released frames costs exactly N+M frames.
        let v = send(&mut s, r#"{"cmd":"press","button":"Start","frames":2,"release":1}"#);
        assert_eq!(v["frame"], json!(6));
        // Observation never advances the console.
        send(&mut s, r#"{"cmd":"read-mem","addr":"7E:0000","len":8}"#);
        send(&mut s, r#"{"cmd":"ping"}"#);
        assert_eq!(send(&mut s, r#"{"cmd":"state"}"#)["frame"], json!(6));
    }

    #[test]
    fn the_greeting_announces_the_protocol_the_rom_and_the_commands() {
        let s = test_session();
        let g = s.greeting();
        assert_eq!(g["event"], json!("ready"));
        assert_eq!(g["protocol"], json!(PROTOCOL_VERSION));
        assert_eq!(g["frame"], json!(0));
        assert_eq!(g["title"], json!("AGENT TEST"));
        assert_eq!(g["buttons"].as_array().unwrap().len(), BUTTONS.len());
        assert_eq!(g["commands"].as_array().unwrap().len(), HELP.len());
        assert!(g.get("id").is_none(), "the greeting answers no request");
    }

    #[test]
    fn a_screenshot_writes_a_png_and_returns_its_path() {
        let mut s = test_session();
        let v = send(&mut s, r#"{"cmd":"screenshot"}"#);
        let path = PathBuf::from(v["path"].as_str().expect("path"));
        assert_eq!(v["width"], json!(SCREEN_WIDTH));
        assert_eq!(v["height"], json!(SCREEN_HEIGHT));
        let bytes = std::fs::read(&path).expect("png written");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        // The default name carries the frame, and two captures at the same
        // frame never overwrite each other.
        assert!(path.to_string_lossy().contains("frame_00000"), "{}", path.display());
        let second = send(&mut s, r#"{"cmd":"screenshot"}"#);
        assert_ne!(second["path"], v["path"]);
        let _ = std::fs::remove_dir_all(&s.out_dir);
    }
}
