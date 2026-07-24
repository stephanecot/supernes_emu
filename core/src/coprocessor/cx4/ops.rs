//! CX4 HLE operation bodies: the reverse-engineered command math, transcribed
//! verbatim from snes9x `c4.cpp` (`C4TransfWireFrame*`, `C4CalcWireFrame`,
//! `C4Op0D/15/1F`) and `c4emu.cpp` (sprite/wireframe rasterisers). See
//! `.claude/skills/snes-refs/references/cx4.md` §4.
//!
//! Fixed-point: the wireframe rotate/project uses C-library `sin`/`cos` on
//! `f64` exactly as snes9x does (`⚠` visually correct, not bit-exact to the
//! HG51B169 Q15 tables — cx4.md §7). The scale/rotate and polar commands use
//! the Q15 tables in [`super::tables`].

use super::tables::{COS_TABLE, SIN_TABLE};
use super::{rom_read, sar, Cx4};

const C4_PI: f64 = 3.14159265;

/// C double→int16 conversion: truncate toward zero, then narrow (wrap) to 16
/// bits, matching a C `(int16)(double)` cast for in-range values.
#[inline]
fn to_i16(f: f64) -> i16 {
    f as i64 as i16
}

impl Cx4 {
    // ---- Shared 3-D transforms (byte-scale angles, 128 = 2π) ----

    /// `C4TransfWireFrame`: rotate about X/Y/Z, subtract the `$95` Z bias, then
    /// perspective-project. Reads/writes `wf_x`/`wf_y`.
    pub(super) fn transf_wireframe(&mut self) {
        let mut c4x = self.wf_x as f64;
        let mut c4y = self.wf_y as f64;
        let mut c4z = self.wf_z as f64 - 0x95 as f64;

        let t = -(self.wf_x2 as f64) * C4_PI * 2.0 / 128.0;
        let c4y2 = c4y * t.cos() - c4z * t.sin();
        let c4z2 = c4y * t.sin() + c4z * t.cos();

        let t = -(self.wf_y2 as f64) * C4_PI * 2.0 / 128.0;
        let c4x2 = c4x * t.cos() + c4z2 * t.sin();
        c4z = -c4x * t.sin() + c4z2 * t.cos();

        let t = -(self.wf_dist as f64) * C4_PI * 2.0 / 128.0;
        c4x = c4x2 * t.cos() - c4y2 * t.sin();
        c4y = c4x2 * t.sin() + c4y2 * t.cos();

        let denom = 0x90 as f64 * (c4z + 0x95 as f64);
        self.wf_x = to_i16(c4x * self.wf_scale as f64 / denom * 0x95 as f64);
        self.wf_y = to_i16(c4y * self.wf_scale as f64 / denom * 0x95 as f64);
    }

    /// `C4TransfWireFrame2`: same rotations, no Z bias, orthographic scale.
    pub(super) fn transf_wireframe2(&mut self) {
        let mut c4x = self.wf_x as f64;
        let mut c4y = self.wf_y as f64;
        let c4z = self.wf_z as f64;

        let t = -(self.wf_x2 as f64) * C4_PI * 2.0 / 128.0;
        let c4y2 = c4y * t.cos() - c4z * t.sin();
        let c4z2 = c4y * t.sin() + c4z * t.cos();

        let t = -(self.wf_y2 as f64) * C4_PI * 2.0 / 128.0;
        let c4x2 = c4x * t.cos() + c4z2 * t.sin();
        // The orthographic path re-rotates but never reads the resulting c4z
        // (no perspective divide), so the reassignment in snes9x is dropped here.

        let t = -(self.wf_dist as f64) * C4_PI * 2.0 / 128.0;
        c4x = c4x2 * t.cos() - c4y2 * t.sin();
        c4y = c4x2 * t.sin() + c4y2 * t.cos();

        self.wf_x = to_i16(c4x * self.wf_scale as f64 / 0x100 as f64);
        self.wf_y = to_i16(c4y * self.wf_scale as f64 / 0x100 as f64);
    }

    /// `C4CalcWireFrame`: Bresenham setup — span length into `wf_dist`, per-step
    /// (256-scaled) increment into `wf_x`/`wf_y`, major axis pinned to ±256.
    pub(super) fn calc_wireframe(&mut self) {
        self.wf_x = self.wf_x2.wrapping_sub(self.wf_x);
        self.wf_y = self.wf_y2.wrapping_sub(self.wf_y);

        if (self.wf_x as i32).abs() > (self.wf_y as i32).abs() {
            self.wf_dist = ((self.wf_x as i32).abs() + 1) as i16;
            self.wf_y = to_i16(256.0 * self.wf_y as f64 / (self.wf_x as i32).abs() as f64);
            self.wf_x = if self.wf_x < 0 { -256 } else { 256 };
        } else if self.wf_y != 0 {
            self.wf_dist = ((self.wf_y as i32).abs() + 1) as i16;
            self.wf_x = to_i16(256.0 * self.wf_x as f64 / (self.wf_y as i32).abs() as f64);
            self.wf_y = if self.wf_y < 0 { -256 } else { 256 };
        } else {
            self.wf_dist = 0;
        }
    }

    // ---- Scalar math (`C4Op*`) ----

    /// `C4Op1F`: atan2(Y,X) into a 9-bit angle (512 = full circle).
    pub(super) fn op_1f(&mut self) {
        if self.if_x == 0 {
            self.if_angle_res = if self.if_y > 0 { 0x80 } else { 0x180 };
        } else {
            let tanval = self.if_y as f64 / self.if_x as f64;
            let mut angle = to_i16(tanval.atan() / (C4_PI * 2.0) * 512.0) as i32;
            if self.if_x < 0 {
                angle += 0x100;
            }
            self.if_angle_res = (angle & 0x1FF) as i16;
        }
    }

    /// `C4Op15`: `if_dist = (int16) sqrt(X² + Y²)`.
    pub(super) fn op_15(&mut self) {
        let v = ((self.if_y as f64 * self.if_y as f64) + (self.if_x as f64 * self.if_x as f64)).sqrt();
        self.if_dist = to_i16(v);
    }

    /// `C4Op0D`: scale (X,Y) to magnitude `if_dist_val` with the chip's on-program
    /// 0.98/0.99 factors.
    pub(super) fn op_0d(&mut self) {
        let mag =
            ((self.if_y as f64 * self.if_y as f64) + (self.if_x as f64 * self.if_x as f64)).sqrt();
        let t = self.if_dist_val as f64 / mag;
        self.if_y = to_i16(self.if_y as f64 * t * 0.99);
        self.if_x = to_i16(self.if_x as f64 * t * 0.98);
    }

    /// Command `$22`: per-scanline trapezoid left/right spans into `$6800`/`$6900`.
    pub(super) fn trapezoid(&mut self) {
        let angle1 = (self.read_word(0x1F8C) & 0x1FF) as usize;
        let angle2 = (self.read_word(0x1F8F) & 0x1FF) as usize;

        let tan1 = if COS_TABLE[angle1] != 0 {
            ((SIN_TABLE[angle1] as i32) << 16) / COS_TABLE[angle1] as i32
        } else {
            0x8000_0000u32 as i32
        };
        let tan2 = if COS_TABLE[angle2] != 0 {
            ((SIN_TABLE[angle2] as i32) << 16) / COS_TABLE[angle2] as i32
        } else {
            0x8000_0000u32 as i32
        };

        let mut y = self.read_word(0x1F83).wrapping_sub(self.read_word(0x1F89)) as i16;

        for j in 0..225usize {
            let (mut left, mut right): (i16, i16);
            if y >= 0 {
                let base = -(self.read_word(0x1F80) as i32) + self.read_word(0x1F86) as i32;
                left = sar(tan1.wrapping_mul(y as i32), 16)
                    .wrapping_add(base) as i16;
                right = sar(tan2.wrapping_mul(y as i32), 16)
                    .wrapping_add(base)
                    .wrapping_add(self.read_word(0x1F93) as i32) as i16;

                if left < 0 && right < 0 {
                    left = 1;
                    right = 0;
                } else if left < 0 {
                    left = 0;
                } else if right < 0 {
                    right = 0;
                }

                if left > 255 && right > 255 {
                    left = 255;
                    right = 254;
                } else if left > 255 {
                    left = 255;
                } else if right > 255 {
                    right = 255;
                }
            } else {
                left = 1;
                right = 0;
            }

            self.ram[j + 0x800] = left as u8;
            self.ram[j + 0x900] = right as u8;
            y = y.wrapping_add(1);
        }
    }

    // ---- Sprite / wireframe rasterisers ----

    /// `$00:$00` `C4ConvOAM`: build SNES OAM from a sprite list at `$6220+`.
    ///
    /// `⚠` UNVERIFIED edge cases (snes9x `XXX:` comments): attribute-bit masking
    /// (`SprAttr`), carry from the `SprName` addition, and whether the
    /// no-sub-sprite branch should also cull to the on-screen box.
    pub(super) fn conv_oam(&mut self, rom: &[u8]) {
        let oam_start = (self.ram[0x626] as usize) << 2;
        // Clear OAM-to-be from $61fd downward to the write cursor. `i` steps by 4
        // and, being misaligned vs `oam_start`, can pass it, so guard the usize
        // subtraction against underflow (an unguarded `i -= 4` at i<4 wraps to a
        // huge index and panics).
        let mut i = 0x1FD;
        while i > oam_start {
            self.ram[i] = 0xE0;
            match i.checked_sub(4) {
                Some(n) => i = n,
                None => break,
            }
        }

        if self.ram[0x620] == 0 {
            return;
        }

        let global_x = self.read_word(0x0621);
        let global_y = self.read_word(0x0623);
        let mut oam_ptr = oam_start;
        let mut oam_ptr2 = 0x200 + (self.ram[0x626] as usize >> 2);
        let mut offset = (self.ram[0x626] & 3) * 2;
        let mut spr_count: i32 = 128 - self.ram[0x626] as i32;

        let mut srcptr = 0x220usize;
        let mut i = self.ram[0x620] as i32;
        while i > 0 && spr_count > 0 {
            let spr_x = (self.read_word(srcptr).wrapping_sub(global_x)) as i16;
            let spr_y = (self.read_word(srcptr + 2).wrapping_sub(global_y)) as i16;
            let spr_name = self.ram[srcptr + 5];
            let spr_attr = self.ram[srcptr + 4] | self.ram[srcptr + 6];

            let rom_addr = self.read_3word(srcptr + 7);
            let sub_count = rom_read(rom, rom_addr, 0);
            if sub_count != 0 {
                let mut k = 0usize;
                let mut spr_cnt = sub_count as i32;
                while spr_cnt > 0 && spr_count > 0 {
                    let b0 = rom_read(rom, rom_addr, 1 + k * 4);
                    let b1 = rom_read(rom, rom_addr, 1 + k * 4 + 1);
                    let b2 = rom_read(rom, rom_addr, 1 + k * 4 + 2);
                    let b3 = rom_read(rom, rom_addr, 1 + k * 4 + 3);

                    let mut x = b1 as i8 as i16;
                    if spr_attr & 0x40 != 0 {
                        x = -x - if b0 & 0x20 != 0 { 16 } else { 8 };
                    }
                    x = x.wrapping_add(spr_x);

                    if (-16..=272).contains(&(x as i32)) {
                        let mut y = b2 as i8 as i16;
                        if spr_attr & 0x80 != 0 {
                            y = -y - if b0 & 0x20 != 0 { 16 } else { 8 };
                        }
                        y = y.wrapping_add(spr_y);

                        if (-16..=224).contains(&(y as i32)) {
                            self.ram[oam_ptr & 0x1FFF] = (x & 0xFF) as u8;
                            self.ram[(oam_ptr + 1) & 0x1FFF] = y as u8;
                            self.ram[(oam_ptr + 2) & 0x1FFF] = spr_name.wrapping_add(b3);
                            self.ram[(oam_ptr + 3) & 0x1FFF] = spr_attr ^ (b0 & 0xC0);

                            let p2 = oam_ptr2 & 0x1FFF;
                            self.ram[p2] &= !(3u8 << offset);
                            if x & 0x100 != 0 {
                                self.ram[p2] |= 1u8 << offset;
                            }
                            if b0 & 0x20 != 0 {
                                self.ram[p2] |= 2u8 << offset;
                            }

                            oam_ptr += 4;
                            spr_count -= 1;
                            offset = (offset + 2) & 6;
                            if offset == 0 {
                                oam_ptr2 += 1;
                            }
                        }
                    }
                    spr_cnt -= 1;
                    k += 1;
                }
            } else if spr_count > 0 {
                self.ram[oam_ptr & 0x1FFF] = spr_x as u8;
                self.ram[(oam_ptr + 1) & 0x1FFF] = spr_y as u8;
                self.ram[(oam_ptr + 2) & 0x1FFF] = spr_name;
                self.ram[(oam_ptr + 3) & 0x1FFF] = spr_attr;

                let p2 = oam_ptr2 & 0x1FFF;
                self.ram[p2] &= !(3u8 << offset);
                if spr_x & 0x100 != 0 {
                    self.ram[p2] |= 3u8 << offset;
                } else {
                    self.ram[p2] |= 2u8 << offset;
                }

                oam_ptr += 4;
                spr_count -= 1;
                offset = (offset + 2) & 6;
                if offset == 0 {
                    oam_ptr2 += 1;
                }
            }

            srcptr += 16;
            i -= 1;
        }
    }

    /// `$00:$03`/`$00:$07` `C4DoScaleRotate`: affine scale+rotate a 4bpp tile
    /// bitmap (source `$6600+`) into de-bitplaned output planes.
    ///
    /// `⚠` UNVERIFIED (snes9x `XXX:` comments): the exact matrix for the
    /// axis-aligned rotation special cases and the center assumptions.
    pub(super) fn do_scale_rotate(&mut self, _rom: &[u8], row_padding: i32) {
        let mut x_scale = self.read_word(0x1F8F) as i32;
        if x_scale & 0x8000 != 0 {
            x_scale = 0x7FFF;
        }
        let mut y_scale = self.read_word(0x1F92) as i32;
        if y_scale & 0x8000 != 0 {
            y_scale = 0x7FFF;
        }

        let angle = self.read_word(0x1F80);
        let (a, b, c, d): (i16, i16, i16, i16) = match angle {
            0 => (x_scale as i16, 0, 0, y_scale as i16),
            128 => (0, (-y_scale) as i16, x_scale as i16, 0),
            256 => ((-x_scale) as i16, 0, 0, (-y_scale) as i16),
            384 => (0, y_scale as i16, (-x_scale) as i16, 0),
            _ => {
                let idx = (angle & 0x1FF) as usize;
                (
                    sar(COS_TABLE[idx] as i32 * x_scale, 15) as i16,
                    (-sar(SIN_TABLE[idx] as i32 * y_scale, 15)) as i16,
                    sar(SIN_TABLE[idx] as i32 * x_scale, 15) as i16,
                    sar(COS_TABLE[idx] as i32 * y_scale, 15) as i16,
                )
            }
        };

        let w = (self.ram[0x1F89] & !7) as i32;
        let h = (self.ram[0x1F8C] & !7) as i32;

        let clear = ((w + row_padding / 4) * h / 2).clamp(0, 0x2000) as usize;
        for byte in self.ram[0..clear].iter_mut() {
            *byte = 0;
        }

        let cx = self.read_word(0x1F83) as i16 as i32;
        let cy = self.read_word(0x1F86) as i16 as i32;

        let mut line_x = (cx << 12) - cx * a as i32 - cx * b as i32;
        let mut line_y = (cy << 12) - cy * c as i32 - cy * d as i32;

        let mut outidx: i32 = 0;
        let mut bit: u8 = 0x80;

        for _y in 0..h {
            let mut xx = line_x as u32;
            let mut yy = line_y as u32;

            for _x in 0..w {
                let byte = if (xx >> 12) >= w as u32 || (yy >> 12) >= h as u32 {
                    0u8
                } else {
                    let addr = (yy >> 12) * w as u32 + (xx >> 12);
                    let mut b = self.ram[(0x600 + (addr >> 1) as usize) & 0x1FFF];
                    if addr & 1 != 0 {
                        b >>= 4;
                    }
                    b
                };

                let o = outidx as usize & 0x1FFF;
                if byte & 1 != 0 {
                    self.ram[o] |= bit;
                }
                if byte & 2 != 0 {
                    self.ram[(o + 1) & 0x1FFF] |= bit;
                }
                if byte & 4 != 0 {
                    self.ram[(o + 16) & 0x1FFF] |= bit;
                }
                if byte & 8 != 0 {
                    self.ram[(o + 17) & 0x1FFF] |= bit;
                }

                bit >>= 1;
                if bit == 0 {
                    bit = 0x80;
                    outidx += 32;
                }

                xx = xx.wrapping_add(a as u32);
                yy = yy.wrapping_add(c as u32);
            }

            outidx += 2 + row_padding;
            if outidx & 0x10 != 0 {
                outidx &= !0x10;
            } else {
                outidx -= w * 4 + row_padding;
            }

            line_x = line_x.wrapping_add(b as i32);
            line_y = line_y.wrapping_add(d as i32);
        }
    }

    /// `$00:$05` `C4TransformLines`: rotate/project a vertex list and build the
    /// line table.
    pub(super) fn transform_lines(&mut self) {
        self.wf_x2 = self.ram[0x1F83] as i16;
        self.wf_y2 = self.ram[0x1F86] as i16;
        self.wf_dist = self.ram[0x1F89] as i16;
        self.wf_scale = self.ram[0x1F8C] as i16;

        let count = self.read_word(0x1F80) as i32;
        let mut ptr = 0usize;
        for _ in 0..count {
            self.wf_x = self.read_word(ptr + 1) as i16;
            self.wf_y = self.read_word(ptr + 5) as i16;
            self.wf_z = self.read_word(ptr + 9) as i16;
            self.transf_wireframe();
            self.write_word(ptr + 1, self.wf_x.wrapping_add(0x80) as u16);
            self.write_word(ptr + 5, self.wf_y.wrapping_add(0x50) as u16);
            ptr += 0x10;
        }

        self.write_word(0x600, 23);
        self.write_word(0x602, 0x60);
        self.write_word(0x605, 0x40);
        self.write_word(0x600 + 8, 23);
        self.write_word(0x602 + 8, 0x60);
        self.write_word(0x605 + 8, 0x40);

        let count = self.read_word(0xB00) as i32;
        let mut ptr = 0xB02usize;
        let mut ptr2 = 0usize;
        for _ in 0..count {
            let a = self.ram[ptr] as usize;
            let bidx = self.ram[ptr + 1] as usize;
            self.wf_x = self.read_word((a << 4) + 1) as i16;
            self.wf_y = self.read_word((a << 4) + 5) as i16;
            self.wf_x2 = self.read_word((bidx << 4) + 1) as i16;
            self.wf_y2 = self.read_word((bidx << 4) + 5) as i16;
            self.calc_wireframe();

            let dist = if self.wf_dist != 0 { self.wf_dist } else { 1 };
            self.write_word(ptr2 + 0x600, dist as u16);
            self.write_word(ptr2 + 0x602, self.wf_x as u16);
            self.write_word(ptr2 + 0x605, self.wf_y as u16);
            ptr += 2;
            ptr2 += 8;
        }
    }

    /// `$01` / `$00:$08` `C4DrawWireFrame`: draw all model edges from a ROM line
    /// table into the render planes at `$6300+`.
    ///
    /// `⚠` UNVERIFIED: the `$FFFF` back-scan reuse loop is bounded here to avoid
    /// runaway ROM reads.
    pub(super) fn draw_wireframe(&mut self, rom: &[u8]) {
        let mut line = self.read_3word(0x1F80);
        let count = self.ram[0x0295] as i32;
        let point_bank = (self.ram[0x1F82] as u32) << 16;

        for _ in 0..count {
            let l0 = rom_read(rom, line, 0);
            let l1 = rom_read(rom, line, 1);
            let l2 = rom_read(rom, line, 2);
            let l3 = rom_read(rom, line, 3);
            let color = rom_read(rom, line, 4);

            let point1 = if l0 == 0xFF && l1 == 0xFF {
                let mut tmp = line.wrapping_sub(5);
                let mut guard = 0;
                while rom_read(rom, tmp, 2) == 0xFF && rom_read(rom, tmp, 3) == 0xFF && guard < 256 {
                    tmp = tmp.wrapping_sub(5);
                    guard += 1;
                }
                point_bank | ((rom_read(rom, tmp, 2) as u32) << 8) | rom_read(rom, tmp, 3) as u32
            } else {
                point_bank | ((l0 as u32) << 8) | l1 as u32
            };
            let point2 = point_bank | ((l2 as u32) << 8) | l3 as u32;

            let x1 = ((rom_read(rom, point1, 0) as u16) << 8 | rom_read(rom, point1, 1) as u16) as i16;
            let y1 = ((rom_read(rom, point1, 2) as u16) << 8 | rom_read(rom, point1, 3) as u16) as i16;
            let z1 = ((rom_read(rom, point1, 4) as u16) << 8 | rom_read(rom, point1, 5) as u16) as i16;
            let x2 = ((rom_read(rom, point2, 0) as u16) << 8 | rom_read(rom, point2, 1) as u16) as i16;
            let y2 = ((rom_read(rom, point2, 2) as u16) << 8 | rom_read(rom, point2, 3) as u16) as i16;
            let z2 = ((rom_read(rom, point2, 4) as u16) << 8 | rom_read(rom, point2, 5) as u16) as i16;

            self.draw_line(x1 as i32, y1 as i32, z1, x2 as i32, y2 as i32, z2, color);
            line = line.wrapping_add(5);
        }
    }

    /// `C4DrawLine`: transform both endpoints, offset by +48, step with
    /// `C4CalcWireFrame`, plot into the two 1bpp planes at `$6300`/`$6301`.
    #[allow(clippy::too_many_arguments)]
    fn draw_line(&mut self, x1: i32, y1: i32, z1: i16, x2: i32, y2: i32, z2: i16, color: u8) {
        self.wf_x = x1 as i16;
        self.wf_y = y1 as i16;
        self.wf_z = z1;
        self.wf_scale = self.ram[0x1F90] as i16;
        self.wf_x2 = self.ram[0x1F86] as i16;
        self.wf_y2 = self.ram[0x1F87] as i16;
        self.wf_dist = self.ram[0x1F88] as i16;
        self.transf_wireframe2();
        let mut x1 = ((self.wf_x as i32) + 48) << 8;
        let mut y1 = ((self.wf_y as i32) + 48) << 8;

        self.wf_x = x2 as i16;
        self.wf_y = y2 as i16;
        self.wf_z = z2;
        self.transf_wireframe2();
        let x2b = ((self.wf_x as i32) + 48) << 8;
        let y2b = ((self.wf_y as i32) + 48) << 8;

        self.wf_x = (x1 >> 8) as i16;
        self.wf_y = (y1 >> 8) as i16;
        self.wf_x2 = (x2b >> 8) as i16;
        self.wf_y2 = (y2b >> 8) as i16;
        self.calc_wireframe();
        let step_x = self.wf_x as i32;
        let step_y = self.wf_y as i32;

        let mut i = if self.wf_dist != 0 { self.wf_dist as i32 } else { 1 };
        while i > 0 {
            if x1 > 0xFF && y1 > 0xFF && x1 < 0x6000 && y1 < 0x6000 {
                let ys = (y1 >> 8) as u32;
                let xs = (x1 >> 8) as u32;
                let addr = ((((ys >> 3) << 8) - ((ys >> 3) << 6) + ((xs >> 3) << 4) + (ys & 7) * 2)
                    & 0xFFFF) as usize;
                let bit = 0x80u8 >> (xs & 7);

                self.ram[(addr + 0x300) & 0x1FFF] &= !bit;
                self.ram[(addr + 0x301) & 0x1FFF] &= !bit;
                if color & 1 != 0 {
                    self.ram[(addr + 0x300) & 0x1FFF] |= bit;
                }
                if color & 2 != 0 {
                    self.ram[(addr + 0x301) & 0x1FFF] |= bit;
                }
            }
            x1 += step_x;
            y1 += step_y;
            i -= 1;
        }
    }

    /// `$00:$0C` `C4BitPlaneWave`: sinusoidal bitplane distortion from the height
    /// table at `$6A00`/`$6A10`, source `$6B00`.
    pub(super) fn bit_plane_wave(&mut self) {
        const BMPDATA: [usize; 40] = [
            0x0000, 0x0002, 0x0004, 0x0006, 0x0008, 0x000A, 0x000C, 0x000E, 0x0200, 0x0202, 0x0204,
            0x0206, 0x0208, 0x020A, 0x020C, 0x020E, 0x0400, 0x0402, 0x0404, 0x0406, 0x0408, 0x040A,
            0x040C, 0x040E, 0x0600, 0x0602, 0x0604, 0x0606, 0x0608, 0x060A, 0x060C, 0x060E, 0x0800,
            0x0802, 0x0804, 0x0806, 0x0808, 0x080A, 0x080C, 0x080E,
        ];

        let mut dst = 0usize;
        let mut waveptr = self.ram[0x1F83] as usize;
        let mut mask1: u16 = 0xC0C0;
        let mut mask2: u16 = 0x3F3F;

        for _j in 0..0x10 {
            for src_off in [0xA00usize, 0xA10usize] {
                loop {
                    let mut height = -(self.ram[waveptr + 0xB00] as i8 as i16) - 16;
                    for &bd in BMPDATA.iter() {
                        let idx = (dst + bd) & 0x1FFF;
                        let mut tmp = ((self.ram[idx] as u16) | ((self.ram[idx + 1] as u16) << 8)) & mask2;
                        if height >= 0 {
                            if height < 8 {
                                let ho = (src_off + height as usize * 2) & 0x1FFF;
                                let hv = (self.ram[ho] as u16) | ((self.ram[ho + 1] as u16) << 8);
                                tmp |= mask1 & hv;
                            } else {
                                tmp |= mask1 & 0xFF00;
                            }
                        }
                        self.ram[idx] = tmp as u8;
                        self.ram[idx + 1] = (tmp >> 8) as u8;
                        height += 1;
                    }
                    waveptr = (waveptr + 1) & 0x7F;
                    mask1 = (mask1 >> 2) | (mask1 << 6);
                    mask2 = (mask2 >> 2) | (mask2 << 6);
                    if mask1 == 0xC0C0 {
                        break;
                    }
                }
                dst += 16;
            }
        }
    }

    /// `$00:$0B` `C4SprDisintegrate`: per-axis scale of a tile toward/away for the
    /// disintegration effect.
    pub(super) fn spr_disintegrate(&mut self) {
        let width = self.ram[0x1F89] as i32;
        let height = self.ram[0x1F8C] as i32;
        let cx = self.read_word(0x1F80) as i16 as i32;
        let cy = self.read_word(0x1F83) as i16 as i32;

        let scale_x = self.read_word(0x1F86) as i16 as i32;
        let scale_y = self.read_word(0x1F8F) as i16 as i32;
        let start_x = (-cx * scale_x + (cx << 8)) as u32;
        let start_y = (-cy * scale_y + (cy << 8)) as u32;

        let clear = (width * height / 2).clamp(0, 0x2000) as usize;
        for byte in self.ram[0..clear].iter_mut() {
            *byte = 0;
        }

        let mut src = 0x600usize;
        let mut y = start_y;
        for _i in 0..height {
            let mut x = start_x;
            for j in 0..width {
                if (x >> 8) < width as u32
                    && (y >> 8) < height as u32
                    && (y >> 8) * width as u32 + (x >> 8) < 0x2000
                {
                    let pixel = if j & 1 != 0 {
                        self.ram[src & 0x1FFF] >> 4
                    } else {
                        self.ram[src & 0x1FFF]
                    };
                    let idx = ((y >> 11) * width as u32 * 4
                        + (x >> 11) * 32
                        + ((y >> 8) & 7) * 2) as usize
                        & 0x1FFF;
                    let mask = 0x80u8 >> (x >> 8 & 7);

                    if pixel & 1 != 0 {
                        self.ram[idx] |= mask;
                    }
                    if pixel & 2 != 0 {
                        self.ram[(idx + 1) & 0x1FFF] |= mask;
                    }
                    if pixel & 4 != 0 {
                        self.ram[(idx + 16) & 0x1FFF] |= mask;
                    }
                    if pixel & 8 != 0 {
                        self.ram[(idx + 17) & 0x1FFF] |= mask;
                    }
                }
                if j & 1 != 0 {
                    src += 1;
                }
                x = x.wrapping_add(scale_x as u32);
            }
            y = y.wrapping_add(scale_y as u32);
        }
    }
}
