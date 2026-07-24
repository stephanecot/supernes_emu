//! SA-1 arithmetic unit and variable-length bit reader.
//!
//! Behaviour transcribed from `sa1.md` §5, itself verified against bsnes
//! `sfc/coprocessor/sa1/io.cpp`. All results are computed instantly (no cycle
//! latency), matching bsnes; the ~5/6-cycle hardware latency is immaterial.

use super::mmc::Mmc;
use serde::{Deserialize, Serialize};

/// Multiply / signed-divide / cumulative-multiply-accumulate unit.
/// `$2250 MCNT` selects the mode; the operation runs when `$2254 MBH` is written.
#[derive(Serialize, Deserialize, Default)]
pub struct ArithUnit {
    /// Multiplicand / dividend (signed 16-bit).
    pub ma: u16,
    /// Multiplier / divisor (signed for multiply/sum, unsigned for divide).
    pub mb: u16,
    /// 40-bit result accumulator (MR1-MR5, $2306-$230A).
    pub mr: u64,
    /// Overflow flag ($230B); only the cumulative-sum path writes it.
    pub of: bool,
    /// MCNT bit0 `md`: divide-select (meaningful only when `acm`=0).
    pub md: bool,
    /// MCNT bit1 `acm`: cumulative multiply-accumulate mode.
    pub acm: bool,
}

impl ArithUnit {
    /// Write `$2250 MCNT`. Setting `acm` (bit1) zeroes the 40-bit accumulator MR
    /// (fresh-sum start); OF is left untouched (bsnes: only `mr` is cleared).
    pub fn write_mcnt(&mut self, v: u8) {
        self.md = v & 0x01 != 0;
        self.acm = v & 0x02 != 0;
        if self.acm {
            self.mr = 0;
        }
    }

    /// Run the selected operation. Called when `$2254 MBH` is written.
    pub fn execute(&mut self) {
        if self.acm {
            // Cumulative multiply-accumulate: MR += (s16)MA * (s16)MB.
            let prod = (self.ma as i16 as i64) * (self.mb as i16 as i64);
            let sum = self.mr.wrapping_add(prod as u64);
            self.of = (sum >> 40) & 1 != 0;
            self.mr = sum & 0xFF_FFFF_FFFF;
            self.mb = 0;
        } else if self.md {
            // Signed dividend / unsigned divisor, rounds toward -inf.
            // Quotient -> MR1-MR2 (low 16), remainder -> MR3-MR4 (high 16).
            let dividend = self.ma as i16 as i32;
            let divisor = self.mb;
            if divisor == 0 {
                // Div-by-zero: MR = 0, OF untouched (no div-by-0 overflow flag).
                self.mr = 0;
            } else {
                let d = divisor as i32;
                let quot = dividend.div_euclid(d);
                let rem = dividend.rem_euclid(d);
                self.mr = (((rem as u32) & 0xFFFF) as u64) << 16 | ((quot as u32) & 0xFFFF) as u64;
            }
            self.ma = 0;
            self.mb = 0;
        } else {
            // Signed 16x16 multiply -> 32-bit product in MR1-MR4.
            let prod = (self.ma as i16 as i32) * (self.mb as i16 as i32);
            self.mr = prod as u32 as u64;
            self.mb = 0;
        }
    }

    /// Read MR byte `i` (0 = MR1 $2306 .. 4 = MR5 $230A).
    pub fn mr_byte(&self, i: u32) -> u8 {
        (self.mr >> (i * 8)) as u8
    }
}

/// Variable-length bit reader ($2258-$225B -> $230C-$230D). Reads an arbitrary
/// MSB-first bit stream from ROM. `sa1.md` §5.
#[derive(Serialize, Deserialize, Default)]
pub struct BitReader {
    /// 24-bit ROM byte address of the stream head.
    pub va: u32,
    /// Bit offset (0-7, MSB-first) into the byte at `va`.
    pub vbit: u32,
    /// Field length in bits (1-16). VBD.VVVV=0 means 16.
    pub length: u32,
    /// VBD.H: auto-increment the bit pointer after reading VDP high byte.
    pub auto: bool,
}

impl BitReader {
    /// Decode `$2258 VBD` (`H---VVVV`).
    pub fn write_vbd(&mut self, v: u8) {
        self.auto = v & 0x80 != 0;
        let n = (v & 0x0F) as u32;
        self.length = if n == 0 { 16 } else { n };
    }

    /// Return the next `length` bits right-justified in a 16-bit window,
    /// without advancing the pointer. Reads up to 3 stream bytes.
    ///
    /// `va` ($2259-$225B) is a 24-bit CPU bus address, not a linear ROM offset:
    /// each stream byte is translated through the Super MMC (bsnes `readVBR`),
    /// so e.g. `$00:8000` selects the CXB-mapped ROM block, not `rom[0x8000]`.
    pub fn peek(&self, rom: &[u8], mmc: &Mmc) -> u16 {
        let b0 = rom_byte(rom, mmc, self.va) as u32;
        let b1 = rom_byte(rom, mmc, self.va.wrapping_add(1) & 0xFF_FFFF) as u32;
        let b2 = rom_byte(rom, mmc, self.va.wrapping_add(2) & 0xFF_FFFF) as u32;
        let window = (b0 << 16) | (b1 << 8) | b2;
        // vbit (0-7) + length (1-16) <= 23, so a 24-bit window always suffices.
        let shift = 24 - self.vbit - self.length;
        let mask = if self.length >= 16 { 0xFFFF } else { (1u32 << self.length) - 1 };
        ((window >> shift) & mask) as u16
    }

    /// Advance the bit pointer by `length` bits (auto-increment mode).
    pub fn advance(&mut self) {
        let total = self.vbit + self.length;
        self.va = (self.va + (total >> 3)) & 0xFF_FFFF;
        self.vbit = total & 7;
    }
}

/// Fetch a ROM byte at CPU bus address `addr`, mapped through the Super MMC.
/// Non-ROM regions (e.g. a LoROM bank below $8000) return 0.
fn rom_byte(rom: &[u8], mmc: &Mmc, addr: u32) -> u8 {
    match mmc.rom_offset(addr) {
        Some(o) => rom.get(o).copied().unwrap_or(0),
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiply_signed() {
        let mut a = ArithUnit::default();
        // 32767 * 32767 = 0x3FFF0001.
        a.write_mcnt(0x00);
        a.ma = 0x7FFF;
        a.mb = 0x7FFF;
        a.execute();
        assert_eq!(a.mr as u32, 0x3FFF_0001);
        assert_eq!(a.mb, 0, "MB cleared after multiply");

        // -1 * 2 = -2 = 0xFFFFFFFE.
        a.write_mcnt(0x00);
        a.ma = 0xFFFF;
        a.mb = 0x0002;
        a.execute();
        assert_eq!(a.mr as u32, 0xFFFF_FFFE);

        // -32768 * -32768 = 0x40000000.
        a.write_mcnt(0x00);
        a.ma = 0x8000;
        a.mb = 0x8000;
        a.execute();
        assert_eq!(a.mr as u32, 0x4000_0000);
    }

    #[test]
    fn divide_signed_unsigned() {
        let mut a = ArithUnit::default();
        // 100 / 7 = 14 rem 2. Quotient in low16, remainder in high16.
        a.write_mcnt(0x01);
        a.ma = 100;
        a.mb = 7;
        a.execute();
        assert_eq!(a.mr & 0xFFFF, 14);
        assert_eq!((a.mr >> 16) & 0xFFFF, 2);
        assert_eq!(a.ma, 0);
        assert_eq!(a.mb, 0);

        // Negative dividend rounds toward -inf: -100 / 7 = -15 rem 5.
        a.write_mcnt(0x01);
        a.ma = (-100i16) as u16;
        a.mb = 7;
        a.execute();
        assert_eq!(a.mr & 0xFFFF, (-15i16 as u16) as u64);
        assert_eq!((a.mr >> 16) & 0xFFFF, 5);
    }

    #[test]
    fn divide_by_zero() {
        let mut a = ArithUnit::default();
        a.of = true; // must stay untouched
        a.write_mcnt(0x01);
        a.ma = 1234;
        a.mb = 0;
        a.execute();
        assert_eq!(a.mr, 0);
        assert!(a.of, "divide-by-zero must not touch OF");
    }

    #[test]
    fn cumulative_sum_and_overflow() {
        let mut a = ArithUnit::default();
        a.write_mcnt(0x02); // acm=1: reset accumulator
        assert_eq!(a.mr, 0);
        a.ma = 0x7FFF;
        a.mb = 0x7FFF;
        a.execute(); // += 0x3FFF0001
        assert_eq!(a.mr, 0x3FFF_0001);
        assert!(!a.of);
        a.ma = 0x7FFF;
        a.mb = 0x7FFF;
        a.execute(); // += 0x3FFF0001 -> 0x7FFE0002
        assert_eq!(a.mr, 0x7FFE_0002);
        assert!(!a.of);

        // Force a 40-bit overflow with a preloaded near-max accumulator.
        a.mr = 0xFF_FFFF_FFFF;
        a.ma = 0x7FFF;
        a.mb = 0x0002; // += 0xFFFE
        a.execute();
        assert!(a.of, "bit40 carry sets OF");
        assert_eq!(a.mr, (0xFF_FFFF_FFFFu64 + 0xFFFE) & 0xFF_FFFF_FFFF);
    }

    #[test]
    fn bit_reader_msb_first() {
        // Stream at ROM offset 0, addressed as LoROM $00:8000 with the default MMC.
        // Stream: 0b1011_0010 0b1100_1111 ...
        let rom = [0xB2, 0xCF, 0x00];
        let mmc = Mmc::default();
        let mut r = BitReader::default();
        r.write_vbd(0x84); // auto, length 4
        r.va = 0x00_8000;
        r.vbit = 0;
        // First 4 bits MSB-first: 1011 = 0xB.
        assert_eq!(r.peek(&rom, &mmc), 0xB);
        r.advance();
        assert_eq!(r.vbit, 4);
        // Next 4 bits: 0010 = 0x2.
        assert_eq!(r.peek(&rom, &mmc), 0x2);
        r.advance();
        assert_eq!(r.vbit, 0);
        assert_eq!(r.va, 0x00_8001);
        // Next 4 bits from 0xCF: 1100 = 0xC.
        assert_eq!(r.peek(&rom, &mmc), 0xC);
    }

    #[test]
    fn bit_reader_crosses_byte() {
        // Read 12 bits starting at bit offset 4 of 0xB2 0xCF 0x12.
        let rom = [0xB2, 0xCF, 0x12];
        let mmc = Mmc::default();
        let mut r = BitReader::default();
        r.write_vbd(0x0C); // length 12, no auto
        r.va = 0x00_8000;
        r.vbit = 4;
        // bits 4..16 = 0010 1100 1111 = 0x2CF.
        assert_eq!(r.peek(&rom, &mmc), 0x2CF);
    }

    #[test]
    fn bit_reader_length_zero_means_16() {
        let mmc = Mmc::default();
        let mut r = BitReader::default();
        r.write_vbd(0x00);
        assert_eq!(r.length, 16);
        let rom = [0xAB, 0xCD, 0x00];
        r.va = 0x00_8000;
        r.vbit = 0;
        assert_eq!(r.peek(&rom, &mmc), 0xABCD);
    }

    #[test]
    fn bit_reader_maps_through_mmc() {
        // VDA as a raw offset would read rom[0]; as a bus address it must map
        // the HiROM region $C0:0000 -> ROM offset 0 through the Super MMC.
        let rom = [0x5A, 0x00, 0x00];
        let mmc = Mmc::default();
        let mut r = BitReader::default();
        r.write_vbd(0x08); // length 8
        r.va = 0xC0_0000;
        r.vbit = 0;
        assert_eq!(r.peek(&rom, &mmc), 0x5A);
        // A non-ROM address (LoROM bank below $8000) reads as 0.
        r.va = 0x00_0000;
        assert_eq!(r.peek(&rom, &mmc), 0x00);
    }
}
