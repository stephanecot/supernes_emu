//! APU (SPC700 + S-DSP). The SPC700 core runs the IPL boot ROM and any uploaded
//! sound driver over a private 64 KB ARAM; the S-CPU only sees the four comm
//! ports ($2140-$2143). `catch_up` advances the SPC700 and its three timers to
//! a master-clock timestamp using a 32.32 fixed-point cycle accumulator so there
//! is no long-term drift between the two clock domains.

pub mod brr;
pub mod dsp;
pub mod ipl;
pub mod spc700;

use dsp::Dsp;
use ipl::IPL_ROM;
use spc700::{Spc700, Spc700Bus};

/// SPC700 nominal CPU clock: 2.048 MHz S-SMP / 2.
const SPC_CLOCK_HZ: u32 = 1_024_000;

/// Stage-1 prescaler in SPC cycles: timers 0/1 tick at 8 kHz (1.024e6 / 128),
/// timer 2 at 64 kHz (1.024e6 / 16).
const T01_PERIOD: u16 = 128;
const T2_PERIOD: u16 = 16;

struct Timer {
    enabled: bool,
    /// Stage-2 target ($FA-$FC); $00 means 256.
    target: u8,
    /// Stage-1 SPC-cycle prescaler accumulator.
    prescaler: u16,
    /// Stage-2 internal 8-bit counter.
    stage: u16,
    /// 4-bit output counter ($FD-$FF), read-clears.
    out: u8,
    period: u16,
}

impl Timer {
    fn new(period: u16) -> Self {
        Timer { enabled: false, target: 0, prescaler: 0, stage: 0, out: 0, period }
    }

    fn tick(&mut self, cycles: u32, running: bool) {
        if !self.enabled || !running {
            return;
        }
        self.prescaler += cycles as u16;
        while self.prescaler >= self.period {
            self.prescaler -= self.period;
            self.stage += 1;
            let tgt = if self.target == 0 { 256 } else { self.target as u16 };
            if self.stage >= tgt {
                self.stage = 0;
                self.out = (self.out + 1) & 0x0F;
            }
        }
    }
}

pub struct Apu {
    /// Last value the CPU wrote to each port ($2140-$2143 -> SPC $F4-$F7).
    pub ports_from_cpu: [u8; 4],
    /// What the SPC700 exposes to the CPU (SPC $F4-$F7 -> $2140-$2143).
    pub ports_to_cpu: [u8; 4],

    spc: Spc700,
    ram: Box<[u8; 0x10000]>,
    dsp: Dsp,
    timers: [Timer; 3],
    /// $F0 TEST (power-on $0A: RAM writes + timers enabled).
    test: u8,
    /// $F1 CONTROL last written value.
    control: u8,
    /// $F2 DSPADDR (bit7 = write-protect DSP data).
    dspaddr: u8,
    /// CONTROL bit7: IPL ROM overlaid at $FFC0-$FFFF.
    ipl_enabled: bool,

    master_hz: u32,
    /// SPC cycles per master cycle, 32.32 fixed point.
    spc_cycle_fixed: u64,
    /// Fractional SPC-cycle accumulator (low 32 bits).
    frac_accum: u64,
    last_master: u64,
    /// Total SPC cycles executed (monotonic).
    spc_clock: u64,
    /// Signed SPC-cycle budget; negative = last instruction overshot (carried
    /// so the average rate has no drift).
    cycle_budget: i64,

    /// SPC cycles accumulated toward the next 32 kHz sample (1 sample / 32 cyc).
    sample_accum: u32,
    /// Generated 32 kHz stereo samples awaiting `drain_samples`.
    audio: Vec<(i16, i16)>,

    /// Optional `--trace-spc` sink: called with a formatted trace line for each
    /// instruction the SPC700 is about to execute (like the S-CPU trace hook).
    spc_trace: Option<Box<dyn FnMut(&str)>>,
}

/// SPC cycles per generated stereo sample: 1.024 MHz / 32000 Hz = 32.
const SPC_CYCLES_PER_SAMPLE: u32 = 32;

/// S-DSP nominal output rate.
const DSP_OUTPUT_HZ: u32 = 32_000;

/// Cap on buffered samples (~2 s) so a frontend that never drains cannot grow
/// the buffer without bound.
const AUDIO_BUFFER_CAP: usize = 64_000;

impl Apu {
    pub fn new() -> Self {
        let mut apu = Apu {
            ports_from_cpu: [0; 4],
            ports_to_cpu: [0; 4],
            spc: Spc700::new(),
            ram: vec![0u8; 0x10000].into_boxed_slice().try_into().unwrap(),
            dsp: Dsp::new(),
            timers: [Timer::new(T01_PERIOD), Timer::new(T01_PERIOD), Timer::new(T2_PERIOD)],
            test: 0x0A,
            control: 0,
            dspaddr: 0,
            ipl_enabled: true,
            master_hz: 21_477_272,
            spc_cycle_fixed: 0,
            frac_accum: 0,
            last_master: 0,
            spc_clock: 0,
            cycle_budget: 0,
            sample_accum: 0,
            audio: Vec::new(),
            spc_trace: None,
        };
        apu.recompute_ratio();
        apu.reset();
        apu
    }

    /// Called by `Bus::new` with the region master clock (NTSC 21_477_272,
    /// PAL 21_281_370). The Apu is region-agnostic and only needs the ratio to
    /// the SPC700's fixed 1.024 MHz clock.
    pub fn set_region(&mut self, master_hz: u32) {
        self.master_hz = master_hz;
        self.recompute_ratio();
    }

    fn recompute_ratio(&mut self) {
        self.spc_cycle_fixed =
            (((SPC_CLOCK_HZ as u128) << 32) / self.master_hz as u128) as u64;
    }

    /// Re-fetch the reset vector and jump the SPC700 to the IPL entry ($FFC0).
    pub fn reset(&mut self) {
        let Apu {
            spc,
            ram,
            dsp,
            timers,
            test,
            control,
            dspaddr,
            ipl_enabled,
            ports_from_cpu,
            ports_to_cpu,
            ..
        } = self;
        let mut mem = SpcMem {
            ram,
            dsp,
            timers,
            test,
            control,
            dspaddr,
            ipl_enabled,
            ports_from_cpu,
            ports_to_cpu,
        };
        spc.reset(&mut mem);
    }

    /// S-DSP output sample rate (32 kHz stereo).
    pub fn sample_rate(&self) -> u32 {
        DSP_OUTPUT_HZ
    }

    /// Drain the S-DSP's stereo samples generated since the last call, appending
    /// them to `out`. Callers should `catch_up` to the current clock first so the
    /// DSP has produced all pending samples.
    pub fn drain_samples(&mut self, out: &mut Vec<(i16, i16)>) {
        out.append(&mut self.audio);
    }

    /// Install the `--trace-spc` sink. It fires once per SPC700 instruction,
    /// before execution, for as long as it stays installed (across `catch_up`).
    pub fn set_spc_trace(&mut self, sink: Box<dyn FnMut(&str)>) {
        self.spc_trace = Some(sink);
    }

    /// Remove the trace sink and return it (drop it to flush any buffered writer).
    pub fn clear_spc_trace(&mut self) -> Option<Box<dyn FnMut(&str)>> {
        self.spc_trace.take()
    }

    /// CPU read of $2140-$217F (mirrored every 4 bytes): the SPC-side outputs.
    pub fn read_port(&mut self, port: u8) -> u8 {
        self.ports_to_cpu[(port & 3) as usize]
    }

    /// CPU write of $2140-$2143: the SPC-side inputs ($F4-$F7).
    pub fn write_port(&mut self, port: u8, value: u8) {
        self.ports_from_cpu[(port & 3) as usize] = value;
    }

    /// Advance the SPC700 and timers up to `master_clock`.
    pub fn catch_up(&mut self, master_clock: u64) {
        if master_clock <= self.last_master {
            return;
        }
        let delta = master_clock - self.last_master;
        self.last_master = master_clock;
        let total = self.frac_accum as u128 + delta as u128 * self.spc_cycle_fixed as u128;
        let whole = (total >> 32) as u64;
        self.frac_accum = (total & 0xFFFF_FFFF) as u64;
        self.cycle_budget += whole as i64;
        self.run_budget();
    }

    fn run_budget(&mut self) {
        let Apu {
            spc,
            ram,
            dsp,
            timers,
            test,
            control,
            dspaddr,
            ipl_enabled,
            ports_from_cpu,
            ports_to_cpu,
            spc_clock,
            cycle_budget,
            sample_accum,
            audio,
            spc_trace,
            ..
        } = self;
        if spc.stopped {
            *cycle_budget = 0;
            return;
        }
        let mut mem = SpcMem {
            ram,
            dsp,
            timers,
            test,
            control,
            dspaddr,
            ipl_enabled,
            ports_from_cpu,
            ports_to_cpu,
        };
        while *cycle_budget > 0 {
            if let Some(sink) = spc_trace.as_mut() {
                // Read raw ARAM (no side effects, unlike a bus read of the
                // $F0-$FF I/O overlay which would clear timer counters), but
                // honor the IPL ROM overlay so boot-phase code disassembles.
                let ram = &mem.ram;
                let ipl = *mem.ipl_enabled;
                let mut read = |a: u16| {
                    if ipl && (0xFFC0..=0xFFFF).contains(&a) {
                        ipl::IPL_ROM[(a - 0xFFC0) as usize]
                    } else {
                        ram[a as usize]
                    }
                };
                let line = crate::debug::spc_disasm::spc_trace_line(spc, &mut read);
                sink(&line);
            }
            let c = spc.step(&mut mem);
            let running = timers_running(*mem.test);
            for t in mem.timers.iter_mut() {
                t.tick(c, running);
            }
            *cycle_budget -= c as i64;
            *spc_clock += c as u64;
            // The S-DSP emits one 32 kHz stereo sample every 32 SPC cycles,
            // reading ARAM (sample directory, BRR blocks, echo buffer) directly.
            *sample_accum += c;
            while *sample_accum >= SPC_CYCLES_PER_SAMPLE {
                *sample_accum -= SPC_CYCLES_PER_SAMPLE;
                mem.dsp.tick(&mut **mem.ram, audio);
            }
            // If the frontend is not draining, drop the oldest samples so the
            // buffer cannot grow without bound (the DSP state still advances).
            if audio.len() > AUDIO_BUFFER_CAP {
                let overflow = audio.len() - AUDIO_BUFFER_CAP;
                audio.drain(0..overflow);
            }
            if spc.stopped {
                *cycle_budget = 0;
                break;
            }
        }
    }
}

/// Timers run when TEST bit3 (enable) is set and bit0 (halt) is clear.
fn timers_running(test: u8) -> bool {
    (test & 0x08) != 0 && (test & 0x01) == 0
}

/// SPC700-side view of ARAM: RAM plus the $F0-$FF I/O overlay and the IPL ROM
/// overlay at $FFC0-$FFFF. Borrows the Apu's memory/IO fields (but not the CPU),
/// so the core can be stepped while these are mutably held.
struct SpcMem<'a> {
    ram: &'a mut Box<[u8; 0x10000]>,
    dsp: &'a mut Dsp,
    timers: &'a mut [Timer; 3],
    test: &'a mut u8,
    control: &'a mut u8,
    dspaddr: &'a mut u8,
    ipl_enabled: &'a mut bool,
    ports_from_cpu: &'a mut [u8; 4],
    ports_to_cpu: &'a mut [u8; 4],
}

impl<'a> SpcMem<'a> {
    fn io_read(&mut self, addr: u16) -> u8 {
        match addr {
            // Write-only registers read back $00.
            0xF0 | 0xF1 | 0xFA | 0xFB | 0xFC => 0,
            0xF2 => *self.dspaddr & 0x7F,
            0xF3 => self.dsp.read(*self.dspaddr & 0x7F),
            0xF4..=0xF7 => self.ports_from_cpu[(addr - 0xF4) as usize],
            0xF8 | 0xF9 => self.ram[addr as usize],
            0xFD => {
                let v = self.timers[0].out;
                self.timers[0].out = 0;
                v
            }
            0xFE => {
                let v = self.timers[1].out;
                self.timers[1].out = 0;
                v
            }
            0xFF => {
                let v = self.timers[2].out;
                self.timers[2].out = 0;
                v
            }
            _ => 0,
        }
    }

    fn io_write(&mut self, addr: u16, val: u8) {
        match addr {
            0xF0 => *self.test = val,
            0xF1 => {
                for i in 0..3 {
                    let en = val & (1 << i) != 0;
                    // 0->1 transition resets the timer's internal counter and TnOUT.
                    if en && !self.timers[i].enabled {
                        self.timers[i].stage = 0;
                        self.timers[i].out = 0;
                        self.timers[i].prescaler = 0;
                    }
                    self.timers[i].enabled = en;
                }
                if val & 0x10 != 0 {
                    self.ports_from_cpu[0] = 0;
                    self.ports_from_cpu[1] = 0;
                }
                if val & 0x20 != 0 {
                    self.ports_from_cpu[2] = 0;
                    self.ports_from_cpu[3] = 0;
                }
                *self.ipl_enabled = val & 0x80 != 0;
                *self.control = val;
            }
            0xF2 => *self.dspaddr = val,
            // Writing $F2 with bit7 set makes DSPDATA read-only.
            0xF3 => {
                if *self.dspaddr & 0x80 == 0 {
                    self.dsp.write(*self.dspaddr & 0x7F, val);
                }
            }
            0xF4..=0xF7 => self.ports_to_cpu[(addr - 0xF4) as usize] = val,
            0xF8 | 0xF9 => self.ram[addr as usize] = val,
            0xFA => self.timers[0].target = val,
            0xFB => self.timers[1].target = val,
            0xFC => self.timers[2].target = val,
            // $FD-$FF are read-only.
            _ => {}
        }
    }
}

impl<'a> Spc700Bus for SpcMem<'a> {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x00F0..=0x00FF => self.io_read(addr),
            0xFFC0..=0xFFFF if *self.ipl_enabled => IPL_ROM[(addr - 0xFFC0) as usize],
            _ => self.ram[addr as usize],
        }
    }

    fn write(&mut self, addr: u16, val: u8) {
        match addr {
            0x00F0..=0x00FF => self.io_write(addr, val),
            // Writes to $FFC0-$FFFF always land in the underlying RAM.
            _ => self.ram[addr as usize] = val,
        }
    }
}

impl Default for Apu {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_counts_to_target() {
        // 8 kHz timer, target 2 => one TnOUT increment every 2*128 = 256 cycles.
        let mut t = Timer::new(T01_PERIOD);
        t.enabled = true;
        t.target = 2;
        for _ in 0..(2 * T01_PERIOD as u32) - 1 {
            t.tick(1, true);
        }
        assert_eq!(t.out, 0);
        t.tick(1, true);
        assert_eq!(t.out, 1);
    }

    #[test]
    fn timer_wraps_4bit() {
        let mut t = Timer::new(T2_PERIOD);
        t.enabled = true;
        t.target = 1; // increment every 16 cycles
        for _ in 0..(16 * 16) {
            t.tick(1, true);
        }
        assert_eq!(t.out, 0); // 16 increments wrap 15->0
    }

    #[test]
    fn timer_halted_by_test() {
        let mut t = Timer::new(T2_PERIOD);
        t.enabled = true;
        t.target = 1;
        for _ in 0..64 {
            t.tick(1, false); // TEST halt => no ticks
        }
        assert_eq!(t.out, 0);
    }

    #[test]
    fn ipl_exposes_aa_bb_after_reset() {
        // The IPL zero-fills page 1 then writes $AA/$BB to ports 0/1. Running a
        // few thousand SPC cycles must land it in the "wait for $CC" spin with
        // the ready signal exposed to the S-CPU.
        let mut apu = Apu::new();
        apu.set_region(21_477_272);
        apu.catch_up(300_000); // ~14000 SPC cycles
        assert_eq!(apu.read_port(0), 0xAA);
        assert_eq!(apu.read_port(1), 0xBB);
    }

    #[test]
    fn generates_samples_at_32khz() {
        // The DSP emits one stereo sample per 32 SPC cycles. Advancing ~1 PAL
        // frame of master clock must drain roughly (SPC cycles / 32) samples.
        let mut apu = Apu::new();
        apu.set_region(21_281_370);
        let frame_master = 21_281_370 / 50;
        apu.catch_up(frame_master as u64);
        let mut out = Vec::new();
        apu.drain_samples(&mut out);
        // ~1.024e6/50/32 = 640 samples; allow slack for the cycle budget.
        assert!(out.len() > 500 && out.len() < 800, "sample count {}", out.len());
        // Draining again yields nothing until more time elapses.
        let mut out2 = Vec::new();
        apu.drain_samples(&mut out2);
        assert!(out2.is_empty());
        assert_eq!(apu.sample_rate(), 32_000);
    }

    #[test]
    fn dsp_register_roundtrip_via_ports() {
        // SPC writes DSP addr/data through $F2/$F3; the stub stores and returns.
        let mut apu = Apu::new();
        // Drive the SpcMem directly.
        let Apu { ram, dsp, timers, test, control, dspaddr, ipl_enabled, ports_from_cpu, ports_to_cpu, .. } =
            &mut apu;
        let mut mem = SpcMem {
            ram,
            dsp,
            timers,
            test,
            control,
            dspaddr,
            ipl_enabled,
            ports_from_cpu,
            ports_to_cpu,
        };
        mem.write(0x00F2, 0x4C); // select KON
        mem.write(0x00F3, 0x55);
        mem.write(0x00F2, 0x4C);
        assert_eq!(mem.read(0x00F3), 0x55);
        // bit7 in DSPADDR makes data write-only.
        mem.write(0x00F2, 0x80 | 0x0C);
        mem.write(0x00F3, 0xEE); // dropped
        mem.write(0x00F2, 0x0C);
        assert_ne!(mem.read(0x00F3), 0xEE);
    }
}
