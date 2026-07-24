//! snes-frontend CLI. Contract lives in .claude/skills/snes-build-test/SKILL.md
//! — keep the two in sync.

mod audio;
mod input;
mod menu;
mod picker;
mod prefs;
mod save;
mod spc;
mod state;
mod video;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use snes_core::{Cartridge, JoypadState, Mapping, Region, Snes, SCREEN_HEIGHT, SCREEN_WIDTH};

/// Product name: `Prisme` is the platform, `SuperNes` the emulated console.
/// Used by `--version`, the window title and the macOS About panel.
pub const APP_NAME: &str = "Prisme - SuperNes";
/// Version of the `prisme` package (frontend/Cargo.toml).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Default)]
struct Args {
    rom: Option<PathBuf>,
    info: bool,
    version: bool,
    disasm: bool,
    addr: Option<(u8, u16)>,
    count: u32,
    headless: bool,
    frames: u32,
    dump_frame: Option<PathBuf>,
    dump_frame_every: Option<u32>,
    dump_dir: Option<PathBuf>,
    trace: Option<PathBuf>,
    trace_spc: Option<PathBuf>,
    trace_gsu: Option<PathBuf>,
    trace_sa1: Option<PathBuf>,
    trace_start_frame: u32,
    trace_end_frame: u32,
    log_mmio: bool,
    watch: Vec<(u8, u16)>,
    script: Option<PathBuf>,
    dump_state: Option<PathBuf>,
    dump_audio: Option<PathBuf>,
    save: Option<PathBuf>,
    save_state_at: Option<(u32, PathBuf)>,
    load_state: Option<PathBuf>,
    dump_spc: Option<PathBuf>,
}

const USAGE: &str = "usage: prisme [rom.sfc|.smc|.zip] [flags]
  <rom> omitted, windowed mode          open a native file-open dialog to pick a ROM
  --version                             print the application name and version, then exit
  --info                                print header info and exit
  --disasm [--addr BB:AAAA] [--count N] disassemble and exit
  --headless --frames N                 emulate N frames without a window
  --dump-frame PATH.png                 write final framebuffer as PNG on exit
  --dump-frame-every N --dump-dir DIR   write DIR/frame_XXXXX.png every N frames
  --trace PATH [--trace-start-frame A --trace-end-frame B]
  --trace-spc PATH                      SPC700 trace, same bounds
  --trace-gsu PATH                      GSU/SuperFX trace, same bounds (needs a SuperFX cart)
  --trace-sa1 PATH                      SA-1 65C816 trace, same bounds (needs an SA-1 cart)
  --log-mmio                            log named MMIO writes to stderr
  --watch BB:AAAA                       log every read/write at a bus address
  --script PATH                         input script: <frame> <button> <held>
  --dump-state DIR                      dump wram/vram/cgram/oam/apuram on exit
  --dump-audio PATH.wav                 headless: write 32kHz 16-bit stereo WAV
  --dump-spc PATH.spc                   write the APU state as an .spc music file on exit
  --save PATH                           battery SRAM file (default: <rom>.srm)
  --load-state FILE                     headless: load a save-state before frame 0
  --save-state-at FRAME FILE            headless: write a save-state after FRAME";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut parsed = match parse_args(&args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    // `--version` answers before anything else, including the ROM picker: it
    // must stay usable with no ROM argument and no display.
    if parsed.version {
        println!("{APP_NAME} {VERSION}");
        return ExitCode::SUCCESS;
    }
    // No ROM path and not --headless: pick one with a native file dialog
    // before building anything else, so the rest of `run` sees a ROM path
    // exactly as if it had been passed on the command line. rfd's dialog
    // must run on the main thread on macOS; this is the main thread and no
    // window/event loop exists yet, so the constraint is trivially met.
    if parsed.rom.is_none() {
        match picker::pick_rom() {
            Some(path) => parsed.rom = Some(path),
            None => {
                println!("No ROM selected; exiting.");
                return ExitCode::SUCCESS;
            }
        }
    }
    match run(parsed) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut a = Args { count: 30, frames: 1, trace_end_frame: u32::MAX, ..Args::default() };
    let mut it = args.iter().peekable();
    let value = |it: &mut std::iter::Peekable<std::slice::Iter<String>>,
                     flag: &str|
     -> Result<String, String> {
        it.next().cloned().ok_or_else(|| format!("{flag} requires a value"))
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--version" | "-V" => a.version = true,
            "--info" => a.info = true,
            "--disasm" => a.disasm = true,
            "--addr" => a.addr = Some(parse_bus_addr(&value(&mut it, "--addr")?)?),
            "--count" => a.count = parse_num(&value(&mut it, "--count")?)?,
            "--headless" => a.headless = true,
            "--frames" => a.frames = parse_num(&value(&mut it, "--frames")?)?,
            "--dump-frame" => a.dump_frame = Some(value(&mut it, "--dump-frame")?.into()),
            "--dump-frame-every" => {
                a.dump_frame_every = Some(parse_num(&value(&mut it, "--dump-frame-every")?)?)
            }
            "--dump-dir" => a.dump_dir = Some(value(&mut it, "--dump-dir")?.into()),
            "--trace" => a.trace = Some(value(&mut it, "--trace")?.into()),
            "--trace-spc" => a.trace_spc = Some(value(&mut it, "--trace-spc")?.into()),
            "--trace-gsu" => a.trace_gsu = Some(value(&mut it, "--trace-gsu")?.into()),
            "--trace-sa1" => a.trace_sa1 = Some(value(&mut it, "--trace-sa1")?.into()),
            "--trace-start-frame" => {
                a.trace_start_frame = parse_num(&value(&mut it, "--trace-start-frame")?)?
            }
            "--trace-end-frame" => {
                a.trace_end_frame = parse_num(&value(&mut it, "--trace-end-frame")?)?
            }
            "--log-mmio" => a.log_mmio = true,
            "--watch" => a.watch.push(parse_bus_addr(&value(&mut it, "--watch")?)?),
            "--script" => a.script = Some(value(&mut it, "--script")?.into()),
            "--dump-state" => a.dump_state = Some(value(&mut it, "--dump-state")?.into()),
            "--dump-audio" => a.dump_audio = Some(value(&mut it, "--dump-audio")?.into()),
            "--dump-spc" => a.dump_spc = Some(value(&mut it, "--dump-spc")?.into()),
            "--save" => a.save = Some(value(&mut it, "--save")?.into()),
            "--load-state" => a.load_state = Some(value(&mut it, "--load-state")?.into()),
            "--save-state-at" => {
                let frame = parse_num(&value(&mut it, "--save-state-at")?)?;
                let file = value(&mut it, "--save-state-at")?;
                a.save_state_at = Some((frame, file.into()));
            }
            "--help" | "-h" => return Err("help requested".into()),
            s if s.starts_with("--") => return Err(format!("unknown flag: {s}")),
            _ => {
                if a.rom.is_some() {
                    return Err(format!("unexpected positional argument: {arg}"));
                }
                a.rom = Some(arg.into());
            }
        }
    }
    // A missing ROM path is only an error in --headless mode (there is no
    // window to attach a file dialog to, and every headless flag needs cart
    // data to act on). In windowed mode `main` opens a file-open dialog
    // instead of failing here.
    // `--version` prints and exits before any cart is touched, so it is
    // exempt from that requirement.
    if a.rom.is_none() && a.headless && !a.version {
        return Err("no ROM path given".into());
    }
    Ok(a)
}

fn parse_num(s: &str) -> Result<u32, String> {
    s.parse().map_err(|_| format!("invalid number: {s}"))
}

/// Parse `BB:AAAA` (hex bank : hex 16-bit address).
fn parse_bus_addr(s: &str) -> Result<(u8, u16), String> {
    let (bank, addr) = s.split_once(':').ok_or_else(|| format!("expected BB:AAAA, got {s}"))?;
    let bank = u8::from_str_radix(bank, 16).map_err(|_| format!("bad bank in {s}"))?;
    let addr = u16::from_str_radix(addr, 16).map_err(|_| format!("bad address in {s}"))?;
    Ok((bank, addr))
}

fn run(args: Args) -> Result<(), String> {
    let rom_path = args.rom.as_ref().unwrap();
    let bytes = load_rom_bytes(rom_path)?;
    let mut cart = Cartridge::from_bytes(bytes)?;

    // Sidecar SRAM save: loaded before Snes::new so the game's own init code
    // (which typically reads its save-flag byte early) sees restored state.
    // `sram_baseline` is the post-load snapshot; save::save_if_dirty diffs
    // against it on exit so an untouched cart is never rewritten.
    let save_path = args.save.clone().unwrap_or_else(|| save::default_save_path(rom_path));
    let sram_baseline = save::load_sram(&mut cart, &save_path);

    if args.info {
        print_info(&cart);
        return Ok(());
    }
    if args.disasm {
        return run_disasm(cart, &args);
    }
    if args.trace_spc.is_some() && !args.headless {
        eprintln!("--trace-spc requires --headless; ignoring");
    }
    if args.trace_gsu.is_some() && !args.headless {
        eprintln!("--trace-gsu requires --headless; ignoring");
    }
    if args.trace_sa1.is_some() && !args.headless {
        eprintln!("--trace-sa1 requires --headless; ignoring");
    }

    // Preferences are read on both paths, so a malformed file is reported the
    // same way everywhere; `persist` is set only for the windowed run, so an
    // automated headless run never writes the user's file back. No preference
    // takes part in the CLI contract — headless behavior is unchanged.
    let prefs = prefs::Prefs::load(!args.headless);

    if !args.headless {
        if args.dump_audio.is_some() {
            eprintln!("--dump-audio requires --headless; ignoring (windowed mode plays live)");
        }
        if args.dump_spc.is_some() {
            eprintln!("--dump-spc requires --headless; ignoring (use Fichier > Exporter la musique)");
        }
        return video::run(rom_path.clone(), cart, save_path, sram_baseline, prefs);
    }

    let script = match &args.script {
        Some(path) => parse_script(path)?,
        None => BTreeMap::new(),
    };

    let cart_has_gsu = cart.superfx.is_some();
    let cart_has_sa1 = cart.sa1.is_some();
    let cart_title = cart.title.trim().to_string();
    let mut snes = Snes::new(cart);
    if let Some(path) = &args.load_state {
        let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        snes.load_state(&bytes)?;
        eprintln!("state: loaded {}", path.display());
    }
    if let Some(dir) = &args.dump_dir {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }

    // Arm the core debug taps (stderr) requested on the command line.
    snes.bus.debug.log_mmio = args.log_mmio;
    snes.bus.debug.watch =
        args.watch.iter().map(|&(b, a)| ((b as u32) << 16) | a as u32).collect();

    let mut trace_writer = match &args.trace {
        Some(path) => Some(open_trace(path)?),
        None => None,
    };

    // SPC700 trace shares the CPU trace's frame bounds. The SPC runs lazily
    // inside APU `catch_up`, so the sink is installed on the Snes for the whole
    // frame range rather than driven per-instruction from here. A shared writer
    // lets the closure own a handle while `main` retains one to flush at the end.
    let spc_writer = match &args.trace_spc {
        Some(path) => Some(Rc::new(RefCell::new(open_trace(path)?))),
        None => None,
    };
    let mut spc_installed = false;

    // GSU trace shares the same frame bounds; the GSU runs lazily inside Bus
    // gsu_catch_up, so (like the SPC trace) the sink is installed on the Snes
    // for the whole frame range rather than driven per-instruction from here.
    if args.trace_gsu.is_some() && !cart_has_gsu {
        eprintln!("--trace-gsu: no SuperFX/GSU coprocessor in this cart; skipping");
    }
    let gsu_writer = match &args.trace_gsu {
        Some(path) if cart_has_gsu => Some(Rc::new(RefCell::new(open_trace(path)?))),
        _ => None,
    };
    let mut gsu_installed = false;

    // SA-1 trace: same frame bounds; the SA-1 CPU runs lazily inside Bus
    // sa1_catch_up, so the sink is installed on the Snes for the frame range.
    if args.trace_sa1.is_some() && !cart_has_sa1 {
        eprintln!("--trace-sa1: no SA-1 coprocessor in this cart; skipping");
    }
    let sa1_writer = match &args.trace_sa1 {
        Some(path) if cart_has_sa1 => Some(Rc::new(RefCell::new(open_trace(path)?))),
        _ => None,
    };
    let mut sa1_installed = false;

    let mut audio_pcm: Vec<(i16, i16)> = Vec::new();

    for frame in 0..args.frames {
        let p1 = script_state(&script, frame);
        let in_range =
            frame >= args.trace_start_frame && frame <= args.trace_end_frame;
        if let Some(w) = &spc_writer {
            if in_range && !spc_installed {
                let w = Rc::clone(w);
                snes.set_spc_trace(Box::new(move |line: &str| {
                    let _ = writeln!(w.borrow_mut(), "{line}");
                }));
                spc_installed = true;
            } else if !in_range && spc_installed {
                snes.clear_spc_trace();
                spc_installed = false;
            }
        }
        if let Some(w) = &gsu_writer {
            if in_range && !gsu_installed {
                let w = Rc::clone(w);
                snes.set_gsu_trace(Box::new(move |line: &str| {
                    let _ = writeln!(w.borrow_mut(), "{line}");
                }));
                gsu_installed = true;
            } else if !in_range && gsu_installed {
                snes.clear_gsu_trace();
                gsu_installed = false;
            }
        }
        if let Some(w) = &sa1_writer {
            if in_range && !sa1_installed {
                let w = Rc::clone(w);
                snes.set_sa1_trace(Box::new(move |line: &str| {
                    let _ = writeln!(w.borrow_mut(), "{line}");
                }));
                sa1_installed = true;
            } else if !in_range && sa1_installed {
                snes.clear_sa1_trace();
                sa1_installed = false;
            }
        }
        let tracing = trace_writer.is_some() && in_range;
        if tracing {
            let w = trace_writer.as_mut().unwrap();
            let mut sink = |line: &str| {
                let _ = writeln!(w, "{line}");
            };
            snes.run_frame_with_trace([p1, JoypadState::default()], &mut sink);
        } else {
            snes.run_frame([p1, JoypadState::default()]);
        }
        if args.dump_audio.is_some() {
            snes.drain_audio(&mut audio_pcm);
        }
        if let (Some(every), Some(dir)) = (args.dump_frame_every, &args.dump_dir) {
            if every > 0 && frame % every == 0 {
                let path = dir.join(format!("frame_{frame:05}.png"));
                write_frame_png(&snes, &path)?;
            }
        }
        if let Some((at, path)) = &args.save_state_at {
            if frame == *at {
                let bytes = snes.save_state();
                std::fs::write(path, &bytes)
                    .map_err(|e| format!("write {}: {e}", path.display()))?;
                println!("wrote {} ({} bytes) at frame {}", path.display(), bytes.len(), at);
            }
        }
    }

    // Persist SRAM as soon as the run loop is done, ahead of the optional
    // dump/trace-flush steps below, so a failure writing a debug artifact
    // never costs the player their save.
    save::save_if_dirty(&snes.bus.cart, &save_path, &sram_baseline);

    if let Some(w) = trace_writer.as_mut() {
        w.flush().map_err(|e| format!("flush trace: {e}"))?;
    }

    if spc_installed {
        snes.clear_spc_trace();
    }
    if let Some(w) = &spc_writer {
        w.borrow_mut().flush().map_err(|e| format!("flush spc trace: {e}"))?;
    }

    if sa1_installed {
        snes.clear_sa1_trace();
    }
    if let Some(w) = &sa1_writer {
        w.borrow_mut().flush().map_err(|e| format!("flush sa1 trace: {e}"))?;
    }

    if gsu_installed {
        snes.clear_gsu_trace();
    }
    if let Some(w) = &gsu_writer {
        w.borrow_mut().flush().map_err(|e| format!("flush gsu trace: {e}"))?;
    }

    if let Some(path) = &args.dump_frame {
        write_frame_png(&snes, path)?;
        println!("wrote {}", path.display());
    }

    if let Some(dir) = &args.dump_state {
        dump_state(&snes, dir)?;
    }

    if let Some(path) = &args.dump_spc {
        let path = resolve_out_path(path);
        let bytes = spc::build(&snes, &cart_title);
        write_new_file(&path, &bytes)?;
        println!("wrote {} ({} bytes)", path.display(), bytes.len());
    }

    if let Some(path) = &args.dump_audio {
        let rate = snes.sample_rate();
        let path = resolve_out_path(path);
        write_wav(&path, &audio_pcm, rate)?;
        println!("wrote {} ({} frames @ {} Hz)", path.display(), audio_pcm.len(), rate);
    }
    Ok(())
}

/// Write a canonical 44-byte-header PCM WAV: 16-bit signed, 2 channels,
/// interleaved little-endian L,R. `samples` are (left, right) frames.
fn write_wav(path: &Path, samples: &[(i16, i16)], sample_rate: u32) -> Result<(), String> {
    const CHANNELS: u32 = 2;
    const BITS: u32 = 16;
    let block_align: u32 = CHANNELS * BITS / 8; // 4 bytes/frame
    let byte_rate: u32 = sample_rate * block_align;
    let data_len: u32 = (samples.len() as u32) * block_align;
    let riff_len: u32 = 36 + data_len; // total file size minus the 8-byte RIFF tag

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
    }
    let file = File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    let mut w = BufWriter::new(file);
    let mut hdr = Vec::with_capacity(44);
    hdr.extend_from_slice(b"RIFF");
    hdr.extend_from_slice(&riff_len.to_le_bytes());
    hdr.extend_from_slice(b"WAVE");
    hdr.extend_from_slice(b"fmt ");
    hdr.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    hdr.extend_from_slice(&1u16.to_le_bytes()); // PCM
    hdr.extend_from_slice(&(CHANNELS as u16).to_le_bytes());
    hdr.extend_from_slice(&sample_rate.to_le_bytes());
    hdr.extend_from_slice(&byte_rate.to_le_bytes());
    hdr.extend_from_slice(&(block_align as u16).to_le_bytes());
    hdr.extend_from_slice(&(BITS as u16).to_le_bytes());
    hdr.extend_from_slice(b"data");
    hdr.extend_from_slice(&data_len.to_le_bytes());
    w.write_all(&hdr).map_err(|e| format!("write wav header: {e}"))?;

    let mut pcm = Vec::with_capacity(samples.len() * block_align as usize);
    for &(l, r) in samples {
        pcm.extend_from_slice(&l.to_le_bytes());
        pcm.extend_from_slice(&r.to_le_bytes());
    }
    w.write_all(&pcm).map_err(|e| format!("write wav data: {e}"))?;
    w.flush().map_err(|e| format!("flush wav: {e}"))?;
    Ok(())
}

/// Dump raw PPU/CPU memories for offline inspection. VRAM words are written
/// little-endian (low byte = plane 0).
fn dump_state(snes: &Snes, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let mut vram = vec![0u8; snes.bus.ppu.vram.len() * 2];
    for (i, w) in snes.bus.ppu.vram.iter().enumerate() {
        vram[i * 2] = *w as u8;
        vram[i * 2 + 1] = (*w >> 8) as u8;
    }
    let mut cgram = vec![0u8; snes.bus.ppu.cgram.len() * 2];
    for (i, w) in snes.bus.ppu.cgram.iter().enumerate() {
        cgram[i * 2] = *w as u8;
        cgram[i * 2 + 1] = (*w >> 8) as u8;
    }
    let mut oam = snes.bus.ppu.oam_lo.to_vec();
    oam.extend_from_slice(&snes.bus.ppu.oam_hi);
    let write = |name: &str, data: &[u8]| -> Result<(), String> {
        let p = dir.join(name);
        std::fs::write(&p, data).map_err(|e| format!("write {}: {e}", p.display()))
    };
    let p = &snes.bus.ppu;
    let summary = format!(
        "forced_blank={} brightness={} bg_mode={} bg3_priority={} main_screen={:#04x} sub_screen={:#04x} backdrop={:#06x} cgram1={:#06x}\n",
        p.forced_blank, p.brightness, p.bg_mode, p.bg3_priority, p.main_screen, p.sub_screen, p.cgram[0], p.cgram[1]
    );
    write("ppu.txt", summary.as_bytes())?;
    print!("{summary}");
    write("wram.bin", &snes.bus.wram[..])?;
    write("vram.bin", &vram)?;
    write("cgram.bin", &cgram)?;
    write("oam.bin", &oam)?;
    println!("dumped state to {}", dir.display());
    Ok(())
}

/// `--disasm`: disassemble `--count` instructions from `--addr` (or the reset
/// vector) by walking the bus. Fetches use `read_no_tick`, so no clock advances.
fn run_disasm(cart: Cartridge, args: &Args) -> Result<(), String> {
    let mut snes = Snes::new(cart);
    // Reset leaves the CPU in emulation mode (M=1, X=1); use those widths for an
    // explicit --addr as well, since the true flag state there is unknown.
    let (mut addr, m_flag, x_flag) = match args.addr {
        Some((bank, off)) => (((bank as u32) << 16) | off as u32, true, true),
        None => (
            ((snes.cpu.pbr as u32) << 16) | snes.cpu.pc as u32,
            snes.cpu.p.m(),
            snes.cpu.p.x(),
        ),
    };
    for _ in 0..args.count {
        let bank = addr & 0xFF_0000;
        let mut fetch = |a: u32| snes.bus.read_no_tick(a);
        let (text, len) =
            snes_core::debug::disasm::disassemble_one(&mut fetch, addr, m_flag, x_flag);
        println!("{:02X}:{:04X}  {}", (addr >> 16) as u8, (addr & 0xFFFF) as u16, text);
        // Instruction fetches wrap within the program bank (K:PC rule).
        let next_off = (addr as u16).wrapping_add(len as u16) as u32;
        addr = bank | next_off;
    }
    Ok(())
}

/// Write `data` to `path`, creating the parent directory if needed.
pub(crate) fn write_new_file(path: &Path, data: &[u8]) -> Result<(), String> {
    create_parent_dir(path)?;
    std::fs::write(path, data).map_err(|e| format!("write {}: {e}", path.display()))
}

/// `mkdir -p` on a file path's parent, skipping bare file names.
pub(crate) fn create_parent_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
    }
    Ok(())
}

/// Broken-down local calendar time, used to name screenshots/SPC exports and
/// to fill the `.spc` ID666 dump date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CalendarTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl CalendarTime {
    /// `YYYYMMDD-HHMMSS`, safe on every filesystem and sorting chronologically.
    pub fn file_stamp(&self) -> String {
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// ID666 text-format dump date: `MM/DD/YYYY` (11 bytes with the NUL).
    pub fn id666_date(&self) -> String {
        format!("{:02}/{:02}/{:04}", self.month, self.day, self.year)
    }
}

/// Current local wall-clock time. On unix the C library does the timezone/DST
/// work (`localtime_r`); elsewhere the UTC decomposition is used, since std
/// exposes no timezone database.
pub(crate) fn now_local() -> CalendarTime {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    #[cfg(unix)]
    {
        // SAFETY: `localtime_r` writes into the caller-provided `tm` and takes
        // a pointer to a live `time_t`; both live for the whole call and the
        // _r form needs no global lock.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let t = secs as libc::time_t;
        let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
        if ok {
            return CalendarTime {
                year: tm.tm_year + 1900,
                month: (tm.tm_mon + 1) as u8,
                day: tm.tm_mday as u8,
                hour: tm.tm_hour as u8,
                minute: tm.tm_min as u8,
                second: tm.tm_sec as u8,
            };
        }
    }
    civil_from_unix(secs)
}

/// Proleptic-Gregorian decomposition of a Unix timestamp (UTC), after Howard
/// Hinnant's `civil_from_days`.
pub(crate) fn civil_from_unix(secs: i64) -> CalendarTime {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Shift the era origin to 0000-03-01 so leap days land at the end of a year.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March-based
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    CalendarTime {
        year: (if m <= 2 { y + 1 } else { y }) as i32,
        month: m as u8,
        day: d as u8,
        hour: (rem / 3600) as u8,
        minute: (rem % 3600 / 60) as u8,
        second: (rem % 60) as u8,
    }
}

/// Characters no common filesystem accepts in a name (the union of the POSIX
/// separator and the Windows reserved set), plus control characters.
fn is_forbidden_in_file_name(c: char) -> bool {
    c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
}

/// Turn a cartridge title into a portable file-name stem: forbidden characters
/// become `_`, runs of whitespace collapse to a single `_`, trailing dots and
/// spaces are dropped (Windows strips them silently), and an empty result falls
/// back to `SNES` so a blank/garbage header still produces a usable name.
pub(crate) fn sanitize_file_stem(title: &str) -> String {
    let mut out = String::new();
    let mut pending_sep = false;
    for c in title.chars().take(64) {
        if c.is_whitespace() {
            pending_sep = !out.is_empty();
            continue;
        }
        let c = if is_forbidden_in_file_name(c) { '_' } else { c };
        if pending_sep {
            out.push('_');
            pending_sep = false;
        }
        out.push(c);
    }
    while out.ends_with('.') || out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        "SNES".to_string()
    } else {
        out
    }
}

/// `dir/<stem>.<ext>`, with `_2`, `_3`… appended until the name is free, so two
/// captures within the same second never overwrite each other.
pub(crate) fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    for n in 2..1000 {
        let p = dir.join(format!("{stem}_{n}.{ext}"));
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{stem}_{}.{ext}", std::process::id()))
}

/// Resolve a debug-output path: absolute paths are honored as-is; relative
/// paths are rooted under `target/debug-out/` (SKILL output-hygiene rule).
fn resolve_out_path(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        Path::new("target/debug-out").join(p)
    }
}

fn open_trace(path: &Path) -> Result<BufWriter<File>, String> {
    let resolved = resolve_out_path(path);
    if let Some(parent) = resolved.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
    }
    let f = File::create(&resolved).map_err(|e| format!("create {}: {e}", resolved.display()))?;
    eprintln!("tracing 65C816 to {}", resolved.display());
    Ok(BufWriter::new(f))
}

fn print_info(cart: &Cartridge) {
    let mapping = match cart.mapping {
        Mapping::LoRom => "LoROM",
        Mapping::HiRom => "HiROM",
    };
    let region = match cart.region {
        Region::Ntsc => "NTSC",
        Region::Pal => "PAL",
    };
    println!("Title:    {}", cart.title);
    println!("Mapping:  {}{}", mapping, if cart.fastrom { " (FastROM)" } else { "" });
    println!("Region:   {}", region);
    println!(
        "ROM size: {} bytes ({:.1} MB)",
        cart.rom.len(),
        cart.rom.len() as f64 / (1024.0 * 1024.0)
    );
    println!("SRAM:     {} bytes", cart.sram.len());
    println!(
        "Checksum: ${:04X} ({})",
        cart.header_checksum,
        if cart.checksum_valid { "valid" } else { "INVALID" }
    );
}

/// Load raw .sfc/.smc bytes, or the first ROM entry of a .zip. `pub(crate)`
/// so `video.rs` can reuse it for the in-game "open ROM" (`O`) hotkey.
pub(crate) fn load_rom_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let is_zip = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("zip"))
        .unwrap_or(false);
    if !is_zip {
        return std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()));
    }
    let file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("zip {}: {e}", path.display()))?;
    // Prefer .smc/.sfc entries; fall back to the first file entry.
    let mut candidate: Option<usize> = None;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| e.to_string())?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_ascii_lowercase();
        if name.ends_with(".smc") || name.ends_with(".sfc") {
            candidate = Some(i);
            break;
        }
        candidate.get_or_insert(i);
    }
    let idx = candidate.ok_or_else(|| format!("{}: empty zip", path.display()))?;
    let mut entry = archive.by_index(idx).map_err(|e| e.to_string())?;
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes).map_err(|e| format!("unzip: {e}"))?;
    Ok(bytes)
}

/// Script line format: `<frame> <button> <frames_held>`. `#` starts a comment.
fn parse_script(path: &Path) -> Result<BTreeMap<u32, Vec<(String, u32)>>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut script: BTreeMap<u32, Vec<(String, u32)>> = BTreeMap::new();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 3 {
            return Err(format!(
                "{}:{}: expected `<frame> <button> <frames_held>`",
                path.display(),
                lineno + 1
            ));
        }
        let frame: u32 = parse_num(parts[0])?;
        let held: u32 = parse_num(parts[2])?;
        // Validate the button name up front.
        input::set_button(&mut JoypadState::default(), parts[1], true)
            .map_err(|e| format!("{}:{}: {e}", path.display(), lineno + 1))?;
        script.entry(frame).or_default().push((parts[1].to_string(), held.max(1)));
    }
    Ok(script)
}

/// Player-1 state for `frame`: union of all script entries active at that frame.
fn script_state(script: &BTreeMap<u32, Vec<(String, u32)>>, frame: u32) -> JoypadState {
    let mut state = JoypadState::default();
    for (&start, presses) in script.range(..=frame) {
        for (button, held) in presses {
            if frame < start + held {
                let _ = input::set_button(&mut state, button, true);
            }
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_flag_parses_without_a_rom() {
        for flag in ["--version", "-V"] {
            let args = vec![flag.to_string()];
            let parsed = parse_args(&args).expect(flag);
            assert!(parsed.version);
            assert!(parsed.rom.is_none());
        }
        // Also accepted alongside --headless, which normally requires a ROM.
        let args = vec!["--headless".to_string(), "--version".to_string()];
        assert!(parse_args(&args).expect("--headless --version").version);
    }

    #[test]
    fn version_string_is_the_package_version() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
        assert_eq!(APP_NAME, "Prisme - SuperNes");
    }

    #[test]
    fn civil_from_unix_matches_known_utc_instants() {
        let t = civil_from_unix(0);
        assert_eq!((t.year, t.month, t.day, t.hour, t.minute, t.second), (1970, 1, 1, 0, 0, 0));
        // 2001-09-09T01:46:40Z, the "billennium" timestamp.
        let t = civil_from_unix(1_000_000_000);
        assert_eq!((t.year, t.month, t.day, t.hour, t.minute, t.second), (2001, 9, 9, 1, 46, 40));
        let t = civil_from_unix(1_700_000_000);
        assert_eq!(
            (t.year, t.month, t.day, t.hour, t.minute, t.second),
            (2023, 11, 14, 22, 13, 20)
        );
        // Leap day of a 400-year leap year.
        let t = civil_from_unix(951_782_400);
        assert_eq!((t.year, t.month, t.day), (2000, 2, 29));
        // Last second before the epoch (negative timestamp).
        let t = civil_from_unix(-1);
        assert_eq!((t.year, t.month, t.day, t.hour, t.minute, t.second), (1969, 12, 31, 23, 59, 59));
    }

    #[test]
    fn calendar_time_formats_stamp_and_id666_date() {
        let t = CalendarTime { year: 2026, month: 7, day: 4, hour: 9, minute: 5, second: 3 };
        assert_eq!(t.file_stamp(), "20260704-090503");
        assert_eq!(t.id666_date(), "07/04/2026");
        assert_eq!(t.id666_date().len(), 10);
    }

    #[test]
    fn sanitize_file_stem_produces_portable_names() {
        assert_eq!(sanitize_file_stem("SUPER MARIOWORLD"), "SUPER_MARIOWORLD");
        assert_eq!(sanitize_file_stem("  SECRET OF MANA   "), "SECRET_OF_MANA");
        assert_eq!(sanitize_file_stem("A/B:C*D?E\"F<G>H|I"), "A_B_C_D_E_F_G_H_I");
        assert_eq!(sanitize_file_stem("bad\u{7}ctrl"), "bad_ctrl");
        assert_eq!(sanitize_file_stem("trailing..."), "trailing");
        assert_eq!(sanitize_file_stem("   "), "SNES");
        assert_eq!(sanitize_file_stem(""), "SNES");
        // Long titles are bounded so the final name stays well under any
        // filesystem's per-component limit.
        assert!(sanitize_file_stem(&"X".repeat(200)).len() <= 64);
    }

    #[test]
    fn capture_file_names_are_title_then_timestamp() {
        // Shape of the names `App::take_screenshot` / `App::export_spc` build.
        let t = CalendarTime { year: 2026, month: 7, day: 24, hour: 21, minute: 30, second: 45 };
        let stem = format!("{}_{}", sanitize_file_stem("Secret of MANA "), t.file_stamp());
        assert_eq!(stem, "Secret_of_MANA_20260724-213045");
        let stem =
            format!("{}_{}", sanitize_file_stem("MARIO_ALLSTARS+WORLD"), t.file_stamp());
        assert_eq!(stem, "MARIO_ALLSTARS+WORLD_20260724-213045");
    }

    #[test]
    fn unique_path_avoids_clobbering_an_existing_file() {
        let dir = std::env::temp_dir().join(format!("prisme_unique_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let first = unique_path(&dir, "shot", "png");
        assert_eq!(first, dir.join("shot.png"));
        std::fs::write(&first, b"x").expect("write");
        let second = unique_path(&dir, "shot", "png");
        assert_eq!(second, dir.join("shot_2.png"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dump_spc_flag_parses() {
        let args = vec!["rom.sfc".to_string(), "--dump-spc".to_string(), "out.spc".to_string()];
        let parsed = parse_args(&args).expect("parse");
        assert_eq!(parsed.dump_spc, Some(PathBuf::from("out.spc")));
        // A missing value is an error, not a silent default.
        assert!(parse_args(&["--dump-spc".to_string()]).is_err());
    }

    #[test]
    fn write_wav_header_is_canonical() {
        let samples = vec![(0i16, 0i16), (1000, -1000), (32767, -32768)];
        let rate = 32_000u32;
        let dir = std::env::temp_dir();
        let path = dir.join(format!("snes_wav_test_{}.wav", std::process::id()));
        write_wav(&path, &samples, rate).expect("write_wav");
        let bytes = std::fs::read(&path).expect("read back");
        let _ = std::fs::remove_file(&path);

        let block_align = 4u32; // 2 ch * 16 bit / 8
        let data_len = samples.len() as u32 * block_align; // 12
        assert_eq!(bytes.len(), 44 + data_len as usize);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 36 + data_len);
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 16); // PCM fmt size
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 1); // PCM
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 2); // channels
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), rate);
        assert_eq!(u32::from_le_bytes(bytes[28..32].try_into().unwrap()), rate * block_align); // byte_rate
        assert_eq!(u16::from_le_bytes(bytes[32..34].try_into().unwrap()), block_align as u16);
        assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 16); // bits
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), data_len);
        // First interleaved frame is L=0,R=0; second is L=1000,R=-1000 LE.
        assert_eq!(&bytes[44..48], &[0, 0, 0, 0]);
        assert_eq!(i16::from_le_bytes(bytes[48..50].try_into().unwrap()), 1000);
        assert_eq!(i16::from_le_bytes(bytes[50..52].try_into().unwrap()), -1000);
    }
}

/// Write the console's raw 256x224 framebuffer as an RGBA PNG. Reads straight
/// from the core, so no windowed overlay/zoom/filter can ever appear in it —
/// shared by `--dump-frame`, `--dump-frame-every` and the F12 screenshot.
pub(crate) fn write_frame_png(snes: &Snes, path: &Path) -> Result<(), String> {
    create_parent_dir(path)?;
    let mut rgba = vec![0u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
    snes.framebuffer.to_rgba(&mut rgba);
    let file = File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    let mut encoder =
        png::Encoder::new(BufWriter::new(file), SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| format!("png header: {e}"))?;
    writer.write_image_data(&rgba).map_err(|e| format!("png write: {e}"))?;
    Ok(())
}
