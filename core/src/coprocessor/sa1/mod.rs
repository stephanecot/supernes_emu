//! SA-1 (Super Accelerator 1) cartridge coprocessor.
//!
//! A second 65C816 (reusing `crate::cpu::Cpu`) plus the Super MMC mapper, an
//! arithmetic unit, a variable-length bit reader, an H/V timer, a DMA controller
//! and 2 KB of on-chip I-RAM. Behaviour follows
//! `.claude/skills/snes-refs/references/sa1.md` (verified against bsnes
//! `sfc/coprocessor/sa1/`).
//!
//! # Integration model (for `bus.rs` / `cartridge/`)
//!
//! * **ROM sharing**: the SA-1 does NOT own the cartridge ROM. Like the SuperFX
//!   `SuperFx`, the owner (cartridge) passes `&[u8]` ROM into every entry point
//!   that can touch it: [`Sa1::run`], [`Sa1::read_io`], [`Sa1::write_io`]. For
//!   S-CPU ROM fetches, translate the bus address with [`Sa1::rom_offset`] and
//!   index the cart's ROM directly.
//! * **BW-RAM ownership**: the SA-1 OWNS the battery-backed BW-RAM (up to 256 KB).
//!   Route S-CPU BW-RAM linear ($40-$4F) and window ($6000-$7FFF) accesses through
//!   [`Sa1::read_bwram`]/[`Sa1::write_bwram_scpu`] and
//!   [`Sa1::scpu_window_offset`]. Persist the battery via [`Sa1::bwram`] /
//!   [`Sa1::bwram_mut`].
//! * **I-RAM**: shared 2 KB. S-CPU $3000-$37FF routes to [`Sa1::read_iram`] /
//!   [`Sa1::write_iram_scpu`].
//! * **Interrupts to the S-CPU**: OR [`Sa1::scpu_irq_line`] into the S-CPU IRQ
//!   level. When the S-CPU takes an IRQ/NMI, consult [`Sa1::scpu_irq_vector`] /
//!   [`Sa1::scpu_nmi_vector`]; `Some(v)` overrides the ROM vector.
//! * **Catch-up**: after the S-CPU advances `n` master cycles, call
//!   `sa1.run(rom, n / 2)` (SA-1 = master/2). Deterministic, like the APU/SuperFX.

pub mod cpu;
pub mod io;
pub mod math;
pub mod mmc;

#[cfg(test)]
mod tests;

use crate::cpu::Cpu;
use math::{ArithUnit, BitReader};
use mmc::Mmc;
use serde::{Deserialize, Serialize};

/// Default / maximum BW-RAM size (256 KB).
pub const BWRAM_MAX: usize = 0x40000;

/// Detect an SA-1 cart from the ROM header bytes: map_mode ($FFD5) low nibble $3,
/// or chipset ($FFD6) high nibble $3. `sa1.md` §8.
pub fn is_sa1(map_mode: u8, chipset: u8) -> bool {
    (map_mode & 0x0F) == 0x03 || (chipset & 0xF0) == 0x30
}

/// SA-1 coprocessor: the SA-1 CPU plus all shared/private state.
#[derive(Serialize, Deserialize)]
pub struct Sa1 {
    /// The SA-1's own 65C816 (independent register file from the S-CPU).
    pub(crate) cpu: Cpu,
    pub(crate) st: Sa1State,
    /// `--trace-sa1` sink: fires once per SA-1 instruction, immediately before
    /// it executes. Host-side tap, not part of the serialized state.
    #[serde(skip)]
    trace: Option<Box<dyn FnMut(&str)>>,
}

/// Everything the SA-1 owns except the CPU register file (kept separate so the
/// `Sa1Bus` can borrow it mutably while `cpu.step` runs).
#[derive(Serialize, Deserialize)]
pub struct Sa1State {
    /// On-chip 2 KB I-RAM (shared: S-CPU $3000-$37FF, SA-1 $0000-$07FF/$3000-$37FF).
    #[serde(with = "crate::serde_util::boxed_bytes")]
    pub(crate) iram: Box<[u8; 0x800]>,
    /// Battery-backed BW-RAM (up to 256 KB).
    pub(crate) bwram: Vec<u8>,

    pub(crate) mmc: Mmc,
    pub(crate) arith: ArithUnit,
    pub(crate) bits: BitReader,

    // ---- CPU control / reset / wait ($2200) ----
    /// SA-1 held in reset (CCNT bit5). On the 1->0 edge the SA-1 CPU is reset.
    pub(crate) sa1_reset: bool,
    /// SA-1 halted / waiting (CCNT bit6).
    pub(crate) sa1_wait: bool,
    /// Latched reset request; consumed at the next `run`.
    #[serde(default)]
    pub(crate) reset_pending: bool,

    // ---- Message ports ----
    /// 4-bit message S-CPU -> SA-1 (CCNT bits 0-3, read in CFR).
    pub(crate) msg_to_sa1: u8,
    /// 4-bit message SA-1 -> S-CPU (SCNT bits 0-3, read in SFR).
    pub(crate) msg_to_scpu: u8,

    // ---- SA-1 CPU vectors ($2203-$2208) ----
    pub(crate) crv: u16,
    pub(crate) cnv: u16,
    pub(crate) civ: u16,

    // ---- S-CPU vector overrides ($220C-$220F), selected by SCNT ----
    pub(crate) snv: u16,
    pub(crate) siv: u16,
    /// SCNT bit6 S: S-CPU IRQ vector source (true = SIV override).
    pub(crate) scpu_irq_sel: bool,
    /// SCNT bit4 N: S-CPU NMI vector source (true = SNV override).
    pub(crate) scpu_nmi_sel: bool,

    // ---- Interrupt enables ----
    /// SIE ($2201): S-CPU IRQ enables. bit7 I (from SA-1), bit5 C (char-conv DMA).
    pub(crate) sie: u8,
    /// CIE ($220A): SA-1 IRQ enables. bit7 I, bit6 T, bit5 D, bit4 N.
    pub(crate) cie: u8,

    // ---- Pending interrupt flags ----
    /// SFR.I: SA-1 -> S-CPU IRQ pending.
    pub(crate) sfr_irq: bool,
    /// SFR.D: char-conversion DMA IRQ pending (to S-CPU).
    pub(crate) sfr_dma: bool,
    /// CFR.I: S-CPU -> SA-1 IRQ pending.
    pub(crate) cfr_irq: bool,
    /// CFR.T: timer IRQ pending.
    pub(crate) cfr_timer: bool,
    /// CFR.D: SA-1 DMA-end IRQ pending.
    pub(crate) cfr_dma: bool,
    /// CFR.N: S-CPU -> SA-1 NMI pending.
    pub(crate) cfr_nmi: bool,
    /// Previous SA-1 NMI line level, for edge detection in `take_nmi`.
    #[serde(default)]
    pub(crate) nmi_prev: bool,

    // ---- H/V timer ($2210-$2215) ----
    /// TMC: bit7 mode (0=H/V, 1=linear 18-bit), bit1 V-enable, bit0 H-enable.
    pub(crate) tmc: u8,
    pub(crate) h_target: u16,
    pub(crate) v_target: u16,
    pub(crate) h_count: u16,
    pub(crate) v_count: u16,
    /// Free-running 18-bit counter (linear timer mode).
    pub(crate) linear: u32,
    /// Latched H/V counters (read $2302-$2305).
    pub(crate) h_latch: u16,
    pub(crate) v_latch: u16,

    // ---- BW-RAM / I-RAM protection ($2226-$222A) ----
    /// SBWE bit7: S-CPU BW-RAM write enable.
    pub(crate) sbwe: bool,
    /// CBWE bit7: SA-1 BW-RAM write enable.
    pub(crate) cbwe: bool,
    /// BWPA low nibble: protected-area exponent (first 256*2^AAAA bytes).
    pub(crate) bwpa: u8,
    /// SIWP: S-CPU I-RAM per-256-byte-page write enable.
    pub(crate) siwp: u8,
    /// CIWP: SA-1 I-RAM per-256-byte-page write enable.
    pub(crate) ciwp: u8,

    // ---- DMA ($2230-$224F) ----
    pub(crate) dcnt: u8,
    pub(crate) cdma: u8,
    pub(crate) dma_src: u32,
    pub(crate) dma_dst: u32,
    pub(crate) dma_cnt: u16,
    /// BBF ($223F) bit7: 0 = 4bpp bitmap, 1 = 2bpp bitmap.
    pub(crate) bbf: u8,
    /// Bitmap register file BRF0-15 ($2240-$224F).
    pub(crate) brf: [u8; 16],
}

impl Sa1 {
    pub fn new(bwram_size: usize) -> Self {
        let size = bwram_size.clamp(0x800, BWRAM_MAX);
        Sa1 {
            cpu: Cpu::new(),
            trace: None,
            st: Sa1State {
                iram: Box::new([0; 0x800]),
                bwram: vec![0; size],
                mmc: Mmc::default(),
                arith: ArithUnit::default(),
                bits: BitReader::default(),
                sa1_reset: true,
                sa1_wait: false,
                reset_pending: false,
                msg_to_sa1: 0,
                msg_to_scpu: 0,
                crv: 0,
                cnv: 0,
                civ: 0,
                snv: 0,
                siv: 0,
                scpu_irq_sel: false,
                scpu_nmi_sel: false,
                sie: 0,
                cie: 0,
                sfr_irq: false,
                sfr_dma: false,
                cfr_irq: false,
                cfr_timer: false,
                cfr_dma: false,
                cfr_nmi: false,
                nmi_prev: false,
                tmc: 0,
                h_target: 0,
                v_target: 0,
                h_count: 0,
                v_count: 0,
                linear: 0,
                h_latch: 0,
                v_latch: 0,
                sbwe: false,
                cbwe: false,
                bwpa: 0,
                siwp: 0,
                ciwp: 0,
                dcnt: 0,
                cdma: 0,
                dma_src: 0,
                dma_dst: 0,
                dma_cnt: 0,
                bbf: 0,
                brf: [0; 16],
            },
        }
    }

    /// Install a `--trace-sa1` sink (one call per SA-1 instruction, before it
    /// executes). Mirrors `Snes::set_gsu_trace`; not part of the save state.
    pub fn set_trace(&mut self, sink: Box<dyn FnMut(&str)>) {
        self.trace = Some(sink);
    }

    /// Remove the SA-1 trace sink; drop the returned box to flush its writer.
    pub fn clear_trace(&mut self) -> Option<Box<dyn FnMut(&str)>> {
        self.trace.take()
    }

    // ---- Catch-up ----------------------------------------------------------

    /// True when the SA-1 CPU is actively executing: not held in reset (CCNT.r)
    /// and not halted/waiting (CCNT.R). The catch-up driver rebases its clock
    /// while this is false so a later release does not receive a retroactive
    /// budget covering the whole idle span.
    pub fn is_running(&self) -> bool {
        (!self.st.sa1_reset || self.st.reset_pending) && !self.st.sa1_wait
    }

    /// Advance the SA-1 by up to `budget` SA-1 cycles (= master cycles / 2).
    /// No-op while held in reset (CCNT.r) or halted (CCNT.R). Deterministic.
    pub fn run(&mut self, rom: &[u8], budget: i64) {
        if self.st.reset_pending {
            self.st.reset_pending = false;
            let mut bus = cpu::Sa1Bus::new(&mut self.st, rom);
            self.cpu.reset(&mut bus);
        }
        if self.st.sa1_reset {
            return;
        }
        let mut remaining = budget;
        while remaining > 0 {
            if self.st.sa1_wait {
                break;
            }
            if let Some(sink) = self.trace.as_mut() {
                let st = &self.st;
                let mut fetch = |a: u32| st.fetch_no_tick(rom, a);
                let line = crate::debug::trace::trace_line(&self.cpu, &mut fetch);
                sink(&line);
            }
            let mut bus = cpu::Sa1Bus::new(&mut self.st, rom);
            self.cpu.step(&mut bus);
            let used = bus.cycles.max(1);
            remaining -= used as i64;
            self.st.tick_timer(used);
        }
    }

    // ---- S-CPU-facing I/O ($2200-$23FF) ------------------------------------

    /// Read an SA-1 I/O register. `addr` is the full bus address ($2200-$23FF).
    pub fn read_io(&mut self, rom: &[u8], addr: u16) -> u8 {
        self.st.read_io(rom, addr)
    }

    /// Write an SA-1 I/O register. May start the arithmetic unit, a DMA, arm the
    /// bit reader, reset/halt the SA-1 CPU, or raise/clear interrupts.
    pub fn write_io(&mut self, rom: &[u8], addr: u16, value: u8) {
        self.st.write_io(rom, addr, value);
    }

    // ---- ROM / BW-RAM / I-RAM routing --------------------------------------

    /// Translate a bus address to a linear ROM offset via the Super MMC, or
    /// `None` if not a ROM region. Same mapping for both CPUs.
    pub fn rom_offset(&self, addr: u32) -> Option<usize> {
        self.st.mmc.rom_offset(addr)
    }

    /// Compute the linear BW-RAM offset for an S-CPU $6000-$7FFF window access.
    pub fn scpu_window_offset(&self, addr: u16) -> usize {
        self.st.mmc.scpu_window_offset(addr)
    }

    /// Read BW-RAM at a linear offset (mirrored to the actual size).
    pub fn read_bwram(&self, offset: usize) -> u8 {
        if self.st.bwram.is_empty() {
            0
        } else {
            self.st.bwram[offset % self.st.bwram.len()]
        }
    }

    /// S-CPU BW-RAM write, honouring SBWE and the BWPA protected area.
    pub fn write_bwram_scpu(&mut self, offset: usize, value: u8) {
        self.st.write_bwram(true, offset, value);
    }

    /// Read I-RAM (offset 0-0x7FF).
    pub fn read_iram(&self, off: usize) -> u8 {
        self.st.iram[off & 0x7FF]
    }

    /// S-CPU I-RAM write, honouring the SIWP per-page write protection.
    pub fn write_iram_scpu(&mut self, off: usize, value: u8) {
        self.st.write_iram(true, off, value);
    }

    // ---- Interrupt lines to the S-CPU --------------------------------------

    /// Level of the SA-1 -> S-CPU IRQ line (OR into the S-CPU IRQ level).
    pub fn scpu_irq_line(&self) -> bool {
        (self.st.sfr_irq && self.st.sie & 0x80 != 0)
            || (self.st.sfr_dma && self.st.sie & 0x20 != 0)
    }

    /// S-CPU IRQ vector override, or `None` to use the ROM vector.
    pub fn scpu_irq_vector(&self) -> Option<u16> {
        if self.st.scpu_irq_sel {
            Some(self.st.siv)
        } else {
            None
        }
    }

    /// S-CPU NMI vector override, or `None` to use the ROM vector.
    pub fn scpu_nmi_vector(&self) -> Option<u16> {
        if self.st.scpu_nmi_sel {
            Some(self.st.snv)
        } else {
            None
        }
    }

    // ---- Battery / save-state accessors ------------------------------------

    pub fn bwram(&self) -> &[u8] {
        &self.st.bwram
    }

    pub fn bwram_mut(&mut self) -> &mut [u8] {
        &mut self.st.bwram
    }

    pub fn bwram_size(&self) -> usize {
        self.st.bwram.len()
    }
}
