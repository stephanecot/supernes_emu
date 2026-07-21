//! SPC700 trace-line formatting for `--trace-spc`.
//!
//! Mnemonics and instruction lengths transcribed from the 256-opcode table in
//! references/apu.md (itself from the nesdev S-SMP page / fullsnes). This is a
//! debug aid: the mnemonic template is shown verbatim from the table and the
//! actual operand bytes are appended in hex, so no operand-ordering assumptions
//! are baked in.

use crate::apu::spc700::Spc700;

/// (mnemonic, total instruction length in bytes), indexed by opcode.
/// Transcribed column-by-column from the apu.md opcode table ($00-$3F, $40-$7F,
/// $80-$BF, $C0-$FF).
const TABLE: [(&str, u8); 256] = [
    // $00-$0F
    ("NOP", 1), ("TCALL 0", 1), ("SET1 d.0", 2), ("BBS d.0,r", 3),
    ("OR A,d", 2), ("OR A,!a", 3), ("OR A,(X)", 1), ("OR A,[d+X]", 2),
    ("OR A,#i", 2), ("OR d,d", 3), ("OR1 C,m.b", 3), ("ASL d", 2),
    ("ASL !a", 3), ("PUSH PSW", 1), ("TSET1 !a", 3), ("BRK", 1),
    // $10-$1F
    ("BPL r", 2), ("TCALL 1", 1), ("CLR1 d.0", 2), ("BBC d.0,r", 3),
    ("OR A,d+X", 2), ("OR A,!a+X", 3), ("OR A,!a+Y", 3), ("OR A,[d]+Y", 2),
    ("OR d,#i", 3), ("OR (X),(Y)", 1), ("DECW d", 2), ("ASL d+X", 2),
    ("ASL A", 1), ("DEC X", 1), ("CMP X,!a", 3), ("JMP [!a+X]", 3),
    // $20-$2F
    ("CLRP", 1), ("TCALL 2", 1), ("SET1 d.1", 2), ("BBS d.1,r", 3),
    ("AND A,d", 2), ("AND A,!a", 3), ("AND A,(X)", 1), ("AND A,[d+X]", 2),
    ("AND A,#i", 2), ("AND d,d", 3), ("OR1 C,/m.b", 3), ("ROL d", 2),
    ("ROL !a", 3), ("PUSH A", 1), ("CBNE d,r", 3), ("BRA r", 2),
    // $30-$3F
    ("BMI r", 2), ("TCALL 3", 1), ("CLR1 d.1", 2), ("BBC d.1,r", 3),
    ("AND A,d+X", 2), ("AND A,!a+X", 3), ("AND A,!a+Y", 3), ("AND A,[d]+Y", 2),
    ("AND d,#i", 3), ("AND (X),(Y)", 1), ("INCW d", 2), ("ROL d+X", 2),
    ("ROL A", 1), ("INC X", 1), ("CMP X,d", 2), ("CALL !a", 3),
    // $40-$4F
    ("SETP", 1), ("TCALL 4", 1), ("SET1 d.2", 2), ("BBS d.2,r", 3),
    ("EOR A,d", 2), ("EOR A,!a", 3), ("EOR A,(X)", 1), ("EOR A,[d+X]", 2),
    ("EOR A,#i", 2), ("EOR d,d", 3), ("AND1 C,m.b", 3), ("LSR d", 2),
    ("LSR !a", 3), ("PUSH X", 1), ("TCLR1 !a", 3), ("PCALL u", 2),
    // $50-$5F
    ("BVC r", 2), ("TCALL 5", 1), ("CLR1 d.2", 2), ("BBC d.2,r", 3),
    ("EOR A,d+X", 2), ("EOR A,!a+X", 3), ("EOR A,!a+Y", 3), ("EOR A,[d]+Y", 2),
    ("EOR d,#i", 3), ("EOR (X),(Y)", 1), ("CMPW YA,d", 2), ("LSR d+X", 2),
    ("LSR A", 1), ("MOV X,A", 1), ("CMP Y,!a", 3), ("JMP !a", 3),
    // $60-$6F
    ("CLRC", 1), ("TCALL 6", 1), ("SET1 d.3", 2), ("BBS d.3,r", 3),
    ("CMP A,d", 2), ("CMP A,!a", 3), ("CMP A,(X)", 1), ("CMP A,[d+X]", 2),
    ("CMP A,#i", 2), ("CMP d,d", 3), ("AND1 C,/m.b", 3), ("ROR d", 2),
    ("ROR !a", 3), ("PUSH Y", 1), ("DBNZ d,r", 3), ("RET", 1),
    // $70-$7F
    ("BVS r", 2), ("TCALL 7", 1), ("CLR1 d.3", 2), ("BBC d.3,r", 3),
    ("CMP A,d+X", 2), ("CMP A,!a+X", 3), ("CMP A,!a+Y", 3), ("CMP A,[d]+Y", 2),
    ("CMP d,#i", 3), ("CMP (X),(Y)", 1), ("ADDW YA,d", 2), ("ROR d+X", 2),
    ("ROR A", 1), ("MOV A,X", 1), ("CMP Y,d", 2), ("RETI", 1),
    // $80-$8F
    ("SETC", 1), ("TCALL 8", 1), ("SET1 d.4", 2), ("BBS d.4,r", 3),
    ("ADC A,d", 2), ("ADC A,!a", 3), ("ADC A,(X)", 1), ("ADC A,[d+X]", 2),
    ("ADC A,#i", 2), ("ADC d,d", 3), ("EOR1 C,m.b", 3), ("DEC d", 2),
    ("DEC !a", 3), ("MOV Y,#i", 2), ("POP PSW", 1), ("MOV d,#i", 3),
    // $90-$9F
    ("BCC r", 2), ("TCALL 9", 1), ("CLR1 d.4", 2), ("BBC d.4,r", 3),
    ("ADC A,d+X", 2), ("ADC A,!a+X", 3), ("ADC A,!a+Y", 3), ("ADC A,[d]+Y", 2),
    ("ADC d,#i", 3), ("ADC (X),(Y)", 1), ("SUBW YA,d", 2), ("DEC d+X", 2),
    ("DEC A", 1), ("MOV X,SP", 1), ("DIV YA,X", 1), ("XCN A", 1),
    // $A0-$AF
    ("EI", 1), ("TCALL 10", 1), ("SET1 d.5", 2), ("BBS d.5,r", 3),
    ("SBC A,d", 2), ("SBC A,!a", 3), ("SBC A,(X)", 1), ("SBC A,[d+X]", 2),
    ("SBC A,#i", 2), ("SBC d,d", 3), ("MOV1 C,m.b", 3), ("INC d", 2),
    ("INC !a", 3), ("CMP Y,#i", 2), ("POP A", 1), ("MOV (X)+,A", 1),
    // $B0-$BF
    ("BCS r", 2), ("TCALL 11", 1), ("CLR1 d.5", 2), ("BBC d.5,r", 3),
    ("SBC A,d+X", 2), ("SBC A,!a+X", 3), ("SBC A,!a+Y", 3), ("SBC A,[d]+Y", 2),
    ("SBC d,#i", 3), ("SBC (X),(Y)", 1), ("MOVW YA,d", 2), ("INC d+X", 2),
    ("INC A", 1), ("MOV SP,X", 1), ("DAS A", 1), ("MOV A,(X)+", 1),
    // $C0-$CF
    ("DI", 1), ("TCALL 12", 1), ("SET1 d.6", 2), ("BBS d.6,r", 3),
    ("MOV d,A", 2), ("MOV !a,A", 3), ("MOV (X),A", 1), ("MOV [d+X],A", 2),
    ("CMP X,#i", 2), ("MOV !a,X", 3), ("MOV1 m.b,C", 3), ("MOV d,Y", 2),
    ("MOV !a,Y", 3), ("MOV X,#i", 2), ("POP X", 1), ("MUL YA", 1),
    // $D0-$DF
    ("BNE r", 2), ("TCALL 13", 1), ("CLR1 d.6", 2), ("BBC d.6,r", 3),
    ("MOV d+X,A", 2), ("MOV !a+X,A", 3), ("MOV !a+Y,A", 3), ("MOV [d]+Y,A", 2),
    ("MOV d,X", 2), ("MOV d+Y,X", 2), ("MOVW d,YA", 2), ("MOV d+X,Y", 2),
    ("DEC Y", 1), ("MOV A,Y", 1), ("CBNE d+X,r", 3), ("DAA A", 1),
    // $E0-$EF
    ("CLRV", 1), ("TCALL 14", 1), ("SET1 d.7", 2), ("BBS d.7,r", 3),
    ("MOV A,d", 2), ("MOV A,!a", 3), ("MOV A,(X)", 1), ("MOV A,[d+X]", 2),
    ("MOV A,#i", 2), ("MOV X,!a", 3), ("NOT1 m.b", 3), ("MOV Y,d", 2),
    ("MOV Y,!a", 3), ("NOTC", 1), ("POP Y", 1), ("SLEEP", 1),
    // $F0-$FF
    ("BEQ r", 2), ("TCALL 15", 1), ("CLR1 d.7", 2), ("BBC d.7,r", 3),
    ("MOV A,d+X", 2), ("MOV A,!a+X", 3), ("MOV A,!a+Y", 3), ("MOV A,[d]+Y", 2),
    ("MOV X,d", 2), ("MOV X,d+Y", 2), ("MOV d,d", 3), ("MOV Y,d+X", 2),
    ("INC Y", 1), ("MOV Y,A", 1), ("DBNZ Y,r", 2), ("STOP", 1),
];

/// PSW flag letters in N V P B H I Z C order; uppercase = set.
fn psw_string(spc: &Spc700) -> String {
    let f = |set: bool, up: char, lo: char| if set { up } else { lo };
    [
        f(spc.n, 'N', 'n'),
        f(spc.v, 'V', 'v'),
        f(spc.p, 'P', 'p'),
        f(spc.b, 'B', 'b'),
        f(spc.h, 'H', 'h'),
        f(spc.i, 'I', 'i'),
        f(spc.z, 'Z', 'z'),
        f(spc.c, 'C', 'c'),
    ]
    .iter()
    .collect()
}

/// Format one SPC700 trace line for the instruction about to execute at PC.
/// `read` fetches ARAM bytes (the disassembler makes no clock side effects).
/// Operand bytes are shown raw in hex after the mnemonic template.
pub fn spc_trace_line(spc: &Spc700, read: &mut dyn FnMut(u16) -> u8) -> String {
    let op = read(spc.pc);
    let (mnem, len) = TABLE[op as usize];
    let mut ops = String::new();
    for i in 1..len as u16 {
        ops.push_str(&format!("{:02X} ", read(spc.pc.wrapping_add(i))));
    }
    format!(
        "{:04X}: {:02X} {:<6}{:<14} A:{:02X} X:{:02X} Y:{:02X} SP:{:02X} YA:{:02X}{:02X} P:{}",
        spc.pc,
        op,
        ops,
        mnem,
        spc.a,
        spc.x,
        spc.y,
        spc.sp,
        spc.y,
        spc.a,
        psw_string(spc),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_full_and_lengths_sane() {
        assert_eq!(TABLE.len(), 256);
        for (op, &(name, len)) in TABLE.iter().enumerate() {
            assert!(!name.is_empty(), "opcode {op:02X} has no mnemonic");
            assert!((1..=3).contains(&len), "opcode {op:02X} bad length {len}");
        }
        assert_eq!(TABLE[0x00], ("NOP", 1));
        assert_eq!(TABLE[0xE8], ("MOV A,#i", 2));
        assert_eq!(TABLE[0x3F], ("CALL !a", 3));
        assert_eq!(TABLE[0xFF], ("STOP", 1));
    }

    #[test]
    fn trace_line_shows_pc_mnemonic_operands_and_regs() {
        let mut spc = Spc700::new();
        spc.pc = 0x0500;
        spc.a = 0x12;
        spc.x = 0x34;
        spc.y = 0x56;
        spc.sp = 0xEF;
        // E8 78 = MOV A,#$78
        let bytes = [0xE8u8, 0x78];
        let mut read = |a: u16| bytes[(a - 0x0500) as usize];
        let line = spc_trace_line(&spc, &mut read);
        assert!(line.starts_with("0500: E8 78    MOV A,#i"), "got: {line}");
        assert!(line.contains("A:12"));
        assert!(line.contains("X:34"));
        assert!(line.contains("YA:5612"));
        assert!(line.contains("SP:EF"));
    }
}
