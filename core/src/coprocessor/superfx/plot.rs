//! Pixel plotting: PLOT / RPIX / COLOR / CMODE bitplane storage into Game Pak
//! RAM per SCMR screen mode/height and SCBR base, with the POR transparency /
//! dither / nibble logic and an 8-pixel primary pixel cache.

use super::gsu::SuperFx;

impl SuperFx {
    /// Bits-per-pixel from SCMR color depth (MD0-1). 2 = reserved -> 4bpp.
    pub(crate) fn color_depth(&self) -> u32 {
        match self.scmr & 0x03 {
            0 => 2, // 4-color
            1 => 4, // 16-color
            3 => 8, // 256-color
            _ => 4,
        }
    }

    /// Screen-height mode: 0=128px, 1=160px, 2=192px, 3=OBJ (256px).
    /// POR bit4 (OBJ) or bit3 (freeze-high) force OBJ mapping.
    fn height_mode(&self) -> u8 {
        if self.por & 0x10 != 0 || self.por & 0x08 != 0 {
            return 3;
        }
        let ht0 = (self.scmr >> 2) & 1;
        let ht1 = (self.scmr >> 5) & 1;
        (ht1 << 1) | ht0
    }

    fn tile_number(&self, x: u16, y: u16) -> usize {
        let xt = (x as usize) >> 3;
        let yt = (y as usize) >> 3;
        match self.height_mode() {
            0 => xt * 0x10 + yt,
            1 => xt * 0x14 + yt,
            2 => xt * 0x18 + yt,
            _ => {
                // OBJ mode (SNES 2-D OBJ layout).
                ((y as usize) >> 7) * 0x200
                    + ((x as usize) >> 7) * 0x100
                    + (yt & 0x0F) * 0x10
                    + (xt & 0x0F)
            }
        }
    }

    /// RAM offset (from RAM start) of the tile-row byte for pixel (x,y).
    fn tile_row_address(&self, x: u16, y: u16, depth: u32) -> usize {
        let tileno = self.tile_number(x, y);
        let sz = match depth {
            2 => 0x10,
            4 => 0x20,
            _ => 0x40,
        };
        tileno * sz + (self.scbr as usize) * 0x400 + ((y as usize) & 7) * 2
    }

    /// Apply POR high-nibble / freeze-high logic to a color source byte.
    pub(crate) fn apply_color(&self, source: u8) -> u8 {
        let mut c = source;
        if self.por & 0x04 != 0 {
            // High-nibble: replace incoming LSB nibble by incoming MSB nibble.
            c = (c & 0xF0) | (c >> 4);
        }
        if self.por & 0x08 != 0 {
            // Freeze-high: write only the low nibble, keep COLR's high nibble.
            c = (c & 0x0F) | (self.colr & 0xF0);
        }
        c
    }

    /// PLOT: plot (R1,R2) = COLR into the pixel cache (POR transparency/dither
    /// applied), then R1 = R1 + 1.
    pub(crate) fn plot(&mut self) {
        let x = self.r[1];
        let y = self.r[2];
        let depth = self.color_depth();

        let mut color = self.colr;
        // Dither (4/16-color only): swap to high nibble on the checkerboard.
        if self.por & 0x02 != 0 && depth != 8 && ((x ^ y) & 1) != 0 {
            color >>= 4;
        }
        let mask: u8 = ((1u16 << depth) - 1) as u8;
        color &= mask;

        // Color-0 transparency: check low 2/4/8 bits (freeze-high caps at 4).
        let check_bits = if self.por & 0x08 != 0 {
            depth.min(4)
        } else {
            depth
        };
        let cmask: u8 = ((1u16 << check_bits) - 1) as u8;
        let transparent = (color & cmask) == 0;

        if self.por & 0x01 == 0 && transparent {
            // Color 0 not plotted, but R1 still advances.
            self.r[1] = self.r[1].wrapping_add(1);
            return;
        }

        self.plot_pixel(x, y, color);
        self.r[1] = self.r[1].wrapping_add(1);
    }

    fn plot_pixel(&mut self, x: u16, y: u16, color: u8) {
        let tile_x = x & 0xFFF8;
        if self.pcache_flags != 0 && (tile_x != self.pcache_x || y != self.pcache_y) {
            self.flush_pixel_cache();
        }
        self.pcache_x = tile_x;
        self.pcache_y = y;
        let col = (x & 7) as usize;
        self.pcache_bits[col] = color;
        self.pcache_flags |= 1 << col;
        if self.pcache_flags == 0xFF {
            self.flush_pixel_cache();
        }
    }

    /// Flush the pixel cache to Game Pak RAM as bitplanes. Partial flushes
    /// (fewer than 8 plotted pixels) merge with existing RAM via read-modify-write.
    pub(crate) fn flush_pixel_cache(&mut self) {
        if self.pcache_flags == 0 {
            return;
        }
        let x = self.pcache_x;
        let y = self.pcache_y;
        let depth = self.color_depth();
        let base = self.tile_row_address(x, y, depth);

        for p in 0..depth as usize {
            let plane_addr = base + (p >> 1) * 0x10 + (p & 1);
            let mut byte = self.ram_byte_abs(plane_addr);
            for col in 0..8usize {
                if self.pcache_flags & (1 << col) != 0 {
                    let bit = 7 - col;
                    let colorbit = (self.pcache_bits[col] >> p) & 1;
                    if colorbit != 0 {
                        byte |= 1 << bit;
                    } else {
                        byte &= !(1 << bit);
                    }
                }
            }
            self.ram_set_abs(plane_addr, byte);
        }
        self.pcache_flags = 0;
    }

    /// RPIX: flush the pixel cache to RAM, then read the pixel at (R1,R2) from
    /// RAM. Flags 000-s-z. Returns the pixel value.
    pub(crate) fn rpix(&mut self) -> u16 {
        self.flush_pixel_cache();
        let x = self.r[1];
        let y = self.r[2];
        let depth = self.color_depth();
        let base = self.tile_row_address(x, y, depth);
        let bit = 7 - (x & 7) as usize;
        let mut value = 0u16;
        for p in 0..depth as usize {
            let plane_addr = base + (p >> 1) * 0x10 + (p & 1);
            let b = self.ram_byte_abs(plane_addr);
            value |= (((b >> bit) & 1) as u16) << p;
        }
        self.z = value == 0;
        self.s = false;
        value
    }
}

#[cfg(test)]
mod tests {
    use super::super::gsu::{SuperFx, VCR_GSU2};

    #[test]
    fn plot_4color_bitplanes() {
        let mut fx = SuperFx::new(0x8000, VCR_GSU2);
        // 4-color mode (MD=0), height 128, GSU owns ROM+RAM.
        fx.scmr = 0x18;
        fx.colr = 3; // both bitplanes set
        fx.r[1] = 0; // x
        fx.r[2] = 0; // y
        fx.plot();
        fx.flush_pixel_cache();
        // Tile 0, row 0, column 0 (bit7). Planes 0 and 1 at base+0/+1.
        assert_eq!(fx.ram()[0], 0x80);
        assert_eq!(fx.ram()[1], 0x80);
    }

    #[test]
    fn plot_then_rpix_roundtrip() {
        let mut fx = SuperFx::new(0x8000, VCR_GSU2);
        fx.scmr = 0x18; // 4-color
        fx.colr = 2;
        fx.r[1] = 5;
        fx.r[2] = 3;
        fx.plot();
        // RPIX reads back (R1,R2). R1 unchanged after plot? plot inc'd R1 to 6.
        fx.r[1] = 5;
        let v = fx.rpix();
        assert_eq!(v, 2);
    }

    #[test]
    fn color_transparency_skips_but_advances() {
        let mut fx = SuperFx::new(0x8000, VCR_GSU2);
        fx.scmr = 0x18;
        fx.colr = 0; // transparent color 0
        fx.por = 0; // bit0=0 => do not plot color 0
        fx.r[1] = 4;
        fx.r[2] = 4;
        fx.plot();
        assert_eq!(fx.r[1], 5); // advanced
        assert_eq!(fx.ram()[0], 0); // nothing plotted
    }
}
