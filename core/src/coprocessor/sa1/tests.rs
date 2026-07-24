//! SA-1 integration tests: message-port handshake, IRQ/NMI state transitions,
//! vector overrides, and a small end-to-end SA-1 code run.

use super::*;

const NO_ROM: &[u8] = &[];

fn sa1() -> Sa1 {
    Sa1::new(0x40000)
}

#[test]
fn detection() {
    assert!(is_sa1(0x23, 0x00)); // map_mode low nibble 3
    assert!(is_sa1(0x33, 0x00));
    assert!(is_sa1(0x00, 0x35)); // chipset high nibble 3
    assert!(!is_sa1(0x20, 0x02)); // plain HiROM
    assert!(!is_sa1(0x30, 0x13)); // GSU
}

#[test]
fn message_ports_roundtrip() {
    let mut s = sa1();
    // S-CPU -> SA-1 message in CCNT low nibble, read back in CFR low nibble.
    s.write_io(NO_ROM, 0x2200, 0x0A);
    assert_eq!(s.read_io(NO_ROM, 0x2301) & 0x0F, 0x0A);
    // SA-1 -> S-CPU message in SCNT low nibble, read back in SFR low nibble.
    s.write_io(NO_ROM, 0x2209, 0x05);
    assert_eq!(s.read_io(NO_ROM, 0x2300) & 0x0F, 0x05);
}

#[test]
fn scpu_to_sa1_irq_handshake() {
    let mut s = sa1();
    // Enable SA-1 IRQ-from-S-CPU (CIE.I).
    s.write_io(NO_ROM, 0x220A, 0x80);
    // S-CPU requests IRQ (CCNT.I) while keeping reset asserted (bit5).
    s.write_io(NO_ROM, 0x2200, 0xA0);
    assert!(s.read_io(NO_ROM, 0x2301) & 0x80 != 0, "CFR.I pending");
    assert!(sa1_irq_line(&s), "SA-1 IRQ asserted");
    // SA-1 acknowledges via CIC.I (write-1-to-clear).
    s.write_io(NO_ROM, 0x220B, 0x80);
    assert!(s.read_io(NO_ROM, 0x2301) & 0x80 == 0);
    assert!(!sa1_irq_line(&s));
}

#[test]
fn scpu_to_sa1_nmi_handshake() {
    let mut s = sa1();
    s.write_io(NO_ROM, 0x220A, 0x10); // CIE.N enable
    s.write_io(NO_ROM, 0x2200, 0x30); // CCNT.N + hold reset
    assert!(s.read_io(NO_ROM, 0x2301) & 0x10 != 0, "CFR.N pending");
    s.write_io(NO_ROM, 0x220B, 0x10); // CIC.N clear
    assert!(s.read_io(NO_ROM, 0x2301) & 0x10 == 0);
}

#[test]
fn sa1_to_scpu_irq_handshake() {
    let mut s = sa1();
    s.write_io(NO_ROM, 0x2201, 0x80); // SIE.I enable
    s.write_io(NO_ROM, 0x2209, 0x80); // SCNT.I request
    assert!(s.read_io(NO_ROM, 0x2300) & 0x80 != 0, "SFR.I pending");
    assert!(s.scpu_irq_line(), "S-CPU IRQ asserted");
    s.write_io(NO_ROM, 0x2202, 0x80); // SIC.I clear
    assert!(!s.scpu_irq_line());
    assert!(s.read_io(NO_ROM, 0x2300) & 0x80 == 0);
}

#[test]
fn scpu_irq_masked_when_disabled() {
    let mut s = sa1();
    s.write_io(NO_ROM, 0x2209, 0x80); // request, but SIE.I disabled
    assert!(s.read_io(NO_ROM, 0x2300) & 0x80 != 0, "pending flag still set");
    assert!(!s.scpu_irq_line(), "line gated by SIE.I");
}

#[test]
fn scpu_vector_overrides() {
    let mut s = sa1();
    assert_eq!(s.scpu_irq_vector(), None);
    assert_eq!(s.scpu_nmi_vector(), None);
    // Program the override vectors.
    s.write_io(NO_ROM, 0x220E, 0x34); // SIVL
    s.write_io(NO_ROM, 0x220F, 0x12); // SIVH
    s.write_io(NO_ROM, 0x220C, 0x78); // SNVL
    s.write_io(NO_ROM, 0x220D, 0x56); // SNVH
    // SCNT: S=1 (IRQ vec = SIV), N=1 (NMI vec = SNV).
    s.write_io(NO_ROM, 0x2209, 0x50);
    assert_eq!(s.scpu_irq_vector(), Some(0x1234));
    assert_eq!(s.scpu_nmi_vector(), Some(0x5678));
    // Reflected in SFR bits V (bit6) and N (bit4).
    let sfr = s.read_io(NO_ROM, 0x2300);
    assert!(sfr & 0x40 != 0 && sfr & 0x10 != 0);
}

#[test]
fn arithmetic_registers_via_io() {
    let mut s = sa1();
    s.write_io(NO_ROM, 0x2250, 0x00); // multiply
    s.write_io(NO_ROM, 0x2251, 0x03); // MAL
    s.write_io(NO_ROM, 0x2252, 0x00); // MAH -> MA = 3
    s.write_io(NO_ROM, 0x2253, 0x05); // MBL
    s.write_io(NO_ROM, 0x2254, 0x00); // MBH -> MB = 5, run
    // 3 * 5 = 15.
    assert_eq!(s.read_io(NO_ROM, 0x2306), 15);
    assert_eq!(s.read_io(NO_ROM, 0x2307), 0);
}

#[test]
fn sa1_cpu_runs_and_writes_iram() {
    // SA-1 program at ROM offset 0 (LoROM $00:8000 with default MMC):
    //   LDA #$42 ; STA $3000 ; STP
    let mut rom = vec![0u8; 0x10000];
    rom[0] = 0xA9;
    rom[1] = 0x42;
    rom[2] = 0x8D;
    rom[3] = 0x00;
    rom[4] = 0x30;
    rom[5] = 0xDB;

    let mut s = sa1();
    // Reset vector CRV = $8000.
    s.write_io(&rom, 0x2203, 0x00);
    s.write_io(&rom, 0x2204, 0x80);
    // Allow SA-1 to write I-RAM (CIWP all pages).
    s.write_io(&rom, 0x222A, 0xFF);
    // Release SA-1 from reset (CCNT with reset bit clear, no wait).
    s.write_io(&rom, 0x2200, 0x00);

    s.run(&rom, 10_000);

    assert!(s.cpu.stopped, "SA-1 halted on STP");
    assert_eq!(s.read_iram(0), 0x42);
}

#[test]
fn iram_protection_blocks_write() {
    let mut s = sa1();
    // No SIWP pages enabled: S-CPU write dropped.
    s.write_iram_scpu(0x10, 0x99);
    assert_eq!(s.read_iram(0x10), 0x00);
    // Enable page 0.
    s.write_io(NO_ROM, 0x2229, 0x01);
    s.write_iram_scpu(0x10, 0x99);
    assert_eq!(s.read_iram(0x10), 0x99);
}

#[test]
fn bwram_protection_blocks_write() {
    // bsnes gate: a write is dropped only when SBWE=0 AND CBWE=0 AND the
    // address is inside the BWPA protected area (offset < 0x100 << bwpa).
    let mut s = sa1();
    // Default bwpa=0 -> protected area is offsets 0x00..0xFF. Both enables off.
    // Inside the protected area -> dropped.
    s.write_bwram_scpu(0x00, 0x77);
    assert_eq!(s.read_bwram(0x00), 0x00, "protected low byte, both enables off");
    // Outside the protected area (offset 0x100) -> stored even with enables off.
    s.write_bwram_scpu(0x100, 0x55);
    assert_eq!(s.read_bwram(0x100), 0x55, "outside protected area stores anyway");
    // Enabling SBWE permits the protected-area write too.
    s.write_io(NO_ROM, 0x2226, 0x80); // SBWE enable
    s.write_bwram_scpu(0x00, 0x99);
    assert_eq!(s.read_bwram(0x00), 0x99, "SBWE enable lifts protection");
}

#[test]
fn normal_dma_rom_to_iram() {
    let mut rom = vec![0u8; 0x10000];
    for (i, b) in rom.iter_mut().enumerate().take(8) {
        *b = 0xF0 | i as u8;
    }
    let mut s = sa1();
    s.write_io(&rom, 0x222A, 0xFF); // CIWP: allow I-RAM writes
    // DCNT: enable (bit7), normal mode, source ROM (SS=00).
    s.write_io(&rom, 0x2230, 0x80);
    // Source bus address $C0:0000 (HiROM region, block 0 -> ROM offset 0).
    s.write_io(&rom, 0x2232, 0x00);
    s.write_io(&rom, 0x2233, 0x00);
    s.write_io(&rom, 0x2234, 0xC0);
    // Count = 4.
    s.write_io(&rom, 0x2238, 0x04);
    s.write_io(&rom, 0x2239, 0x00);
    // Dest I-RAM offset $20; write $2235 then trigger on $2236.
    s.write_io(&rom, 0x2235, 0x20);
    s.write_io(&rom, 0x2236, 0x00);
    assert_eq!(s.read_iram(0x20), 0xF0);
    assert_eq!(s.read_iram(0x23), 0xF3);
}

#[test]
fn save_state_roundtrip() {
    let mut s = sa1();
    s.write_io(NO_ROM, 0x2200, 0x07);
    s.write_io(NO_ROM, 0x2226, 0x80);
    s.write_bwram_scpu(0x50, 0xAB);
    let bytes = bincode::serialize(&s).unwrap();
    let mut back: Sa1 = bincode::deserialize(&bytes).unwrap();
    assert_eq!(back.read_io(NO_ROM, 0x2301) & 0x0F, 0x07);
    assert_eq!(back.read_bwram(0x50), 0xAB);
}

fn sa1_irq_line(s: &Sa1) -> bool {
    s.st.cfr_irq && s.st.cie & 0x80 != 0
}
