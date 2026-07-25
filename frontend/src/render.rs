//! CPU-side frame composition for the windowed present path (`docs/ROADMAP.md`
//! Phase 2 — Affichage): zoom, filtering and pixel-aspect-ratio (PAR)
//! correction with letterboxing/pillarboxing.
//!
//! Runs only from `video.rs`'s windowed present path. Headless `--dump-frame`/
//! `--dump-frame-every` and the `F12` screenshot both read `Snes::framebuffer`
//! directly through `crate::write_frame_png`, never through this module, so
//! they stay byte-identical to the emulated 256x224 core output regardless of
//! zoom/filter/aspect.
//!
//! **Shader vs. CPU (decision):** `pixels` 0.15's built-in scaling renderer
//! hard-codes a nearest-neighbor `wgpu::Sampler`
//! (`pixels-0.15.0/src/renderers.rs`, `mag_filter`/`min_filter`:
//! `FilterMode::Nearest`) with no public builder option to switch it to
//! linear — bilinear/CRT would need `pixels`' "custom shader" integration
//! path (owning a second wgpu pipeline, a hand-written WGSL module and its own
//! bind groups) for what is, per pixel, a small blend. Instead, `Pixels`'
//! buffer and surface are always kept at the *same* size (the physical window
//! size, see `App::apply_resize`), which makes its internal scaling pass a
//! 1:1, filter-irrelevant copy; every visible pixel is produced by this
//! module's CPU code instead. Cost is measured in this module's own tests
//! (`compose_frame_cost_stays_within_frame_budget`) — see the function doc
//! for the numbers measured on this development machine (Apple Silicon,
//! `--release`).

use snes_core::{SCREEN_HEIGHT, SCREEN_WIDTH};

/// Pixel-aspect-ratio (PAR) handling for the presented image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aspect {
    /// Square pixels: the emulated 256x224 buffer is scaled uniformly, no
    /// horizontal stretch.
    PixelPerfect,
    /// Corrects for the SNES's non-square dot clock: in the console's
    /// 256-pixel-wide modes each pixel is 8:7 (width:height) — about 14.3 %
    /// wider than tall — which a period CRT stretched into a ~4:3 picture.
    /// Source: https://forums.nesdev.org/viewtopic.php?t=23885 (8:7 PAR
    /// derived from the shared NES/SNES 256px-mode dot clock).
    Tv,
}

impl Aspect {
    /// Parses `prefs.aspect`; any value other than `"tv"` (including an
    /// unrecognized string from a hand-edited or newer-build file) falls
    /// back to pixel-perfect rather than failing — the stored string itself
    /// is left untouched by the caller, only the *rendering* falls back.
    pub fn from_pref(s: &str) -> Self {
        match s {
            "tv" => Aspect::Tv,
            _ => Aspect::PixelPerfect,
        }
    }

    pub fn as_pref(self) -> &'static str {
        match self {
            Aspect::PixelPerfect => "pixel-perfect",
            Aspect::Tv => "tv",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Aspect::PixelPerfect => Aspect::Tv,
            Aspect::Tv => Aspect::PixelPerfect,
        }
    }
}

/// Presentation filter, independent of `Aspect` and of the zoom factor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// Nearest-neighbor: sharp, blocky pixels. Default (decision recorded in
    /// `docs/ROADMAP.md` Phase 2).
    None,
    /// Bilinear interpolation.
    Smooth,
    /// Bilinear base (the "léger adoucissement" the roadmap asks for) plus
    /// scanlines: every other *source* scanline darkened, so the effect's
    /// density tracks the emulated 224-line picture rather than the output
    /// pixel grid (see `compose_frame`).
    Crt,
}

impl Filter {
    pub fn from_pref(s: &str) -> Self {
        match s {
            "smooth" => Filter::Smooth,
            "crt" => Filter::Crt,
            _ => Filter::None,
        }
    }

    pub fn as_pref(self) -> &'static str {
        match self {
            Filter::None => "none",
            Filter::Smooth => "smooth",
            Filter::Crt => "crt",
        }
    }

    /// `Aucun -> Lissé -> CRT -> Aucun`, used by the `V` hotkey and mirrors
    /// `menu::FILTER_IDS`' order.
    pub fn next(self) -> Self {
        match self {
            Filter::None => Filter::Smooth,
            Filter::Smooth => Filter::Crt,
            Filter::Crt => Filter::None,
        }
    }
}

/// Content rectangle (in output pixels) the emulated picture is drawn into;
/// the rest of the output buffer is filled with `LETTERBOX_COLOR`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Black, fully opaque: `pixels`' render target has no alpha blending stage
/// of its own here, but an explicit 255 keeps the buffer well-defined if that
/// ever changes.
const LETTERBOX_COLOR: [u8; 4] = [0, 0, 0, 255];

/// Darkened scanline brightness, as a percentage of the original channel
/// value (0..=100). 60 % is a common CRT-shader scanline strength: dark
/// enough to read as an "old TV" texture without crushing shadow detail.
const SCANLINE_BRIGHTNESS_PCT: u32 = 60;

/// The 256x224 content's size at the requested `Aspect`, before any zoom.
/// `Tv` stretches the width by 8:7 (see `Aspect::Tv`); height is untouched,
/// since the PAR correction is horizontal only.
pub fn content_dims(aspect: Aspect) -> (u32, u32) {
    match aspect {
        Aspect::PixelPerfect => (SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32),
        Aspect::Tv => {
            let w = (SCREEN_WIDTH as f64 * 8.0 / 7.0).round() as u32;
            (w, SCREEN_HEIGHT as u32)
        }
    }
}

/// `content_dims(aspect)` scaled by the integer zoom factor (clamped to at
/// least 1x so a corrupt/zero preference value can't request a zero-size
/// window).
pub fn zoomed_dims(zoom: u8, aspect: Aspect) -> (u32, u32) {
    let (w, h) = content_dims(aspect);
    let z = zoom.max(1) as u32;
    (w * z, h * z)
}

/// Scales `target` down to fit within `max`, preserving its aspect ratio, if
/// it doesn't already fit; otherwise returns `target` unchanged. Used to keep
/// the window usable when the requested zoom would exceed the screen (see
/// module docs and `App::resize_window_for_display_prefs`). A `max` with a
/// zero dimension is treated as "no limit" (can't compute a meaningful
/// monitor size) rather than collapsing the window to nothing.
pub fn clamp_to_available(target: (u32, u32), max: (u32, u32)) -> (u32, u32) {
    let (tw, th) = target;
    let (mw, mh) = max;
    if mw == 0 || mh == 0 || (tw <= mw && th <= mh) {
        return target;
    }
    let scale = (mw as f64 / tw as f64).min(mh as f64 / th as f64);
    let w = ((tw as f64 * scale).floor() as u32).max(1);
    let h = ((th as f64 * scale).floor() as u32).max(1);
    (w, h)
}

/// Largest rectangle with `aspect`'s content aspect ratio that fits inside a
/// freely-resized `window_w`x`window_h` window, centered — the rest is
/// letterboxed/pillarboxed by `compose_frame`. The window is resizable by
/// dragging its edges (`with_resizable(true)`, winit's default) as well as by
/// the zoom shortcuts, so this runs on every `WindowEvent::Resized`, not only
/// at a fixed zoom step.
///
/// `PixelPerfect` snaps to the *largest whole-number* scale that fits (e.g. a
/// 600x500 window only fits 256x224 at 2x = 512x448, never a continuous
/// 2.23x) so every emulated pixel maps to a whole number of output pixels —
/// required for `Filter::None` to stay genuinely sharp instead of showing
/// fractional-pixel shimmer. If the window is smaller than the native
/// picture in either dimension (no integer factor >= 1 fits), this falls
/// back to the same continuous scaling `Tv` always uses, rather than
/// cropping or overflowing the window. `Tv` never snaps to an integer
/// factor: its whole point is filling the period-accurate 8:7 shape, not
/// keeping square-pixel sharpness.
///
/// A zero-sized window or content aspect degenerates to a full-window rect
/// rather than dividing by zero.
pub fn letterbox(window_w: u32, window_h: u32, aspect: Aspect) -> Rect {
    let (cw, ch) = content_dims(aspect);
    if window_w == 0 || window_h == 0 || cw == 0 || ch == 0 {
        return Rect { x: 0, y: 0, w: window_w, h: window_h };
    }
    let fits_at_1x = window_w >= cw && window_h >= ch;
    let (w, h) = if aspect == Aspect::PixelPerfect && fits_at_1x {
        let scale = (window_w / cw).min(window_h / ch).max(1);
        (cw * scale, ch * scale)
    } else {
        let scale = (window_w as f64 / cw as f64).min(window_h as f64 / ch as f64);
        (
            ((cw as f64 * scale).floor() as u32).max(1),
            ((ch as f64 * scale).floor() as u32).max(1),
        )
    };
    let w = w.clamp(1, window_w);
    let h = h.clamp(1, window_h);
    let x = (window_w - w) / 2;
    let y = (window_h - h) / 2;
    Rect { x, y, w, h }
}

/// Nearest-neighbor sample of the native `SCREEN_WIDTH`x`SCREEN_HEIGHT` RGBA8
/// buffer; `sx`/`sy` must already be in bounds.
fn sample_nearest(native: &[u8], sx: usize, sy: usize) -> [u8; 4] {
    let idx = (sy * SCREEN_WIDTH + sx) * 4;
    [native[idx], native[idx + 1], native[idx + 2], native[idx + 3]]
}

/// Bilinear sample from precomputed neighbor coordinates and Q8 fixed-point
/// blend weights (`tx_q`/`ty_q`, 0..=256, where 256 is a full step to the
/// next source pixel). Fixed-point integer weights avoid a
/// multiply/round/clamp in `f64` per channel per output pixel — see
/// `compose_frame`'s cost-measurement test for the effect this and the `Col`
/// table below have on the worst-case (zoom x4, `Aspect::Tv`, `Filter::Crt`)
/// cost. Alpha is always 255 in the native buffer (`FrameBuffer::to_rgba`
/// never writes anything else), so only RGB is blended; the caller fills
/// alpha directly instead of paying for a fourth channel of interpolation
/// that always produces the same constant.
fn sample_bilinear(native: &[u8], x0: usize, x1: usize, tx_q: u32, y0: usize, y1: usize, ty_q: u32) -> [u8; 3] {
    let p00 = sample_nearest(native, x0, y0);
    let p10 = sample_nearest(native, x1, y0);
    let p01 = sample_nearest(native, x0, y1);
    let p11 = sample_nearest(native, x1, y1);
    let w00 = (256 - tx_q) * (256 - ty_q);
    let w10 = tx_q * (256 - ty_q);
    let w01 = (256 - tx_q) * ty_q;
    let w11 = tx_q * ty_q;
    let mut out = [0u8; 3];
    for c in 0..3 {
        let sum =
            p00[c] as u32 * w00 + p10[c] as u32 * w10 + p01[c] as u32 * w01 + p11[c] as u32 * w11;
        // `sum` is in 0..=(255 * 256 * 256); +32768 (half of 65536) before
        // the shift rounds to nearest instead of always truncating down.
        out[c] = ((sum + 32768) >> 16).min(255) as u8;
    }
    out
}

/// One output column's horizontal resampling coordinates, precomputed once
/// per `compose_frame` call and reused for every row (see `sample_bilinear`
/// doc). For nearest-neighbor sampling, `x0 == x1` and `tx_q` is unused.
struct Col {
    x0: usize,
    x1: usize,
    tx_q: u32,
}

/// Builds the `rect.w`-long column table `compose_frame` indexes once per
/// output pixel instead of recomputing a floor+division per pixel.
fn build_columns(rect_w: u32, bilinear: bool) -> Vec<Col> {
    (0..rect_w)
        .map(|ox| {
            if bilinear {
                let fx =
                    (((ox as f64 + 0.5) * SCREEN_WIDTH as f64 / rect_w as f64) - 0.5).max(0.0);
                let x0 = (fx.floor() as usize).min(SCREEN_WIDTH - 1);
                let x1 = (x0 + 1).min(SCREEN_WIDTH - 1);
                let tx_q = ((fx - x0 as f64).clamp(0.0, 1.0) * 256.0).round() as u32;
                Col { x0, x1, tx_q }
            } else {
                let native_col = (((ox as u64) * SCREEN_WIDTH as u64) / rect_w as u64)
                    .min(SCREEN_WIDTH as u64 - 1) as usize;
                Col { x0: native_col, x1: native_col, tx_q: 0 }
            }
        })
        .collect()
}

/// Composes the native `SCREEN_WIDTH`x`SCREEN_HEIGHT` RGBA8 `native` buffer
/// (already carrying the FPS/status overlays, drawn by `video.rs` before this
/// call) into `out`, an `out_w`x`out_h` RGBA8 buffer — normally `pixels`'
/// `frame_mut()`, kept at the window's physical size (see module docs).
///
/// `filter` selects nearest-neighbor (`None`) or bilinear (`Smooth`/`Crt`)
/// resampling; `Crt` additionally darkens every other *source* scanline
/// (`native_row % 2 == 1`) to `SCANLINE_BRIGHTNESS_PCT`, so the effect's
/// density is tied to the emulated 224-line picture rather than to the zoom
/// factor. `aspect` picks the content rectangle via `letterbox`; anything
/// outside it is filled with `LETTERBOX_COLOR`.
///
/// # Panics
/// `native.len()` must be exactly `SCREEN_WIDTH * SCREEN_HEIGHT * 4` and
/// `out.len()` exactly `out_w * out_h * 4` (both always true for their
/// callers in `video.rs`); violated by a debug_assert rather than a bounds
/// panic deep in the pixel loop.
pub fn compose_frame(
    native: &[u8],
    out: &mut [u8],
    out_w: u32,
    out_h: u32,
    filter: Filter,
    aspect: Aspect,
) {
    debug_assert_eq!(native.len(), SCREEN_WIDTH * SCREEN_HEIGHT * 4);
    debug_assert_eq!(out.len(), (out_w as usize) * (out_h as usize) * 4);

    for px in out.chunks_exact_mut(4) {
        px.copy_from_slice(&LETTERBOX_COLOR);
    }

    let rect = letterbox(out_w, out_h, aspect);
    if rect.w == 0 || rect.h == 0 {
        return;
    }

    let bilinear = matches!(filter, Filter::Smooth | Filter::Crt);
    // Horizontal resampling coordinates depend only on `ox`/`rect.w`, not on
    // the row: computed once here instead of `rect.h` times per column (see
    // `build_columns` doc and this function's cost-measurement test).
    let cols = build_columns(rect.w, bilinear);

    for oy in 0..rect.h {
        let y = rect.y + oy;
        // Which native scanline this output row is nearest to; used both for
        // nearest-neighbor sampling and to decide which rows `Crt` darkens,
        // independent of `filter`.
        let native_row =
            (((oy as u64) * SCREEN_HEIGHT as u64) / rect.h as u64).min(SCREEN_HEIGHT as u64 - 1)
                as usize;
        let darken = filter == Filter::Crt && native_row % 2 == 1;
        // Vertical resampling coordinates for this row, computed once
        // (mirrors `cols` on the horizontal axis).
        let (y0, y1, ty_q) = if bilinear {
            let fy = (((oy as f64 + 0.5) * SCREEN_HEIGHT as f64 / rect.h as f64) - 0.5).max(0.0);
            let y0 = (fy.floor() as usize).min(SCREEN_HEIGHT - 1);
            let y1 = (y0 + 1).min(SCREEN_HEIGHT - 1);
            let ty_q = ((fy - y0 as f64).clamp(0.0, 1.0) * 256.0).round() as u32;
            (y0, y1, ty_q)
        } else {
            (native_row, native_row, 0)
        };
        let row_base = (y * out_w) as usize * 4;
        for (ox, col) in cols.iter().enumerate() {
            let mut color = if bilinear {
                let rgb = sample_bilinear(native, col.x0, col.x1, col.tx_q, y0, y1, ty_q);
                [rgb[0], rgb[1], rgb[2], 255]
            } else {
                sample_nearest(native, col.x0, native_row)
            };
            if darken {
                for c in &mut color[..3] {
                    *c = ((*c as u32 * SCANLINE_BRIGHTNESS_PCT) / 100) as u8;
                }
            }
            let idx = row_base + (rect.x as usize + ox) * 4;
            out[idx..idx + 4].copy_from_slice(&color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_dims_pixel_perfect_is_the_native_resolution() {
        assert_eq!(content_dims(Aspect::PixelPerfect), (256, 224));
    }

    #[test]
    fn content_dims_tv_stretches_width_by_8_over_7() {
        // 256 * 8 / 7 = 292.571...; rounds to 293. Height is untouched: the
        // 8:7 PAR correction is horizontal-only (see `Aspect::Tv`).
        assert_eq!(content_dims(Aspect::Tv), (293, 224));
    }

    #[test]
    fn zoomed_dims_multiplies_content_dims_by_the_integer_factor() {
        assert_eq!(zoomed_dims(1, Aspect::PixelPerfect), (256, 224));
        assert_eq!(zoomed_dims(3, Aspect::PixelPerfect), (768, 672));
        assert_eq!(zoomed_dims(4, Aspect::Tv), (293 * 4, 224 * 4));
    }

    #[test]
    fn zoomed_dims_clamps_a_zero_zoom_to_1x() {
        // A corrupt/hand-edited `prefs.zoom = 0` must not produce a
        // zero-size (unusable) window.
        assert_eq!(zoomed_dims(0, Aspect::PixelPerfect), (256, 224));
    }

    #[test]
    fn clamp_to_available_is_a_no_op_when_it_already_fits() {
        assert_eq!(clamp_to_available((800, 600), (1920, 1080)), (800, 600));
        assert_eq!(clamp_to_available((1920, 1080), (1920, 1080)), (1920, 1080));
    }

    #[test]
    fn clamp_to_available_downscales_preserving_aspect_ratio() {
        // ×4 zoom of the TV-corrected picture (1172x896) on a small
        // 1000x800 "screen": width is the binding dimension.
        let (w, h) = clamp_to_available((1172, 896), (1000, 800));
        assert!(w <= 1000 && h <= 800, "must fit within the given max");
        // Aspect ratio preserved to within rounding.
        let orig_ratio = 1172.0 / 896.0;
        let new_ratio = w as f64 / h as f64;
        assert!((orig_ratio - new_ratio).abs() < 0.01, "{orig_ratio} vs {new_ratio}");
    }

    #[test]
    fn clamp_to_available_treats_a_zero_max_as_unbounded() {
        // No usable monitor size (e.g. `primary_monitor()` returned `None`
        // upstream and the caller passed (0,0)): don't collapse the window.
        assert_eq!(clamp_to_available((1024, 896), (0, 0)), (1024, 896));
    }

    #[test]
    fn letterbox_matches_window_exactly_when_aspect_ratios_agree() {
        let rect = letterbox(512, 448, Aspect::PixelPerfect); // exactly 256x224 * 2
        assert_eq!(rect, Rect { x: 0, y: 0, w: 512, h: 448 });
    }

    #[test]
    fn letterbox_pillarboxes_a_window_wider_than_the_content() {
        // Window much wider than 256:224 -> vertical bars are equal on both
        // sides, content height fills the window.
        let rect = letterbox(1600, 448, Aspect::PixelPerfect);
        assert_eq!(rect.h, 448);
        assert!(rect.w < 1600);
        assert_eq!(rect.x, (1600 - rect.w) / 2);
        assert_eq!(rect.y, 0);
    }

    #[test]
    fn letterbox_letterboxes_a_window_taller_than_the_content() {
        let rect = letterbox(512, 1200, Aspect::PixelPerfect);
        assert_eq!(rect.w, 512);
        assert!(rect.h < 1200);
        assert_eq!(rect.y, (1200 - rect.h) / 2);
        assert_eq!(rect.x, 0);
    }

    #[test]
    fn letterbox_handles_a_zero_sized_window_without_panicking() {
        assert_eq!(letterbox(0, 0, Aspect::PixelPerfect), Rect { x: 0, y: 0, w: 0, h: 0 });
        assert_eq!(letterbox(0, 100, Aspect::Tv), Rect { x: 0, y: 0, w: 0, h: 100 });
    }

    #[test]
    fn letterbox_pixel_perfect_snaps_to_the_largest_whole_scale_not_a_fractional_one() {
        // 600x500's own aspect ratio isn't an exact multiple of 256:224: a
        // continuous scale (~2.23x) would fill more of the window than the
        // largest *whole* factor that fits (2x -> 512x448). Pixel-perfect
        // must pick the whole factor so every emulated pixel lands on a
        // whole number of output pixels (required for `Filter::None` to stay
        // genuinely sharp — see the function doc).
        let rect = letterbox(600, 500, Aspect::PixelPerfect);
        assert_eq!(rect, Rect { x: 44, y: 26, w: 512, h: 448 });
    }

    #[test]
    fn letterbox_pixel_perfect_falls_back_to_continuous_scaling_below_1x() {
        // A window smaller than the native 256x224 picture in either
        // dimension: no whole factor >= 1 fits, so this must not overflow
        // the window (nor crop) — same fractional fallback as `Tv`.
        let rect = letterbox(200, 200, Aspect::PixelPerfect);
        assert!(rect.w <= 200 && rect.h <= 200);
        assert!(rect.w > 0 && rect.h > 0);
        // 200/256 is the binding (smaller) ratio: height is letterboxed.
        assert_eq!(rect.w, 200);
        assert!(rect.h < 200);
    }

    #[test]
    fn letterbox_tv_uses_continuous_scaling_never_snapped_to_a_whole_factor() {
        // Same 600x500 window as the pixel-perfect test above, but `Tv`'s
        // content is 293x224: width is the binding dimension and the scale
        // (600/293 ≈ 2.048) is deliberately not a whole number — `Tv`'s
        // purpose is the period-accurate 8:7 shape, not square-pixel
        // sharpness, so it must fill the window right up to continuous scale
        // rather than snap down to the 2x (586-wide) pixel-perfect-style step.
        let rect = letterbox(600, 500, Aspect::Tv);
        assert_eq!(rect, Rect { x: 0, y: 21, w: 600, h: 458 });
        assert_ne!(rect.w, 293 * 2, "must not have snapped to the whole-factor step");
    }

    /// Solid-color native buffer: every pixel is `color`.
    fn solid_native(color: [u8; 4]) -> Vec<u8> {
        let mut buf = vec![0u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
        for px in buf.chunks_exact_mut(4) {
            px.copy_from_slice(&color);
        }
        buf
    }

    #[test]
    fn compose_frame_nearest_zoom_reproduces_exact_source_colors() {
        // A native buffer split top/bottom red/blue, scaled x2 with `None`
        // (nearest): every output pixel must be exactly one of the two
        // source colors, no blending.
        let mut native = solid_native([255, 0, 0, 255]);
        for y in (SCREEN_HEIGHT / 2)..SCREEN_HEIGHT {
            for x in 0..SCREEN_WIDTH {
                let idx = (y * SCREEN_WIDTH + x) * 4;
                native[idx..idx + 4].copy_from_slice(&[0, 0, 255, 255]);
            }
        }
        let (out_w, out_h) = zoomed_dims(2, Aspect::PixelPerfect);
        let mut out = vec![0u8; out_w as usize * out_h as usize * 4];
        compose_frame(&native, &mut out, out_w, out_h, Filter::None, Aspect::PixelPerfect);

        let pixel_at = |x: u32, y: u32| -> [u8; 4] {
            let idx = ((y * out_w + x) as usize) * 4;
            out[idx..idx + 4].try_into().unwrap()
        };
        assert_eq!(pixel_at(0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel_at(out_w - 1, out_h - 1), [0, 0, 255, 255]);
    }

    #[test]
    fn compose_frame_crt_darkens_only_odd_source_scanlines() {
        let native = solid_native([200, 200, 200, 255]);
        let (out_w, out_h) = zoomed_dims(2, Aspect::PixelPerfect); // 512x448
        let mut out = vec![0u8; out_w as usize * out_h as usize * 4];
        compose_frame(&native, &mut out, out_w, out_h, Filter::Crt, Aspect::PixelPerfect);

        // At zoom 2, native row 0 covers output rows 0..2, native row 1
        // covers rows 2..4: row 2 must be darkened, row 0 must not.
        let row_color = |y: u32| -> [u8; 4] {
            let idx = ((y * out_w) as usize) * 4;
            out[idx..idx + 4].try_into().unwrap()
        };
        let bright = row_color(0);
        let dark = row_color(2);
        assert_eq!(bright, [200, 200, 200, 255], "even source scanline must stay untouched");
        assert!(dark[0] < bright[0], "odd source scanline must be darkened");
        assert_eq!(dark[0], (200u32 * SCANLINE_BRIGHTNESS_PCT / 100) as u8);
        assert_eq!(dark[3], 255, "alpha must stay opaque");
    }

    #[test]
    fn compose_frame_none_filter_never_darkens_scanlines() {
        let native = solid_native([200, 200, 200, 255]);
        let (out_w, out_h) = zoomed_dims(2, Aspect::PixelPerfect);
        let mut out = vec![0u8; out_w as usize * out_h as usize * 4];
        compose_frame(&native, &mut out, out_w, out_h, Filter::None, Aspect::PixelPerfect);
        assert!(out.chunks_exact(4).all(|p| p == [200, 200, 200, 255]));
    }

    #[test]
    fn compose_frame_pillarboxes_black_bars_outside_the_content_rect() {
        let native = solid_native([10, 20, 30, 255]);
        // A window much wider than the pixel-perfect content: bars appear at
        // the far left/right columns.
        let (out_w, out_h) = (2000u32, 448u32);
        let mut out = vec![0u8; out_w as usize * out_h as usize * 4];
        compose_frame(&native, &mut out, out_w, out_h, Filter::None, Aspect::PixelPerfect);
        let pixel_at = |x: u32, y: u32| -> [u8; 4] {
            let idx = ((y * out_w + x) as usize) * 4;
            out[idx..idx + 4].try_into().unwrap()
        };
        assert_eq!(pixel_at(0, out_h / 2), LETTERBOX_COLOR);
        assert_eq!(pixel_at(out_w - 1, out_h / 2), LETTERBOX_COLOR);
        // Center column is inside the content rect.
        assert_eq!(pixel_at(out_w / 2, out_h / 2), [10, 20, 30, 255]);
    }

    #[test]
    fn compose_frame_smooth_blends_across_a_hard_edge() {
        // Left half red, right half blue; scaled up with bilinear, a sample
        // straddling the seam must not be exactly either source color.
        let mut native = solid_native([255, 0, 0, 255]);
        for y in 0..SCREEN_HEIGHT {
            for x in (SCREEN_WIDTH / 2)..SCREEN_WIDTH {
                let idx = (y * SCREEN_WIDTH + x) * 4;
                native[idx..idx + 4].copy_from_slice(&[0, 0, 255, 255]);
            }
        }
        let (out_w, out_h) = zoomed_dims(4, Aspect::PixelPerfect);
        let mut out = vec![0u8; out_w as usize * out_h as usize * 4];
        compose_frame(&native, &mut out, out_w, out_h, Filter::Smooth, Aspect::PixelPerfect);
        let idx = ((out_h / 2 * out_w + out_w / 2) as usize) * 4;
        let seam: [u8; 4] = out[idx..idx + 4].try_into().unwrap();
        assert_ne!(seam, [255, 0, 0, 255]);
        assert_ne!(seam, [0, 0, 255, 255]);
        assert!(seam[0] > 0 && seam[2] > 0, "seam pixel must blend both channels: {seam:?}");
    }

    #[test]
    fn filter_and_aspect_round_trip_through_their_pref_strings() {
        for f in [Filter::None, Filter::Smooth, Filter::Crt] {
            assert_eq!(Filter::from_pref(f.as_pref()), f);
        }
        for a in [Aspect::PixelPerfect, Aspect::Tv] {
            assert_eq!(Aspect::from_pref(a.as_pref()), a);
        }
        // Unknown/corrupt strings fall back to the default rendering mode
        // rather than panicking; the stored string itself is left alone by
        // the caller (see `Aspect::from_pref` doc).
        assert_eq!(Filter::from_pref("bogus"), Filter::None);
        assert_eq!(Aspect::from_pref("bogus"), Aspect::PixelPerfect);
    }

    #[test]
    fn filter_next_cycles_through_all_three_and_back() {
        assert_eq!(Filter::None.next(), Filter::Smooth);
        assert_eq!(Filter::Smooth.next(), Filter::Crt);
        assert_eq!(Filter::Crt.next(), Filter::None);
    }

    #[test]
    fn aspect_toggled_is_its_own_inverse() {
        assert_eq!(Aspect::PixelPerfect.toggled(), Aspect::Tv);
        assert_eq!(Aspect::Tv.toggled(), Aspect::PixelPerfect);
        assert_eq!(Aspect::PixelPerfect.toggled().toggled(), Aspect::PixelPerfect);
    }

    /// Cost measurement (rule: "on doit tenir 50-60 images/s" — the module's
    /// CPU compositing must stay a small fraction of a 16-20ms frame budget).
    /// Not a strict perf gate (avoids CI flakiness on a loaded/slow/debug
    /// build): the bound is generous, and the actual measured number is
    /// printed for `cargo test --release -p prisme -- --nocapture` to
    /// surface it. On this development machine (Apple Silicon, `--release`),
    /// the worst case exercised here (zoom x4, 1172x896 for `Aspect::Tv`,
    /// `Filter::Crt` — bilinear plus the scanline darken pass, the most
    /// expensive per-pixel combination) measured ~4.1ms/frame after the `Col`
    /// table + Q8 fixed-point optimizations in `build_columns`/
    /// `sample_bilinear` (down from ~8.7ms with the naive per-pixel `f64`
    /// version this replaced) — comfortably inside a 50fps (20ms) budget at
    /// this window size. Cost is proportional to *window area*, though: the
    /// window is freely resizable (`docs/ROADMAP.md` Phase 2 clarification),
    /// so a window dragged much larger than any zoom preset — e.g. maximized
    /// on a 4K display, ~7.9x this test's pixel count — scales the same
    /// ~7.9x; see `compose_frame_cost_at_a_maximized_4k_window` (`#[ignore]`)
    /// for that measurement.
    #[test]
    fn compose_frame_cost_stays_within_frame_budget() {
        let native = solid_native([128, 64, 200, 255]);
        let (out_w, out_h) = zoomed_dims(4, Aspect::Tv);
        let mut out = vec![0u8; out_w as usize * out_h as usize * 4];
        let start = std::time::Instant::now();
        const ITERATIONS: u32 = 20;
        for _ in 0..ITERATIONS {
            compose_frame(&native, &mut out, out_w, out_h, Filter::Crt, Aspect::Tv);
        }
        let per_frame = start.elapsed() / ITERATIONS;
        eprintln!(
            "compose_frame({out_w}x{out_h}, Crt, Tv): {:.3} ms/frame",
            per_frame.as_secs_f64() * 1000.0
        );
        // Debug builds are ~20-30x slower than `--release` for this kind of
        // per-pixel scalar code (measured: ~4ms release vs. ~120ms debug for
        // this exact case — consistent with the workspace-wide "debug builds
        // are ~20x too slow" note in `.claude/skills/snes-build-test/
        // SKILL.md`), so the bound has to accommodate an unoptimized `cargo
        // test` run too; it still catches a genuine algorithmic regression
        // (e.g. an accidental O(w*h*window_area) blowup).
        let bound = if cfg!(debug_assertions) {
            std::time::Duration::from_millis(500)
        } else {
            std::time::Duration::from_millis(50)
        };
        assert!(
            per_frame < bound,
            "compose_frame took {per_frame:?}, expected well under a frame budget"
        );
    }

    /// Same worst-case filter combination as
    /// `compose_frame_cost_stays_within_frame_budget`, at a window size a
    /// free-resize (not just a zoom preset) can actually reach: a 3840x2160
    /// ("4K") maximized window. `#[ignore]`d by default — this is a
    /// documentation measurement, not a gate (a slow/loaded/debug run would
    /// make it flaky, and its result belongs in the implementation report,
    /// not CI). Run explicitly with `cargo test --release -p prisme --bin
    /// prisme -- --ignored --nocapture render::`.
    #[test]
    #[ignore]
    fn compose_frame_cost_at_a_maximized_4k_window() {
        let native = solid_native([128, 64, 200, 255]);
        let (out_w, out_h) = (3840u32, 2160u32);
        let mut out = vec![0u8; out_w as usize * out_h as usize * 4];
        let start = std::time::Instant::now();
        const ITERATIONS: u32 = 10;
        for _ in 0..ITERATIONS {
            compose_frame(&native, &mut out, out_w, out_h, Filter::Crt, Aspect::Tv);
        }
        let per_frame = start.elapsed() / ITERATIONS;
        eprintln!(
            "compose_frame({out_w}x{out_h}, Crt, Tv): {:.3} ms/frame",
            per_frame.as_secs_f64() * 1000.0
        );
    }

    /// Same window size as `compose_frame_cost_at_a_maximized_4k_window`,
    /// with the *default* filter (`Filter::None`, nearest-neighbor — see
    /// `Prefs::default`/`docs/ROADMAP.md`'s "filtre par défaut" decision)
    /// instead of the worst-case `Crt`: shows the default stays well inside
    /// budget even at this size, unlike `Crt`/`Smooth`'s bilinear pass.
    #[test]
    #[ignore]
    fn compose_frame_cost_at_a_maximized_4k_window_default_filter() {
        let native = solid_native([128, 64, 200, 255]);
        let (out_w, out_h) = (3840u32, 2160u32);
        let mut out = vec![0u8; out_w as usize * out_h as usize * 4];
        let start = std::time::Instant::now();
        const ITERATIONS: u32 = 10;
        for _ in 0..ITERATIONS {
            compose_frame(&native, &mut out, out_w, out_h, Filter::None, Aspect::PixelPerfect);
        }
        let per_frame = start.elapsed() / ITERATIONS;
        eprintln!(
            "compose_frame({out_w}x{out_h}, None, PixelPerfect): {:.3} ms/frame",
            per_frame.as_secs_f64() * 1000.0
        );
    }
}
