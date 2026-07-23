//! Console top level: CPU + Bus, frame loop.

use crate::bus::Bus;
use crate::cartridge::Cartridge;
use crate::cpu::Cpu;
use crate::joypad::JoypadState;
use crate::scheduler::CYCLES_PER_LINE;
use crate::FrameBuffer;
use serde::{Deserialize, Serialize};

/// Save-state container magic ("SNES-ST\0"): the first 8 bytes of every blob.
const STATE_MAGIC: [u8; 8] = *b"SNES-ST\0";
/// Save-state format version. Bump on any change to the serialized layout so
/// `load_state` rejects blobs written by an incompatible build.
const STATE_VERSION: u32 = 1;
/// Fixed prefix length: magic(8) + version(4) + rom_checksum(2) + rom_len(4).
const STATE_HEADER_LEN: usize = 8 + 4 + 2 + 4;

#[derive(Serialize, Deserialize)]
pub struct Snes {
    pub cpu: Cpu,
    pub bus: Bus,
    /// Mirror of the PPU frame the frontend reads; regenerated every rendered
    /// frame, so it is excluded from save states.
    #[serde(skip)]
    pub framebuffer: FrameBuffer,
}

impl Snes {
    pub fn new(cart: Cartridge) -> Snes {
        let mut bus = Bus::new(cart);
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        Snes { cpu, bus, framebuffer: FrameBuffer::new() }
    }

    /// Emulate one full video frame (262/312 lines). Always terminates: while
    /// the CPU core is a stub (M1) and does not advance the clock, the
    /// scheduler is stepped one scanline per iteration as a guard.
    pub fn run_frame(&mut self, inputs: [JoypadState; 2]) -> &FrameBuffer {
        self.bus.set_inputs(inputs);
        self.bus.ppu.start_frame();
        self.bus.scheduler.frame_done = false;
        while !self.bus.scheduler.frame_done {
            let before = self.bus.scheduler.clock;
            self.cpu.step(&mut self.bus);
            if self.bus.scheduler.clock == before {
                // Guard: CPU made no progress (STP/dead core); force time
                // forward and still fire the per-line render events.
                self.bus.scheduler.tick(CYCLES_PER_LINE);
                self.bus.post_tick();
            }
        }
        self.mirror_framebuffer();
        &self.bus.ppu.framebuffer
    }

    /// S-DSP output sample rate (32 kHz stereo). Thin passthrough so the
    /// frontend need not reach into `bus.apu`.
    pub fn sample_rate(&self) -> u32 {
        self.bus.apu.sample_rate()
    }

    /// Append the APU's stereo samples produced so far to `out`, then clear the
    /// APU-side queue. Call once per emulated frame after `run_frame`.
    pub fn drain_audio(&mut self, out: &mut Vec<(i16, i16)>) {
        self.bus.apu.catch_up(self.bus.scheduler.clock);
        self.bus.apu.drain_samples(out);
    }

    /// Install a `--trace-spc` sink that fires once per SPC700 instruction. The
    /// SPC700 runs lazily inside APU `catch_up` (driven by CPU port access and
    /// `drain_audio`), so the sink stays installed until `clear_spc_trace`.
    pub fn set_spc_trace(&mut self, sink: Box<dyn FnMut(&str)>) {
        self.bus.apu.set_spc_trace(sink);
    }

    /// Remove the SPC700 trace sink; drop the returned box to flush its writer.
    pub fn clear_spc_trace(&mut self) -> Option<Box<dyn FnMut(&str)>> {
        self.bus.apu.clear_spc_trace()
    }

    /// True if the loaded cartridge has a SuperFX/GSU coprocessor.
    pub fn has_superfx(&self) -> bool {
        self.bus.cart.superfx.is_some()
    }

    /// Install a `--trace-gsu` sink that fires once per GSU instruction
    /// (including prefix-only bytes), immediately before it executes. The GSU
    /// runs lazily inside `Bus::gsu_catch_up` (driven by CPU/PPU ticks), so
    /// the sink stays installed until `clear_gsu_trace`. No-op if the cart has
    /// no SuperFX chip (`has_superfx() == false`).
    pub fn set_gsu_trace(&mut self, sink: Box<dyn FnMut(&str)>) {
        if let Some(fx) = self.bus.cart.superfx.as_mut() {
            fx.set_gsu_trace(sink);
        }
    }

    /// Remove the GSU trace sink; drop the returned box to flush its writer.
    pub fn clear_gsu_trace(&mut self) -> Option<Box<dyn FnMut(&str)>> {
        self.bus.cart.superfx.as_mut().and_then(|fx| fx.clear_gsu_trace())
    }

    /// Copy the PPU's freshly-rendered frame into `self.framebuffer`, the
    /// mirror the frontend reads by field access (`snes.framebuffer`).
    fn mirror_framebuffer(&mut self) {
        self.framebuffer.0.copy_from_slice(&self.bus.ppu.framebuffer.0[..]);
    }

    /// Same as `run_frame`, but invokes `sink` with a Mesen2-format trace line
    /// for the instruction about to execute, before every CPU step. The trace
    /// text is disassembled over a bus-backed fetch closure (`read_no_tick`, no
    /// clock side effects). Only this path allocates a `String` per
    /// instruction; the plain `run_frame` path is untouched.
    pub fn run_frame_with_trace(
        &mut self,
        inputs: [JoypadState; 2],
        sink: &mut dyn FnMut(&str),
    ) -> &FrameBuffer {
        self.bus.set_inputs(inputs);
        self.bus.ppu.start_frame();
        self.bus.scheduler.frame_done = false;
        while !self.bus.scheduler.frame_done {
            if !self.cpu.stopped && !self.cpu.waiting {
                let bus = &mut self.bus;
                let mut fetch = |a: u32| bus.read_no_tick(a);
                let line = crate::debug::trace::trace_line(&self.cpu, &mut fetch);
                sink(&line);
            }
            let before = self.bus.scheduler.clock;
            self.cpu.step(&mut self.bus);
            if self.bus.scheduler.clock == before {
                self.bus.scheduler.tick(CYCLES_PER_LINE);
                self.bus.post_tick();
            }
        }
        self.mirror_framebuffer();
        &self.bus.ppu.framebuffer
    }

    /// Serialize the full active emulator state to a versioned binary blob. The
    /// ROM image is not included (it is reattached from the loaded game on
    /// `load_state`); the header carries the ROM's header checksum and length so
    /// a state cannot be loaded onto a different game. SRAM is included.
    pub fn save_state(&self) -> Vec<u8> {
        let body = bincode::serialize(self).expect("Snes state is serializable");
        let mut out = Vec::with_capacity(STATE_HEADER_LEN + body.len());
        out.extend_from_slice(&STATE_MAGIC);
        out.extend_from_slice(&STATE_VERSION.to_le_bytes());
        out.extend_from_slice(&self.bus.cart.header_checksum.to_le_bytes());
        out.extend_from_slice(&(self.bus.cart.rom.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// Restore a blob produced by `save_state` onto this console, keeping the
    /// currently-loaded ROM. Rejects a wrong magic/version, a truncated blob,
    /// a ROM whose header checksum or length differs from the saved game, and a
    /// corrupt body. On success the whole CPU/bus/PPU/APU/DMA/scheduler/cart
    /// state (including SRAM) is replaced. Host-side taps that are not emulated
    /// hardware (`Bus::debug` log/watch config and any installed SPC trace sink)
    /// are not part of the blob and are cleared to their defaults; callers that
    /// arm them must do so after `load_state`.
    pub fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        if data.len() < STATE_HEADER_LEN {
            return Err(format!(
                "save state too short: {} bytes (need >= {STATE_HEADER_LEN})",
                data.len()
            ));
        }
        if data[..8] != STATE_MAGIC {
            return Err("save state magic mismatch (not a SNES-ST blob)".to_string());
        }
        let version = u32::from_le_bytes(data[8..12].try_into().unwrap());
        if version != STATE_VERSION {
            return Err(format!(
                "save state version {version} unsupported (expected {STATE_VERSION})"
            ));
        }
        let rom_checksum = u16::from_le_bytes(data[12..14].try_into().unwrap());
        let rom_len = u32::from_le_bytes(data[14..18].try_into().unwrap()) as usize;
        if rom_checksum != self.bus.cart.header_checksum
            || rom_len != self.bus.cart.rom.len()
        {
            return Err(format!(
                "save state is for a different ROM (checksum ${:04X}/len {}, loaded ${:04X}/len {})",
                rom_checksum,
                rom_len,
                self.bus.cart.header_checksum,
                self.bus.cart.rom.len()
            ));
        }
        let mut restored: Snes = bincode::deserialize(&data[STATE_HEADER_LEN..])
            .map_err(|e| format!("save state body is corrupt: {e}"))?;
        // The ROM was skipped during serialization; reattach the live image.
        restored.bus.cart.rom = std::mem::take(&mut self.bus.cart.rom);
        *self = restored;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pal_cart() -> Cartridge {
        let mut rom = vec![0u8; 0x10000];
        rom[0x7FC0..0x7FC0 + 21].copy_from_slice(b"FRAME TEST           ");
        rom[0x7FC0 + 0x15] = 0x20;
        rom[0x7FC0 + 0x19] = 2;
        rom[0x7FC0 + 0x3C] = 0x00;
        rom[0x7FC0 + 0x3D] = 0x80;
        Cartridge::from_bytes(rom).unwrap()
    }

    #[test]
    fn reset_fetches_vector_and_sets_state() {
        let mut rom = vec![0u8; 0x10000];
        rom[0x7FC0..0x7FC0 + 21].copy_from_slice(b"RESET TEST           ");
        rom[0x7FC0 + 0x15] = 0x20;
        rom[0x7FC0 + 0x19] = 2;
        rom[0x7FFC] = 0x34; // reset vector $8034 (offset $7FFC = $00:FFFC in LoROM)
        rom[0x7FFD] = 0x80;
        let snes = Snes::new(Cartridge::from_bytes(rom).unwrap());
        assert_eq!(snes.cpu.pc, 0x8034);
        assert!(snes.cpu.emulation);
        assert_eq!(snes.cpu.s, 0x01FF);
        assert!(snes.cpu.p.m() && snes.cpu.p.x() && snes.cpu.p.i());
    }

    #[test]
    fn run_frame_terminates_with_stub_cpu() {
        let mut snes = Snes::new(pal_cart());
        for _ in 0..3 {
            snes.run_frame([JoypadState::default(); 2]);
        }
        // 3 PAL frames worth of clock must have elapsed.
        assert!(snes.bus.scheduler.clock >= 3 * 312 * CYCLES_PER_LINE);
    }

    /// LoROM cart whose reset code drives the backdrop color (CGRAM[0]) from two
    /// WRAM bytes: $0000, incremented every main-loop iteration, and $0001,
    /// incremented by an H-IRQ handler that fires (and acks $4211) once per
    /// scanline. Full brightness, no forced blank, so the whole screen shows a
    /// color whose low byte tracks the loop counter and whose high byte tracks
    /// the per-line IRQ count. The framebuffer is therefore a sensitive function
    /// of CPU + WRAM + PPU + scheduler CLOCK **and** scheduler IRQ state
    /// (irq_mode/htime/irq_target): dropping any of those from the save state
    /// diverges the restored run.
    fn counter_cart(checksum: u16) -> Cartridge {
        let mut rom = vec![0u8; 0x10000];
        // Reset $8000 (emulation/8-bit):
        //   LDA #$0F; STA $2100        ; brightness 15, blank off
        //   LDA #$64; STA $4207        ; HTIME lo = 100 dots
        //   LDA #$00; STA $4208        ; HTIME hi = 0
        //   LDA #$10; STA $4200        ; enable H-IRQ (every line), NMI/auto-joy off
        //   CLI                        ; unmask IRQ
        // loop $8015:
        //   INC $0000                  ; main counter++
        //   LDA #$00; STA $2121        ; CGADD = 0
        //   LDA $0000; STA $2122       ; CGRAM low  = main counter
        //   LDA $0001; STA $2122       ; CGRAM high = IRQ counter -> commit CGRAM[0]
        //   JMP $8015
        // IRQ handler $802C:
        //   INC $0001                  ; IRQ counter++
        //   LDA $4211                  ; ack TIMEUP (clears the level-held IRQ)
        //   RTI
        let code: [u8; 51] = [
            0xA9, 0x0F, 0x8D, 0x00, 0x21, // LDA #$0F : STA $2100
            0xA9, 0x64, 0x8D, 0x07, 0x42, // LDA #$64 : STA $4207
            0xA9, 0x00, 0x8D, 0x08, 0x42, // LDA #$00 : STA $4208
            0xA9, 0x10, 0x8D, 0x00, 0x42, // LDA #$10 : STA $4200
            0x58, // CLI
            0xEE, 0x00, 0x00, // INC $0000
            0xA9, 0x00, 0x8D, 0x21, 0x21, // LDA #$00 : STA $2121
            0xAD, 0x00, 0x00, 0x8D, 0x22, 0x21, // LDA $0000 : STA $2122
            0xAD, 0x01, 0x00, 0x8D, 0x22, 0x21, // LDA $0001 : STA $2122
            0x4C, 0x15, 0x80, // JMP $8015
            0xEE, 0x01, 0x00, // INC $0001
            0xAD, 0x11, 0x42, // LDA $4211
            0x40, // RTI
        ];
        rom[..code.len()].copy_from_slice(&code);
        rom[0x7FC0..0x7FC0 + 21].fill(b' ');
        rom[0x7FC0..0x7FC0 + 14].copy_from_slice(b"SAVESTATE TEST");
        rom[0x7FC0 + 0x15] = 0x20; // LoROM
        rom[0x7FC0 + 0x19] = 2; // PAL
        rom[0x7FC0 + 0x3C] = 0x00;
        rom[0x7FC0 + 0x3D] = 0x80;
        // Header checksum bytes ($7FDE/$7FDF) double as the save-state ROM id.
        rom[0x7FDE] = (checksum & 0xFF) as u8;
        rom[0x7FDF] = (checksum >> 8) as u8;
        // Emulation vectors: reset $00:FFFC -> $8000, IRQ/BRK $00:FFFE -> $802C.
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        rom[0x7FFE] = 0x2C;
        rom[0x7FFF] = 0x80;
        Cartridge::from_bytes(rom).unwrap()
    }

    /// Arm HDMA channel 0 to write master brightness ($2100 INIDISP) once per
    /// scanline from a table in bank-$00 WRAM, so rendered lines depend on live
    /// HDMA/DMA channel state (hdmaen + the $43x0-$43x4 channel registers) plus
    /// the WRAM table. Dropping any of those from the save state diverges the
    /// restored run. Direct mode, transfer unit 0 (one byte per line), repeat
    /// entries so each covered line gets its own brightness value.
    fn arm_hdma_brightness(snes: &mut Snes) {
        let table: [u8; 11] =
            [0x84, 0x0F, 0x08, 0x0F, 0x04, 0x84, 0x0C, 0x02, 0x0C, 0x06, 0x00];
        for (i, &b) in table.iter().enumerate() {
            snes.bus.wram[0x0200 + i] = b;
        }
        snes.bus.dma.write(0x00, 0x00); // DMAP ch0: A->B, direct, unit 0
        snes.bus.dma.write(0x01, 0x00); // BBAD -> $2100
        snes.bus.dma.write(0x02, 0x00); // A1T lo
        snes.bus.dma.write(0x03, 0x02); // A1T hi -> table at $00:0200
        snes.bus.dma.write(0x04, 0x00); // A1B bank $00
        snes.bus.dma.hdmaen = 0x01;
    }

    fn hash_fb(fb: &FrameBuffer) -> u64 {
        let mut h = 0xcbf29ce484222325u64;
        for &px in fb.0.iter() {
            h = (h ^ px as u64).wrapping_mul(0x100000001b3);
        }
        h
    }

    fn hash_samples(samples: &[(i16, i16)]) -> u64 {
        let mut h = 0xcbf29ce484222325u64;
        for &(l, r) in samples {
            for b in l.to_le_bytes().into_iter().chain(r.to_le_bytes()) {
                h = (h ^ b as u64).wrapping_mul(0x100000001b3);
            }
        }
        h
    }

    /// Run `n` frames, returning the final framebuffer hash and every drained
    /// stereo sample. Draining each frame catches the APU up so the audio
    /// reflects the full DSP/voice state.
    fn run_frames(snes: &mut Snes, n: usize) -> (u64, Vec<(i16, i16)>) {
        let mut audio = Vec::new();
        for _ in 0..n {
            snes.run_frame([JoypadState::default(); 2]);
            snes.drain_audio(&mut audio);
        }
        (hash_fb(&snes.framebuffer), audio)
    }

    #[test]
    fn save_state_roundtrip_is_deterministic() {
        const K: usize = 5;
        const L: usize = 7;
        let mut a = Snes::new(counter_cart(0x1234));
        for _ in 0..K {
            a.run_frame([JoypadState::default(); 2]);
        }
        let at_save = hash_fb(&a.framebuffer);

        // Clear the (so-far silent) APU queue, then arm the subsystems the CPU
        // program does not exercise: a keyed-on DSP voice (audio depends on the
        // full APU/DSP state) and an HDMA channel (video depends on HDMA/DMA
        // state). The scheduler IRQ path is already driven by the cart itself.
        let mut discard = Vec::new();
        a.drain_audio(&mut discard);
        a.bus.apu.test_kon_voice0();
        arm_hdma_brightness(&mut a);

        let blob = a.save_state();

        let (fb_a, audio_a) = run_frames(&mut a, L);
        // The program must make the frame evolve and the voice must sound, else
        // the test proves nothing about state completeness.
        assert_ne!(at_save, fb_a, "framebuffer must change across frames");
        assert!(audio_a.iter().any(|&(l, _)| l != 0), "voice must produce audio");

        // Fresh console with the ROM reattached, restore, run the same L frames.
        let mut b = Snes::new(counter_cart(0x1234));
        b.load_state(&blob).unwrap();
        let (fb_b, audio_b) = run_frames(&mut b, L);
        assert_eq!(fb_a, fb_b, "video diverged: a CPU/PPU/DMA/IRQ field is not captured");
        assert_eq!(
            hash_samples(&audio_a),
            hash_samples(&audio_b),
            "audio diverged: an APU/DSP/voice field is not captured"
        );
    }

    #[test]
    fn load_state_rejects_short_and_corrupt() {
        let mut a = Snes::new(counter_cart(0x1234));
        a.run_frame([JoypadState::default(); 2]);
        assert!(a.load_state(&[]).is_err());
        assert!(a.load_state(&[0, 1, 2, 3]).is_err());
        let mut blob = a.save_state();
        // Corrupt the magic.
        blob[0] ^= 0xFF;
        assert!(a.load_state(&blob).is_err());
        // Corrupt the version.
        let mut blob = a.save_state();
        blob[8] ^= 0xFF;
        assert!(a.load_state(&blob).is_err());
    }

    #[test]
    fn load_state_rejects_mismatched_rom() {
        let a = Snes::new(counter_cart(0x1234));
        let blob = a.save_state();
        // A different game (distinct header checksum) must be refused.
        let mut other = Snes::new(counter_cart(0x5678));
        assert!(other.load_state(&blob).is_err());
    }
}
