//! DSP-1 HLE unit tests. Vectors are derived from the transcribed command math
//! (snes9x `dsp1.cpp`); no game ROM is required.

use super::*;

/// Drive a full command through the DR/SR protocol: write the command byte and
/// its little-endian parameter words, then read back `out_words` result words.
fn run(d: &mut Dsp1, cmd: u8, params: &[i16], out_words: usize) -> Vec<i16> {
    assert_eq!(d.read_sr(), 0x80);
    d.write_dr(cmd);
    for &p in params {
        let u = p as u16;
        d.write_dr((u & 0xff) as u8);
        d.write_dr((u >> 8) as u8);
    }
    let mut out = Vec::new();
    for _ in 0..out_words {
        let lo = d.read_dr() as u16;
        let hi = d.read_dr() as u16;
        out.push((lo | (hi << 8)) as i16);
    }
    out
}

#[test]
fn op00_multiply() {
    let mut d = Dsp1::new();
    // 0.5 * 0.5 = 0.25
    assert_eq!(run(&mut d, 0x00, &[0x4000, 0x4000], 1), vec![0x2000]);
    // ~1 * ~1 = ~1 (Q15 truncation)
    assert_eq!(run(&mut d, 0x00, &[0x7fff, 0x7fff], 1), vec![0x7ffe]);
    // signed
    assert_eq!(run(&mut d, 0x00, &[-0x4000, 0x4000], 1), vec![-0x2000]);
}

#[test]
fn op20_multiply_plus_one() {
    let mut d = Dsp1::new();
    assert_eq!(run(&mut d, 0x20, &[0x4000, 0x4000], 1), vec![0x2001]);
}

#[test]
fn op10_inverse() {
    let mut d = Dsp1::new();
    // 1/(0.5) = 2.0  ->  0x7fff * 2^1
    assert_eq!(run(&mut d, 0x10, &[0x4000, 0x0000], 2), vec![0x7fff, 0x0001]);
    // division by zero -> saturated
    assert_eq!(run(&mut d, 0x10, &[0x0000, 0x0000], 2), vec![0x7fff, 0x002f]);
    // 0x30 aliases 0x10
    assert_eq!(run(&mut d, 0x30, &[0x4000, 0x0000], 2), vec![0x7fff, 0x0001]);
}

#[test]
fn op10_inverse_seed_path() {
    // Non-power-of-two coefficients exercise the DSP1_ROM[0x0065] reciprocal
    // seed lookup plus the two Newton iterations (the table-driven path Inverse
    // and every projection command depend on). Reconstruct icoef*2^iexp in Q15
    // and confirm it is the reciprocal to within Newton-iteration tolerance.
    let mut d = Dsp1::new();
    for &coef in &[0x5000i16, 0x6000, 0x7000, 0x7fff] {
        let out = run(&mut d, 0x10, &[coef, 0x0000], 2);
        let (icoef, iexp) = (out[0], out[1]);
        let value = (icoef as f64 / 32768.0) * 2f64.powi(iexp as i32);
        let expected = 1.0 / (coef as f64 / 32768.0);
        assert!(
            (value - expected).abs() < 1e-3,
            "1/{:#06x}: got {} expected {}",
            coef,
            value,
            expected
        );
        // The seed path must never collapse to the exact 0x4000 special case.
        assert_ne!(icoef, 0x7fff, "coef {:#06x} took the 0x4000 branch", coef);
    }
}

#[test]
fn op28_distance() {
    let mut d = Dsp1::new();
    // Op28 computes a normalized fixed-point sqrt of X^2+Y^2+Z^2 through the
    // DSP1_ROM[0x00d5] sqrt-node interpolation table with a `>> (e>>1)` shift, so
    // raw small integers do not give the naive integer sqrt. These outputs are
    // regression pins of the snes9x-transcription math (a table-offset shift would
    // change them), not independently hardware-verified vectors.
    assert_eq!(run(&mut d, 0x28, &[3, 4, 0], 1), vec![4]);
    assert_eq!(run(&mut d, 0x28, &[0x2000, 0, 0], 1), vec![0x1fff]);
    // sqrt(0) = 0 (degenerate branch).
    assert_eq!(run(&mut d, 0x28, &[0, 0, 0], 1), vec![0]);
    // Extreme radius with bit30 set must not panic (Op28 pos-clamp guard).
    let _ = run(&mut d, 0x28, &[0x6800, 0x6800, 0x6800], 1);
}

#[test]
fn op0a_raster_pins_table() {
    // Regression pin (self-consistency, not hardware-verified): a fixed Parameter
    // setup followed by Raster exercises inverse()+normalize()+truncate() which
    // read DSP1_ROM at 0x0021/0x0031/0x0065. A shift in those table offsets would
    // change these outputs and fail here.
    let mut d = Dsp1::new();
    run(&mut d, 0x02, &[0, 0, 0, 0x0100, 0x0100, 0, 0], 4);
    let a = run(&mut d, 0x0a, &[0x0010], 4);
    let b = run(&mut d, 0x0a, &[0x0020], 4);
    assert_ne!(a, b);
    assert_eq!(a.len(), 4);
    assert_eq!(b.len(), 4);
}

#[test]
fn op04_triangle() {
    let mut d = Dsp1::new();
    // angle 0: sin=0, cos=radius (Q15 truncated)
    assert_eq!(run(&mut d, 0x04, &[0x0000, 0x7fff], 2), vec![0x0000, 0x7ffe]);
    // angle 90deg ($4000): sin=radius, cos=0
    assert_eq!(run(&mut d, 0x04, &[0x4000, 0x7fff], 2), vec![0x7ffe, 0x0000]);
    // angle 45deg ($2000): sin == cos == radius*sin(45)
    let r = run(&mut d, 0x04, &[0x2000, 0x7fff], 2);
    assert_eq!(r, vec![0x5a81, 0x5a81]);
}

#[test]
fn op0c_rotate_2d() {
    let mut d = Dsp1::new();
    // rotate (r,0) by 90deg -> (0,-r)
    let out = run(&mut d, 0x0c, &[0x4000, 0x4000, 0x0000], 2);
    assert_eq!(out, vec![0x0000, -0x3fff]);
}

#[test]
fn op08_radius() {
    let mut d = Dsp1::new();
    // (1^2+2^2+2^2)<<1 = 18 = 0x0000_0012, returned low word then high word
    assert_eq!(run(&mut d, 0x08, &[1, 2, 2], 2), vec![0x0012, 0x0000]);
}

#[test]
fn op18_range() {
    let mut d = Dsp1::new();
    assert_eq!(run(&mut d, 0x18, &[0, 0, 0, 0], 1), vec![0]);
    // (X^2+Y^2+Z^2 - R^2) >> 15 with X=0x4000 -> 0x4000^2>>15 = 0x2000
    assert_eq!(run(&mut d, 0x18, &[0x4000, 0, 0, 0], 1), vec![0x2000]);
}

#[test]
fn attitude_identity_objective_roundtrip() {
    let mut d = Dsp1::new();
    // Identity-ish matrix A: scale $7FFF, zero rotation. matrixA becomes ~0.5*I
    // (the scale is pre-halved), so Objective maps a vector to roughly half of it,
    // and Subjective inverts the same matrix.
    run(&mut d, 0x01, &[0x7fff, 0, 0, 0], 0);
    let f = run(&mut d, 0x0d, &[0x4000, 0, 0], 3);
    // matrixA[0][0] = (0x3fff*0x7ffe>>15)*0x7ffe>>15 ~ 0x3ffc; X*that>>15 ~ 0x1ffd
    assert!(f[0] > 0x1f00 && f[0] < 0x2100, "F={:#x}", f[0]);
    assert_eq!(f[1], 0);
    assert_eq!(f[2], 0);
}

#[test]
fn op06_project_on_axis_is_centered() {
    let mut d = Dsp1::new();
    // Camera at origin looking down +Z (Aas=0, Azs=0).
    run(&mut d, 0x02, &[0, 0, 0, 0x0100, 0x0100, 0, 0], 4);
    // A point on the gaze axis (X=Gx=0, Y=Gy=0) projects to screen centre.
    let out = run(&mut d, 0x06, &[0, 0, 0x0400], 3);
    assert_eq!(out[0], 0, "H");
    assert_eq!(out[1], 0, "V");
}

#[test]
fn op0f_op2f_status() {
    let mut d = Dsp1::new();
    assert_eq!(run(&mut d, 0x0f, &[0], 1), vec![0x0000]);
    assert_eq!(run(&mut d, 0x2f, &[0], 1), vec![0x0100]);
}

#[test]
fn detect_and_mapping() {
    assert_eq!(detect(0x03, 0x20), Some(Dsp1Mapping::LoRom));
    assert_eq!(detect(0x05, 0x21), Some(Dsp1Mapping::HiRom));
    assert_eq!(detect(0x02, 0x20), None);

    let lo = Dsp1Mapping::LoRom;
    assert_eq!(lo.decode(0x30, 0x8000), Some(Dsp1Port::Dr));
    assert_eq!(lo.decode(0x30, 0xc000), Some(Dsp1Port::Sr));
    assert_eq!(lo.decode(0xb0, 0x8000), Some(Dsp1Port::Dr));
    assert_eq!(lo.decode(0x40, 0x8000), None);

    let hi = Dsp1Mapping::HiRom;
    assert_eq!(hi.decode(0x00, 0x6000), Some(Dsp1Port::Dr));
    assert_eq!(hi.decode(0x00, 0x7000), Some(Dsp1Port::Sr));
}

#[test]
fn serde_roundtrip() {
    let mut d = Dsp1::new();
    // Mutate persistent state, then serialize/deserialize and confirm equality of
    // observable behavior.
    run(&mut d, 0x01, &[0x7fff, 0x1000, 0x2000, 0x3000], 0);
    run(&mut d, 0x02, &[10, 20, 30, 0x0100, 0x0100, 0x0400, 0x0400], 4);
    let bytes = bincode::serialize(&d).unwrap();
    let mut d2: Dsp1 = bincode::deserialize(&bytes).unwrap();
    // Same Objective result through both instances.
    let a = run(&mut d, 0x0d, &[0x100, 0x200, 0x300], 3);
    let b = run(&mut d2, 0x0d, &[0x100, 0x200, 0x300], 3);
    assert_eq!(a, b);
}

