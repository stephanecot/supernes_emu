//! DSP-1 HLE command math, transcribed from the public snes9x `dsp1.cpp`
//! reverse-engineered command set. All fixed-point widths and truncation match
//! the reference: every `int16 * int16 >> 15` is evaluated in 32-bit and stored
//! back to 16-bit (truncation), sums that can exceed 32 bits use 64-bit
//! intermediates cast back to i32 before the shift (reproducing the wrap point
//! of C's `int`). No math is invented here.

use super::tables::{DSP1_MUL_TABLE, DSP1_ROM, DSP1_SIN_TABLE};
use super::Dsp1;

#[inline]
fn q15(a: i32, b: i32) -> i32 {
    (a * b) >> 15
}

/// Q15 sine of a 16-bit angle (full circle = $10000).
pub(super) fn dsp1_sin(angle: i16) -> i16 {
    if angle < 0 {
        if angle == -32768 {
            return 0;
        }
        return -dsp1_sin(-angle);
    }
    let s = DSP1_SIN_TABLE[(angle >> 8) as usize] as i32
        + ((DSP1_MUL_TABLE[(angle & 0xff) as usize] as i32
            * DSP1_SIN_TABLE[0x40 + (angle >> 8) as usize] as i32)
            >> 15);
    (if s > 32767 { 32767 } else { s }) as i16
}

/// Q15 cosine of a 16-bit angle.
pub(super) fn dsp1_cos(mut angle: i16) -> i16 {
    if angle < 0 {
        if angle == -32768 {
            return -32768;
        }
        angle = -angle;
    }
    let s = DSP1_SIN_TABLE[0x40 + (angle >> 8) as usize] as i32
        - ((DSP1_MUL_TABLE[(angle & 0xff) as usize] as i32
            * DSP1_SIN_TABLE[(angle >> 8) as usize] as i32)
            >> 15);
    (if s < -32768 { -32767 } else { s }) as i16
}

/// Float reciprocal `1/(coefficient * 2^exponent)`, normalized. Returns
/// (iCoefficient Q15, iExponent).
pub(super) fn inverse(mut coefficient: i16, mut exponent: i16) -> (i16, i16) {
    if coefficient == 0 {
        return (0x7fff, 0x002f);
    }

    let mut sign: i16 = 1;
    if coefficient < 0 {
        if coefficient < -32767 {
            coefficient = -32767;
        }
        coefficient = -coefficient;
        sign = -1;
    }

    while coefficient < 0x4000 {
        coefficient <<= 1;
        exponent = exponent.wrapping_sub(1);
    }

    let icoef;
    if coefficient == 0x4000 {
        if sign == 1 {
            icoef = 0x7fff;
        } else {
            icoef = -0x4000;
            exponent = exponent.wrapping_sub(1);
        }
    } else {
        let mut iv: i16 = DSP1_ROM[(((coefficient - 0x4000) >> 7) as usize) + 0x0065] as i16;
        iv = ((iv as i32 + ((-(iv as i32) * q15(coefficient as i32, iv as i32)) >> 15)) << 1) as i16;
        iv = ((iv as i32 + ((-(iv as i32) * q15(coefficient as i32, iv as i32)) >> 15)) << 1) as i16;
        icoef = (iv as i32 * sign as i32) as i16;
    }

    (icoef, 1i16.wrapping_sub(exponent))
}

/// Normalize a Q15 value into `[$4000,$7FFF]`, returning (coefficient, exponent
/// adjusted from the supplied value).
pub(super) fn normalize(m: i16, exponent: i16) -> (i16, i16) {
    let mut e: i16 = 0;
    let mut i: i16 = 0x4000;
    if m < 0 {
        while (m & i) != 0 && i != 0 {
            i >>= 1;
            e += 1;
        }
    } else {
        while (m & i) == 0 && i != 0 {
            i >>= 1;
            e += 1;
        }
    }

    let coef = if e > 0 {
        ((m as i32 * DSP1_ROM[(0x21 + e as i32) as usize] as i32) << 1) as i16
    } else {
        m
    };
    (coef, exponent.wrapping_sub(e))
}

/// Normalize a 32-bit product. Returns (coefficient Q15, exponent).
pub(super) fn normalize_double(product: i32) -> (i16, i16) {
    let n: i16 = (product & 0x7fff) as i16;
    let m: i16 = (product >> 15) as i16;
    let mut e: i16 = 0;
    let mut i: i16 = 0x4000;
    if m < 0 {
        while (m & i) != 0 && i != 0 {
            i >>= 1;
            e += 1;
        }
    } else {
        while (m & i) == 0 && i != 0 {
            i >>= 1;
            e += 1;
        }
    }

    let mut coef: i32;
    if e > 0 {
        coef = (m as i32 * DSP1_ROM[(0x0021 + e as i32) as usize] as i32) << 1;
        if e < 15 {
            coef += (n as i32 * DSP1_ROM[(0x0040 - e as i32) as usize] as i32) >> 15;
        } else {
            i = 0x4000;
            if m < 0 {
                while (n & i) != 0 && i != 0 {
                    i >>= 1;
                    e += 1;
                }
            } else {
                while (n & i) == 0 && i != 0 {
                    i >>= 1;
                    e += 1;
                }
            }
            if e > 15 {
                coef = (n as i32 * DSP1_ROM[(0x0012 + e as i32) as usize] as i32) << 1;
            } else {
                coef += n as i32;
            }
        }
    } else {
        coef = m as i32;
    }

    (coef as i16, e)
}

/// Denormalize a Q15 coefficient by exponent, saturating on overflow.
pub(super) fn truncate(c: i16, e: i16) -> i16 {
    if e > 0 {
        if c > 0 {
            return 32767;
        }
        if c < 0 {
            return -32767;
        }
    } else if e < 0 {
        let idx = (0x31 + e as i32).clamp(0, 1023) as usize;
        return ((c as i32 * DSP1_ROM[idx] as i32) >> 15) as i16;
    }
    c
}

fn shift_r(c: i16, e: i16) -> i16 {
    // `0x31 + e` can go out of range for extreme exponents; snes9x reads OOB
    // garbage there rather than crashing, so clamp to the table bounds.
    let idx = (0x31 + e as i32).clamp(0, 1023) as usize;
    ((c as i32 * DSP1_ROM[idx] as i32) >> 15) as i16
}

/// Build a ZYX Euler attitude matrix (Q15) with the scale `m0` pre-halved.
fn attitude_matrix(m0: i16, zr: i16, yr: i16, xr: i16) -> [[i16; 3]; 3] {
    let sz = dsp1_sin(zr) as i32;
    let cz = dsp1_cos(zr) as i32;
    let sy = dsp1_sin(yr) as i32;
    let cy = dsp1_cos(yr) as i32;
    let sx = dsp1_sin(xr) as i32;
    let cx = dsp1_cos(xr) as i32;
    let m = (m0 >> 1) as i32;

    let mut a = [[0i16; 3]; 3];
    a[0][0] = q15(q15(m, cz), cy) as i16;
    a[0][1] = (-q15(q15(m, sz), cy)) as i16;
    a[0][2] = q15(m, sy) as i16;

    a[1][0] = (q15(q15(m, sz), cx) + q15(q15(q15(m, cz), sx), sy)) as i16;
    a[1][1] = (q15(q15(m, cz), cx) - q15(q15(q15(m, sz), sx), sy)) as i16;
    a[1][2] = (-q15(q15(m, sx), cy)) as i16;

    a[2][0] = (q15(q15(m, sz), sx) - q15(q15(q15(m, cz), cx), sy)) as i16;
    a[2][1] = (q15(q15(m, cz), sx) + q15(q15(q15(m, sz), cx), sy)) as i16;
    a[2][2] = q15(q15(m, cx), cy) as i16;
    a
}

const MAX_AZS_EXP: [i16; 16] = [
    0x38b4, 0x38b7, 0x38ba, 0x38be, 0x38c0, 0x38c4, 0x38c7, 0x38ca, 0x38ce, 0x38d0, 0x38d4, 0x38d7,
    0x38da, 0x38dd, 0x38e0, 0x38e4,
];

impl Dsp1 {
    #[inline]
    fn p(&self, i: usize) -> i16 {
        (self.parameters[i] as u16 | ((self.parameters[i + 1] as u16) << 8)) as i16
    }

    pub(super) fn set_out(&mut self, words: &[i16]) {
        for (i, w) in words.iter().enumerate() {
            let u = *w as u16;
            self.output[i * 2] = (u & 0xff) as u8;
            self.output[i * 2 + 1] = (u >> 8) as u8;
        }
        self.out_count = (words.len() * 2) as i32;
    }

    /// Op02 Parameter: establish the projection camera. Returns (Vof, Vva, Cx, Cy).
    fn parameter(&mut self) -> [i16; 4] {
        let fx = self.p(0);
        let fy = self.p(2);
        let fz = self.p(4);
        let lfe = self.p(6);
        let les = self.p(8);
        let aas = self.p(10);
        let azs = self.p(12);

        let mut azs_c = azs;

        self.sin_aas = dsp1_sin(aas);
        self.cos_aas = dsp1_cos(aas);
        self.sin_azs = dsp1_sin(azs);
        self.cos_azs = dsp1_cos(azs);

        self.nx = q15(self.sin_azs as i32, -(self.sin_aas as i32)) as i16;
        self.ny = q15(self.sin_azs as i32, self.cos_aas as i32) as i16;
        self.nz = q15(self.cos_azs as i32, 0x7fff) as i16;

        let lfe_nx = q15(lfe as i32, self.nx as i32) as i16;
        let lfe_ny = q15(lfe as i32, self.ny as i32) as i16;
        let lfe_nz = q15(lfe as i32, self.nz as i32) as i16;

        self.centre_x = (fx as i32 + lfe_nx as i32) as i16;
        self.centre_y = (fy as i32 + lfe_ny as i32) as i16;
        let centre_z = (fz as i32 + lfe_nz as i32) as i16;

        let les_nx = q15(les as i32, self.nx as i32) as i16;
        let les_ny = q15(les as i32, self.ny as i32) as i16;
        let les_nz = q15(les as i32, self.nz as i32) as i16;

        self.gx = (self.centre_x as i32 - les_nx as i32) as i16;
        self.gy = (self.centre_y as i32 - les_ny as i32) as i16;
        self.gz = (centre_z as i32 - les_nz as i32) as i16;

        let (c_les, e_les) = normalize(les, 0);
        self.c_les = c_les;
        self.e_les = e_les;
        self.g_les = les;

        let (mut c, e) = normalize(centre_z, 0);
        self.vplane_c = c;
        self.vplane_e = e;

        let mut max_azs = MAX_AZS_EXP[(-e) as usize];

        if azs_c < 0 {
            max_azs = -max_azs;
            if azs_c < max_azs + 1 {
                azs_c = max_azs + 1;
            }
        } else if azs_c > max_azs {
            azs_c = max_azs;
        }

        self.sin_azs_clip = dsp1_sin(azs_c);
        self.cos_azs_clip = dsp1_cos(azs_c);

        let (sc1, se1) = inverse(self.cos_azs_clip, 0);
        self.secazs_c1 = sc1;
        self.secazs_e1 = se1;

        let (c2, mut e2) = normalize(q15(c as i32, self.secazs_c1 as i32) as i16, e);
        c = c2;
        e2 = e2.wrapping_add(self.secazs_e1);

        c = q15(truncate(c, e2) as i32, self.sin_azs_clip as i32) as i16;

        self.centre_x = (self.centre_x as i32 + q15(c as i32, self.sin_aas as i32)) as i16;
        self.centre_y = (self.centre_y as i32 - q15(c as i32, self.cos_aas as i32)) as i16;

        let cx = self.centre_x;
        let cy = self.centre_y;
        let mut vof: i16 = 0;

        if azs != azs_c || azs == max_azs {
            let mut azs2 = azs;
            if azs2 == -32768 {
                azs2 = -32767;
            }
            let mut cc = azs2 - max_azs;
            if cc >= 0 {
                cc -= 1;
            }
            let aux = (!((cc as i32) << 2)) as i16;

            cc = q15(aux as i32, DSP1_ROM[0x0328] as i16 as i32) as i16;
            cc = (q15(cc as i32, aux as i32) + DSP1_ROM[0x0327] as i16 as i32) as i16;
            vof = (vof as i32 - q15(q15(cc as i32, aux as i32), les as i32)) as i16;

            cc = q15(aux as i32, aux as i32) as i16;
            let aux2 = (q15(cc as i32, DSP1_ROM[0x0324] as i16 as i32) + DSP1_ROM[0x0325] as i16 as i32) as i16;
            self.cos_azs_clip = (self.cos_azs_clip as i32
                + q15(q15(cc as i32, aux2 as i32), self.cos_azs_clip as i32)) as i16;
        }

        self.voffset = q15(les as i32, self.cos_azs_clip as i32) as i16;

        let (csec, e0) = inverse(self.sin_azs_clip, 0);
        let (c3, e3) = normalize(self.voffset, e0);
        let (mut c4, mut e4) = normalize(q15(c3 as i32, csec as i32) as i16, e3);
        if c4 == -32768 {
            c4 >>= 1;
            e4 = e4.wrapping_add(1);
        }
        let vva = truncate(c4.wrapping_neg(), e4);

        let (sc2, se2) = inverse(self.cos_azs_clip, 0);
        self.secazs_c2 = sc2;
        self.secazs_e2 = se2;

        [vof, vva, cx, cy]
    }

    /// Op0A Raster: Mode-7 affine coefficients A,B,C,D for scanline `vs`.
    pub(super) fn raster(&self, vs: i16) -> [i16; 4] {
        let arg = (q15(vs as i32, self.sin_azs as i32) + self.voffset as i32) as i16;
        let (c0, e0) = inverse(arg, 7);
        let e = e0.wrapping_add(self.vplane_e);

        let c1 = q15(c0 as i32, self.vplane_c as i32) as i16;
        let e1 = e.wrapping_add(self.secazs_e2);

        let (c, e2) = normalize(c1, e);
        let c = truncate(c, e2);
        let an = q15(c as i32, self.cos_aas as i32) as i16;
        let cn = q15(c as i32, self.sin_aas as i32) as i16;

        let (cb, e1b) = normalize(q15(c1 as i32, self.secazs_c2 as i32) as i16, e1);
        let cb = truncate(cb, e1b);
        let bn = q15(cb as i32, -(self.sin_aas as i32)) as i16;
        let dn = q15(cb as i32, self.cos_aas as i32) as i16;

        [an, bn, cn, dn]
    }

    /// Op06 Project: 3D world point -> screen (H, V, M).
    fn project(&self, x: i16, y: i16, z: i16) -> [i16; 3] {
        let (mut px, mut e4) = normalize_double(x as i32 - self.gx as i32);
        let (mut py, mut e) = normalize_double(y as i32 - self.gy as i32);
        let (mut pz, mut e3) = normalize_double(z as i32 - self.gz as i32);
        px >>= 1;
        e4 = e4.wrapping_sub(1);
        py >>= 1;
        e = e.wrapping_sub(1);
        pz >>= 1;
        e3 = e3.wrapping_sub(1);

        let mut refe = if e < e3 { e } else { e3 };
        refe = if refe < e4 { refe } else { e4 };

        px = shift_r(px, e4 - refe);
        py = shift_r(py, e - refe);
        pz = shift_r(pz, e3 - refe);

        let c11 = -q15(px as i32, self.nx as i32);
        let c8 = -q15(py as i32, self.ny as i32);
        let c9 = -q15(pz as i32, self.nz as i32);
        let c12 = c11 + c8 + c9;

        let mut aux4: i64 = c12 as i64;
        refe = 16 - refe;
        if refe >= 0 {
            aux4 <<= refe as u32;
        } else {
            aux4 >>= (-refe) as u32;
        }
        if aux4 == -1 {
            aux4 = 0;
        }
        aux4 >>= 1;

        let aux = self.g_les as u16 as i64 + aux4;
        let (c10, e2n) = normalize_double(aux as i32);
        let e2 = 15i16.wrapping_sub(e2n);

        let (c4b, _e4b) = inverse(c10, 0);
        let c2 = q15(c4b as i32, self.c_les as i32) as i16;

        // H
        let c16 = q15(px as i32, q15(self.cos_aas as i32, 0x7fff));
        let c20 = q15(py as i32, q15(self.sin_aas as i32, 0x7fff));
        let c17 = (c16 + c20) as i16;
        let c18 = q15(c17 as i32, c2 as i32) as i16;
        let (c19, e7) = normalize(c18, 0);
        let h = truncate(
            c19,
            self.e_les.wrapping_sub(e2).wrapping_add(refe).wrapping_add(e7),
        );

        // V
        let c21 = q15(px as i32, q15(self.cos_azs as i32, -(self.sin_aas as i32)));
        let c22 = q15(py as i32, q15(self.cos_azs as i32, self.cos_aas as i32));
        let c23 = q15(pz as i32, q15(-(self.sin_azs as i32), 0x7fff));
        let c24 = (c21 + c22 + c23) as i16;
        let c26 = q15(c24 as i32, c2 as i32) as i16;
        let (c25, e6) = normalize(c26, 0);
        let v = truncate(
            c25,
            self.e_les.wrapping_sub(e2).wrapping_add(refe).wrapping_add(e6),
        );

        // M
        let (c6, e4c) = normalize(c2, 0);
        let m = truncate(
            c6,
            e4c.wrapping_add(self.e_les).wrapping_sub(e2).wrapping_sub(7),
        );

        [h, v, m]
    }

    /// Op0E Target: screen (H, V) -> world (X, Y) on the target plane.
    fn target(&self, h: i16, v: i16) -> [i16; 2] {
        let arg = (q15(v as i32, self.sin_azs as i32) + self.voffset as i32) as i16;
        let (c0, e0) = inverse(arg, 8);
        let e = e0.wrapping_add(self.vplane_e);

        let c1 = q15(c0 as i32, self.vplane_c as i32) as i16;
        let e1 = e.wrapping_add(self.secazs_e1);

        let h8 = h << 8;
        let (cc, e2) = normalize(c1, e);
        let c = q15(truncate(cc, e2) as i32, h8 as i32) as i16;
        let mut x = (self.centre_x as i32 + q15(c as i32, self.cos_aas as i32)) as i16;
        let mut y = (self.centre_y as i32 - q15(c as i32, self.sin_aas as i32)) as i16;

        let v8 = v << 8;
        let (cc2, e1b) = normalize(q15(c1 as i32, self.secazs_c1 as i32) as i16, e1);
        let c = q15(truncate(cc2, e1b) as i32, v8 as i32) as i16;
        x = (x as i32 + q15(c as i32, -(self.sin_aas as i32))) as i16;
        y = (y as i32 + q15(c as i32, self.cos_aas as i32)) as i16;

        [x, y]
    }

    /// Op14 Gyrate: integrate attitude angles by an angular-velocity vector.
    fn gyrate(&self) -> [i16; 3] {
        let zr = self.p(0);
        let xr = self.p(2);
        let yr = self.p(4);
        let u = self.p(6);
        let f = self.p(8);
        let l = self.p(10);

        let (csec, esec) = inverse(dsp1_cos(xr), 0);

        // Rotation around Z
        let (c0, e0) =
            normalize_double(u as i32 * dsp1_cos(yr) as i32 - f as i32 * dsp1_sin(yr) as i32);
        let ez = esec.wrapping_sub(e0);
        let (cz, ezf) = normalize(q15(c0 as i32, csec as i32) as i16, ez);
        let zrr = zr.wrapping_add(truncate(cz, ezf));

        // Rotation around X
        let xrr = (xr as i32
            + q15(u as i32, dsp1_sin(yr) as i32)
            + q15(f as i32, dsp1_cos(yr) as i32)) as i16;

        // Rotation around Y
        let (c1, e1) =
            normalize_double(u as i32 * dsp1_cos(yr) as i32 + f as i32 * dsp1_sin(yr) as i32);
        let ey = esec.wrapping_sub(e1);
        let (csin, ey2) = normalize(dsp1_sin(xr), ey);
        let ctan = q15(csec as i32, csin as i32) as i16;
        let (cy, ey3) = normalize((-q15(c1 as i32, ctan as i32)) as i16, ey2);
        let yrr = (yr as i32 + truncate(cy, ey3) as i32 + l as i32) as i16;

        [zrr, xrr, yrr]
    }

    /// Op1C Polar: rotate a vector by Z, Y, X angles (three sequential 2D rotations).
    fn polar(&self) -> [i16; 3] {
        let za = self.p(0);
        let ya = self.p(2);
        let xa = self.p(4);
        let mut xbr = self.p(6);
        let mut ybr = self.p(8);
        let mut zbr = self.p(10);

        let sz = dsp1_sin(za) as i32;
        let cz = dsp1_cos(za) as i32;
        let x1 = (q15(ybr as i32, sz) + q15(xbr as i32, cz)) as i16;
        let y1 = (q15(ybr as i32, cz) - q15(xbr as i32, sz)) as i16;
        xbr = x1;
        ybr = y1;

        let sy = dsp1_sin(ya) as i32;
        let cy = dsp1_cos(ya) as i32;
        let z1 = (q15(xbr as i32, sy) + q15(zbr as i32, cy)) as i16;
        let xar = (q15(xbr as i32, cy) - q15(zbr as i32, sy)) as i16;
        zbr = z1;

        let sx = dsp1_sin(xa) as i32;
        let cx = dsp1_cos(xa) as i32;
        let yar = (q15(zbr as i32, sx) + q15(ybr as i32, cx)) as i16;
        let zar = (q15(zbr as i32, cx) - q15(ybr as i32, sx)) as i16;

        [xar, yar, zar]
    }

    /// Execute the currently-selected command against the collected parameters,
    /// buffering the result bytes (little-endian words) in `output`.
    pub(super) fn execute(&mut self) {
        self.out_count = 0;

        match self.command {
            0x1f => {
                self.out_count = 2048;
            }

            0x00 => {
                let r = q15(self.p(0) as i32, self.p(2) as i32) as i16;
                self.set_out(&[r]);
            }
            0x20 => {
                let r = (q15(self.p(0) as i32, self.p(2) as i32) as i16).wrapping_add(1);
                self.set_out(&[r]);
            }

            0x10 | 0x30 => {
                let (c, e) = inverse(self.p(0), self.p(2));
                self.set_out(&[c, e]);
            }

            0x04 | 0x24 => {
                let a = self.p(0);
                let r = self.p(2);
                let sin = q15(dsp1_sin(a) as i32, r as i32) as i16;
                let cos = q15(dsp1_cos(a) as i32, r as i32) as i16;
                self.set_out(&[sin, cos]);
            }

            0x08 => {
                let x = self.p(0) as i64;
                let y = self.p(2) as i64;
                let z = self.p(4) as i64;
                let size = ((x * x + y * y + z * z) << 1) as i32;
                let ll = (size & 0xffff) as i16;
                let lh = ((size >> 16) & 0xffff) as i16;
                self.set_out(&[ll, lh]);
            }

            0x18 => {
                let x = self.p(0) as i64;
                let y = self.p(2) as i64;
                let z = self.p(4) as i64;
                let r = self.p(6) as i64;
                let d = (((x * x + y * y + z * z - r * r) as i32) >> 15) as i16;
                self.set_out(&[d]);
            }
            0x38 => {
                let x = self.p(0) as i64;
                let y = self.p(2) as i64;
                let z = self.p(4) as i64;
                let r = self.p(6) as i64;
                let d = ((((x * x + y * y + z * z - r * r) as i32) >> 15) as i16).wrapping_add(1);
                self.set_out(&[d]);
            }

            0x28 => {
                let x = self.p(0) as i64;
                let y = self.p(2) as i64;
                let z = self.p(4) as i64;
                let radius = (x * x + y * y + z * z) as i32;
                let r = if radius == 0 {
                    0
                } else {
                    let (mut c, e) = normalize_double(radius);
                    if e & 1 != 0 {
                        c = q15(c as i32, 0x4000) as i16;
                    }
                    // For radii with bit30 set, `normalize_double` returns a
                    // negative coefficient (matching snes9x's `int16 m = Product>>15`
                    // wrap). snes9x then indexes DSP1ROM out of bounds and returns
                    // benign garbage; clamp to the sqrt table range so extreme,
                    // guest-controlled radii degrade to garbage instead of panicking.
                    let pos = q15(c as i32, 0x0040).clamp(0, 0x3f) as usize;
                    let node1 = DSP1_ROM[0x00d5 + pos] as i16;
                    let node2 = DSP1_ROM[0x00d6 + pos] as i16;
                    let r0 = (((node2 as i32 - node1 as i32) * ((c & 0x1ff) as i32) >> 9)
                        + node1 as i32) as i16;
                    r0 >> (e >> 1)
                };
                self.set_out(&[r]);
            }

            0x0c | 0x2c => {
                let a = self.p(0);
                let x1 = self.p(2);
                let y1 = self.p(4);
                let x2 = (q15(y1 as i32, dsp1_sin(a) as i32) + q15(x1 as i32, dsp1_cos(a) as i32)) as i16;
                let y2 = (q15(y1 as i32, dsp1_cos(a) as i32) - q15(x1 as i32, dsp1_sin(a) as i32)) as i16;
                self.set_out(&[x2, y2]);
            }

            0x1c | 0x3c => {
                let out = self.polar();
                self.set_out(&out);
            }

            0x02 | 0x12 | 0x22 | 0x32 => {
                let out = self.parameter();
                self.set_out(&out);
            }

            0x1a => {
                self.op0a_vs = self.p(0);
                let out = self.raster(self.op0a_vs);
                self.op0a_vs = self.op0a_vs.wrapping_add(1);
                self.set_out(&out);
            }
            0x0a => {
                self.op0a_vs = self.p(0);
                let out = self.raster(self.op0a_vs);
                self.op0a_vs = self.op0a_vs.wrapping_add(1);
                self.set_out(&out);
            }

            0x06 | 0x16 | 0x26 | 0x36 => {
                let out = self.project(self.p(0), self.p(2), self.p(4));
                self.set_out(&out);
            }

            0x0e | 0x1e | 0x2e | 0x3e => {
                let out = self.target(self.p(0), self.p(2));
                self.set_out(&out);
            }

            0x01 | 0x05 | 0x31 | 0x35 => {
                self.matrix_a = attitude_matrix(self.p(0), self.p(2), self.p(4), self.p(6));
            }
            0x11 | 0x15 => {
                self.matrix_b = attitude_matrix(self.p(0), self.p(2), self.p(4), self.p(6));
            }
            0x21 | 0x25 => {
                self.matrix_c = attitude_matrix(self.p(0), self.p(2), self.p(4), self.p(6));
            }

            0x0d | 0x09 | 0x39 | 0x3d => {
                let out = objective(&self.matrix_a, self.p(0), self.p(2), self.p(4));
                self.set_out(&out);
            }
            0x1d | 0x19 => {
                let out = objective(&self.matrix_b, self.p(0), self.p(2), self.p(4));
                self.set_out(&out);
            }
            0x2d | 0x29 => {
                let out = objective(&self.matrix_c, self.p(0), self.p(2), self.p(4));
                self.set_out(&out);
            }

            0x03 | 0x33 => {
                let out = subjective(&self.matrix_a, self.p(0), self.p(2), self.p(4));
                self.set_out(&out);
            }
            0x13 => {
                let out = subjective(&self.matrix_b, self.p(0), self.p(2), self.p(4));
                self.set_out(&out);
            }
            0x23 => {
                let out = subjective(&self.matrix_c, self.p(0), self.p(2), self.p(4));
                self.set_out(&out);
            }

            0x0b | 0x3b => {
                let s = scalar(&self.matrix_a, self.p(0), self.p(2), self.p(4));
                self.set_out(&[s]);
            }
            0x1b => {
                let s = scalar(&self.matrix_b, self.p(0), self.p(2), self.p(4));
                self.set_out(&[s]);
            }
            0x2b => {
                let s = scalar(&self.matrix_c, self.p(0), self.p(2), self.p(4));
                self.set_out(&[s]);
            }

            0x14 | 0x34 => {
                let out = self.gyrate();
                self.set_out(&out);
            }

            0x0f | 0x07 => {
                self.set_out(&[0x0000]);
            }
            0x2f | 0x27 => {
                self.set_out(&[0x0100]);
            }

            _ => {}
        }
    }
}

fn objective(m: &[[i16; 3]; 3], x: i16, y: i16, z: i16) -> [i16; 3] {
    let f = (q15(x as i32, m[0][0] as i32)
        + q15(y as i32, m[0][1] as i32)
        + q15(z as i32, m[0][2] as i32)) as i16;
    let l = (q15(x as i32, m[1][0] as i32)
        + q15(y as i32, m[1][1] as i32)
        + q15(z as i32, m[1][2] as i32)) as i16;
    let u = (q15(x as i32, m[2][0] as i32)
        + q15(y as i32, m[2][1] as i32)
        + q15(z as i32, m[2][2] as i32)) as i16;
    [f, l, u]
}

fn subjective(m: &[[i16; 3]; 3], f: i16, l: i16, u: i16) -> [i16; 3] {
    let x = (q15(f as i32, m[0][0] as i32)
        + q15(l as i32, m[1][0] as i32)
        + q15(u as i32, m[2][0] as i32)) as i16;
    let y = (q15(f as i32, m[0][1] as i32)
        + q15(l as i32, m[1][1] as i32)
        + q15(u as i32, m[2][1] as i32)) as i16;
    let z = (q15(f as i32, m[0][2] as i32)
        + q15(l as i32, m[1][2] as i32)
        + q15(u as i32, m[2][2] as i32)) as i16;
    [x, y, z]
}

fn scalar(m: &[[i16; 3]; 3], x: i16, y: i16, z: i16) -> i16 {
    let s = (x as i64 * m[0][0] as i64 + y as i64 * m[0][1] as i64 + z as i64 * m[0][2] as i64)
        as i32;
    (s >> 15) as i16
}
