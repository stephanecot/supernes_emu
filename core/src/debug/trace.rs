//! Mesen2-format CPU trace line formatting.

use crate::cpu::{Cpu, Flags};
use crate::debug::disasm::disassemble_one;

/// Flag letters in Mesen2 nvmxdizc order: uppercase = set, lowercase =
/// clear. In emulation mode bit4 is the B (break) flag, not X (P flags
/// table, cpu-65c816.md) — shown as b/B instead of x/X so a trace diff
/// against real Mesen2 output (which also switches the letter) stays
/// meaningful across mode changes.
fn flags_string(p: Flags, emulation: bool) -> String {
    let flag_char = |set: bool, upper: char, lower: char| if set { upper } else { lower };
    let (x_upper, x_lower) = if emulation { ('B', 'b') } else { ('X', 'x') };
    [
        flag_char(p.n(), 'N', 'n'),
        flag_char(p.v(), 'V', 'v'),
        flag_char(p.m(), 'M', 'm'),
        flag_char(p.x(), x_upper, x_lower),
        flag_char(p.d(), 'D', 'd'),
        flag_char(p.i(), 'I', 'i'),
        flag_char(p.z(), 'Z', 'z'),
        flag_char(p.c(), 'C', 'c'),
    ]
    .iter()
    .collect()
}

/// Format one Mesen2-style trace line for the instruction about to execute
/// at PBR:PC. `fetch` reads program bytes at 24-bit addresses (bus-backed,
/// so this works headless without giving `Cpu` a bus reference).
pub fn trace_line(cpu: &Cpu, fetch: &mut dyn FnMut(u32) -> u8) -> String {
    let addr = ((cpu.pbr as u32) << 16) | cpu.pc as u32;
    let (text, _len) = disassemble_one(fetch, addr, cpu.p.m(), cpu.p.x());
    format!(
        "{:02X}:{:04X} {:<28}A:{:04X} X:{:04X} Y:{:04X} S:{:04X} D:{:04X} DB:{:02X} P:{}",
        cpu.pbr,
        cpu.pc,
        text,
        cpu.a,
        cpu.x,
        cpu.y,
        cpu.s,
        cpu.d,
        cpu.dbr,
        flags_string(cpu.p, cpu.emulation),
    )
}

/// Extended trace line adding PPU V/H counters and a master-clock cycle
/// count, for correlating CPU trace lines with PPU timing (frontend
/// `--trace` wiring passes these when the scheduler exposes them).
pub fn trace_line_with_counters(
    cpu: &Cpu,
    fetch: &mut dyn FnMut(u32) -> u8,
    v_counter: u16,
    h_counter: u16,
    cycle: u64,
) -> String {
    format!(
        "{} V:{v_counter} H:{h_counter} CYC:{cycle}",
        trace_line(cpu, fetch)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_line_formats_registers_and_disassembly() {
        let mut cpu = Cpu::new();
        cpu.pbr = 0x80;
        cpu.pc = 0x8000;
        cpu.a = 0x1234;
        cpu.x = 0x0056;
        cpu.y = 0x0078;
        cpu.s = 0x01FF;
        cpu.d = 0x0000;
        cpu.dbr = 0x00;
        cpu.p.set_n(true);
        cpu.p.set_c(true);
        cpu.emulation = false;

        // LDA #$1234 at $80:8000 (m=0 since p.m() defaults false after
        // Cpu::new()... Cpu::new() sets M=1, so force native widths here).
        cpu.p.set_m(false);
        cpu.p.set_x(false);
        let bytes = [0xA9u8, 0x34, 0x12];
        let mut fetch = |a: u32| bytes[(a - 0x808000) as usize];

        let line = trace_line(&cpu, &mut fetch);
        assert!(line.starts_with("80:8000 LDA #$1234"));
        assert!(line.contains("A:1234"));
        assert!(line.contains("X:0056"));
        assert!(line.contains("Y:0078"));
        assert!(line.contains("S:01FF"));
        assert!(line.contains("D:0000"));
        assert!(line.contains("DB:00"));
        // n=1,v=0,m=0,x=0,d=0,i=1(reset default),z=0,c=1 -> "Nvmxd Izc" with
        // I still set from Cpu::new() default (I=1 after reset-like new()).
        assert!(line.contains("P:"));
        let p_field = line.split("P:").nth(1).unwrap();
        assert_eq!(p_field.chars().next(), Some('N'));
        assert_eq!(p_field.chars().nth(6), Some('z'));
        assert_eq!(p_field.chars().nth(7), Some('C'));
    }

    #[test]
    fn flags_string_uses_break_letter_in_emulation_mode() {
        let mut p = Flags(0);
        p.set_x(true);
        assert_eq!(flags_string(p, true), "nvmBdizc");
        assert_eq!(flags_string(p, false), "nvmXdizc");
    }
}
