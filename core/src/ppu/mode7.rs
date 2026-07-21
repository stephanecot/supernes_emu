//! Mode 7: signed 8.8 fixed-point affine transform sampling the 128×128
//! tilemap / 256-color char data interleaved in VRAM (low bytes = tilemap,
//! high bytes = char). Products are truncated to multiples of 64 before
//! summing (ppu.md §13, anomie/fullsnes-verified).

use crate::ppu::Ppu;

/// Sign-extend a 13-bit Mode 7 scroll value to i32.
fn se13(v: u16) -> i32 {
    let v = (v & 0x1FFF) as i32;
    if v & 0x1000 != 0 {
        v - 0x2000
    } else {
        v
    }
}

/// 13-bit signed → ±1023 clip quirk: bit13 acts as the sign of a 10-bit field.
fn clip(a: i32) -> i32 {
    if a & 0x2000 != 0 {
        a | !0x3FF
    } else {
        a & 0x3FF
    }
}

/// Sample the Mode 7 playfield at pixel (vx, vy) applying M7SEL screen-over
/// `over` (0/1 = wrap, 2 = transparent outside, 3 = tile $00 outside). Returns
/// the 8-bit color index (0 = transparent for BG1).
fn sample(ppu: &Ppu, vx: i32, vy: i32, over: u8) -> u8 {
    let outside = (vx & !0x3FF) != 0 || (vy & !0x3FF) != 0;
    let use_tile0 = match over {
        2 => {
            if outside {
                return 0;
            }
            false
        }
        3 => outside,
        _ => false,
    };
    let px = (vx & 0x3FF) as u32;
    let py = (vy & 0x3FF) as u32;
    let tile = if use_tile0 {
        0u32
    } else {
        let map_idx = ((py >> 3) * 128 + (px >> 3)) & 0x7FFF;
        (ppu.vram[map_idx as usize] & 0xFF) as u32
    };
    let char_idx = (tile * 64 + (py & 7) * 8 + (px & 7)) & 0x7FFF;
    (ppu.vram[char_idx as usize] >> 8) as u8
}

/// Render one Mode 7 scanline (`line` = visible row 0..=223) into `out` as
/// 8-bit color indices (0 = transparent). BG1 uses all 8 bits; EXTBG BG2 (built
/// by the compositor) uses bit7 = priority, low 7 bits = color.
pub fn render_mode7_line(ppu: &Ppu, line: u16, out: &mut [u8; 256]) {
    let hflip = ppu.m7sel & 0x01 != 0;
    let vflip = ppu.m7sel & 0x02 != 0;
    let over = (ppu.m7sel >> 6) & 0x03;

    let mosaic = ppu.mosaic_enable & 0x01 != 0 && ppu.mosaic_size != 0;
    let msize = ppu.mosaic_size as u32 + 1;

    let a = ppu.m7a as i32;
    let b = ppu.m7b as i32;
    let c = ppu.m7c as i32;
    let d = ppu.m7d as i32;
    let cx = ppu.m7x as i32;
    let cy = ppu.m7y as i32;
    let hh = clip(se13(ppu.m7_hofs) - cx);
    let vv = clip(se13(ppu.m7_vofs) - cy);

    let sline = if mosaic {
        (line as u32 / msize) * msize
    } else {
        line as u32
    };
    let screen_y = if vflip { 255 - sline as i32 } else { sline as i32 };

    // Per-line origin: constant terms + the M7B/M7D·y line terms, each product
    // truncated to a multiple of 64.
    let base_x = (a * hh & !63) + (b * vv & !63) + (b * screen_y & !63) + (cx << 8);
    let base_y = (c * hh & !63) + (d * vv & !63) + (d * screen_y & !63) + (cy << 8);

    for (col, o) in out.iter_mut().enumerate() {
        let scol = if mosaic {
            (col as u32 / msize) * msize
        } else {
            col as u32
        };
        let screen_x = if hflip { 255 - scol as i32 } else { scol as i32 };
        let vx = (base_x + a * screen_x) >> 8;
        let vy = (base_y + c * screen_x) >> 8;
        *o = sample(ppu, vx, vy, over);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Identity matrix (M7A=M7D=1.0, M7B=M7C=0, center/scroll 0) maps screen
    /// (x,y) 1:1 to playfield (x,y).
    #[test]
    fn identity_matrix_maps_1to1() {
        let mut ppu = Ppu::new();
        ppu.m7a = 0x0100;
        ppu.m7d = 0x0100;
        ppu.m7b = 0;
        ppu.m7c = 0;
        ppu.m7x = 0;
        ppu.m7y = 0;
        ppu.m7_hofs = 0;
        ppu.m7_vofs = 0;
        ppu.m7sel = 0; // wrap

        // Tilemap: playfield tile (0,0) = tile 1, tile (1,0) = tile 2.
        ppu.vram[0] = (ppu.vram[0] & 0xFF00) | 1; // map index 0 low byte
        ppu.vram[1] = (ppu.vram[1] & 0xFF00) | 2; // map index 1 low byte
        // Char data (high bytes): tile 1 pixel (0,0)=0xAB, tile 2 pixel(0,0)=0xCD.
        ppu.vram[1 * 64] = (0xABu16) << 8;
        ppu.vram[2 * 64] = (0xCDu16) << 8;

        let mut out = [0u8; 256];
        render_mode7_line(&ppu, 0, &mut out);
        assert_eq!(out[0], 0xAB); // screen (0,0) -> playfield (0,0) -> tile 1
        assert_eq!(out[8], 0xCD); // screen (8,0) -> playfield (8,0) -> tile 2
    }

    #[test]
    fn transparent_outside_screen_over() {
        let mut ppu = Ppu::new();
        ppu.m7a = 0x0100;
        ppu.m7d = 0x0100;
        ppu.m7sel = 0x80; // over = 2: outside 1024×1024 transparent
        ppu.m7_hofs = 0x1FFF; // -1 as 13-bit signed -> playfield x negative
        let mut out = [0u8; 256];
        render_mode7_line(&ppu, 0, &mut out);
        // Screen column 0 samples playfield x = -1 (outside) -> transparent.
        assert_eq!(out[0], 0);
    }

    #[test]
    fn clip_quirk() {
        assert_eq!(clip(5), 5);
        assert_eq!(clip(-1), -1);
        // bit13 set forces sign-extension from bit10.
        assert_eq!(clip(0x2000), 0x2000 | !0x3FF);
        assert_eq!(clip(0x0400), 0); // 1024 masked to 10 bits -> 0
    }
}
