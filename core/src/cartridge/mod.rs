//! Cartridge loader: copier-header strip, LoROM/HiROM header scoring,
//! region and SRAM-size decode.
//!
//! Header layout (base = $7FC0 LoROM, $FFC0 HiROM):
//!   +$00..+$14  title, 21 bytes, space-padded ASCII (JIS X 0201)
//!   +$15        map mode: $20/$30 LoROM, $21/$31 HiROM (bit4 = FastROM)
//!   +$16        cartridge type
//!   +$17        ROM size (1 << n KB)
//!   +$18        SRAM size (1 << n KB, 0 = none)
//!   +$19        country code: 0,1,13 => NTSC; 2..=12 => PAL
//!   +$1C..+$1D  checksum complement (little endian)
//!   +$1E..+$1F  checksum (little endian); checksum + complement == $FFFF

pub mod mapping;
pub mod sram;

pub use mapping::Mapping;

use crate::coprocessor::superfx::{SuperFx, VCR_GSU2};
use crate::scheduler::Region;
use sram::Sram;

const LOROM_HEADER: usize = 0x7FC0;
const HIROM_HEADER: usize = 0xFFC0;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Cartridge {
    /// The ROM image is NOT part of a save state (large, and reloaded from the
    /// game file); `Snes::load_state` reattaches the currently-loaded ROM after
    /// deserializing, so this restores as an empty vec.
    #[serde(skip)]
    pub rom: Vec<u8>,
    pub sram: Sram,
    pub mapping: Mapping,
    pub region: Region,
    pub title: String,
    /// Header map-mode byte bit4: cartridge supports 3.58 MHz FastROM access.
    pub fastrom: bool,
    /// Checksum stored in the header.
    pub header_checksum: u16,
    /// True if the checksum computed over the ROM matches the header.
    pub checksum_valid: bool,
    /// SuperFX / GSU coprocessor unit (with its own Game Pak RAM), present only
    /// when the header declares a GSU chipset. `None` for plain LoROM/HiROM
    /// carts, which take the exact original mapping path.
    #[serde(default)]
    pub superfx: Option<SuperFx>,
}

impl Cartridge {
    pub fn from_bytes(mut bytes: Vec<u8>) -> Result<Cartridge, String> {
        if bytes.len() < 0x8000 {
            return Err(format!("ROM too small: {} bytes", bytes.len()));
        }
        // 512-byte copier header (SWC/SMC): present iff size mod 32KB == 512.
        if bytes.len() % 0x8000 == 512 {
            bytes.drain(..512);
        }
        let rom = bytes;

        let lo_score = score_header(&rom, LOROM_HEADER, Mapping::LoRom);
        let hi_score = score_header(&rom, HIROM_HEADER, Mapping::HiRom);
        // Tie goes to LoROM (far more common).
        let (mapping, base) = if hi_score > lo_score {
            (Mapping::HiRom, HIROM_HEADER)
        } else {
            (Mapping::LoRom, LOROM_HEADER)
        };
        if lo_score <= 0 && hi_score <= 0 {
            return Err(format!(
                "no plausible SNES header found (LoROM score {lo_score}, HiROM score {hi_score})"
            ));
        }

        let title = rom[base..base + 21]
            .iter()
            .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { ' ' })
            .collect::<String>()
            .trim_end()
            .to_string();
        let map_mode = rom[base + 0x15];
        let fastrom = map_mode & 0x10 != 0;
        let region = decode_region(rom[base + 0x19]);
        let sram_size = decode_sram_size(rom[base + 0x18]);
        let header_checksum = u16::from_le_bytes([rom[base + 0x1E], rom[base + 0x1F]]);
        let checksum_valid = compute_checksum(&rom) == header_checksum;

        // Chipset byte $16: high nibble $1 = GSU/SuperFX coprocessor, with a
        // coprocessor-present low nibble ($3+); GSU declares LoROM (superfx.md
        // §10). No VCR value is defined for GSU1, and the only test cart is a
        // GSU2, so the version register always reports GSU2.
        let chipset = rom[base + 0x16];
        let superfx = if mapping == Mapping::LoRom
            && chipset & 0xF0 == 0x10
            && chipset & 0x0F >= 0x03
        {
            Some(SuperFx::new(superfx_ram_size(&rom, base), VCR_GSU2))
        } else {
            None
        };

        Ok(Cartridge {
            rom,
            sram: Sram::new(sram_size),
            mapping,
            region,
            title,
            fastrom,
            header_checksum,
            checksum_valid,
            superfx,
        })
    }

    /// SuperFX SNES-side read (open bus = `None`) honoring the GSU/SNES bus
    /// lockout: while the GSU runs and owns a resource (SCMR RON/RAN), SNES
    /// reads of Game Pak ROM/RAM return open bus; ROM additionally exposes the
    /// fixed exception-vector bytes keyed by the address low nibble (superfx.md
    /// §4). Only called when `superfx` is `Some`.
    fn superfx_read(&self, addr: u32) -> Option<u8> {
        let fx = self.superfx.as_ref().unwrap();
        if let Some(off) = mapping::superfx_ram_offset(addr) {
            if fx.snes_ram_blocked() {
                return None;
            }
            return Some(fx.ram_byte_abs(off));
        }
        if let Some(off) = mapping::superfx_rom_offset(addr) {
            if fx.snes_rom_blocked() {
                return fx.rom_vector_override((addr & 0xFFFF) as u16);
            }
            if self.rom.is_empty() {
                return None;
            }
            return Some(self.rom[mapping::mirror(off, self.rom.len())]);
        }
        None
    }

    /// SuperFX SNES-side write. Game Pak RAM writes are dropped while the GSU
    /// owns RAM (RAN); ROM writes are ignored. Only called when `superfx` is
    /// `Some`.
    fn superfx_write(&mut self, addr: u32, value: u8) {
        if let Some(off) = mapping::superfx_ram_offset(addr) {
            let fx = self.superfx.as_mut().unwrap();
            if !fx.snes_ram_blocked() {
                fx.ram_set_abs(off, value);
            }
        }
    }

    /// Run the GSU for `budget` GSU clocks against the borrowed ROM image.
    /// No-op for non-SuperFX carts or when the GSU is halted.
    pub fn superfx_run(&mut self, budget: i64) {
        if let Some(fx) = self.superfx.as_mut() {
            fx.run(&self.rom, budget);
        }
    }

    /// Bus read into cartridge space. `None` = unmapped (open bus).
    pub fn read(&self, addr: u32) -> Option<u8> {
        if self.superfx.is_some() {
            return self.superfx_read(addr);
        }
        mapping::read(self.mapping, &self.rom, &self.sram, addr)
    }

    pub fn write(&mut self, addr: u32, value: u8) {
        if self.superfx.is_some() {
            self.superfx_write(addr, value);
            return;
        }
        mapping::write(self.mapping, &mut self.sram, addr, value);
    }
}

/// SuperFX Game Pak RAM size from the expansion-RAM header byte ($FFBD, i.e.
/// header base - 3): (1 << n) KB (superfx.md §10). Carts that lack the extended
/// header (Star Fox family) carry junk here; those default to 32 KB.
fn superfx_ram_size(rom: &[u8], base: usize) -> usize {
    let n = if base >= 3 { rom[base - 3] } else { 0 };
    match n {
        1..=0x0C => 0x400usize << n,
        _ => 0x8000,
    }
}

/// Country code at header+$19: 0 (Japan), 1 (USA), 13 (South Korea) => NTSC;
/// 2..=12 (Europe & variants, Australia) => PAL. Unknown values default to NTSC.
pub fn decode_region(country: u8) -> Region {
    match country {
        2..=12 => Region::Pal,
        _ => Region::Ntsc,
    }
}

/// SRAM size byte: 0 = none, else 1 << n KB. Values above $0C (4 MB) are
/// implausible junk and treated as none.
pub fn decode_sram_size(n: u8) -> usize {
    match n {
        0 => 0,
        1..=0x0C => 0x400usize << n,
        _ => 0,
    }
}

/// Score a candidate header location. Higher wins; <= 0 means implausible.
fn score_header(rom: &[u8], base: usize, expected: Mapping) -> i32 {
    if base + 0x40 > rom.len() {
        return i32::MIN;
    }
    let mut score = 0i32;

    let complement = u16::from_le_bytes([rom[base + 0x1C], rom[base + 0x1D]]);
    let checksum = u16::from_le_bytes([rom[base + 0x1E], rom[base + 0x1F]]);
    if checksum.wrapping_add(complement) == 0xFFFF && checksum != 0 {
        score += 8;
    }

    let map_mode = rom[base + 0x15];
    let mode_matches = match expected {
        Mapping::LoRom => map_mode & 0x0F == 0x00,
        Mapping::HiRom => map_mode & 0x0F == 0x01,
    };
    if mode_matches && map_mode & 0xE0 == 0x20 {
        score += 4;
    } else if mode_matches {
        score += 1;
    } else {
        score -= 4;
    }

    if rom[base..base + 21].iter().all(|&b| (0x20..0x7F).contains(&b)) {
        score += 2;
    }

    // Emulation-mode reset vector at base+$3C must point into ROM ($8000+).
    let reset = u16::from_le_bytes([rom[base + 0x3C], rom[base + 0x3D]]);
    if reset >= 0x8000 {
        score += 2;
    } else {
        score -= 2;
    }

    score
}

/// Header checksum: 16-bit sum of every ROM byte. For non-power-of-two sizes
/// the image splits into the largest power-of-two part plus a remainder; the
/// remainder is counted enough times to pad the total to the next power of two
/// (matches how the mirrored bus would sum).
pub fn compute_checksum(rom: &[u8]) -> u16 {
    let len = rom.len();
    if len == 0 {
        return 0;
    }
    let sum_slice =
        |s: &[u8]| s.iter().fold(0u32, |acc, &b| acc.wrapping_add(b as u32));
    if len.is_power_of_two() {
        return sum_slice(rom) as u16;
    }
    let main = if len == 0 { 0 } else { 1usize << (usize::BITS - 1 - len.leading_zeros() as u32) };
    let remainder = &rom[main..];
    let mut sum = sum_slice(&rom[..main]);
    if !remainder.is_empty() {
        // Repeat the tail to fill main..2*main.
        let repeats = (main / remainder.len()).max(1) as u32;
        sum = sum.wrapping_add(sum_slice(remainder).wrapping_mul(repeats));
    }
    sum as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic image with a valid header at `base`.
    fn synth_rom(size: usize, base: usize, map_mode: u8, country: u8, sram: u8) -> Vec<u8> {
        let mut rom = vec![0u8; size];
        rom[base..base + 21].copy_from_slice(b"TEST CARTRIDGE       ");
        rom[base + 0x15] = map_mode;
        rom[base + 0x18] = sram;
        rom[base + 0x19] = country;
        // Reset vector inside ROM.
        rom[base + 0x3C] = 0x00;
        rom[base + 0x3D] = 0x80;
        rom_with_checksum(&rom, base)
    }

    fn rom_with_checksum(rom: &[u8], base: usize) -> Vec<u8> {
        // Iterate once: writing checksum changes the sum, so solve directly.
        // sum = S0 + cs_lo + cs_hi + cp_lo + cp_hi, with cs + cp = 0xFFFF.
        // cp = 0xFFFF - cs, so cs_lo+cs_hi+cp_lo+cp_hi always sums byte-wise to
        // 0xFF+0xFF = 510 regardless of cs. Compute S0 with those 4 bytes zero,
        // add 510, that is the final checksum.
        let mut rom = rom.to_vec();
        rom[base + 0x1C] = 0;
        rom[base + 0x1D] = 0;
        rom[base + 0x1E] = 0;
        rom[base + 0x1F] = 0;
        let cs = compute_checksum(&rom).wrapping_add(510);
        let cp = 0xFFFFu16 - cs;
        rom[base + 0x1C..base + 0x1E].copy_from_slice(&cp.to_le_bytes());
        rom[base + 0x1E..base + 0x20].copy_from_slice(&cs.to_le_bytes());
        rom
    }

    #[test]
    fn copier_header_is_stripped() {
        let mut inner = synth_rom(0x10000, super::LOROM_HEADER, 0x20, 2, 0);
        inner[0] = 0xAB;
        let mut with_hdr = vec![0xEEu8; 512];
        with_hdr.extend_from_slice(&inner);
        assert_eq!(with_hdr.len() % 0x8000, 512);
        let cart = Cartridge::from_bytes(with_hdr).unwrap();
        assert_eq!(cart.rom.len(), 0x10000);
        assert_eq!(cart.rom[0], 0xAB);
    }

    #[test]
    fn no_copier_header_left_alone() {
        let rom = synth_rom(0x10000, super::LOROM_HEADER, 0x20, 2, 0);
        let cart = Cartridge::from_bytes(rom).unwrap();
        assert_eq!(cart.rom.len(), 0x10000);
    }

    #[test]
    fn lorom_header_wins_on_synthetic_lorom() {
        let rom = synth_rom(0x20000, super::LOROM_HEADER, 0x20, 2, 3);
        let cart = Cartridge::from_bytes(rom).unwrap();
        assert_eq!(cart.mapping, Mapping::LoRom);
        assert_eq!(cart.region, Region::Pal);
        assert_eq!(cart.sram.len(), 0x2000);
        assert_eq!(cart.title, "TEST CARTRIDGE");
        assert!(cart.checksum_valid);
        assert!(!cart.fastrom);
    }

    #[test]
    fn hirom_header_wins_on_synthetic_hirom() {
        let rom = synth_rom(0x20000, super::HIROM_HEADER, 0x31, 1, 0);
        let cart = Cartridge::from_bytes(rom).unwrap();
        assert_eq!(cart.mapping, Mapping::HiRom);
        assert_eq!(cart.region, Region::Ntsc);
        assert!(cart.fastrom);
    }

    #[test]
    fn region_decode_table() {
        assert_eq!(decode_region(0), Region::Ntsc); // Japan
        assert_eq!(decode_region(1), Region::Ntsc); // USA
        assert_eq!(decode_region(13), Region::Ntsc); // South Korea
        for c in 2..=12u8 {
            assert_eq!(decode_region(c), Region::Pal, "country {c}");
        }
        assert_eq!(decode_region(0xFF), Region::Ntsc);
    }

    #[test]
    fn sram_size_decode() {
        assert_eq!(decode_sram_size(0), 0);
        assert_eq!(decode_sram_size(1), 0x800);
        assert_eq!(decode_sram_size(3), 0x2000); // 8 KB, the most common
        assert_eq!(decode_sram_size(0xFF), 0);
    }

    #[test]
    fn superfx_gsu2_header_detected() {
        // LoROM map-mode $20, chipset $15 (ROM+GSU+RAM+Battery = GSU2), exp-RAM
        // byte $06 -> 64 KB Game Pak RAM.
        let mut rom = synth_rom(0x80000, super::LOROM_HEADER, 0x20, 2, 0);
        rom[super::LOROM_HEADER + 0x16] = 0x15;
        rom[super::LOROM_HEADER - 3] = 0x06; // $FFBD exp-RAM size
        let rom = rom_with_checksum(&rom, super::LOROM_HEADER);
        let cart = Cartridge::from_bytes(rom).unwrap();
        assert_eq!(cart.mapping, Mapping::LoRom);
        let fx = cart.superfx.as_ref().expect("GSU detected");
        assert_eq!(fx.ram_size(), 0x10000);
    }

    #[test]
    fn plain_lorom_has_no_superfx() {
        let rom = synth_rom(0x20000, super::LOROM_HEADER, 0x20, 2, 3);
        let cart = Cartridge::from_bytes(rom).unwrap();
        assert!(cart.superfx.is_none());
    }
}
