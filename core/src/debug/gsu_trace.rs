//! GSU (SuperFX) trace-line formatting for `--trace-gsu`.

use crate::coprocessor::superfx::SuperFx;
use crate::debug::gsu_disasm::disassemble_one_ex;

/// SFR flag letters Z,S,CY,OV,GO,B,ALT1,ALT2 (superfx.md §2); uppercase = set,
/// `-` = clear. Fixed letters (not upper/lower pairs) for ALT1/ALT2 since
/// their "on" character (1/2) is more readable at a glance than a case change.
fn sfr_string(fx: &SuperFx) -> String {
    let f = |set: bool, c: char| if set { c } else { '-' };
    [
        f(fx.z, 'Z'),
        f(fx.s, 'S'),
        f(fx.cy, 'C'),
        f(fx.ov, 'V'),
        f(fx.go, 'G'),
        f(fx.b, 'B'),
        f(fx.alt1, '1'),
        f(fx.alt2, '2'),
    ]
    .iter()
    .collect()
}

/// Format one GSU trace line for the instruction about to execute.
///
/// `fetch` reads the GSU's own code-fetch view (code cache when valid, else
/// ROM/RAM) at 24-bit PBR:addr16 addresses, with no side effects (callers pass
/// `SuperFx::peek_code`, which never fills/mutates the cache — the emulator
/// behaves identically whether or not a trace sink is installed).
///
/// The displayed PC is `r15 - 1`, not `r15`: the GSU's one-byte prefetch
/// pipeline (`gsu.rs` module doc) means `r15` always addresses the *next*
/// fetch, while the byte about to execute (`pipe`) sits at `r15 - 1` at the
/// moment this is called (right before `execute_one` consumes it).
pub fn gsu_trace_line(fx: &SuperFx, fetch: &mut dyn FnMut(u32) -> u8) -> String {
    let pc = fx.r[15].wrapping_sub(1);
    let addr = ((fx.pbr as u32) << 16) | pc as u32;
    let (text, _len) =
        disassemble_one_ex(fetch, addr, fx.alt1, fx.alt2, fx.b, fx.sreg as u8);

    let mut regs = String::new();
    for i in 0..16 {
        regs.push_str(&format!("R{i}:{:04X} ", fx.r[i]));
    }

    format!(
        "{:02X}:{:04X} {:<20}{}SFR:{}",
        fx.pbr,
        pc,
        text,
        regs,
        sfr_string(fx),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coprocessor::superfx::VCR_GSU2;

    #[test]
    fn trace_line_shows_pc_disasm_registers_and_flags() {
        let mut fx = SuperFx::new(0x8000, VCR_GSU2);
        fx.pbr = 0x00;
        fx.r[15] = 0x0002; // pipeline already prefetched one byte past the opcode
        fx.r[3] = 0x1234;
        fx.z = true;
        fx.cy = true;
        // ADD R3 (0x53) at PBR:0001, the byte `pipe` = r15-1 addresses.
        let bytes = [0x53u8];
        let mut fetch = |a: u32| bytes[(a - 0x000001) as usize];
        let line = gsu_trace_line(&fx, &mut fetch);
        assert!(line.starts_with("00:0001 ADD R3"), "got: {line}");
        assert!(line.contains("R3:1234"));
        assert!(line.contains("R15:0002"));
        // Flag order Z,S,CY,OV,GO,B,ALT1,ALT2: Z and CY set, rest clear.
        assert!(line.contains("SFR:Z-C-----"), "got: {line}");
    }

    #[test]
    fn sfr_string_reflects_flag_state() {
        let mut fx = SuperFx::new(0x8000, VCR_GSU2);
        fx.z = true;
        fx.cy = true;
        fx.alt2 = true;
        assert_eq!(sfr_string(&fx), "Z-C----2");
    }
}
