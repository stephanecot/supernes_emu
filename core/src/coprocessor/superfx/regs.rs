//! SNES-visible GSU register file at $3000-$34FF (low 16 bits of the bus
//! address within banks $00-$3F/$80-$BF).
//!
//! $3000-$301F  R0-R15 (16-bit, write latch on even byte, commit on odd byte)
//! $3030/$3031  SFR (status/flags)
//! $3033        BRAMR   (W)
//! $3034        PBR     (R/W)
//! $3036        ROMBR   (R)
//! $3037        CFGR    (W)
//! $3038        SCBR    (W)
//! $3039        CLSR    (W)
//! $303A        SCMR    (W)
//! $303B        VCR     (R)  version code
//! $303C        RAMBR   (R)
//! $303E/$303F  CBR     (R)
//! $3100-$32FF  512-byte code cache

use super::gsu::SuperFx;

impl SuperFx {
    /// Assemble the 16-bit SFR from decomposed flag state.
    fn sfr(&self) -> u16 {
        let mut v = 0u16;
        if self.z {
            v |= 0x0002;
        }
        if self.cy {
            v |= 0x0004;
        }
        if self.s {
            v |= 0x0008;
        }
        if self.ov {
            v |= 0x0010;
        }
        if self.go {
            v |= 0x0020;
        }
        if self.rom_read {
            v |= 0x0040;
        }
        if self.alt1 {
            v |= 0x0100;
        }
        if self.alt2 {
            v |= 0x0200;
        }
        if self.b {
            v |= 0x1000;
        }
        if self.irq {
            v |= 0x8000;
        }
        v
    }

    /// SNES read of a GSU register byte. `addr` is the 16-bit in-bank offset.
    pub fn read_mmio(&mut self, addr: u16) -> u8 {
        match addr {
            0x3000..=0x301F => {
                let n = ((addr - 0x3000) >> 1) as usize;
                if addr & 1 == 0 {
                    (self.r[n] & 0xFF) as u8
                } else {
                    (self.r[n] >> 8) as u8
                }
            }
            0x3030 => (self.sfr() & 0xFF) as u8,
            0x3031 => {
                let hi = (self.sfr() >> 8) as u8;
                // Reading the SFR high byte (IRQ bit) clears the IRQ flag.
                self.irq = false;
                hi
            }
            0x3034 => self.pbr,
            0x3036 => self.rombr,
            0x3037 => self.cfgr,
            0x3038 => self.scbr,
            0x3039 => self.clsr,
            0x303A => self.scmr,
            0x303B => self.version,
            0x303C => self.rambr,
            0x303E => (self.cbr & 0xFF) as u8,
            0x303F => (self.cbr >> 8) as u8,
            0x3100..=0x32FF => self.cache[((addr - 0x3100) & 0x1FF) as usize],
            _ => 0,
        }
    }

    /// SNES write to a GSU register byte. `addr` is the 16-bit in-bank offset.
    pub fn write_mmio(&mut self, addr: u16, value: u8) {
        match addr {
            0x3000..=0x301F => {
                let n = ((addr - 0x3000) >> 1) as usize;
                if addr & 1 == 0 {
                    // Even byte: latch the low byte.
                    self.latch = value;
                } else {
                    // Odd byte: commit LSB = latch, MSB = value.
                    self.r[n] = (self.latch as u16) | ((value as u16) << 8);
                    if addr == 0x301F {
                        // Writing R15 MSB sets GO=1 and starts the GSU.
                        self.go = true;
                        self.primed = false;
                    }
                }
            }
            0x3030 => {
                // SFR low byte holds the R/W flag bits (1-5).
                self.z = value & 0x02 != 0;
                self.cy = value & 0x04 != 0;
                self.s = value & 0x08 != 0;
                self.ov = value & 0x10 != 0;
                let go_new = value & 0x20 != 0;
                if !go_new {
                    // Aborting: forces CBR=0 and empties the cache.
                    self.go = false;
                    self.cbr = 0;
                    self.invalidate_cache();
                    self.primed = false;
                } else if !self.go {
                    // GO 0->1 start: the pipeline (re-)primes on the next run.
                    self.go = true;
                    self.primed = false;
                }
                // SFR is R/W while the GSU runs (superfx.md §2/§5): a GO=1 write
                // to an already-running GSU must not restart or re-prime the
                // pipeline, so `primed` is left untouched here.
            }
            0x3031 => {} // SFR high byte bits are internal; SNES writes ignored.
            0x3033 => self.bramr = value,
            0x3034 => self.pbr = value,
            0x3037 => self.cfgr = value,
            0x3038 => self.scbr = value,
            0x3039 => self.clsr = value,
            0x303A => self.scmr = value,
            0x3100..=0x32FF => {
                let off = ((addr - 0x3100) & 0x1FF) as usize;
                self.cache[off] = value;
                // Writing the last byte of a 16-byte line marks it non-empty.
                if off & 0x0F == 0x0F {
                    self.cache_valid[off >> 4] = true;
                }
            }
            _ => {}
        }
    }
}
