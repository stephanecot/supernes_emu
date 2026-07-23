//! LoROM / HiROM address decode.
//!
//! LoROM: 32KB ROM banks. Banks $00-$7D/$80-$FF at $8000-$FFFF map linearly
//! (offset = bank*32K + addr-$8000); banks $40-$6F also expose the same data
//! in their lower half. SRAM: banks $70-$7D/$F0-$FF at $0000-$7FFF.
//!
//! HiROM: 64KB ROM banks. Banks $C0-$FF (and $40-$7D) map the full 64KB
//! (offset = (bank & $3F)*64K + addr); banks $00-$3F/$80-$BF expose the upper
//! half at $8000-$FFFF at the same offsets. SRAM: banks $20-$3F/$A0-$BF at
//! $6000-$7FFF, 8KB per bank window.
//!
//! Non-power-of-two ROMs mirror per the standard cascade (bsnes algorithm):
//! the image splits into power-of-two blocks, each repeating to fill its slot.

use super::sram::Sram;

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Mapping {
    LoRom,
    HiRom,
}

/// Mirror `addr` into a ROM of `size` bytes (size need not be a power of two).
pub fn mirror(mut addr: usize, mut size: usize) -> usize {
    if size == 0 {
        return 0;
    }
    let mut base = 0usize;
    let mut mask = 1usize << (usize::BITS - 1);
    while addr >= size {
        while addr & mask == 0 {
            mask >>= 1;
        }
        addr -= mask;
        if size > mask {
            size -= mask;
            base += mask;
        }
        mask >>= 1;
    }
    base + addr
}

/// ROM byte offset for a 24-bit bus address, or `None` if the address does not
/// decode to ROM under this mapping. Only called for cartridge-owned regions
/// (the bus intercepts WRAM/MMIO first).
pub fn rom_offset(mapping: Mapping, addr: u32) -> Option<usize> {
    let bank = ((addr >> 16) & 0xFF) as usize & 0x7F; // $80-$FF mirrors $00-$7F
    let off = (addr & 0xFFFF) as usize;
    match mapping {
        Mapping::LoRom => {
            if off >= 0x8000 {
                Some(bank * 0x8000 + (off - 0x8000))
            } else if (0x40..0x70).contains(&bank) {
                // Lower halves of banks $40-$6F mirror the same 32KB ROM bank.
                Some(bank * 0x8000 + (off & 0x7FFF))
            } else {
                None
            }
        }
        Mapping::HiRom => {
            if bank >= 0x40 || off >= 0x8000 {
                Some((bank & 0x3F) * 0x10000 + off)
            } else {
                None
            }
        }
    }
}

/// SRAM byte offset for a bus address, or `None` if outside the SRAM window.
pub fn sram_offset(mapping: Mapping, addr: u32) -> Option<usize> {
    let bank = ((addr >> 16) & 0xFF) as usize & 0x7F;
    let off = (addr & 0xFFFF) as usize;
    match mapping {
        Mapping::LoRom => {
            // Banks $70-$7D ($F0-$FD mirrored) lower half. $7E/$7F are WRAM on
            // the bus; only $FE/$FF reach here masked to $7E/$7F — exclude them.
            if (0x70..0x7E).contains(&bank) && off < 0x8000 {
                Some((bank - 0x70) * 0x8000 + off)
            } else {
                None
            }
        }
        Mapping::HiRom => {
            if (0x20..0x40).contains(&bank) && (0x6000..0x8000).contains(&off) {
                Some((bank - 0x20) * 0x2000 + (off - 0x6000))
            } else {
                None
            }
        }
    }
}

/// SuperFX / GSU2 SNES-side Game Pak ROM offset (pre-mirror), or `None` if the
/// address is not in a ROM window (superfx.md §4). GSU carts are Slow-ROM only:
/// the LoROM window ($00-3F:8000-FFFF) and the linear HiROM window
/// ($40-5F:0000-FFFF) address the same image ($40:0000 == $00:8000); the fast
/// banks $80-BF are unused and read as open bus.
pub fn superfx_rom_offset(addr: u32) -> Option<usize> {
    let bank = ((addr >> 16) & 0xFF) as usize;
    let off = (addr & 0xFFFF) as usize;
    match bank {
        0x00..=0x3F if off >= 0x8000 => Some(bank * 0x8000 + (off - 0x8000)),
        0x40..=0x5F => Some((bank - 0x40) * 0x10000 + off),
        _ => None,
    }
}

/// SuperFX / GSU2 SNES-side Game Pak RAM offset, or `None` if the address is
/// not in a RAM window (superfx.md §4). Banks $70-$71 map the RAM linearly;
/// $00-3F/$80-BF:6000-7FFF mirror the first 8 KB ($70:0000-1FFF).
pub fn superfx_ram_offset(addr: u32) -> Option<usize> {
    let bank = ((addr >> 16) & 0xFF) as usize;
    let off = (addr & 0xFFFF) as usize;
    match bank {
        0x00..=0x3F | 0x80..=0xBF if (0x6000..=0x7FFF).contains(&off) => {
            Some(off - 0x6000)
        }
        0x70..=0x71 => Some((bank - 0x70) * 0x10000 + off),
        _ => None,
    }
}

pub fn read(mapping: Mapping, rom: &[u8], sram: &Sram, addr: u32) -> Option<u8> {
    if let Some(off) = sram_offset(mapping, addr) {
        if !sram.is_empty() {
            return sram.get(off);
        }
        // No SRAM chip: LoROM banks $70+ fall through to open bus; HiROM
        // $6000-$7FFF likewise.
        if mapping == Mapping::HiRom {
            return None;
        }
    }
    rom_offset(mapping, addr).map(|off| rom[mirror(off, rom.len())])
}

pub fn write(mapping: Mapping, sram: &mut Sram, addr: u32, value: u8) {
    if let Some(off) = sram_offset(mapping, addr) {
        sram.set(off, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear_rom(len: usize) -> Vec<u8> {
        // Byte value = low 8 bits of a mixed offset hash, so mirrors are detectable.
        (0..len).map(|i| ((i ^ (i >> 8) ^ (i >> 16)) & 0xFF) as u8).collect()
    }

    #[test]
    fn mirror_power_of_two() {
        assert_eq!(mirror(0x0000, 0x20000), 0x0000);
        assert_eq!(mirror(0x20000, 0x20000), 0x0000);
        assert_eq!(mirror(0x25555, 0x20000), 0x05555);
    }

    #[test]
    fn mirror_non_power_of_two() {
        // 0x30000 = 128KB + 64KB. The 64KB tail repeats to fill 128K..256K.
        assert_eq!(mirror(0x05555, 0x30000), 0x05555);
        assert_eq!(mirror(0x30000, 0x30000), 0x20000);
        assert_eq!(mirror(0x38000, 0x30000), 0x28000);
        // Beyond 256K the whole pattern repeats.
        assert_eq!(mirror(0x40000, 0x30000), 0x00000);
        assert_eq!(mirror(0x70000, 0x30000), 0x20000);
    }

    #[test]
    fn lorom_decode() {
        // $00:8000 -> offset 0
        assert_eq!(rom_offset(Mapping::LoRom, 0x00_8000), Some(0));
        // $00:FFFF -> 0x7FFF
        assert_eq!(rom_offset(Mapping::LoRom, 0x00_FFFF), Some(0x7FFF));
        // $01:8000 -> 0x8000
        assert_eq!(rom_offset(Mapping::LoRom, 0x01_8000), Some(0x8000));
        // $80:8000 mirrors $00:8000
        assert_eq!(rom_offset(Mapping::LoRom, 0x80_8000), Some(0));
        assert_eq!(
            rom_offset(Mapping::LoRom, 0x80_8000),
            rom_offset(Mapping::LoRom, 0x00_8000)
        );
        // Bank $40 lower half mirrors its upper half.
        assert_eq!(
            rom_offset(Mapping::LoRom, 0x40_0000),
            rom_offset(Mapping::LoRom, 0x40_8000)
        );
        // Lower halves of banks $00-$3F are system space, not ROM.
        assert_eq!(rom_offset(Mapping::LoRom, 0x00_0000), None);
        assert_eq!(rom_offset(Mapping::LoRom, 0x20_6000), None);
    }

    #[test]
    fn lorom_sram_window() {
        let mut sram = Sram::new(0x2000);
        sram.set(0x123, 0x5A);
        let rom = linear_rom(0x8000);
        // $70:0123 reads SRAM.
        assert_eq!(read(Mapping::LoRom, &rom, &sram, 0x70_0123), Some(0x5A));
        // $F0:0123 mirrors it.
        assert_eq!(read(Mapping::LoRom, &rom, &sram, 0xF0_0123), Some(0x5A));
        // 8KB SRAM mirrors within the 32KB window.
        assert_eq!(read(Mapping::LoRom, &rom, &sram, 0x70_2123), Some(0x5A));
        // Writes land in SRAM.
        let mut sram2 = Sram::new(0x2000);
        write(Mapping::LoRom, &mut sram2, 0x71_0042, 0xA5);
        assert_eq!(sram2.get(0x8000 + 0x42), Some(0xA5)); // bank $71 -> second 32KB page (mirrored into 8KB)
    }

    #[test]
    fn hirom_decode() {
        // $C0:0000 -> offset 0
        assert_eq!(rom_offset(Mapping::HiRom, 0xC0_0000), Some(0));
        // $C1:1234 -> 0x11234
        assert_eq!(rom_offset(Mapping::HiRom, 0xC1_1234), Some(0x11234));
        // $00:8000 -> 0x8000 (upper half of bank 0)
        assert_eq!(rom_offset(Mapping::HiRom, 0x00_8000), Some(0x8000));
        // $00:FFFF -> 0xFFFF
        assert_eq!(rom_offset(Mapping::HiRom, 0x00_FFFF), Some(0xFFFF));
        // $40:0000 -> 0x00000 (banks $40-$7D mirror $C0-$FD)
        assert_eq!(rom_offset(Mapping::HiRom, 0x40_0000), Some(0));
        // $80:8000 mirrors $00:8000
        assert_eq!(
            rom_offset(Mapping::HiRom, 0x80_8000),
            rom_offset(Mapping::HiRom, 0x00_8000)
        );
        // Lower halves of banks $00-$3F outside SRAM window are not ROM.
        assert_eq!(rom_offset(Mapping::HiRom, 0x00_0000), None);
    }

    #[test]
    fn hirom_rom_mirroring_2mb() {
        let rom = linear_rom(0x200000); // 2 MB
        // $C0:0000 vs $E0:0000: bank & 0x3F wraps 0x20 -> offset 0x200000 -> mirror -> 0
        let a = read(Mapping::HiRom, &rom, &Sram::new(0), 0xC0_0000);
        let b = read(Mapping::HiRom, &rom, &Sram::new(0), 0xE0_0000);
        assert_eq!(a, b);
        assert_eq!(a, Some(rom[0]));
    }

    #[test]
    fn hirom_sram_window() {
        let mut sram = Sram::new(0x2000);
        write(Mapping::HiRom, &mut sram, 0x20_6000, 0x77);
        assert_eq!(sram.get(0), Some(0x77));
        let rom = linear_rom(0x10000);
        assert_eq!(read(Mapping::HiRom, &rom, &sram, 0x20_6000), Some(0x77));
        // Mirror at $A0:6000.
        assert_eq!(read(Mapping::HiRom, &rom, &sram, 0xA0_6000), Some(0x77));
        // Outside the window: $20:5FFF is open bus (None).
        assert_eq!(read(Mapping::HiRom, &rom, &sram, 0x20_5FFF), None);
    }

    #[test]
    fn lorom_mirroring_160kb_analog() {
        // 160KB = 128KB + 32KB, same shape as SMAS+SMW's 2.5MB.
        let rom = linear_rom(0x28000);
        let sram = Sram::new(0);
        // Offset 0x28000 (bank $05:8000) mirrors into the 32KB tail block.
        let direct = read(Mapping::LoRom, &rom, &sram, 0x05_8000);
        assert_eq!(direct, Some(rom[0x20000])); // tail repeats: 0x28000 -> 0x20000
        // Way past the end wraps to the start.
        let wrapped = read(Mapping::LoRom, &rom, &sram, 0x10_8000); // offset 0x80000
        assert_eq!(wrapped, Some(rom[0]));
    }
}
