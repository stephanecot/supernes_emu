//! Pure 65C816 arithmetic/logic helpers (ADC/SBC incl. BCD, shifts, compares).
//! All width-generic: `eight=true` operates on the low 8 bits, else on 16 bits.

use super::Flags;

#[inline]
fn width(eight: bool) -> (u16, u16) {
    if eight {
        (0x80, 0x00FF)
    } else {
        (0x8000, 0xFFFF)
    }
}

#[inline]
fn set_nz(p: &mut Flags, v: u16, sign: u16, mask: u16) {
    p.set_n(v & sign != 0);
    p.set_z(v & mask == 0);
}

/// Decimal (BCD) ADC of a single byte with carry-in. Returns
/// (result, carry_out, overflow). Exact 65C816 sequence: V is computed on the
/// signed value of the high-nibble sum before the +$60 correction.
fn adc_bcd8(a: u8, b: u8, cin: bool) -> (u8, bool, bool) {
    let mut al = (a & 0x0F) as i32 + (b & 0x0F) as i32 + cin as i32;
    if al >= 0x0A {
        al = ((al + 0x06) & 0x0F) + 0x10;
    }
    let mut a2 = (a & 0xF0) as i32 + (b & 0xF0) as i32 + al;
    let v = (!(a ^ b) & (a ^ (a2 as u8)) & 0x80) != 0;
    if a2 >= 0xA0 {
        a2 += 0x60;
    }
    ((a2 & 0xFF) as u8, a2 >= 0x100, v)
}

/// Decimal (BCD) SBC of a single byte. Returns (result, byte_carry) where
/// byte_carry (no-borrow) chains into the next byte's carry-in. C and V for the
/// full operation are taken from the binary computation, not from here.
fn sbc_bcd8(a: u8, b: u8, cin: bool) -> (u8, bool) {
    let mut al = (a & 0x0F) as i32 - (b & 0x0F) as i32 + cin as i32 - 1;
    if al < 0 {
        al = ((al - 0x06) & 0x0F) - 0x10;
    }
    let mut a2 = (a & 0xF0) as i32 - (b & 0xF0) as i32 + al;
    let byte_carry = a2 >= 0;
    if a2 < 0 {
        a2 -= 0x60;
    }
    ((a2 & 0xFF) as u8, byte_carry)
}

/// ADC: A = A + B + C. Binary or (when D=1) BCD. Sets N V Z C.
pub fn adc(a: u16, b: u16, p: &mut Flags, eight: bool) -> u16 {
    let (sign, mask) = width(eight);
    let a = a & mask;
    let b = b & mask;
    let cin = p.c();
    if p.d() {
        let (full, carry, v) = if eight {
            let (r, c, v) = adc_bcd8(a as u8, b as u8, cin);
            (r as u16, c, v)
        } else {
            let (rl, c1, _) = adc_bcd8(a as u8, b as u8, cin);
            let (rh, c2, v) = adc_bcd8((a >> 8) as u8, (b >> 8) as u8, c1);
            ((rl as u16) | ((rh as u16) << 8), c2, v)
        };
        p.set_c(carry);
        p.set_v(v);
        set_nz(p, full, sign, mask);
        return full;
    }
    let sum = a as u32 + b as u32 + cin as u32;
    let r = (sum as u16) & mask;
    p.set_c(sum > mask as u32);
    p.set_v((!(a ^ b) & (a ^ r) & sign) != 0);
    set_nz(p, r, sign, mask);
    r
}

/// SBC: A = A - B - !C. Binary or (when D=1) BCD; C and V always come from the
/// binary subtraction (C=1 means no borrow). Sets N V Z C.
pub fn sbc(a: u16, b: u16, p: &mut Flags, eight: bool) -> u16 {
    let (sign, mask) = width(eight);
    let a = a & mask;
    let b = b & mask;
    let cin = p.c();
    let b_inv = (!b) & mask;
    let sum = a as u32 + b_inv as u32 + cin as u32;
    let bin = (sum as u16) & mask;
    let carry = sum > mask as u32;
    let overflow = ((a ^ b) & (a ^ bin) & sign) != 0;
    let result = if p.d() {
        if eight {
            sbc_bcd8(a as u8, b as u8, cin).0 as u16
        } else {
            let (rl, c1) = sbc_bcd8(a as u8, b as u8, cin);
            let (rh, _) = sbc_bcd8((a >> 8) as u8, (b >> 8) as u8, c1);
            (rl as u16) | ((rh as u16) << 8)
        }
    } else {
        bin
    };
    p.set_c(carry);
    p.set_v(overflow);
    set_nz(p, result, sign, mask);
    result
}

/// CMP/CPX/CPY: binary A - B; sets N Z C only (C=1 iff A >= B). No decimal, no V.
pub fn cmp(a: u16, b: u16, p: &mut Flags, eight: bool) {
    let (sign, mask) = width(eight);
    let a = a & mask;
    let b = b & mask;
    let diff = a as u32 + ((!b) & mask) as u32 + 1;
    let r = (diff as u16) & mask;
    p.set_c(diff > mask as u32);
    set_nz(p, r, sign, mask);
}

pub fn asl(v: u16, p: &mut Flags, eight: bool) -> u16 {
    let (sign, mask) = width(eight);
    p.set_c(v & sign != 0);
    let r = (v << 1) & mask;
    set_nz(p, r, sign, mask);
    r
}

pub fn lsr(v: u16, p: &mut Flags, eight: bool) -> u16 {
    let (sign, mask) = width(eight);
    p.set_c(v & 1 != 0);
    let r = (v & mask) >> 1;
    set_nz(p, r, sign, mask);
    r
}

pub fn rol(v: u16, p: &mut Flags, eight: bool) -> u16 {
    let (sign, mask) = width(eight);
    let cin = p.c() as u16;
    p.set_c(v & sign != 0);
    let r = ((v << 1) | cin) & mask;
    set_nz(p, r, sign, mask);
    r
}

pub fn ror(v: u16, p: &mut Flags, eight: bool) -> u16 {
    let (sign, mask) = width(eight);
    let cin = if p.c() { sign } else { 0 };
    p.set_c(v & 1 != 0);
    let r = (((v & mask) >> 1) | cin) & mask;
    set_nz(p, r, sign, mask);
    r
}

pub fn inc(v: u16, p: &mut Flags, eight: bool) -> u16 {
    let (sign, mask) = width(eight);
    let r = v.wrapping_add(1) & mask;
    set_nz(p, r, sign, mask);
    r
}

pub fn dec(v: u16, p: &mut Flags, eight: bool) -> u16 {
    let (sign, mask) = width(eight);
    let r = v.wrapping_sub(1) & mask;
    set_nz(p, r, sign, mask);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(bits: u8) -> Flags {
        Flags(bits)
    }

    #[test]
    fn adc_binary_overflow() {
        // $50 + $50 = $A0, signed overflow set, no carry (8-bit).
        let mut p = flags(0);
        let r = adc(0x50, 0x50, &mut p, true);
        assert_eq!(r, 0xA0);
        assert!(p.v());
        assert!(p.n());
        assert!(!p.c());
        assert!(!p.z());
    }

    #[test]
    fn adc_binary_carry() {
        // $FF + $01 = $00 with carry, zero set (8-bit).
        let mut p = flags(0);
        let r = adc(0xFF, 0x01, &mut p, true);
        assert_eq!(r, 0x00);
        assert!(p.c());
        assert!(p.z());
        assert!(!p.n());
    }

    #[test]
    fn adc_decimal_8() {
        // BCD 08 + 08 = 16, no carry.
        let mut p = flags(Flags::D);
        let r = adc(0x08, 0x08, &mut p, true);
        assert_eq!(r, 0x16);
        assert!(!p.c());

        // BCD 99 + 01 = 00 with carry.
        let mut p = flags(Flags::D);
        let r = adc(0x99, 0x01, &mut p, true);
        assert_eq!(r, 0x00);
        assert!(p.c());
        assert!(p.z());

        // BCD 09 + 01 = 10.
        let mut p = flags(Flags::D);
        assert_eq!(adc(0x09, 0x01, &mut p, true), 0x10);
        assert!(!p.c());
    }

    #[test]
    fn adc_decimal_16() {
        // BCD 9999 + 0001 = 0000 with carry.
        let mut p = flags(Flags::D);
        let r = adc(0x9999, 0x0001, &mut p, false);
        assert_eq!(r, 0x0000);
        assert!(p.c());
        assert!(p.z());

        // BCD 1234 + 5678 = 6912, no carry.
        let mut p = flags(Flags::D);
        let r = adc(0x1234, 0x5678, &mut p, false);
        assert_eq!(r, 0x6912);
        assert!(!p.c());
        assert!(!p.n());
    }

    #[test]
    fn adc_decimal_16_carry_in() {
        // BCD 1200 + 3400 + C = 4601.
        let mut p = flags(Flags::D | Flags::C);
        let r = adc(0x1200, 0x3400, &mut p, false);
        assert_eq!(r, 0x4601);
        assert!(!p.c());
    }

    #[test]
    fn sbc_binary_borrow() {
        // 8-bit: $00 - $01 with C=1(no borrow in) = $FF, borrow out (C=0).
        let mut p = flags(Flags::C);
        let r = sbc(0x00, 0x01, &mut p, true);
        assert_eq!(r, 0xFF);
        assert!(!p.c());
        assert!(p.n());
    }

    #[test]
    fn sbc_binary_no_borrow() {
        // $50 - $30 with C=1 = $20, no borrow (C=1).
        let mut p = flags(Flags::C);
        let r = sbc(0x50, 0x30, &mut p, true);
        assert_eq!(r, 0x20);
        assert!(p.c());
        assert!(!p.n());
    }

    #[test]
    fn sbc_decimal_8() {
        // BCD 46 - 12 with C=1 = 34.
        let mut p = flags(Flags::D | Flags::C);
        let r = sbc(0x46, 0x12, &mut p, true);
        assert_eq!(r, 0x34);
        assert!(p.c());

        // BCD 00 - 01 with C=1 = 99, borrow (C=0).
        let mut p = flags(Flags::D | Flags::C);
        let r = sbc(0x00, 0x01, &mut p, true);
        assert_eq!(r, 0x99);
        assert!(!p.c());
    }

    #[test]
    fn sbc_decimal_16() {
        // BCD 9999 - 0001 with C=1 = 9998.
        let mut p = flags(Flags::D | Flags::C);
        let r = sbc(0x9999, 0x0001, &mut p, false);
        assert_eq!(r, 0x9998);
        assert!(p.c());

        // BCD 0000 - 0001 with C=1 = 9999, borrow.
        let mut p = flags(Flags::D | Flags::C);
        let r = sbc(0x0000, 0x0001, &mut p, false);
        assert_eq!(r, 0x9999);
        assert!(!p.c());
    }

    #[test]
    fn cmp_sets_carry_when_ge() {
        let mut p = flags(0);
        cmp(0x50, 0x30, &mut p, true);
        assert!(p.c());
        assert!(!p.z());
        let mut p = flags(0);
        cmp(0x30, 0x30, &mut p, true);
        assert!(p.c());
        assert!(p.z());
        let mut p = flags(0);
        cmp(0x20, 0x30, &mut p, true);
        assert!(!p.c());
        assert!(p.n());
    }

    #[test]
    fn shifts_carry_and_zero() {
        let mut p = flags(0);
        assert_eq!(asl(0x80, &mut p, true), 0x00);
        assert!(p.c());
        assert!(p.z());

        let mut p = flags(0);
        assert_eq!(lsr(0x01, &mut p, true), 0x00);
        assert!(p.c());
        assert!(p.z());
        assert!(!p.n());

        // ROL brings carry into bit0; 16-bit width keeps bit15 as sign.
        let mut p = flags(Flags::C);
        assert_eq!(rol(0x4000, &mut p, false), 0x8001);
        assert!(!p.c());
        assert!(p.n());

        let mut p = flags(Flags::C);
        assert_eq!(ror(0x0001, &mut p, false), 0x8000);
        assert!(p.c());
        assert!(p.n());
    }

    #[test]
    fn inc_dec_wrap() {
        let mut p = flags(0);
        assert_eq!(inc(0xFF, &mut p, true), 0x00);
        assert!(p.z());
        let mut p = flags(0);
        assert_eq!(dec(0x00, &mut p, true), 0xFF);
        assert!(p.n());
        let mut p = flags(0);
        assert_eq!(inc(0xFFFF, &mut p, false), 0x0000);
        assert!(p.z());
    }
}
