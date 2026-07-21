//! BRR sample decoding: 9-byte blocks (1 header + 16 samples, 2 per byte, high
//! nibble first), 4 filters, loop/end flags. Pure decoder used by the S-DSP.
//!
//! Header byte `SSSS FFLE`: S = shift 0-12 (13-15 clamp to shift 12 with the
//! nibble replaced by `nibble >> 3`), F = filter 0-3, L = loop, E = end.

/// Parsed BRR block header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrrHeader {
    pub shift: u8,
    pub filter: u8,
    pub loop_flag: bool,
    pub end_flag: bool,
}

pub fn parse_header(h: u8) -> BrrHeader {
    BrrHeader {
        shift: h >> 4,
        filter: (h >> 2) & 0x03,
        loop_flag: h & 0x02 != 0,
        end_flag: h & 0x01 != 0,
    }
}

/// Decode one 9-byte block into 16 samples using the two previous decoded
/// samples `p1` (old) / `p2` (older) as filter history. Returns the 16 samples,
/// the parsed header and the updated `(p1, p2)` history for the next block. Each
/// output sample is 15-bit signed (the value that feeds the Gaussian filter and
/// becomes the next block's history).
pub fn decode_block(bytes: &[u8; 9], mut p1: i32, mut p2: i32) -> ([i32; 16], BrrHeader, i32, i32) {
    let hdr = parse_header(bytes[0]);
    let shift = hdr.shift;
    let mut out = [0i32; 16];
    for (i, slot) in out.iter_mut().enumerate() {
        let byte = bytes[1 + i / 2];
        let raw = if i & 1 == 0 { byte >> 4 } else { byte & 0x0F };
        // 4-bit two's-complement nibble, range -8..+7.
        let nib = if raw >= 8 { raw as i32 - 16 } else { raw as i32 };
        // sample = (nibble << shift) >> 1 (arithmetic). Shift 13-15 acts as
        // shift 12 with nibble replaced by (nibble >> 3): result 0 or -2048.
        let s = if shift <= 12 { (nib << shift) >> 1 } else { (nib >> 3) << 12 >> 1 };
        let filtered = match hdr.filter {
            0 => s,
            1 => s + p1 + ((-p1) >> 4),
            2 => s + 2 * p1 + ((-3 * p1) >> 5) - p2 + (p2 >> 4),
            _ => s + 2 * p1 + ((-13 * p1) >> 6) - p2 + ((3 * p2) >> 4),
        };
        // Clamp to signed 16-bit, then clip to 15 bits by sign-extending bit 14.
        let c16 = filtered.clamp(-32768, 32767);
        let c15 = ((c16 & 0x7FFF) ^ 0x4000) - 0x4000;
        p2 = p1;
        p1 = c15;
        *slot = c15;
    }
    (out, hdr, p1, p2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(header: u8, b0: u8) -> [u8; 9] {
        let mut b = [0u8; 9];
        b[0] = header;
        b[1] = b0;
        b
    }

    #[test]
    fn header_flags() {
        // $43 = shift 4, filter 0, loop+end set.
        let h = parse_header(0x43);
        assert_eq!(h.shift, 4);
        assert_eq!(h.filter, 0);
        assert!(h.loop_flag);
        assert!(h.end_flag);
        // $D8 = shift 13, filter 2, no flags.
        let h = parse_header(0xD8);
        assert_eq!(h.shift, 13);
        assert_eq!(h.filter, 2);
        assert!(!h.loop_flag);
        assert!(!h.end_flag);
    }

    #[test]
    fn filter0_direct() {
        // shift 4, filter 0, nibbles 1 then 2: (1<<4)>>1=8, (2<<4)>>1=16.
        let (s, _, _, _) = decode_block(&block(0x40, 0x12), 0, 0);
        assert_eq!(s[0], 8);
        assert_eq!(s[1], 16);
        assert_eq!(s[2], 0);
    }

    #[test]
    fn filter1_recursion() {
        // shift 4, filter 1, nibbles 1,1. s0=8 -> new=8. s1=8 -> 8+8+((-8)>>4)=15.
        let (s, _, _, _) = decode_block(&block(0x44, 0x11), 0, 0);
        assert_eq!(s[0], 8);
        assert_eq!(s[1], 15);
    }

    #[test]
    fn filter2_recursion() {
        // shift 4, filter 2, nibbles 1,1. s0=8. s1=8+16+((-24)>>5)-0+0=23.
        let (s, _, _, _) = decode_block(&block(0x48, 0x11), 0, 0);
        assert_eq!(s[0], 8);
        assert_eq!(s[1], 23);
    }

    #[test]
    fn filter3_recursion() {
        // shift 4, filter 3, nibbles 1,1. s0=8. s1=8+16+((-104)>>6)-0+0=22.
        let (s, _, _, _) = decode_block(&block(0x4C, 0x11), 0, 0);
        assert_eq!(s[0], 8);
        assert_eq!(s[1], 22);
    }

    #[test]
    fn shift_over_12_clamps() {
        // shift 13, filter 0: high nibble 8 (=-8) -> -2048, nibble 0 -> 0.
        let (s, _, _, _) = decode_block(&block(0xD0, 0x80), 0, 0);
        assert_eq!(s[0], -2048);
        assert_eq!(s[1], 0);
    }
}
