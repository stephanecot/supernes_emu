//! Windows W1/W2: per-layer enable/invert, AND/OR/XOR/XNOR combination, and
//! the color-math window regions (CGWSEL). Pure column tests the compositor
//! calls; no rendering. Layer indices: 0=BG1..3=BG4, 4=OBJ, 5=Color (ppu.md
//! §10-11).

use crate::ppu::Ppu;

pub const W_BG1: usize = 0;
pub const W_OBJ: usize = 4;
pub const W_COLOR: usize = 5;

/// Per-layer (W1 enable, W1 invert, W2 enable, W2 invert) from W12SEL/W34SEL/
/// WOBJSEL. Within each 2-bit field bit0 = invert (outside range counts as
/// inside), bit1 = enable.
fn win_sel(ppu: &Ppu, layer: usize) -> (bool, bool, bool, bool) {
    let bits = match layer {
        0 => ppu.w12sel & 0x0F,
        1 => (ppu.w12sel >> 4) & 0x0F,
        2 => ppu.w34sel & 0x0F,
        3 => (ppu.w34sel >> 4) & 0x0F,
        4 => ppu.wobjsel & 0x0F,
        _ => (ppu.wobjsel >> 4) & 0x0F,
    };
    (
        bits & 0x02 != 0,
        bits & 0x01 != 0,
        bits & 0x08 != 0,
        bits & 0x04 != 0,
    )
}

/// Per-layer combine op (0=OR, 1=AND, 2=XOR, 3=XNOR) from WBGLOG/WOBJLOG.
fn win_logic(ppu: &Ppu, layer: usize) -> u8 {
    match layer {
        0 => ppu.wbglog & 0x03,
        1 => (ppu.wbglog >> 2) & 0x03,
        2 => (ppu.wbglog >> 4) & 0x03,
        3 => (ppu.wbglog >> 6) & 0x03,
        4 => ppu.wobjlog & 0x03,
        _ => (ppu.wobjlog >> 2) & 0x03,
    }
}

/// Is column `x` inside the combined window area for `layer`? This is the raw
/// window region; the compositor gates it by TMW/TSW ($212E/$212F) before
/// removing a layer pixel.
pub fn active(ppu: &Ppu, layer: usize, x: usize) -> bool {
    let (w1en, w1inv, w2en, w2inv) = win_sel(ppu, layer);
    let x8 = x as u8;
    // Inclusive ranges; left>right yields an empty range (no x satisfies).
    let a1 = (x8 >= ppu.w1_left && x8 <= ppu.w1_right) ^ w1inv;
    let a2 = (x8 >= ppu.w2_left && x8 <= ppu.w2_right) ^ w2inv;
    match (w1en, w2en) {
        (false, false) => false,
        (true, false) => a1,
        (false, true) => a2,
        (true, true) => match win_logic(ppu, layer) {
            0 => a1 || a2,
            1 => a1 && a2,
            2 => a1 ^ a2,
            _ => !(a1 ^ a2),
        },
    }
}

/// CGWSEL region select (0=never, 1=outside color window, 2=inside, 3=always).
fn region(mode: u8, inside: bool) -> bool {
    match mode & 0x03 {
        0 => false,
        1 => !inside,
        2 => inside,
        _ => true,
    }
}

/// CGWSEL bits7-6: is the main-screen pixel at `x` forced to black?
pub fn force_black_region(ppu: &Ppu, x: usize) -> bool {
    region((ppu.cgwsel >> 6) & 0x03, active(ppu, W_COLOR, x))
}

/// CGWSEL bits5-4: is color math prevented at `x`?
pub fn prevent_math_region(ppu: &Ppu, x: usize) -> bool {
    region((ppu.cgwsel >> 4) & 0x03, active(ppu, W_COLOR, x))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_window_range_inclusive() {
        let mut ppu = Ppu::new();
        // BG1 W1 enabled, not inverted; range [10,20].
        ppu.w12sel = 0b10; // W1 enable bit for BG1
        ppu.w1_left = 10;
        ppu.w1_right = 20;
        assert!(!active(&ppu, W_BG1, 9));
        assert!(active(&ppu, W_BG1, 10));
        assert!(active(&ppu, W_BG1, 20));
        assert!(!active(&ppu, W_BG1, 21));
    }

    #[test]
    fn inverted_window() {
        let mut ppu = Ppu::new();
        ppu.w12sel = 0b11; // BG1 W1 enable + invert
        ppu.w1_left = 10;
        ppu.w1_right = 20;
        assert!(active(&ppu, W_BG1, 9));
        assert!(!active(&ppu, W_BG1, 15));
    }

    #[test]
    fn empty_when_left_gt_right() {
        let mut ppu = Ppu::new();
        ppu.w12sel = 0b10;
        ppu.w1_left = 200;
        ppu.w1_right = 10;
        assert!(!active(&ppu, W_BG1, 5));
        assert!(!active(&ppu, W_BG1, 205));
    }

    #[test]
    fn two_windows_and_or() {
        let mut ppu = Ppu::new();
        // BG1 W1 [0,50] and W2 [40,100], both enabled.
        ppu.w12sel = 0b1010; // W1 enable (bit1) + W2 enable (bit3)
        ppu.w1_left = 0;
        ppu.w1_right = 50;
        ppu.w2_left = 40;
        ppu.w2_right = 100;
        // OR (default logic 0).
        assert!(active(&ppu, W_BG1, 10));
        assert!(active(&ppu, W_BG1, 90));
        // AND (logic 1): only the overlap [40,50].
        ppu.wbglog = 0b01;
        assert!(!active(&ppu, W_BG1, 10));
        assert!(active(&ppu, W_BG1, 45));
        assert!(!active(&ppu, W_BG1, 90));
    }

    #[test]
    fn color_math_regions() {
        let mut ppu = Ppu::new();
        // Color window (layer 5) via WOBJSEL bits7-4: W1 enable, range [10,20].
        ppu.wobjsel = 0b0010_0000; // Color W1 enable
        ppu.w1_left = 10;
        ppu.w1_right = 20;
        // CGWSEL 5-4 = 2 (prevent math inside color window).
        ppu.cgwsel = 0b0010_0000;
        assert!(prevent_math_region(&ppu, 15));
        assert!(!prevent_math_region(&ppu, 5));
        // CGWSEL 7-6 = 3 (always force black).
        ppu.cgwsel = 0b1100_0000;
        assert!(force_black_region(&ppu, 0));
    }
}
