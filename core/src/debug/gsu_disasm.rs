//! GSU (SuperFX) disassembler. Opcode decode transcribed 1:1 from the
//! `execute_one` match in `coprocessor/superfx/gsu.rs` (itself sourced from
//! `.claude/skills/snes-refs/references/superfx.md` §9), so any mnemonic here
//! reflects exactly what the core executes for that opcode byte under a given
//! ALT1/ALT2 state.
//!
//! Prefix model (superfx.md §8): ALT1/ALT2 (and ALT3 = both) select the
//! variant of the following non-prefix opcode; `alt1`/`alt2` here are the
//! *incoming* prefix state (as if already applied by a preceding ALT1/ALT2/
//! ALT3 byte), matching how `execute_one` reads `self.alt1`/`self.alt2` when
//! decoding the opcode at `addr`. Prefix bytes themselves (`3D`/`3E`/`3F`,
//! `10-1F` TO, `20-2F` WITH, `B0-BF` FROM) are each disassembled as their own
//! one-byte instruction (they are separate pipeline fetch-execute steps on
//! real hardware, per `gsu.rs`'s prefix-opcode arms returning early), so a
//! caller stepping instruction-by-instruction naturally sees them as distinct
//! trace lines.

/// Register operand text ("R0".."R15"); GSU register numbers are conventionally
/// written decimal (fullsnes/nesdev usage), not hex.
fn reg(n: u8) -> String {
    format!("R{n}")
}

/// Fetch one instruction-stream byte at `PBR:(r15+off)`. R15 is a 16-bit GSU
/// register: address wraps modulo $10000 within the fixed program bank (PBR
/// does not change mid-instruction), mirroring the 65816 disassembler's
/// K:PC-wrap rule.
fn fetch_byte(fetch: &mut dyn FnMut(u32) -> u8, bank: u32, pc16: u32, off: u32) -> u8 {
    fetch(bank | (pc16.wrapping_add(off) & 0xFFFF))
}

/// Disassemble one GSU instruction. `fetch` reads program-stream bytes at
/// 24-bit PBR:R15 addresses; `alt1`/`alt2` are the live SFR.ALT1/ALT2 state
/// (ALT3 = both true) that a preceding prefix byte left in effect. Returns
/// (text, instruction length in bytes).
///
/// This does not know the SFR.B (WITH) state or the current Sreg, so it always
/// shows the `1n`/`Bn` opcode range as its prefix reading (`TO Rn`/`FROM Rn`)
/// rather than the `MOVE`/`MOVES` reading a WITH prefix would select; use
/// [`disassemble_one_ex`] (which takes `b`/`sreg`) for a fully accurate line,
/// e.g. from live SuperFx state in the trace formatter.
pub fn disassemble_one(
    fetch: &mut dyn FnMut(u32) -> u8,
    addr: u32,
    alt1: bool,
    alt2: bool,
) -> (String, u8) {
    disassemble_one_ex(fetch, addr, alt1, alt2, false, 0)
}

/// Full-context GSU disassembly: `b` (SFR.B, set by a preceding WITH Rn) and
/// `sreg` (the register WITH/FROM last selected) resolve `1n`/`Bn` as
/// `MOVE Rd,Rs` / `MOVES Rd,Rs` instead of `TO Rn` / `FROM Rn` (superfx.md
/// §8, `gsu.rs` 0x10-0x1F / 0xB0-0xBF arms).
pub fn disassemble_one_ex(
    fetch: &mut dyn FnMut(u32) -> u8,
    addr: u32,
    alt1: bool,
    alt2: bool,
    b: bool,
    sreg: u8,
) -> (String, u8) {
    let bank = addr & 0xFF0000;
    let pc16 = addr & 0xFFFF;
    let op = fetch_byte(fetch, bank, pc16, 0);
    let n = op & 0x0F;

    match op {
        0x00 => ("STOP".to_string(), 1),
        0x01 => ("NOP".to_string(), 1),
        0x02 => ("CACHE".to_string(), 1),
        0x03 => ("LSR".to_string(), 1),
        0x04 => ("ROL".to_string(), 1),

        // Branches (rel8, prefixes preserved by the caller's state tracking).
        // Target = R15 + disp with R15 sampled *after* the disp-byte fetch
        // also prefetches the delay-slot byte (gsu.rs `branch`: fetching disp
        // advances R15 one further, to addr+3, not addr+2 — the pipeline
        // "byte after the branch executes before the target" behavior,
        // superfx.md §9 branch table note). Verified against
        // `coprocessor/superfx/tests.rs::branch_taken_and_delay_slot`.
        0x05..=0x0F => {
            let disp = fetch_byte(fetch, bank, pc16, 1) as i8;
            let target = pc16.wrapping_add(3).wrapping_add(disp as i32 as u32) & 0xFFFF;
            let mnem = match op {
                0x05 => "BRA",
                0x06 => "BGE",
                0x07 => "BLT",
                0x08 => "BNE",
                0x09 => "BEQ",
                0x0A => "BPL",
                0x0B => "BMI",
                0x0C => "BCC",
                0x0D => "BCS",
                0x0E => "BVC",
                _ => "BVS",
            };
            (format!("{mnem} ${target:04X}"), 2)
        }

        0x10..=0x1F => {
            if b {
                (format!("MOVE {},{}", reg(n), reg(sreg)), 1)
            } else {
                (format!("TO {}", reg(n)), 1)
            }
        }
        0x20..=0x2F => (format!("WITH {}", reg(n)), 1),

        0x30..=0x3B => {
            let mnem = if alt1 { "STB" } else { "STW" };
            (format!("{mnem} ({})", reg(n)), 1)
        }
        0x3C => ("LOOP".to_string(), 1),
        0x3D => ("ALT1".to_string(), 1),
        0x3E => ("ALT2".to_string(), 1),
        0x3F => ("ALT3".to_string(), 1),

        0x40..=0x4B => {
            let mnem = if alt1 { "LDB" } else { "LDW" };
            (format!("{mnem} ({})", reg(n)), 1)
        }
        0x4C => ((if alt1 { "RPIX" } else { "PLOT" }).to_string(), 1),
        0x4D => ("SWAP".to_string(), 1),
        0x4E => ((if alt1 { "CMODE" } else { "COLOR" }).to_string(), 1),
        0x4F => ("NOT".to_string(), 1),

        0x50..=0x5F => {
            let text = match (alt1, alt2) {
                (false, false) => format!("ADD {}", reg(n)),
                (true, false) => format!("ADC {}", reg(n)),
                (false, true) => format!("ADD #${n:X}"),
                (true, true) => format!("ADC #${n:X}"),
            };
            (text, 1)
        }
        0x60..=0x6F => {
            // ALT3 (both set) is CMP Rn (register operand, flags-only), not an
            // immediate form (gsu.rs 0x60-0x6F: (true,true) uses r[n]).
            let text = match (alt1, alt2) {
                (false, false) => format!("SUB {}", reg(n)),
                (true, false) => format!("SBC {}", reg(n)),
                (false, true) => format!("SUB #${n:X}"),
                (true, true) => format!("CMP {}", reg(n)),
            };
            (text, 1)
        }

        0x70 => ("MERGE".to_string(), 1),
        0x71..=0x7F => {
            let text = match (alt1, alt2) {
                (false, false) => format!("AND {}", reg(n)),
                (true, false) => format!("BIC {}", reg(n)),
                (false, true) => format!("AND #${n:X}"),
                (true, true) => format!("BIC #${n:X}"),
            };
            (text, 1)
        }

        0x80..=0x8F => {
            let text = match (alt1, alt2) {
                (false, false) => format!("MULT {}", reg(n)),
                (true, false) => format!("UMULT {}", reg(n)),
                (false, true) => format!("MULT #${n:X}"),
                (true, true) => format!("UMULT #${n:X}"),
            };
            (text, 1)
        }

        0x90 => ("SBK".to_string(), 1),
        0x91..=0x94 => (format!("LINK #{n}"), 1),
        0x95 => ("SEX".to_string(), 1),
        0x96 => ((if alt1 { "DIV2" } else { "ASR" }).to_string(), 1),
        0x97 => ("ROR".to_string(), 1),
        0x98..=0x9D => {
            let mnem = if alt1 { "LJMP" } else { "JMP" };
            (format!("{mnem} {}", reg(n)), 1)
        }
        0x9E => ("LOB".to_string(), 1),
        0x9F => ((if alt1 { "LMULT" } else { "FMULT" }).to_string(), 1),

        0xA0..=0xAF => {
            if alt1 {
                // LMS Rn,(kk): kk is the raw immediate byte (word addr = kk*2).
                let kk = fetch_byte(fetch, bank, pc16, 1);
                (format!("LMS {},(${kk:02X})", reg(n)), 2)
            } else if alt2 {
                let kk = fetch_byte(fetch, bank, pc16, 1);
                (format!("SMS (${kk:02X}),{}", reg(n)), 2)
            } else {
                let pp = fetch_byte(fetch, bank, pc16, 1);
                (format!("IBT {},#${pp:02X}", reg(n)), 2)
            }
        }

        0xC0 => ("HIB".to_string(), 1),
        0xC1..=0xCF => {
            let text = match (alt1, alt2) {
                (false, false) => format!("OR {}", reg(n)),
                (true, false) => format!("XOR {}", reg(n)),
                (false, true) => format!("OR #${n:X}"),
                (true, true) => format!("XOR #${n:X}"),
            };
            (text, 1)
        }

        0xD0..=0xDE => (format!("INC {}", reg(n)), 1),
        0xDF => {
            // ALT1-alone is undefined for DF; falls back to the plain GETC
            // reading (superfx.md §8 "ignored prefixes"), matching gsu.rs's
            // `if alt2 && !alt1 {RAMB} else if alt1 && alt2 {ROMB} else {GETC}`.
            let text = if alt2 && !alt1 {
                "RAMB"
            } else if alt1 && alt2 {
                "ROMB"
            } else {
                "GETC"
            };
            (text.to_string(), 1)
        }

        0xE0..=0xEE => (format!("DEC {}", reg(n)), 1),
        0xEF => {
            let text = match (alt1, alt2) {
                (false, false) => "GETB",
                (true, false) => "GETBH",
                (false, true) => "GETBL",
                (true, true) => "GETBS",
            };
            (text.to_string(), 1)
        }

        0xF0..=0xFF => {
            if alt1 {
                let lo = fetch_byte(fetch, bank, pc16, 1) as u16;
                let hi = fetch_byte(fetch, bank, pc16, 2) as u16;
                (format!("LM {},(${:04X})", reg(n), lo | (hi << 8)), 3)
            } else if alt2 {
                let lo = fetch_byte(fetch, bank, pc16, 1) as u16;
                let hi = fetch_byte(fetch, bank, pc16, 2) as u16;
                (format!("SM (${:04X}),{}", lo | (hi << 8), reg(n)), 3)
            } else {
                let lo = fetch_byte(fetch, bank, pc16, 1) as u16;
                let hi = fetch_byte(fetch, bank, pc16, 2) as u16;
                (format!("IWT {},#${:04X}", reg(n), lo | (hi << 8)), 3)
            }
        }

        0xB0..=0xBF => {
            if b {
                (format!("MOVES {},{}", reg(n), reg(sreg)), 1)
            } else {
                (format!("FROM {}", reg(n)), 1)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Disassembles `bytes` placed at 24-bit `addr`.
    fn disasm_bytes(bytes: &[u8], addr: u32, alt1: bool, alt2: bool) -> (String, u8) {
        let mut fetch = |a: u32| -> u8 {
            let off = (a.wrapping_sub(addr) & 0xFFFF) as usize;
            bytes[off]
        };
        disassemble_one(&mut fetch, addr, alt1, alt2)
    }

    #[test]
    fn stop_and_nop() {
        assert_eq!(disasm_bytes(&[0x00], 0, false, false), ("STOP".to_string(), 1));
        assert_eq!(disasm_bytes(&[0x01], 0, false, false), ("NOP".to_string(), 1));
    }

    #[test]
    fn to_from_with_prefixes() {
        assert_eq!(disasm_bytes(&[0x13], 0, false, false), ("TO R3".to_string(), 1));
        assert_eq!(disasm_bytes(&[0x25], 0, false, false), ("WITH R5".to_string(), 1));
        assert_eq!(disasm_bytes(&[0xB7], 0, false, false), ("FROM R7".to_string(), 1));
    }

    #[test]
    fn add_register_no_prefix() {
        // 0x52 = ADD R2 (base form, no ALT).
        let (text, len) = disasm_bytes(&[0x52], 0, false, false);
        assert_eq!(text, "ADD R2");
        assert_eq!(len, 1);
    }

    #[test]
    fn adc_is_alt1_of_add() {
        // Same byte (0x52) under ALT1 decodes as ADC R2 (gsu.rs 0x50-0x5F: alt1=true,alt2=false -> ADC).
        let (text, len) = disasm_bytes(&[0x52], 0, true, false);
        assert_eq!(text, "ADC R2");
        assert_eq!(len, 1);
    }

    #[test]
    fn add_immediate_is_alt2() {
        let (text, _len) = disasm_bytes(&[0x5A], 0, false, true);
        assert_eq!(text, "ADD #$A");
    }

    #[test]
    fn cmp_is_alt3_of_sub() {
        // 0x63 under ALT3 (alt1=alt2=true) -> CMP R3 (register operand, not #n).
        let (text, _len) = disasm_bytes(&[0x63], 0, true, true);
        assert_eq!(text, "CMP R3");
    }

    #[test]
    fn stb_alt1_of_stw() {
        let (text, len) = disasm_bytes(&[0x32], 0, true, false);
        assert_eq!(text, "STB (R2)");
        assert_eq!(len, 1);
    }

    #[test]
    fn stw_base_form() {
        let (text, len) = disasm_bytes(&[0x32], 0, false, false);
        assert_eq!(text, "STW (R2)");
        assert_eq!(len, 1);
    }

    #[test]
    fn ibt_immediate() {
        // A3 FE = IBT R3,#$FE, 2 bytes.
        let (text, len) = disasm_bytes(&[0xA3, 0xFE], 0, false, false);
        assert_eq!(text, "IBT R3,#$FE");
        assert_eq!(len, 2);
    }

    #[test]
    fn lms_is_alt1_of_ibt_row() {
        // ALT1 A3 12 = LMS R3,($12).
        let (text, len) = disasm_bytes(&[0xA3, 0x12], 0, true, false);
        assert_eq!(text, "LMS R3,($12)");
        assert_eq!(len, 2);
    }

    #[test]
    fn iwt_immediate_16bit() {
        // F4 34 12 = IWT R4,#$1234 (lo, hi).
        let (text, len) = disasm_bytes(&[0xF4, 0x34, 0x12], 0, false, false);
        assert_eq!(text, "IWT R4,#$1234");
        assert_eq!(len, 3);
    }

    #[test]
    fn lm_is_alt1_of_iwt_row() {
        let (text, len) = disasm_bytes(&[0xF4, 0x34, 0x12], 0, true, false);
        assert_eq!(text, "LM R4,($1234)");
        assert_eq!(len, 3);
    }

    #[test]
    fn branch_target_resolved() {
        // BRA +1 at addr 0: target = 0+3+1 = 4 (pipeline prefetches the
        // delay-slot byte before the target is computed; not a plain +2).
        // Matches coprocessor/superfx/tests.rs::branch_taken_and_delay_slot.
        let (text, len) = disasm_bytes(&[0x05, 0x01], 0, false, false);
        assert_eq!(text, "BRA $0004");
        assert_eq!(len, 2);
    }

    #[test]
    fn beq_mnemonic() {
        let (text, _len) = disasm_bytes(&[0x09, 0x00], 0, false, false);
        assert_eq!(text, "BEQ $0003");
    }

    #[test]
    fn jmp_and_ljmp() {
        assert_eq!(disasm_bytes(&[0x9A], 0, false, false), ("JMP R10".to_string(), 1));
        assert_eq!(disasm_bytes(&[0x9A], 0, true, false), ("LJMP R10".to_string(), 1));
    }

    #[test]
    fn getb_family() {
        assert_eq!(disasm_bytes(&[0xEF], 0, false, false), ("GETB".to_string(), 1));
        assert_eq!(disasm_bytes(&[0xEF], 0, true, false), ("GETBH".to_string(), 1));
        assert_eq!(disasm_bytes(&[0xEF], 0, false, true), ("GETBL".to_string(), 1));
        assert_eq!(disasm_bytes(&[0xEF], 0, true, true), ("GETBS".to_string(), 1));
    }

    #[test]
    fn df_alt1_alone_falls_back_to_getc() {
        // Undefined ALT1-alone form of DF: falls back to plain GETC, matching
        // gsu.rs's ignored-prefix behavior (superfx.md §8).
        assert_eq!(disasm_bytes(&[0xDF], 0, true, false), ("GETC".to_string(), 1));
        assert_eq!(disasm_bytes(&[0xDF], 0, false, true), ("RAMB".to_string(), 1));
        assert_eq!(disasm_bytes(&[0xDF], 0, true, true), ("ROMB".to_string(), 1));
    }

    #[test]
    fn move_via_with_uses_b_and_sreg() {
        // The public 2-flag disassemble_one always shows TO Rn (no B/Sreg context).
        assert_eq!(disasm_bytes(&[0x13], 0, false, false), ("TO R3".to_string(), 1));
        // The full-context entry point resolves it to MOVE R3,R1 when B is set
        // and Sreg=1 (as WITH R1 would have left it).
        let bytes = [0x13u8];
        let mut fetch = |a: u32| bytes[a as usize];
        let (text, len) = disassemble_one_ex(&mut fetch, 0, false, false, true, 1);
        assert_eq!(text, "MOVE R3,R1");
        assert_eq!(len, 1);
    }

    #[test]
    fn moves_via_from_uses_b_and_sreg() {
        let bytes = [0xB3u8];
        let mut fetch = |a: u32| bytes[a as usize];
        let (text, len) = disassemble_one_ex(&mut fetch, 0, false, false, true, 2);
        assert_eq!(text, "MOVES R3,R2");
        assert_eq!(len, 1);
    }

    #[test]
    fn merge_and_unary_ops_have_no_operand() {
        assert_eq!(disasm_bytes(&[0x70], 0, false, false), ("MERGE".to_string(), 1));
        assert_eq!(disasm_bytes(&[0x4F], 0, false, false), ("NOT".to_string(), 1));
        assert_eq!(disasm_bytes(&[0x4D], 0, false, false), ("SWAP".to_string(), 1));
        assert_eq!(disasm_bytes(&[0x9F], 0, false, false), ("FMULT".to_string(), 1));
        assert_eq!(disasm_bytes(&[0x9F], 0, true, false), ("LMULT".to_string(), 1));
    }

    #[test]
    fn link_and_inc_dec() {
        assert_eq!(disasm_bytes(&[0x92], 0, false, false), ("LINK #2".to_string(), 1));
        assert_eq!(disasm_bytes(&[0xD3], 0, false, false), ("INC R3".to_string(), 1));
        assert_eq!(disasm_bytes(&[0xE3], 0, false, false), ("DEC R3".to_string(), 1));
    }
}
