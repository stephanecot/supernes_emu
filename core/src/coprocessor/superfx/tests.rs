//! GSU instruction-set unit tests with hand-computed vectors from superfx.md.

use super::gsu::{SuperFx, VCR_GSU2};

/// Assemble `code` at ROM offset 0 (bank $00, R15=$0000), grant ROM+RAM to the
/// GSU, start it and run to STOP. Prefix bytes used: FROM=$Bn, TO=$1n.
fn run(setup: impl FnOnce(&mut SuperFx), code: &[u8]) -> SuperFx {
    let mut rom = vec![0u8; 0x400];
    rom[..code.len()].copy_from_slice(code);
    let mut fx = SuperFx::new(0x8000, VCR_GSU2);
    fx.scmr = 0x18; // RON | RAN: GSU owns ROM+RAM
    setup(&mut fx);
    fx.r[15] = 0;
    fx.pbr = 0;
    fx.go = true;
    fx.primed = false;
    fx.run(&rom, 100_000);
    fx
}

const FROM: u8 = 0xB0;
const TO: u8 = 0x10;
const STOP: u8 = 0x00;
const ALT1: u8 = 0x3D;
const ALT2: u8 = 0x3E;
const ALT3: u8 = 0x3F;

#[test]
fn add_registers() {
    // R3 = R1 + R2, 5 + 3 = 8.
    let fx = run(
        |fx| {
            fx.r[1] = 5;
            fx.r[2] = 3;
        },
        &[FROM | 1, TO | 3, 0x50 | 2, STOP],
    );
    assert_eq!(fx.r[3], 8);
    assert!(!fx.z && !fx.cy && !fx.s && !fx.ov);
}

#[test]
fn add_sets_carry_and_overflow() {
    // R3 = R1 + R2 with 0x8000 + 0x8000 = 0x0000, carry + overflow.
    let fx = run(
        |fx| {
            fx.r[1] = 0x8000;
            fx.r[2] = 0x8000;
        },
        &[FROM | 1, TO | 3, 0x50 | 2, STOP],
    );
    assert_eq!(fx.r[3], 0);
    assert!(fx.z && fx.cy && fx.ov && !fx.s);
}

#[test]
fn adc_uses_carry_in() {
    // Set CY via first ADD (0xFFFF+1 -> carry), then ADC.
    let fx = run(
        |fx| {
            fx.r[1] = 0xFFFF;
            fx.r[2] = 1;
            fx.r[5] = 0x0010;
        },
        &[
            FROM | 1, TO | 3, 0x50 | 2, // R3 = FFFF+1 = 0, CY=1
            FROM | 5, TO | 6, ALT1, 0x50 | 2, // R6 = R5 + R2 + CY = 0x10+1+1
            STOP,
        ],
    );
    assert_eq!(fx.r[6], 0x0012);
}

#[test]
fn sub_and_cmp() {
    // SUB: R3 = R1 - R2 = 10 - 3 = 7, no borrow -> CY=1.
    let fx = run(
        |fx| {
            fx.r[1] = 10;
            fx.r[2] = 3;
        },
        &[FROM | 1, TO | 3, 0x60 | 2, STOP],
    );
    assert_eq!(fx.r[3], 7);
    assert!(fx.cy && !fx.z);

    // CMP (ALT3 of SUB) sets flags only; R3 unchanged.
    let fx = run(
        |fx| {
            fx.r[1] = 3;
            fx.r[2] = 10;
            fx.r[3] = 0xDEAD;
        },
        &[FROM | 1, TO | 3, ALT3, 0x60 | 2, STOP],
    );
    assert_eq!(fx.r[3], 0xDEAD); // untouched
    assert!(!fx.cy); // borrow occurred
}

#[test]
fn add_immediate_alt2() {
    // ADD #n (ALT2): R3 = R1 + 4.
    let fx = run(
        |fx| fx.r[1] = 0x20,
        &[FROM | 1, TO | 3, ALT2, 0x50 | 4, STOP],
    );
    assert_eq!(fx.r[3], 0x24);
}

#[test]
fn and_or_xor() {
    let fx = run(
        |fx| {
            fx.r[1] = 0xF0F0;
            fx.r[2] = 0x0FF0;
        },
        &[FROM | 1, TO | 3, 0x70 | 2, STOP], // AND
    );
    assert_eq!(fx.r[3], 0x00F0);

    let fx = run(
        |fx| {
            fx.r[1] = 0xF000;
            fx.r[2] = 0x000F;
        },
        &[FROM | 1, TO | 3, 0xC0 | 2, STOP], // OR
    );
    assert_eq!(fx.r[3], 0xF00F);

    let fx = run(
        |fx| {
            fx.r[1] = 0xFF00;
            fx.r[2] = 0x0FF0;
        },
        &[FROM | 1, TO | 3, ALT1, 0xC0 | 2, STOP], // XOR (ALT1 of OR)
    );
    assert_eq!(fx.r[3], 0xF0F0);
}

#[test]
fn shifts() {
    // LSR: 3 >> 1 = 1, CY=1.
    let fx = run(|fx| fx.r[1] = 3, &[FROM | 1, TO | 3, 0x03, STOP]);
    assert_eq!(fx.r[3], 1);
    assert!(fx.cy && !fx.s);

    // ASR: 0xFFFE (-2) >> 1 = 0xFFFF (-1).
    let fx = run(|fx| fx.r[1] = 0xFFFE, &[FROM | 1, TO | 3, 0x96, STOP]);
    assert_eq!(fx.r[3], 0xFFFF);
    assert!(fx.s);

    // ROR: with CY=0 in, 1 >> 1 = 0, CY out = 1.
    let fx = run(|fx| fx.r[1] = 1, &[FROM | 1, TO | 3, 0x97, STOP]);
    assert_eq!(fx.r[3], 0);
    assert!(fx.cy && fx.z);
}

#[test]
fn mult_signed_and_unsigned() {
    // MULT (signed): (int8)0xFF * (int8)0x02 = -1 * 2 = -2 = 0xFFFE.
    let fx = run(
        |fx| {
            fx.r[1] = 0x00FF;
            fx.r[2] = 0x0002;
        },
        &[FROM | 1, TO | 3, 0x80 | 2, STOP],
    );
    assert_eq!(fx.r[3], 0xFFFE);

    // UMULT (ALT1): 0xFF * 2 = 510 = 0x01FE.
    let fx = run(
        |fx| {
            fx.r[1] = 0x00FF;
            fx.r[2] = 0x0002;
        },
        &[FROM | 1, TO | 3, ALT1, 0x80 | 2, STOP],
    );
    assert_eq!(fx.r[3], 0x01FE);
}

#[test]
fn fmult_and_lmult() {
    // FMULT: 0x7FFF * 0x7FFF = 0x3FFF0001; high word 0x3FFF into Rd.
    let fx = run(
        |fx| {
            fx.r[1] = 0x7FFF;
            fx.r[6] = 0x7FFF;
        },
        &[FROM | 1, TO | 3, 0x9F, STOP],
    );
    assert_eq!(fx.r[3], 0x3FFF);
    assert!(!fx.z);

    // LMULT (ALT1): R4 = low word 0x0001, Rd = 0x3FFF.
    let fx = run(
        |fx| {
            fx.r[1] = 0x7FFF;
            fx.r[6] = 0x7FFF;
        },
        &[FROM | 1, TO | 3, ALT1, 0x9F, STOP],
    );
    assert_eq!(fx.r[3], 0x3FFF);
    assert_eq!(fx.r[4], 0x0001);
}

#[test]
fn merge_op() {
    // MERGE: (R7 & FF00) | (R8 >> 8) = 0x1200 | 0x00AB = 0x12AB.
    let fx = run(
        |fx| {
            fx.r[7] = 0x1234;
            fx.r[8] = 0xABCD;
        },
        &[TO | 3, 0x70, STOP],
    );
    assert_eq!(fx.r[3], 0x12AB);
}

#[test]
fn immediates_ibt_iwt() {
    // IBT R3,#$FE -> sign-extend to 0xFFFE.
    let fx = run(|_| {}, &[0xA0 | 3, 0xFE, STOP]);
    assert_eq!(fx.r[3], 0xFFFE);

    // IWT R4,#$1234 (lo,hi).
    let fx = run(|_| {}, &[0xF0 | 4, 0x34, 0x12, STOP]);
    assert_eq!(fx.r[4], 0x1234);
}

#[test]
fn move_via_with_prefix() {
    // WITH R1 (sets Sreg=Dreg=1, B=1); TO R3 executes as MOVE R3,R1.
    let fx = run(
        |fx| {
            fx.r[1] = 0xCAFE;
            fx.r[3] = 0;
        },
        &[0x20 | 1, TO | 3, STOP],
    );
    assert_eq!(fx.r[3], 0xCAFE);
}

#[test]
fn branch_taken_and_delay_slot() {
    // BRA over a byte: delay slot (INC R5) executes, then target sets R6.
    // Layout: [00]=BRA, [01]=disp, [02]=INC R5 (delay slot), [03]=NOP,...
    // Target = R15 + disp, with R15 = 3 after fetching disp. Set target to
    // an IWT that loads R6.
    // code: BRA(05) disp INC_R5(D5) IWT_R6 #$0001 ... target
    // disp chosen so target skips the IWT-at-4 and lands on IWT-at-7.
    // Simpler: verify the delay slot runs and a conditional is honored.
    let fx = run(
        |fx| {
            fx.r[5] = 0;
        },
        &[
            0x05, 0x01, // BRA +1 (target = 3+1 = 4)
            0xD0 | 5, // delay slot: INC R5
            STOP,     // addr 3 (skipped by branch target 4)
            STOP,     // addr 4 (branch target)
        ],
    );
    assert_eq!(fx.r[5], 1); // delay slot executed
}

#[test]
fn beq_conditional() {
    // Prove the taken path diverges from fall-through. AND sets Z=1 so BEQ is
    // taken; the delay slot (byte after the branch) is a NOP so it clobbers no
    // flag/register, and INC R5 sits only at the branch target. A taken branch
    // reaches INC R5 (R5=1); the fall-through would hit STOP first (R5=0), so
    // R5==1 alone proves the branch was taken. INC then clears Z (R5=1).
    let fx = run(
        |fx| {
            fx.r[1] = 0;
            fx.r[5] = 0;
        },
        &[
            FROM | 1, TO | 2, 0x70 | 1, // idx0-2: R2 = R1 & R1 = 0 -> Z=1
            0x09, 0x02, // idx3-4: BEQ +2
            0x01,       // idx5: delay slot NOP (always executed)
            STOP,       // idx6: fall-through target (not-taken lands here)
            STOP,       // idx7: filler
            0xD0 | 5,   // idx8: INC R5 (taken target)
            STOP,       // idx9
        ],
    );
    assert_eq!(fx.r[5], 1); // branch was taken (delay slot did not touch R5)
    assert!(!fx.z); // INC R5 (R5 1) cleared Z
}

#[test]
fn stop_sets_irq_and_clears_go() {
    let fx = run(|_| {}, &[STOP]);
    assert!(!fx.is_running());
    assert!(fx.irq_line());
}

#[test]
fn mmio_register_latch() {
    let mut fx = SuperFx::new(0x8000, VCR_GSU2);
    fx.write_mmio(0x3000, 0x34); // R0 low latch
    fx.write_mmio(0x3001, 0x12); // R0 commit
    assert_eq!(fx.r[0], 0x1234);
    assert_eq!(fx.read_mmio(0x3000), 0x34);
    assert_eq!(fx.read_mmio(0x3001), 0x12);
}

#[test]
fn mmio_go_start_runs_program() {
    // Program at ROM $0000: IWT R3,#$00AA ; STOP.
    let mut rom = vec![0u8; 0x400];
    rom[0] = 0xF0 | 3;
    rom[1] = 0xAA;
    rom[2] = 0x00;
    rom[3] = STOP;
    let mut fx = SuperFx::new(0x8000, VCR_GSU2);
    fx.scmr = 0x18;
    // Set R15 = $0000 via MMIO; writing $301F sets GO.
    fx.write_mmio(0x301E, 0x00);
    fx.write_mmio(0x301F, 0x00);
    assert!(fx.is_running());
    fx.run(&rom, 100_000);
    assert_eq!(fx.r[3], 0x00AA);
    assert!(!fx.is_running());
}

#[test]
fn sfr_read_clears_irq() {
    let fx_stop = run(|_| {}, &[STOP]);
    let mut fx = fx_stop;
    assert!(fx.irq);
    let _ = fx.read_mmio(0x3030); // low byte read: IRQ retained
    assert!(fx.irq);
    let _ = fx.read_mmio(0x3031); // high byte read: IRQ cleared
    assert!(!fx.irq);
}

#[test]
fn sfr_write_go0_resets_cache_and_cbr() {
    let mut fx = SuperFx::new(0x8000, VCR_GSU2);
    fx.cbr = 0x1230;
    fx.cache_valid = [true; 32];
    fx.go = true;
    fx.write_mmio(0x3030, 0x00); // GO=0 abort
    assert!(!fx.go);
    assert_eq!(fx.cbr, 0);
    assert!(fx.cache_valid.iter().all(|&v| !v));
}

// ---- ROM/RAM read-ahead buffer (superfx.md §7; bsnes timing.cpp) --------

#[test]
fn getb_reads_byte_at_rombr_r14() {
    // Baseline: with no intervening ROMB, GETB just returns the byte at
    // [ROMBR:R14] (ROMBR=0 by default) once the read-ahead has latched.
    let fx = run(
        |_| {},
        &[
            0xFE, 0x10, 0x00, // IWT R14,#$0010 -> arms the read-ahead
            TO | 3, 0xEF,     // GETB: R3 = romdr
            STOP,
        ],
    );
    // rom[..code.len()] holds the program; offset $10 lands just past it and
    // defaults to 0x00 (rom is zero-filled), so GETB must return 0.
    assert_eq!(fx.r[3], 0x00);
}

#[test]
fn romb_before_readahead_expiry_changes_captured_bank() {
    // Core correctness property of the read-ahead buffer (superfx.md §7;
    // bsnes `SuperFX::readROMBuffer`/`instructionGETC_RAMB_ROMB` both call
    // `syncROMBuffer()` before touching ROMBR): GETB never reads the bus
    // live, it returns `romdr`, latched when the pending read (armed by the
    // preceding IWT R14) completes. ROMB executed *before* that completion
    // forces the read to finish under the OLD ROMBR first, so the new bank
    // ROMB sets has NO effect on this GETB's result even though ROMBR now
    // reads back as the new value.
    // Marker offset $0100 sits safely past the short program below (which
    // must NOT overlap it, or the program bytes themselves clobber the
    // marker read by GETB).
    let mut rom = vec![0u8; 0x8200];
    rom[0x0100] = 0xAA; // bank 0 byte at offset $0100
    rom[0x8100] = 0xBB; // bank 1 byte at the same in-bank offset
    let code: &[u8] = &[
        0xFE, 0x00, 0x01, // IWT R14,#$0100 -> arms the read-ahead (ROMBR=0)
        FROM | 1,         // FROM R1 (Sreg=R1=1)
        ALT3,             // ALT1+ALT2
        0xDF,             // ROMB: ROMBR=R1=1 (syncs the read under ROMBR=0 FIRST)
        TO | 3,           // TO R3
        0xEF,             // GETB: R3 = romdr
        STOP,
    ];
    rom[..code.len()].copy_from_slice(code);
    let mut fx = SuperFx::new(0x8000, VCR_GSU2);
    fx.scmr = 0x18;
    fx.r[1] = 1;
    fx.r[15] = 0;
    fx.pbr = 0;
    fx.go = true;
    fx.primed = false;
    fx.run(&rom, 100_000);
    assert_eq!(
        fx.r[3], 0xAA,
        "GETB must return the byte read under the OLD ROMBR (0), not the new one (1)"
    );
    assert_eq!(fx.rombr, 1, "ROMB still takes effect for later reads");
}

#[test]
fn getb_returns_latched_byte_not_a_live_reread_after_natural_expiry() {
    // Same property as above, exercised via the *natural* countdown decay
    // (several NOPs let `romcl` reach 0 on its own) instead of a forced
    // sync: a ROMB issued *after* the read has already latched must not
    // retroactively change an already-captured byte, because GETB always
    // returns the cached `romdr`, never a live [ROMBR:R14] read.
    let mut rom = vec![0u8; 0x8200];
    rom[0x0100] = 0xAA; // bank 0
    rom[0x8100] = 0xBB; // bank 1
    let code: &[u8] = &[
        0xFE, 0x00, 0x01, // IWT R14,#$0100 -> arms the read-ahead (latency 6)
        0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, // 8x NOP: drains romcl to 0
        FROM | 1, ALT3, 0xDF, // ROMB: ROMBR=1, AFTER the read already latched
        TO | 3, 0xEF, // GETB: R3 = romdr (still the pre-ROMB value)
        STOP,
    ];
    rom[..code.len()].copy_from_slice(code);
    let mut fx = SuperFx::new(0x8000, VCR_GSU2);
    fx.scmr = 0x18;
    fx.r[1] = 1;
    fx.r[15] = 0;
    fx.pbr = 0;
    fx.go = true;
    fx.primed = false;
    fx.run(&rom, 100_000);
    assert_eq!(fx.r[3], 0xAA, "already-latched byte must not change retroactively");
    assert_eq!(fx.rombr, 1);
}

#[test]
fn ram_write_queue_round_trip_via_stw_ldw() {
    // STW queues the write into the RAM buffer (superfx.md §7); LDW must
    // sync (drain) the pending queue before reading, so a read immediately
    // following a write to the same address sees the written value.
    let fx = run(
        |fx| {
            fx.r[1] = 0x1234; // value
            fx.r[2] = 0x0010; // address
        },
        &[
            FROM | 1, 0x30 | 2, // STW (R2): word[R2] = R1
            TO | 4, 0x40 | 2,   // LDW (R2): R4 = word[R2]
            STOP,
        ],
    );
    assert_eq!(fx.r[4], 0x1234);
}

#[test]
fn ramb_before_write_drain_lands_in_old_bank() {
    // RAM analog of the ROMB test: RAMB syncs (drains) the pending queued
    // write BEFORE changing RAMBR (bsnes `syncRAMBuffer()` before assigning),
    // so a write queued while RAMBR=0 lands in bank 0's RAM even though RAMB
    // switches to bank 1 before the write's own latency would have drained
    // it naturally.
    let mut rom = vec![0u8; 0x400];
    let code: &[u8] = &[
        0xF0 | 1, 0x34, 0x12, // IWT R1,#$1234 (value)
        0xF0 | 2, 0x10, 0x00, // IWT R2,#$0010 (address)
        FROM | 1, 0x30 | 2,   // STW (R2): queues word[$0010]=$1234 under RAMBR=0
        FROM | 3, ALT2, 0xDF, // RAMB: RAMBR=R3=1 (syncs the queued write to OLD bank 0 first)
        STOP,
    ];
    rom[..code.len()].copy_from_slice(code);
    // 128 KB RAM so bank 0 ($700000+) and bank 1 ($710000+) don't alias
    // through the emulator's `% ram.len()` mirroring (a real cart's RAM is
    // usually <=64 KB and would alias in practice too, but that would defeat
    // this specific regression check).
    let mut fx = SuperFx::new(0x20000, VCR_GSU2);
    fx.scmr = 0x18;
    fx.r[3] = 1;
    fx.r[15] = 0;
    fx.pbr = 0;
    fx.go = true;
    fx.primed = false;
    fx.run(&rom, 100_000);
    assert_eq!(fx.rambr, 1);
    assert_eq!(fx.ram[0x0010], 0x34, "bank 0 got the drained write");
    assert_eq!(fx.ram[0x0011], 0x12);
    assert_eq!(fx.ram[0x10010], 0x00, "bank 1 must be untouched");
    assert_eq!(fx.ram[0x10011], 0x00);
}
