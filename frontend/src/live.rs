//! `--agent-attach`: the same control channel, aimed at the console the player
//! is watching instead of one of its own.
//!
//! `agent.rs` serves a *second* emulator over stdin/stdout: it owns its `Snes`,
//! and it runs a `step 300` in a tight loop because nothing else in that
//! process wants the thread. That is the right shape for a cheat search, whose
//! whole point is that nobody has to look at it. It is the wrong shape for
//! "play this passage for me", where the player asked to *watch*.
//!
//! So this module turns the dialect of `agent.rs` — same commands, same JSON,
//! same one-request-one-response rule, parsed by the very same code — into
//! something the winit event loop can serve **between two frames**:
//!
//! * a TCP listener on `127.0.0.1:0` (the OS picks the port). TCP and not a
//!   Unix socket because `std` has no `AF_UNIX` on Windows, and this ships
//!   there too;
//! * a secret on the first line, since a loopback port is open to every process
//!   on the machine and none of them was invited to play someone's game;
//! * a reader thread that turns the socket into a queue of lines, a writer
//!   thread that turns a queue of answers back into socket writes — so neither
//!   a silent client nor a client that stopped reading can stall the window;
//! * and a **pending command**: a `step N` or `press … N frames` holds the
//!   frames it still owes and the buttons it is holding, spends exactly one
//!   frame per iteration of the event loop, and is answered only when the
//!   count reaches zero. Nothing here ever loops over frames. That is the
//!   entire difference between this file and `agent.rs`, and the reason the
//!   window keeps drawing while the assistant plays.
//!
//! **The console only moves when it is asked to.** Between two commands — the
//! seconds the assistant spends looking at a screenshot and deciding — the
//! caller holds the emulation still (`Server::owes_frames` is false). It costs
//! the picture nothing (the window keeps presenting and the shell keeps
//! reacting) and it keeps the protocol's promise intact: `step 1` means one
//! frame, not one frame plus however long a model took to think.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use snes_core::{JoypadState, Snes, SCREEN_HEIGHT, SCREEN_WIDTH};

use crate::agent::{self, Request};
use crate::cheats::Cheats;
use crate::input;
use crate::paths::GamePaths;

/// Environment variable the attaching client reads its secret from, so it never
/// has to appear in a command line other local users can read with `ps`.
pub const SECRET_VAR: &str = "PRISME_AGENT_SECRET";

/// How long the accept/write thread waits before looking at the shutdown flag
/// again. Short enough that closing a session is instant, long enough to cost
/// nothing while one is idle.
const POLL: Duration = Duration::from_millis(20);

/// Most requests cost no frame, so a whole burst of them is normally answered
/// in one pass. This caps how many, so a client that queued a thousand
/// `read-mem` lines cannot spend a frame's worth of time in the event loop:
/// the rest simply waits for the next pass, one sixtieth of a second later.
const MAX_PER_PASS: usize = 32;

/// A client that connects and then says nothing holds the accept thread. It
/// gets this long to present its secret.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// What a command needs from the running session to be carried out. Borrowed
/// for the duration of one `pump`, so nothing here outlives the frame it acts
/// on — and so the assistant is unambiguously driving *this* console.
pub struct Console<'a> {
    pub snes: &'a mut Snes,
    pub cheats: &'a mut Cheats,
    pub paths: &'a GamePaths,
    pub rom: &'a Path,
    /// Re-derived after a `load-state`, exactly as the windowed load-state
    /// path does: the restored blob carries its own SRAM, and diffing the next
    /// battery write against the pre-load bytes would rewrite `.srm` with an
    /// older copy.
    pub sram_baseline: &'a mut Vec<u8>,
    /// Set when a `cheat-*` command changed the sidecar, so the shell can
    /// re-read what the sheet lists.
    pub cheats_changed: bool,
}

/// A `step` or `press` in flight: the frames it still owes, and the buttons it
/// is holding while it owes them.
///
/// This is the whole state machine. It never emulates anything and never
/// sleeps: the caller runs one frame, tells it so, and gets back either
/// `None` — meaning "come back after the next frame" — or the single answer
/// the request was owed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    id: Value,
    /// Empty for a `step`, which is what tells the two answers apart.
    buttons: Vec<String>,
    pad: JoypadState,
    /// Frames left to run with the buttons held.
    hold: u32,
    /// Frames left to run with nothing held, once the hold is over.
    release: u32,
    /// What the request asked for, echoed back on the answer.
    frames: u32,
    releases: u32,
}

impl Pending {
    /// Refuses an unknown button name here, before a single frame is run, the
    /// same way `agent::buttons` does for the stdio channel.
    fn new(id: Value, buttons: Vec<String>, frames: u32, release: u32) -> Result<Self, String> {
        let mut pad = JoypadState::default();
        for b in &buttons {
            input::set_button(&mut pad, b, true)?;
        }
        Ok(Pending { id, buttons, pad, hold: frames, release, frames, releases: release })
    }

    /// True while there is still a frame to run for this command.
    fn owes_frames(&self) -> bool {
        self.hold > 0 || self.release > 0
    }

    /// What player 1 must see on the frame about to run: the buttons while the
    /// hold lasts, nothing during the release that follows it.
    pub fn pad(&self) -> JoypadState {
        if self.hold > 0 {
            self.pad
        } else {
            JoypadState::default()
        }
    }

    /// One frame has been emulated. `Some` is the answer this command owed,
    /// produced only once the last frame it asked for is spent.
    fn frame_elapsed(&mut self, frame: u64) -> Option<Value> {
        if self.hold > 0 {
            self.hold -= 1;
        } else if self.release > 0 {
            self.release -= 1;
        }
        if self.owes_frames() {
            return None;
        }
        Some(agent::reply(&self.id, self.body(frame)))
    }

    /// The two answer shapes of `agent.rs`, unchanged: a `step` reports the
    /// frames it ran, a `press` also reports what it held and for how long.
    fn body(&self, frame: u64) -> Value {
        if self.buttons.is_empty() {
            json!({ "frame": frame, "frames": self.frames })
        } else {
            json!({
                "frame": frame,
                "buttons": self.buttons,
                "frames": self.frames,
                "release": self.releases,
            })
        }
    }
}

/// The listener, its connection, and the command in flight.
pub struct Server {
    port: u16,
    secret: String,
    /// Lines the client sent, in the order it sent them.
    requests: Receiver<String>,
    /// Answers, drained by a thread of its own: a client that stopped reading
    /// must not be able to block the event loop mid-frame.
    replies: Sender<String>,
    /// Raised on drop; the socket is shut down with it so no thread is left
    /// blocked on a read that will never complete.
    shutdown: Arc<AtomicBool>,
    /// A client just presented a valid secret and is owed the greeting, which
    /// only the event loop can build (it is the one holding the console).
    hello: Arc<AtomicBool>,
    stream: Arc<Mutex<Option<TcpStream>>>,
    pending: Option<Pending>,
    /// Frames this channel has watched go by. The agent's only clock.
    frame: u64,
    out_dir: PathBuf,
    states: HashMap<PathBuf, u64>,
    last_state: Option<PathBuf>,
    at_saved_state: bool,
    /// Set by `quit`: the channel answered and wants no more.
    closed: bool,
}

impl Server {
    /// Open the door. `out_dir` is where an unnamed screenshot or save state
    /// lands — the game's own sidecar folder rather than `target/`, since an
    /// installed application's working directory is nobody's business.
    pub fn start(out_dir: PathBuf) -> Result<Server, String> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .map_err(|e| format!("listen on 127.0.0.1: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("read the listening port: {e}"))?
            .port();
        // Polled rather than blocked on, so the thread can notice a shutdown
        // between two connection attempts instead of sitting in `accept`.
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("set the listener non-blocking: {e}"))?;

        let secret = new_secret();
        let (tx_req, requests) = channel();
        let (replies, rx_rep) = channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let hello = Arc::new(AtomicBool::new(false));
        let stream = Arc::new(Mutex::new(None));

        let server = Server {
            port,
            secret: secret.clone(),
            requests,
            replies,
            shutdown: Arc::clone(&shutdown),
            hello: Arc::clone(&hello),
            stream: Arc::clone(&stream),
            pending: None,
            frame: 0,
            out_dir,
            states: HashMap::new(),
            last_state: None,
            at_saved_state: false,
            closed: false,
        };

        std::thread::Builder::new()
            .name("prisme-agent-listen".to_string())
            .spawn(move || serve(&listener, &secret, &tx_req, &rx_rep, &shutdown, &hello, &stream))
            .map_err(|e| format!("could not start the control-channel thread: {e}"))?;
        Ok(server)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// True while a command still has frames to run. The caller emulates
    /// exactly when this holds, and holds the console still when it does not —
    /// see the module docs.
    pub fn owes_frames(&self) -> bool {
        self.pending.is_some()
    }

    /// What player 1 must see this frame, or `None` when no command is in
    /// flight (in which case the caller runs no frame at all).
    pub fn pad(&self) -> Option<JoypadState> {
        self.pending.as_ref().map(Pending::pad)
    }

    /// The client sent `quit`, or hung up: the session is over.
    pub fn closed(&self) -> bool {
        self.closed
    }

    /// Answer everything that can be answered now, and arm the one command that
    /// cannot. Never blocks and never emulates: the frames a `step`/`press`
    /// asks for are run by the caller, one per call to `frame_elapsed`.
    pub fn pump(&mut self, console: &mut Console) {
        if self.hello.swap(false, Ordering::Relaxed) {
            let greeting = agent::greeting(self.describe(console));
            self.send(greeting);
        }
        // One request, one response, in order: a command still owing frames
        // keeps the channel to itself until it is answered.
        for _ in 0..MAX_PER_PASS {
            if self.pending.is_some() || self.closed {
                break;
            }
            let Ok(line) = self.requests.try_recv() else { break };
            self.handle(&line, console);
        }
    }

    /// One frame was emulated. Sends the pending command's answer when that
    /// frame was the last one it owed.
    pub fn frame_elapsed(&mut self) {
        self.frame += 1;
        self.at_saved_state = false;
        let Some(pending) = &mut self.pending else { return };
        if let Some(answer) = pending.frame_elapsed(self.frame) {
            self.pending = None;
            self.send(answer);
        }
    }

    fn handle(&mut self, line: &str, console: &mut Console) {
        let (id, parsed) = agent::parse(line);
        match parsed {
            Err(e) => self.send(agent::error(&id, e)),
            Ok(Request::Step { frames }) => self.arm(id, Vec::new(), frames, 0),
            Ok(Request::Press { buttons, frames, release }) => {
                self.arm(id, buttons, frames, release)
            }
            Ok(Request::Quit) => {
                let answer = agent::reply(&id, json!({ "frame": self.frame }));
                self.send(answer);
                self.closed = true;
            }
            Ok(req) => match self.exec(req, console) {
                Ok(body) => self.send(agent::reply(&id, body)),
                Err(e) => self.send(agent::error(&id, e)),
            },
        }
    }

    /// Take charge of a `step`/`press`. A request that asks for no frame at all
    /// (`"frames":0`) is answered on the spot rather than waiting for a frame
    /// nobody will run.
    fn arm(&mut self, id: Value, buttons: Vec<String>, frames: u32, release: u32) {
        let pending = match Pending::new(id.clone(), buttons, frames, release) {
            Ok(p) => p,
            Err(e) => {
                let answer = agent::error(&id, format!("{e} (known: {})", agent::BUTTONS.join(" ")));
                return self.send(answer);
            }
        };
        if !pending.owes_frames() {
            let answer = agent::reply(&id, pending.body(self.frame));
            return self.send(answer);
        }
        self.pending = Some(pending);
    }

    /// Everything that costs no frame. Same commands, same bodies as
    /// `agent::Session::exec` — carried out on the console in the window.
    fn exec(&mut self, req: Request, console: &mut Console) -> Result<Value, String> {
        match req {
            Request::Screenshot { path } => {
                let path = self.out_path(path, &format!("frame_{:05}", self.frame), "png")?;
                crate::write_frame_png(console.snes, &path)?;
                Ok(json!({
                    "path": agent::display_path(&path),
                    "frame": self.frame,
                    "width": SCREEN_WIDTH,
                    "height": SCREEN_HEIGHT,
                }))
            }
            Request::ReadMem { addr, len } => {
                agent::check_range(addr, len)?;
                let mut hex = String::with_capacity(len as usize * 2);
                for i in 0..len {
                    let b = console.snes.bus.read_no_tick(agent::step_addr(addr, i));
                    hex.push_str(&format!("{b:02X}"));
                }
                Ok(json!({
                    "addr": agent::format_addr(addr),
                    "len": len,
                    "hex": hex,
                    "frame": self.frame,
                }))
            }
            Request::WriteMem { addr, bytes } => {
                agent::check_range(addr, bytes.len() as u32)?;
                for (i, b) in bytes.iter().enumerate() {
                    console.snes.bus.write_no_tick(agent::step_addr(addr, i as u32), *b);
                }
                self.at_saved_state = false;
                Ok(json!({
                    "addr": agent::format_addr(addr),
                    "len": bytes.len(),
                    "frame": self.frame,
                }))
            }
            Request::SaveState { path } => {
                let path = self.out_path(path, &format!("state_{:05}", self.frame), "state")?;
                let bytes = console.snes.save_state();
                crate::atomic::write(&path, &bytes)?;
                // Keyed by the resolved path, like the stdio channel: a state
                // saved as `s.state` and reloaded as `./s.state` is the same
                // one, and so is one reached through a symlinked temp folder.
                let path = agent::resolve(&path);
                self.states.insert(path.clone(), self.frame);
                self.last_state = Some(path.clone());
                self.at_saved_state = true;
                Ok(json!({
                    "path": agent::display_path(&path),
                    "bytes": bytes.len(),
                    "frame": self.frame,
                }))
            }
            Request::LoadState { path } => {
                let path = path
                    .or_else(|| self.last_state.clone())
                    .ok_or("no save state taken this session; pass \"path\"")?;
                let bytes = std::fs::read(&path)
                    .map_err(|e| format!("read {}: {e}", path.display()))?;
                console.snes.load_state(&bytes)?;
                // The state blob replaced the whole cart, SRAM included, and
                // the restored console no longer holds what a `once` cheat
                // wrote — both are true in the window too (see
                // `video::App::load_state`).
                *console.sram_baseline = console.snes.bus.cart.sram.as_bytes().to_vec();
                console.cheats.rearm();
                let path = agent::resolve(&path);
                let restored = self.states.get(&path).copied();
                if let Some(f) = restored {
                    self.frame = f;
                }
                self.last_state = Some(path.clone());
                self.at_saved_state = true;
                Ok(json!({
                    "path": agent::display_path(&path),
                    "frame": self.frame,
                    "frame_restored": restored.is_some(),
                }))
            }
            Request::CheatList => Ok(self.cheat_state(json!({}), console)),
            Request::CheatAdd { cheat } => {
                let cheat = *cheat;
                let value = agent::cheat_json(&cheat);
                let replaced = console.cheats.add(cheat);
                console.cheats.save(console.paths)?;
                console.cheats_changed = true;
                Ok(self.cheat_state(json!({ "cheat": value, "replaced": replaced }), console))
            }
            Request::CheatRemove { name } => {
                if !console.cheats.remove(&name) {
                    return Err(agent::unknown_cheat(&name, console.cheats));
                }
                console.cheats.save(console.paths)?;
                console.cheats_changed = true;
                Ok(self.cheat_state(json!({ "removed": name }), console))
            }
            Request::CheatEnable { name, enabled } => {
                let value = match console.cheats.set_enabled(&name, enabled) {
                    Some(cheat) => agent::cheat_json(cheat),
                    None => return Err(agent::unknown_cheat(&name, console.cheats)),
                };
                console.cheats.save(console.paths)?;
                console.cheats_changed = true;
                Ok(self.cheat_state(json!({ "cheat": value }), console))
            }
            Request::State => Ok(self.describe(console)),
            Request::Ping => Ok(json!({ "frame": self.frame })),
            Request::Help => Ok(json!({ "commands": agent::HELP, "buttons": agent::BUTTONS })),
            // Handled by the caller: they close the channel, which `exec` has
            // no way to say.
            Request::Step { .. } | Request::Press { .. } | Request::Quit => {
                unreachable!("frames and quit are handled by `handle`")
            }
        }
    }

    fn cheat_state(&self, fields: Value, console: &Console) -> Value {
        agent::cheat_state(fields, console.cheats, &console.paths.cheats_write())
    }

    fn describe(&self, console: &Console) -> Value {
        agent::describe(agent::Snapshot {
            snes: console.snes,
            rom: console.rom,
            frame: self.frame,
            at_saved_state: self.at_saved_state,
            cheats: console.cheats.list().len(),
            last_state: self
                .last_state
                .as_deref()
                .map(|p| (p, self.states.get(p).copied())),
            out_dir: &self.out_dir,
        })
    }

    /// The path a capture goes to: the one the caller named, or a fresh one
    /// under `out_dir`.
    fn out_path(&self, given: Option<PathBuf>, stem: &str, ext: &str) -> Result<PathBuf, String> {
        match given {
            Some(p) => {
                crate::create_parent_dir(&p)?;
                Ok(p)
            }
            None => {
                std::fs::create_dir_all(&self.out_dir)
                    .map_err(|e| format!("create {}: {e}", self.out_dir.display()))?;
                Ok(crate::unique_path(&self.out_dir, stem, ext))
            }
        }
    }

    /// Queue one answer. Dropped silently when nobody is connected any more —
    /// the session is over and there is nobody left to tell.
    fn send(&self, v: Value) {
        let _ = self.replies.send(v.to_string());
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        // A reader thread parked in `read` does not wake on a flag: shutting
        // the socket down is what ends it.
        if let Ok(stream) = self.stream.lock() {
            if let Some(stream) = stream.as_ref() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

/// Accept one client at a time, check its secret, then act as the write half of
/// the connection while a thread of its own reads it.
fn serve(
    listener: &TcpListener,
    secret: &str,
    requests: &Sender<String>,
    replies: &Receiver<String>,
    shutdown: &Arc<AtomicBool>,
    hello: &Arc<AtomicBool>,
    shared: &Arc<Mutex<Option<TcpStream>>>,
) {
    while !shutdown.load(Ordering::Relaxed) {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(POLL);
                continue;
            }
            Err(_) => return,
        };
        if stream.set_nonblocking(false).is_err() {
            continue;
        }
        match handshake(&stream, secret) {
            Ok(()) => {}
            Err(e) => {
                // Said out loud rather than dropped in silence: a client with
                // the wrong secret is nearly always the right client started
                // the wrong way, and a connection that closes without a word
                // is the least debuggable failure there is.
                let mut w = &stream;
                let _ = writeln!(w, "{}", agent::error(&Value::Null, e));
                let _ = w.flush();
                continue;
            }
        }
        let Ok(reader) = stream.try_clone() else { continue };
        // Kept so `Server::drop` can shut the socket down: a thread parked in
        // `read` is not woken by a flag.
        if let (Ok(mut slot), Ok(handle)) = (shared.lock(), stream.try_clone()) {
            *slot = Some(handle);
        }
        let alive = Arc::new(AtomicBool::new(true));
        let done = Arc::clone(&alive);
        let tx = requests.clone();
        let reading = std::thread::Builder::new()
            .name("prisme-agent-read".to_string())
            .spawn(move || {
                for line in BufReader::new(reader).lines() {
                    let Ok(line) = line else { break };
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                done.store(false, Ordering::Relaxed);
            });
        if reading.is_err() {
            continue;
        }
        hello.store(true, Ordering::Relaxed);
        write_answers(&stream, replies, shutdown, &alive);
        let _ = stream.shutdown(std::net::Shutdown::Both);
        if let Ok(mut slot) = shared.lock() {
            *slot = None;
        }
    }
}

/// First line or nothing: the shared secret, exactly as the application handed
/// it to the assistant.
///
/// Read one byte at a time, deliberately: a `BufReader` here would swallow up
/// to 8 KB of whatever followed the newline, and the first command of the
/// session would vanish into a buffer the reader thread never sees.
fn handshake(stream: &TcpStream, secret: &str) -> Result<(), String> {
    stream
        .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
        .map_err(|e| format!("set the handshake timeout: {e}"))?;
    let mut line = Vec::with_capacity(secret.len() + 2);
    let mut byte = [0u8; 1];
    let mut source = stream;
    loop {
        match std::io::Read::read(&mut source, &mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => line.push(byte[0]),
            Err(e) => return Err(format!("read the secret: {e}")),
        }
        // A client that sends an endless first line is not one of ours.
        if line.len() > secret.len() + 8 {
            break;
        }
    }
    let line = String::from_utf8_lossy(&line);
    // Constant-time comparison is beside the point on a port whose whole
    // lifetime is one request, but a length check before the bytes is free.
    let given = line.trim_end_matches(['\r', '\n']);
    if given.len() != secret.len() || given != secret {
        return Err(format!(
            "wrong or missing secret on the first line; pass the one the application printed (env {SECRET_VAR})"
        ));
    }
    stream.set_read_timeout(None).map_err(|e| format!("clear the read timeout: {e}"))?;
    Ok(())
}

/// Drain the answer queue onto the socket until the client hangs up or the
/// session ends.
fn write_answers(
    stream: &TcpStream,
    replies: &Receiver<String>,
    shutdown: &Arc<AtomicBool>,
    alive: &Arc<AtomicBool>,
) {
    let mut out = stream;
    while alive.load(Ordering::Relaxed) && !shutdown.load(Ordering::Relaxed) {
        match replies.recv_timeout(POLL) {
            Ok(line) => {
                if writeln!(out, "{line}").is_err() || out.flush().is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The `Server` was dropped: there is nothing left to answer with.
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// 128 bits, from the OS-seeded hasher `std` already carries plus the clock and
/// the process id.
///
/// Deliberately not a cryptographic construction, and it does not need to be:
/// it guards a loopback port that exists for the length of one request, against
/// another process on the same machine happening upon it. Anything running as
/// the player's own user can do worse than this to them anyway.
fn new_secret() -> String {
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut out = String::with_capacity(32);
    for round in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(nanos);
        h.write_u32(std::process::id());
        h.write_u64(round);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// `--agent-attach HOST:PORT`: be the client, so the assistant's command line
/// stays one line and the protocol stays exactly what `agent.rs` documents.
/// Everything typed on stdin goes to the socket, everything the socket answers
/// goes to stdout — no parsing, no dialect of its own.
pub fn attach(addr: &str, secret: Option<&str>) -> Result<(), String> {
    let secret = match secret {
        Some(s) => s.to_string(),
        None => std::env::var(SECRET_VAR).map_err(|_| {
            format!("no secret: set {SECRET_VAR} or pass --agent-secret (the application gives you one)")
        })?,
    };
    let addr: SocketAddr = addr
        .parse()
        .map_err(|e| format!("--agent-attach wants HOST:PORT like 127.0.0.1:50000 ({e})"))?;
    // Nothing about this channel is meant to leave the machine, and an address
    // that is not loopback is a mistake worth refusing rather than honoring.
    if !addr.ip().is_loopback() {
        return Err(format!("--agent-attach only connects to loopback, not {}", addr.ip()));
    }
    let stream = TcpStream::connect(addr).map_err(|e| format!("connect to {addr}: {e}"))?;
    let mut to_socket =
        stream.try_clone().map_err(|e| format!("split the connection: {e}"))?;
    writeln!(to_socket, "{secret}").map_err(|e| format!("send the secret: {e}"))?;
    to_socket.flush().map_err(|e| format!("send the secret: {e}"))?;

    std::thread::Builder::new()
        .name("prisme-attach-stdin".to_string())
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if writeln!(to_socket, "{line}").is_err() || to_socket.flush().is_err() {
                    break;
                }
            }
            // Closing our half tells the application the assistant is done,
            // which is how it lets go of the player's console.
            let _ = to_socket.shutdown(std::net::Shutdown::Write);
        })
        .map_err(|e| format!("could not start the input thread: {e}"))?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in BufReader::new(stream).lines() {
        let line = line.map_err(|e| format!("read the channel: {e}"))?;
        writeln!(out, "{line}").map_err(|e| format!("write stdout: {e}"))?;
        out.flush().map_err(|e| format!("flush stdout: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use snes_core::Cartridge;

    /// Minimal PAL LoROM cart, like `agent`'s: enough header to map, and 64 KB
    /// of $00 so `run_frame` still terminates.
    fn test_snes() -> Snes {
        let mut rom = vec![0u8; 0x10000];
        rom[0x7FC0..0x7FC0 + 21].copy_from_slice(b"LIVE TEST            ");
        rom[0x7FC0 + 0x15] = 0x20;
        rom[0x7FC0 + 0x19] = 2; // PAL
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        Snes::new(Cartridge::from_bytes(rom).expect("cart"))
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("prisme_live_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// The session the event loop would be holding: a console, its cheats and
    /// its sidecar layout, plus the frame counter a caller would be watching.
    struct Harness {
        server: Server,
        snes: Snes,
        cheats: Cheats,
        paths: GamePaths,
        rom: PathBuf,
        sram_baseline: Vec<u8>,
        /// Frames actually emulated, counted by the harness itself rather than
        /// read back from the server — this is the number the window would be
        /// drawing.
        frames_run: u64,
        /// Iterations of the fake event loop, so a command that ran its frames
        /// in a tight loop instead of across iterations is visible.
        iterations: u64,
        /// What player 1 was actually fed, frame by frame.
        pads: Vec<JoypadState>,
    }

    impl Harness {
        fn new(tag: &str) -> Harness {
            let dir = scratch(tag);
            let rom = dir.join("live-test.sfc");
            let paths = GamePaths::new(&rom, "LIVE_TEST-0000", Some(dir.clone()), None);
            Harness {
                server: Server::start(dir).expect("listener"),
                snes: test_snes(),
                cheats: Cheats::default(),
                paths,
                rom,
                sram_baseline: Vec::new(),
                frames_run: 0,
                iterations: 0,
                pads: Vec::new(),
            }
        }

        /// Exactly what `video::App::about_to_wait` does for the live channel,
        /// with the window and the pacing left out: drain the channel, then run
        /// **at most one frame**, then hand control back to the caller.
        fn iterate(&mut self) {
            self.iterations += 1;
            {
                let mut console = Console {
                    snes: &mut self.snes,
                    cheats: &mut self.cheats,
                    paths: &self.paths,
                    rom: &self.rom,
                    sram_baseline: &mut self.sram_baseline,
                    cheats_changed: false,
                };
                self.server.pump(&mut console);
            }
            if let Some(pad) = self.server.pad() {
                self.snes.run_frame([pad, JoypadState::default()]);
                self.cheats.apply(&mut self.snes);
                self.pads.push(pad);
                self.frames_run += 1;
                self.server.frame_elapsed();
            }
        }

        /// Turn the loop until an answer comes back, or give up. The budget is
        /// what proves the command finished when it was supposed to.
        ///
        /// An iteration that emulated nothing waits a millisecond, standing in
        /// for the frame pacing a real event loop does: without it this spins
        /// through its whole budget faster than the socket can deliver a line.
        fn run_until(&mut self, client: &mut Client, budget: u64) -> Option<Value> {
            for _ in 0..budget {
                let frames = self.frames_run;
                self.iterate();
                if let Some(v) = client.try_read() {
                    return Some(v);
                }
                if self.frames_run == frames {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            None
        }

        /// Turn the loop `n` times at the pace of a real one, expecting nothing.
        fn spin(&mut self, n: u64) {
            for _ in 0..n {
                self.iterate();
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    /// A client on the other end of the socket, speaking the protocol by hand.
    ///
    /// Its own reader lives on a thread so a test can ask "has it answered
    /// yet?" without a read timeout — one that fired mid-line would eat half an
    /// answer and make the test flaky for reasons that have nothing to do with
    /// what it is checking.
    struct Client {
        write: TcpStream,
        read: Receiver<String>,
    }

    impl Client {
        fn connect(server: &Server, secret: &str) -> Client {
            let stream = TcpStream::connect(SocketAddr::from((
                Ipv4Addr::LOCALHOST,
                server.port(),
            )))
            .expect("connect");
            let mut write = stream.try_clone().expect("clone");
            writeln!(write, "{secret}").expect("secret");
            write.flush().expect("flush");
            let (tx, read) = channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
            Client { write, read }
        }

        fn send(&mut self, line: &str) {
            writeln!(self.write, "{line}").expect("send");
            self.write.flush().expect("flush");
        }

        /// One answer, or `None` if none has arrived yet.
        fn try_read(&mut self) -> Option<Value> {
            let line = self.read.try_recv().ok()?;
            Some(serde_json::from_str(&line).expect("a JSON line"))
        }

        /// Block for one answer, turning the harness' loop while waiting.
        fn expect(&mut self, h: &mut Harness, budget: u64) -> Value {
            h.run_until(self, budget).expect("an answer")
        }
    }

    fn greeted(h: &mut Harness, c: &mut Client) -> Value {
        c.expect(h, 500)
    }

    #[test]
    fn a_connection_is_greeted_once_the_secret_checks_out() {
        let mut h = Harness::new("greeting");
        let secret = h.server.secret().to_string();
        let mut c = Client::connect(&h.server, &secret);
        let g = greeted(&mut h, &mut c);
        assert_eq!(g["event"], json!("ready"), "{g}");
        assert_eq!(g["title"], json!("LIVE TEST"));
        assert_eq!(g["frame"], json!(0));
        assert_eq!(g["protocol"], json!(agent::PROTOCOL_VERSION));
        // Not one frame was emulated by connecting: the console is the
        // player's, and it moves only when a command asks it to.
        assert_eq!(h.frames_run, 0);
    }

    #[test]
    fn a_connection_that_does_not_know_the_secret_is_refused() {
        let mut h = Harness::new("secret");
        let mut c = Client::connect(&h.server, "not-the-secret");
        // The refusal is a value, like every other error of this protocol, and
        // the connection is closed right after it.
        let v = c.expect(&mut h, 500);
        assert!(v["error"].as_str().expect("an error").contains("secret"), "{v}");
        h.spin(30);
        assert!(c.try_read().is_none(), "the connection should have been closed");
        // Nothing it sent is ever executed.
        let _ = writeln!(c.write, r#"{{"cmd":"step","frames":1}}"#);
        h.spin(30);
        assert_eq!(h.frames_run, 0, "a refused client must not drive the console");

        // …and the door is still open for the real one.
        let secret = h.server.secret().to_string();
        let mut good = Client::connect(&h.server, &secret);
        let g = greeted(&mut h, &mut good);
        assert_eq!(g["event"], json!("ready"), "{g}");
    }

    /// The heart of it: 300 frames, one per iteration of the loop, answered
    /// after the last one and not before.
    #[test]
    fn a_step_spans_one_frame_per_iteration_and_answers_only_at_the_end() {
        let mut h = Harness::new("step300");
        let secret = h.server.secret().to_string();
        let mut c = Client::connect(&h.server, &secret);
        greeted(&mut h, &mut c);
        let before = h.frames_run;
        c.send(r#"{"id":1,"cmd":"step","frames":300}"#);

        // Let the request arrive, then watch it spend its frames.
        let mut ran = 0;
        for _ in 0..300 {
            let frames = h.frames_run;
            h.iterate();
            if h.frames_run > frames {
                ran += 1;
                // One frame per iteration: never two, never a burst.
                assert_eq!(h.frames_run - frames, 1);
            }
            if ran > 0 && ran < 300 {
                assert!(h.server.owes_frames(), "the command is still in flight");
                assert!(c.try_read().is_none(), "answered after {ran} frames of 300");
            }
        }
        // The first iteration or two only carried the request across the
        // socket; keep turning until the 300th frame is spent.
        let v = c.expect(&mut h, 400);
        assert_eq!(v["id"], json!(1), "{v}");
        assert_eq!(v["frames"], json!(300));
        assert_eq!(h.frames_run - before, 300, "exactly 300 frames, no more");
        assert_eq!(v["frame"], json!(300));
        assert!(!h.server.owes_frames(), "nothing is owed once it is answered");

        // And the console stands still again until the next command.
        let idle = h.frames_run;
        h.spin(30);
        assert_eq!(h.frames_run, idle, "no command, no frame");
    }

    /// The proof the window keeps moving: draining a `step 300` hands control
    /// back to the caller on every single frame. If any of it ran in a tight
    /// loop, the iteration count would not match the frame count.
    #[test]
    fn draining_a_long_step_yields_to_the_caller_on_every_frame() {
        let mut h = Harness::new("yield");
        let secret = h.server.secret().to_string();
        let mut c = Client::connect(&h.server, &secret);
        greeted(&mut h, &mut c);
        c.send(r#"{"cmd":"step","frames":300}"#);
        let iterations_before = h.iterations;
        let frames_before = h.frames_run;
        let v = c.expect(&mut h, 400);
        assert_eq!(v["frames"], json!(300), "{v}");
        let frames = h.frames_run - frames_before;
        let iterations = h.iterations - iterations_before;
        assert_eq!(frames, 300);
        // Printed so the numbers can be read rather than taken on trust
        // (`cargo test -- --nocapture`).
        eprintln!("step 300: {frames} frames over {iterations} iterations of the loop");
        // One iteration per frame, plus the handful spent waiting for the
        // request to cross the socket. The point is the *lower* bound: the
        // caller was given the chance to draw, poll input and play audio 300
        // times, which is what "the window keeps moving" means here.
        assert!(iterations >= frames, "{iterations} iterations for {frames} frames");
        assert!(iterations < frames + 50, "{iterations} iterations for {frames} frames");
    }

    #[test]
    fn a_press_holds_its_buttons_then_releases_them() {
        let mut h = Harness::new("press");
        let secret = h.server.secret().to_string();
        let mut c = Client::connect(&h.server, &secret);
        greeted(&mut h, &mut c);
        c.send(r#"{"id":"p","cmd":"press","buttons":["B","Right"],"frames":4,"release":3}"#);
        let v = c.expect(&mut h, 500);
        assert_eq!(v["id"], json!("p"), "{v}");
        assert_eq!(v["frames"], json!(4));
        assert_eq!(v["release"], json!(3));
        assert_eq!(v["buttons"], json!(["B", "Right"]));
        assert_eq!(v["frame"], json!(7), "4 held + 3 released = 7 frames");

        // What the console was actually fed, frame by frame: held for four,
        // then let go for three, then nothing at all.
        let mut expected = JoypadState::default();
        input::set_button(&mut expected, "B", true).expect("B");
        input::set_button(&mut expected, "Right", true).expect("Right");
        assert_eq!(h.frames_run, 7);
        assert_eq!(&h.pads[..4], &[expected; 4], "the buttons are held for the frames asked for");
        assert_eq!(&h.pads[4..], &[JoypadState::default(); 3], "then let go, for exactly three");
        h.spin(20);
        assert_eq!(h.frames_run, 7, "and the console stops there");
    }

    #[test]
    fn an_unknown_command_is_an_error_and_the_channel_stays_open() {
        let mut h = Harness::new("unknown");
        let secret = h.server.secret().to_string();
        let mut c = Client::connect(&h.server, &secret);
        greeted(&mut h, &mut c);
        for line in ["{\"id\":1,\"cmd\":\"fly\"}", "not json", "", "{\"cmd\":\"press\"}"] {
            c.send(line);
            let v = c.expect(&mut h, 500);
            assert!(v["error"].is_string(), "{line} -> {v}");
        }
        // Still usable, and still able to drive the console afterwards.
        c.send(r#"{"cmd":"ping"}"#);
        assert_eq!(c.expect(&mut h, 500)["ok"], json!(true));
        c.send(r#"{"cmd":"step","frames":2}"#);
        let v = c.expect(&mut h, 500);
        assert_eq!(v["frame"], json!(2), "{v}");
        assert_eq!(h.frames_run, 2);
        // An unknown *button* is refused before a frame is run, too.
        c.send(r#"{"cmd":"press","button":"Turbo"}"#);
        let v = c.expect(&mut h, 500);
        assert!(v["error"].is_string(), "{v}");
        assert_eq!(h.frames_run, 2, "a refused press emulates nothing");
    }

    /// Observation costs no frame, here as on the stdio channel — which is what
    /// lets an assistant look at the screen without the game moving under it.
    #[test]
    fn looking_at_the_console_never_advances_it() {
        let mut h = Harness::new("observe");
        let secret = h.server.secret().to_string();
        let mut c = Client::connect(&h.server, &secret);
        greeted(&mut h, &mut c);
        c.send(r#"{"cmd":"write-mem","addr":"7E:0100","hex":"DEADBEEF"}"#);
        assert_eq!(c.expect(&mut h, 500)["len"], json!(4));
        c.send(r#"{"cmd":"read-mem","addr":"7E:0100","len":4}"#);
        assert_eq!(c.expect(&mut h, 500)["hex"], json!("DEADBEEF"));
        c.send(r#"{"cmd":"screenshot"}"#);
        let v = c.expect(&mut h, 500);
        let shot = PathBuf::from(v["path"].as_str().expect("a path"));
        assert_eq!(&std::fs::read(&shot).expect("png")[..8], b"\x89PNG\r\n\x1a\n");
        c.send(r#"{"cmd":"state"}"#);
        let v = c.expect(&mut h, 500);
        assert_eq!(v["frame"], json!(0), "{v}");
        assert_eq!(h.frames_run, 0);
    }

    /// The way back: a state taken on the live console restores the live
    /// console, and puts the frame counter back with it.
    #[test]
    fn a_save_state_round_trips_on_the_players_own_console() {
        let mut h = Harness::new("state");
        let secret = h.server.secret().to_string();
        let mut c = Client::connect(&h.server, &secret);
        greeted(&mut h, &mut c);
        c.send(r#"{"cmd":"write-mem","addr":"7E:0200","hex":"11"}"#);
        c.expect(&mut h, 500);
        c.send(r#"{"cmd":"step","frames":2}"#);
        c.expect(&mut h, 500);
        c.send(r#"{"cmd":"save-state"}"#);
        let saved = c.expect(&mut h, 500);
        let path = saved["path"].as_str().expect("a path").to_string();
        assert_eq!(saved["frame"], json!(2), "{saved}");

        c.send(r#"{"cmd":"write-mem","addr":"7E:0200","hex":"22"}"#);
        c.expect(&mut h, 500);
        c.send(r#"{"cmd":"step","frames":3}"#);
        assert_eq!(c.expect(&mut h, 500)["frame"], json!(5));

        c.send(&format!(r#"{{"cmd":"load-state","path":"{path}"}}"#));
        let v = c.expect(&mut h, 500);
        assert_eq!(v["frame"], json!(2), "{v}");
        assert_eq!(v["frame_restored"], json!(true));
        c.send(r#"{"cmd":"read-mem","addr":"7E:0200","len":1}"#);
        assert_eq!(c.expect(&mut h, 500)["hex"], json!("11"));
        // The console the assistant restored is the one in the window.
        assert_eq!(h.snes.bus.read_no_tick(0x7E_0200), 0x11);
    }

    /// A cheat found on the live channel lands in the game's own sidecar and is
    /// held on the running console — the handover the feature exists for.
    #[test]
    fn a_cheat_added_on_the_live_channel_reaches_the_running_game() {
        let mut h = Harness::new("cheat");
        let secret = h.server.secret().to_string();
        let mut c = Client::connect(&h.server, &secret);
        greeted(&mut h, &mut c);
        c.send(r#"{"cmd":"cheat-add","name":"Vies","addr":"7E:0DBE","hex":"63"}"#);
        let v = c.expect(&mut h, 500);
        assert_eq!(v["count"], json!(1), "{v}");
        assert!(Path::new(v["path"].as_str().expect("a path")).is_file());
        c.send(r#"{"cmd":"write-mem","addr":"7E:0DBE","hex":"02"}"#);
        c.expect(&mut h, 500);
        c.send(r#"{"cmd":"step","frames":1}"#);
        c.expect(&mut h, 500);
        assert_eq!(h.snes.bus.read_no_tick(0x7E_0DBE), 0x63, "the frozen value is held");
    }

    #[test]
    fn quit_answers_and_closes_the_channel() {
        let mut h = Harness::new("quit");
        let secret = h.server.secret().to_string();
        let mut c = Client::connect(&h.server, &secret);
        greeted(&mut h, &mut c);
        assert!(!h.server.closed());
        c.send(r#"{"id":9,"cmd":"quit"}"#);
        let v = c.expect(&mut h, 500);
        assert_eq!(v["id"], json!(9), "{v}");
        assert!(h.server.closed(), "the shell must let go of the channel");
    }

    /// The state machine on its own, with no socket and no console: this is the
    /// piece that must never run frames of its own.
    #[test]
    fn the_pending_command_counts_frames_and_nothing_else() {
        let mut p = Pending::new(json!(1), Vec::new(), 3, 0).expect("a step");
        assert_eq!(p.pad(), JoypadState::default());
        assert_eq!(p.frame_elapsed(1), None);
        assert_eq!(p.frame_elapsed(2), None);
        let answer = p.frame_elapsed(3).expect("answered on the third frame");
        assert_eq!(answer["frames"], json!(3));
        assert_eq!(answer["frame"], json!(3));
        assert!(!p.owes_frames());

        // A press: held, then released, then answered.
        let mut p = Pending::new(json!(null), vec!["A".into()], 2, 2).expect("a press");
        let held = p.pad();
        assert_ne!(held, JoypadState::default());
        assert_eq!(p.frame_elapsed(1), None);
        assert_eq!(p.pad(), held, "still held on the second frame");
        assert_eq!(p.frame_elapsed(2), None);
        assert_eq!(p.pad(), JoypadState::default(), "let go for the release");
        assert_eq!(p.frame_elapsed(3), None);
        let answer = p.frame_elapsed(4).expect("answered after 2+2 frames");
        // No id was given, so none comes back (never a null one).
        assert!(answer.get("id").is_none(), "{answer}");
        assert_eq!(answer["buttons"], json!(["A"]));

        // An unknown button never becomes a pending command.
        assert!(Pending::new(json!(1), vec!["Turbo".into()], 1, 0).is_err());
    }

    #[test]
    fn a_command_that_asks_for_no_frame_is_answered_at_once() {
        let mut h = Harness::new("zero");
        let secret = h.server.secret().to_string();
        let mut c = Client::connect(&h.server, &secret);
        greeted(&mut h, &mut c);
        c.send(r#"{"cmd":"step","frames":0}"#);
        let v = c.expect(&mut h, 500);
        assert_eq!(v["frames"], json!(0), "{v}");
        assert_eq!(h.frames_run, 0);
        assert!(!h.server.owes_frames(), "nothing may stay armed");
    }

    #[test]
    fn attaching_to_anything_but_loopback_is_refused() {
        assert!(attach("10.0.0.1:5000", Some("x")).unwrap_err().contains("loopback"));
        assert!(attach("not-an-address", Some("x")).unwrap_err().contains("HOST:PORT"));
    }

    #[test]
    fn two_secrets_are_never_the_same() {
        let a = new_secret();
        let b = new_secret();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
