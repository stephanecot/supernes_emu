//! snes-core — pure SNES emulation library. No I/O dependencies; fully testable headless.

pub mod apu;
pub mod bus;
pub mod cartridge;
pub mod coprocessor;
pub mod cpu;
pub mod debug;
pub mod dma;
pub mod joypad;
pub mod ppu;
pub mod scheduler;
pub(crate) mod serde_util;
pub mod snes;

pub use cartridge::{Cartridge, Mapping};
pub use joypad::JoypadState;
pub use scheduler::Region;
pub use snes::Snes;

pub const SCREEN_WIDTH: usize = 256;
pub const SCREEN_HEIGHT: usize = 224;

/// Final composited frame, 256x224 pixels in SNES BGR555 format
/// (bits 0-4 = red, 5-9 = green, 10-14 = blue, bit 15 unused).
pub struct FrameBuffer(pub Box<[u16; SCREEN_WIDTH * SCREEN_HEIGHT]>);

impl FrameBuffer {
    pub fn new() -> Self {
        FrameBuffer(vec![0u16; SCREEN_WIDTH * SCREEN_HEIGHT].into_boxed_slice().try_into().unwrap())
    }

    /// Expand BGR555 to RGBA8888. `out` must be at least 256*224*4 bytes.
    /// 5-bit channels expand as (c << 3) | (c >> 2) so $1F maps to $FF.
    pub fn to_rgba(&self, out: &mut [u8]) {
        assert!(out.len() >= SCREEN_WIDTH * SCREEN_HEIGHT * 4);
        for (i, &px) in self.0.iter().enumerate() {
            let r = (px & 0x1F) as u8;
            let g = ((px >> 5) & 0x1F) as u8;
            let b = ((px >> 10) & 0x1F) as u8;
            out[i * 4] = (r << 3) | (r >> 2);
            out[i * 4 + 1] = (g << 3) | (g >> 2);
            out[i * 4 + 2] = (b << 3) | (b >> 2);
            out[i * 4 + 3] = 0xFF;
        }
    }
}

impl Default for FrameBuffer {
    fn default() -> Self {
        Self::new()
    }
}
