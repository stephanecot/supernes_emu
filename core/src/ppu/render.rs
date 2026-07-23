//! Scanline compositor: builds the MAIN and SUB screen candidate pixels
//! independently (per-mode priority order ppu.md §9, per-layer window masks),
//! then applies color math (ppu.md §11), master brightness and forced blank
//! (ppu.md §15), writing BGR555 into `ppu.framebuffer`.
//!
//! Hires output (true hires modes 5/6, or pseudo-hires $2133 bit3 in a low-res
//! mode) produces 512 half-dots: the subscreen occupies the left/even half-dots
//! and the main screen the right/odd half-dots (fullsnes "shift subscreen half
//! dot to the left"; ppu.md §11). The two half-dots per screen column are
//! averaged into the 256-wide framebuffer. In true hires the two BG passes
//! sample different half-dots of the 512-dot field; in pseudo-hires both passes
//! sample the same low-res BG and only the main/sub split differs.

use crate::ppu::background::render_bg_line;
use crate::ppu::mode7::render_mode7_line;
use crate::ppu::window;
use crate::ppu::{LayerPixel, ObjPixel, Ppu};
use crate::{SCREEN_HEIGHT, SCREEN_WIDTH};

/// A layer slot in a priority order: an OBJ priority class or a BG at a given
/// tilemap-priority bit.
#[derive(Clone, Copy)]
enum Src {
    /// OBJ pixels whose priority (OAM byte3 bits5-4) equals this value.
    Obj(u8),
    /// BG `index` (0=BG1) pixels whose tilemap priority bit equals `prio`.
    Bg(usize, u8),
}

use Src::{Bg, Obj};

// Front -> back priority tables, ppu.md §9. Sn = OBJ priority n; nH/nL = BG n
// tilemap priority 1/0.

// Mode 0: S3 1H 2H S2 1L 2L S1 3H 4H S0 3L 4L
const ORDER0: &[Src] = &[
    Obj(3), Bg(0, 1), Bg(1, 1), Obj(2), Bg(0, 0), Bg(1, 0),
    Obj(1), Bg(2, 1), Bg(3, 1), Obj(0), Bg(2, 0), Bg(3, 0),
];
// Mode 1, BGMODE bit3=0: S3 1H 2H S2 1L 2L S1 3H S0 3L
const ORDER1: &[Src] = &[
    Obj(3), Bg(0, 1), Bg(1, 1), Obj(2), Bg(0, 0), Bg(1, 0),
    Obj(1), Bg(2, 1), Obj(0), Bg(2, 0),
];
// Mode 1, BGMODE bit3=1: 3H S3 1H 2H S2 1L 2L S1 S0 3L
const ORDER1_BG3: &[Src] = &[
    Bg(2, 1), Obj(3), Bg(0, 1), Bg(1, 1), Obj(2), Bg(0, 0),
    Bg(1, 0), Obj(1), Obj(0), Bg(2, 0),
];
// Modes 2,3,4,5: S3 1H S2 2H S1 1L S0 2L
const ORDER2345: &[Src] = &[
    Obj(3), Bg(0, 1), Obj(2), Bg(1, 1), Obj(1), Bg(0, 0), Obj(0), Bg(1, 0),
];
// Mode 6: S3 1H S2 S1 1L S0
const ORDER6: &[Src] = &[Obj(3), Bg(0, 1), Obj(2), Obj(1), Bg(0, 0), Obj(0)];
// Mode 7: S3 S2 S1 1 S0 (BG1 has no priority bit; both bits treated the same).
const ORDER7: &[Src] = &[Obj(3), Obj(2), Obj(1), Bg(0, 0), Bg(0, 1), Obj(0)];
// Mode 7 + EXTBG: S3 S2 2H S1 1 S0 2L (BG2 priority = pixel bit7).
const ORDER7_EXTBG: &[Src] = &[
    Obj(3), Obj(2), Bg(1, 1), Obj(1), Bg(0, 0), Bg(0, 1), Obj(0), Bg(1, 0),
];

fn priority_order(mode: u8, bg3_priority: bool, extbg: bool) -> &'static [Src] {
    match mode {
        0 => ORDER0,
        1 => {
            if bg3_priority {
                ORDER1_BG3
            } else {
                ORDER1
            }
        }
        2 | 3 | 4 | 5 => ORDER2345,
        6 => ORDER6,
        _ => {
            if extbg {
                ORDER7_EXTBG
            } else {
                ORDER7
            }
        }
    }
}

/// A resolved topmost pixel for one screen.
struct Resolved {
    /// CGRAM index (palette base folded in), or 8bpp value for BG1 direct color.
    idx: u8,
    /// CGADSUB bit index: 0-3 = BG1-4, 4 = OBJ, 5 = backdrop.
    math_bit: u8,
    /// OBJ palette 0-7 (color math only applies to OBJ palettes 4-7).
    obj_pal: u8,
}

/// Topmost visible pixel for one screen at column `x`, honoring layer enables
/// (`enables`: bit4 OBJ, bits3-0 BG4..BG1) and per-layer window masking
/// (`win_enables`: same layout, inside-window ⇒ pixel removed).
fn resolve_screen(
    ppu: &Ppu,
    order: &[Src],
    bgs: &[[LayerPixel; 256]; 4],
    obj: &[ObjPixel; 256],
    enables: u8,
    win_enables: u8,
    x: usize,
) -> Option<Resolved> {
    for slot in order {
        match *slot {
            Obj(p) => {
                if enables & 0x10 != 0
                    && !(win_enables & 0x10 != 0 && window::active(ppu, window::W_OBJ, x))
                {
                    let px = &obj[x];
                    if px.opaque && px.priority == p {
                        return Some(Resolved {
                            idx: px.color,
                            math_bit: 4,
                            obj_pal: px.palette,
                        });
                    }
                }
            }
            Bg(i, prio) => {
                if enables & (1 << i) != 0
                    && !(win_enables & (1 << i) != 0 && window::active(ppu, i, x))
                {
                    let px = &bgs[i][x];
                    if px.opaque && px.priority == prio {
                        return Some(Resolved {
                            idx: px.color,
                            math_bit: i as u8,
                            obj_pal: 0,
                        });
                    }
                }
            }
        }
    }
    None
}

/// Direct color (CGWSEL bit0): 8bpp pixel `BBGGGRRR` → BGR555. The tilemap
/// palette low bits are assumed 0 (mode 7 sets them 0; modes 3/4 approximate).
fn direct_color(idx: u8) -> u16 {
    let r = ((idx & 0x07) as u16) << 2;
    let g = (((idx >> 3) & 0x07) as u16) << 2;
    let b = (((idx >> 6) & 0x03) as u16) << 3;
    r | (g << 5) | (b << 10)
}

/// Resolve a `Resolved` to a BGR555 color, applying direct color to BG1 in the
/// 8bpp-capable modes (3/4/7) when CGWSEL bit0 is set.
fn resolve_color(ppu: &Ppu, r: &Resolved) -> u16 {
    if r.math_bit == 0 && ppu.cgwsel & 0x01 != 0 && matches!(ppu.bg_mode, 3 | 4 | 7) {
        direct_color(r.idx)
    } else {
        ppu.cgram[r.idx as usize]
    }
}

/// COLDATA fixed color (also the subscreen backdrop).
fn fixed_color(ppu: &Ppu) -> u16 {
    (ppu.coldata_r as u16) | ((ppu.coldata_g as u16) << 5) | ((ppu.coldata_b as u16) << 10)
}

/// One 5-bit color-math channel. Add saturates at 31; subtract clamps at 0;
/// half is applied after the clamp (add+half halves the 6-bit sum = true
/// average). ppu.md §11.
#[inline]
fn cmath_channel(a: u16, b: u16, subtract: bool, half: bool) -> u16 {
    let sum = if subtract { a.saturating_sub(b) } else { a + b };
    if half {
        sum >> 1
    } else {
        sum.min(31)
    }
}

fn color_math(main: u16, addend: u16, subtract: bool, half: bool) -> u16 {
    let r = cmath_channel(main & 0x1F, addend & 0x1F, subtract, half);
    let g = cmath_channel((main >> 5) & 0x1F, (addend >> 5) & 0x1F, subtract, half);
    let b = cmath_channel((main >> 10) & 0x1F, (addend >> 10) & 0x1F, subtract, half);
    r | (g << 5) | (b << 10)
}

/// Master-brightness scale a 5-bit channel. INIDISP $2100: brightness 0 =
/// screen black (0/16), N=1..15 = c × (N+1)/16 (fullsnes INIDISP; ppu.md §15).
#[inline]
fn scale_channel(c: u16, brightness: u8) -> u16 {
    if brightness == 0 {
        0
    } else {
        (c * (brightness as u16 + 1)) >> 4
    }
}

fn apply_brightness(color: u16, brightness: u8) -> u16 {
    let r = scale_channel(color & 0x1F, brightness);
    let g = scale_channel((color >> 5) & 0x1F, brightness);
    let b = scale_channel((color >> 10) & 0x1F, brightness);
    r | (g << 5) | (b << 10)
}

/// Per-channel average of two BGR555 colors (the two hires half-dots blur into
/// one 256-wide framebuffer pixel).
fn average(a: u16, b: u16) -> u16 {
    let r = ((a & 0x1F) + (b & 0x1F)) >> 1;
    let g = (((a >> 5) & 0x1F) + ((b >> 5) & 0x1F)) >> 1;
    let bl = (((a >> 10) & 0x1F) + ((b >> 10) & 0x1F)) >> 1;
    r | (g << 5) | (bl << 10)
}

/// Resolve the main-screen pixel at column `x` (topmost of `main_bgs`) and apply
/// color math (ppu.md §11), returning the pre-brightness BGR555 color. The color
/// math addend, when CGWSEL bit1 selects the subscreen, is the topmost pixel of
/// `sub_bgs`. Outside hires `main_bgs` and `sub_bgs` are the same array.
fn composite_main(
    ppu: &Ppu,
    order: &[Src],
    main_bgs: &[[LayerPixel; 256]; 4],
    sub_bgs: &[[LayerPixel; 256]; 4],
    obj: &[ObjPixel; 256],
    x: usize,
) -> u16 {
    let main = resolve_screen(ppu, order, main_bgs, obj, ppu.main_screen, ppu.main_window, x);
    let (mut main_color, main_bit, is_obj, obj_pal) = match &main {
        Some(r) => (resolve_color(ppu, r), r.math_bit, r.math_bit == 4, r.obj_pal),
        // No main layer -> backdrop (CGRAM color 0), CGADSUB bit5.
        None => (ppu.cgram[0], 5, false, 0),
    };

    // CGWSEL 7-6: force the main-screen pixel to black (math may still add).
    let fb = window::force_black_region(ppu, x);
    if fb {
        main_color = 0;
    }

    let prevent = window::prevent_math_region(ppu, x);
    let layer_math = ppu.cgadsub & (1 << main_bit) != 0;
    let obj_ok = !is_obj || obj_pal >= 4;

    if !prevent && layer_math && obj_ok {
        let use_sub = ppu.cgwsel & 0x02 != 0;
        let (addend, sub_transparent) = if use_sub {
            match resolve_screen(ppu, order, sub_bgs, obj, ppu.sub_screen, ppu.sub_window, x) {
                Some(r) => (resolve_color(ppu, &r), false),
                // Transparent subscreen -> fixed color, half suppressed.
                None => (fixed_color(ppu), true),
            }
        } else {
            (fixed_color(ppu), false)
        };
        let subtract = ppu.cgadsub & 0x80 != 0;
        let half = ppu.cgadsub & 0x40 != 0 && !fb && !(use_sub && sub_transparent);
        color_math(main_color, addend, subtract, half)
    } else {
        main_color
    }
}

/// Subscreen pixel shown on the hires left/even half-dot: topmost subscreen
/// layer, or the subscreen backdrop (COLDATA fixed color) when transparent
/// (ppu.md §11). No color math is applied to the subscreen half-dot.
fn subscreen_display(
    ppu: &Ppu,
    order: &[Src],
    sub_bgs: &[[LayerPixel; 256]; 4],
    obj: &[ObjPixel; 256],
    x: usize,
) -> u16 {
    match resolve_screen(ppu, order, sub_bgs, obj, ppu.sub_screen, ppu.sub_window, x) {
        Some(r) => resolve_color(ppu, &r),
        None => fixed_color(ppu),
    }
}

/// Composite one visible scanline (`line` = row 0..=223) into `ppu.framebuffer`.
pub fn render_scanline(ppu: &mut Ppu, line: u16) {
    let row = line as usize;
    if row >= SCREEN_HEIGHT {
        return;
    }
    let base = row * SCREEN_WIDTH;
    let brightness = ppu.brightness;

    // Forced blank: screen black regardless of contents.
    if ppu.forced_blank {
        ppu.framebuffer.0[base..base + SCREEN_WIDTH].fill(0);
        return;
    }

    let mut obj = [ObjPixel::default(); 256];
    crate::ppu::sprites::render_obj_line(ppu, line, &mut obj);

    // True hires (5/6) samples different half-dots per pass; pseudo-hires blends
    // the main/sub screens on alternating half-dots in a low-res mode. Mode 7 is
    // never hires and keeps the single-pass path.
    let hires_output =
        (matches!(ppu.bg_mode, 5 | 6) || ppu.pseudo_hires) && ppu.bg_mode != 7;

    let mut bgs_main = [[LayerPixel::default(); 256]; 4];
    let mut bgs_sub = [[LayerPixel::default(); 256]; 4];
    if ppu.bg_mode == 7 {
        let mut m7 = [0u8; 256];
        render_mode7_line(ppu, line, &mut m7);
        for x in 0..SCREEN_WIDTH {
            let v = m7[x];
            bgs_main[0][x] = LayerPixel {
                color: v,
                priority: 0,
                opaque: v != 0,
            };
            if ppu.extbg {
                // EXTBG BG2: bit7 = priority, low 7 bits = color index.
                let c = v & 0x7F;
                bgs_main[1][x] = LayerPixel {
                    color: c,
                    priority: (v >> 7) & 1,
                    opaque: c != 0,
                };
            }
        }
    } else {
        for i in 0..4 {
            // Main screen = odd (right) half-dot; subscreen = even (left).
            render_bg_line(ppu, i, line, 1, &mut bgs_main[i]);
            if hires_output {
                render_bg_line(ppu, i, line, 0, &mut bgs_sub[i]);
            }
        }
    }

    let order = priority_order(ppu.bg_mode, ppu.bg3_priority, ppu.extbg);

    if hires_output {
        for x in 0..SCREEN_WIDTH {
            let main_c = composite_main(ppu, order, &bgs_main, &bgs_sub, &obj, x);
            let sub_c = subscreen_display(ppu, order, &bgs_sub, &obj, x);
            ppu.framebuffer.0[base + x] = average(
                apply_brightness(main_c, brightness),
                apply_brightness(sub_c, brightness),
            );
        }
    } else {
        for x in 0..SCREEN_WIDTH {
            let out = composite_main(ppu, order, &bgs_main, &bgs_main, &obj, x);
            ppu.framebuffer.0[base + x] = apply_brightness(out, brightness);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bg_px(color: u8, priority: u8) -> LayerPixel {
        LayerPixel {
            color,
            priority,
            opaque: true,
        }
    }
    fn obj_px(color: u8, priority: u8) -> ObjPixel {
        ObjPixel {
            color,
            priority,
            palette: 0,
            opaque: true,
        }
    }

    fn idx_of(r: Option<Resolved>) -> Option<u8> {
        r.map(|r| r.idx)
    }

    #[test]
    fn mode1_default_bg1_over_bg2_same_priority() {
        let ppu = Ppu::new();
        let mut bgs = [[LayerPixel::default(); 256]; 4];
        bgs[0][0] = bg_px(10, 0);
        bgs[1][0] = bg_px(20, 0);
        let obj = [ObjPixel::default(); 256];
        let order = priority_order(1, false, false);
        assert_eq!(
            idx_of(resolve_screen(&ppu, order, &bgs, &obj, 0x0F, 0, 0)),
            Some(10)
        );
    }

    #[test]
    fn mode1_bg3_priority_lift() {
        let ppu = Ppu::new();
        let mut bgs = [[LayerPixel::default(); 256]; 4];
        bgs[0][0] = bg_px(10, 1);
        bgs[2][0] = bg_px(30, 1);
        let obj = [ObjPixel::default(); 256];
        let o0 = priority_order(1, false, false);
        assert_eq!(
            idx_of(resolve_screen(&ppu, o0, &bgs, &obj, 0x0F, 0, 0)),
            Some(10)
        );
        let o1 = priority_order(1, true, false);
        assert_eq!(
            idx_of(resolve_screen(&ppu, o1, &bgs, &obj, 0x0F, 0, 0)),
            Some(30)
        );
    }

    #[test]
    fn obj_priority_interleaves_with_bg() {
        let ppu = Ppu::new();
        let mut bgs = [[LayerPixel::default(); 256]; 4];
        bgs[0][0] = bg_px(10, 1);
        let mut obj = [ObjPixel::default(); 256];
        obj[0] = obj_px(200, 3);
        let order = priority_order(1, false, false);
        assert_eq!(
            idx_of(resolve_screen(&ppu, order, &bgs, &obj, 0x1F, 0, 0)),
            Some(200)
        );
        obj[0] = obj_px(200, 1);
        assert_eq!(
            idx_of(resolve_screen(&ppu, order, &bgs, &obj, 0x1F, 0, 0)),
            Some(10)
        );
    }

    #[test]
    fn main_screen_disable_removes_layer() {
        let ppu = Ppu::new();
        let mut bgs = [[LayerPixel::default(); 256]; 4];
        bgs[0][0] = bg_px(10, 0);
        bgs[1][0] = bg_px(20, 0);
        let obj = [ObjPixel::default(); 256];
        let order = priority_order(1, false, false);
        assert_eq!(
            idx_of(resolve_screen(&ppu, order, &bgs, &obj, 0x0E, 0, 0)),
            Some(20)
        );
    }

    #[test]
    fn window_masks_a_layer() {
        let mut ppu = Ppu::new();
        // BG1 W1 enabled, range [0,10].
        ppu.w12sel = 0b10;
        ppu.w1_left = 0;
        ppu.w1_right = 10;
        let mut bgs = [[LayerPixel::default(); 256]; 4];
        bgs[0][5] = bg_px(10, 0);
        bgs[1][5] = bg_px(20, 0);
        let obj = [ObjPixel::default(); 256];
        let order = priority_order(1, false, false);
        // Window masking off for BG1 -> BG1 wins.
        assert_eq!(
            idx_of(resolve_screen(&ppu, order, &bgs, &obj, 0x0F, 0x00, 5)),
            Some(10)
        );
        // Enable BG1 masking (win bit0): x=5 is inside window -> BG1 removed, BG2.
        assert_eq!(
            idx_of(resolve_screen(&ppu, order, &bgs, &obj, 0x0F, 0x01, 5)),
            Some(20)
        );
        // Outside the window (x=15) BG1 is not masked.
        bgs[0][15] = bg_px(10, 0);
        bgs[1][15] = bg_px(20, 0);
        assert_eq!(
            idx_of(resolve_screen(&ppu, order, &bgs, &obj, 0x0F, 0x01, 15)),
            Some(10)
        );
    }

    #[test]
    fn backdrop_when_all_transparent() {
        let ppu = Ppu::new();
        let bgs = [[LayerPixel::default(); 256]; 4];
        let obj = [ObjPixel::default(); 256];
        let order = priority_order(0, false, false);
        assert!(resolve_screen(&ppu, order, &bgs, &obj, 0x1F, 0, 0).is_none());
    }

    #[test]
    fn color_math_add_sub_half() {
        // add, no half, no saturation.
        assert_eq!(cmath_channel(10, 5, false, false), 15);
        // add saturates at 31.
        assert_eq!(cmath_channel(20, 20, false, false), 31);
        // add + half = true average of the 6-bit sum (no early clamp).
        assert_eq!(cmath_channel(20, 20, false, true), 20);
        // subtract clamps at 0.
        assert_eq!(cmath_channel(5, 10, true, false), 0);
        // subtract then half.
        assert_eq!(cmath_channel(20, 4, true, true), 8);

        // Full-color add on packed BGR555.
        let m = 10 | (10 << 5) | (10 << 10);
        let a = 5 | (5 << 5) | (5 << 10);
        assert_eq!(color_math(m, a, false, false), 15 | (15 << 5) | (15 << 10));
    }

    #[test]
    fn coldata_channel_accumulation() {
        let mut ppu = Ppu::new();
        // Write red only.
        ppu.write(0x32, 0x20 | 0x0A);
        assert_eq!(ppu.coldata_r, 0x0A);
        assert_eq!(ppu.coldata_g, 0);
        // Write green only; red retained.
        ppu.write(0x32, 0x40 | 0x05);
        assert_eq!(ppu.coldata_r, 0x0A);
        assert_eq!(ppu.coldata_g, 0x05);
        // Write blue only; red+green retained.
        ppu.write(0x32, 0x80 | 0x1F);
        assert_eq!(fixed_color(&ppu), 0x0A | (0x05 << 5) | (0x1F << 10));
    }

    #[test]
    fn brightness_scaling() {
        assert_eq!(apply_brightness(0x7FFF, 15), 0x7FFF);
        assert_eq!(scale_channel(31, 15), 31);
        assert_eq!(scale_channel(31, 7), (31 * 8) >> 4);
        // Brightness 0 = screen black, not c/16.
        assert_eq!(scale_channel(16, 0), 0);
        assert_eq!(scale_channel(31, 0), 0);
        assert_eq!(apply_brightness(0x7FFF, 0), 0);
    }

    #[test]
    fn subscreen_add_composites_through_color_math() {
        let mut ppu = Ppu::new();
        ppu.bg_mode = 1;
        ppu.brightness = 15;
        ppu.main_screen = 0x02; // BG2 on main
        ppu.sub_screen = 0x01; // BG1 on sub
        ppu.cgwsel = 0x02; // addend = subscreen
        ppu.cgadsub = 0x20 | 0x02; // backdrop math enable + BG2 math enable, add
        ppu.cgram[0] = 0; // backdrop black
        ppu.cgram[1] = 0x0210; // BG1 (sub) color
        ppu.cgram[2 * 16 + 1] = 0x0004; // BG2 (main) palette2 idx1

        // BG1 tile -> palette 0 idx 1 at column 0.
        ppu.bg_char_base[0] = 0x0000;
        ppu.vram[16] = 0x0080; // 4bpp tile1 row0 col0 -> value 1
        ppu.vram[0] = 1; // BG1 map entry tile 1
                         // BG2 tile -> palette 2 idx 1.
        ppu.bg_char_base[1] = 0x0000;
        ppu.bg_map_base[1] = 0x0400;
        ppu.vram[0x0400] = 1 | (2 << 10); // BG2 map entry tile 1 palette 2
        ppu.render_scanline(0);
        // Main BG2 (0x0004) + Sub BG1 (0x0210) added per channel.
        let expected = color_math(0x0004, 0x0210, false, false);
        assert_eq!(ppu.framebuffer.0[0], expected);
    }

    #[test]
    fn pseudo_hires_averages_main_and_sub_half_dots() {
        let mut ppu = Ppu::new();
        ppu.bg_mode = 1;
        ppu.brightness = 15;
        ppu.pseudo_hires = true; // $2133 bit3
        ppu.main_screen = 0x01; // BG1 on main
        ppu.sub_screen = 0x02; // BG2 on sub
        ppu.cgwsel = 0;
        ppu.cgadsub = 0; // no color math
        ppu.cgram[0] = 0;
        // BG1 (main) tile 1 -> palette 0 idx 1 -> cgram[1] = red 31.
        ppu.bg_char_base[0] = 0x0000;
        ppu.vram[16] = 0x0080; // 4bpp tile1 row0 col0 value 1
        ppu.vram[0] = 1;
        ppu.cgram[1] = 0x001F;
        // BG2 (sub) tile 1 palette 1 -> cgram[17] = blue 31.
        ppu.bg_char_base[1] = 0x0000;
        ppu.bg_map_base[1] = 0x0400;
        ppu.vram[0x0400] = 1 | (1 << 10);
        ppu.cgram[17] = 0x7C00;

        ppu.render_scanline(0);

        // Left half-dot = subscreen (BG2), right half-dot = main (BG1), averaged.
        let expected = average(apply_brightness(0x001F, 15), apply_brightness(0x7C00, 15));
        assert_eq!(ppu.framebuffer.0[0], expected);
    }

    #[test]
    fn non_hires_output_unchanged_by_hires_path() {
        // With pseudo-hires off the compositor must take the single-pass path.
        let mut ppu = Ppu::new();
        ppu.bg_mode = 1;
        ppu.brightness = 15;
        ppu.main_screen = 0x01;
        ppu.cgram[0] = 0x1234;
        ppu.bg_char_base[0] = 0x0000;
        ppu.vram[16] = 0x0080;
        ppu.vram[0] = 1;
        ppu.cgram[1] = 0x7FFF;
        ppu.render_scanline(0);
        assert_eq!(ppu.framebuffer.0[0], 0x7FFF);
        assert_eq!(ppu.framebuffer.0[1], 0x1234);
    }

    #[test]
    fn forced_blank_and_render_write_row() {
        let mut ppu = Ppu::new();
        ppu.bg_mode = 1;
        ppu.brightness = 15;
        ppu.main_screen = 0x01;
        ppu.cgram[0] = 0x1234;
        ppu.bg_char_base[0] = 0x0000;
        ppu.vram[16] = 0x0080;
        ppu.vram[0] = 1;
        ppu.cgram[1] = 0x7FFF;
        ppu.render_scanline(0);
        assert_eq!(ppu.framebuffer.0[0], 0x7FFF);
        assert_eq!(ppu.framebuffer.0[1], 0x1234);

        ppu.forced_blank = true;
        ppu.render_scanline(0);
        assert_eq!(ppu.framebuffer.0[0], 0);
    }
}
