//! SuperFX / GSU cartridge coprocessor.

pub mod gsu;
pub mod plot;
pub mod regs;

pub use gsu::{SuperFx, VCR_GSU2};

#[cfg(test)]
mod tests;
