//! S-PPU data layer: VRAM/CGRAM/OAM memories and their CPU ports ($2100-$213F)
//! with hardware-exact latch/increment/prefetch behavior, plus the typed
//! register decode and shared pixel types the renderer modules build on.
//!
//! Rendering (BG tile fetch, sprite evaluation, compositing, color math,
//! windows, Mode 7) lives in the sibling modules; this file only owns state,
//! ports, and the compositor contract documented below.
//!
//! # Compositor contract (the renderer agents must conform to these exactly)
//!
//! All coordinates are physical SNES pixels; `line` is the visible scanline
//! 0..=223 (V=1..=224 in scheduler terms). One scanline is 256 pixels wide;
//! renderers write fixed `[_; 256]` arrays indexed by screen X (left = 0).
//!
//! Color indices are 8-bit CGRAM indices with the palette base already folded
//! in (BG palette base, OBJ base 128, Mode-0 per-BG offset, etc.). Index whose
//! low palette bits are 0 = transparent for that layer.
//!
//! ## `sprites.rs`
//! ```ignore
//! pub fn render_obj_line(ppu: &mut Ppu, line: u16, out: &mut [ObjPixel; 256]);
//! ```
//! Evaluates OAM for `line`, writes one `ObjPixel` per screen column, and sets
//! `ppu.obj_range_over` / `ppu.obj_time_over` for that line. `out` must be
//! fully written (transparent entries = `ObjPixel::default()`).
//!
//! ## `background.rs`
//! ```ignore
//! pub fn render_bg_line(ppu: &Ppu, bg_index: usize, line: u16, out: &mut [LayerPixel; 256]);
//! ```
//! `bg_index` 0..=3 = BG1..BG4. Applies that layer's scroll, mosaic, tile-size
//! and tilemap/char fetch for the current `ppu.bg_mode`, writing one
//! `LayerPixel` per column (transparent = `LayerPixel::default()`). Mode 7 and
//! offset-per-tile helpers may be additional `pub fn`s in `background.rs` /
//! `mode7.rs`; `render.rs` is free to call them.
//!
//! ## `render.rs`
//! ```ignore
//! pub fn render_scanline(ppu: &mut Ppu, line: u16);
//! ```
//! Owns compositing: fetches BG lines + the OBJ line, applies the per-mode
//! front-to-back priority order (ppu.md §9), main/sub screen enables, windows,
//! color math and brightness, and writes the final BGR555 pixels into
//! `ppu.framebuffer` for row `line`. Invoked via [`Ppu::render_scanline`].

pub mod background;
pub mod color_math;
pub mod mode7;
pub mod render;
pub mod sprites;
pub mod window;

use crate::FrameBuffer;

/// One pixel emitted by the sprite renderer for a scanline column.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct ObjPixel {
    /// Full 8-bit CGRAM index (OBJ base 128 + palette*16 + pixel). Meaningful
    /// only when `opaque`.
    pub color: u8,
    /// OBJ priority 0-3 (OAM byte 3 bits5-4); places the sprite among the BGs.
    pub priority: u8,
    /// OBJ palette 0-7 (OAM byte 3 bits3-1). Color math only applies to OBJ
    /// pixels from palettes 4-7 (CGADSUB bit4).
    pub palette: u8,
    /// True if this column holds an opaque OBJ pixel (color low nibble != 0).
    pub opaque: bool,
}

/// One pixel emitted by a background tile fetch for a scanline column.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct LayerPixel {
    /// Full 8-bit CGRAM index with palette base applied. Meaningful only when
    /// `opaque`.
    pub color: u8,
    /// Tilemap priority bit (tilemap entry bit13): 0 = low, 1 = high.
    pub priority: u8,
    /// True if opaque (non-zero pixel within its palette).
    pub opaque: bool,
}

pub struct Ppu {
    // --- Memories ---
    /// 32K words (64 KB), two byte planes fused into one u16 per word address.
    pub vram: Box<[u16; 0x8000]>,
    /// 256 BGR555 colors (`0BBBBBGG GGGRRRRR`).
    pub cgram: [u16; 256],
    /// OAM table 1 (512 B): 128 sprites × 4 bytes.
    pub oam_lo: [u8; 512],
    /// OAM table 2 (32 B): 2 bits/sprite (X bit8 + size).
    pub oam_hi: [u8; 32],

    /// Final composited frame for the current field.
    pub framebuffer: FrameBuffer,

    /// Raw last-written $2100-$213F (window/color-math regs and anything the
    /// typed decode does not break out are read from here by the renderers).
    pub regs: [u8; 0x40],
    /// PPU1/PPU2 open-bus data latches (drive undriven read bits).
    pub ppu1_mdr: u8,
    pub ppu2_mdr: u8,

    // --- VRAM port ---
    /// $2115 VMAIN.
    pub vmain: u8,
    /// $2116/$2117 word address (15-bit meaningful).
    pub vram_addr: u16,
    /// Read prefetch buffer (dummy-read latch).
    pub vram_prefetch: u16,

    // --- CGRAM port ---
    /// $2121 color index.
    pub cgram_addr: u8,
    /// Low-byte latch for $2122 word writes.
    pub cgram_latch: u8,
    /// Byte toggle shared by $2122 write and $213B read (false = low byte next).
    pub cgram_hi: bool,

    // --- OAM port ---
    /// OAMADD reload value from $2102 (+ $2103 bit0), 9-bit word address.
    pub oam_addr_reg: u16,
    /// $2103 bit7: sprite priority rotation enable.
    pub oam_priority: bool,
    /// Internal byte address (0..0x3FF), auto-incrementing.
    pub oam_addr: u16,
    /// Low-byte latch for $2104 word writes into table 1.
    pub oam_latch: u8,

    // --- Counter latch / status ($2137/$213C-$213F) ---
    /// Current H/V dot counters; the bus/scheduler updates these so $2137 can
    /// latch them. Latched into OPHCT/OPVCT on SLHV.
    pub h_counter: u16,
    pub v_counter: u16,
    pub ophct: u16,
    pub opvct: u16,
    /// OPHCT/OPVCT read flip-flops (false = low byte next). Reset by $213F read.
    pub ophct_hi: bool,
    pub opvct_hi: bool,
    /// Counter-latch flag ($213F bit6). Reset by $213F read.
    pub counter_latched: bool,
    /// Interlace field toggle ($213F bit7).
    pub interlace_field: bool,
    /// $213F bit4: 0 = NTSC (60 Hz), 1 = PAL (50 Hz). Region-dependent.
    pub is_pal: bool,
    /// $213E bit6 OBJ range over (>32 sprites on a line).
    pub obj_range_over: bool,
    /// $213E bit7 OBJ time over (>34 tile slivers on a line).
    pub obj_time_over: bool,

    // --- Typed register decode (read by the renderers) ---
    /// $2100 INIDISP bit7.
    pub forced_blank: bool,
    /// $2100 INIDISP bits3-0 master brightness 0-15.
    pub brightness: u8,
    /// $2101 OBSEL bits7-5 object size mode.
    pub obj_size: u8,
    /// $2101 OBSEL name base word address (bits2-0 << 13).
    pub obj_name_base: u16,
    /// $2101 OBSEL name-select gap for tiles $100-$1FF ((NN+1) << 12).
    pub obj_name_gap: u16,
    /// $2105 BGMODE bits2-0.
    pub bg_mode: u8,
    /// $2105 bit3: Mode 1 BG3 priority lift.
    pub bg3_priority: bool,
    /// $2105 bits4-7: per-BG char size (false = 8×8, true = 16×16), BG1..BG4.
    pub bg_tile_size: [bool; 4],
    /// $2106 bits7-4 mosaic size (0 = 1×1 .. 15 = 16×16).
    pub mosaic_size: u8,
    /// $2106 bits3-0 mosaic enable (bit0 = BG1 .. bit3 = BG4).
    pub mosaic_enable: u8,
    /// $2107-$210A BGnSC tilemap base word address.
    pub bg_map_base: [u16; 4],
    /// $2107-$210A BGnSC size (bits1-0 = YX quadrant layout).
    pub bg_map_size: [u8; 4],
    /// $210B/$210C char base word address per BG.
    pub bg_char_base: [u16; 4],
    /// $210D-$2114 H/V scroll (10-bit).
    pub bg_hofs: [u16; 4],
    pub bg_vofs: [u16; 4],
    /// Shared scroll write latch (all BGnH/VOFS).
    pub bgofs_latch: u8,
    /// Extra latch used only by BGnHOFS writes (low-3-bits path).
    pub bghofs_latch: u8,
    // --- Mode 7 ---
    /// $210D/$210E M7HOFS/M7VOFS (13-bit signed).
    pub m7_hofs: u16,
    pub m7_vofs: u16,
    /// Shared single Mode-7 write latch ($210D/$210E + $211B-$2120).
    pub mode7_latch: u8,
    /// $211B-$211E matrix (16-bit signed 8.8).
    pub m7a: i16,
    pub m7b: i16,
    pub m7c: i16,
    pub m7d: i16,
    /// $211F/$2120 center (13-bit signed).
    pub m7x: i16,
    pub m7y: i16,
    /// $211A M7SEL.
    pub m7sel: u8,
    /// Most recent byte written to $211C (8-bit signed) — MPY multiplier.
    pub m7_mul_operand: i8,
    // --- Screen enables / windows / SETINI ---
    /// $212C TM main-screen layer enable (bit4 OBJ, bits3-0 BG4..BG1).
    pub main_screen: u8,
    /// $212D TS sub-screen layer enable.
    pub sub_screen: u8,
    /// $212E TMW main-screen window masking enable.
    pub main_window: u8,
    /// $212F TSW sub-screen window masking enable.
    pub sub_window: u8,
    /// $2133 bit2 overscan (239 lines).
    pub overscan: bool,
    /// $2133 bit0 screen interlace.
    pub interlace: bool,
    /// $2133 bit1 OBJ interlace.
    pub obj_interlace: bool,
    /// $2133 bit3 pseudo-hires.
    pub pseudo_hires: bool,
    /// $2133 bit6 EXTBG (Mode 7 BG2).
    pub extbg: bool,

    // --- Windows ($2123-$212B) ---
    /// $2123 W12SEL: BG1/BG2 per-window enable+invert (2 bits each, low=invert).
    pub w12sel: u8,
    /// $2124 W34SEL: BG3/BG4.
    pub w34sel: u8,
    /// $2125 WOBJSEL: OBJ / Color window.
    pub wobjsel: u8,
    /// $2126 WH0 W1 left, $2127 WH1 W1 right (inclusive; left>right ⇒ empty).
    pub w1_left: u8,
    pub w1_right: u8,
    /// $2128 WH2 W2 left, $2129 WH3 W2 right.
    pub w2_left: u8,
    pub w2_right: u8,
    /// $212A WBGLOG: per-BG window combine (0=OR,1=AND,2=XOR,3=XNOR).
    pub wbglog: u8,
    /// $212B WOBJLOG: OBJ (bits1-0) and Color (bits3-2) window combine.
    pub wobjlog: u8,

    // --- Color math ($2130-$2132) ---
    /// $2130 CGWSEL: force-black region (7-6), prevent-math region (5-4),
    /// addend select (bit1: 0=fixed color, 1=subscreen), direct color (bit0).
    pub cgwsel: u8,
    /// $2131 CGADSUB: sub(bit7)/half(bit6)/backdrop(bit5)/OBJ(bit4)/BG4-1(3-0).
    pub cgadsub: u8,
    /// $2132 COLDATA fixed color, per-channel 5-bit (accumulated across writes).
    pub coldata_r: u8,
    pub coldata_g: u8,
    pub coldata_b: u8,
}

impl Ppu {
    pub fn new() -> Self {
        Ppu {
            vram: vec![0u16; 0x8000].into_boxed_slice().try_into().unwrap(),
            cgram: [0; 256],
            oam_lo: [0; 512],
            oam_hi: [0; 32],
            framebuffer: FrameBuffer::new(),
            regs: [0; 0x40],
            ppu1_mdr: 0,
            ppu2_mdr: 0,
            vmain: 0,
            vram_addr: 0,
            vram_prefetch: 0,
            cgram_addr: 0,
            cgram_latch: 0,
            cgram_hi: false,
            oam_addr_reg: 0,
            oam_priority: false,
            oam_addr: 0,
            oam_latch: 0,
            h_counter: 0,
            v_counter: 0,
            ophct: 0,
            opvct: 0,
            ophct_hi: false,
            opvct_hi: false,
            counter_latched: false,
            interlace_field: false,
            is_pal: false,
            obj_range_over: false,
            obj_time_over: false,
            forced_blank: false,
            brightness: 0,
            obj_size: 0,
            obj_name_base: 0,
            obj_name_gap: 0,
            bg_mode: 0,
            bg3_priority: false,
            bg_tile_size: [false; 4],
            mosaic_size: 0,
            mosaic_enable: 0,
            bg_map_base: [0; 4],
            bg_map_size: [0; 4],
            bg_char_base: [0; 4],
            bg_hofs: [0; 4],
            bg_vofs: [0; 4],
            bgofs_latch: 0,
            bghofs_latch: 0,
            m7_hofs: 0,
            m7_vofs: 0,
            mode7_latch: 0,
            m7a: 0,
            m7b: 0,
            m7c: 0,
            m7d: 0,
            m7x: 0,
            m7y: 0,
            m7sel: 0,
            m7_mul_operand: 0,
            main_screen: 0,
            sub_screen: 0,
            main_window: 0,
            sub_window: 0,
            overscan: false,
            interlace: false,
            obj_interlace: false,
            pseudo_hires: false,
            extbg: false,
            w12sel: 0,
            w34sel: 0,
            wobjsel: 0,
            w1_left: 0,
            w1_right: 0,
            w2_left: 0,
            w2_right: 0,
            wbglog: 0,
            wobjlog: 0,
            cgwsel: 0,
            cgadsub: 0,
            coldata_r: 0,
            coldata_g: 0,
            coldata_b: 0,
        }
    }

    /// Render one visible scanline into `self.framebuffer`. Delegates to the
    /// compositor in `render.rs`.
    pub fn render_scanline(&mut self, line: u16) {
        render::render_scanline(self, line);
    }

    /// Called at the top of a new field. Toggles the interlace field flag; the
    /// scheduler drives per-line rendering.
    pub fn start_frame(&mut self) {
        self.interlace_field = !self.interlace_field;
        // $213E OBJ range-over (bit6) / time-over (bit7) latch when any scanline
        // overflows and are cleared only at end of V-blank / pre-render.
        self.obj_range_over = false;
        self.obj_time_over = false;
    }

    /// Feed the current H/V dot counters (bus/scheduler side effect) so a
    /// subsequent $2137/SLHV read latches the correct values.
    pub fn set_hv_counters(&mut self, h: u16, v: u16) {
        self.h_counter = h;
        self.v_counter = v;
    }

    /// Reload the internal OAM byte address from OAMADD — hardware does this at
    /// the start of V-blank (unless in forced blank).
    pub fn reload_oam_addr(&mut self) {
        self.oam_addr = (self.oam_addr_reg & 0x1FF) << 1;
    }

    // --- VRAM helpers ---

    fn vram_increment(&self) -> u16 {
        match self.vmain & 0x03 {
            0 => 1,
            1 => 32,
            _ => 128, // 2 and 3 both step 128 words
        }
    }

    /// VMAIN bits3-2 address remap applied before every VRAM access (rotate the
    /// low 8/9/10 bits left by 3 — matches ppu.md §6 2bpp/4bpp/8bpp tables).
    fn vram_remap(&self, addr: u16) -> u16 {
        let a = addr & 0x7FFF;
        match (self.vmain >> 2) & 0x03 {
            0 => a,
            1 => (a & 0xFF00) | ((a & 0x001F) << 3) | ((a & 0x00E0) >> 5),
            2 => (a & 0xFE00) | ((a & 0x003F) << 3) | ((a & 0x01C0) >> 6),
            _ => (a & 0xFC00) | ((a & 0x007F) << 3) | ((a & 0x0380) >> 7),
        }
    }

    fn vram_reload_prefetch(&mut self) {
        self.vram_prefetch = self.vram[self.vram_remap(self.vram_addr) as usize];
    }

    fn vram_read_low(&mut self) -> u8 {
        let v = (self.vram_prefetch & 0xFF) as u8;
        // VMAIN bit7 = 0 → $2139 read triggers reload + increment.
        if self.vmain & 0x80 == 0 {
            self.vram_reload_prefetch();
            self.vram_addr = self.vram_addr.wrapping_add(self.vram_increment());
        }
        v
    }

    fn vram_read_high(&mut self) -> u8 {
        let v = (self.vram_prefetch >> 8) as u8;
        // VMAIN bit7 = 1 → $213A read triggers reload + increment.
        if self.vmain & 0x80 != 0 {
            self.vram_reload_prefetch();
            self.vram_addr = self.vram_addr.wrapping_add(self.vram_increment());
        }
        v
    }

    fn vram_write_low(&mut self, value: u8) {
        let idx = self.vram_remap(self.vram_addr) as usize;
        self.vram[idx] = (self.vram[idx] & 0xFF00) | value as u16;
        if self.vmain & 0x80 == 0 {
            self.vram_addr = self.vram_addr.wrapping_add(self.vram_increment());
        }
    }

    fn vram_write_high(&mut self, value: u8) {
        let idx = self.vram_remap(self.vram_addr) as usize;
        self.vram[idx] = (self.vram[idx] & 0x00FF) | ((value as u16) << 8);
        if self.vmain & 0x80 != 0 {
            self.vram_addr = self.vram_addr.wrapping_add(self.vram_increment());
        }
    }

    // --- CGRAM helpers ---

    fn cgram_write(&mut self, value: u8) {
        if !self.cgram_hi {
            self.cgram_latch = value;
            self.cgram_hi = true;
        } else {
            self.cgram[self.cgram_addr as usize] =
                (self.cgram_latch as u16 | ((value as u16) << 8)) & 0x7FFF;
            self.cgram_addr = self.cgram_addr.wrapping_add(1);
            self.cgram_hi = false;
        }
    }

    fn cgram_read(&mut self) -> u8 {
        let word = self.cgram[self.cgram_addr as usize];
        if !self.cgram_hi {
            self.cgram_hi = true;
            (word & 0xFF) as u8
        } else {
            // High read: bits6-0 = color bits14-8, bit7 = PPU2 open bus.
            let v = ((word >> 8) & 0x7F) as u8 | (self.ppu2_mdr & 0x80);
            self.cgram_addr = self.cgram_addr.wrapping_add(1);
            self.cgram_hi = false;
            v
        }
    }

    // --- OAM helpers ---

    fn oam_write(&mut self, value: u8) {
        let addr = self.oam_addr & 0x3FF;
        if addr < 0x200 {
            if addr & 1 == 0 {
                self.oam_latch = value;
            } else {
                self.oam_lo[(addr - 1) as usize] = self.oam_latch;
                self.oam_lo[addr as usize] = value;
            }
        } else {
            self.oam_hi[(addr & 0x1F) as usize] = value;
        }
        self.oam_addr = (self.oam_addr + 1) & 0x3FF;
    }

    fn oam_read(&mut self) -> u8 {
        let addr = self.oam_addr & 0x3FF;
        let v = if addr < 0x200 {
            self.oam_lo[addr as usize]
        } else {
            self.oam_hi[(addr & 0x1F) as usize]
        };
        self.oam_addr = (self.oam_addr + 1) & 0x3FF;
        v
    }

    // --- Counter latch / status helpers ---

    fn latch_counters(&mut self) {
        // SLHV: latch current H/V into OPHCT/OPVCT. WRIO bit7 gating is applied
        // by the bus (not modeled here).
        self.ophct = self.h_counter & 0x1FF;
        self.opvct = self.v_counter & 0x1FF;
        self.counter_latched = true;
    }

    fn ophct_read(&mut self) -> u8 {
        if !self.ophct_hi {
            self.ophct_hi = true;
            (self.ophct & 0xFF) as u8
        } else {
            self.ophct_hi = false;
            ((self.ophct >> 8) & 0x01) as u8 | (self.ppu2_mdr & 0xFE)
        }
    }

    fn opvct_read(&mut self) -> u8 {
        if !self.opvct_hi {
            self.opvct_hi = true;
            (self.opvct & 0xFF) as u8
        } else {
            self.opvct_hi = false;
            ((self.opvct >> 8) & 0x01) as u8 | (self.ppu2_mdr & 0xFE)
        }
    }

    fn stat77(&self) -> u8 {
        (self.obj_time_over as u8) << 7
            | (self.obj_range_over as u8) << 6
            | (self.ppu1_mdr & 0x10)
            | 0x01 // PPU1 version 1
    }

    fn stat78(&mut self) -> u8 {
        let v = (self.interlace_field as u8) << 7
            | (self.counter_latched as u8) << 6
            | (self.ppu2_mdr & 0x20)
            | (self.is_pal as u8) << 4
            | 0x01; // PPU2 version 1
        // Reading $213F resets the OPHCT/OPVCT flip-flops and the latch flag.
        self.ophct_hi = false;
        self.opvct_hi = false;
        self.counter_latched = false;
        v
    }

    fn mpy(&self) -> u32 {
        (self.m7a as i32).wrapping_mul(self.m7_mul_operand as i32) as u32
    }

    /// CPU read of $21xx (reg = addr & 0x3F). `None` = open bus (bus supplies
    /// the CPU MDR).
    ///
    /// Every read that returns data refreshes the driving chip's open-bus latch
    /// (PPU1-MDR for $2134-$213A/$213E, PPU2-MDR for $213B/$213C/$213D/$213F);
    /// undriven bits on a later partial read then return that last bus value
    /// rather than 0 (ppu.md §7, §14). $2137 drives no data (CPU open bus).
    pub fn read(&mut self, reg: u8) -> Option<u8> {
        match reg & 0x3F {
            0x34 => {
                let v = (self.mpy() & 0xFF) as u8;
                self.ppu1_mdr = v;
                Some(v)
            }
            0x35 => {
                let v = (self.mpy() >> 8) as u8;
                self.ppu1_mdr = v;
                Some(v)
            }
            0x36 => {
                let v = (self.mpy() >> 16) as u8;
                self.ppu1_mdr = v;
                Some(v)
            }
            // $2137 SLHV: latch counters; returns CPU open bus.
            0x37 => {
                self.latch_counters();
                None
            }
            0x38 => {
                let v = self.oam_read();
                self.ppu1_mdr = v;
                Some(v)
            }
            0x39 => {
                let v = self.vram_read_low();
                self.ppu1_mdr = v;
                Some(v)
            }
            0x3A => {
                let v = self.vram_read_high();
                self.ppu1_mdr = v;
                Some(v)
            }
            0x3B => {
                let v = self.cgram_read();
                self.ppu2_mdr = v;
                Some(v)
            }
            0x3C => {
                let v = self.ophct_read();
                self.ppu2_mdr = v;
                Some(v)
            }
            0x3D => {
                let v = self.opvct_read();
                self.ppu2_mdr = v;
                Some(v)
            }
            0x3E => {
                let v = self.stat77();
                self.ppu1_mdr = v;
                Some(v)
            }
            0x3F => {
                let v = self.stat78();
                self.ppu2_mdr = v;
                Some(v)
            }
            _ => None,
        }
    }

    /// CPU write to $21xx (reg = addr & 0x3F).
    pub fn write(&mut self, reg: u8, value: u8) {
        let reg = reg & 0x3F;
        self.regs[reg as usize] = value;
        match reg {
            // $2100 INIDISP
            0x00 => {
                self.forced_blank = value & 0x80 != 0;
                self.brightness = value & 0x0F;
            }
            // $2101 OBSEL
            0x01 => {
                self.obj_size = (value >> 5) & 0x07;
                self.obj_name_base = ((value as u16) & 0x07) << 13;
                let nn = (value as u16 >> 3) & 0x03;
                self.obj_name_gap = (nn + 1) << 12;
            }
            // $2102 OAMADDL
            0x02 => {
                self.oam_addr_reg = (self.oam_addr_reg & 0x100) | value as u16;
                self.reload_oam_addr();
            }
            // $2103 OAMADDH: bit0 = table select, bit7 = priority rotation.
            0x03 => {
                self.oam_addr_reg =
                    (self.oam_addr_reg & 0x0FF) | (((value as u16) & 0x01) << 8);
                self.oam_priority = value & 0x80 != 0;
                self.reload_oam_addr();
            }
            // $2104 OAMDATA
            0x04 => self.oam_write(value),
            // $2105 BGMODE
            0x05 => {
                self.bg_mode = value & 0x07;
                self.bg3_priority = value & 0x08 != 0;
                self.bg_tile_size[0] = value & 0x10 != 0;
                self.bg_tile_size[1] = value & 0x20 != 0;
                self.bg_tile_size[2] = value & 0x40 != 0;
                self.bg_tile_size[3] = value & 0x80 != 0;
            }
            // $2106 MOSAIC
            0x06 => {
                self.mosaic_size = (value >> 4) & 0x0F;
                self.mosaic_enable = value & 0x0F;
            }
            // $2107-$210A BGnSC
            0x07..=0x0A => {
                let n = (reg - 0x07) as usize;
                self.bg_map_base[n] = ((value as u16) & 0xFC) << 8;
                self.bg_map_size[n] = value & 0x03;
            }
            // $210B BG12NBA
            0x0B => {
                self.bg_char_base[0] = ((value as u16) & 0x0F) << 12;
                self.bg_char_base[1] = ((value as u16) >> 4) << 12;
            }
            // $210C BG34NBA
            0x0C => {
                self.bg_char_base[2] = ((value as u16) & 0x0F) << 12;
                self.bg_char_base[3] = ((value as u16) >> 4) << 12;
            }
            // $210D BG1HOFS + M7HOFS
            0x0D => {
                self.write_bg_hofs(0, value);
                self.write_m7_ofs(true, value);
            }
            // $210E BG1VOFS + M7VOFS
            0x0E => {
                self.write_bg_vofs(0, value);
                self.write_m7_ofs(false, value);
            }
            // $210F/$2111/$2113 BG2/3/4 HOFS
            0x0F => self.write_bg_hofs(1, value),
            0x11 => self.write_bg_hofs(2, value),
            0x13 => self.write_bg_hofs(3, value),
            // $2110/$2112/$2114 BG2/3/4 VOFS
            0x10 => self.write_bg_vofs(1, value),
            0x12 => self.write_bg_vofs(2, value),
            0x14 => self.write_bg_vofs(3, value),
            // $2115 VMAIN
            0x15 => self.vmain = value,
            // $2116 VMADDL
            0x16 => {
                self.vram_addr = (self.vram_addr & 0xFF00) | value as u16;
                self.vram_reload_prefetch();
            }
            // $2117 VMADDH
            0x17 => {
                self.vram_addr = (self.vram_addr & 0x00FF) | ((value as u16) << 8);
                self.vram_reload_prefetch();
            }
            // $2118 VMDATAL / $2119 VMDATAH
            0x18 => self.vram_write_low(value),
            0x19 => self.vram_write_high(value),
            // $211A M7SEL
            0x1A => self.m7sel = value,
            // $211B-$2120 Mode 7 matrix/center (shared mode7_latch, low first)
            0x1B => {
                self.m7a = ((value as u16) << 8 | self.mode7_latch as u16) as i16;
                self.mode7_latch = value;
            }
            0x1C => {
                self.m7b = ((value as u16) << 8 | self.mode7_latch as u16) as i16;
                self.mode7_latch = value;
                self.m7_mul_operand = value as i8;
            }
            0x1D => {
                self.m7c = ((value as u16) << 8 | self.mode7_latch as u16) as i16;
                self.mode7_latch = value;
            }
            0x1E => {
                self.m7d = ((value as u16) << 8 | self.mode7_latch as u16) as i16;
                self.mode7_latch = value;
            }
            0x1F => {
                self.m7x = sign_extend_13((value as u16) << 8 | self.mode7_latch as u16);
                self.mode7_latch = value;
            }
            0x20 => {
                self.m7y = sign_extend_13((value as u16) << 8 | self.mode7_latch as u16);
                self.mode7_latch = value;
            }
            // $2121 CGADD
            0x21 => {
                self.cgram_addr = value;
                self.cgram_hi = false;
            }
            // $2122 CGDATA
            0x22 => self.cgram_write(value),
            // $2123-$2125 window per-layer enable/invert
            0x23 => self.w12sel = value,
            0x24 => self.w34sel = value,
            0x25 => self.wobjsel = value,
            // $2126-$2129 window positions (WH0-WH3)
            0x26 => self.w1_left = value,
            0x27 => self.w1_right = value,
            0x28 => self.w2_left = value,
            0x29 => self.w2_right = value,
            // $212A WBGLOG / $212B WOBJLOG
            0x2A => self.wbglog = value,
            0x2B => self.wobjlog = value,
            // $212C TM / $212D TS
            0x2C => self.main_screen = value,
            0x2D => self.sub_screen = value,
            // $212E TMW / $212F TSW
            0x2E => self.main_window = value,
            0x2F => self.sub_window = value,
            // $2130 CGWSEL / $2131 CGADSUB
            0x30 => self.cgwsel = value,
            0x31 => self.cgadsub = value,
            // $2132 COLDATA: bits7-5 select B/G/R planes; only selected channels
            // are updated (unselected retain their prior value).
            0x32 => {
                let v = value & 0x1F;
                if value & 0x20 != 0 {
                    self.coldata_r = v;
                }
                if value & 0x40 != 0 {
                    self.coldata_g = v;
                }
                if value & 0x80 != 0 {
                    self.coldata_b = v;
                }
            }
            // $2133 SETINI
            0x33 => {
                self.overscan = value & 0x04 != 0;
                self.interlace = value & 0x01 != 0;
                self.obj_interlace = value & 0x02 != 0;
                self.pseudo_hires = value & 0x08 != 0;
                self.extbg = value & 0x40 != 0;
            }
            _ => {}
        }
    }

    fn write_bg_hofs(&mut self, n: usize, value: u8) {
        self.bg_hofs[n] = (((value as u16) << 8)
            | ((self.bgofs_latch as u16) & !0x07)
            | ((self.bghofs_latch as u16) & 0x07))
            & 0x3FF;
        self.bgofs_latch = value;
        self.bghofs_latch = value;
    }

    fn write_bg_vofs(&mut self, n: usize, value: u8) {
        self.bg_vofs[n] = (((value as u16) << 8) | self.bgofs_latch as u16) & 0x3FF;
        self.bgofs_latch = value;
    }

    fn write_m7_ofs(&mut self, horizontal: bool, value: u8) {
        let v = ((value as u16) << 8) | self.mode7_latch as u16;
        if horizontal {
            self.m7_hofs = v & 0x1FFF;
        } else {
            self.m7_vofs = v & 0x1FFF;
        }
        self.mode7_latch = value;
    }
}

/// Sign-extend a 13-bit value to i16 (Mode 7 M7X/M7Y).
fn sign_extend_13(v: u16) -> i16 {
    let v = v & 0x1FFF;
    if v & 0x1000 != 0 {
        (v | 0xE000) as i16
    } else {
        v as i16
    }
}

impl Default for Ppu {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_addr(ppu: &mut Ppu, addr: u16) {
        ppu.write(0x16, (addr & 0xFF) as u8);
        ppu.write(0x17, (addr >> 8) as u8);
    }

    #[test]
    fn vram_word_write_increment_on_high() {
        let mut ppu = Ppu::new();
        ppu.write(0x15, 0x80); // VMAIN: step 1, increment on $2119
        set_addr(&mut ppu, 0);
        ppu.write(0x18, 0x34);
        ppu.write(0x19, 0x12);
        ppu.write(0x18, 0x78);
        ppu.write(0x19, 0x56);
        assert_eq!(ppu.vram[0], 0x1234);
        assert_eq!(ppu.vram[1], 0x5678);
        assert_eq!(ppu.vram_addr, 2);
    }

    #[test]
    fn vram_increment_on_low_only() {
        let mut ppu = Ppu::new();
        ppu.write(0x15, 0x00); // VMAIN: step 1, increment on $2118 (low)
        set_addr(&mut ppu, 0);
        ppu.write(0x18, 0xAA); // low write increments
        assert_eq!(ppu.vram_addr, 1);
        ppu.write(0x19, 0xBB); // high write does not
        assert_eq!(ppu.vram_addr, 1);
        assert_eq!(ppu.vram[0], 0x00AA);
        assert_eq!(ppu.vram[1], 0xBB00);
    }

    #[test]
    fn vram_increment_steps() {
        for (bits, step) in [(0u8, 1u16), (1, 32), (2, 128), (3, 128)] {
            let mut ppu = Ppu::new();
            ppu.write(0x15, 0x80 | bits);
            set_addr(&mut ppu, 0);
            ppu.write(0x18, 0);
            ppu.write(0x19, 0);
            assert_eq!(ppu.vram_addr, step, "bits {bits}");
        }
    }

    #[test]
    fn vram_remap_2bpp() {
        let mut ppu = Ppu::new();
        // VMAIN bits3-2 = 01 (2bpp remap). addr low byte YYYccccc.
        ppu.vmain = 0x04;
        // 0x0001 = ccccc=00001 YYY=000 -> cccccYYY = 00001000 = 0x08
        assert_eq!(ppu.vram_remap(0x0001), 0x0008);
        // 0x00E0 = ccccc=00000 YYY=111 -> 00000111 = 0x0007
        assert_eq!(ppu.vram_remap(0x00E0), 0x0007);
        // high bits (rrrrrrrr) pass through
        assert_eq!(ppu.vram_remap(0x1F01), 0x1F08);
    }

    #[test]
    fn vram_prefetch_dummy_read() {
        let mut ppu = Ppu::new();
        ppu.vram[5] = 0xABCD;
        ppu.vram[6] = 0x1122;
        ppu.write(0x15, 0x00); // step 1, increment/reload on $2139 low read
        set_addr(&mut ppu, 5); // prefetch loaded with vram[5]
        // Full-word read pattern on the low port: A, A, A+1 ...
        assert_eq!(ppu.read(0x39), Some(0xCD)); // vram[5] low
        assert_eq!(ppu.read(0x39), Some(0xCD)); // still vram[5] low (dummy)
        assert_eq!(ppu.read(0x39), Some(0x22)); // vram[6] low
    }

    #[test]
    fn cgram_word_latch_write_and_read() {
        let mut ppu = Ppu::new();
        ppu.write(0x21, 0x10); // CGADD = 0x10
        ppu.write(0x22, 0x34); // low latch
        ppu.write(0x22, 0x12); // store word 0x1234 & 0x7FFF
        assert_eq!(ppu.cgram[0x10], 0x1234);
        assert_eq!(ppu.cgram_addr, 0x11);
        // bit15 masked off on write
        ppu.write(0x21, 0x20);
        ppu.write(0x22, 0xFF);
        ppu.write(0x22, 0xFF);
        assert_eq!(ppu.cgram[0x20], 0x7FFF);
        // read back index 0x10
        ppu.write(0x21, 0x10);
        assert_eq!(ppu.read(0x3B), Some(0x34)); // low
        assert_eq!(ppu.read(0x3B), Some(0x12)); // high (bit7 = ppu2 open bus = 0)
        assert_eq!(ppu.cgram_addr, 0x11);
    }

    #[test]
    fn oam_word_and_byte_addressing() {
        let mut ppu = Ppu::new();
        // Table 1: even write latches, odd write commits the word.
        ppu.write(0x02, 0x00); // OAMADDL
        ppu.write(0x03, 0x00); // OAMADDH -> internal addr 0
        ppu.write(0x04, 0x11); // latch
        ppu.write(0x04, 0x22); // write word to bytes 0,1
        assert_eq!(ppu.oam_lo[0], 0x11);
        assert_eq!(ppu.oam_lo[1], 0x22);
        assert_eq!(ppu.oam_addr, 2);
        // Table 2: byte-direct writes. OAMADD bit8 selects high table.
        ppu.write(0x02, 0x00);
        ppu.write(0x03, 0x01); // internal addr = (0x100 & 0x1FF) << 1 = 0x200
        assert_eq!(ppu.oam_addr, 0x200);
        ppu.write(0x04, 0x5A);
        assert_eq!(ppu.oam_hi[0], 0x5A);
        assert_eq!(ppu.oam_addr, 0x201);
        // read back table 1 byte 0
        ppu.write(0x02, 0x00);
        ppu.write(0x03, 0x00);
        assert_eq!(ppu.read(0x38), Some(0x11));
        assert_eq!(ppu.read(0x38), Some(0x22));
    }

    #[test]
    fn oam_high_table_mirrors_32_bytes() {
        let mut ppu = Ppu::new();
        ppu.oam_hi[0] = 0x99;
        // internal addr 0x220 mirrors 0x200 (addr & 0x1F = 0)
        ppu.oam_addr = 0x220;
        assert_eq!(ppu.oam_read(), 0x99);
    }

    #[test]
    fn counter_latch_and_flipflops() {
        let mut ppu = Ppu::new();
        ppu.set_hv_counters(0x123, 0x0AB);
        // Before latch the flag is clear.
        assert!(!ppu.counter_latched);
        ppu.read(0x37); // SLHV latches, returns open bus (None)
        assert!(ppu.counter_latched);
        // OPHCT: low byte then bit8.
        assert_eq!(ppu.read(0x3C), Some(0x23));
        assert_eq!(ppu.read(0x3C).map(|v| v & 1), Some(0x01));
        // OPVCT: low byte then bit8 (0xAB -> low 0xAB, bit8 = 0).
        assert_eq!(ppu.read(0x3D), Some(0xAB));
        assert_eq!(ppu.read(0x3D).map(|v| v & 1), Some(0x00));
        // STAT78 reports the latch flag (still set), then clears it and resets
        // the flip-flops.
        let s = ppu.read(0x3F).unwrap();
        assert_eq!(s & 0x40, 0x40);
        assert!(!ppu.counter_latched);
        assert!(!ppu.ophct_hi);
        assert!(!ppu.opvct_hi);
    }

    #[test]
    fn stat78_region_bit_preserved() {
        let mut ppu = Ppu::new();
        ppu.is_pal = false;
        assert_eq!(ppu.read(0x3F), Some(0x01));
        let mut ppu = Ppu::new();
        ppu.is_pal = true;
        assert_eq!(ppu.read(0x3F), Some(0x11));
    }

    #[test]
    fn bg_scroll_shared_latch() {
        let mut ppu = Ppu::new();
        // BG1HOFS write twice (low first): value2<<8 | (bgofs_latch & ~7) | (bghofs_latch & 7)
        ppu.write(0x0D, 0x05); // bgofs_latch=bghofs_latch=0x05
        ppu.write(0x0D, 0x01); // BG1HOFS = (0x01<<8)|(0x05&~7)|(0x05&7) = 0x100|0x00|0x05 = 0x105
        assert_eq!(ppu.bg_hofs[0], 0x105);
        // BG1VOFS: value<<8 | bgofs_latch
        ppu.write(0x0E, 0x02); // bgofs_latch currently 0x01 -> VOFS = ... then latch=0x02
        assert_eq!(ppu.bg_vofs[0], (0x02 << 8 | 0x01) & 0x3FF);
    }

    #[test]
    fn obsel_decode() {
        let mut ppu = Ppu::new();
        // size=011, NN=10, base=101
        ppu.write(0x01, 0b011_10_101);
        assert_eq!(ppu.obj_size, 0b011);
        assert_eq!(ppu.obj_name_base, 0b101 << 13);
        assert_eq!(ppu.obj_name_gap, ((0b10u16) + 1) << 12);
    }

    #[test]
    fn open_bus_latches_refresh_on_read() {
        let mut ppu = Ppu::new();
        // A PPU1 read drives the PPU1 open-bus latch; STAT77 bit4 is undriven
        // and returns that latched value on the next read.
        ppu.oam_lo[0] = 0xFF;
        ppu.oam_addr = 0;
        assert_eq!(ppu.read(0x38), Some(0xFF));
        assert_eq!(ppu.read(0x3E).unwrap() & 0x10, 0x10);
        // A PPU2 read drives the PPU2 latch; STAT78 bit5 (undriven) reflects it.
        ppu.ophct = 0x1FF;
        ppu.ophct_hi = false;
        assert_eq!(ppu.read(0x3C), Some(0xFF));
        assert_eq!(ppu.read(0x3F).unwrap() & 0x20, 0x20);
        // OPHCT high read: bits7-1 come from the PPU2 latch (=0xFF here).
        ppu.ophct = 0x100;
        ppu.ophct_hi = false;
        assert_eq!(ppu.read(0x3C), Some(0x00)); // low byte -> ppu2_mdr = 0x00
        assert_eq!(ppu.read(0x3C), Some(0x01)); // bit8=1, bits7-1 = mdr = 0
    }

    #[test]
    fn mode7_mpy() {
        let mut ppu = Ppu::new();
        // M7A = 0x0002
        ppu.write(0x1B, 0x02);
        ppu.write(0x1B, 0x00);
        // M7B second (high) byte = multiplier: write low 0, high 3 -> operand 3
        ppu.write(0x1C, 0x00);
        ppu.write(0x1C, 0x03);
        let expected = (0x0002i32 * 3) as u32;
        assert_eq!(ppu.read(0x34), Some((expected & 0xFF) as u8));
        assert_eq!(ppu.read(0x35), Some((expected >> 8) as u8));
        assert_eq!(ppu.read(0x36), Some((expected >> 16) as u8));
    }
}
