//! CX4 HLE unit tests against the documented op vectors (cx4.md §7).

use super::tables::{COS_TABLE, SIN_TABLE};
use super::{is_cx4, maps, Cx4};

fn set_word(c: &mut Cx4, addr: u16, v: u16) {
    c.write(&[], addr, v as u8);
    c.write(&[], addr + 1, (v >> 8) as u8);
}

fn set_3word(c: &mut Cx4, addr: u16, v: u32) {
    c.write(&[], addr, v as u8);
    c.write(&[], addr + 1, (v >> 8) as u8);
    c.write(&[], addr + 2, (v >> 16) as u8);
}

fn get_word(c: &Cx4, addr: u16) -> u16 {
    c.read(addr) as u16 | ((c.read(addr + 1) as u16) << 8)
}

fn get_3word(c: &Cx4, addr: u16) -> u32 {
    c.read(addr) as u32 | ((c.read(addr + 1) as u32) << 8) | ((c.read(addr + 2) as u32) << 16)
}

fn run(c: &mut Cx4, sub: u8, cmd: u8) {
    c.write(&[], 0x7F4D, sub);
    c.write(&[], 0x7F4F, cmd);
}

#[test]
fn sin_cos_table_symmetry() {
    assert_eq!(SIN_TABLE[0], 0);
    assert_eq!(SIN_TABLE[128], 32767);
    assert_eq!(SIN_TABLE[256], 0);
    assert_eq!(SIN_TABLE[384], -32767);
    for i in 0..512 {
        assert_eq!(COS_TABLE[i], SIN_TABLE[(i + 128) & 0x1FF]);
    }
}

#[test]
fn op15_pythagorean() {
    let mut c = Cx4::new();
    set_word(&mut c, 0x7F80, 3);
    set_word(&mut c, 0x7F83, 4);
    run(&mut c, 0x02, 0x15);
    assert_eq!(get_word(&c, 0x7F80), 5);

    let mut c = Cx4::new();
    set_word(&mut c, 0x7F80, 0);
    set_word(&mut c, 0x7F83, 0);
    run(&mut c, 0x02, 0x15);
    assert_eq!(get_word(&c, 0x7F80), 0);
}

#[test]
fn op1f_atan_axes() {
    // X=0, Y>0 -> $080
    let mut c = Cx4::new();
    set_word(&mut c, 0x7F80, 0);
    set_word(&mut c, 0x7F83, 1);
    run(&mut c, 0x02, 0x1F);
    assert_eq!(get_word(&c, 0x7F86), 0x080);

    // X=0, Y<0 -> $180
    let mut c = Cx4::new();
    set_word(&mut c, 0x7F80, 0);
    set_word(&mut c, 0x7F83, 0xFFFF);
    run(&mut c, 0x02, 0x1F);
    assert_eq!(get_word(&c, 0x7F86), 0x180);

    // X>0, Y=0 -> $000
    let mut c = Cx4::new();
    set_word(&mut c, 0x7F80, 1);
    set_word(&mut c, 0x7F83, 0);
    run(&mut c, 0x02, 0x1F);
    assert_eq!(get_word(&c, 0x7F86), 0x000);

    // X<0, Y=0 -> $100
    let mut c = Cx4::new();
    set_word(&mut c, 0x7F80, 0xFFFF);
    set_word(&mut c, 0x7F83, 0);
    run(&mut c, 0x02, 0x1F);
    assert_eq!(get_word(&c, 0x7F86), 0x100);
}

#[test]
fn cmd25_multiply() {
    let mut c = Cx4::new();
    set_3word(&mut c, 0x7F80, 2);
    set_3word(&mut c, 0x7F83, 3);
    run(&mut c, 0x02, 0x25);
    assert_eq!(get_3word(&c, 0x7F80), 6);

    // Wraps mod 2^24.
    let mut c = Cx4::new();
    set_3word(&mut c, 0x7F80, 0x00_1000);
    set_3word(&mut c, 0x7F83, 0x00_1000);
    run(&mut c, 0x02, 0x25);
    assert_eq!(get_3word(&c, 0x7F80), 0x100_0000 & 0xFF_FFFF);
}

#[test]
fn cmd54_square() {
    let mut c = Cx4::new();
    set_3word(&mut c, 0x7F80, 3);
    run(&mut c, 0x0E, 0x54);
    assert_eq!(get_3word(&c, 0x7F83), 9);
    assert_eq!(get_3word(&c, 0x7F86), 0);

    // -1 (0xFFFFFF sign-extended) squared = 1.
    let mut c = Cx4::new();
    set_3word(&mut c, 0x7F80, 0xFF_FFFF);
    run(&mut c, 0x0E, 0x54);
    assert_eq!(get_3word(&c, 0x7F83), 1);
    assert_eq!(get_3word(&c, 0x7F86), 0);
}

#[test]
fn cmd05_propulsion() {
    let mut c = Cx4::new();
    set_word(&mut c, 0x7F83, 0x0100);
    set_word(&mut c, 0x7F81, 0x0200);
    run(&mut c, 0x02, 0x05);
    assert_eq!(get_word(&c, 0x7F80), 0x0200);
}

#[test]
fn cmd13_polar_to_rect() {
    // theta=128 (90 deg): Cos=0, Sin=32767; r=1.
    // X = SAR(1*0*2, 8) = 0 ; Y = SAR(1*32767*2, 8) = 255.
    let mut c = Cx4::new();
    set_word(&mut c, 0x7F80, 128);
    set_word(&mut c, 0x7F83, 1);
    run(&mut c, 0x02, 0x13);
    assert_eq!(get_3word(&c, 0x7F86), 0);
    assert_eq!(get_3word(&c, 0x7F89), 255);
}

#[test]
fn cmd10_polar_to_rect_with_skew() {
    // theta=0: Cos=32767, Sin=0; r1=256.
    // X = SAR(256*32767*2, 16) = 255 ; Y = 0 - SAR(0,6) = 0.
    let mut c = Cx4::new();
    set_word(&mut c, 0x7F80, 0);
    set_word(&mut c, 0x7F83, 0x0100);
    run(&mut c, 0x02, 0x10);
    assert_eq!(get_3word(&c, 0x7F86), 255);
    assert_eq!(get_3word(&c, 0x7F89), 0);
}

#[test]
fn cmd2d_transform_identity() {
    // Angles 0, scale 0x100 -> orthographic identity: (X,Y) pass through.
    let mut c = Cx4::new();
    set_word(&mut c, 0x7F81, 10); // X
    set_word(&mut c, 0x7F84, 20); // Y
    set_word(&mut c, 0x7F87, 0); // Z
    c.write(&[], 0x7F89, 0); // X angle
    c.write(&[], 0x7F8A, 0); // Y angle
    c.write(&[], 0x7F8B, 0); // Z angle
    set_word(&mut c, 0x7F90, 0x0100); // scale
    run(&mut c, 0x02, 0x2D);
    assert_eq!(get_word(&c, 0x7F80) as i16, 10);
    assert_eq!(get_word(&c, 0x7F83) as i16, 20);
}

#[test]
fn cmd89_immediate_rom() {
    let mut c = Cx4::new();
    run(&mut c, 0x0E, 0x89);
    assert_eq!(c.read(0x7F80), 0x36);
    assert_eq!(c.read(0x7F81), 0x43);
    assert_eq!(c.read(0x7F82), 0x05);
}

#[test]
fn param_poke_special_case() {
    // sub-mode $0E, byte < $40, (byte&3)==0 => $7F80 = byte>>2, no op.
    let mut c = Cx4::new();
    c.write(&[], 0x7F4D, 0x0E);
    c.write(&[], 0x7F4F, 0x20);
    assert_eq!(c.read(0x7F80), 0x08);
}

#[test]
fn status_register_idle() {
    let c = Cx4::new();
    assert_eq!(c.read(0x7F5E), 0x00);
}

#[test]
fn dma_load_from_rom() {
    // 8 KB ROM: LoROM fetch of addr $000010 -> ROM offset 0x10.
    let mut rom = vec![0u8; 0x8000];
    rom[0x10] = 0xAB;
    rom[0x11] = 0xCD;
    let mut c = Cx4::new();
    set_3word(&mut c, 0x7F40, 0x00_0010);
    set_word(&mut c, 0x7F43, 2);
    set_word(&mut c, 0x7F45, 0x6100);
    c.write(&rom, 0x7F47, 0x00);
    assert_eq!(c.read(0x6100), 0xAB);
    assert_eq!(c.read(0x6101), 0xCD);
}

#[test]
fn detection_and_mapping() {
    assert!(is_cx4(0x20, 0xF3));
    assert!(is_cx4(0x30, 0xF3));
    assert!(!is_cx4(0x20, 0xF5));
    assert!(!is_cx4(0x21, 0xF3));

    assert!(maps(0x00, 0x6000));
    assert!(maps(0x3F, 0x7FFF));
    assert!(maps(0x80, 0x7000));
    assert!(!maps(0x00, 0x8000));
    assert!(!maps(0x40, 0x6000));
}
