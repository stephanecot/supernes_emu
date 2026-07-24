//! SA-1 memory-mapped I/O ($2200-$23FF) plus BW-RAM/I-RAM protection, the H/V
//! timer, and the normal DMA controller. Register bits per `sa1.md` §3.

use super::Sa1State;

impl Sa1State {
    /// Side-effect-free program-byte fetch for the `--trace-sa1` disassembler.
    /// Covers the SA-1's code regions (I-RAM, BW-RAM, ROM via the MMC, and the
    /// intercepted own-vectors); I/O space returns 0 (never contains code).
    pub(crate) fn fetch_no_tick(&self, rom: &[u8], addr: u32) -> u8 {
        let addr = addr & 0xFF_FFFF;
        match addr {
            0x00_FFFA | 0x00_FFEA => return self.cnv as u8,
            0x00_FFFB | 0x00_FFEB => return (self.cnv >> 8) as u8,
            0x00_FFFE | 0x00_FFEE => return self.civ as u8,
            0x00_FFFF | 0x00_FFEF => return (self.civ >> 8) as u8,
            0x00_FFFC => return self.crv as u8,
            0x00_FFFD => return (self.crv >> 8) as u8,
            _ => {}
        }
        if let Some(o) = self.mmc.rom_offset(addr) {
            if rom.is_empty() {
                return 0;
            }
            return rom.get(o % rom.len()).copied().unwrap_or(0);
        }
        let bank = (addr >> 16) & 0xFF;
        let off = (addr & 0xFFFF) as u16;
        match bank {
            0x00..=0x3F | 0x80..=0xBF => match off {
                0x0000..=0x07FF | 0x3000..=0x37FF => self.iram[off as usize & 0x7FF],
                _ => 0,
            },
            0x40..=0x4F if !self.bwram.is_empty() => {
                let o = ((bank - 0x40) << 16 | off as u32) as usize;
                self.bwram[o % self.bwram.len()]
            }
            _ => 0,
        }
    }

    // ---- Register I/O ------------------------------------------------------

    /// Read a status register ($2300-$23FF). Config registers ($2200-$22FF) are
    /// write-only and read back as open bus (0).
    pub(crate) fn read_io(&mut self, rom: &[u8], addr: u16) -> u8 {
        match addr {
            0x2300 => {
                // SFR `IVDNmmmm`.
                (self.sfr_irq as u8) << 7
                    | (self.scpu_irq_sel as u8) << 6
                    | (self.sfr_dma as u8) << 5
                    | (self.scpu_nmi_sel as u8) << 4
                    | (self.msg_to_scpu & 0x0F)
            }
            0x2301 => {
                // CFR `ITDNmmmm`.
                (self.cfr_irq as u8) << 7
                    | (self.cfr_timer as u8) << 6
                    | (self.cfr_dma as u8) << 5
                    | (self.cfr_nmi as u8) << 4
                    | (self.msg_to_sa1 & 0x0F)
            }
            0x2302 => {
                // HCR/VCR latch the live H/V counters on the $2302 read;
                // $2303-$2305 then return the same snapshot (bsnes io.cpp).
                self.h_latch = self.h_count;
                self.v_latch = self.v_count;
                self.h_latch as u8
            }
            0x2303 => (self.h_latch >> 8) as u8 & 0x01,
            0x2304 => self.v_latch as u8,
            0x2305 => (self.v_latch >> 8) as u8 & 0x01,
            0x2306..=0x230A => self.arith.mr_byte((addr - 0x2306) as u32),
            0x230B => (self.arith.of as u8) << 7,
            0x230C => self.bits.peek(rom, &self.mmc) as u8,
            0x230D => {
                let hi = (self.bits.peek(rom, &self.mmc) >> 8) as u8;
                if self.bits.auto {
                    self.bits.advance();
                }
                hi
            }
            // VC ($230E) is open bus on hardware.
            _ => 0,
        }
    }

    /// Write a config register ($2200-$22FF). Reads of $2300+ are ignored here.
    pub(crate) fn write_io(&mut self, rom: &[u8], addr: u16, v: u8) {
        match addr {
            0x2200 => {
                // CCNT `IRrNmmmm`.
                self.msg_to_sa1 = v & 0x0F;
                self.sa1_wait = v & 0x40 != 0;
                let new_reset = v & 0x20 != 0;
                if self.sa1_reset && !new_reset {
                    // Reset released (1->0): boot the SA-1 CPU from CRV.
                    self.reset_pending = true;
                }
                self.sa1_reset = new_reset;
                if v & 0x80 != 0 {
                    self.cfr_irq = true;
                }
                if v & 0x10 != 0 {
                    self.cfr_nmi = true;
                }
            }
            0x2201 => self.sie = v & 0xA0,
            0x2202 => {
                if v & 0x80 != 0 {
                    self.sfr_irq = false;
                }
                if v & 0x20 != 0 {
                    self.sfr_dma = false;
                }
            }
            0x2203 => self.crv = (self.crv & 0xFF00) | v as u16,
            0x2204 => self.crv = (self.crv & 0x00FF) | (v as u16) << 8,
            0x2205 => self.cnv = (self.cnv & 0xFF00) | v as u16,
            0x2206 => self.cnv = (self.cnv & 0x00FF) | (v as u16) << 8,
            0x2207 => self.civ = (self.civ & 0xFF00) | v as u16,
            0x2208 => self.civ = (self.civ & 0x00FF) | (v as u16) << 8,
            0x2209 => {
                // SCNT `IS-Nmmmm`.
                self.msg_to_scpu = v & 0x0F;
                self.scpu_irq_sel = v & 0x40 != 0;
                self.scpu_nmi_sel = v & 0x10 != 0;
                if v & 0x80 != 0 {
                    self.sfr_irq = true;
                }
            }
            0x220A => self.cie = v & 0xF0,
            0x220B => {
                if v & 0x80 != 0 {
                    self.cfr_irq = false;
                }
                if v & 0x40 != 0 {
                    self.cfr_timer = false;
                }
                if v & 0x20 != 0 {
                    self.cfr_dma = false;
                }
                if v & 0x10 != 0 {
                    self.cfr_nmi = false;
                }
            }
            0x220C => self.snv = (self.snv & 0xFF00) | v as u16,
            0x220D => self.snv = (self.snv & 0x00FF) | (v as u16) << 8,
            0x220E => self.siv = (self.siv & 0xFF00) | v as u16,
            0x220F => self.siv = (self.siv & 0x00FF) | (v as u16) << 8,

            // H/V timer.
            0x2210 => self.tmc = v,
            0x2211 => {
                self.h_count = 0;
                self.v_count = 0;
                self.linear = 0;
            }
            0x2212 => self.h_target = (self.h_target & 0xFF00) | v as u16,
            0x2213 => self.h_target = (self.h_target & 0x00FF) | ((v as u16 & 0x01) << 8),
            0x2214 => self.v_target = (self.v_target & 0xFF00) | v as u16,
            0x2215 => self.v_target = (self.v_target & 0x00FF) | ((v as u16 & 0x01) << 8),

            // Super MMC.
            0x2220..=0x2223 => self.mmc.write_bank((addr - 0x2220) as usize, v),
            0x2224 => self.mmc.bmaps = v & 0x1F,
            0x2225 => self.mmc.bmap = v,

            // Protection.
            0x2226 => self.sbwe = v & 0x80 != 0,
            0x2227 => self.cbwe = v & 0x80 != 0,
            0x2228 => self.bwpa = v & 0x0F,
            0x2229 => self.siwp = v,
            0x222A => self.ciwp = v,

            // DMA.
            0x2230 => self.dcnt = v,
            0x2231 => self.cdma = v,
            0x2232 => self.dma_src = (self.dma_src & 0xFFFF00) | v as u32,
            0x2233 => self.dma_src = (self.dma_src & 0xFF00FF) | (v as u32) << 8,
            0x2234 => self.dma_src = (self.dma_src & 0x00FFFF) | (v as u32) << 16,
            0x2235 => self.dma_dst = (self.dma_dst & 0xFFFF00) | v as u32,
            0x2236 => {
                self.dma_dst = (self.dma_dst & 0xFF00FF) | (v as u32) << 8;
                // Writing $2236 triggers a normal DMA to I-RAM.
                if self.dcnt & 0x80 != 0 && self.dcnt & 0x20 == 0 {
                    self.dma_normal(rom, false);
                }
            }
            0x2237 => {
                self.dma_dst = (self.dma_dst & 0x00FFFF) | (v as u32) << 16;
                // Writing $2237 triggers a normal DMA to BW-RAM.
                if self.dcnt & 0x80 != 0 && self.dcnt & 0x20 == 0 {
                    self.dma_normal(rom, true);
                }
            }
            0x2238 => self.dma_cnt = (self.dma_cnt & 0xFF00) | v as u16,
            0x2239 => self.dma_cnt = (self.dma_cnt & 0x00FF) | (v as u16) << 8,
            0x223F => self.bbf = v,
            0x2240..=0x224F => self.brf[(addr - 0x2240) as usize] = v,

            // Arithmetic unit.
            0x2250 => self.arith.write_mcnt(v),
            0x2251 => self.arith.ma = (self.arith.ma & 0xFF00) | v as u16,
            0x2252 => self.arith.ma = (self.arith.ma & 0x00FF) | (v as u16) << 8,
            0x2253 => self.arith.mb = (self.arith.mb & 0xFF00) | v as u16,
            0x2254 => {
                self.arith.mb = (self.arith.mb & 0x00FF) | (v as u16) << 8;
                self.arith.execute();
            }

            // Variable-length bit reader.
            0x2258 => self.bits.write_vbd(v),
            0x2259 => self.bits.va = (self.bits.va & 0xFFFF00) | v as u32,
            0x225A => self.bits.va = (self.bits.va & 0xFF00FF) | (v as u32) << 8,
            0x225B => {
                self.bits.va = (self.bits.va & 0x00FFFF) | (v as u32) << 16;
                self.bits.vbit = 0;
            }

            _ => {}
        }
    }

    // ---- Normal DMA --------------------------------------------------------

    /// Byte-copy DMA (`sa1.md` §6). Source per DCNT.SS, dest chosen by the trigger
    /// address. On completion the pollable CFR.D flag is always set; CIE.D only
    /// gates the IRQ line (in `irq_level`), so games may poll CFR.D with the IRQ
    /// masked (bsnes dma.cpp sets `dma_irqfl` unconditionally).
    fn dma_normal(&mut self, rom: &[u8], to_bwram: bool) {
        let src_type = self.dcnt & 0x03;
        let count = self.dma_cnt as usize;
        for i in 0..count {
            let s = self.dma_src.wrapping_add(i as u32) & 0xFF_FFFF;
            let byte = match src_type {
                0 => self
                    .mmc
                    .rom_offset(s)
                    .and_then(|o| rom.get(o).copied())
                    .unwrap_or(0),
                1 => {
                    if self.bwram.is_empty() {
                        0
                    } else {
                        self.bwram[s as usize % self.bwram.len()]
                    }
                }
                _ => self.iram[s as usize & 0x7FF],
            };
            let d = self.dma_dst.wrapping_add(i as u32);
            if to_bwram {
                if !self.bwram.is_empty() {
                    let idx = d as usize % self.bwram.len();
                    self.bwram[idx] = byte;
                }
            } else {
                self.iram[d as usize & 0x7FF] = byte;
            }
        }
        self.cfr_dma = true;
    }

    // ---- BW-RAM / I-RAM writes with protection -----------------------------

    /// BW-RAM write with the exact bsnes protection gate (`bwram.cpp`
    /// `writeCPU`/`writeLinear`): a write is dropped only when **both** master
    /// write-enables are off (SBWE and CBWE) **and** the address lies within the
    /// BWPA protected area (`offset & 0x3FFFF < 0x100 << bwpa`). Otherwise it
    /// stores — a single enable bit, or an address outside the protected area,
    /// permits the write regardless of which CPU issued it. (The earlier
    /// per-CPU `SBWE ? : CBWE` gate wrongly dropped SMRPG's `$40:3D00` handshake
    /// write, which is outside the protected area with both enables off.)
    pub(crate) fn write_bwram(&mut self, _from_scpu: bool, offset: usize, value: u8) {
        if self.bwram.is_empty() {
            return;
        }
        if !self.sbwe
            && !self.cbwe
            && (offset & 0x3FFFF) < (0x100usize << self.bwpa)
        {
            return;
        }
        let idx = offset % self.bwram.len();
        self.bwram[idx] = value;
    }

    /// I-RAM write honouring the per-256-byte-page protection (SIWP / CIWP).
    pub(crate) fn write_iram(&mut self, from_scpu: bool, off: usize, value: u8) {
        let off = off & 0x7FF;
        let page = off >> 8;
        let mask = if from_scpu { self.siwp } else { self.ciwp };
        if mask & (1 << page) != 0 {
            self.iram[off] = value;
        }
    }

    // ---- H/V timer ---------------------------------------------------------

    /// Advance the timer by `cycles` SA-1 cycles and raise the timer IRQ (CFR.T)
    /// on a compare match. Linear (18-bit) mode is exact; H/V mode is an
    /// approximation of the PPU dot/scanline counters (no dot-clock sync).
    pub(crate) fn tick_timer(&mut self, cycles: u32) {
        if self.tmc & 0x80 != 0 {
            // Linear 18-bit free-running counter.
            let target = (self.h_target as u32 & 0x1FF) | ((self.v_target as u32 & 0x1FF) << 9);
            let prev = self.linear;
            self.linear = (self.linear + cycles) & 0x3FFFF;
            if crossed(prev, self.linear, target, 0x3FFFF) {
                self.cfr_timer = true;
            }
        } else {
            // H/V mode: 341 dots/line, 262 lines (NTSC), ~2 SA-1 cycles/dot.
            // TMC.0 = H-enable, TMC.1 = V-enable (`sa1.md` §3.2):
            //   H-only  -> fires each line when H reaches HCNT;
            //   V-only  -> fires at H=0 of the line where V reaches VCNT;
            //   both    -> fires once per frame when H==HCNT AND V==VCNT.
            let h_en = self.tmc & 0x01 != 0;
            let v_en = self.tmc & 0x02 != 0;
            let h_tgt = self.h_target as u32 & 0x1FF;
            let v_tgt = self.v_target as u32 & 0x1FF;
            let prev_h = self.h_count as u32;
            let mut dots = prev_h + cycles / 2;
            let mut new_line = false;
            while dots >= 341 {
                dots -= 341;
                self.v_count = (self.v_count + 1) % 262;
                new_line = true;
                if v_en && !h_en && self.v_count as u32 == v_tgt {
                    self.cfr_timer = true;
                }
            }
            let new_h = dots;
            // Within the final line, did H pass HCNT? A freshly entered line
            // spans dots 0..=new_h.
            let h_hit = if new_line {
                h_tgt <= new_h
            } else {
                crossed(prev_h, new_h, h_tgt, 340)
            };
            let v_match = self.v_count as u32 == v_tgt;
            if h_en && h_hit && (!v_en || v_match) {
                self.cfr_timer = true;
            }
            self.h_count = new_h as u16;
        }
    }
}

/// True if the counter stepping from `prev` to `now` (wrapping at `wrap`) passed
/// through `target`.
fn crossed(prev: u32, now: u32, target: u32, wrap: u32) -> bool {
    if now >= prev {
        target > prev && target <= now
    } else {
        // Wrapped around `wrap`.
        target > prev || target <= now || target == wrap.wrapping_add(1)
    }
}
