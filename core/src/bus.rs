//! System bus: full address decode (banks/mirrors), master-clock advance per
//! access with the region speed table, open-bus MDR, WRAM port $2180-$2183.

use crate::apu::Apu;
use crate::cartridge::Cartridge;
use crate::cpu::CpuBus;
use crate::dma::Dma;
use crate::joypad::{Joypad, JoypadState};
use crate::ppu::Ppu;
use crate::scheduler::{Region, Scheduler};

pub const WRAM_SIZE: usize = 0x20000; // 128 KB

/// Opt-in debug taps used by the frontend's `--log-mmio` / `--watch` flags.
/// All checks are behind a `false`/empty guard so the normal (untraced) access
/// path stays branch-cheap and allocation-free.
#[derive(Default)]
pub struct DebugHooks {
    /// Log every write to a named $21xx/$42xx/$43xx register to stderr.
    pub log_mmio: bool,
    /// 24-bit bus addresses whose every read/write is logged to stderr.
    pub watch: Vec<u32>,
}

/// $4210 RDNMI bits3-0: fixed CPU version field. Real SNES units report 1 or
/// 2 (early vs. later 5A22 revisions); Nintendo stopped incrementing after 2,
/// which is what Super Mario World's boot code expects (mmio.md ยง8).
const CPU_VERSION: u8 = 2;

/// $4212.0 auto-joypad-busy stays set for 4224 master cycles (~3.1 scanlines)
/// from the read start at vblank (timing.md §7-8).
const AUTO_JOYPAD_CYCLES: u64 = 4224;

/// Master-cycle offset from the vblank line start (H=0) to the auto-joypad read
/// start. Hardware begins between H=32.5 and H=95.5 (H=74.5 on the first frame);
/// 74.5 dots * 4 = 298 master cycles (timing.md §8). The JOY snapshot value is
/// input-latch-invariant within a frame, so only the busy window is offset here.
const AUTO_JOYPAD_START_OFFSET: u64 = 298;

/// GP-DMA master-cycle costs (timing.md §10). Alignment padding (2-8 cycles
/// before/after each pause) is not modeled; documented approximation.
const GDMA_WHOLE_OVERHEAD: u64 = 8;
const GDMA_CHANNEL_OVERHEAD: u64 = 8;
const GDMA_PER_BYTE: u64 = 8;

/// HDMA master-cycle costs (timing.md §11). The per-frame init and per-line
/// overheads are the reference's "~18"; alignment padding is not modeled
/// (documented approximation, same as GP-DMA).
const HDMA_INIT_OVERHEAD: u64 = 18;
const HDMA_LINE_OVERHEAD: u64 = 18;
const HDMA_CHANNEL_OVERHEAD: u64 = 8;
const HDMA_INDIRECT_RELOAD: u64 = 16;
const HDMA_PER_BYTE: u64 = 8;

pub struct Bus {
    pub wram: Box<[u8; WRAM_SIZE]>,
    pub cart: Cartridge,
    pub ppu: Ppu,
    pub apu: Apu,
    pub dma: Dma,
    pub joypads: [Joypad; 2],
    pub scheduler: Scheduler,
    /// Memory data register: last byte driven on the data bus (open-bus reads
    /// return this).
    pub mdr: u8,
    /// $420D MEMSEL bit0: FastROM (6 cycles) in banks $80-$FF.
    pub fastrom: bool,
    /// WRAM port address for $2180 (WMDATA), 17 bits, auto-increment.
    wram_addr: u32,
    /// $4200 NMITIMEN mirror. Bits7/5-4 are forwarded to the scheduler as
    /// authoritative state on write; bit0 (auto-joypad enable) is read back
    /// from here when the scheduler pulses `auto_joypad_pending`.
    nmitimen: u8,
    /// $4201 WRIO mirror. No external I/O device modeled: $4213 RDIO loops
    /// back what was last written (bits7-6), matching an open-collector pin
    /// with no external pulldown (mmio.md ยง7-8).
    wrio: u8,
    /// $4202 WRMPYA: 8-bit multiplicand, latched until $4203 starts the
    /// multiply (mmio.md ยง7).
    wrmpya: u8,
    /// $4204/$4205 WRDIVL/H: 16-bit dividend, latched until $4206 starts the
    /// divide.
    wrdiv: u16,
    /// $4214/$4215 RDDIVL/H: divide quotient. Also destroyed by a $4203
    /// write per the WRMPYB quirk (mmio.md ยง7 "Multiply/divide latency").
    rddiv: u16,
    /// $4216/$4217 RDMPYL/H: multiply product, or divide remainder.
    rdmpy: u16,
    /// $4218-$421F JOY1-4 auto-read results, one 16-bit word per
    /// port/data-line pair.
    joy: [u16; 4],
    /// Next visible scanline (scheduler V, 1..=224) that has NOT yet been
    /// rendered this frame. `post_tick` renders every visible line the moment
    /// the scheduler advances past it, so mid-frame $21xx writes only affect
    /// later lines (raster effects). Reset to 1 while the scheduler is on the
    /// pre-render line (V=0).
    render_line: u16,
    /// Master-clock timestamp at which $4212.0 (auto-joypad busy) clears; 0
    /// when no auto-read is in progress.
    auto_joypad_busy_until: u64,
    /// Next scheduler line V (0..=vblank_line-1) whose HDMA per-line transfer
    /// (H=278) has not yet run this frame. Analogous to `render_line`: HDMA
    /// for line V runs the moment the scheduler advances past V, so its PPU
    /// writes land before the affected line (V+1) is rendered (timing.md §11).
    hdma_line: u16,
    /// True once HDMA has been initialized for the current frame (on the first
    /// post-vblank re-entry into the visible region); reset while V is in
    /// vblank so the next frame re-inits.
    hdma_inited: bool,
    /// Re-entrancy guard: HDMA advances the master clock via `dma_tick`, whose
    /// `post_tick` would otherwise recurse into HDMA. GP-DMA is intentionally
    /// NOT guarded so a mid-GP-DMA HDMA transfer point preempts it (timing.md
    /// §10 HDMA priority).
    hdma_running: bool,
    /// Opt-in stderr taps for the frontend debug flags.
    pub debug: DebugHooks,
}

impl Bus {
    pub fn new(cart: Cartridge) -> Self {
        let region = cart.region;
        let mut apu = Apu::new();
        // The SPC700 runs off a fixed 1.024 MHz clock; give the APU the region
        // master clock so its catch-up ratio is exact for PAL as well as NTSC.
        apu.set_region(region.master_clock_hz());
        let mut ppu = Ppu::new();
        // $213F STAT78 bit4 must reflect the console region so region-detecting
        // boot code takes the correct 50/60 Hz init path.
        ppu.is_pal = region == Region::Pal;
        Bus {
            wram: vec![0u8; WRAM_SIZE].into_boxed_slice().try_into().unwrap(),
            cart,
            ppu,
            apu,
            dma: Dma::new(),
            joypads: [Joypad::new(), Joypad::new()],
            scheduler: Scheduler::new(region),
            mdr: 0,
            fastrom: false,
            wram_addr: 0,
            nmitimen: 0,
            wrio: 0xFF,
            wrmpya: 0xFF,
            wrdiv: 0xFFFF,
            rddiv: 0,
            rdmpy: 0,
            joy: [0; 4],
            render_line: 1,
            auto_joypad_busy_until: 0,
            hdma_line: 0,
            hdma_inited: false,
            hdma_running: false,
            debug: DebugHooks::default(),
        }
    }

    pub fn set_inputs(&mut self, inputs: [JoypadState; 2]) {
        self.joypads[0].state = inputs[0];
        self.joypads[1].state = inputs[1];
    }

    /// Master-cycle cost of one CPU access at `addr`:
    /// 6 fast ($2000-$3FFF, $4200-$5FFF, FastROM banks $80+), 8 slow (WRAM,
    /// $6000-$7FFF, SlowROM), 12 ($4000-$41FF joypad region).
    pub fn access_speed(addr: u32, fastrom: bool) -> u64 {
        let bank = (addr >> 16) as u8;
        let off = (addr & 0xFFFF) as u16;
        match bank {
            0x00..=0x3F => match off {
                0x0000..=0x1FFF => 8,
                0x2000..=0x3FFF => 6,
                0x4000..=0x41FF => 12,
                0x4200..=0x5FFF => 6,
                _ => 8,
            },
            0x40..=0x7F => 8,
            0x80..=0xBF => match off {
                0x0000..=0x1FFF => 8,
                0x2000..=0x3FFF => 6,
                0x4000..=0x41FF => 12,
                0x4200..=0x5FFF => 6,
                0x6000..=0x7FFF => 8,
                _ => {
                    if fastrom {
                        6
                    } else {
                        8
                    }
                }
            },
            _ => {
                if fastrom {
                    6
                } else {
                    8
                }
            }
        }
    }

    /// Read without advancing the clock (used by DMA later and by debug tools).
    /// Still updates open-bus/WRAM-port side effects.
    pub fn read_no_tick(&mut self, addr: u32) -> u8 {
        let bank = (addr >> 16) as u8 & 0xFF;
        let off = (addr & 0xFFFF) as u16;
        match bank {
            0x00..=0x3F | 0x80..=0xBF => match off {
                // First 8KB of WRAM mirrored in every system bank.
                0x0000..=0x1FFF => self.wram[off as usize],
                0x2100..=0x213F => {
                    // $2137 SLHV latches the live H/V counters into OPHCT/OPVCT,
                    // but only when $4201 (WRIO) bit7 is set: every H/V-counter
                    // latch trigger is gated by WRIO.7 (fullsnes: "working only
                    // if WRIO.Bit7 is (or was) set"). Feed the scheduler H/V
                    // first. 1 dot = 4 master cycles. $2137 always drives CPU
                    // open bus (mmio.md §7).
                    if off == 0x2137 {
                        if self.wrio & 0x80 != 0 {
                            // OPHCT range is 0-339 (fullsnes); a latch in the
                            // final ~2 dots of the 1364-cycle line would compute
                            // 340 without the long-dot layout, so clamp.
                            let h = ((self.scheduler.h_cycles() / 4) as u16).min(339);
                            self.ppu.set_hv_counters(h, self.scheduler.v);
                            self.ppu.read(0x37);
                        }
                        self.mdr
                    } else {
                        self.ppu.read((off & 0xFF) as u8).unwrap_or(self.mdr)
                    }
                }
                // APU ports, mirrored every 4 bytes across $2140-$217F. Catch
                // the APU up to the current master-clock time before every
                // access so its port state reflects everything it has run so
                // far (lazy catch-up sync, ARCHITECTURE.md).
                0x2140..=0x217F => {
                    self.apu.catch_up(self.scheduler.clock);
                    self.apu.read_port((off & 3) as u8)
                }
                // $2180 WMDATA: read WRAM at the 17-bit port address, post-increment.
                0x2180 => {
                    let v = self.wram[(self.wram_addr & 0x1FFFF) as usize];
                    self.wram_addr = (self.wram_addr + 1) & 0x1FFFF;
                    v
                }
                // $2181-$2183 are write-only: open bus.
                // Divergence (timing.md §8, mmio.md line 114): on hardware,
                // reading $4016/$4017 or $4218-$421F while the auto-joypad busy
                // window ($4212.0) is active returns values corrupted by the
                // auto-read shift state machine. We return the clean values
                // regardless; games are required to poll $4212.0 first, so a
                // correctly-written game never observes the difference, and
                // returning open-bus garbage risks breaking a game that reads
                // without polling. Not modeled by design.
                // $4016 JOYA: bit0 = port1 data1, bit1 = port1 data2 (no
                // multitap modeled -> 0), bits7-2 open bus (mmio.md §6).
                0x4016 => (self.mdr & 0xFC) | (self.joypads[0].read() & 1),
                // $4017 JOYB: bit0 = port2 data1, bit1 = data2 (0), bits4-2
                // always read 1 (tied to GND, active-low), bits7-5 open bus.
                0x4017 => (self.mdr & 0xE0) | 0x1C | (self.joypads[1].read() & 1),
                // $4210-$421F: read-only CPU registers with real semantics.
                0x4210 => self.read_rdnmi(),
                0x4211 => self.read_timeup(),
                0x4212 => self.read_hvbjoy(),
                // $4213 RDIO: bits7-6 loop back $4201 WRIO IOBits (no external
                // device; open-collector reads back what was written); bits5-0
                // unconnected, "read as set by $4201" (mmio.md §7-8).
                0x4213 => self.wrio,
                0x4214 => (self.rddiv & 0xFF) as u8,
                0x4215 => (self.rddiv >> 8) as u8,
                0x4216 => (self.rdmpy & 0xFF) as u8,
                0x4217 => (self.rdmpy >> 8) as u8,
                0x4218 => (self.joy[0] & 0xFF) as u8,
                0x4219 => (self.joy[0] >> 8) as u8,
                0x421A => (self.joy[1] & 0xFF) as u8,
                0x421B => (self.joy[1] >> 8) as u8,
                0x421C => (self.joy[2] & 0xFF) as u8,
                0x421D => (self.joy[2] >> 8) as u8,
                0x421E => (self.joy[3] & 0xFF) as u8,
                0x421F => (self.joy[3] >> 8) as u8,
                0x4300..=0x437F => {
                    self.dma.read((off & 0x7F) as u8).unwrap_or(self.mdr)
                }
                // Cartridge regions: $6000-$7FFF (HiROM SRAM window / LoROM
                // expansion) and $8000-$FFFF ROM.
                0x6000..=0xFFFF => self.cart.read(addr).unwrap_or(self.mdr),
                // Everything else ($2000-$20FF, $2184-$3FFF, $4000-$41FF gaps,
                // $4200-$420F write-only NMITIMEN..MEMSEL, $4220-$42FF,
                // $4380-$5FFF): open bus.
                _ => self.mdr,
            },
            // Banks $7E-$7F: 128KB WRAM.
            0x7E | 0x7F => {
                self.wram[((bank as usize - 0x7E) << 16) | off as usize]
            }
            // Banks $40-$7D, $C0-$FF: cartridge.
            _ => self.cart.read(addr).unwrap_or(self.mdr),
        }
    }

    pub fn write_no_tick(&mut self, addr: u32, value: u8) {
        let bank = (addr >> 16) as u8 & 0xFF;
        let off = (addr & 0xFFFF) as u16;
        match bank {
            0x00..=0x3F | 0x80..=0xBF => match off {
                0x0000..=0x1FFF => self.wram[off as usize] = value,
                0x2100..=0x213F => {
                    self.ppu.write((off & 0xFF) as u8, value);
                    // $2133 bit2 (overscan) moves vblank/NMI/auto-joypad to
                    // V=240 (timing.md §2/§4).
                    if off == 0x2133 {
                        self.scheduler.set_overscan(value & 0x04 != 0);
                    }
                }
                0x2140..=0x217F => {
                    self.apu.catch_up(self.scheduler.clock);
                    self.apu.write_port((off & 3) as u8, value)
                }
                // WRAM port: $2180 data, $2181/82/83 address low/mid/high (bit0).
                0x2180 => {
                    self.wram[(self.wram_addr & 0x1FFFF) as usize] = value;
                    self.wram_addr = (self.wram_addr + 1) & 0x1FFFF;
                }
                0x2181 => {
                    self.wram_addr = (self.wram_addr & 0x1FF00) | value as u32;
                }
                0x2182 => {
                    self.wram_addr =
                        (self.wram_addr & 0x100FF) | ((value as u32) << 8);
                }
                0x2183 => {
                    self.wram_addr =
                        (self.wram_addr & 0x0FFFF) | (((value & 1) as u32) << 16);
                }
                // $4016 bit0 = OUT0, the shared latch line to BOTH ports.
                0x4016 => {
                    self.joypads[0].write_strobe(value);
                    self.joypads[1].write_strobe(value);
                }
                0x4200 => {
                    self.nmitimen = value;
                    // bit7 NMI enable, bits5-4 H/V-IRQ mode, bit0 auto-joypad
                    // enable (read back from `nmitimen` at vblank).
                    self.scheduler.set_nmi_enable(value & 0x80 != 0);
                    self.scheduler.set_irq_mode((value >> 4) & 0x3);
                }
                0x4201 => {
                    // bit7 (port2 IOBit) on a 1->0 transition latches the PPU
                    // H/V counters, exactly like reading $2137 SLHV (mmio.md §7).
                    if self.wrio & 0x80 != 0 && value & 0x80 == 0 {
                        self.latch_hv_counters();
                    }
                    self.wrio = value;
                }
                0x4202 => self.wrmpya = value,
                0x4203 => {
                    // Write to WRMPYB starts the 8x8 unsigned multiply.
                    // STUB: real hardware takes 8 CPU cycles (mmio.md ยง7);
                    // modeled as instantaneous here.
                    self.rdmpy = self.wrmpya as u16 * value as u16;
                    // Quirk (mmio.md ยง7): writing $4203 also destroys
                    // $4214/5, setting RDDIV = WRMPYB with high byte $00.
                    self.rddiv = value as u16;
                }
                0x4204 => self.wrdiv = (self.wrdiv & 0xFF00) | value as u16,
                0x4205 => self.wrdiv = (self.wrdiv & 0x00FF) | ((value as u16) << 8),
                0x4206 => {
                    // Write to WRDIVB starts the 16/8 unsigned divide.
                    // STUB: real hardware takes 16 CPU cycles (mmio.md ยง7);
                    // modeled as instantaneous here. Divide by zero:
                    // quotient=$FFFF, remainder=dividend.
                    if value == 0 {
                        self.rddiv = 0xFFFF;
                        self.rdmpy = self.wrdiv;
                    } else {
                        self.rddiv = self.wrdiv / value as u16;
                        self.rdmpy = self.wrdiv % value as u16;
                    }
                }
                0x4207 => self.scheduler.set_htime_lo(value),
                0x4208 => self.scheduler.set_htime_hi(value),
                0x4209 => self.scheduler.set_vtime_lo(value),
                0x420A => self.scheduler.set_vtime_hi(value),
                // $420B/$420C: DMA enables (transfers execute in M3).
                0x420B => self.dma.mdmaen = value,
                0x420C => {
                    // Writing $420C mid-frame activates/deactivates channels
                    // immediately (timing.md §11): the per-line pass re-reads
                    // HDMAEN each line, so clearing a bit stops that channel
                    // this frame and setting one starts it. A channel started
                    // mid-frame is NOT auto-initialized (software must set
                    // A2A/$43xA itself). fullsnes quirk: if HDMAEN was already
                    // nonzero, a newly started channel begins with do_transfer=1.
                    let prev = self.dma.hdmaen;
                    self.dma.hdmaen = value;
                    let newly = value & !prev;
                    for ch in 0..8 {
                        if newly & (1 << ch) != 0 {
                            self.dma.set_hdma_channel_active(ch, true);
                            if prev != 0 {
                                self.dma.set_hdma_wants_transfer(ch, true);
                            }
                        }
                    }
                }
                // $420D MEMSEL bit0: FastROM enable.
                0x420D => self.fastrom = value & 1 != 0,
                // $420E-$421F: read-only/unused, writes have no effect.
                0x4300..=0x437F => self.dma.write((off & 0x7F) as u8, value),
                0x6000..=0xFFFF => self.cart.write(addr, value),
                _ => {}
            },
            0x7E | 0x7F => {
                self.wram[((bank as usize - 0x7E) << 16) | off as usize] = value
            }
            _ => self.cart.write(addr, value),
        }
    }

    /// $4210 RDNMI: bit7 = vblank-NMI-occurred flag (set unconditionally at
    /// vblank start, even with NMI disabled), bits6-4 open bus, bits3-0 = CPU
    /// version. Reading clears bit7 but does NOT cancel an already-latched
    /// CPU NMI (`scheduler.nmi_pending`) (timing.md ยง5).
    fn read_rdnmi(&mut self) -> u8 {
        let mut v = (self.mdr & 0x70) | CPU_VERSION;
        if self.scheduler.vblank_nmi_flag {
            v |= 0x80;
        }
        self.scheduler.vblank_nmi_flag = false;
        v
    }

    /// $4211 TIMEUP: bit7 = H/V-IRQ occurred, bits6-0 open bus. Read-clear
    /// (read-ack); does not model the sub-line read-ack race of timing.md ยง6.
    fn read_timeup(&mut self) -> u8 {
        let mut v = self.mdr & 0x7F;
        if self.scheduler.irq_pending {
            v |= 0x80;
        }
        self.scheduler.irq_pending = false;
        v
    }

    /// $4212 HVBJOY: bit7 vblank, bit6 hblank, bits5-1 open bus, bit0
    /// auto-joypad-busy. Busy is held for AUTO_JOYPAD_CYCLES from the vblank
    /// auto-read start; the JOY snapshot itself is taken instantaneously at
    /// that start (documented approximation, timing.md 7-8).
    fn read_hvbjoy(&self) -> u8 {
        let mut v = self.mdr & 0x3E;
        if self.scheduler.in_vblank {
            v |= 0x80;
        }
        if self.scheduler.hblank() {
            v |= 0x40;
        }
        // bit0 auto-joypad busy: held for AUTO_JOYPAD_CYCLES from read start.
        if self.scheduler.clock < self.auto_joypad_busy_until {
            v |= 0x01;
        }
        v
    }

    /// Snapshot both pads' 16-bit serial state into JOY1/JOY2 ($4218-$421B).
    /// JOY3/JOY4 (2nd data line per port, multitap) are left at 0: no
    /// multitap support. Called by the bus when the scheduler pulses
    /// `auto_joypad_pending` at vblank start, gated on $4200 bit0
    /// (mmio.md ยง8, timing.md ยง8).
    fn latch_auto_joypad(&mut self) {
        self.joy[0] = self.joypads[0].state.to_bits();
        self.joy[1] = self.joypads[1].state.to_bits();
        // Auto-read physically clocks the shared serial line 16 times, leaving
        // the manual $4016/$4017 shift registers exhausted (timing.md §8).
        self.joypads[0].auto_read_shift();
        self.joypads[1].auto_read_shift();
    }

    /// Latch the PPU H/V counters exactly as a $2137 SLHV read does, feeding
    /// the live scheduler H/V first. Called on the $4201 bit7 1->0 edge
    /// (mmio.md §7). The WRIO.7 gate that guards $2137 is inherently satisfied
    /// here: the pin "was set" immediately before this falling edge. 1 dot =
    /// 4 master cycles.
    fn latch_hv_counters(&mut self) {
        // OPHCT range 0-339 (fullsnes); clamp the flat dot count (see $2137).
        let h = ((self.scheduler.h_cycles() / 4) as u16).min(339);
        self.ppu.set_hv_counters(h, self.scheduler.v);
        self.ppu.read(0x37);
    }

    /// Runs once after every `scheduler.tick()` call: renders newly-completed
    /// visible scanlines, and consumes the auto-joypad and per-line
    /// APU-catch-up pulses the scheduler raises. Kept `pub(crate)` so the
    /// `Snes` frame loop can drive it on its no-CPU-progress guard path.
    pub(crate) fn post_tick(&mut self) {
        if self.scheduler.auto_joypad_pending {
            self.scheduler.auto_joypad_pending = false;
            // Auto-read requires $4200.0 set AND OUT0 (strobe) low (timing.md
            // 8, PUNCHLIST M5). Busy flag then held for 4224 master cycles.
            if self.nmitimen & 0x01 != 0 && !self.joypads[0].strobe {
                self.latch_auto_joypad();
                // Read starts at H≈74.5 of the vblank line, not H=0 where this
                // pulse fires; anchor the busy window to the line start so its
                // start/end land ~298 cycles later (timing.md §8).
                self.auto_joypad_busy_until = self.scheduler.line_start
                    + AUTO_JOYPAD_START_OFFSET
                    + AUTO_JOYPAD_CYCLES;
            }
        }
        // HDMA (init at V=0, per-line transfers at H=278 of V=0..vblank_line-1)
        // runs BEFORE the render loop: the transfer at the end of line V writes
        // this-line raster registers that must be in place before line V+1
        // composites (timing.md §11 'during hblank, before the line it
        // affects'). Guarded because HDMA advances the clock through `dma_tick`,
        // whose `post_tick` would otherwise re-enter here.
        if !self.hdma_running {
            self.hdma_running = true;
            self.run_hdma();
            self.hdma_running = false;
        }
        // Render every visible line the scheduler has fully passed. The
        // scheduler's V only wraps to 0 on the pre-render line, so `V > line`
        // uniquely marks completion of visible lines 1..=224 within a frame.
        if self.scheduler.v == 0 {
            self.render_line = 1;
        }
        while self.render_line <= 224 && self.scheduler.v > self.render_line {
            // Scheduler V=1..=224 maps to PPU visible row 0..=223.
            self.ppu.render_scanline(self.render_line - 1);
            self.render_line += 1;
        }
        if self.scheduler.line_boundary_crossed {
            self.scheduler.line_boundary_crossed = false;
            // Once-per-scanline APU catch-up in addition to the per-access
            // catch-up on $2140-$217F (ARCHITECTURE.md lazy catch-up sync):
            // keeps the APU roughly in step even during long stretches with
            // no port access (e.g. while the CPU renders or waits on DMA).
            self.apu.catch_up(self.scheduler.clock);
        }
    }

    /// Advance the master clock during a DMA transfer. Uses the same
    /// `tick` + `post_tick` path as CPU accesses so PPU per-line render
    /// events and NMI/auto-joypad pulses still fire mid-transfer.
    fn dma_tick(&mut self, cycles: u64) {
        self.scheduler.tick(cycles);
        self.post_tick();
    }

    /// Execute all channels enabled in $420B (MDMAEN) in channel order 0..7,
    /// CPU stalled. Costs (timing.md §10): 8-cycle whole-DMA overhead, 8
    /// per channel, 8 per byte; alignment padding not modeled. $420B
    /// self-clears at completion. HDMA-priority preemption is not modeled.
    pub fn run_gdma(&mut self) {
        self.dma_tick(GDMA_WHOLE_OVERHEAD);
        for ch in 0..8 {
            if self.dma.mdmaen & (1 << ch) == 0 {
                continue;
            }
            self.dma_tick(GDMA_CHANNEL_OVERHEAD);
            let a_to_b = self.dma.direction_a_to_b(ch);
            let bbad = self.dma.bbad(ch);
            let pattern = self.dma.transfer_pattern(ch);
            let step = self.dma.a_step(ch);
            // Byte counter: $0000 = 65536 (mmio.md §9).
            let mut remaining = self.dma.byte_count(ch);
            let mut p = 0usize;
            while remaining > 0 {
                let b_off = pattern[p % pattern.len()];
                // B-bus target $21xx, xx = (BBADx + pattern offset) & $FF.
                let b_addr = 0x2100 | bbad.wrapping_add(b_off) as u32;
                let a_addr = self.dma.a1_addr(ch);
                if a_to_b {
                    let byte = self.read_no_tick(a_addr);
                    self.mdr = byte;
                    self.write_no_tick(b_addr, byte);
                } else {
                    let byte = self.read_no_tick(b_addr);
                    self.mdr = byte;
                    self.write_no_tick(a_addr, byte);
                }
                // A-bus offset steps within its bank; DASx byte counter
                // decrements to 0 (wraps $0000->$FFFF for the 65536 case).
                self.dma.advance_a1(ch, step);
                self.dma.set_das(ch, self.dma.das(ch).wrapping_sub(1));
                p += 1;
                remaining -= 1;
                self.dma_tick(GDMA_PER_BYTE);
            }
        }
        self.dma.mdmaen = 0;
    }

    /// Drive HDMA off the scheduler: initialize every enabled channel once at
    /// V=0, then run one per-line transfer for each scheduler line V that has
    /// completed (0..vblank_line-1). The transfer at the end of line V takes
    /// effect on line V+1 (timing.md §11).
    fn run_hdma(&mut self) {
        // Init once per frame (hardware: V=0 H=6). Trigger on the first
        // post-vblank re-entry into the visible region rather than sampling
        // exactly V==0, so a tick that crosses the whole pre-render line cannot
        // skip init. `hdma_inited` is cleared while V is in vblank.
        if self.scheduler.v >= self.scheduler.vblank_line {
            self.hdma_inited = false;
        } else if !self.hdma_inited {
            self.hdma_init();
            self.hdma_line = 0;
            self.hdma_inited = true;
        }
        let last_line = self.scheduler.vblank_line;
        while self.hdma_line < last_line && self.scheduler.v > self.hdma_line {
            self.hdma_transfer_line();
            self.hdma_line += 1;
        }
    }

    /// Per-frame HDMA init at V=0 (~H=6): for every channel enabled in $420C,
    /// point the table address A2Ax at the table start A1Tx and read the first
    /// line-count entry (and, in indirect mode, the data pointer) (timing.md §11).
    fn hdma_init(&mut self) {
        let mut cost = 0u64;
        let mut any = false;
        for ch in 0..8 {
            self.dma.set_hdma_channel_active(ch, false);
            self.dma.set_hdma_wants_transfer(ch, false);
            if self.dma.hdmaen & (1 << ch) == 0 {
                continue;
            }
            any = true;
            self.dma.set_hdma_channel_active(ch, true);
            self.dma.set_a2a(ch, self.dma.a1_offset(ch));
            cost += HDMA_CHANNEL_OVERHEAD;
            cost += self.hdma_reload(ch);
        }
        if any {
            self.dma_tick(cost + HDMA_INIT_OVERHEAD);
        }
    }

    /// One HDMA scanline pass over all active channels: transfer a unit if the
    /// channel's do-transfer flag is set, decrement the 8-bit NLTRx counter
    /// (bit7 is the next line's repeat flag), and reload the next table entry
    /// when the 7-bit counter reaches 0 (timing.md §11).
    fn hdma_transfer_line(&mut self) {
        if self.dma.hdma_active & self.dma.hdmaen == 0 {
            return;
        }
        let mut cost = HDMA_LINE_OVERHEAD;
        for ch in 0..8 {
            // HDMAEN is re-evaluated every line: a channel runs only if its
            // $420C bit is currently set AND it has not terminated (timing.md
            // §11; bsnes hdmaActive = enable && !completed).
            if self.dma.hdmaen & (1 << ch) == 0 || !self.dma.hdma_channel_active(ch) {
                continue;
            }
            cost += HDMA_CHANNEL_OVERHEAD;
            if self.dma.hdma_wants_transfer(ch) {
                cost += self.hdma_transfer_unit(ch) * HDMA_PER_BYTE;
            }
            let counter = self.dma.nltr_raw(ch).wrapping_sub(1);
            self.dma.set_nltr(ch, counter);
            self.dma.set_hdma_wants_transfer(ch, counter & 0x80 != 0);
            if counter & 0x7F == 0 {
                cost += self.hdma_reload(ch);
            }
        }
        self.dma_tick(cost);
    }

    /// Read the next line-count byte from the HDMA table into NLTRx (A2A++),
    /// and, in indirect mode, the 16-bit data pointer into DASx (A2A += 2). A
    /// $00 line-count byte terminates the channel for the rest of the frame.
    /// Returns the indirect-fetch cycle cost (0 in direct mode) (timing.md §11).
    ///
    /// Intentional divergence: timing.md §11 step 4 lists the indirect pointer
    /// load before the $00 terminate check, so a literal reading would advance
    /// A2A by 3 (and charge 16 cycles) even for a terminating entry. We
    /// terminate first and skip the pointer load. This is unobservable within a
    /// frame (the channel is now inactive and A2A is reloaded from A1T at the
    /// next frame's init) and avoids charging a cycle cost the hardware sources
    /// (nesdev/fullsnes) do not settle for a terminating indirect entry.
    fn hdma_reload(&mut self, ch: usize) -> u64 {
        let bank = self.dma.a1_bank(ch);
        let a2a = self.dma.a2a(ch);
        let byte = self.read_no_tick(((bank as u32) << 16) | a2a as u32);
        self.mdr = byte;
        self.dma.set_nltr(ch, byte);
        self.dma.set_a2a(ch, a2a.wrapping_add(1));
        if byte == 0 {
            self.dma.set_hdma_channel_active(ch, false);
            self.dma.set_hdma_wants_transfer(ch, false);
            return 0;
        }
        self.dma.set_hdma_wants_transfer(ch, true);
        if self.dma.hdma_indirect(ch) {
            let p = self.dma.a2a(ch);
            let lo = self.read_no_tick(((bank as u32) << 16) | p as u32);
            let hi = self.read_no_tick(((bank as u32) << 16) | p.wrapping_add(1) as u32);
            self.mdr = hi;
            self.dma.set_das(ch, u16::from_le_bytes([lo, hi]));
            self.dma.set_a2a(ch, p.wrapping_add(2));
            return HDMA_INDIRECT_RELOAD;
        }
        0
    }

    /// Transfer one HDMA unit (1/2/4 bytes per DMAPx mode) for channel `ch`.
    /// The A-bus side is the direct table pointer (A2Ax, in bank A1Bx) or,
    /// in indirect mode, the data pointer (DASx, in bank DASBx); either
    /// increments by one per byte. HDMA always uses incrementing steps
    /// (timing.md §11). Returns the number of bytes transferred.
    fn hdma_transfer_unit(&mut self, ch: usize) -> u64 {
        let a_to_b = self.dma.direction_a_to_b(ch);
        let bbad = self.dma.bbad(ch);
        let indirect = self.dma.hdma_indirect(ch);
        let pattern = self.dma.transfer_pattern(ch);
        let bank = if indirect { self.dma.dasb(ch) } else { self.dma.a1_bank(ch) };
        for &off in pattern {
            let b_addr = 0x2100 | bbad.wrapping_add(off) as u32;
            let a_off = if indirect { self.dma.das(ch) } else { self.dma.a2a(ch) };
            let a_addr = ((bank as u32) << 16) | a_off as u32;
            if a_to_b {
                let byte = self.read_no_tick(a_addr);
                self.mdr = byte;
                self.write_no_tick(b_addr, byte);
            } else {
                let byte = self.read_no_tick(b_addr);
                self.mdr = byte;
                self.write_no_tick(a_addr, byte);
            }
            let next = a_off.wrapping_add(1);
            if indirect {
                self.dma.set_das(ch, next);
            } else {
                self.dma.set_a2a(ch, next);
            }
        }
        pattern.len() as u64
    }

    /// Emit `--log-mmio` / `--watch` lines to stderr. No-op unless a debug tap
    /// is armed, so the normal access path is unaffected.
    fn debug_tap(&self, addr: u32, value: u8, is_write: bool) {
        if !self.watch_is_empty() {
            let full = addr & 0xFF_FFFF;
            if self.debug.watch.iter().any(|&w| w == full) {
                eprintln!(
                    "watch {} {:02X}:{:04X} = {:02X}",
                    if is_write { "WR" } else { "RD" },
                    (full >> 16) as u8,
                    (full & 0xFFFF) as u16,
                    value
                );
            }
        }
        if self.debug.log_mmio && is_write && Self::is_mapped_mmio(addr) {
            if let Some(name) = crate::debug::mmio_reg_name(addr) {
                eprintln!(
                    "mmio WR {:02X}:{:04X} {:<11} = {:02X}",
                    (addr >> 16) as u8,
                    (addr & 0xFFFF) as u16,
                    name,
                    value
                );
            }
        }
    }

    fn watch_is_empty(&self) -> bool {
        self.debug.watch.is_empty()
    }

    /// True only when `addr` decodes to a real hardware register: the $2100-$21FF
    /// / $4200-$5FFF MMIO windows in the system banks $00-$3F / $80-$BF. Excludes
    /// the $0000-$1FFF WRAM mirror in those banks and all of the $7E/$7F WRAM
    /// banks, whose low 16 bits alias register offsets and would otherwise be
    /// logged as fake $21xx/$42xx events by `--log-mmio`.
    fn is_mapped_mmio(addr: u32) -> bool {
        let bank = (addr >> 16) as u8;
        if !matches!(bank, 0x00..=0x3F | 0x80..=0xBF) {
            return false;
        }
        matches!((addr & 0xFFFF) as u16, 0x2100..=0x21FF | 0x4200..=0x5FFF)
    }
}

impl CpuBus for Bus {
    fn read(&mut self, addr: u32) -> u8 {
        self.scheduler.tick(Self::access_speed(addr, self.fastrom));
        self.post_tick();
        let v = self.read_no_tick(addr);
        self.mdr = v;
        self.debug_tap(addr, v, false);
        v
    }

    fn write(&mut self, addr: u32, value: u8) {
        self.scheduler.tick(Self::access_speed(addr, self.fastrom));
        self.post_tick();
        self.mdr = value;
        self.write_no_tick(addr, value);
        self.debug_tap(addr, value, true);
        // $420B MDMAEN write with any channel enabled: run GP-DMA now, CPU
        // stalled ($420B only decodes in banks $00-$3F / $80-$BF).
        let bank = (addr >> 16) as u8;
        if (addr & 0xFFFF) == 0x420B
            && matches!(bank, 0x00..=0x3F | 0x80..=0xBF)
            && self.dma.mdmaen != 0
        {
            self.run_gdma();
        }
    }

    fn idle(&mut self) {
        self.scheduler.tick(6);
        self.post_tick();
    }

    fn take_nmi(&mut self) -> bool {
        if self.scheduler.nmi_pending {
            self.scheduler.nmi_pending = false;
            true
        } else {
            false
        }
    }

    fn irq_level(&mut self) -> bool {
        // Level-held: stays true until $4211 is read or $4200 bits5-4 are
        // cleared (timing.md ยง6).
        self.scheduler.irq_pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::Cartridge;
    use crate::joypad::JoypadState;
    use crate::scheduler::{CYCLES_PER_LINE, NMI_LINE};

    fn test_bus() -> Bus {
        // Minimal LoROM image with a valid header.
        let mut rom = vec![0u8; 0x10000];
        rom[0x7FC0..0x7FC0 + 21].copy_from_slice(b"BUS TEST             ");
        rom[0x7FC0 + 0x15] = 0x20;
        rom[0x7FC0 + 0x19] = 2; // PAL
        rom[0x7FC0 + 0x3C] = 0x00;
        rom[0x7FC0 + 0x3D] = 0x80;
        rom[0] = 0x42; // visible at $00:8000
        Bus::new(Cartridge::from_bytes(rom).unwrap())
    }

    #[test]
    fn wram_mirrors() {
        let mut bus = test_bus();
        bus.write_no_tick(0x7E_0123, 0xAB);
        assert_eq!(bus.read_no_tick(0x00_0123), 0xAB);
        assert_eq!(bus.read_no_tick(0xBF_0123), 0xAB);
        bus.write_no_tick(0x30_1FFF, 0xCD);
        assert_eq!(bus.read_no_tick(0x7E_1FFF), 0xCD);
        // Second 64KB of WRAM only via bank $7F.
        bus.write_no_tick(0x7F_0123, 0x77);
        assert_eq!(bus.read_no_tick(0x7F_0123), 0x77);
        assert_eq!(bus.read_no_tick(0x7E_0123), 0xAB);
    }

    #[test]
    fn wram_port_2180() {
        let mut bus = test_bus();
        bus.write_no_tick(0x00_2181, 0x34);
        bus.write_no_tick(0x00_2182, 0x12);
        bus.write_no_tick(0x00_2183, 0x01);
        bus.write_no_tick(0x00_2180, 0x99); // writes $7F:1234, addr++
        assert_eq!(bus.read_no_tick(0x7F_1234), 0x99);
        // Address auto-incremented; reset it and read back through the port.
        bus.write_no_tick(0x00_2181, 0x34);
        bus.write_no_tick(0x00_2182, 0x12);
        bus.write_no_tick(0x00_2183, 0x01);
        assert_eq!(bus.read_no_tick(0x00_2180), 0x99);
    }

    #[test]
    fn open_bus_returns_mdr() {
        let mut bus = test_bus();
        let v = CpuBus::read(&mut bus, 0x00_8000); // ROM: $42, loads MDR
        assert_eq!(v, 0x42);
        // $2000-$20FF is unmapped: open bus repeats the last bus value.
        assert_eq!(CpuBus::read(&mut bus, 0x00_2000), 0x42);
        // Writes also drive MDR.
        CpuBus::write(&mut bus, 0x7E_0000, 0x99);
        assert_eq!(CpuBus::read(&mut bus, 0x00_2000), 0x99);
    }

    #[test]
    fn speed_table() {
        assert_eq!(Bus::access_speed(0x00_0000, false), 8); // WRAM mirror
        assert_eq!(Bus::access_speed(0x00_2100, false), 6); // MMIO
        assert_eq!(Bus::access_speed(0x00_4016, false), 12); // joypad region
        assert_eq!(Bus::access_speed(0x00_4200, false), 6); // internal regs
        assert_eq!(Bus::access_speed(0x00_8000, false), 8); // SlowROM
        assert_eq!(Bus::access_speed(0x80_8000, false), 8); // SlowROM, high bank
        assert_eq!(Bus::access_speed(0x80_8000, true), 6); // FastROM
        assert_eq!(Bus::access_speed(0x00_8000, true), 8); // FastROM never in $00-$7F
        assert_eq!(Bus::access_speed(0xC0_0000, true), 6);
        assert_eq!(Bus::access_speed(0x7E_0000, false), 8); // WRAM
    }

    #[test]
    fn memsel_enables_fastrom() {
        let mut bus = test_bus();
        assert!(!bus.fastrom);
        CpuBus::write(&mut bus, 0x00_420D, 1);
        assert!(bus.fastrom);
        assert_eq!(bus.read_no_tick(0x00_420D), 1);
    }

    #[test]
    fn clock_advances_per_access() {
        let mut bus = test_bus();
        let t0 = bus.scheduler.clock;
        CpuBus::read(&mut bus, 0x00_8000); // slow: 8
        CpuBus::read(&mut bus, 0x00_2100); // fast: 6
        CpuBus::read(&mut bus, 0x00_4016); // xslow: 12
        bus.idle(); // 6
        assert_eq!(bus.scheduler.clock - t0, 8 + 6 + 12 + 6);
    }

    #[test]
    fn nmi_enable_gating_and_take_nmi() {
        let mut bus = test_bus();
        // Advance to vblank start without enabling NMI ($4200 left at reset $00).
        while bus.scheduler.v != NMI_LINE {
            bus.scheduler.tick(CYCLES_PER_LINE);
        }
        assert!(!CpuBus::take_nmi(&mut bus));
        // Enabling NMI mid-vblank, while $4210.7 is already set, is a 0->1
        // edge of (enable AND flag): it must fire immediately.
        CpuBus::write(&mut bus, 0x00_4200, 0x80);
        assert!(CpuBus::take_nmi(&mut bus));
        assert!(!CpuBus::take_nmi(&mut bus)); // latch consumed
    }

    #[test]
    fn rdnmi_read_clear() {
        let mut bus = test_bus();
        while bus.scheduler.v != NMI_LINE {
            bus.scheduler.tick(CYCLES_PER_LINE);
        }
        let v = CpuBus::read(&mut bus, 0x00_4210);
        assert_eq!(v & 0x80, 0x80);
        assert_eq!(v & 0x0F, CPU_VERSION);
        let v2 = CpuBus::read(&mut bus, 0x00_4210);
        assert_eq!(v2 & 0x80, 0); // cleared by the previous read
    }

    #[test]
    fn hvbjoy_vblank_bit() {
        let mut bus = test_bus();
        assert_eq!(bus.read_no_tick(0x00_4212) & 0x80, 0);
        while bus.scheduler.v != NMI_LINE {
            bus.scheduler.tick(CYCLES_PER_LINE);
        }
        assert_eq!(bus.read_no_tick(0x00_4212) & 0x80, 0x80);
    }

    #[test]
    fn hvbjoy_hblank_bit() {
        let mut bus = test_bus();
        bus.scheduler.tick(4); // H=1: hblank cleared
        assert_eq!(bus.read_no_tick(0x00_4212) & 0x40, 0);
        bus.scheduler.tick(274 * 4 - 4); // H=274: hblank set
        assert_eq!(bus.read_no_tick(0x00_4212) & 0x40, 0x40);
    }

    #[test]
    fn multiply_and_divide() {
        let mut bus = test_bus();
        CpuBus::write(&mut bus, 0x00_4202, 12); // WRMPYA
        CpuBus::write(&mut bus, 0x00_4203, 10); // WRMPYB, starts multiply
        let product = bus.read_no_tick(0x00_4216) as u16
            | ((bus.read_no_tick(0x00_4217) as u16) << 8);
        assert_eq!(product, 120);
        // Quirk: writing $4203 also sets RDDIV = WRMPYB, high byte 0.
        assert_eq!(bus.read_no_tick(0x00_4214), 10);
        assert_eq!(bus.read_no_tick(0x00_4215), 0);

        CpuBus::write(&mut bus, 0x00_4204, 100); // dividend low
        CpuBus::write(&mut bus, 0x00_4205, 0); // dividend high (100 total)
        CpuBus::write(&mut bus, 0x00_4206, 7); // divisor, starts divide
        let quotient = bus.read_no_tick(0x00_4214) as u16
            | ((bus.read_no_tick(0x00_4215) as u16) << 8);
        let remainder = bus.read_no_tick(0x00_4216) as u16
            | ((bus.read_no_tick(0x00_4217) as u16) << 8);
        assert_eq!(quotient, 14);
        assert_eq!(remainder, 2);

        // Divide by zero: quotient = $FFFF, remainder = dividend.
        CpuBus::write(&mut bus, 0x00_4206, 0);
        assert_eq!(bus.read_no_tick(0x00_4214), 0xFF);
        assert_eq!(bus.read_no_tick(0x00_4215), 0xFF);
        assert_eq!(bus.read_no_tick(0x00_4216), 100);
        assert_eq!(bus.read_no_tick(0x00_4217), 0);
    }

    #[test]
    fn auto_joypad_latch_on_vblank_when_enabled() {
        let mut bus = test_bus();
        bus.set_inputs([
            JoypadState { a: true, ..Default::default() },
            JoypadState::default(),
        ]);
        CpuBus::write(&mut bus, 0x00_4200, 0x01); // enable auto-joypad
        while bus.scheduler.v != NMI_LINE {
            bus.scheduler.tick(CYCLES_PER_LINE);
            bus.post_tick();
        }
        // A pressed -> serial bit7 of the low byte (to_bits() layout).
        assert_eq!(bus.read_no_tick(0x00_4218), 0x80);
        assert_eq!(bus.read_no_tick(0x00_4219), 0x00);
    }

    #[test]
    fn auto_joypad_not_latched_when_disabled() {
        let mut bus = test_bus();
        bus.set_inputs([
            JoypadState { a: true, ..Default::default() },
            JoypadState::default(),
        ]);
        // $4200 left at reset value $00: auto-joypad disabled.
        while bus.scheduler.v != NMI_LINE {
            bus.scheduler.tick(CYCLES_PER_LINE);
            bus.post_tick();
        }
        assert_eq!(bus.read_no_tick(0x00_4218), 0);
    }

    #[test]
    fn per_line_render_advances_once_per_visible_line() {
        let mut bus = test_bus();
        assert_eq!(bus.render_line, 1); // pre-render line V=0, nothing rendered
        while bus.scheduler.v < NMI_LINE {
            bus.scheduler.tick(CYCLES_PER_LINE);
            bus.post_tick();
            // After the scheduler passes visible line k (v == k+1), lines
            // 1..k are rendered, so render_line tracks v (capped at 225).
            if bus.scheduler.v >= 2 {
                assert_eq!(bus.render_line, bus.scheduler.v.min(225));
            }
        }
        // Exactly the 224 visible lines V=1..=224 rendered, once each.
        assert_eq!(bus.render_line, 225);
    }

    #[test]
    fn per_frame_render_line_resets_at_prerender_line() {
        let mut bus = test_bus();
        // Run a full frame so v wraps back to 0.
        while !bus.scheduler.frame_done {
            bus.scheduler.tick(CYCLES_PER_LINE);
            bus.post_tick();
        }
        assert_eq!(bus.scheduler.v, 0);
        assert_eq!(bus.render_line, 1); // reset for the next frame
    }

    #[test]
    fn gdma_moves_bytes_to_vram() {
        let mut bus = test_bus();
        // Source bytes in the first-8KB WRAM mirror of bank $00.
        for (i, b) in [0x11u8, 0x22, 0x33, 0x44].iter().enumerate() {
            bus.write_no_tick(i as u32, *b);
        }
        bus.write_no_tick(0x00_2115, 0x80); // VMAIN: +1 word after $2119
        bus.write_no_tick(0x00_2116, 0x00); // VMADDL
        bus.write_no_tick(0x00_2117, 0x00); // VMADDH
        bus.write_no_tick(0x00_4300, 0x01); // ch0 DMAP: A->B, unit 1, +1
        bus.write_no_tick(0x00_4301, 0x18); // BBAD -> $2118
        bus.write_no_tick(0x00_4302, 0x00); // A1TL
        bus.write_no_tick(0x00_4303, 0x00); // A1TH
        bus.write_no_tick(0x00_4304, 0x00); // A1B (bank $00)
        bus.write_no_tick(0x00_4305, 0x04); // DASL = 4 bytes
        bus.write_no_tick(0x00_4306, 0x00); // DASH
        let t0 = bus.scheduler.clock;
        CpuBus::write(&mut bus, 0x00_420B, 0x01); // MDMAEN ch0 -> run now
        assert_eq!(bus.dma.mdmaen, 0); // $420B self-clears
        // Unit 1 pattern [0,1]: low then high byte per VRAM word.
        assert_eq!(bus.ppu.vram[0], 0x2211);
        assert_eq!(bus.ppu.vram[1], 0x4433);
        assert_eq!(bus.dma.a1_offset(0), 0x0004); // A1T stepped +1/byte
        assert_eq!(bus.dma.das(0), 0); // byte counter decremented to 0
        // $420B access (6) + whole(8) + channel(8) + 4 bytes * 8 (32).
        assert_eq!(bus.scheduler.clock - t0, 6 + 8 + 8 + 4 * 8);
    }

    #[test]
    fn hdma_direct_per_line_transfers_and_terminates() {
        let mut bus = test_bus();
        // Direct HDMA table in the bank-$00 WRAM mirror at $0100:
        // 0x82 = repeat, count 2 (transfer a unit on 2 consecutive lines);
        // two 2-byte units; 0x00 terminator.
        let table = [0x82u8, 0x11, 0x22, 0x33, 0x44, 0x00];
        for (i, b) in table.iter().enumerate() {
            bus.write_no_tick(0x0100 + i as u32, *b);
        }
        bus.write_no_tick(0x00_2115, 0x80); // VMAIN: +1 word after $2119
        bus.write_no_tick(0x00_2116, 0x00); // VMADDL
        bus.write_no_tick(0x00_2117, 0x00); // VMADDH
        bus.write_no_tick(0x00_4300, 0x01); // ch0 DMAP: A->B, unit 1, direct
        bus.write_no_tick(0x00_4301, 0x18); // BBAD -> $2118
        bus.write_no_tick(0x00_4302, 0x00); // A1TL (table start $0100)
        bus.write_no_tick(0x00_4303, 0x01); // A1TH
        bus.write_no_tick(0x00_4304, 0x00); // A1B (bank $00)
        bus.write_no_tick(0x00_420C, 0x01); // HDMAEN ch0

        bus.post_tick(); // V=0: HDMA init
        assert!(bus.dma.hdma_channel_active(0));
        for _ in 0..3 {
            bus.scheduler.tick(CYCLES_PER_LINE);
            bus.post_tick();
        }
        // Two units transferred to VRAM words 0 and 1, then $00 terminated it.
        assert_eq!(bus.ppu.vram[0], 0x2211);
        assert_eq!(bus.ppu.vram[1], 0x4433);
        assert!(!bus.dma.hdma_channel_active(0));
    }

    #[test]
    fn hdma_non_repeat_transfers_once_then_holds() {
        let mut bus = test_bus();
        // Line-count $03 = repeat flag clear: transfer ONE unit on the first
        // line, then pause 2 lines (timing.md §11 "$01-$80: transfer 1 unit
        // now, then pause N-1 lines"). $00 terminates.
        let table = [0x03u8, 0x11, 0x22, 0x00];
        for (i, b) in table.iter().enumerate() {
            bus.write_no_tick(0x0100 + i as u32, *b);
        }
        bus.write_no_tick(0x00_2115, 0x80); // VMAIN: +1 word after $2119
        bus.write_no_tick(0x00_2116, 0x00); // VMADDL
        bus.write_no_tick(0x00_2117, 0x00); // VMADDH
        bus.write_no_tick(0x00_4300, 0x01); // ch0 DMAP: A->B, unit 1, direct
        bus.write_no_tick(0x00_4301, 0x18); // BBAD -> $2118
        bus.write_no_tick(0x00_4302, 0x00); // A1TL (table start $0100)
        bus.write_no_tick(0x00_4303, 0x01); // A1TH
        bus.write_no_tick(0x00_4304, 0x00); // A1B (bank $00)
        bus.write_no_tick(0x00_420C, 0x01); // HDMAEN ch0

        bus.post_tick(); // V=0: HDMA init
        for _ in 0..3 {
            bus.scheduler.tick(CYCLES_PER_LINE);
            bus.post_tick();
        }
        // Only the first line transferred a unit; VMADD advanced one word, so
        // word 1 was never written (a repeat table would have filled it).
        assert_eq!(bus.ppu.vram[0], 0x2211);
        assert_eq!(bus.ppu.vram[1], 0x0000);
        assert!(!bus.dma.hdma_channel_active(0)); // $00 terminated it
    }

    #[test]
    fn hdma_indirect_per_line_transfers_and_terminates() {
        let mut bus = test_bus();
        // Indirect table in the bank-$00 WRAM mirror at $0100: repeat count 2
        // then a 16-bit data pointer ($0200), $00 terminator. The two 2-byte
        // units live at the indirect data address, streamed one per line.
        let table = [0x82u8, 0x00, 0x02, 0x00]; // 0x82; ptr=$0200; terminator
        for (i, b) in table.iter().enumerate() {
            bus.write_no_tick(0x0100 + i as u32, *b);
        }
        let data = [0x11u8, 0x22, 0x33, 0x44];
        for (i, b) in data.iter().enumerate() {
            bus.write_no_tick(0x0200 + i as u32, *b);
        }
        bus.write_no_tick(0x00_2115, 0x80); // VMAIN: +1 word after $2119
        bus.write_no_tick(0x00_2116, 0x00); // VMADDL
        bus.write_no_tick(0x00_2117, 0x00); // VMADDH
        bus.write_no_tick(0x00_4300, 0x41); // ch0 DMAP: A->B, indirect, unit 1
        bus.write_no_tick(0x00_4301, 0x18); // BBAD -> $2118
        bus.write_no_tick(0x00_4302, 0x00); // A1TL (table start $0100)
        bus.write_no_tick(0x00_4303, 0x01); // A1TH
        bus.write_no_tick(0x00_4304, 0x00); // A1B (table bank $00)
        bus.write_no_tick(0x00_4307, 0x00); // DASB (indirect data bank $00)
        bus.write_no_tick(0x00_420C, 0x01); // HDMAEN ch0

        bus.post_tick(); // V=0: HDMA init reads count + indirect pointer
        assert!(bus.dma.hdma_channel_active(0));
        assert_eq!(bus.dma.das(0), 0x0200); // indirect pointer loaded
        for _ in 0..3 {
            bus.scheduler.tick(CYCLES_PER_LINE);
            bus.post_tick();
        }
        // Two units streamed from the indirect data at $0200, then $00 in the
        // table terminated the channel.
        assert_eq!(bus.ppu.vram[0], 0x2211);
        assert_eq!(bus.ppu.vram[1], 0x4433);
        assert!(!bus.dma.hdma_channel_active(0));
    }

    #[test]
    fn hdma_reinits_each_frame() {
        let mut bus = test_bus();
        bus.write_no_tick(0x00_2115, 0x80); // VMAIN: +1 word after $2119
        bus.write_no_tick(0x00_2116, 0x00); // VMADDL
        bus.write_no_tick(0x00_2117, 0x00); // VMADDH
        bus.write_no_tick(0x00_4300, 0x01); // ch0 DMAP: A->B, direct, unit 1 (word)
        bus.write_no_tick(0x00_4301, 0x18); // BBAD -> $2118/$2119 (VRAM data)
        bus.write_no_tick(0x00_4302, 0x00);
        bus.write_no_tick(0x00_4303, 0x01); // A1T = $0100
        bus.write_no_tick(0x00_4304, 0x00);
        // Direct table with two "transfer-now, pause 127 lines" entries
        // ($80 = 128-line span each): the channel transfers one word at line 0,
        // a second word at line 128, and never reaches the $00 terminator
        // within the ~224 visible lines, so it stays active the whole frame.
        let table = [0x80u8, 0x11, 0x22, 0x80, 0x33, 0x44, 0x00];
        for (i, b) in table.iter().enumerate() {
            bus.write_no_tick(0x0100 + i as u32, *b);
        }
        bus.write_no_tick(0x00_420C, 0x01);
        bus.post_tick(); // init frame 1
        assert_eq!(bus.dma.a2a(0), 0x0101); // one line-count byte read
        assert!(bus.dma.hdma_channel_active(0));
        // Run past line 128's second transfer.
        while bus.scheduler.v < 130 {
            bus.scheduler.tick(CYCLES_PER_LINE);
            bus.post_tick();
        }
        // Exactly two units transferred (lines 0 and 128); the channel is still
        // active mid-frame, proving it was not terminated after the first entry.
        assert_eq!(bus.ppu.vram[0], 0x2211);
        assert_eq!(bus.ppu.vram[1], 0x4433);
        assert_eq!(bus.ppu.vram[2], 0x0000);
        assert_eq!(bus.dma.a2a(0), 0x0106);
        assert!(bus.dma.hdma_channel_active(0));
        // Run the rest of the frame; at the next V=0 HDMA must re-init A2A to A1T.
        while !bus.scheduler.frame_done {
            bus.scheduler.tick(CYCLES_PER_LINE);
            bus.post_tick();
        }
        bus.post_tick(); // V=0 of next frame: re-init
        assert_eq!(bus.dma.a2a(0), 0x0101); // table pointer reset then reloaded
        assert!(bus.dma.hdma_channel_active(0));
    }

    #[test]
    fn hdma_clear_420c_midframe_stops_channel() {
        let mut bus = test_bus();
        bus.write_no_tick(0x00_2115, 0x80);
        bus.write_no_tick(0x00_2116, 0x00);
        bus.write_no_tick(0x00_2117, 0x00);
        bus.write_no_tick(0x00_4300, 0x01); // A->B, direct, unit 1
        bus.write_no_tick(0x00_4301, 0x18); // BBAD -> $2118
        bus.write_no_tick(0x00_4302, 0x00);
        bus.write_no_tick(0x00_4303, 0x01); // A1T = $0100
        bus.write_no_tick(0x00_4304, 0x00);
        let table = [0x80u8, 0x11, 0x22, 0x80, 0x33, 0x44, 0x00];
        for (i, b) in table.iter().enumerate() {
            bus.write_no_tick(0x0100 + i as u32, *b);
        }
        bus.write_no_tick(0x00_420C, 0x01);
        bus.post_tick(); // init: line-0 unit written to vram[0]
        while bus.scheduler.v < 2 {
            bus.scheduler.tick(CYCLES_PER_LINE);
            bus.post_tick();
        }
        assert_eq!(bus.ppu.vram[0], 0x2211);
        // Clearing $420C mid-frame must stop the channel immediately: the
        // second entry's word at line 128 must NOT be written this frame.
        bus.write_no_tick(0x00_420C, 0x00);
        while bus.scheduler.v < 130 {
            bus.scheduler.tick(CYCLES_PER_LINE);
            bus.post_tick();
        }
        assert_eq!(bus.ppu.vram[1], 0x0000);
    }

    #[test]
    fn rdio_reflects_wrio_all_bits() {
        let mut bus = test_bus();
        bus.mdr = 0x00;
        CpuBus::write(&mut bus, 0x00_4201, 0xA5);
        // $4213 reads back all of WRIO (bits7-6 IOBit inputs, bits5-0 "as set
        // by $4201"), not CPU open bus (mmio.md §7-8).
        assert_eq!(bus.read_no_tick(0x00_4213), 0xA5);
    }

    #[test]
    fn overscan_bit_moves_vblank_line() {
        let mut bus = test_bus();
        assert_eq!(bus.scheduler.vblank_line, NMI_LINE);
        CpuBus::write(&mut bus, 0x00_2133, 0x04); // SETINI bit2 = overscan
        assert_eq!(bus.scheduler.vblank_line, 240);
        CpuBus::write(&mut bus, 0x00_2133, 0x00);
        assert_eq!(bus.scheduler.vblank_line, NMI_LINE);
    }

    #[test]
    fn wrio_falling_edge_latches_hv_counters() {
        let mut bus = test_bus();
        bus.scheduler.tick(100 * 4); // ~dot 100 of line 0
        CpuBus::write(&mut bus, 0x00_4201, 0xFF); // bit7 high
        assert!(!bus.ppu.counter_latched);
        CpuBus::write(&mut bus, 0x00_4201, 0x7F); // bit7 1->0: latch
        assert!(bus.ppu.counter_latched);
    }

    #[test]
    fn counter_latch_read_sequence() {
        let mut bus = test_bus();
        // WRIO bit7 is set at reset ($FF): the SLHV latch gate is open.
        bus.scheduler.tick(300 * 4); // dot 300 of line 0 (V=0)
        bus.read_no_tick(0x00_2137); // SLHV: latch H=300, V=0
        assert!(bus.ppu.counter_latched); // $213F bit6
        // OPHCT $213C flip-flop: 1st read = low byte, 2nd read = high bit (+
        // PPU2 open bus in the upper 7 bits, so mask bit0).
        assert_eq!(bus.read_no_tick(0x00_213C), (300 & 0xFF) as u8);
        assert_eq!(bus.read_no_tick(0x00_213C) & 0x01, ((300 >> 8) & 1) as u8);
        // OPVCT $213D: low then high; V=0.
        assert_eq!(bus.read_no_tick(0x00_213D), 0);
        assert_eq!(bus.read_no_tick(0x00_213D) & 0x01, 0);
        // Reading $213F resets both read flip-flops and the latch flag.
        bus.read_no_tick(0x00_213F);
        assert!(!bus.ppu.counter_latched);
        // Flip-flop reset: the next $213C read is the low byte again.
        assert_eq!(bus.read_no_tick(0x00_213C), (300 & 0xFF) as u8);
    }

    #[test]
    fn slhv_latch_gated_by_wrio_bit7() {
        let mut bus = test_bus();
        // Clearing WRIO bit7 is itself a 1->0 edge that latches once.
        bus.scheduler.tick(50 * 4);
        CpuBus::write(&mut bus, 0x00_4201, 0x00);
        assert!(bus.ppu.counter_latched);
        bus.read_no_tick(0x00_213F); // reset latch flag + flip-flops
        assert!(!bus.ppu.counter_latched);
        // With WRIO bit7 clear the gate is closed: reading $2137 must NOT latch.
        bus.scheduler.tick(100 * 4);
        bus.read_no_tick(0x00_2137);
        assert!(!bus.ppu.counter_latched);
    }

    #[test]
    fn timeup_read_clears_and_deasserts_irq() {
        let mut bus = test_bus();
        CpuBus::write(&mut bus, 0x00_4209, 3); // VTIME low = 3
        CpuBus::write(&mut bus, 0x00_420A, 0); // VTIME high
        CpuBus::write(&mut bus, 0x00_4200, 0x20); // mode 2 = V-IRQ once/frame
        while bus.scheduler.v < 3 {
            bus.scheduler.tick(CYCLES_PER_LINE);
            bus.post_tick();
        }
        bus.scheduler.tick(20); // past the V=VTIME H=~2.5 trigger
        bus.post_tick();
        assert!(CpuBus::irq_level(&mut bus));
        let timeup = CpuBus::read(&mut bus, 0x00_4211);
        assert_eq!(timeup & 0x80, 0x80); // TIMEUP bit7 set
        assert!(!CpuBus::irq_level(&mut bus)); // read-ack de-asserted the line
        // Second read: flag already cleared.
        assert_eq!(CpuBus::read(&mut bus, 0x00_4211) & 0x80, 0);
    }

    #[test]
    fn joypad_read_open_bus_and_driven_bits() {
        let mut bus = test_bus();
        bus.set_inputs([
            JoypadState { b: true, ..Default::default() },
            JoypadState::default(),
        ]);
        bus.write_no_tick(0x00_4016, 1); // OUT0 high: latch
        bus.write_no_tick(0x00_4016, 0); // OUT0 low: begin shift
        bus.mdr = 0xA5;
        let a = bus.read_no_tick(0x00_4016);
        assert_eq!(a & 0x01, 0x01); // port1 data1 = B pressed
        assert_eq!(a & 0xFC, 0xA5 & 0xFC); // bits7-2 open bus
        bus.mdr = 0x00;
        let b = bus.read_no_tick(0x00_4017);
        assert_eq!(b & 0x01, 0x00); // port2 idle
        assert_eq!(b & 0x1C, 0x1C); // bits4-2 always driven to 1
        assert_eq!(b & 0xE0, 0x00); // bits7-5 open bus (mdr=0)
    }

    #[test]
    fn auto_joypad_busy_flag_window() {
        let mut bus = test_bus();
        bus.set_inputs([
            JoypadState { a: true, ..Default::default() },
            JoypadState::default(),
        ]);
        CpuBus::write(&mut bus, 0x00_4200, 0x01); // enable, strobe low
        while bus.scheduler.v != NMI_LINE {
            bus.scheduler.tick(CYCLES_PER_LINE);
            bus.post_tick();
        }
        assert_eq!(bus.read_no_tick(0x00_4212) & 0x01, 0x01); // busy set
        assert_eq!(bus.read_no_tick(0x00_4218), 0x80); // A -> low-byte bit7
        // The read starts at H≈74.5 (AUTO_JOYPAD_START_OFFSET) of the vblank
        // line, so the busy window ends AUTO_JOYPAD_CYCLES after that, later
        // than the H=0 pulse. Advance to just before `busy_until`.
        let remaining = bus.auto_joypad_busy_until - bus.scheduler.clock;
        bus.scheduler.tick(remaining - 1);
        assert_eq!(bus.read_no_tick(0x00_4212) & 0x01, 0x01); // still busy
        bus.scheduler.tick(1); // reach busy_until exactly
        assert_eq!(bus.read_no_tick(0x00_4212) & 0x01, 0x00); // busy cleared
    }

    #[test]
    fn auto_joypad_suppressed_when_strobe_high() {
        let mut bus = test_bus();
        bus.set_inputs([
            JoypadState { a: true, ..Default::default() },
            JoypadState::default(),
        ]);
        CpuBus::write(&mut bus, 0x00_4200, 0x01); // enable
        CpuBus::write(&mut bus, 0x00_4016, 0x01); // OUT0 held high
        while bus.scheduler.v != NMI_LINE {
            bus.scheduler.tick(CYCLES_PER_LINE);
            bus.post_tick();
        }
        assert_eq!(bus.read_no_tick(0x00_4218), 0x00); // not latched
        assert_eq!(bus.read_no_tick(0x00_4212) & 0x01, 0x00); // busy not set
    }

    #[test]
    fn h_irq_via_cpubus() {
        let mut bus = test_bus();
        CpuBus::write(&mut bus, 0x00_4207, 10); // HTIME low
        CpuBus::write(&mut bus, 0x00_4208, 0); // HTIME high
        CpuBus::write(&mut bus, 0x00_4200, 0x10); // mode=1 (H-IRQ every line)
        assert!(!CpuBus::irq_level(&mut bus));
        bus.scheduler.tick(CYCLES_PER_LINE); // past the trigger point of line 0
        bus.post_tick();
        assert!(CpuBus::irq_level(&mut bus));
        let timeup = CpuBus::read(&mut bus, 0x00_4211);
        assert_eq!(timeup & 0x80, 0x80);
        assert!(!CpuBus::irq_level(&mut bus)); // read-ack cleared it
    }
}
