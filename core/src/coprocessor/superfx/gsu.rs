//! SuperFX / GSU coprocessor core (custom 16-bit RISC, NOT a 65C816).
//!
//! Register file R0-R15 (R15 = PC), status/flag register SFR, the SNES-visible
//! control registers, a 512-byte / 32-line code cache, and the pixel-plot state.
//! The GSU owns Game Pak RAM (`ram`) and borrows the Game Pak ROM image for the
//! duration of a `run`/`step`.
//!
//! Pipeline model: R15 always addresses the *next* opcode. A one-byte prefetch
//! register (`pipe`) holds the byte at R15-1; `fetch()` returns it and prefetches
//! the following byte, so the byte after any branch/jump/R15-write is fetched and
//! executed before the target (real GSU pipeline behavior).
//!
//! Prefix state machine (ALT1/ALT2/ALT3, TO/WITH/FROM) is reset after every
//! normal opcode; branches ($05-$0F) and the prefix opcodes themselves preserve
//! it. WITH sets the B flag so a following TO/FROM byte executes as MOVE/MOVES.

/// GSU2 version code returned by VCR ($303B).
pub const VCR_GSU2: u8 = 0x04;

/// Ring-buffer length for `recent_pc` tight-loop diagnosis.
const RECENT_PC_LEN: usize = 16;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct SuperFx {
    /// General-purpose registers. R15 is the program counter.
    pub(crate) r: [u16; 16],

    // SFR flags (bits 1-4), run/irq/rom-read status, prefix state.
    pub(crate) z: bool,
    pub(crate) cy: bool,
    pub(crate) s: bool,
    pub(crate) ov: bool,
    /// SFR bit5 GO: GSU running.
    pub(crate) go: bool,
    /// SFR bit15 IRQ: set on STOP, cleared when SNES reads SFR high byte.
    pub(crate) irq: bool,
    /// SFR bit6 R: a GETxx ROM read-ahead is in progress (set by `arm_rom_buffer`
    /// when R14 is written, cleared when the buffered read completes).
    pub(crate) rom_read: bool,
    pub(crate) alt1: bool,
    pub(crate) alt2: bool,
    /// SFR bit12 B (WITH prefix active).
    pub(crate) b: bool,

    /// Source register index selected by FROM/WITH (default R0).
    pub(crate) sreg: usize,
    /// Destination register index selected by TO/WITH (default R0).
    pub(crate) dreg: usize,

    // SNES-visible control registers.
    pub(crate) pbr: u8,
    pub(crate) rombr: u8,
    pub(crate) rambr: u8,
    pub(crate) scbr: u8,
    pub(crate) scmr: u8,
    pub(crate) cfgr: u8,
    pub(crate) clsr: u8,
    /// Backup-RAM enable ($3033); no effect on shipped PCBs.
    pub(crate) bramr: u8,
    pub(crate) cbr: u16,
    pub(crate) version: u8,

    /// Current plot color (COLR, not SNES-addressable).
    pub(crate) colr: u8,
    /// Plot option register (POR), set by CMODE (bits 0-4).
    pub(crate) por: u8,

    /// 16-bit register write latch (low byte held until high byte commits).
    pub(crate) latch: u8,

    /// Prefetched opcode byte (holds mem[R15-1]).
    pub(crate) pipe: u8,
    /// Address the current `pipe` byte was fetched from (debug/trace only).
    #[serde(skip)]
    pub(crate) pipe_pc: u16,
    /// Pipeline has been primed for the current run.
    pub(crate) primed: bool,

    /// 512-byte code cache (32 lines x 16 bytes).
    #[serde(with = "crate::serde_util::boxed_bytes")]
    pub(crate) cache: Box<[u8; 512]>,
    pub(crate) cache_valid: [bool; 32],

    /// Game Pak RAM (owned by the GSU).
    pub(crate) ram: Vec<u8>,

    /// Last RAM word address touched by a load/store (target of SBK).
    pub(crate) last_ram_addr: u16,

    // ---- ROM/RAM read-ahead & write-queue buffers (superfx.md §7; bsnes
    // timing.cpp romcl/romdr, ramcl/ramar/ramdr). GETB/GETC never read the
    // bus live: they consume `romdr`, which is (re)loaded `buffer_latency()`
    // GSU clocks after any write to R14, using ROMBR as it stands at the
    // moment the countdown reaches zero (natural decay via `tick_buffers`,
    // or forced immediately by `sync_rom_buffer` before GETB/GETC/ROMB). This
    // is what makes a ROMB issued between an R14 write and the matching GETB
    // able to steer which bank is captured, while a ROMB issued *after* the
    // read has already latched cannot retroactively change it.
    /// ROM buffer data register: the byte GETB/GETC return.
    pub(crate) romdr: u8,
    /// GSU clocks remaining until `romdr` is (re)loaded from [ROMBR:R14]; 0 =
    /// no read-ahead in flight (romdr holds the last completed read).
    pub(crate) romcl: u32,
    /// RAM buffer data register: the byte queued by STW/STB/SM/SMS/SBK,
    /// written to `ramar` when `ramcl` reaches 0.
    pub(crate) ramdr: u8,
    /// RAM address the queued `ramdr` byte targets.
    pub(crate) ramar: u16,
    /// GSU clocks remaining until the queued RAM write drains; 0 = idle.
    pub(crate) ramcl: u32,
    /// Set for one instruction when this instruction wrote R14 (any write
    /// counts, even to the same value, matching bsnes `Register::modified`);
    /// consumed at the end of `execute_one` to re-arm `romcl` so the freshly
    /// armed countdown is not decremented by the same instruction's own
    /// clocks (bsnes re-arms strictly after `instruction()` returns).
    #[serde(skip)]
    pub(crate) r14_written: bool,

    // Pixel cache: one 8-pixel row buffered before flush to RAM.
    pub(crate) pcache_x: u16,
    pub(crate) pcache_y: u16,
    pub(crate) pcache_bits: [u8; 8],
    pub(crate) pcache_flags: u8,

    // ---- Debug-only instrumentation (`--trace-gsu`); not emulated hardware
    // state, excluded from save states, and never observed by game code. ----
    /// `--trace-gsu` sink: fires once per executed instruction (including
    /// prefix-only bytes ALT1/ALT2/ALT3/TO/WITH/FROM, each its own pipeline
    /// fetch-execute step), immediately before it executes.
    #[serde(skip)]
    pub(crate) gsu_trace: Option<Box<dyn FnMut(&str)>>,
    /// Total GSU instructions executed (including prefix-only bytes), for
    /// hang diagnosis (e.g. "did the GSU ever run at all").
    #[serde(skip)]
    pub(crate) instructions_executed: u64,
    /// Ring buffer of the last `RECENT_PC_LEN` instruction addresses (R15-1 at
    /// fetch time). A tight loop shows up as a handful of distinct values
    /// repeating here even without a trace sink installed.
    #[serde(skip)]
    pub(crate) recent_pc: [u16; RECENT_PC_LEN],
    #[serde(skip)]
    pub(crate) recent_pc_idx: usize,
}

impl SuperFx {
    pub fn new(ram_size: usize, version: u8) -> Self {
        SuperFx {
            r: [0; 16],
            z: false,
            cy: false,
            s: false,
            ov: false,
            go: false,
            irq: false,
            rom_read: false,
            alt1: false,
            alt2: false,
            b: false,
            sreg: 0,
            dreg: 0,
            pbr: 0,
            rombr: 0,
            rambr: 0,
            scbr: 0,
            scmr: 0,
            cfgr: 0,
            clsr: 0,
            bramr: 0,
            cbr: 0,
            version,
            colr: 0,
            por: 0,
            latch: 0,
            pipe: 0,
            pipe_pc: 0,
            primed: false,
            cache: Box::new([0; 512]),
            cache_valid: [false; 32],
            ram: vec![0; ram_size.max(1)],
            last_ram_addr: 0,
            romdr: 0,
            romcl: 0,
            ramdr: 0,
            ramar: 0,
            ramcl: 0,
            r14_written: false,
            pcache_x: 0,
            pcache_y: 0,
            pcache_bits: [0; 8],
            pcache_flags: 0,
            gsu_trace: None,
            instructions_executed: 0,
            recent_pc: [0; RECENT_PC_LEN],
            recent_pc_idx: 0,
        }
    }

    // ---- Debug/trace API (--trace-gsu) ---------------------------------------

    /// Install the `--trace-gsu` sink (see `gsu_trace` field doc).
    pub fn set_gsu_trace(&mut self, sink: Box<dyn FnMut(&str)>) {
        self.gsu_trace = Some(sink);
    }

    /// Remove the trace sink and return it (drop it to flush any buffered writer).
    pub fn clear_gsu_trace(&mut self) -> Option<Box<dyn FnMut(&str)>> {
        self.gsu_trace.take()
    }

    /// Total GSU instructions executed since construction (debug counter).
    pub fn instructions_executed(&self) -> u64 {
        self.instructions_executed
    }

    /// Last up to `RECENT_PC_LEN` instruction addresses, oldest first; a tight
    /// loop shows as a short repeating run here (hang-diagnosis aid).
    pub fn recent_pc(&self) -> Vec<u16> {
        (0..RECENT_PC_LEN)
            .map(|i| self.recent_pc[(self.recent_pc_idx + i) % RECENT_PC_LEN])
            .collect()
    }

    // ---- Public API for the Bus / cartridge integration ----------------------

    /// SFR bit5 GO: the GSU is running and (per SCMR) may own Game Pak ROM/RAM.
    pub fn is_running(&self) -> bool {
        self.go
    }

    /// GSU->SNES IRQ line: SFR.IRQ set (by STOP) and not masked by CFGR bit7.
    pub fn irq_line(&self) -> bool {
        self.irq && (self.cfgr & 0x80) == 0
    }

    /// SCMR bit4 RON: 1 = GSU owns Game Pak ROM.
    pub fn rom_granted(&self) -> bool {
        self.scmr & 0x10 != 0
    }

    /// SCMR bit3 RAN: 1 = GSU owns Game Pak RAM.
    pub fn ram_granted(&self) -> bool {
        self.scmr & 0x08 != 0
    }

    /// True while the SNES CPU must be locked out of Game Pak ROM (open bus).
    pub fn snes_rom_blocked(&self) -> bool {
        self.go && self.rom_granted()
    }

    /// True while the SNES CPU must be locked out of Game Pak RAM (open bus).
    pub fn snes_ram_blocked(&self) -> bool {
        self.go && self.ram_granted()
    }

    pub fn ram(&self) -> &[u8] {
        &self.ram
    }

    pub fn ram_mut(&mut self) -> &mut [u8] {
        &mut self.ram
    }

    pub fn ram_size(&self) -> usize {
        self.ram.len()
    }

    /// GSU base-clock divider from the master clock: 2 in 10.7 MHz mode
    /// (master/2), 1 in 21.4 MHz mode (CLSR bit0 CLS = 1).
    pub fn clock_divider(&self) -> u32 {
        if self.clsr & 0x01 != 0 {
            1
        } else {
            2
        }
    }

    /// While GO=1 & RON=1 the SNES sees fixed exception vectors instead of ROM.
    /// Only the native CPU vector locations $FFE4-$FFEF expose fixed bytes
    /// (superfx.md §4); every other locked-out ROM address is open bus, so this
    /// gates on `addr16 & FFF0 == FFE0` before decoding the vector. Returns the
    /// byte for `addr16`, or `None` (open bus) if it is not a vector byte.
    pub fn rom_vector_override(&self, addr16: u16) -> Option<u8> {
        if addr16 & 0xFFF0 != 0xFFE0 {
            return None;
        }
        let val: u16 = match addr16 & 0x000E {
            0x4 => 0x0104, // COP  ($FFE4/E5)
            0x6 => 0x0100, // BRK  ($FFE6/E7)
            0x8 => 0x0100, // ABT  ($FFE8/E9)
            0xA => 0x0108, // NMI  ($FFEA/EB)
            0xE => 0x010C, // IRQ  ($FFEE/EF, H/V-IRQ & GSU STOP)
            _ => return None, // $FFE0-E3 / $FFEC-ED reserved: open bus
        };
        // Vectors are word-aligned at even addresses: LSB at even, MSB at odd.
        let byte = if addr16 & 1 == 0 {
            (val & 0xFF) as u8
        } else {
            (val >> 8) as u8
        };
        Some(byte)
    }

    /// Run the GSU for up to `budget` GSU clocks, or until STOP / a bus WAIT.
    /// Deterministic: driven by the Bus as a catch-up against elapsed master
    /// cycles (`gsu_clocks = master_cycles / clock_divider()`), analogous to the
    /// APU catch-up. The GSU stalls (WAIT) if it needs ROM but RON=0.
    pub fn run(&mut self, rom: &[u8], mut budget: i64) {
        if !self.go {
            return;
        }
        let dbg = std::env::var("GSU_TRACE").is_ok();
        if !self.primed {
            if dbg {
                eprintln!("GSU START pbr={:02X} r15={:04X} scmr={:02X} scbr={:02X} rombr={:02X} rambr={:02X} r0={:04X} r1={:04X} r2={:04X} r13={:04X} r14={:04X} r12={:04X} colr={:02X}",
                    self.pbr, self.r[15], self.scmr, self.scbr, self.rombr, self.rambr,
                    self.r[0], self.r[1], self.r[2], self.r[13], self.r[14], self.r[12], self.colr);
            }
            self.prime(rom);
            self.primed = true;
        }
        while self.go && budget > 0 {
            // GSU-side WAIT (superfx.md §4): the next opcode must be fetched from
            // Game Pak ROM but the SNES owns ROM (RON=0). A fetch that resolves to
            // a valid cache line needs no ROM bus, so it does not stall.
            if !self.rom_granted() && self.fetch_needs_rom() {
                break;
            }
            if dbg && std::env::var("GSU_TRACE_FULL").is_ok() {
                eprintln!(
                    "GSU r15={:04X} op={:02X} z={} cy={} s={} ov={} rombr={:02X} R0={:04X} R6={:04X} R7={:04X} R8={:04X} R9={:04X} R10={:04X} R11={:04X} R14={:04X}",
                    self.r[15], self.pipe,
                    self.z as u8, self.cy as u8, self.s as u8, self.ov as u8, self.rombr,
                    self.r[0], self.r[6], self.r[7], self.r[8], self.r[9], self.r[10], self.r[11], self.r[14]
                );
            }
            let was_go = self.go;
            self.maybe_trace(rom);
            let c = self.execute_one(rom);
            if dbg && was_go && !self.go {
                eprintln!("GSU STOP at pbr={:02X} r15={:04X}", self.pbr, self.r[15]);
            }
            budget -= c as i64;
        }
    }

    /// True when the next opcode fetch (at PBR:R15) would touch the Game Pak ROM
    /// bus: PBR points at a ROM bank ($00-$5F) and R15 is not served by a valid
    /// code-cache line. Used to decide whether a RON=0 fetch must WAIT.
    fn fetch_needs_rom(&self) -> bool {
        if self.pbr > 0x5F {
            return false; // PBR in RAM banks $70/$71 or cache: not a ROM fetch
        }
        let idx = self.r[15].wrapping_sub(self.cbr) as usize;
        if idx < 0x200 && self.cache_valid[idx >> 4] {
            return false; // served from a valid cache line
        }
        true
    }

    /// Execute a single GSU instruction, priming the pipeline first if needed.
    /// Returns the approximate GSU-clock cost.
    pub fn step(&mut self, rom: &[u8]) -> u32 {
        if !self.go {
            return 0;
        }
        if !self.primed {
            self.prime(rom);
            self.primed = true;
        }
        self.maybe_trace(rom);
        self.execute_one(rom)
    }

    // ---- Memory access -------------------------------------------------------

    fn map_rom_offset(&self, bank: u8, addr: u16) -> usize {
        let b = bank as usize;
        if b <= 0x3F {
            // LoROM: both halves of the 64K bank mirror the same 32K ROM bank.
            b * 0x8000 + (addr as usize & 0x7FFF)
        } else {
            // HiROM window $40-$5F: linear.
            (b - 0x40) * 0x10000 + addr as usize
        }
    }

    pub(crate) fn rom_byte(&self, rom: &[u8], bank: u8, addr: u16) -> u8 {
        if rom.is_empty() {
            return 0;
        }
        rom[self.map_rom_offset(bank, addr) % rom.len()]
    }

    fn ram_offset(&self, addr: u16) -> usize {
        (self.rambr as usize) * 0x10000 + addr as usize
    }

    pub(crate) fn ram_byte(&self, addr: u16) -> u8 {
        if self.ram.is_empty() {
            return 0;
        }
        let o = self.ram_offset(addr) % self.ram.len();
        self.ram[o]
    }

    pub(crate) fn ram_set(&mut self, addr: u16, v: u8) {
        if self.ram.is_empty() {
            return;
        }
        let o = self.ram_offset(addr) % self.ram.len();
        self.ram[o] = v;
    }

    // GSU word RAM access: the low byte lands at `addr`, the high byte at
    // `addr ^ 1` (superfx.md §9 "word at odd address accesses (addr AND NOT 1)
    // with LSB/MSB swapped"; bsnes writeRAMBuffer(ramaddr)/(ramaddr^1)). For an
    // even address this is the usual addr/addr+1; for an odd address the high
    // byte goes to addr-1, so STW/LDW/SM/LM/SBK to odd addresses byte-swap
    // rather than crossing into the next word.
    fn ram_read_word(&self, addr: u16) -> u16 {
        let lo = self.ram_byte(addr) as u16;
        let hi = self.ram_byte(addr ^ 1) as u16;
        lo | (hi << 8)
    }

    // ---- ROM/RAM read-ahead & write-queue buffers --------------------------

    /// GSU clocks for one buffered ROM/RAM transfer: 5 in 21.4 MHz mode
    /// (CLSR bit0 CLS=1), 6 in 10.7 MHz mode (bsnes `updateROMBuffer`/
    /// `writeRAMBuffer`: `regs.clsr ? 5 : 6`).
    fn buffer_latency(&self) -> u32 {
        if self.clsr & 0x01 != 0 {
            5
        } else {
            6
        }
    }

    /// Write register `n` and, if `n == 14`, flag that R14 changed this
    /// instruction (see `r14_written`). Every write counts, even a write of
    /// the same value (bsnes `Register::assign` sets `modified` unconditionally).
    #[inline]
    fn write_reg(&mut self, n: usize, v: u16) {
        self.r[n] = v;
        if n == 14 {
            self.r14_written = true;
        }
    }

    /// Re-arm the ROM read-ahead countdown after a write to R14 (SNES-side
    /// MMIO write, or a GSU instruction writing R14). Overwrites any prior
    /// in-flight countdown outright (bsnes `updateROMBuffer`: one buffer
    /// slot, no queueing of R14 writes). `romdr` keeps its stale value until
    /// expiry; nothing observes it before then because GETB/GETC/ROMB always
    /// force completion first via `sync_rom_buffer`.
    pub(crate) fn arm_rom_buffer(&mut self) {
        self.romcl = self.buffer_latency();
        self.rom_read = true;
    }

    /// Advance the ROM/RAM buffer countdowns by `elapsed` GSU clocks (called
    /// once per executed instruction with that instruction's own clock cost,
    /// approximating bsnes' interleaved `step(clocks)` calls during opcode
    /// fetch/execution). A countdown that reaches zero here latches using
    /// ROMBR/RAMBR *at this moment* -- the property that lets a ROMB between
    /// an R14 write and its GETB steer the captured bank.
    fn tick_buffers(&mut self, rom: &[u8], elapsed: u32) {
        if self.romcl > 0 {
            self.romcl = self.romcl.saturating_sub(elapsed);
            if self.romcl == 0 {
                self.romdr = self.gsu_bus_read(rom, self.rombr, self.r[14]);
                self.rom_read = false;
            }
        }
        if self.ramcl > 0 {
            self.ramcl = self.ramcl.saturating_sub(elapsed);
            if self.ramcl == 0 {
                self.ram_set(self.ramar, self.ramdr);
            }
        }
    }

    /// Force any pending ROM read-ahead to complete immediately, using
    /// ROMBR/R14 as they stand right now. Called before GETB/GETC read
    /// `romdr` and before ROMB changes ROMBR, so a ROMB that follows an
    /// in-flight R14 write cannot retroactively change the bank of a read
    /// hardware already started (bsnes `readROMBuffer`/`instructionGETC_
    /// RAMB_ROMB` both call `syncROMBuffer()` first). A no-op if nothing is
    /// pending (`romcl == 0`).
    fn sync_rom_buffer(&mut self, rom: &[u8]) {
        if self.romcl > 0 {
            self.romdr = self.gsu_bus_read(rom, self.rombr, self.r[14]);
            self.romcl = 0;
            self.rom_read = false;
        }
    }

    /// Force any queued RAM write to drain immediately (bsnes `syncRAMBuffer`).
    /// Called before RAM reads (LDW/LDB/LM/LMS), before a second buffered
    /// write reuses the queue slot, and before RAMB changes RAMBR (so the
    /// queued byte lands in the bank it was written under).
    pub(crate) fn sync_ram_buffer(&mut self) {
        if self.ramcl > 0 {
            self.ram_set(self.ramar, self.ramdr);
            self.ramcl = 0;
        }
    }

    /// Queue one buffered RAM byte write (STB/STW/SBK/SM/SMS byte lanes):
    /// drain any previous pending write first (it occupies the single
    /// buffer slot), then queue this one for `buffer_latency()` clocks out.
    fn ram_buffered_write_byte(&mut self, addr: u16, v: u8) {
        self.sync_ram_buffer();
        self.ramar = addr;
        self.ramdr = v;
        self.ramcl = self.buffer_latency();
    }

    fn ram_buffered_write_word(&mut self, addr: u16, v: u16) {
        self.ram_buffered_write_byte(addr, (v & 0xFF) as u8);
        self.ram_buffered_write_byte(addr ^ 1, (v >> 8) as u8);
    }

    /// Buffered RAM byte read: drain any pending queued write first (real
    /// RAM reads are not read-ahead buffered on GSU, only writes are queued;
    /// bsnes `readRAMBuffer` syncs then reads directly).
    fn ram_buffered_read_byte(&mut self, addr: u16) -> u8 {
        self.sync_ram_buffer();
        self.ram_byte(addr)
    }

    fn ram_buffered_read_word(&mut self, addr: u16) -> u16 {
        self.sync_ram_buffer();
        self.ram_read_word(addr)
    }

    /// Force both buffers to complete right away. Called when the GSU stops
    /// (STOP opcode, or SNES aborting via SFR GO=0) so a still-queued RAM
    /// write or in-flight ROM read-ahead is not silently lost: on hardware
    /// the internal clock keeps advancing (and thus keeps draining the
    /// buffers) even while GO=0 (bsnes `SuperFX::main` still calls `step(6)`
    /// every cycle when `sfr.g == 0`); our scheduler only advances clocks
    /// while executing instructions, so this is the equivalent drain point.
    pub(crate) fn flush_buffers(&mut self, rom: &[u8]) {
        self.sync_rom_buffer(rom);
        self.sync_ram_buffer();
    }

    /// Absolute (RAMBR-independent) byte access from RAM start; used by plotting,
    /// whose base is $700000 + SCBR*$400.
    pub(crate) fn ram_byte_abs(&self, off: usize) -> u8 {
        if self.ram.is_empty() {
            return 0;
        }
        self.ram[off % self.ram.len()]
    }

    pub(crate) fn ram_set_abs(&mut self, off: usize, v: u8) {
        if self.ram.is_empty() {
            return;
        }
        let n = self.ram.len();
        self.ram[off % n] = v;
    }

    /// GSU bus read with the hardware bank decode (superfx.md §4; bsnes
    /// `SuperFX::read`): `$00-3F` LoROM ROM, `$40-5F` HiROM ROM, `$60-7F` Game
    /// Pak RAM (linear, masked to RAM size), `$80-FF` open bus. Used for both
    /// opcode fetch (PBR) and GETB/GETC (ROMBR) so a ROMBR pointing at the RAM
    /// window reads RAM, not a wrapped ROM address.
    pub(crate) fn gsu_bus_read(&self, rom: &[u8], bank: u8, addr: u16) -> u8 {
        match bank {
            0x00..=0x5F => self.rom_byte(rom, bank, addr),
            0x60..=0x7F => {
                let off = ((bank as usize) << 16) | addr as usize;
                self.ram_byte_abs(off)
            }
            _ => 0, // $80-$FF: open bus
        }
    }

    fn underlying_code_byte(&self, rom: &[u8], bank: u8, addr: u16) -> u8 {
        self.gsu_bus_read(rom, bank, addr)
    }

    /// Opcode fetch through the code cache. Addresses inside the 512-byte window
    /// [CBR, CBR+$200) read the cache; a missed line is filled from the
    /// underlying ROM/RAM. Anything else reads directly.
    fn read_code(&mut self, rom: &[u8], bank: u8, addr: u16) -> u8 {
        let idx = addr.wrapping_sub(self.cbr) as usize;
        if idx < 0x200 {
            let line = idx >> 4;
            if !self.cache_valid[line] {
                let base = self.cbr.wrapping_add((line as u16) * 16);
                for i in 0..16u16 {
                    let byte = self.underlying_code_byte(rom, bank, base.wrapping_add(i));
                    self.cache[line * 16 + i as usize] = byte;
                }
                self.cache_valid[line] = true;
            }
            return self.cache[idx];
        }
        self.underlying_code_byte(rom, bank, addr)
    }

    /// Non-mutating equivalent of `read_code`, for the trace formatter: reads
    /// a cache line if already filled, otherwise computes the byte the way a
    /// real fetch would (`underlying_code_byte`) without filling the cache.
    /// Tracing must never change what/when the cache gets filled, so the
    /// emulator behaves identically with tracing on or off.
    pub(crate) fn peek_code(&self, rom: &[u8], bank: u8, addr: u16) -> u8 {
        let idx = addr.wrapping_sub(self.cbr) as usize;
        if idx < 0x200 && self.cache_valid[idx >> 4] {
            return self.cache[idx];
        }
        self.underlying_code_byte(rom, bank, addr)
    }

    fn prime(&mut self, rom: &[u8]) {
        self.pipe_pc = self.r[15];
        self.pipe = self.read_code(rom, self.pbr, self.r[15]);
        self.r[15] = self.r[15].wrapping_add(1);
    }

    /// Consume the prefetched byte and prefetch the next; advances R15.
    fn fetch(&mut self, rom: &[u8]) -> u8 {
        let out = self.pipe;
        self.pipe_pc = self.r[15];
        self.pipe = self.read_code(rom, self.pbr, self.r[15]);
        self.r[15] = self.r[15].wrapping_add(1);
        out
    }

    pub(crate) fn invalidate_cache(&mut self) {
        self.cache_valid = [false; 32];
    }

    // ---- Register helpers ----------------------------------------------------

    #[inline]
    fn src(&self) -> u16 {
        self.r[self.sreg]
    }

    #[inline]
    fn set_dst(&mut self, v: u16) {
        self.write_reg(self.dreg, v);
    }

    /// Update hang-diagnosis counters and, if a sink is installed, emit a
    /// trace line for the instruction about to execute. Called once per
    /// `execute_one` (see call sites in `run`/`step`), before any register or
    /// flag mutation. The counter/ring-buffer update is a fixed-size array
    /// store (no allocation); the trace line itself (which does allocate a
    /// `String`) is only built when `gsu_trace` is `Some`, so the normal
    /// (tracing-off) path stays allocation-free per instruction.
    fn maybe_trace(&mut self, rom: &[u8]) {
        let pc = self.r[15].wrapping_sub(1);
        self.recent_pc[self.recent_pc_idx] = pc;
        self.recent_pc_idx = (self.recent_pc_idx + 1) % RECENT_PC_LEN;
        self.instructions_executed = self.instructions_executed.wrapping_add(1);
        if self.gsu_trace.is_none() {
            return;
        }
        let mut fetch = |a: u32| self.peek_code(rom, (a >> 16) as u8, (a & 0xFFFF) as u16);
        let line = crate::debug::gsu_trace::gsu_trace_line(self, &mut fetch);
        if let Some(sink) = self.gsu_trace.as_mut() {
            sink(&line);
        }
    }

    // ---- Instruction execution ----------------------------------------------

    /// Common tail for prefix-only bytes (ALT1/ALT2/ALT3/TO/WITH/FROM
    /// selection): these cost 1 GSU clock and must NOT run the prefix-state
    /// reset (they ARE the prefix state being built), but GSU clock time
    /// still elapses, so a pending ROM/RAM buffer countdown still ticks.
    fn finish_prefix_only(&mut self, rom: &[u8]) -> u32 {
        self.tick_buffers(rom, 1);
        1
    }

    fn execute_one(&mut self, rom: &[u8]) -> u32 {
        let op = self.fetch(rom);
        let mut cycles: u32 = 1;
        let mut preserve_prefix = false;

        match op {
            // ---- Prefix opcodes (do not reset prefix state) ----
            0x3D => {
                self.alt1 = true;
                return self.finish_prefix_only(rom);
            }
            0x3E => {
                self.alt2 = true;
                return self.finish_prefix_only(rom);
            }
            0x3F => {
                self.alt1 = true;
                self.alt2 = true;
                return self.finish_prefix_only(rom);
            }
            0x10..=0x1F => {
                let n = (op & 0x0F) as usize;
                if self.b {
                    // MOVE Rd,Rs (TO byte under B flag). No flags.
                    self.write_reg(n, self.r[self.sreg]);
                    cycles = 2;
                } else {
                    self.dreg = n;
                    return self.finish_prefix_only(rom);
                }
            }
            0x20..=0x2F => {
                let n = (op & 0x0F) as usize;
                self.sreg = n;
                self.dreg = n;
                self.b = true;
                return self.finish_prefix_only(rom);
            }
            0xB0..=0xBF => {
                let n = (op & 0x0F) as usize;
                if self.b {
                    // MOVES Rd,Rs. Flags 000vs-z, OV = src bit7.
                    let v = self.r[n];
                    self.write_reg(self.dreg, v);
                    self.ov = v & 0x80 != 0;
                    self.s = v & 0x8000 != 0;
                    self.z = v == 0;
                    cycles = 2;
                } else {
                    self.sreg = n;
                    return self.finish_prefix_only(rom);
                }
            }

            // ---- Branches (rel8); prefixes preserved ----
            0x05..=0x0F => {
                self.branch(rom, op);
                preserve_prefix = true;
                cycles = 2;
            }

            // ---- Special / control ----
            0x00 => {
                // STOP: GO=0, IRQ=1.
                self.go = false;
                self.irq = true;
                self.primed = false;
                // Drain any in-flight ROM read-ahead / queued RAM write: on
                // hardware the buffer clocks keep advancing after GO=0 (see
                // `flush_buffers`), so nothing is lost.
                self.flush_buffers(rom);
            }
            0x01 => {} // NOP (still resets prefix state)
            0x02 => {
                // CACHE
                let target = self.r[15] & 0xFFF0;
                if self.cbr != target {
                    self.cbr = target;
                    self.invalidate_cache();
                }
            }
            0x3C => {
                // LOOP: R12=R12-1; if Z=0 then R15=R13. Flags 000-s-z.
                self.r[12] = self.r[12].wrapping_sub(1);
                self.s = self.r[12] & 0x8000 != 0;
                self.z = self.r[12] == 0;
                if !self.z {
                    self.r[15] = self.r[13];
                }
            }

            // ---- Shifts ----
            0x03 => {
                // LSR. Flags 000-0cz.
                let sv = self.src();
                let res = sv >> 1;
                self.cy = sv & 1 != 0;
                self.s = false;
                self.z = res == 0;
                self.set_dst(res);
            }
            0x04 => {
                // ROL. Flags 000-scz.
                let sv = self.src();
                let cin = self.cy as u16;
                let res = (sv << 1) | cin;
                self.cy = sv & 0x8000 != 0;
                self.s = res & 0x8000 != 0;
                self.z = res == 0;
                self.set_dst(res);
            }

            // ---- STW / STB (0x30-0x3B) ----
            0x30..=0x3B => {
                let n = (op & 0x0F) as usize;
                let addr = self.r[n];
                let v = self.src();
                if self.alt1 {
                    // STB
                    self.ram_buffered_write_byte(addr, (v & 0xFF) as u8);
                    cycles = 4;
                } else {
                    self.ram_buffered_write_word(addr, v);
                    cycles = 3;
                }
                self.last_ram_addr = addr;
            }

            // ---- LDW / LDB (0x40-0x4B) ----
            0x40..=0x4B => {
                let n = (op & 0x0F) as usize;
                let addr = self.r[n];
                if self.alt1 {
                    // LDB (zero-extend byte)
                    let b = self.ram_buffered_read_byte(addr) as u16;
                    self.set_dst(b);
                    cycles = 6;
                } else {
                    let w = self.ram_buffered_read_word(addr);
                    self.set_dst(w);
                    cycles = 7;
                }
                self.last_ram_addr = addr;
            }
            0x4C => {
                if self.alt1 {
                    // RPIX
                    let v = self.rpix();
                    self.set_dst(v);
                    cycles = 20;
                } else {
                    // PLOT
                    self.plot();
                }
            }
            0x4D => {
                // SWAP: Rd = Rs ROR 8.
                let sv = self.src();
                let res = (sv >> 8) | (sv << 8);
                self.s = res & 0x8000 != 0;
                self.z = res == 0;
                self.set_dst(res);
            }
            0x4E => {
                if self.alt1 {
                    // CMODE: POR = Rs AND 1Fh.
                    self.por = (self.src() & 0x1F) as u8;
                    cycles = 2;
                } else {
                    // COLOR: COLR = color(Rs AND FFh).
                    let c = self.apply_color((self.src() & 0xFF) as u8);
                    self.colr = c;
                }
            }
            0x4F => {
                // NOT: Rd = Rs XOR FFFFh.
                let res = self.src() ^ 0xFFFF;
                self.s = res & 0x8000 != 0;
                self.z = res == 0;
                self.set_dst(res);
            }

            // ---- ADD / ADC / ADD# / ADC# ----
            0x50..=0x5F => {
                let n = (op & 0x0F) as usize;
                let (operand, use_carry) = match (self.alt1, self.alt2) {
                    (false, false) => (self.r[n], false),
                    (true, false) => (self.r[n], true),
                    (false, true) => (n as u16, false),
                    (true, true) => (n as u16, true),
                };
                let sv = self.src();
                let cin = if use_carry && self.cy { 1i32 } else { 0 };
                let result = sv as i32 + operand as i32 + cin;
                self.cy = result > 0xFFFF;
                self.ov = (!(sv ^ operand) & (sv ^ result as u16) & 0x8000) != 0;
                self.s = result & 0x8000 != 0;
                self.z = (result as u16) == 0;
                self.set_dst(result as u16);
                if self.alt1 || self.alt2 {
                    cycles = 2;
                }
            }

            // ---- SUB / SBC / SUB# / CMP ----
            0x60..=0x6F => {
                let n = (op & 0x0F) as usize;
                let (operand, sbc, cmp) = match (self.alt1, self.alt2) {
                    (false, false) => (self.r[n], false, false),
                    (true, false) => (self.r[n], true, false),
                    (false, true) => (n as u16, false, false),
                    (true, true) => (self.r[n], false, true),
                };
                let sv = self.src();
                let bin = if sbc && !self.cy { 1i32 } else { 0 };
                let result = sv as i32 - operand as i32 - bin;
                self.cy = result >= 0;
                self.ov = ((sv ^ operand) & (sv ^ result as u16) & 0x8000) != 0;
                self.s = result & 0x8000 != 0;
                self.z = (result as u16) == 0;
                if !cmp {
                    self.set_dst(result as u16);
                }
                if self.alt1 || self.alt2 {
                    cycles = 2;
                }
            }

            // ---- MERGE / AND / BIC / AND# / BIC# ----
            0x70 => {
                // MERGE: Rd = (R7 AND FF00h) + (R8 >> 8).
                let res = (self.r[7] & 0xFF00) | (self.r[8] >> 8);
                self.s = res & 0x8080 != 0;
                self.ov = res & 0xC0C0 != 0;
                self.cy = res & 0xE0E0 != 0;
                self.z = res & 0xF0F0 != 0;
                self.set_dst(res);
            }
            0x71..=0x7F => {
                let n = (op & 0x0F) as usize;
                let (operand, bic) = match (self.alt1, self.alt2) {
                    (false, false) => (self.r[n], false),
                    (true, false) => (self.r[n], true),
                    (false, true) => (n as u16, false),
                    (true, true) => (n as u16, true),
                };
                let sv = self.src();
                let res = if bic { sv & !operand } else { sv & operand };
                self.s = res & 0x8000 != 0;
                self.z = res == 0;
                self.set_dst(res);
                if self.alt1 || self.alt2 {
                    cycles = 2;
                }
            }

            // ---- MULT / UMULT / MULT# / UMULT# ----
            0x80..=0x8F => {
                let n = (op & 0x0F) as usize;
                let a = self.src() & 0xFF;
                let bv = self.r[n] & 0xFF;
                let res: u16 = match (self.alt1, self.alt2) {
                    (false, false) => ((a as i8 as i32) * (bv as i8 as i32)) as u16,
                    (true, false) => ((a as u32) * (bv as u32)) as u16,
                    (false, true) => ((a as i8 as i32) * (n as i8 as i32)) as u16,
                    (true, true) => ((a as u32) * (n as u32)) as u16,
                };
                self.s = res & 0x8000 != 0;
                self.z = res == 0;
                self.set_dst(res);
                cycles = 2;
            }

            // ---- SBK / LINK / SEX / ASR / ROR / JMP / LOB / FMULT ----
            0x90 => {
                // SBK: word[last RAM addr] = Rs.
                let v = self.src();
                let addr = self.last_ram_addr;
                self.ram_buffered_write_word(addr, v);
            }
            0x91..=0x94 => {
                // LINK #n: R11 = R15 + n.
                let n = (op & 0x0F) as u16;
                self.r[11] = self.r[15].wrapping_add(n);
            }
            0x95 => {
                // SEX: sign-extend low byte.
                let res = (self.src() as u8) as i8 as i16 as u16;
                self.s = res & 0x8000 != 0;
                self.z = res == 0;
                self.set_dst(res);
            }
            0x96 => {
                // ASR (base); DIV2 (ALT1 = ASR but 0 if Rs = -1).
                let sv = self.src();
                let res = if self.alt1 && sv == 0xFFFF {
                    0
                } else {
                    ((sv as i16) >> 1) as u16
                };
                self.cy = sv & 1 != 0;
                self.s = res & 0x8000 != 0;
                self.z = res == 0;
                self.set_dst(res);
                if self.alt1 {
                    cycles = 2;
                }
            }
            0x97 => {
                // ROR.
                let sv = self.src();
                let cin = if self.cy { 0x8000u16 } else { 0 };
                let res = (sv >> 1) | cin;
                self.cy = sv & 1 != 0;
                self.s = res & 0x8000 != 0;
                self.z = res == 0;
                self.set_dst(res);
            }
            0x98..=0x9D => {
                let n = (op & 0x0F) as usize;
                if self.alt1 {
                    // LJMP Rn: R15 = Rs, PBR = Rn low byte, CBR = R15 & FFF0h.
                    self.pbr = (self.r[n] & 0xFF) as u8;
                    self.r[15] = self.src();
                    self.cbr = self.r[15] & 0xFFF0;
                    self.invalidate_cache();
                    cycles = 2;
                } else {
                    // JMP Rn.
                    self.r[15] = self.r[n];
                }
            }
            0x9E => {
                // LOB: Rd = Rs AND FFh, SF from bit7.
                let res = self.src() & 0xFF;
                self.s = res & 0x80 != 0;
                self.z = res == 0;
                self.set_dst(res);
            }
            0x9F => {
                // FMULT (base) / LMULT (ALT1).
                let sv = self.src() as i16 as i32;
                let r6 = self.r[6] as i16 as i32;
                let result = sv * r6;
                if self.alt1 {
                    self.r[4] = result as u16;
                }
                let hi = (result >> 16) as u16;
                self.cy = (result & 0x8000) != 0;
                self.s = (result as u32 & 0x8000_0000) != 0;
                self.z = hi == 0;
                // FMULT with Dreg=R4 leaves R4 unchanged (superfx.md §9); LMULT
                // (ALT1) always writes the high word to Dreg, even R4.
                if self.alt1 || self.dreg != 4 {
                    self.set_dst(hi);
                }
                cycles = if self.alt1 { 9 } else { 4 };
            }

            // ---- IBT / LMS / SMS (0xA0-0xAF) ----
            0xA0..=0xAF => {
                let n = (op & 0x0F) as usize;
                if self.alt1 {
                    // LMS Rn,(kk): addr = kk*2.
                    let kk = self.fetch(rom) as u16;
                    let addr = kk << 1;
                    let w = self.ram_buffered_read_word(addr);
                    self.write_reg(n, w);
                    self.last_ram_addr = addr;
                    cycles = 10;
                } else if self.alt2 {
                    // SMS (kk),Rn.
                    let kk = self.fetch(rom) as u16;
                    let addr = kk << 1;
                    self.ram_buffered_write_word(addr, self.r[n]);
                    self.last_ram_addr = addr;
                    cycles = 8;
                } else {
                    // IBT Rn,#pp (sign-extended).
                    let pp = self.fetch(rom);
                    self.write_reg(n, pp as i8 as i16 as u16);
                    cycles = 2;
                }
            }

            // ---- HIB / OR / XOR / OR# / XOR# ----
            0xC0 => {
                // HIB: Rd = Rs >> 8, SF from bit7.
                let res = self.src() >> 8;
                self.s = res & 0x80 != 0;
                self.z = res == 0;
                self.set_dst(res);
            }
            0xC1..=0xCF => {
                let n = (op & 0x0F) as usize;
                let (operand, xor) = match (self.alt1, self.alt2) {
                    (false, false) => (self.r[n], false),
                    (true, false) => (self.r[n], true),
                    (false, true) => (n as u16, false),
                    (true, true) => (n as u16, true),
                };
                let sv = self.src();
                let res = if xor { sv ^ operand } else { sv | operand };
                self.s = res & 0x8000 != 0;
                self.z = res == 0;
                self.set_dst(res);
                if self.alt1 || self.alt2 {
                    cycles = 2;
                }
            }

            // ---- INC / GETC / RAMB / ROMB ----
            0xD0..=0xDE => {
                let n = (op & 0x0F) as usize;
                self.write_reg(n, self.r[n].wrapping_add(1));
                self.s = self.r[n] & 0x8000 != 0;
                self.z = self.r[n] == 0;
            }
            0xDF => {
                if self.alt2 && !self.alt1 {
                    // RAMB: RAMBR = Rs AND 01h. Sync the queued write first
                    // (bsnes `syncRAMBuffer()` before assigning) so it drains
                    // into the OLD bank, not the new one.
                    self.sync_ram_buffer();
                    self.rambr = (self.src() & 0x01) as u8;
                    cycles = 2;
                } else if self.alt1 && self.alt2 {
                    // ROMB: ROMBR = Rs AND FFh. Sync the pending read-ahead
                    // first (bsnes `syncROMBuffer()` before assigning) so an
                    // in-flight prefetch latches under the OLD bank, not the
                    // one this instruction is about to set.
                    self.sync_rom_buffer(rom);
                    self.rombr = (self.src() & 0xFF) as u8;
                    cycles = 2;
                } else {
                    // GETC: COLR = color(romdr). Never reads the bus live.
                    self.sync_rom_buffer(rom);
                    let b = self.romdr;
                    self.colr = self.apply_color(b);
                }
            }

            // ---- DEC / GETB(H/L/S) ----
            0xE0..=0xEE => {
                let n = (op & 0x0F) as usize;
                self.write_reg(n, self.r[n].wrapping_sub(1));
                self.s = self.r[n] & 0x8000 != 0;
                self.z = self.r[n] == 0;
            }
            0xEF => {
                // GETB/GETBH/GETBL/GETBS: Rd = romdr (the read-ahead buffer
                // latched by the last R14 write), never a live bus read
                // (superfx.md §7; bsnes `readROMBuffer`).
                self.sync_rom_buffer(rom);
                let b = self.romdr;
                if std::env::var("GSU_GETB").is_ok() {
                    eprintln!("GETB rombr={:02X} r14={:04X} -> {:02X}", self.rombr, self.r[14], b);
                }
                match (self.alt1, self.alt2) {
                    (false, false) => self.set_dst(b as u16), // GETB
                    (true, false) => {
                        // GETBH: Rd.hi = byte, lo unchanged.
                        let d = self.r[self.dreg];
                        self.set_dst((d & 0x00FF) | ((b as u16) << 8));
                    }
                    (false, true) => {
                        // GETBL: Rd.lo = byte, hi unchanged.
                        let d = self.r[self.dreg];
                        self.set_dst((d & 0xFF00) | (b as u16));
                    }
                    (true, true) => self.set_dst(b as i8 as i16 as u16), // GETBS
                }
                cycles = 3;
            }

            // ---- IWT / LM / SM (0xF0-0xFF) ----
            0xF0..=0xFF => {
                let n = (op & 0x0F) as usize;
                if self.alt1 {
                    // LM Rn,(hilo).
                    let lo = self.fetch(rom) as u16;
                    let hi = self.fetch(rom) as u16;
                    let addr = lo | (hi << 8);
                    let w = self.ram_buffered_read_word(addr);
                    self.write_reg(n, w);
                    self.last_ram_addr = addr;
                    cycles = 11;
                } else if self.alt2 {
                    // SM (hilo),Rn.
                    let lo = self.fetch(rom) as u16;
                    let hi = self.fetch(rom) as u16;
                    let addr = lo | (hi << 8);
                    self.ram_buffered_write_word(addr, self.r[n]);
                    self.last_ram_addr = addr;
                    cycles = 9;
                } else {
                    // IWT Rn,#yyxx.
                    let lo = self.fetch(rom) as u16;
                    let hi = self.fetch(rom) as u16;
                    self.write_reg(n, lo | (hi << 8));
                    cycles = 3;
                }
            }
        }

        if !preserve_prefix {
            self.alt1 = false;
            self.alt2 = false;
            self.b = false;
            self.sreg = 0;
            self.dreg = 0;
        }

        // ROM/RAM read-ahead & write-queue bookkeeping (superfx.md §7; bsnes
        // `step()`/`SuperFX::main`). Decay any countdown that was already
        // pending *before* this instruction by this instruction's own clock
        // cost, THEN (not before) re-arm a fresh countdown if this
        // instruction wrote R14 -- the new countdown must not be shortened
        // by the clocks of the very instruction that started it (bsnes only
        // calls `updateROMBuffer()` after `instruction()` has fully returned).
        self.tick_buffers(rom, cycles);
        if self.r14_written {
            self.r14_written = false;
            self.arm_rom_buffer();
        }
        cycles
    }

    fn branch(&mut self, rom: &[u8], op: u8) {
        let disp = self.fetch(rom) as i8 as i16;
        let take = match op {
            0x05 => true,                 // BRA
            0x06 => (self.s ^ self.ov) == false, // BGE
            0x07 => (self.s ^ self.ov) == true,  // BLT
            0x08 => !self.z,              // BNE
            0x09 => self.z,               // BEQ
            0x0A => !self.s,              // BPL
            0x0B => self.s,               // BMI
            0x0C => !self.cy,             // BCC
            0x0D => self.cy,              // BCS
            0x0E => !self.ov,             // BVC
            0x0F => self.ov,              // BVS
            _ => false,
        };
        if take {
            self.r[15] = self.r[15].wrapping_add(disp as u16);
        }
    }
}
