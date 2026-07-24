//! CX4 (Capcom Custom Chip 4 / Hitachi HG51B169) cartridge coprocessor, HLE.
//!
//! Used by Mega Man X2 and Mega Man X3 (both LoROM) for 3-D wireframe/vector
//! graphics, sprite scale/rotate, OAM building and trigonometric math. The
//! chip's internal 24-bit microprogram is Capcom-copyrighted, so this is a
//! high-level reimplementation of the reverse-engineered command set (snes9x
//! `c4.cpp` / `c4emu.cpp`), not an HG51B169 core. See
//! `.claude/skills/snes-refs/references/cx4.md`.
//!
//! # Integration model (for `bus.rs` / `cartridge/`)
//!
//! * **ROM sharing**: the CX4 does NOT own the cartridge ROM. Like the SuperFX
//!   and SA-1, the owner passes `&[u8]` ROM into every entry point that can
//!   touch it ([`Cx4::write`]), because a few command/DMA triggers fetch model,
//!   line and sprite data from ROM through the LoROM window.
//! * **Window**: the CX4 exposes a flat 8 KB RAM at `$6000-$7FFF` in banks
//!   `$00-$3F` (mirror `$80-$BF`). Route reads through [`Cx4::read`] and writes
//!   through [`Cx4::write`]; [`maps`] decides whether an address hits the window.
//! * **Detection**: [`is_cx4`] on the header chipset/map-mode bytes.
//! * **Save states**: only the 8 KB `ram` array is persistent; all `wf_*`/`atan_*`
//!   scratch is transient within one command and is `#[serde(skip)]` + `Default`.

mod ops;
mod tables;

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

/// The CX4 window is a flat 8 KB region (`$6000-$7FFF`, offset `addr - $6000`).
const C4RAM_SIZE: usize = 0x2000;

/// Detect a CX4 cart from the SNES internal header.
///
/// `chipset` is header byte `$16` (`$FFD6`); `map_mode` is byte `$15` (`$FFD5`).
/// CX4 is chipset `$F3` and LoROM (map-mode low nibble `$0`, i.e. `$20`/`$30`).
/// The sibling custom-chip codes `$F5`/`$F6`/`$F9` are OTHER chips, not CX4, so
/// the match on `$F3` must be exact (cx4.md §1).
pub fn is_cx4(map_mode: u8, chipset: u8) -> bool {
    chipset == 0xF3 && (map_mode & 0x0F) == 0x00
}

/// Does CPU address `(bank, addr)` hit the CX4 window `$6000-$7FFF` in banks
/// `$00-$3F` (+ `$80-$BF` mirror)? ROM `$8000-$FFFF` is handled by the normal
/// LoROM cartridge path, not here.
pub fn maps(bank: u8, addr: u16) -> bool {
    (bank & 0x7F) <= 0x3F && (0x6000..=0x7FFF).contains(&addr)
}

/// CX4 coprocessor state.
#[derive(Serialize, Deserialize)]
pub struct Cx4 {
    /// 8 KB CX4 window. `$6000-$6BFF` = 3 KB data RAM; `$7F40-$7FAF` = I/O
    /// ports; the rest is scratch / render planes. All persistent CX4 state
    /// lives here.
    #[serde(with = "crate::serde_util::boxed_bytes")]
    ram: Box<[u8; C4RAM_SIZE]>,

    // Transient wireframe/atan scratch registers (snes9x file-scope `C4WF*` /
    // `C41F*` globals). Always reloaded from `ram` at the start of each command,
    // never read across commands, hence not serialized.
    #[serde(skip)]
    wf_x: i16,
    #[serde(skip)]
    wf_y: i16,
    #[serde(skip)]
    wf_z: i16,
    #[serde(skip)]
    wf_x2: i16,
    #[serde(skip)]
    wf_y2: i16,
    #[serde(skip)]
    wf_dist: i16,
    #[serde(skip)]
    wf_scale: i16,
    #[serde(skip)]
    if_x: i16,
    #[serde(skip)]
    if_y: i16,
    #[serde(skip)]
    if_angle_res: i16,
    #[serde(skip)]
    if_dist: i16,
    #[serde(skip)]
    if_dist_val: i16,
}

impl Default for Cx4 {
    fn default() -> Self {
        Cx4 {
            ram: Box::new([0; C4RAM_SIZE]),
            wf_x: 0,
            wf_y: 0,
            wf_z: 0,
            wf_x2: 0,
            wf_y2: 0,
            wf_dist: 0,
            wf_scale: 0,
            if_x: 0,
            if_y: 0,
            if_angle_res: 0,
            if_dist: 0,
            if_dist_val: 0,
        }
    }
}

impl Cx4 {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a byte from the CX4 window. `addr` is the CPU offset `$6000-$7FFF`.
    ///
    /// `$7F5E` (status) always reads `$00`: the HLE completes every command
    /// instantly, so the "CX4 running" bit is never set. Every other address is
    /// plain RAM (cx4.md §2.1).
    pub fn read(&self, addr: u16) -> u8 {
        if addr == 0x7F5E {
            return 0x00;
        }
        self.ram[(addr as usize).wrapping_sub(0x6000) & (C4RAM_SIZE - 1)]
    }

    /// Write a byte to the CX4 window and run any triggered side effect.
    ///
    /// `rom` is the whole cartridge ROM (the CX4 fetches model/line/sprite data
    /// from it via the LoROM mapping). A write to `$7F4F` dispatches a command;
    /// a write to `$7F47` triggers a ROM->RAM block load (cx4.md §4).
    pub fn write(&mut self, rom: &[u8], addr: u16, value: u8) {
        let off = (addr as usize).wrapping_sub(0x6000) & (C4RAM_SIZE - 1);
        self.ram[off] = value;

        match addr {
            0x7F4F => self.dispatch(rom, value),
            0x7F47 => self.dma_load(rom),
            _ => {}
        }
    }

    /// `$7F47` trigger: `memmove(C4RAM[dst & $1FFF], ROM[src24], len)`.
    fn dma_load(&mut self, rom: &[u8]) {
        let src = self.read_3word(0x1F40);
        let len = self.read_word(0x1F43) as usize;
        let dst = (self.read_word(0x1F45) as usize) & 0x1FFF;
        for i in 0..len {
            let byte = rom_read(rom, src, i);
            let d = (dst + i) & 0x1FFF;
            self.ram[d] = byte;
        }
    }

    /// `$7F4F` trigger: dispatch on the command byte (cx4.md §4).
    fn dispatch(&mut self, rom: &[u8], byte: u8) {
        // Test/parameter-poke special case: with sub-mode $0E, a command byte
        // < $40 and a multiple of 4 just stages `byte >> 2` into $7F80.
        if self.ram[0x1F4D] == 0x0E && byte < 0x40 && (byte & 3) == 0 {
            self.ram[0x1F80] = byte >> 2;
            return;
        }

        match byte {
            0x00 => self.process_sprites(rom),
            0x01 => {
                // Draw wireframe: clear the 16*12*3 render planes first.
                for b in self.ram[0x300..0x300 + 16 * 12 * 3 * 4].iter_mut() {
                    *b = 0;
                }
                self.draw_wireframe(rom);
            }
            0x05 => {
                // Propulsion / reciprocal-scale.
                let mut tmp: i32 = 0x10000;
                let d = self.read_word(0x1F83);
                if d != 0 {
                    tmp = sar(
                        (tmp / d as i32).wrapping_mul(self.read_word(0x1F81) as i32),
                        8,
                    );
                }
                self.write_word(0x1F80, tmp as u16);
            }
            0x0D => {
                self.if_x = self.read_word(0x1F80) as i16;
                self.if_y = self.read_word(0x1F83) as i16;
                self.if_dist_val = self.read_word(0x1F86) as i16;
                self.op_0d();
                self.write_word(0x1F89, self.if_x as u16);
                self.write_word(0x1F8C, self.if_y as u16);
            }
            0x10 => {
                let mut r1 = self.read_word(0x1F83) as i32;
                if r1 & 0x8000 != 0 {
                    r1 |= !0x7FFF;
                } else {
                    r1 &= 0x7FFF;
                }
                let theta = (self.read_word(0x1F80) & 0x1FF) as usize;
                let tmp = sar(
                    r1.wrapping_mul(tables::COS_TABLE[theta] as i32).wrapping_mul(2),
                    16,
                );
                self.write_3word(0x1F86, tmp as u32);
                let tmp = sar(
                    r1.wrapping_mul(tables::SIN_TABLE[theta] as i32).wrapping_mul(2),
                    16,
                );
                // ×(1 − 1/64) skew on the Y (sine) component.
                self.write_3word(0x1F89, (tmp - sar(tmp, 6)) as u32);
            }
            0x13 => {
                let r = self.read_word(0x1F83) as i32;
                let theta = (self.read_word(0x1F80) & 0x1FF) as usize;
                let tmp = sar(r.wrapping_mul(tables::COS_TABLE[theta] as i32).wrapping_mul(2), 8);
                self.write_3word(0x1F86, tmp as u32);
                let tmp = sar(r.wrapping_mul(tables::SIN_TABLE[theta] as i32).wrapping_mul(2), 8);
                self.write_3word(0x1F89, tmp as u32);
            }
            0x15 => {
                self.if_x = self.read_word(0x1F80) as i16;
                self.if_y = self.read_word(0x1F83) as i16;
                self.op_15();
                self.write_word(0x1F80, self.if_dist as u16);
            }
            0x1F => {
                self.if_x = self.read_word(0x1F80) as i16;
                self.if_y = self.read_word(0x1F83) as i16;
                self.op_1f();
                self.write_word(0x1F86, self.if_angle_res as u16);
            }
            0x22 => self.trapezoid(),
            0x25 => {
                let foo = self.read_3word(0x1F80) as i32;
                let bar = self.read_3word(0x1F83) as i32;
                self.write_3word(0x1F80, foo.wrapping_mul(bar) as u32);
            }
            0x2D => {
                self.wf_x = self.read_word(0x1F81) as i16;
                self.wf_y = self.read_word(0x1F84) as i16;
                self.wf_z = self.read_word(0x1F87) as i16;
                self.wf_x2 = self.ram[0x1F89] as i16;
                self.wf_y2 = self.ram[0x1F8A] as i16;
                self.wf_dist = self.ram[0x1F8B] as i16;
                self.wf_scale = self.read_word(0x1F90) as i16;
                self.transf_wireframe2();
                self.write_word(0x1F80, self.wf_x as u16);
                self.write_word(0x1F83, self.wf_y as u16);
            }
            0x40 => {
                let mut sum: u16 = 0;
                for i in 0..0x800 {
                    sum = sum.wrapping_add(self.ram[i] as u16);
                }
                self.write_word(0x1F80, sum);
            }
            0x54 => {
                let mut a = self.read_3word(0x1F80) as i64;
                // Sign-extend from bit 23.
                if (a >> 23) & 1 != 0 {
                    a |= -0x0100_0000_i64;
                }
                a = a.wrapping_mul(a);
                self.write_3word(0x1F83, a as u32);
                self.write_3word(0x1F86, (a >> 24) as u32);
            }
            0x5C => {
                self.ram[0..48].copy_from_slice(&tables::TEST_PATTERN);
            }
            0x89 => {
                self.ram[0x1F80] = 0x36;
                self.ram[0x1F81] = 0x43;
                self.ram[0x1F82] = 0x05;
            }
            // Unknown command bytes do nothing (snes9x logs "Unknown C4 command").
            _ => {}
        }
    }

    fn process_sprites(&mut self, rom: &[u8]) {
        match self.ram[0x1F4D] {
            0x00 => self.conv_oam(rom),
            0x03 => self.do_scale_rotate(rom, 0),
            0x05 => self.transform_lines(),
            0x07 => self.do_scale_rotate(rom, 64),
            0x08 => self.draw_wireframe(rom),
            0x0B => self.spr_disintegrate(),
            0x0C => self.bit_plane_wave(),
            _ => {}
        }
    }

    // ---- C4RAM little-endian accessors (offsets, not CPU addresses) ----

    fn read_word(&self, off: usize) -> u16 {
        (self.ram[off] as u16) | ((self.ram[off + 1] as u16) << 8)
    }

    fn write_word(&mut self, off: usize, v: u16) {
        self.ram[off] = v as u8;
        self.ram[off + 1] = (v >> 8) as u8;
    }

    fn read_3word(&self, off: usize) -> u32 {
        (self.ram[off] as u32)
            | ((self.ram[off + 1] as u32) << 8)
            | ((self.ram[off + 2] as u32) << 16)
    }

    fn write_3word(&mut self, off: usize, v: u32) {
        self.ram[off] = v as u8;
        self.ram[off + 1] = (v >> 8) as u8;
        self.ram[off + 2] = (v >> 16) as u8;
    }
}

/// Arithmetic (sign-preserving) shift right, matching snes9x `SAR`.
#[inline]
pub(super) fn sar(x: i32, n: u32) -> i32 {
    x >> n
}

/// LoROM CX4 ROM fetch: `ROM + ((addr & $FF0000) >> 1) + (addr & $7FFF)`, then
/// byte `i` (`C4GetMemPointer`). Out-of-range reads return `0`.
#[inline]
pub(super) fn rom_read(rom: &[u8], addr: u32, i: usize) -> u8 {
    let base = (((addr & 0xFF_0000) >> 1) + (addr & 0x7FFF)) as usize;
    rom.get(base + i).copied().unwrap_or(0)
}
