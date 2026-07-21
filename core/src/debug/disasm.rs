//! 65816 disassembler. Full 256-opcode table (mnemonic + addressing mode),
//! transcribed from `.claude/skills/snes-refs/references/cpu-65c816.md`
//! "Full 256-opcode table" (Bruce Clark / nesdev wiki source).

/// Addressing mode: determines operand byte count and text formatting.
/// `ImmM`/`ImmX` operand width tracks the live M/X flag at disassembly time
/// (per cpu-65c816.md "Len `3-m`/`3-x`" column), not a fixed encoding.
#[derive(Clone, Copy)]
enum Mode {
    /// No operand (CLC, NOP, TAX, RTS, ...).
    Imp,
    /// Accumulator, explicit "A" operand text (WDC datasheet syntax).
    AccA,
    /// `#$xx` (m=1) or `#$xxxx` (m=0) — ORA/AND/EOR/ADC/BIT/LDA/CMP/SBC #imm.
    ImmM,
    /// `#$xx` (x=1) or `#$xxxx` (x=0) — LDX/LDY/CPX/CPY #imm.
    ImmX,
    /// `#$xx` fixed 8-bit: BRK/COP signature byte, WDM, REP/SEP flag mask.
    Imm8,
    /// `#$xxxx` fixed 16-bit literal pushed as-is (PEA syntax omits '#' —
    /// 6502.org 65C816 opcodes tutorial).
    ImmWord,
    Dir,
    DirX,
    DirY,
    /// `(dir)`.
    IndDir,
    /// `(dir,X)`.
    IndDirX,
    /// `(dir),Y`.
    IndDirY,
    /// `[dir]`.
    IndLong,
    /// `[dir],Y`.
    IndLongY,
    /// PEI "Stack (Direct)": syntax `($nn)`, same 2-byte encoding as `Dir`.
    PeiDir,
    /// `$nn,S`.
    StackRel,
    /// `($nn,S),Y`.
    StackRelIndY,
    Abs,
    AbsX,
    AbsY,
    /// `$bb:xxxx` — bank byte is embedded in the instruction (4 bytes).
    AbsLong,
    AbsLongX,
    /// `(abs)` — JMP only.
    AbsIndirect,
    /// `(abs,X)` — JMP/JSR only.
    AbsIndirectX,
    /// `[abs]` — JMP long-indirect only (sets PBR).
    AbsIndirectLong,
    /// Branch: displayed as the resolved target address, `$aaaa`.
    Rel8,
    /// BRL/PER: displayed as the resolved target address, `$aaaa`.
    Rel16,
    /// MVN/MVP: displayed `#$ss,#$dd` (assembler operand order is src,dest;
    /// machine code stores destBank then srcBank — cpu-65c816.md MVN/MVP note).
    BlockMove,
}

use Mode::*;

/// 256-entry opcode table, opcode-indexed: (mnemonic, addressing mode).
/// Source: cpu-65c816.md "Full 256-opcode table".
const OPCODES: [(&str, Mode); 256] = [
    ("BRK", Imm8), ("ORA", IndDirX), ("COP", Imm8), ("ORA", StackRel),
    ("TSB", Dir), ("ORA", Dir), ("ASL", Dir), ("ORA", IndLong),
    ("PHP", Imp), ("ORA", ImmM), ("ASL", AccA), ("PHD", Imp),
    ("TSB", Abs), ("ORA", Abs), ("ASL", Abs), ("ORA", AbsLong),
    ("BPL", Rel8), ("ORA", IndDirY), ("ORA", IndDir), ("ORA", StackRelIndY),
    ("TRB", Dir), ("ORA", DirX), ("ASL", DirX), ("ORA", IndLongY),
    ("CLC", Imp), ("ORA", AbsY), ("INC", AccA), ("TCS", Imp),
    ("TRB", Abs), ("ORA", AbsX), ("ASL", AbsX), ("ORA", AbsLongX),
    ("JSR", Abs), ("AND", IndDirX), ("JSL", AbsLong), ("AND", StackRel),
    ("BIT", Dir), ("AND", Dir), ("ROL", Dir), ("AND", IndLong),
    ("PLP", Imp), ("AND", ImmM), ("ROL", AccA), ("PLD", Imp),
    ("BIT", Abs), ("AND", Abs), ("ROL", Abs), ("AND", AbsLong),
    ("BMI", Rel8), ("AND", IndDirY), ("AND", IndDir), ("AND", StackRelIndY),
    ("BIT", DirX), ("AND", DirX), ("ROL", DirX), ("AND", IndLongY),
    ("SEC", Imp), ("AND", AbsY), ("DEC", AccA), ("TSC", Imp),
    ("BIT", AbsX), ("AND", AbsX), ("ROL", AbsX), ("AND", AbsLongX),
    ("RTI", Imp), ("EOR", IndDirX), ("WDM", Imm8), ("EOR", StackRel),
    ("MVP", BlockMove), ("EOR", Dir), ("LSR", Dir), ("EOR", IndLong),
    ("PHA", Imp), ("EOR", ImmM), ("LSR", AccA), ("PHK", Imp),
    ("JMP", Abs), ("EOR", Abs), ("LSR", Abs), ("EOR", AbsLong),
    ("BVC", Rel8), ("EOR", IndDirY), ("EOR", IndDir), ("EOR", StackRelIndY),
    ("MVN", BlockMove), ("EOR", DirX), ("LSR", DirX), ("EOR", IndLongY),
    ("CLI", Imp), ("EOR", AbsY), ("PHY", Imp), ("TCD", Imp),
    ("JMP", AbsLong), ("EOR", AbsX), ("LSR", AbsX), ("EOR", AbsLongX),
    ("RTS", Imp), ("ADC", IndDirX), ("PER", Rel16), ("ADC", StackRel),
    ("STZ", Dir), ("ADC", Dir), ("ROR", Dir), ("ADC", IndLong),
    ("PLA", Imp), ("ADC", ImmM), ("ROR", AccA), ("RTL", Imp),
    ("JMP", AbsIndirect), ("ADC", Abs), ("ROR", Abs), ("ADC", AbsLong),
    ("BVS", Rel8), ("ADC", IndDirY), ("ADC", IndDir), ("ADC", StackRelIndY),
    ("STZ", DirX), ("ADC", DirX), ("ROR", DirX), ("ADC", IndLongY),
    ("SEI", Imp), ("ADC", AbsY), ("PLY", Imp), ("TDC", Imp),
    ("JMP", AbsIndirectX), ("ADC", AbsX), ("ROR", AbsX), ("ADC", AbsLongX),
    ("BRA", Rel8), ("STA", IndDirX), ("BRL", Rel16), ("STA", StackRel),
    ("STY", Dir), ("STA", Dir), ("STX", Dir), ("STA", IndLong),
    ("DEY", Imp), ("BIT", ImmM), ("TXA", Imp), ("PHB", Imp),
    ("STY", Abs), ("STA", Abs), ("STX", Abs), ("STA", AbsLong),
    ("BCC", Rel8), ("STA", IndDirY), ("STA", IndDir), ("STA", StackRelIndY),
    ("STY", DirX), ("STA", DirX), ("STX", DirY), ("STA", IndLongY),
    ("TYA", Imp), ("STA", AbsY), ("TXS", Imp), ("TXY", Imp),
    ("STZ", Abs), ("STA", AbsX), ("STZ", AbsX), ("STA", AbsLongX),
    ("LDY", ImmX), ("LDA", IndDirX), ("LDX", ImmX), ("LDA", StackRel),
    ("LDY", Dir), ("LDA", Dir), ("LDX", Dir), ("LDA", IndLong),
    ("TAY", Imp), ("LDA", ImmM), ("TAX", Imp), ("PLB", Imp),
    ("LDY", Abs), ("LDA", Abs), ("LDX", Abs), ("LDA", AbsLong),
    ("BCS", Rel8), ("LDA", IndDirY), ("LDA", IndDir), ("LDA", StackRelIndY),
    ("LDY", DirX), ("LDA", DirX), ("LDX", DirY), ("LDA", IndLongY),
    ("CLV", Imp), ("LDA", AbsY), ("TSX", Imp), ("TYX", Imp),
    ("LDY", AbsX), ("LDA", AbsX), ("LDX", AbsY), ("LDA", AbsLongX),
    ("CPY", ImmX), ("CMP", IndDirX), ("REP", Imm8), ("CMP", StackRel),
    ("CPY", Dir), ("CMP", Dir), ("DEC", Dir), ("CMP", IndLong),
    ("INY", Imp), ("CMP", ImmM), ("DEX", Imp), ("WAI", Imp),
    ("CPY", Abs), ("CMP", Abs), ("DEC", Abs), ("CMP", AbsLong),
    ("BNE", Rel8), ("CMP", IndDirY), ("CMP", IndDir), ("CMP", StackRelIndY),
    ("PEI", PeiDir), ("CMP", DirX), ("DEC", DirX), ("CMP", IndLongY),
    ("CLD", Imp), ("CMP", AbsY), ("PHX", Imp), ("STP", Imp),
    ("JMP", AbsIndirectLong), ("CMP", AbsX), ("DEC", AbsX), ("CMP", AbsLongX),
    ("CPX", ImmX), ("SBC", IndDirX), ("SEP", Imm8), ("SBC", StackRel),
    ("CPX", Dir), ("SBC", Dir), ("INC", Dir), ("SBC", IndLong),
    ("INX", Imp), ("SBC", ImmM), ("NOP", Imp), ("XBA", Imp),
    ("CPX", Abs), ("SBC", Abs), ("INC", Abs), ("SBC", AbsLong),
    ("BEQ", Rel8), ("SBC", IndDirY), ("SBC", IndDir), ("SBC", StackRelIndY),
    ("PEA", ImmWord), ("SBC", DirX), ("INC", DirX), ("SBC", IndLongY),
    ("SED", Imp), ("SBC", AbsY), ("PLX", Imp), ("XCE", Imp),
    ("JSR", AbsIndirectX), ("SBC", AbsX), ("INC", AbsX), ("SBC", AbsLongX),
];

/// Fetch one instruction-stream byte at `K:(pc16+off)`. Wraps within bank
/// `bank` (never carries into `bank+1`) — cpu-65c816.md "Immediate" wrap
/// rule applies to the whole instruction stream, not just #imm operands.
fn fetch_byte(fetch: &mut dyn FnMut(u32) -> u8, bank: u32, pc16: u32, off: u32) -> u8 {
    fetch(bank | (pc16.wrapping_add(off) & 0xFFFF))
}

fn fetch_word(fetch: &mut dyn FnMut(u32) -> u8, bank: u32, pc16: u32, off: u32) -> u16 {
    let lo = fetch_byte(fetch, bank, pc16, off) as u16;
    let hi = fetch_byte(fetch, bank, pc16, off + 1) as u16;
    lo | (hi << 8)
}

/// Disassemble one instruction. `fetch` reads program bytes at 24-bit
/// addresses; `m_flag`/`x_flag` select immediate operand widths.
/// Returns (text, instruction length in bytes).
pub fn disassemble_one(
    fetch: &mut dyn FnMut(u32) -> u8,
    addr: u32,
    m_flag: bool,
    x_flag: bool,
) -> (String, u8) {
    let bank = addr & 0xFF0000;
    let pc16 = addr & 0xFFFF;

    let opcode = fetch_byte(fetch, bank, pc16, 0);
    let (mnemonic, mode) = OPCODES[opcode as usize];

    let (operand, len): (String, u8) = match mode {
        Imp => (String::new(), 1),
        AccA => (" A".to_string(), 1),
        ImmM => {
            if m_flag {
                (format!(" #${:02X}", fetch_byte(fetch, bank, pc16, 1)), 2)
            } else {
                (format!(" #${:04X}", fetch_word(fetch, bank, pc16, 1)), 3)
            }
        }
        ImmX => {
            if x_flag {
                (format!(" #${:02X}", fetch_byte(fetch, bank, pc16, 1)), 2)
            } else {
                (format!(" #${:04X}", fetch_word(fetch, bank, pc16, 1)), 3)
            }
        }
        Imm8 => (format!(" #${:02X}", fetch_byte(fetch, bank, pc16, 1)), 2),
        ImmWord => (format!(" ${:04X}", fetch_word(fetch, bank, pc16, 1)), 3),
        Dir => (format!(" ${:02X}", fetch_byte(fetch, bank, pc16, 1)), 2),
        DirX => (format!(" ${:02X},X", fetch_byte(fetch, bank, pc16, 1)), 2),
        DirY => (format!(" ${:02X},Y", fetch_byte(fetch, bank, pc16, 1)), 2),
        IndDir => (format!(" (${:02X})", fetch_byte(fetch, bank, pc16, 1)), 2),
        IndDirX => (format!(" (${:02X},X)", fetch_byte(fetch, bank, pc16, 1)), 2),
        IndDirY => (format!(" (${:02X}),Y", fetch_byte(fetch, bank, pc16, 1)), 2),
        IndLong => (format!(" [${:02X}]", fetch_byte(fetch, bank, pc16, 1)), 2),
        IndLongY => (format!(" [${:02X}],Y", fetch_byte(fetch, bank, pc16, 1)), 2),
        PeiDir => (format!(" (${:02X})", fetch_byte(fetch, bank, pc16, 1)), 2),
        StackRel => (format!(" ${:02X},S", fetch_byte(fetch, bank, pc16, 1)), 2),
        StackRelIndY => (format!(" (${:02X},S),Y", fetch_byte(fetch, bank, pc16, 1)), 2),
        Abs => (format!(" ${:04X}", fetch_word(fetch, bank, pc16, 1)), 3),
        AbsX => (format!(" ${:04X},X", fetch_word(fetch, bank, pc16, 1)), 3),
        AbsY => (format!(" ${:04X},Y", fetch_word(fetch, bank, pc16, 1)), 3),
        AbsLong => {
            let addr16 = fetch_word(fetch, bank, pc16, 1);
            let bank_byte = fetch_byte(fetch, bank, pc16, 3);
            (format!(" ${bank_byte:02X}:{addr16:04X}"), 4)
        }
        AbsLongX => {
            let addr16 = fetch_word(fetch, bank, pc16, 1);
            let bank_byte = fetch_byte(fetch, bank, pc16, 3);
            (format!(" ${bank_byte:02X}:{addr16:04X},X"), 4)
        }
        AbsIndirect => (format!(" (${:04X})", fetch_word(fetch, bank, pc16, 1)), 3),
        AbsIndirectX => (format!(" (${:04X},X)", fetch_word(fetch, bank, pc16, 1)), 3),
        AbsIndirectLong => (format!(" [${:04X}]", fetch_word(fetch, bank, pc16, 1)), 3),
        Rel8 => {
            let disp = fetch_byte(fetch, bank, pc16, 1) as i8;
            // Target = K:(PC+2+disp8), 16-bit, wraps within bank K.
            let target = pc16.wrapping_add(2).wrapping_add(disp as i32 as u32) & 0xFFFF;
            (format!(" ${target:04X}"), 2)
        }
        Rel16 => {
            let disp = fetch_word(fetch, bank, pc16, 1) as i16;
            // Target = K:(PC+3+disp16), 16-bit, wraps within bank K.
            let target = pc16.wrapping_add(3).wrapping_add(disp as i32 as u32) & 0xFFFF;
            (format!(" ${target:04X}"), 3)
        }
        BlockMove => {
            // Machine code order is destBank,srcBank; assembler operand
            // order is src,dest.
            let dest_bank = fetch_byte(fetch, bank, pc16, 1);
            let src_bank = fetch_byte(fetch, bank, pc16, 2);
            (format!(" #${src_bank:02X},#${dest_bank:02X}"), 3)
        }
    };

    (format!("{mnemonic}{operand}"), len)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Disassembles `bytes` (placed at 24-bit `addr`) and returns the result.
    fn disasm_bytes(bytes: &[u8], addr: u32, m_flag: bool, x_flag: bool) -> (String, u8) {
        let mut fetch = |a: u32| -> u8 {
            let off = (a.wrapping_sub(addr) & 0xFFFF) as usize;
            bytes[off]
        };
        disassemble_one(&mut fetch, addr, m_flag, x_flag)
    }

    #[test]
    fn lda_immediate_16bit() {
        // LDA #$1234, m=0 -> 16-bit immediate, 3 bytes.
        let (text, len) = disasm_bytes(&[0xA9, 0x34, 0x12], 0x008000, false, false);
        assert_eq!(text, "LDA #$1234");
        assert_eq!(len, 3);
    }

    #[test]
    fn lda_immediate_8bit() {
        // LDA #$12, m=1 -> 8-bit immediate, 2 bytes.
        let (text, len) = disasm_bytes(&[0xA9, 0x12], 0x008000, true, false);
        assert_eq!(text, "LDA #$12");
        assert_eq!(len, 2);
    }

    #[test]
    fn ldx_immediate_16bit() {
        // LDX #$1234, x=0 -> 16-bit immediate, 3 bytes (index-width, not m).
        let (text, len) = disasm_bytes(&[0xA2, 0x34, 0x12], 0x008000, true, false);
        assert_eq!(text, "LDX #$1234");
        assert_eq!(len, 3);
    }

    #[test]
    fn sta_absolute() {
        let (text, len) = disasm_bytes(&[0x8D, 0x34, 0x12], 0x008000, false, false);
        assert_eq!(text, "STA $1234");
        assert_eq!(len, 3);
    }

    #[test]
    fn sta_long_x() {
        // STA $7E:0000,X — bank byte embedded in the instruction (long,X).
        let (text, len) = disasm_bytes(&[0x9F, 0x00, 0x00, 0x7E], 0x008000, false, false);
        assert_eq!(text, "STA $7E:0000,X");
        assert_eq!(len, 4);
    }

    #[test]
    fn sta_direct_x() {
        let (text, len) = disasm_bytes(&[0x95, 0x12], 0x008000, false, false);
        assert_eq!(text, "STA $12,X");
        assert_eq!(len, 2);
    }

    #[test]
    fn lda_dir_x_indirect() {
        let (text, len) = disasm_bytes(&[0xA1, 0x12], 0x008000, false, false);
        assert_eq!(text, "LDA ($12,X)");
        assert_eq!(len, 2);
    }

    #[test]
    fn lda_dir_indirect_y() {
        let (text, len) = disasm_bytes(&[0xB1, 0x12], 0x008000, false, false);
        assert_eq!(text, "LDA ($12),Y");
        assert_eq!(len, 2);
    }

    #[test]
    fn lda_dir_indirect_long() {
        let (text, len) = disasm_bytes(&[0xA7, 0x12], 0x008000, false, false);
        assert_eq!(text, "LDA [$12]");
        assert_eq!(len, 2);
    }

    #[test]
    fn lda_dir_indirect_long_y() {
        let (text, len) = disasm_bytes(&[0xB7, 0x12], 0x008000, false, false);
        assert_eq!(text, "LDA [$12],Y");
        assert_eq!(len, 2);
    }

    #[test]
    fn lda_stack_relative() {
        let (text, len) = disasm_bytes(&[0xA3, 0x12], 0x008000, false, false);
        assert_eq!(text, "LDA $12,S");
        assert_eq!(len, 2);
    }

    #[test]
    fn lda_stack_relative_indirect_y() {
        let (text, len) = disasm_bytes(&[0xB3, 0x12], 0x008000, false, false);
        assert_eq!(text, "LDA ($12,S),Y");
        assert_eq!(len, 2);
    }

    #[test]
    fn bra_forward() {
        // BRA $8004: opcode at $8000, disp=+2 -> target = $8000+2+2 = $8004.
        let (text, len) = disasm_bytes(&[0x80, 0x02], 0x008000, false, false);
        assert_eq!(text, "BRA $8004");
        assert_eq!(len, 2);
    }

    #[test]
    fn brl_forward() {
        // BRL $8010: opcode at $8000, disp16=+$0D -> target = $8000+3+$0D = $8010.
        let (text, len) = disasm_bytes(&[0x82, 0x0D, 0x00], 0x008000, false, false);
        assert_eq!(text, "BRL $8010");
        assert_eq!(len, 3);
    }

    #[test]
    fn jmp_indirect() {
        let (text, len) = disasm_bytes(&[0x6C, 0x34, 0x12], 0x008000, false, false);
        assert_eq!(text, "JMP ($1234)");
        assert_eq!(len, 3);
    }

    #[test]
    fn jmp_indirect_x() {
        let (text, len) = disasm_bytes(&[0x7C, 0x34, 0x12], 0x008000, false, false);
        assert_eq!(text, "JMP ($1234,X)");
        assert_eq!(len, 3);
    }

    #[test]
    fn jmp_indirect_long() {
        let (text, len) = disasm_bytes(&[0xDC, 0x34, 0x12], 0x008000, false, false);
        assert_eq!(text, "JMP [$1234]");
        assert_eq!(len, 3);
    }

    #[test]
    fn jsl_long() {
        let (text, len) = disasm_bytes(&[0x22, 0x34, 0x12, 0x7E], 0x008000, false, false);
        assert_eq!(text, "JSL $7E:1234");
        assert_eq!(len, 4);
    }

    #[test]
    fn mvn_operand_order() {
        // Machine code: opcode, destBank, srcBank. Assembler text: src,dest.
        let (text, len) = disasm_bytes(&[0x54, 0x7E, 0x7F], 0x008000, false, false);
        assert_eq!(text, "MVN #$7F,#$7E");
        assert_eq!(len, 3);
    }

    #[test]
    fn asl_accumulator() {
        let (text, len) = disasm_bytes(&[0x0A], 0x008000, false, false);
        assert_eq!(text, "ASL A");
        assert_eq!(len, 1);
    }

    #[test]
    fn nop_implied() {
        let (text, len) = disasm_bytes(&[0xEA], 0x008000, false, false);
        assert_eq!(text, "NOP");
        assert_eq!(len, 1);
    }

    #[test]
    fn pei_direct() {
        let (text, len) = disasm_bytes(&[0xD4, 0x12], 0x008000, false, false);
        assert_eq!(text, "PEI ($12)");
        assert_eq!(len, 2);
    }

    #[test]
    fn pea_absolute() {
        // PEA syntax omits '#' despite pushing a literal 16-bit value.
        let (text, len) = disasm_bytes(&[0xF4, 0x34, 0x12], 0x008000, false, false);
        assert_eq!(text, "PEA $1234");
        assert_eq!(len, 3);
    }

    #[test]
    fn brk_signature_byte() {
        let (text, len) = disasm_bytes(&[0x00, 0x12], 0x008000, false, false);
        assert_eq!(text, "BRK #$12");
        assert_eq!(len, 2);
    }

    #[test]
    fn rel8_wraps_within_bank() {
        // BPL at $00FFFE with disp=+4: target = ($FFFE+2+4)&$FFFF = $0004,
        // still bank $00 (wraps, never carries into bank $01).
        let (text, _len) = disasm_bytes(&[0x10, 0x04], 0x00FFFE, false, false);
        assert_eq!(text, "BPL $0004");
    }
}
