//! OBJ/sprites: OAM evaluation, per-line rendering, the 32-sprite / 34-sliver
//! per-line hardware limits, priority rotation, and the range/time-over flags
//! ($213E bits6/7). Conforms to the `ObjPixel` compositor contract in
//! `super` (see `ppu/mod.rs`).

use super::{ObjPixel, Ppu};

/// OBSEL small/large sizes as (width, height) in pixels (ppu.md §8 table).
fn obj_sizes(mode: u8) -> ((u16, u16), (u16, u16)) {
    match mode & 0x07 {
        0 => ((8, 8), (16, 16)),
        1 => ((8, 8), (32, 32)),
        2 => ((8, 8), (64, 64)),
        3 => ((16, 16), (32, 32)),
        4 => ((16, 16), (64, 64)),
        5 => ((32, 32), (64, 64)),
        6 => ((16, 32), (32, 64)),
        _ => ((16, 32), (32, 32)),
    }
}

/// Table-2 attributes for sprite `idx`: (x_bit8, size_select).
#[inline]
fn table2(ppu: &Ppu, idx: usize) -> (bool, bool) {
    let byte = ppu.oam_hi[idx >> 2];
    let shift = (idx & 3) * 2;
    (byte >> shift & 1 != 0, byte >> (shift + 1) & 1 != 0)
}

#[inline]
fn dims(ppu: &Ppu, idx: usize, small: (u16, u16), large: (u16, u16)) -> (u16, u16) {
    if table2(ppu, idx).1 {
        large
    } else {
        small
    }
}

/// Fetch one 4bpp OBJ pixel (0-15) for base tile `base_tile` (0-511) at sprite
/// pixel (`col`, `row`). 0 = transparent.
fn fetch_pixel(ppu: &Ppu, base_tile: u16, col: u16, row: u16) -> u8 {
    let tile_col = col >> 3;
    let tile_row = row >> 3;
    // OBJ name table is 16×16 tiles; large sprites step the tile number's low
    // nibble (X) and high nibble (Y) independently, each wrapping within its
    // nibble.
    let col_t = ((base_tile & 0x0F) + tile_col) & 0x0F;
    let row_t = (((base_tile >> 4) & 0x0F) + tile_row) & 0x0F;
    let tile_num = (base_tile & 0x100) | (row_t << 4) | col_t;

    let mut word_addr = ppu.obj_name_base + (tile_num & 0xFF) * 16;
    if tile_num & 0x100 != 0 {
        word_addr = word_addr.wrapping_add(ppu.obj_name_gap);
    }
    let fine_y = (row & 7) as usize;
    let bit = 7 - (col & 7) as u8;

    let w01 = ppu.vram[(word_addr as usize + fine_y) & 0x7FFF];
    let w23 = ppu.vram[(word_addr as usize + 8 + fine_y) & 0x7FFF];
    let p0 = (w01 & 0xFF) as u8;
    let p1 = (w01 >> 8) as u8;
    let p2 = (w23 & 0xFF) as u8;
    let p3 = (w23 >> 8) as u8;

    ((p0 >> bit) & 1)
        | (((p1 >> bit) & 1) << 1)
        | (((p2 >> bit) & 1) << 2)
        | (((p3 >> bit) & 1) << 3)
}

/// Evaluate OAM for `line` and write one `ObjPixel` per screen column. Sets
/// `ppu.obj_range_over` (>32 sprites on the line) and `ppu.obj_time_over`
/// (>34 8×1 tile slivers on the line).
pub fn render_obj_line(ppu: &mut Ppu, line: u16, out: &mut [ObjPixel; 256]) {
    *out = [ObjPixel::default(); 256];
    // $213E range/time-over flags are sticky across the frame: only ever SET
    // here; they are cleared once per frame in Ppu::start_frame.

    let (small, large) = obj_sizes(ppu.obj_size);

    // Priority rotation ($2103 bit7): FirstSprite = (OAMADDL & $FE) >> 1.
    let first = if ppu.oam_priority {
        ((ppu.oam_addr_reg & 0xFE) >> 1) as usize
    } else {
        0
    };

    // OBJ interlace ($2133 bit1): sprites are sampled at half vertical
    // resolution (height halved for the range test, row index doubled and
    // field-selected) so they appear half as tall (ppu.md §15, bsnes ppu-fast).
    let obj_il = ppu.obj_interlace;
    let field = ppu.interlace_field as u16;

    // --- Range: first 32 in-range sprites in rotation order; 33rd sets bit6. ---
    let mut in_range = [0usize; 32];
    let mut n_range = 0usize;
    for k in 0..128usize {
        let idx = (first + k) & 0x7F;
        let (_, height) = dims(ppu, idx, small, large);
        let vis_height = if obj_il { height >> 1 } else { height };
        let y = ppu.oam_lo[idx * 4 + 1] as u16;
        let row = line.wrapping_sub(y) & 0xFF;
        if (row as u32) < vis_height as u32 {
            if n_range == 32 {
                ppu.obj_range_over = true;
                break;
            }
            in_range[n_range] = idx;
            n_range += 1;
        }
    }

    // --- Time: tile fetch in REVERSE order; 34-sliver budget; overflow sets
    // bit7 and drops the remaining (lower-index, higher-priority) sprites. ---
    let mut slivers = 0u32;
    'outer: for r in (0..n_range).rev() {
        let idx = in_range[r];
        let (x_bit8, size_sel) = table2(ppu, idx);
        let (width, height) = if size_sel { large } else { small };

        let b0 = ppu.oam_lo[idx * 4] as i32;
        let b1 = ppu.oam_lo[idx * 4 + 1] as u16;
        let b2 = ppu.oam_lo[idx * 4 + 2] as u16;
        let b3 = ppu.oam_lo[idx * 4 + 3];

        let x = if x_bit8 { b0 - 256 } else { b0 };
        let vflip = b3 & 0x80 != 0;
        let hflip = b3 & 0x40 != 0;
        let priority = (b3 >> 4) & 0x03;
        let palette = (b3 >> 1) & 0x07;
        let base_tile = b2 | ((b3 as u16 & 0x01) << 8);

        // Row within the sprite for this line (Y wraps mod 256), V-flipped.
        // OBJ interlace doubles the row (skipping alternate sprite rows) and
        // offsets by the field, applied after the V-flip (bsnes ppu-fast).
        let mut srow = line.wrapping_sub(b1) & 0xFF;
        if obj_il {
            srow <<= 1;
        }
        if vflip {
            srow = height - 1 - srow;
        }
        if obj_il {
            srow = if vflip {
                srow.wrapping_sub(field)
            } else {
                srow.wrapping_add(field)
            } & 0xFF;
        }

        let n_tiles = width / 8;
        let color_base = 128 + (palette as u16) * 16;

        for tx in 0..n_tiles {
            // Only on-screen 8×1 slivers count toward the 34-tile-per-line
            // budget. Exception: a sprite at X=-256 ($100) has all its slivers
            // counted even though none are visible (hardware quirk).
            let tile_left = x + (tx as i32) * 8;
            let counts = x == -256 || (tile_left + 7 >= 0 && tile_left <= 255);
            if counts {
                if slivers >= 34 {
                    ppu.obj_time_over = true;
                    break 'outer;
                }
                slivers += 1;
            }

            for fx in 0..8u16 {
                let screen_col = tx * 8 + fx;
                let sx = x + screen_col as i32;
                if sx < 0 || sx >= 256 {
                    continue;
                }
                // H-flip mirrors the whole sprite: content column = w-1-screen.
                let scol = if hflip {
                    width - 1 - screen_col
                } else {
                    screen_col
                };
                let px = fetch_pixel(ppu, base_tile, scol, srow);
                if px == 0 {
                    continue;
                }
                // Reverse iteration means lower OAM index (processed later)
                // overwrites and lands on top.
                out[sx as usize] = ObjPixel {
                    color: (color_base + px as u16) as u8,
                    priority,
                    palette,
                    opaque: true,
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_sprite(ppu: &mut Ppu, idx: usize, x: u16, y: u8, tile: u8, attr: u8) {
        ppu.oam_lo[idx * 4] = (x & 0xFF) as u8;
        ppu.oam_lo[idx * 4 + 1] = y;
        ppu.oam_lo[idx * 4 + 2] = tile;
        ppu.oam_lo[idx * 4 + 3] = attr;
        let shift = (idx & 3) * 2;
        let byte = &mut ppu.oam_hi[idx >> 2];
        *byte &= !(0b11 << shift);
        *byte |= (((x >> 8) & 1) as u8) << shift; // X bit8
    }

    fn set_sprite_size(ppu: &mut Ppu, idx: usize, large: bool) {
        let shift = (idx & 3) * 2 + 1;
        let byte = &mut ppu.oam_hi[idx >> 2];
        if large {
            *byte |= 1 << shift;
        } else {
            *byte &= !(1 << shift);
        }
    }

    #[test]
    fn single_sprite_pixel_color_priority() {
        let mut ppu = Ppu::new();
        // 8×8 sprites (OBSEL size mode 0 small), name base 0.
        ppu.write(0x01, 0x00);
        // Tile 0, 4bpp: top-left pixel value 5 (planes 0 and 2 set at bit7).
        ppu.vram[0] = 0x0080; // plane0 = 0x80
        ppu.vram[8] = 0x0080; // plane2 = 0x80
        // X=100, Y=50, tile 0, palette 2, priority 1, no flip.
        let attr = (1 << 4) | (2 << 1); // priority=1, palette=2
        set_sprite(&mut ppu, 0, 100, 50, 0, attr);

        let mut out = [ObjPixel::default(); 256];
        render_obj_line(&mut ppu, 50, &mut out);

        let p = out[100];
        assert!(p.opaque);
        assert_eq!(p.color, 128 + 2 * 16 + 5);
        assert_eq!(p.priority, 1);
        assert_eq!(p.palette, 2);
        // Neighbouring column (pixel value 0) is transparent.
        assert!(!out[101].opaque);
        assert!(!ppu.obj_range_over);
        assert!(!ppu.obj_time_over);
    }

    #[test]
    fn hflip_moves_pixel_to_right_edge() {
        let mut ppu = Ppu::new();
        ppu.write(0x01, 0x00);
        ppu.vram[0] = 0x0080;
        ppu.vram[8] = 0x0080;
        let attr = 0x40; // H-flip, priority 0, palette 0
        set_sprite(&mut ppu, 0, 100, 50, 0, attr);

        let mut out = [ObjPixel::default(); 256];
        render_obj_line(&mut ppu, 50, &mut out);
        // Left pixel of the 8×8 tile is mirrored to the rightmost column.
        assert!(!out[100].opaque);
        assert!(out[107].opaque);
        assert_eq!(out[107].color, 128 + 5);
    }

    #[test]
    fn range_over_at_33_sprites() {
        let mut ppu = Ppu::new();
        ppu.write(0x01, 0x00); // 8×8 small
        // 33 sprites all intersecting line 0 (Y=0), spread across X.
        for i in 0..33usize {
            set_sprite(&mut ppu, i, (i as u16) * 7, 0, 0, 0);
        }
        let mut out = [ObjPixel::default(); 256];
        render_obj_line(&mut ppu, 0, &mut out);
        assert!(ppu.obj_range_over);
        // 33 × 1 sliver = 33 ≤ 34, so no time over.
        assert!(!ppu.obj_time_over);
    }

    #[test]
    fn time_over_beyond_34_slivers() {
        let mut ppu = Ppu::new();
        // OBSEL size mode 0: large = 16×16 → 2 slivers/line.
        ppu.write(0x01, 0x00);
        // Park all sprites off line 0 (default Y=0 would intersect).
        for i in 0..128usize {
            ppu.oam_lo[i * 4 + 1] = 240;
        }
        // 18 large sprites × 2 slivers = 36 > 34, and 18 ≤ 32 (no range over).
        for i in 0..18usize {
            set_sprite(&mut ppu, i, (i as u16) * 3, 0, 0, 0);
            set_sprite_size(&mut ppu, i, true);
        }
        let mut out = [ObjPixel::default(); 256];
        render_obj_line(&mut ppu, 0, &mut out);
        assert!(ppu.obj_time_over);
        assert!(!ppu.obj_range_over);
    }

    #[test]
    fn lower_index_sprite_wins_on_overlap() {
        let mut ppu = Ppu::new();
        ppu.write(0x01, 0x00);
        // Two opaque tiles: tile 0 pixel value 1, tile 1 pixel value 2.
        ppu.vram[0] = 0x0080; // tile 0 plane0 bit7
        ppu.vram[16] = 0x0080; // tile 1 (word +16) plane0 bit7
        set_sprite(&mut ppu, 0, 40, 0, 0, 1 << 1); // palette 1
        set_sprite(&mut ppu, 1, 40, 0, 1, 2 << 1); // palette 2, same X
        let mut out = [ObjPixel::default(); 256];
        render_obj_line(&mut ppu, 0, &mut out);
        // Sprite 0 (lower index) is in front.
        assert_eq!(out[40].palette, 1);
        assert_eq!(out[40].color, 128 + 16 + 1);
    }

    #[test]
    fn offscreen_slivers_do_not_consume_time_budget() {
        let mut ppu = Ppu::new();
        ppu.write(0x01, 0x00); // OBSEL mode 0: large = 16×16 → 2 slivers.
        for i in 0..128usize {
            ppu.oam_lo[i * 4 + 1] = 240;
        }
        // 32 large sprites at X=255: tile0 (255..262) is on screen and counts;
        // tile1 (263..270) is off the right edge and must NOT count. So each
        // sprite consumes 1 sliver → 32 ≤ 34, no time over. The old code counted
        // both tiles (64 slivers) and wrongly flagged time over.
        for i in 0..32usize {
            set_sprite(&mut ppu, i, 255, 0, 0, 0);
            set_sprite_size(&mut ppu, i, true);
        }
        let mut out = [ObjPixel::default(); 256];
        render_obj_line(&mut ppu, 0, &mut out);
        assert!(!ppu.obj_time_over);
        assert!(!ppu.obj_range_over);
    }

    #[test]
    fn range_time_flags_are_sticky_across_lines() {
        let mut ppu = Ppu::new();
        ppu.write(0x01, 0x00);
        // 33 sprites intersect line 0 → range over on that line.
        for i in 0..33usize {
            set_sprite(&mut ppu, i, (i as u16) * 7, 0, 0, 0);
        }
        let mut out = [ObjPixel::default(); 256];
        render_obj_line(&mut ppu, 0, &mut out);
        assert!(ppu.obj_range_over);
        // A later clean line (no sprite in range) must NOT clear the flag.
        for i in 0..128usize {
            ppu.oam_lo[i * 4 + 1] = 240;
        }
        render_obj_line(&mut ppu, 5, &mut out);
        assert!(ppu.obj_range_over);
        // Only the per-frame pre-render clear resets it.
        ppu.start_frame();
        assert!(!ppu.obj_range_over);
        assert!(!ppu.obj_time_over);
    }

    #[test]
    fn priority_rotation_first_sprite_wins() {
        let mut ppu = Ppu::new();
        ppu.write(0x01, 0x00);
        ppu.vram[0] = 0x0080; // tile 0 pixel value 1
        ppu.vram[16] = 0x0080; // tile 1 pixel value 1
        for i in 0..128usize {
            ppu.oam_lo[i * 4 + 1] = 240;
        }
        set_sprite(&mut ppu, 0, 40, 0, 0, 1 << 1); // palette 1
        set_sprite(&mut ppu, 1, 40, 0, 1, 2 << 1); // palette 2, same X
        // $2103 bit7 rotation on, OAMADDL=2 → FirstSprite=(2&$FE)>>1=1.
        ppu.write(0x02, 0x02);
        ppu.write(0x03, 0x80);
        let mut out = [ObjPixel::default(); 256];
        render_obj_line(&mut ppu, 0, &mut out);
        // Rotation makes sprite 1 the highest-priority (front) sprite.
        assert_eq!(out[40].palette, 2);
    }

    #[test]
    fn offscreen_negative_x_still_consumes_range() {
        let mut ppu = Ppu::new();
        ppu.write(0x01, 0x00);
        // 33 sprites at X=-256 (byte0=0, X bit8=1): all offscreen but still
        // consume range slots → range over with no visible pixels.
        for i in 0..33usize {
            set_sprite(&mut ppu, i, 0x100, 0, 0, 0);
        }
        let mut out = [ObjPixel::default(); 256];
        render_obj_line(&mut ppu, 0, &mut out);
        assert!(ppu.obj_range_over);
        assert!(out.iter().all(|p| !p.opaque));
    }
}
