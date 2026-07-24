//! DSP-1 / DSP-1B math coprocessor (NEC uPD77C25), high-level emulation.
//!
//! The DSP-1 firmware ROM is copyrighted and undumpable in scope, so this is an
//! HLE reimplementation of the reverse-engineered command set (snes9x
//! `dsp1.cpp`), not a uPD7725 core. The SNES talks to it through two
//! byte-wide memory-mapped ports:
//!
//! - DR (Data Register): the CPU writes a command byte, then the command's
//!   parameter bytes (low byte of each 16-bit word first), then reads the
//!   result bytes back.
//! - SR (Status Register): bit 7 (RQM, `$80`) = ready. HLE completes each
//!   command instantly, so SR always reports ready.
//!
//! Command math lives in [`commands`]; the internal data/sine tables in
//! [`tables`].

mod commands;
mod tables;

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

/// DR/SR port placement, selected from the cartridge map mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Dsp1Mapping {
    /// LoROM / Mode 20: banks $30-$3F (+$B0-$BF mirror), DR $8000-$BFFF, SR $C000-$FFFF.
    LoRom,
    /// HiROM / Mode 21: banks $00-$0F (+$80-$8F mirror), DR $6000-$6FFF, SR $7000-$7FFF.
    HiRom,
}

/// The port an address decodes to within a DSP-1 board.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dsp1Port {
    Dr,
    Sr,
}

impl Dsp1Mapping {
    /// Decode a CPU (bank, address) to the DSP-1 port it selects, if any.
    pub fn decode(self, bank: u8, addr: u16) -> Option<Dsp1Port> {
        let b = bank & 0x7f;
        match self {
            Dsp1Mapping::LoRom => {
                if (0x30..=0x3f).contains(&b) && addr & 0x8000 != 0 {
                    if addr & 0x4000 == 0 {
                        Some(Dsp1Port::Dr)
                    } else {
                        Some(Dsp1Port::Sr)
                    }
                } else {
                    None
                }
            }
            Dsp1Mapping::HiRom => {
                if (0x00..=0x0f).contains(&b) {
                    match addr & 0xf000 {
                        0x6000 => Some(Dsp1Port::Dr),
                        0x7000 => Some(Dsp1Port::Sr),
                        _ => None,
                    }
                } else {
                    None
                }
            }
        }
    }
}

/// Detect DSP-1 presence and mapping from the SNES internal header.
///
/// `chipset` is header byte $16 (ROM+coprocessor variants $03/$04/$05, DSP
/// family = high nibble $0x). `map_mode` is header byte $15 ($20=LoROM,
/// $21=HiROM). The DSP-1 vs DSP-1B distinction is not encoded in the header
/// (resolved by a title database elsewhere); this returns the mapping only.
///
/// WARNING: chipset $03/$04/$05 is the whole NEC DSP coprocessor family. It
/// also matches DSP-2 (Dungeon Master), DSP-3 (SD Gundam GX) and DSP-4
/// (Top Gear 3000), whose command sets are entirely different from DSP-1's.
/// The variant is NOT in the header and must be resolved by a per-title
/// database (checksum/title allow-list); until such a database is wired in,
/// those carts are misidentified and answered with DSP-1 math. Only genuine
/// DSP-1/1B titles (Super Mario Kart, Pilotwings, ...) are correct here.
pub fn detect(chipset: u8, map_mode: u8) -> Option<Dsp1Mapping> {
    if !matches!(chipset, 0x03 | 0x04 | 0x05) {
        return None;
    }
    match map_mode & 0x0f {
        0x01 => Some(Dsp1Mapping::HiRom),
        _ => Some(Dsp1Mapping::LoRom),
    }
}

/// DSP-1 state: the DR/SR command state machine plus persistent math state
/// (attitude matrices and projection camera) that commands build up.
#[derive(Serialize, Deserialize)]
pub struct Dsp1 {
    // ---- Host protocol state machine ----
    command: u8,
    waiting4command: bool,
    first_parameter: bool,
    /// Bytes of parameters still expected (max 7 words = 14 bytes).
    in_count: i32,
    in_index: usize,
    /// Bytes of result still pending to be read.
    out_count: i32,
    out_index: usize,
    parameters: [u8; 16],
    output: [u8; 8],

    // ---- Persistent attitude matrices (Q15) ----
    matrix_a: [[i16; 3]; 3],
    matrix_b: [[i16; 3]; 3],
    matrix_c: [[i16; 3]; 3],

    // ---- Persistent projection camera (Op02 Parameter) ----
    sin_aas: i16,
    cos_aas: i16,
    sin_azs: i16,
    cos_azs: i16,
    nx: i16,
    ny: i16,
    nz: i16,
    centre_x: i16,
    centre_y: i16,
    gx: i16,
    gy: i16,
    gz: i16,
    c_les: i16,
    e_les: i16,
    g_les: i16,
    vplane_c: i16,
    vplane_e: i16,
    sin_azs_clip: i16,
    cos_azs_clip: i16,
    secazs_c1: i16,
    secazs_e1: i16,
    secazs_c2: i16,
    secazs_e2: i16,
    voffset: i16,

    /// Raster (Op0A) scanline counter, auto-incremented per line.
    op0a_vs: i16,
}

impl Default for Dsp1 {
    fn default() -> Self {
        Dsp1 {
            command: 0,
            waiting4command: true,
            first_parameter: true,
            in_count: 0,
            in_index: 0,
            out_count: 0,
            out_index: 0,
            parameters: [0; 16],
            output: [0; 8],
            matrix_a: [[0; 3]; 3],
            matrix_b: [[0; 3]; 3],
            matrix_c: [[0; 3]; 3],
            sin_aas: 0,
            cos_aas: 0,
            sin_azs: 0,
            cos_azs: 0,
            nx: 0,
            ny: 0,
            nz: 0,
            centre_x: 0,
            centre_y: 0,
            gx: 0,
            gy: 0,
            gz: 0,
            c_les: 0,
            e_les: 0,
            g_les: 0,
            vplane_c: 0,
            vplane_e: 0,
            sin_azs_clip: 0,
            cos_azs_clip: 0,
            secazs_c1: 0,
            secazs_e1: 0,
            secazs_c2: 0,
            secazs_e2: 0,
            voffset: 0,
            op0a_vs: 0,
        }
    }
}

impl Dsp1 {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the Status Register. Bit 7 (RQM) is always set: the HLE runs each
    /// command to completion instantly, so it is always ready to accept a byte
    /// or has the next result byte pending.
    pub fn read_sr(&self) -> u8 {
        0x80
    }

    /// Write one byte to the Data Register (command byte, then parameters).
    pub fn write_dr(&mut self, byte: u8) {
        // Op0A/Op1A raster streaming: extra DR writes advance the scanline output.
        if (self.command == 0x0a || self.command == 0x1a) && self.out_count != 0 {
            self.out_count -= 1;
            self.out_index += 1;
            return;
        }

        if self.waiting4command {
            self.command = byte;
            self.in_index = 0;
            self.waiting4command = false;
            self.first_parameter = true;

            let words: i32 = match byte {
                0x00 => 2,
                0x10 | 0x30 => 2,
                0x20 => 2,
                0x04 | 0x24 => 2,
                0x08 => 3,
                0x18 => 4,
                0x28 => 3,
                0x38 => 4,
                0x0c | 0x2c => 3,
                0x1c | 0x3c => 6,
                0x02 | 0x12 | 0x22 | 0x32 => 7,
                0x0a => 1,
                0x1a | 0x2a | 0x3a => {
                    self.command = 0x1a;
                    1
                }
                0x06 | 0x16 | 0x26 | 0x36 => 3,
                0x0e | 0x1e | 0x2e | 0x3e => 2,
                0x01 | 0x05 | 0x31 | 0x35 => 4,
                0x11 | 0x15 => 4,
                0x21 | 0x25 => 4,
                0x0d | 0x09 | 0x39 | 0x3d => 3,
                0x19 | 0x1d => 3,
                0x29 | 0x2d => 3,
                0x03 | 0x33 => 3,
                0x13 => 3,
                0x23 => 3,
                0x0b | 0x3b => 3,
                0x1b => 3,
                0x2b => 3,
                0x14 | 0x34 => 6,
                0x07 | 0x0f => 1,
                0x27 | 0x2f => 1,
                0x1f | 0x17 | 0x37 | 0x3f => {
                    self.command = 0x1f;
                    1
                }
                _ => {
                    self.in_count = 0;
                    self.waiting4command = true;
                    self.first_parameter = true;
                    0
                }
            };
            self.in_count = words << 1;
        } else {
            self.parameters[self.in_index] = byte;
            self.first_parameter = false;
            self.in_index += 1;
        }

        if self.waiting4command || (self.first_parameter && byte == 0x80) {
            self.waiting4command = true;
            self.first_parameter = false;
        } else if self.first_parameter && (self.in_count != 0 || self.in_index == 0) {
            // command byte accepted; still awaiting parameters
        } else if self.in_count != 0 {
            self.in_count -= 1;
            if self.in_count == 0 {
                self.waiting4command = true;
                self.out_index = 0;
                self.execute();
            }
        }
    }

    /// Read one byte from the Data Register (result bytes in order).
    pub fn read_dr(&mut self) -> u8 {
        if self.out_count == 0 {
            return 0x80;
        }

        let t = if self.command == 0x1f {
            // ROM dump: 1024 words little-endian from the internal data ROM.
            let idx = self.out_index;
            let word = tables::DSP1_ROM[idx >> 1];
            if idx & 1 == 0 {
                (word & 0xff) as u8
            } else {
                (word >> 8) as u8
            }
        } else {
            self.output[self.out_index]
        };

        self.out_index += 1;
        self.out_count -= 1;

        if self.out_count == 0 && (self.command == 0x0a || self.command == 0x1a) {
            // Raster streams successive scanlines until a new command is written.
            let out = self.raster(self.op0a_vs);
            self.op0a_vs = self.op0a_vs.wrapping_add(1);
            self.set_out(&out);
            self.out_index = 0;
        }

        self.waiting4command = true;
        t
    }
}
