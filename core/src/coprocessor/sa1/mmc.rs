//! SA-1 Super MMC: ROM bank mapping and BW-RAM windowing / bitmap virtual memory.
//! `sa1.md` §2, §4, §7.

use serde::{Deserialize, Serialize};

/// One 1 MB ROM region binding (CXB/DXB/EXB/FXB).
#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct RomBank {
    /// 1 MB block index 0-7 (AAA, bits 0-2 of the register).
    pub block: u8,
    /// LoROM projection (bit7): true = LoROM maps the same `block`; false = LoROM
    /// maps a fixed default block (0/1/2/3 for CXB/DXB/EXB/FXB).
    pub mode: bool,
}

/// Super MMC state ($2220-$2225).
#[derive(Serialize, Deserialize)]
pub struct Mmc {
    /// CXB, DXB, EXB, FXB.
    pub banks: [RomBank; 4],
    /// BMAPS ($2224): S-CPU BW-RAM window block (`---BBBBB`, one of 32 8 KB blocks).
    pub bmaps: u8,
    /// BMAP ($2225): SA-1 BW-RAM window (`SBBBBBBB`).
    pub bmap: u8,
}

impl Default for Mmc {
    fn default() -> Self {
        // Power-on: identity-ish mapping (block N in region N), projection off.
        Mmc {
            banks: [
                RomBank { block: 0, mode: false },
                RomBank { block: 1, mode: false },
                RomBank { block: 2, mode: false },
                RomBank { block: 3, mode: false },
            ],
            bmaps: 0,
            bmap: 0,
        }
    }
}

const MB: usize = 0x10_0000;

impl Mmc {
    /// Write a CXB/DXB/EXB/FXB register (`region` 0-3). Value = `B----AAA`.
    pub fn write_bank(&mut self, region: usize, v: u8) {
        self.banks[region] = RomBank { block: v & 0x07, mode: v & 0x80 != 0 };
    }

    /// Translate a 24-bit S-CPU/SA-1 bus address to a linear ROM offset, or
    /// `None` if the address is not in a ROM region. Identical for both CPUs.
    ///
    /// HiROM banks $C0-$FF: `block*1MB + (bank&0x0F)*64KB + offset`.
    /// LoROM banks $00-$3F/$80-$BF at $8000-$FFFF: `block*1MB + (bank&0x1F)*32KB
    /// + (offset&0x7FFF)`, where `block` is the region's projection block when
    /// mode=1 else the region index (default 0/1/2/3).
    pub fn rom_offset(&self, addr: u32) -> Option<usize> {
        let bank = ((addr >> 16) & 0xFF) as u8;
        let off = (addr & 0xFFFF) as usize;
        match bank {
            // HiROM regions.
            0xC0..=0xFF => {
                let region = ((bank - 0xC0) >> 4) as usize; // 0..3
                let block = self.banks[region].block as usize;
                let sub = (bank & 0x0F) as usize;
                Some(block * MB + sub * 0x10000 + off)
            }
            // LoROM regions ($8000-$FFFF only).
            _ if off >= 0x8000 => {
                let region = match bank {
                    0x00..=0x1F => 0, // CXB
                    0x20..=0x3F => 1, // DXB
                    0x80..=0x9F => 2, // EXB
                    0xA0..=0xBF => 3, // FXB
                    _ => return None,
                };
                let b = self.banks[region];
                let block = if b.mode { b.block as usize } else { region };
                let within = (bank & 0x1F) as usize;
                Some(block * MB + within * 0x8000 + (off & 0x7FFF))
            }
            _ => None,
        }
    }

    /// S-CPU BW-RAM window ($6000-$7FFF) -> linear BW-RAM offset. BMAPS selects
    /// one of 32 8 KB blocks.
    pub fn scpu_window_offset(&self, addr: u16) -> usize {
        let block = (self.bmaps & 0x1F) as usize;
        block * 0x2000 + (addr as usize & 0x1FFF)
    }

    /// SA-1 BW-RAM window ($6000-$7FFF), linear-source path -> BW-RAM offset.
    /// (Bitmap source, BMAP bit7=1, is handled by `bitmap` access instead.)
    pub fn sa1_window_offset(&self, addr: u16) -> usize {
        let block = (self.bmap & 0x7F) as usize;
        block * 0x2000 + (addr as usize & 0x1FFF)
    }

    /// True when the SA-1 $6000-$7FFF window is in bitmap-source mode (BMAP bit7).
    pub fn sa1_window_is_bitmap(&self) -> bool {
        self.bmap & 0x80 != 0
    }
}

/// Read one pixel from BW-RAM bitmap virtual memory (banks $60-$6F). `virt` is
/// the virtual byte index; `four_bpp` selects 16-color (4bpp, 2 px/byte) vs
/// 4-color (2bpp, 4 px/byte). `sa1.md` §7 / BBF $223F. Returns the pixel value
/// right-justified. Pixel 0 occupies the least-significant field of each byte.
pub fn bitmap_read(bwram: &[u8], virt: usize, four_bpp: bool) -> u8 {
    if bwram.is_empty() {
        return 0;
    }
    if four_bpp {
        let byte = bwram[(virt >> 1) % bwram.len()];
        let shift = (virt & 1) * 4;
        (byte >> shift) & 0x0F
    } else {
        let byte = bwram[(virt >> 2) % bwram.len()];
        let shift = (virt & 3) * 2;
        (byte >> shift) & 0x03
    }
}

/// Write one pixel into BW-RAM bitmap virtual memory (banks $60-$6F).
pub fn bitmap_write(bwram: &mut [u8], virt: usize, four_bpp: bool, pixel: u8) {
    if bwram.is_empty() {
        return;
    }
    if four_bpp {
        let idx = (virt >> 1) % bwram.len();
        let shift = (virt & 1) * 4;
        bwram[idx] = (bwram[idx] & !(0x0F << shift)) | ((pixel & 0x0F) << shift);
    } else {
        let idx = (virt >> 2) % bwram.len();
        let shift = (virt & 3) * 2;
        bwram[idx] = (bwram[idx] & !(0x03 << shift)) | ((pixel & 0x03) << shift);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hirom_bank_mapping() {
        let mut mmc = Mmc::default();
        mmc.write_bank(0, 0x82); // CXB: block=2, projection on
        // $C0:1234 -> block2, sub0.
        assert_eq!(mmc.rom_offset(0xC0_1234), Some(2 * MB + 0x1234));
        // $C5:0000 -> block2, sub5.
        assert_eq!(mmc.rom_offset(0xC5_0000), Some(2 * MB + 5 * 0x10000));
        // Region 1 (DXB) untouched: default block 1 for $D0.
        assert_eq!(mmc.rom_offset(0xD0_0000), Some(1 * MB));
        // Region 3 (FXB) $F3:8000 default block 3.
        assert_eq!(mmc.rom_offset(0xF3_8000), Some(3 * MB + 3 * 0x10000 + 0x8000));
    }

    #[test]
    fn lorom_projection_and_default() {
        let mut mmc = Mmc::default();
        // CXB projection on, block 4: LoROM $00-$1F maps block 4.
        mmc.write_bank(0, 0x84);
        assert_eq!(mmc.rom_offset(0x00_8000), Some(4 * MB));
        assert_eq!(mmc.rom_offset(0x01_8000), Some(4 * MB + 0x8000));
        assert_eq!(mmc.rom_offset(0x1F_FFFF), Some(4 * MB + 0x1F * 0x8000 + 0x7FFF));
        // Below $8000 is not ROM in a LoROM bank.
        assert_eq!(mmc.rom_offset(0x00_1000), None);

        // CXB projection off: LoROM defaults to block 0.
        mmc.write_bank(0, 0x04);
        assert_eq!(mmc.rom_offset(0x00_8000), Some(0));

        // DXB region $20-$3F default block 1 when projection off.
        mmc.write_bank(1, 0x01);
        assert_eq!(mmc.rom_offset(0x20_8000), Some(1 * MB));
        // EXB $80-$9F default block 2.
        assert_eq!(mmc.rom_offset(0x80_8000), Some(2 * MB));
        // FXB $A0-$BF default block 3.
        assert_eq!(mmc.rom_offset(0xA0_8000), Some(3 * MB));
    }

    #[test]
    fn window_offsets() {
        let mut mmc = Mmc::default();
        mmc.bmaps = 5;
        assert_eq!(mmc.scpu_window_offset(0x6000), 5 * 0x2000);
        assert_eq!(mmc.scpu_window_offset(0x7001), 5 * 0x2000 + 0x1001);
        mmc.bmap = 0x03;
        assert!(!mmc.sa1_window_is_bitmap());
        assert_eq!(mmc.sa1_window_offset(0x6000), 3 * 0x2000);
        mmc.bmap = 0x83;
        assert!(mmc.sa1_window_is_bitmap());
    }

    #[test]
    fn bitmap_4bpp_roundtrip() {
        let mut bw = vec![0u8; 16];
        bitmap_write(&mut bw, 0, true, 0x0A);
        bitmap_write(&mut bw, 1, true, 0x05);
        // Two virtual pixels packed into byte 0: low nibble px0, high nibble px1.
        assert_eq!(bw[0], 0x5A);
        assert_eq!(bitmap_read(&bw, 0, true), 0x0A);
        assert_eq!(bitmap_read(&bw, 1, true), 0x05);
    }

    #[test]
    fn bitmap_2bpp_roundtrip() {
        let mut bw = vec![0u8; 16];
        bitmap_write(&mut bw, 0, false, 0b11);
        bitmap_write(&mut bw, 1, false, 0b10);
        bitmap_write(&mut bw, 2, false, 0b01);
        bitmap_write(&mut bw, 3, false, 0b00);
        // 4 pixels in byte 0: px3 px2 px1 px0 = 00 01 10 11 = 0b00011011.
        assert_eq!(bw[0], 0b0001_1011);
        assert_eq!(bitmap_read(&bw, 0, false), 0b11);
        assert_eq!(bitmap_read(&bw, 2, false), 0b01);
    }
}
