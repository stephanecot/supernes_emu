//! Console top level: CPU + Bus, frame loop.

use crate::bus::Bus;
use crate::cartridge::Cartridge;
use crate::cpu::Cpu;
use crate::joypad::JoypadState;
use crate::scheduler::CYCLES_PER_LINE;
use crate::FrameBuffer;

pub struct Snes {
    pub cpu: Cpu,
    pub bus: Bus,
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
}
