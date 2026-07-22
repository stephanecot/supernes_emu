//! snes-frontend CLI. Contract lives in .claude/skills/snes-build-test/SKILL.md
//! — keep the two in sync.

mod audio;
mod input;
mod save;
mod video;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use snes_core::{Cartridge, JoypadState, Mapping, Region, Snes, SCREEN_HEIGHT, SCREEN_WIDTH};

#[derive(Default)]
struct Args {
    rom: Option<PathBuf>,
    info: bool,
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
    trace_start_frame: u32,
    trace_end_frame: u32,
    log_mmio: bool,
    watch: Vec<(u8, u16)>,
    script: Option<PathBuf>,
    dump_state: Option<PathBuf>,
    dump_audio: Option<PathBuf>,
    save: Option<PathBuf>,
}

const USAGE: &str = "usage: snes-frontend <rom.sfc|.smc|.zip> [flags]
  --info                                print header info and exit
  --disasm [--addr BB:AAAA] [--count N] disassemble and exit
  --headless --frames N                 emulate N frames without a window
  --dump-frame PATH.png                 write final framebuffer as PNG on exit
  --dump-frame-every N --dump-dir DIR   write DIR/frame_XXXXX.png every N frames
  --trace PATH [--trace-start-frame A --trace-end-frame B]
  --trace-spc PATH                      SPC700 trace, same bounds
  --log-mmio                            log named MMIO writes to stderr
  --watch BB:AAAA                       log every read/write at a bus address
  --script PATH                         input script: <frame> <button> <held>
  --dump-state DIR                      dump wram/vram/cgram/oam/apuram on exit
  --dump-audio PATH.wav                 headless: write 32kHz 16-bit stereo WAV
  --save PATH                           battery SRAM file (default: <rom>.srm)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = match parse_args(&args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
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
            "--save" => a.save = Some(value(&mut it, "--save")?.into()),
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
    if a.rom.is_none() {
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

    if !args.headless {
        if args.dump_audio.is_some() {
            eprintln!("--dump-audio requires --headless; ignoring (windowed mode plays live)");
        }
        return video::run(cart, save_path, sram_baseline);
    }

    let script = match &args.script {
        Some(path) => parse_script(path)?,
        None => BTreeMap::new(),
    };

    let mut snes = Snes::new(cart);
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

    if let Some(path) = &args.dump_frame {
        write_frame_png(&snes, path)?;
        println!("wrote {}", path.display());
    }

    if let Some(dir) = &args.dump_state {
        dump_state(&snes, dir)?;
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

/// Load raw .sfc/.smc bytes, or the first ROM entry of a .zip.
fn load_rom_bytes(path: &Path) -> Result<Vec<u8>, String> {
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

fn write_frame_png(snes: &Snes, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
    }
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
