//! The SA-1's view of memory, as a `CpuBus` the reused 65C816 core drives.
//!
//! The SA-1 CPU boots and takes interrupts from its own vectors (CRV/CNV/CIV,
//! `$2203-$2208`), not the cartridge vectors; those addresses are intercepted
//! here. Access cost is accumulated in `cycles` (ROM/BW-RAM = 2, I-RAM/I/O = 1,
//! internal cycle = 1) so `Sa1::run` can debit the SA-1 cycle budget. `sa1.md`
//! §1-2.

use super::{mmc, Sa1State};
use crate::cpu::CpuBus;

pub struct Sa1Bus<'a> {
    st: &'a mut Sa1State,
    rom: &'a [u8],
    /// SA-1 cycles consumed since this bus was created.
    pub cycles: u32,
}

impl<'a> Sa1Bus<'a> {
    pub fn new(st: &'a mut Sa1State, rom: &'a [u8]) -> Self {
        Sa1Bus { st, rom, cycles: 0 }
    }

    fn rom_read(&self, offset: usize) -> u8 {
        if self.rom.is_empty() {
            0
        } else {
            self.rom[offset % self.rom.len()]
        }
    }

    /// Intercept the SA-1 CPU's reset / NMI / IRQ (+BRK/COP) vector fetches and
    /// return the SA-1's overridable vectors. `None` = not a vector fetch.
    ///
    /// bsnes gates this on a dedicated vector-fetch path so ordinary data reads
    /// of $00:FFEx return mapped ROM. `crate::cpu::Cpu`/`CpuBus` expose no
    /// vector-fetch signal (vectors are pulled via plain `bus.read`), so this is
    /// keyed on address alone: a SA-1 routine reading its own $00:FFEA-FFFF ROM
    /// bytes as data gets the register value instead. Low practical impact
    /// (`sa1.md` §3.7); adding a hook would require changing the shared CpuBus
    /// trait and every implementor.
    fn vector_byte(&self, addr: u32) -> Option<u8> {
        let (word, off) = match addr {
            0x00_FFEA | 0x00_FFFA => (self.st.cnv, addr & 1), // NMI (native / emu)
            0x00_FFEB | 0x00_FFFB => (self.st.cnv, addr & 1),
            0x00_FFEE | 0x00_FFFE => (self.st.civ, addr & 1), // IRQ/BRK (native / emu)
            0x00_FFEF | 0x00_FFFF => (self.st.civ, addr & 1),
            0x00_FFFC | 0x00_FFFD => (self.st.crv, addr & 1), // reset
            _ => return None,
        };
        Some(if off == 0 { word as u8 } else { (word >> 8) as u8 })
    }
}

impl CpuBus for Sa1Bus<'_> {
    fn read(&mut self, addr: u32) -> u8 {
        let addr = addr & 0xFF_FFFF;
        if let Some(b) = self.vector_byte(addr) {
            self.cycles += 2;
            return b;
        }
        let bank = (addr >> 16) & 0xFF;
        let off = (addr & 0xFFFF) as u16;
        match bank {
            0x00..=0x3F | 0x80..=0xBF => match off {
                0x0000..=0x07FF => {
                    self.cycles += 1;
                    self.st.iram[off as usize & 0x7FF]
                }
                0x2200..=0x23FF => {
                    self.cycles += 1;
                    self.st.read_io(self.rom, 0x2200 | (off & 0x1FF))
                }
                0x3000..=0x37FF => {
                    self.cycles += 1;
                    self.st.iram[off as usize & 0x7FF]
                }
                0x6000..=0x7FFF => {
                    self.cycles += 2;
                    self.bwram_window_read(off)
                }
                0x8000..=0xFFFF => {
                    self.cycles += 2;
                    match self.st.mmc.rom_offset(addr) {
                        Some(o) => self.rom_read(o),
                        None => 0,
                    }
                }
                _ => {
                    self.cycles += 1;
                    0
                }
            },
            0x40..=0x4F => {
                // BW-RAM linear.
                self.cycles += 2;
                let o = ((bank - 0x40) << 16 | off as u32) as usize;
                if self.st.bwram.is_empty() {
                    0
                } else {
                    self.st.bwram[o % self.st.bwram.len()]
                }
            }
            0x60..=0x6F => {
                // BW-RAM bitmap virtual memory.
                self.cycles += 2;
                let virt = ((bank - 0x60) << 16 | off as u32) as usize;
                mmc::bitmap_read(&self.st.bwram, virt, self.st.bbf & 0x80 == 0)
            }
            0xC0..=0xFF => {
                self.cycles += 2;
                match self.st.mmc.rom_offset(addr) {
                    Some(o) => self.rom_read(o),
                    None => 0,
                }
            }
            _ => {
                self.cycles += 1;
                0
            }
        }
    }

    fn write(&mut self, addr: u32, value: u8) {
        let addr = addr & 0xFF_FFFF;
        let bank = (addr >> 16) & 0xFF;
        let off = (addr & 0xFFFF) as u16;
        match bank {
            0x00..=0x3F | 0x80..=0xBF => match off {
                0x0000..=0x07FF => {
                    self.cycles += 1;
                    self.st.write_iram(false, off as usize, value);
                }
                0x2200..=0x23FF => {
                    self.cycles += 1;
                    self.st.write_io(self.rom, 0x2200 | (off & 0x1FF), value);
                }
                0x3000..=0x37FF => {
                    self.cycles += 1;
                    self.st.write_iram(false, off as usize, value);
                }
                0x6000..=0x7FFF => {
                    self.cycles += 2;
                    self.bwram_window_write(off, value);
                }
                _ => {
                    self.cycles += 1;
                }
            },
            0x40..=0x4F => {
                self.cycles += 2;
                let o = ((bank - 0x40) << 16 | off as u32) as usize;
                self.st.write_bwram(false, o, value);
            }
            0x60..=0x6F => {
                self.cycles += 2;
                let virt = ((bank - 0x60) << 16 | off as u32) as usize;
                if self.st.cbwe {
                    mmc::bitmap_write(&mut self.st.bwram, virt, self.st.bbf & 0x80 == 0, value);
                }
            }
            _ => {
                self.cycles += 1;
            }
        }
    }

    fn idle(&mut self) {
        self.cycles += 1;
    }

    fn take_nmi(&mut self) -> bool {
        let line = self.st.cfr_nmi && self.st.cie & 0x10 != 0;
        let edge = line && !self.st.nmi_prev;
        self.st.nmi_prev = line;
        edge
    }

    fn irq_level(&mut self) -> bool {
        (self.st.cfr_irq && self.st.cie & 0x80 != 0)
            || (self.st.cfr_timer && self.st.cie & 0x40 != 0)
            || (self.st.cfr_dma && self.st.cie & 0x20 != 0)
    }
}

impl Sa1Bus<'_> {
    fn bwram_window_read(&self, off: u16) -> u8 {
        if self.st.mmc.sa1_window_is_bitmap() {
            // Bitmap-source window: BMAP block indexes 8 KB of virtual pixels.
            let block = (self.st.mmc.bmap & 0x7F) as usize;
            let virt = block * 0x2000 + (off as usize & 0x1FFF);
            mmc::bitmap_read(&self.st.bwram, virt, self.st.bbf & 0x80 == 0)
        } else {
            let o = self.st.mmc.sa1_window_offset(off);
            if self.st.bwram.is_empty() {
                0
            } else {
                self.st.bwram[o % self.st.bwram.len()]
            }
        }
    }

    fn bwram_window_write(&mut self, off: u16, value: u8) {
        if self.st.mmc.sa1_window_is_bitmap() {
            if self.st.cbwe {
                let block = (self.st.mmc.bmap & 0x7F) as usize;
                let virt = block * 0x2000 + (off as usize & 0x1FFF);
                mmc::bitmap_write(&mut self.st.bwram, virt, self.st.bbf & 0x80 == 0, value);
            }
        } else {
            let o = self.st.mmc.sa1_window_offset(off);
            self.st.write_bwram(false, o, value);
        }
    }
}
