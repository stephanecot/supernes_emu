//! `.spc` music-file export (SNES-SPC700 Sound File Data v0.30).
//!
//! An `.spc` file is a snapshot of the audio subsystem alone: the SPC700
//! register file, the 64 KB ARAM (which contains the game's sound driver, its
//! sequence data and its BRR samples), and the 128 S-DSP registers. A player
//! resumes the SPC700 from that state and the music keeps going, which is why
//! no PCM is stored.
//!
//! Layout (offsets from the v0.30 specification, cross-checked against
//! blargg's `snes_spc` reference implementation, `SNES_SPC::save_spc`):
//!
//! | Offset | Size  | Contents                                              |
//! |--------|-------|-------------------------------------------------------|
//! | $00000 | 33    | `SNES-SPC700 Sound File Data v0.30`                    |
//! | $00021 | 2     | 26, 26                                                |
//! | $00023 | 1     | 26 = ID666 tag present, 27 = absent                    |
//! | $00024 | 1     | version minor (30)                                    |
//! | $00025 | 2     | PC (little-endian)                                    |
//! | $00027 | 1     | A                                                     |
//! | $00028 | 1     | X                                                     |
//! | $00029 | 1     | Y                                                     |
//! | $0002A | 1     | PSW                                                   |
//! | $0002B | 1     | SP (low byte; the stack is fixed at page $01)          |
//! | $0002C | 2     | reserved                                              |
//! | $0002E | 210   | ID666 tag (text format, see `write_id666`)             |
//! | $00100 | 65536 | 64 KB ARAM                                             |
//! | $10100 | 128   | S-DSP registers $00-$7F                                |
//! | $10180 | 64    | unused                                                 |
//! | $101C0 | 64    | IPL ROM image                                          |
//!
//! Total size: 66048 bytes ($10200).
//!
//! Two details the offset table does not spell out, both taken from the
//! reference implementation:
//!   * the ARAM block holds the *true* RAM at $FFC0-$FFFF (the RAM hidden
//!     under the IPL ROM overlay), not the overlay itself — the overlay is
//!     carried separately in the last 64 bytes;
//!   * the $F0-$FF page of the ARAM block is overwritten with the SPC700 I/O
//!     register image (TEST, CONTROL, DSPADDR, the four CPU-in ports, the timer
//!     targets and the live timer counters), since those registers are not RAM
//!     and have no other place in the format. CONTROL ($F1) is what tells a
//!     player whether the IPL ROM overlay and the timers were enabled.

use std::path::Path;

use snes_core::apu::ipl::IPL_ROM;
use snes_core::apu::SpcRegisters;
use snes_core::Snes;

/// Size of a v0.30 `.spc` file without the optional trailing `xid6` chunk.
pub const FILE_SIZE: usize = 0x1_0200;

/// File signature, exactly 33 bytes with no terminator.
const SIGNATURE: &[u8; 33] = b"SNES-SPC700 Sound File Data v0.30";

const OFF_HAS_ID666: usize = 0x23;
const OFF_VERSION_MINOR: usize = 0x24;
const OFF_PC: usize = 0x25;
const OFF_ID666: usize = 0x2E;
const OFF_RAM: usize = 0x100;
const OFF_DSP: usize = 0x1_0100;
const OFF_UNUSED: usize = 0x1_0180;
const OFF_IPL_ROM: usize = 0x1_01C0;

/// $23 marker: an ID666 tag follows (27 would mean "no tag").
const HAS_ID666: u8 = 26;
/// Minor version of the format written in the header at $24.
const VERSION_MINOR: u8 = 30;

/// Default ID666 play length, in seconds — what a player uses before fading.
const PLAY_SECONDS: u32 = 180;
/// Default ID666 fade length, in milliseconds.
const FADE_MS: u32 = 10_000;

/// Name written into the ID666 "dumper" field (16 bytes).
const DUMPER: &str = "Prisme";

/// Build the `.spc` image for the console's current audio state. `game_title`
/// fills the ID666 game/song title fields (a cartridge holds no song names).
pub fn build(snes: &Snes, game_title: &str) -> Vec<u8> {
    let apu = &snes.bus.apu;
    build_image(
        apu.spc_registers(),
        apu.aram(),
        apu.io_registers(),
        apu.dsp_registers(),
        game_title,
        &crate::now_local().id666_date(),
    )
}

/// Write the `.spc` image for the current audio state to `path`, creating the
/// parent directory if needed.
pub fn write(snes: &Snes, path: &Path, game_title: &str) -> Result<(), String> {
    let bytes = build(snes, game_title);
    crate::write_new_file(path, &bytes)
}

/// Assemble the file from already-extracted APU state (pure; the unit tests
/// drive this directly).
fn build_image(
    regs: SpcRegisters,
    aram: &[u8; 0x10000],
    io_regs: [u8; 16],
    dsp_regs: [u8; 128],
    game_title: &str,
    dump_date: &str,
) -> Vec<u8> {
    let mut out = vec![0u8; FILE_SIZE];
    out[..SIGNATURE.len()].copy_from_slice(SIGNATURE);
    out[0x21] = 26;
    out[0x22] = 26;
    out[OFF_HAS_ID666] = HAS_ID666;
    out[OFF_VERSION_MINOR] = VERSION_MINOR;

    out[OFF_PC..OFF_PC + 2].copy_from_slice(&regs.pc.to_le_bytes());
    out[0x27] = regs.a;
    out[0x28] = regs.x;
    out[0x29] = regs.y;
    out[0x2A] = regs.psw;
    out[0x2B] = regs.sp;

    write_id666(&mut out[OFF_ID666..OFF_RAM], game_title, dump_date);

    out[OFF_RAM..OFF_RAM + 0x10000].copy_from_slice(&aram[..]);
    // The I/O page is not RAM: patch in the register image (see module docs).
    out[OFF_RAM + 0xF0..OFF_RAM + 0x100].copy_from_slice(&io_regs);

    out[OFF_DSP..OFF_DSP + 128].copy_from_slice(&dsp_regs);
    // $10180..$101C0 stays zero (unused).
    debug_assert!(out[OFF_UNUSED..OFF_IPL_ROM].iter().all(|&b| b == 0));
    out[OFF_IPL_ROM..OFF_IPL_ROM + 64].copy_from_slice(&IPL_ROM);
    out
}

/// ID666 tag, text format. Field offsets are relative to $2E:
/// song title 32, game title 32, dumper 16, comments 32, dump date 11
/// (`MM/DD/YYYY`), play seconds 3, fade ms 5, artist 32, channel-disable 1,
/// emulator 1, reserved 45. All text fields are NUL-padded ASCII; the numeric
/// fields are ASCII decimal, also NUL-padded.
fn write_id666(tag: &mut [u8], game_title: &str, dump_date: &str) {
    put_text(&mut tag[0x00..0x20], game_title); // song title (no song names on a cart)
    put_text(&mut tag[0x20..0x40], game_title); // game title
    put_text(&mut tag[0x40..0x50], DUMPER);
    put_text(&mut tag[0x50..0x70], ""); // comments
    put_text(&mut tag[0x70..0x7B], dump_date);
    // The two numeric fields are sized to their maximum value (999 s,
    // 99999 ms), so a full-width value legitimately leaves no terminator.
    put_number(&mut tag[0x7B..0x7E], PLAY_SECONDS);
    put_number(&mut tag[0x7E..0x83], FADE_MS);
    put_text(&mut tag[0x83..0xA3], ""); // artist
    // $D1 default channel disable: 0 = every voice enabled.
    tag[0xA3] = 0;
    // $D2 emulator used: 0 = unknown (the enumerated values are 1 = ZSNES,
    // 2 = Snes9x).
    tag[0xA4] = 0;
}

/// Copy `text` into a fixed-size NUL-padded ASCII field, truncated so at least
/// one terminating NUL always remains. Non-ASCII and control bytes become
/// spaces: the format predates any encoding declaration and players assume
/// plain ASCII.
fn put_text(field: &mut [u8], text: &str) {
    field.fill(0);
    let max = field.len() - 1;
    for (dst, c) in field.iter_mut().zip(text.chars().take(max)) {
        *dst = if c.is_ascii() && !c.is_ascii_control() { c as u8 } else { b' ' };
    }
}

/// ASCII decimal in a fixed-width field, NUL-padded on the right. A value too
/// wide for the field is clamped to all-nines rather than truncated (which
/// would divide it by a power of ten).
fn put_number(field: &mut [u8], value: u32) {
    field.fill(0);
    let text = value.to_string();
    if text.len() > field.len() {
        field.fill(b'9');
        return;
    }
    field[..text.len()].copy_from_slice(text.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regs() -> SpcRegisters {
        SpcRegisters { pc: 0x1234, a: 0xAA, x: 0xBB, y: 0xCC, psw: 0x82, sp: 0xEF }
    }

    fn image() -> Vec<u8> {
        let mut aram = Box::new([0u8; 0x10000]);
        aram[0x0000] = 0x11;
        aram[0x0200] = 0x22;
        // I/O page contents in ARAM are stale and must be replaced by the
        // register image, except $F8/$F9 which really are RAM.
        aram[0xF0] = 0x99;
        aram[0xFFC0] = 0x55; // RAM hidden under the IPL ROM overlay
        aram[0xFFFF] = 0x66;
        let mut io = [0u8; 16];
        io[0x0] = 0x0A; // TEST
        io[0x1] = 0x80; // CONTROL: IPL ROM enabled
        io[0x4] = 0xCC; // $F4 port
        io[0xD] = 0x03; // T0OUT
        let mut dsp = [0u8; 128];
        dsp[0x6C] = 0x20; // FLG
        dsp[0x7F] = 0x7E;
        build_image(regs(), &aram, io, dsp, "SUPER GAME", "07/24/2026")
    }

    #[test]
    fn file_is_exactly_66048_bytes() {
        assert_eq!(FILE_SIZE, 66_048);
        assert_eq!(image().len(), 66_048);
    }

    #[test]
    fn header_matches_the_v030_spec() {
        let f = image();
        assert_eq!(&f[0..33], b"SNES-SPC700 Sound File Data v0.30");
        assert_eq!(f[0x21], 26);
        assert_eq!(f[0x22], 26);
        assert_eq!(f[0x23], 26, "ID666 tag present");
        assert_eq!(f[0x24], 30, "version minor");
    }

    #[test]
    fn spc700_registers_are_at_their_documented_offsets() {
        let f = image();
        assert_eq!(u16::from_le_bytes([f[0x25], f[0x26]]), 0x1234);
        assert_eq!(f[0x27], 0xAA);
        assert_eq!(f[0x28], 0xBB);
        assert_eq!(f[0x29], 0xCC);
        assert_eq!(f[0x2A], 0x82);
        assert_eq!(f[0x2B], 0xEF);
    }

    #[test]
    fn id666_text_fields_are_nul_padded_at_their_offsets() {
        let f = image();
        assert_eq!(&f[0x2E..0x38], b"SUPER GAME");
        assert_eq!(f[0x2E + 10], 0, "song title is NUL-padded");
        assert_eq!(&f[0x4E..0x58], b"SUPER GAME");
        assert_eq!(&f[0x6E..0x6E + DUMPER.len()], DUMPER.as_bytes());
        assert_eq!(&f[0x9E..0xA8], b"07/24/2026");
        assert_eq!(f[0xA8], 0, "date field's 11th byte is the terminator");
        assert_eq!(&f[0xA9..0xAC], b"180", "play length in seconds");
        assert_eq!(&f[0xAC..0xB1], b"10000", "fade length in ms");
        assert_eq!(f[0xD1], 0, "no channel disabled");
        assert_eq!(f[0xD2], 0, "emulator: unknown");
        // Everything from the reserved area to the RAM block stays zero.
        assert!(f[0xD3..0x100].iter().all(|&b| b == 0));
    }

    #[test]
    fn numeric_fields_fill_their_width_without_a_terminator() {
        let mut field = [0xFFu8; 3];
        put_number(&mut field, 180);
        assert_eq!(&field, b"180");
        let mut field = [0xFFu8; 5];
        put_number(&mut field, 90);
        assert_eq!(&field, b"90\0\0\0");
        // Overflow is clamped, never truncated into a wrong value.
        let mut field = [0u8; 3];
        put_number(&mut field, 12_345);
        assert_eq!(&field, b"999");
    }

    #[test]
    fn long_and_non_ascii_titles_are_truncated_and_stay_terminated() {
        let mut field = [0xFFu8; 8];
        put_text(&mut field, "ABCDEFGHIJ");
        assert_eq!(&field, b"ABCDEFG\0");
        let mut field = [0u8; 8];
        put_text(&mut field, "Pokémon");
        assert_eq!(&field[..7], b"Pok mon");
        assert_eq!(field[7], 0);
    }

    #[test]
    fn ram_block_carries_aram_with_the_io_page_patched_in() {
        let f = image();
        assert_eq!(f[0x100], 0x11);
        assert_eq!(f[0x100 + 0x200], 0x22);
        // $F0 comes from the register image, not from the stale ARAM byte.
        assert_eq!(f[0x100 + 0xF0], 0x0A);
        assert_eq!(f[0x100 + 0xF1], 0x80);
        assert_eq!(f[0x100 + 0xF4], 0xCC);
        assert_eq!(f[0x100 + 0xFD], 0x03);
        // The RAM hidden under the IPL ROM overlay is what the block holds.
        assert_eq!(f[0x100 + 0xFFC0], 0x55);
        assert_eq!(f[0x100 + 0xFFFF], 0x66);
    }

    #[test]
    fn dsp_unused_and_ipl_rom_blocks_are_at_their_offsets() {
        let f = image();
        assert_eq!(f[0x1_0100 + 0x6C], 0x20);
        assert_eq!(f[0x1_0100 + 0x7F], 0x7E);
        assert!(f[0x1_0180..0x1_01C0].iter().all(|&b| b == 0), "unused block must be zero");
        assert_eq!(&f[0x1_01C0..0x1_0200], &IPL_ROM[..]);
        // The IPL ROM's reset vector ($FFFE/$FFFF -> $FFC0) is the last word.
        assert_eq!(&f[0x1_01FE..0x1_0200], &[0xC0, 0xFF]);
    }
}
