//! BG layers: 2/4/8bpp planar tile decode, 32x32/64x32/32x64/64x64 tilemaps,
//! H/V scroll, 8x8 & 16x16 tiles, H/V flip, palette selection, mosaic,
//! offset-per-tile (modes 2/4/6), screen-interlace vertical doubling, and the
//! per-half-dot `phase` sampling the compositor blends for hires modes 5/6.
//!
//! Mode 7 affine BG rendering lives in `mode7.rs`; `render_bg_line` emits a
//! transparent line for mode 7 and the compositor fills BG1 there.

use crate::ppu::{LayerPixel, Ppu};

/// Bits-per-pixel of `bg_index` (0=BG1..3=BG4) in `mode`, or `None` if that BG
/// is not a tile layer in that mode (absent, or the offset-per-tile channel).
fn bg_bpp(mode: u8, bg_index: usize) -> Option<u8> {
    match mode {
        0 => Some(2),
        1 => match bg_index {
            0 | 1 => Some(4),
            2 => Some(2),
            _ => None,
        },
        2 => match bg_index {
            // BG3 holds offset-per-tile data, not a rendered tile layer.
            0 | 1 => Some(4),
            _ => None,
        },
        3 => match bg_index {
            0 => Some(8),
            1 => Some(4),
            _ => None,
        },
        4 => match bg_index {
            0 => Some(8),
            1 => Some(2),
            _ => None,
        },
        5 => match bg_index {
            0 => Some(4),
            1 => Some(2),
            _ => None,
        },
        6 => match bg_index {
            0 => Some(4),
            _ => None,
        },
        // Mode 7 handled by the affine path (deferred).
        _ => None,
    }
}

/// Vertical source line for BG tile fetch. Screen interlace ($2133 bit0) doubles
/// the vertical resolution: display row `line` in field `field` samples the
/// 448-line source row `line<<1 | field`. The field bit is suppressed while the
/// layer's mosaic is active (bsnes ppu-fast). Non-interlace returns `line`.
fn bg_source_line(interlace: bool, field: bool, mosaic_on: bool, line: u16) -> u32 {
    if interlace {
        ((line as u32) << 1) | (if mosaic_on { 0 } else { field as u32 })
    } else {
        line as u32
    }
}

/// Effective (hofs, vofs) scroll for target BG `bg_index` (0=BG1, 1=BG2) at
/// screen 8-px column `col` in offset-per-tile modes (2/4/6). BG3's tilemap
/// supplies per-column overrides; the leftmost visible column (col 0) is never
/// affected, and offset entry 0 (BG3 tile col `BG3HOFS>>3`) applies to col 1.
/// Returns the base BGnHOFS/BGnVOFS when no entry overrides this column.
/// ppu.md §4 offset-per-tile.
fn opt_scroll(ppu: &Ppu, bg_index: usize, col: u32) -> (u32, u32) {
    let base_h = ppu.bg_hofs[bg_index] as u32;
    let base_v = ppu.bg_vofs[bg_index] as u32;
    if col == 0 {
        return (base_h, base_v);
    }

    let bg3_h = ppu.bg_hofs[2] as u32;
    let bg3_v = ppu.bg_vofs[2] as u32;
    let map_base = ppu.bg_map_base[2] as u32;
    let map_size = ppu.bg_map_size[2];
    // BG3 tile-size shift (8/16 px). Mode 6 is hires: BG3 OPT columns stay 8 px.
    let vshift = 3 + ppu.bg_tile_size[2] as u32;
    let hshift = if ppu.bg_mode == 6 { 3 } else { vshift };

    // BG3 tile column for target column `col`: entry index col-1, horizontally
    // shifted by BG3HOFS>>3.
    let hoff = ((col - 1) << 3) + (bg3_h & !7);
    let htile = hoff >> hshift;

    // bit13 overrides BG1, bit14 overrides BG2.
    let valid: u16 = 1 << (13 + bg_index);

    let read = |voff: u32| -> u16 {
        let vtile = voff >> vshift;
        let addr = tilemap_entry_addr(map_base, map_size, htile, vtile);
        ppu.vram[addr as usize]
    };

    let mut eff_h = base_h;
    let mut eff_v = base_v;

    if ppu.bg_mode == 4 {
        // Mode 4: single row; entry bit15 selects direction (0=H, 1=V).
        let entry = read(bg3_v);
        if entry & valid != 0 {
            if entry & 0x8000 == 0 {
                eff_h = ((entry & 0x3FF & !7) as u32) | (base_h & 7);
            } else {
                eff_v = (entry & 0x3FF) as u32;
            }
        }
    } else {
        // Modes 2/6: H entry at the BG3VOFS row, V entry at the next row (+8 px).
        let hlookup = read(bg3_v);
        let vlookup = read(bg3_v + 8);
        if hlookup & valid != 0 {
            // H replaces BGnHOFS except its low 3 bits.
            eff_h = ((hlookup & 0x3FF & !7) as u32) | (base_h & 7);
        }
        if vlookup & valid != 0 {
            // V replaces BGnVOFS entirely.
            eff_v = (vlookup & 0x3FF) as u32;
        }
    }
    (eff_h, eff_v)
}

/// Render one BG tile line into `out` (256 columns). Transparent columns are
/// `LayerPixel::default()`. `line` is the visible scanline 0..=223. `phase`
/// selects the half-dot (0=even/left, 1=odd/right) in the hires modes 5/6; it is
/// ignored in low-res modes.
pub fn render_bg_line(
    ppu: &Ppu,
    bg_index: usize,
    line: u16,
    phase: u32,
    out: &mut [LayerPixel; 256],
) {
    let bpp = match bg_bpp(ppu.bg_mode, bg_index) {
        Some(b) => b,
        None => {
            *out = [LayerPixel::default(); 256];
            return;
        }
    };

    let tile16 = ppu.bg_tile_size[bg_index];
    let tile_px: u32 = if tile16 { 16 } else { 8 };

    // Modes 5/6 are hires (512 dots): the PPU forces BG tiles to 16 px WIDE
    // (char N = left 8, char N+1 = right 8, i.e. the horizontal half of a
    // 16x16 tile — ppu.md §4). `phase` picks which of the two half-dots this
    // pass samples; the compositor blends them 2:1 into the 256-wide buffer
    // (ppu.md §11). Vertical tile size is unaffected by hires.
    let hires = matches!(ppu.bg_mode, 5 | 6);
    let tw_px: u32 = if hires { 16 } else { tile_px };
    let th_px: u32 = tile_px;

    let map_base = ppu.bg_map_base[bg_index] as u32;
    let map_size = ppu.bg_map_size[bg_index];
    let width_tiles: u32 = if map_size & 0x01 != 0 { 64 } else { 32 };
    let height_tiles: u32 = if map_size & 0x02 != 0 { 64 } else { 32 };
    let map_w_px = width_tiles * tw_px;
    let map_h_px = height_tiles * th_px;

    let char_base = ppu.bg_char_base[bg_index] as u32;
    // Words per tile: 2bpp=8, 4bpp=16, 8bpp=32.
    let words_per_tile = (bpp as u32) * 4;

    let base_hofs = ppu.bg_hofs[bg_index] as u32;
    let base_vofs = ppu.bg_vofs[bg_index] as u32;

    // Mode 0 folds a per-BG CGRAM base offset in (BG1=0,BG2=32,BG3=64,BG4=96).
    let palette_base: u16 = if ppu.bg_mode == 0 {
        (bg_index as u16) * 32
    } else {
        0
    };
    let colors_per_pal: u16 = 1 << bpp;

    let mosaic_on = ppu.mosaic_enable & (1 << bg_index) != 0 && ppu.mosaic_size != 0;
    let msize = (ppu.mosaic_size as u32) + 1;

    // Offset-per-tile (modes 2/4/6, BG1/BG2 only): precompute this line's per
    // screen-column scroll overrides from BG3's tilemap.
    let opt = matches!(ppu.bg_mode, 2 | 4 | 6) && bg_index < 2;
    let mut col_scroll = [(base_hofs, base_vofs); 32];
    if opt {
        for (c, cs) in col_scroll.iter_mut().enumerate() {
            *cs = opt_scroll(ppu, bg_index, c as u32);
        }
    }

    let base_line = bg_source_line(ppu.interlace, ppu.interlace_field, mosaic_on, line);
    let sy = if mosaic_on {
        (base_line / msize) * msize
    } else {
        base_line
    };

    for x in 0..256usize {
        let (hofs, vofs) = col_scroll[(x >> 3) & 31];
        let sx = if mosaic_on {
            (x as u32 / msize) * msize
        } else {
            x as u32
        };

        // Hires: display column x spans two half-dots of the 512-dot BG field;
        // `phase` selects even (2x) or odd (2x+1). Scroll folded in at hires res.
        let field_x = if hires { sx * 2 + phase } else { sx };
        let fx = (field_x + hofs) & (map_w_px - 1);
        let fy = (sy + vofs) & (map_h_px - 1);

        let tile_col = fx / tw_px;
        let tile_row = fy / th_px;

        let entry_addr = tilemap_entry_addr(map_base, map_size, tile_col, tile_row);
        let entry = ppu.vram[entry_addr as usize];

        let char_num = (entry & 0x03FF) as u32;
        let palette = ((entry >> 10) & 0x07) as u16;
        let priority = ((entry >> 13) & 0x01) as u8;
        let hflip = entry & 0x4000 != 0;
        let vflip = entry & 0x8000 != 0;

        // In-tile coordinates with flips applied (0..tw_px / 0..th_px).
        let mut ix = fx % tw_px;
        let mut iy = fy % th_px;
        if hflip {
            ix = tw_px - 1 - ix;
        }
        if vflip {
            iy = th_px - 1 - iy;
        }
        // 16x16 tiles are four 8x8 subtiles at char +0,+1,+16,+17.
        let sub_x = ix / 8;
        let sub_y = iy / 8;
        let fine_x = ix & 7;
        let fine_y = iy & 7;
        let tile_num = (char_num + sub_x + sub_y * 16) & 0x03FF;

        let tile_word = (char_base + tile_num * words_per_tile) & 0x7FFF;
        let val = decode_tile_pixel(ppu, tile_word, fine_x, fine_y, bpp);

        if val != 0 {
            let color = if bpp == 8 {
                val as u16
            } else {
                palette_base + palette * colors_per_pal + val as u16
            };
            out[x] = LayerPixel {
                color: color as u8,
                priority,
                opaque: true,
            };
        } else {
            out[x] = LayerPixel::default();
        }
    }
}

/// Tilemap word address for tile (tx,ty), selecting the 32x32 quadrant per the
/// BGnSC YX size layout (bit0 = 64 wide, bit1 = 64 tall).
fn tilemap_entry_addr(base: u32, size: u8, tx: u32, ty: u32) -> u32 {
    let mut a = base + (ty & 0x1F) * 32 + (tx & 0x1F);
    if size & 0x01 != 0 && tx & 0x20 != 0 {
        a += 0x400;
    }
    if size & 0x02 != 0 && ty & 0x20 != 0 {
        a += if size & 0x01 != 0 { 0x800 } else { 0x400 };
    }
    a & 0x7FFF
}

/// Decode one planar pixel value (0..(1<<bpp)-1) from the tile at `tile_word`.
/// Bit 7 of each plane byte is the leftmost pixel; low byte = even plane, high
/// byte = odd plane, one word per row; 2bpp blocks at +0/+8/+16/+24 words.
fn decode_tile_pixel(ppu: &Ppu, tile_word: u32, fine_x: u32, fine_y: u32, bpp: u8) -> u8 {
    let bit = 7 - fine_x;
    let w0 = ppu.vram[((tile_word + fine_y) & 0x7FFF) as usize];
    let mut val = ((w0 >> bit) & 1) as u8 | (((w0 >> (8 + bit)) & 1) as u8) << 1;
    if bpp >= 4 {
        let w1 = ppu.vram[((tile_word + 8 + fine_y) & 0x7FFF) as usize];
        val |= (((w1 >> bit) & 1) as u8) << 2 | (((w1 >> (8 + bit)) & 1) as u8) << 3;
    }
    if bpp == 8 {
        let w2 = ppu.vram[((tile_word + 16 + fine_y) & 0x7FFF) as usize];
        let w3 = ppu.vram[((tile_word + 24 + fine_y) & 0x7FFF) as usize];
        val |= (((w2 >> bit) & 1) as u8) << 4
            | (((w2 >> (8 + bit)) & 1) as u8) << 5
            | (((w3 >> bit) & 1) as u8) << 6
            | (((w3 >> (8 + bit)) & 1) as u8) << 7;
    }
    val
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a 4bpp 8x8 tile at word `base` so that row `r` has pixel value
    /// `pattern[r][c]` (0..15) for column c. Planes are bit-packed per §2.
    fn write_4bpp_tile(ppu: &mut Ppu, base: usize, pattern: &[[u8; 8]; 8]) {
        for r in 0..8 {
            let (mut p0, mut p1, mut p2, mut p3) = (0u16, 0u16, 0u16, 0u16);
            for c in 0..8 {
                let v = pattern[r][c] as u16;
                let bit = 7 - c;
                p0 |= (v & 1) << bit;
                p1 |= ((v >> 1) & 1) << bit;
                p2 |= ((v >> 2) & 1) << bit;
                p3 |= ((v >> 3) & 1) << bit;
            }
            // planes 0/1 in first block word, planes 2/3 in the +8 block word.
            ppu.vram[base + r] = p0 | (p1 << 8);
            ppu.vram[base + 8 + r] = p2 | (p3 << 8);
        }
    }

    #[test]
    fn decode_4bpp_tile_pixels() {
        let mut ppu = Ppu::new();
        let mut pat = [[0u8; 8]; 8];
        // Distinct values across the top row, a diagonal, and corners.
        pat[0] = [1, 2, 3, 4, 5, 6, 7, 8];
        pat[1][7] = 15;
        pat[7][0] = 9;
        write_4bpp_tile(&mut ppu, 0, &pat);
        for c in 0..8u32 {
            assert_eq!(decode_tile_pixel(&ppu, 0, c, 0, 4), pat[0][c as usize]);
        }
        assert_eq!(decode_tile_pixel(&ppu, 0, 7, 1, 4), 15);
        assert_eq!(decode_tile_pixel(&ppu, 0, 0, 7, 4), 9);
        assert_eq!(decode_tile_pixel(&ppu, 0, 1, 2, 4), 0);
    }

    #[test]
    fn render_bg_line_mode1_4bpp_with_palette_and_scroll() {
        let mut ppu = Ppu::new();
        ppu.bg_mode = 1;
        ppu.bg_map_base[0] = 0x0000;
        ppu.bg_map_size[0] = 0;
        ppu.bg_char_base[0] = 0x1000;
        ppu.bg_tile_size[0] = false;

        let mut pat = [[0u8; 8]; 8];
        pat[0] = [1, 2, 3, 0, 5, 6, 7, 8];
        // 4bpp tile stride = 16 words; place tile number 1 at char_base + 16.
        write_4bpp_tile(&mut ppu, 0x1000 + 16, &pat);

        // Tilemap entry (0,0): tile 1, palette 3, priority 1.
        ppu.vram[0] = 1 | (3 << 10) | (1 << 13);

        let mut out = [LayerPixel::default(); 256];
        render_bg_line(&ppu, 0, 0, 0, &mut out);

        // color = palette*16 + val (mode 1, no per-BG offset).
        assert_eq!(out[0].color, 3 * 16 + 1);
        assert!(out[0].opaque);
        assert_eq!(out[0].priority, 1);
        assert_eq!(out[1].color, 3 * 16 + 2);
        // pixel value 0 -> transparent (color-0 within palette).
        assert!(!out[3].opaque);
        assert_eq!(out[7].color, 3 * 16 + 8);
    }

    #[test]
    fn render_bg_line_hflip() {
        let mut ppu = Ppu::new();
        ppu.bg_mode = 1;
        ppu.bg_char_base[0] = 0x0000;
        let mut pat = [[0u8; 8]; 8];
        pat[0] = [1, 2, 3, 4, 5, 6, 7, 8];
        write_4bpp_tile(&mut ppu, 16, &pat);
        ppu.vram[0] = 1 | 0x4000; // tile 1, H-flip
        let mut out = [LayerPixel::default(); 256];
        render_bg_line(&ppu, 0, 0, 0, &mut out);
        // Reversed row.
        assert_eq!(out[0].color, 8);
        assert_eq!(out[7].color, 1);
    }

    #[test]
    fn interlace_bg_source_line() {
        // Non-interlace: identity regardless of field/mosaic.
        assert_eq!(bg_source_line(false, false, false, 100), 100);
        assert_eq!(bg_source_line(false, true, false, 100), 100);
        // Interlace field 0: row r -> 2r; field 1: 2r+1 (the two 448-line fields).
        assert_eq!(bg_source_line(true, false, false, 100), 200);
        assert_eq!(bg_source_line(true, true, false, 100), 201);
        assert_eq!(bg_source_line(true, false, false, 0), 0);
        assert_eq!(bg_source_line(true, true, false, 0), 1);
        // Mosaic suppresses the field bit.
        assert_eq!(bg_source_line(true, true, true, 100), 200);
    }

    #[test]
    fn offset_per_tile_shifts_one_bg1_column() {
        let mut ppu = Ppu::new();
        ppu.bg_mode = 2; // BG1/BG2 4bpp, BG3 = OPT data
        // BG1: map at 0, char at 0x2000, 8x8 tiles, no scroll.
        ppu.bg_map_base[0] = 0x0000;
        ppu.bg_char_base[0] = 0x2000;
        ppu.bg_tile_size[0] = false;
        ppu.bg_hofs[0] = 0;
        ppu.bg_vofs[0] = 0;
        // BG3 (OPT): map at 0x1000, no scroll, 8x8.
        ppu.bg_map_base[2] = 0x1000;
        ppu.bg_tile_size[2] = false;
        ppu.bg_hofs[2] = 0;
        ppu.bg_vofs[2] = 0;

        // BG1 tilemap row 0: map column c selects tile number c+1, so a
        // horizontal shift of N tiles changes the visible tile.
        for c in 0..8u16 {
            ppu.vram[c as usize] = c + 1;
        }
        // Solid 4bpp tiles: tile T (1..=8) has every pixel = value T.
        for t in 1..=8u16 {
            let base = 0x2000 + (t as usize) * 16;
            for r in 0..8 {
                let (mut p0, mut p1, mut p2, mut p3) = (0u16, 0u16, 0u16, 0u16);
                for c in 0..8 {
                    let bit = 7 - c;
                    p0 |= (t & 1) << bit;
                    p1 |= ((t >> 1) & 1) << bit;
                    p2 |= ((t >> 2) & 1) << bit;
                    p3 |= ((t >> 3) & 1) << bit;
                }
                ppu.vram[base + r] = p0 | (p1 << 8);
                ppu.vram[base + 8 + r] = p2 | (p3 << 8);
            }
        }

        // OPT H entry for screen column 1: BG3 tile (0,0) = vram[0x1000].
        // bit13 = apply to BG1, replacement H scroll value = 16 (two tiles).
        ppu.vram[0x1000] = 0x2000 | 16;

        let mut out = [LayerPixel::default(); 256];
        render_bg_line(&ppu, 0, 0, 0, &mut out);

        // Column 0 (never offset): tile col 0 -> tile 1 -> value 1.
        assert_eq!(out[0].color, 1);
        // Column 1 (screen x 8..15): shifted +16 px -> BG tile col 3 -> tile 4.
        assert_eq!(out[8].color, 4);
        assert_eq!(out[15].color, 4);
        // Column 2 has no OPT entry (BG3 tile (1,0) = 0) -> unshifted tile col 2.
        assert_eq!(out[16].color, 3);
    }

    #[test]
    fn mode0_per_bg_palette_offset() {
        let mut ppu = Ppu::new();
        ppu.bg_mode = 0; // all 2bpp
        ppu.bg_char_base[1] = 0x0000;
        // 2bpp tile 1 (stride 8 words) row 0 = value 1 in column 0.
        ppu.vram[8] = 0x0080; // plane0 bit7 set -> pixel value 1 at col 0
        // BG2 map base default 0; entry (0,0) selects tile 1, palette 0.
        ppu.vram[0] = 1;
        let mut out = [LayerPixel::default(); 256];
        render_bg_line(&ppu, 1, 0, 0, &mut out); // BG2
        // Mode 0 BG2 base offset = 32; palette 0, val 1 -> color 33.
        assert_eq!(out[0].color, 32 + 1);
        assert!(out[0].opaque);
    }
}
