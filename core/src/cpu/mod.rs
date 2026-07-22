//! 65C816 CPU core. Register file, status flags, bus trait, reset sequence.
//! Instruction execution arrives in M1.

pub mod addressing;
pub mod algorithms;
pub mod ops;

use serde::{Deserialize, Serialize};

/// Processor status register P. Bit layout: N V M X D I Z C (bit7..bit0).
/// In emulation mode, bit5 (M position) is unused and bit4 (X position) is the B flag.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Flags(pub u8);

macro_rules! flag_accessors {
    ($($get:ident, $set:ident, $bit:expr;)*) => {
        $(
            #[inline]
            pub fn $get(self) -> bool {
                self.0 & (1 << $bit) != 0
            }
            #[inline]
            pub fn $set(&mut self, v: bool) {
                if v { self.0 |= 1 << $bit } else { self.0 &= !(1 << $bit) }
            }
        )*
    };
}

impl Flags {
    pub const N: u8 = 0x80;
    pub const V: u8 = 0x40;
    pub const M: u8 = 0x20;
    pub const X: u8 = 0x10;
    pub const D: u8 = 0x08;
    pub const I: u8 = 0x04;
    pub const Z: u8 = 0x02;
    pub const C: u8 = 0x01;

    flag_accessors! {
        n, set_n, 7;
        v, set_v, 6;
        m, set_m, 5;
        x, set_x, 4;
        d, set_d, 3;
        i, set_i, 2;
        z, set_z, 1;
        c, set_c, 0;
    }
}

/// Memory interface seen by the CPU. Monomorphized so a flat-RAM bus can be
/// substituted for TomHarte JSON tests without redesign.
pub trait CpuBus {
    /// Read one byte at 24-bit address; advances the master clock by the
    /// region-dependent access cost.
    fn read(&mut self, addr: u32) -> u8;
    /// Write one byte at 24-bit address; advances the master clock.
    fn write(&mut self, addr: u32, value: u8);
    /// One internal (I/O) cycle: 6 master cycles, no bus access.
    fn idle(&mut self);
    /// Returns true once per latched NMI edge (consumes the latch).
    fn take_nmi(&mut self) -> bool;
    /// Level-sensitive IRQ line state.
    fn irq_level(&mut self) -> bool;
}

#[derive(Serialize, Deserialize)]
pub struct Cpu {
    /// Accumulator (C = B:A; 8-bit A when P.M=1).
    pub a: u16,
    pub x: u16,
    pub y: u16,
    /// Stack pointer. In emulation mode the high byte is forced to $01.
    pub s: u16,
    /// Direct page register.
    pub d: u16,
    pub pc: u16,
    /// Data bank register.
    pub dbr: u8,
    /// Program bank register.
    pub pbr: u8,
    pub p: Flags,
    /// E flag: 6502 emulation mode.
    pub emulation: bool,
    /// Halted by WAI until interrupt.
    pub waiting: bool,
    /// Halted by STP until reset.
    pub stopped: bool,
}

impl Cpu {
    pub fn new() -> Self {
        Cpu {
            a: 0,
            x: 0,
            y: 0,
            s: 0x01FF,
            d: 0,
            pc: 0,
            dbr: 0,
            pbr: 0,
            p: Flags(Flags::M | Flags::X | Flags::I),
            emulation: true,
            waiting: false,
            stopped: false,
        }
    }

    /// Hardware reset: E=1, M=X=1, I=1, D=$0000, DBR=PBR=$00, S=$01FF
    /// (S high byte forced to $01 in emulation mode), PC from the emulation-mode
    /// reset vector at $00FFFC.
    pub fn reset<B: CpuBus>(&mut self, bus: &mut B) {
        self.emulation = true;
        self.p.set_m(true);
        self.p.set_x(true);
        self.p.set_i(true);
        self.p.set_d(false);
        self.d = 0;
        self.dbr = 0;
        self.pbr = 0;
        self.s = 0x01FF;
        self.x &= 0x00FF;
        self.y &= 0x00FF;
        self.waiting = false;
        self.stopped = false;
        let lo = bus.read(0x00FFFC) as u16;
        let hi = bus.read(0x00FFFD) as u16;
        self.pc = (hi << 8) | lo;
    }

    /// Execute one instruction, or service a pending interrupt / honor WAI-STP.
    pub fn step<B: CpuBus>(&mut self, bus: &mut B) {
        if self.stopped {
            return;
        }
        let nmi = bus.take_nmi();
        let irq = bus.irq_level();

        if self.waiting {
            if nmi || irq {
                self.waiting = false;
            } else {
                bus.idle();
                return;
            }
        }

        if nmi {
            self.service_interrupt(bus, 0xFFEA, 0xFFFA, false, false);
            return;
        }
        // WAI released with I=1 falls through to execute the next instruction.
        if irq && !self.p.i() {
            self.service_interrupt(bus, 0xFFEE, 0xFFFE, false, false);
            return;
        }

        let opcode = self.fetch8(bus);
        self.execute(bus, opcode);
    }

    // ---- Program / stack primitives ----

    pub(crate) fn fetch8<B: CpuBus>(&mut self, bus: &mut B) -> u8 {
        let addr = ((self.pbr as u32) << 16) | self.pc as u32;
        self.pc = self.pc.wrapping_add(1);
        bus.read(addr)
    }

    pub(crate) fn fetch16<B: CpuBus>(&mut self, bus: &mut B) -> u16 {
        let lo = self.fetch8(bus) as u16;
        let hi = self.fetch8(bus) as u16;
        lo | (hi << 8)
    }

    pub(crate) fn fetch24<B: CpuBus>(&mut self, bus: &mut B) -> u32 {
        let lo = self.fetch8(bus) as u32;
        let mid = self.fetch8(bus) as u32;
        let hi = self.fetch8(bus) as u32;
        lo | (mid << 8) | (hi << 16)
    }

    pub(crate) fn push8<B: CpuBus>(&mut self, bus: &mut B, v: u8) {
        bus.write(self.s as u32, v);
        self.s = self.s.wrapping_sub(1);
        if self.emulation {
            self.s = 0x0100 | (self.s & 0x00FF);
        }
    }

    pub(crate) fn pull8<B: CpuBus>(&mut self, bus: &mut B) -> u8 {
        self.s = self.s.wrapping_add(1);
        if self.emulation {
            self.s = 0x0100 | (self.s & 0x00FF);
        }
        bus.read(self.s as u32)
    }

    pub(crate) fn push16<B: CpuBus>(&mut self, bus: &mut B, v: u16) {
        self.push8(bus, (v >> 8) as u8);
        self.push8(bus, v as u8);
    }

    pub(crate) fn pull16<B: CpuBus>(&mut self, bus: &mut B) -> u16 {
        let lo = self.pull8(bus) as u16;
        let hi = self.pull8(bus) as u16;
        lo | (hi << 8)
    }

    /// Stack push for the "new" 65C816 stack ops (PEA/PEI/PER/PHD/PLD/JSL/RTL).
    /// The stack pointer is the full 16-bit register: even in emulation mode S is
    /// decremented mod $10000 with no per-byte page-1 wrap, so a multi-byte push
    /// may temporarily leave page 1. `stack_end` re-imposes SH=$01 afterwards.
    pub(crate) fn push8_new<B: CpuBus>(&mut self, bus: &mut B, v: u8) {
        bus.write(self.s as u32, v);
        self.s = self.s.wrapping_sub(1);
    }

    pub(crate) fn pull8_new<B: CpuBus>(&mut self, bus: &mut B) -> u8 {
        self.s = self.s.wrapping_add(1);
        bus.read(self.s as u32)
    }

    pub(crate) fn push16_new<B: CpuBus>(&mut self, bus: &mut B, v: u16) {
        self.push8_new(bus, (v >> 8) as u8);
        self.push8_new(bus, v as u8);
    }

    pub(crate) fn pull16_new<B: CpuBus>(&mut self, bus: &mut B) -> u16 {
        let lo = self.pull8_new(bus) as u16;
        let hi = self.pull8_new(bus) as u16;
        lo | (hi << 8)
    }

    /// Re-impose emulation-mode SH=$01 after a "new" stack op completes; a no-op
    /// in native mode where S is already a full 16-bit register.
    pub(crate) fn stack_end(&mut self) {
        if self.emulation {
            self.s = 0x0100 | (self.s & 0x00FF);
        }
    }

    // ---- Register width helpers ----

    /// True when the accumulator/memory is 8-bit (P.M=1).
    pub(crate) fn m8(&self) -> bool {
        self.p.m()
    }
    /// True when index registers are 8-bit (P.X=1).
    pub(crate) fn x8(&self) -> bool {
        self.p.x()
    }

    /// Write the accumulator honoring its width (preserve B when 8-bit).
    pub(crate) fn set_a(&mut self, v: u16) {
        if self.p.m() {
            self.a = (self.a & 0xFF00) | (v & 0x00FF);
        } else {
            self.a = v;
        }
    }

    pub(crate) fn set_x_reg(&mut self, v: u16) {
        self.x = if self.p.x() { v & 0x00FF } else { v };
    }

    pub(crate) fn set_y_reg(&mut self, v: u16) {
        self.y = if self.p.x() { v & 0x00FF } else { v };
    }

    pub(crate) fn set_nz_m(&mut self, v: u16) {
        let sign = if self.p.m() { 0x0080 } else { 0x8000 };
        let mask = if self.p.m() { 0x00FF } else { 0xFFFF };
        self.p.set_n(v & sign != 0);
        self.p.set_z(v & mask == 0);
    }

    pub(crate) fn set_nz_x(&mut self, v: u16) {
        let sign = if self.p.x() { 0x0080 } else { 0x8000 };
        let mask = if self.p.x() { 0x00FF } else { 0xFFFF };
        self.p.set_n(v & sign != 0);
        self.p.set_z(v & mask == 0);
    }

    pub(crate) fn set_nz16(&mut self, v: u16) {
        self.p.set_n(v & 0x8000 != 0);
        self.p.set_z(v == 0);
    }

    /// Re-impose emulation-mode / X-flag invariants after any change to P or E.
    pub(crate) fn apply_flag_constraints(&mut self) {
        if self.emulation {
            self.p.set_m(true);
            self.p.set_x(true);
            self.s = 0x0100 | (self.s & 0x00FF);
        }
        if self.p.x() {
            self.x &= 0x00FF;
            self.y &= 0x00FF;
        }
    }

    /// Common interrupt/BRK/COP entry. `software` (BRK/COP) skips the two lead
    /// internal cycles (their signature fetch replaces them) and, in emulation
    /// mode, `set_b` pushes P with the B flag set.
    pub(crate) fn service_interrupt<B: CpuBus>(
        &mut self,
        bus: &mut B,
        vec_native: u16,
        vec_emu: u16,
        set_b: bool,
        software: bool,
    ) {
        if !software {
            bus.idle();
            bus.idle();
        }
        if !self.emulation {
            self.push8(bus, self.pbr);
        }
        self.push16(bus, self.pc);
        let mut p = self.p.0;
        if self.emulation {
            p = if set_b { p | 0x30 } else { (p | 0x20) & !0x10 };
        }
        self.push8(bus, p);
        self.p.set_i(true);
        self.p.set_d(false);
        self.pbr = 0;
        let vec = if self.emulation { vec_emu } else { vec_native };
        let lo = bus.read(vec as u32) as u16;
        let hi = bus.read(vec.wrapping_add(1) as u32) as u16;
        self.pc = lo | (hi << 8);
    }
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat 24-bit address space; every access is free of timing side effects.
    struct FlatBus {
        ram: Vec<u8>,
    }
    impl FlatBus {
        fn new() -> Self {
            FlatBus { ram: vec![0; 0x100_0000] }
        }
        fn load(&mut self, addr: u32, bytes: &[u8]) {
            self.ram[addr as usize..addr as usize + bytes.len()].copy_from_slice(bytes);
        }
    }
    impl CpuBus for FlatBus {
        fn read(&mut self, addr: u32) -> u8 {
            self.ram[(addr & 0xFF_FFFF) as usize]
        }
        fn write(&mut self, addr: u32, value: u8) {
            self.ram[(addr & 0xFF_FFFF) as usize] = value;
        }
        fn idle(&mut self) {}
        fn take_nmi(&mut self) -> bool {
            false
        }
        fn irq_level(&mut self) -> bool {
            false
        }
    }

    #[test]
    fn executes_native_switch_and_16bit_adc() {
        let mut bus = FlatBus::new();
        // Reset vector -> $8000.
        bus.load(0x00FFFC, &[0x00, 0x80]);
        bus.load(
            0x008000,
            &[
                0x18, // CLC
                0xFB, // XCE  (E=1,C=0 -> native, C=1)
                0xC2, 0x30, // REP #$30 (16-bit A/X/Y)
                0x18, // CLC
                0xA9, 0x34, 0x12, // LDA #$1234
                0x69, 0x11, 0x11, // ADC #$1111 -> $2345
                0x8D, 0x00, 0x00, // STA $0000
                0xDB, // STP
            ],
        );
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        assert_eq!(cpu.pc, 0x8000);
        for _ in 0..32 {
            cpu.step(&mut bus);
            if cpu.stopped {
                break;
            }
        }
        assert!(!cpu.emulation, "XCE must have entered native mode");
        assert!(!cpu.p.m(), "REP must have cleared M");
        assert_eq!(cpu.a, 0x2345);
        assert_eq!(bus.read(0x000000), 0x45);
        assert_eq!(bus.read(0x000001), 0x23);
        assert!(!cpu.p.c() && !cpu.p.z() && !cpu.p.n());
    }

    #[test]
    fn jsr_rts_roundtrip() {
        let mut bus = FlatBus::new();
        bus.load(0x00FFFC, &[0x00, 0x80]);
        // $8000: JSR $8005 ; $8003: STP ; $8005: INC A? emulation 8-bit: LDA #$01; RTS
        bus.load(
            0x008000,
            &[
                0x20, 0x05, 0x80, // JSR $8005
                0xDB, // STP (return lands here after RTS -> next opcode)
                0x00, // padding
                0xA9, 0x42, // $8005 LDA #$42
                0x60, // RTS
            ],
        );
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        for _ in 0..16 {
            cpu.step(&mut bus);
            if cpu.stopped {
                break;
            }
        }
        assert_eq!(cpu.a & 0xFF, 0x42);
        assert_eq!(cpu.pc, 0x8004); // returned to instruction after JSR, then STP fetched
    }

    /// Fetch the opcode at PBR:PC and execute it, bypassing the interrupt path.
    fn run_one(cpu: &mut Cpu, bus: &mut FlatBus) {
        let op = cpu.fetch8(bus);
        cpu.execute(bus, op);
    }

    #[test]
    fn native_stack_wraps_full_16bit() {
        // Native mode 16-bit PHA at S=$0000: full 16-bit stack, no page-1 confine.
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.emulation = false;
        cpu.p.set_m(false);
        cpu.s = 0x0000;
        cpu.a = 0x1234;
        cpu.pbr = 0;
        cpu.pc = 0x8000;
        bus.load(0x008000, &[0x48]); // PHA
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.s, 0xFFFE);
        assert_eq!(bus.read(0x000000), 0x12); // high byte pushed first
        assert_eq!(bus.read(0x00FFFF), 0x34); // low byte wraps below $0000
    }

    #[test]
    fn emu_new_op_pea_does_not_page1_wrap() {
        // Emulation mode PEA at S=$0100: hi→$000100, lo→$0000FF (no page-1 wrap),
        // then SH forced back to $01 (ref example: S=$0100 → S=$01FE).
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.s = 0x0100;
        cpu.pbr = 0;
        cpu.pc = 0x8000;
        bus.load(0x008000, &[0xF4, 0xCD, 0xAB]); // PEA #$ABCD
        run_one(&mut cpu, &mut bus);
        assert_eq!(bus.read(0x000100), 0xAB);
        assert_eq!(bus.read(0x0000FF), 0xCD);
        assert_eq!(cpu.s, 0x01FE);
    }

    #[test]
    fn emu_old_op_pha_still_page1_wraps() {
        // Emulation mode PHA at S=$0100 must confine to page 1: S wraps to $01FF.
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.s = 0x0100;
        cpu.a = 0x42;
        cpu.pbr = 0;
        cpu.pc = 0x8000;
        bus.load(0x008000, &[0x48]); // PHA (8-bit in emulation)
        run_one(&mut cpu, &mut bus);
        assert_eq!(bus.read(0x000100), 0x42);
        assert_eq!(cpu.s, 0x01FF);
    }

    #[test]
    fn emu_stack_relative_crosses_page_no_wrap() {
        // Stack-relative is a "new" mode: S+off is a 16-bit bank-0 address that
        // does not page-wrap. LDA $02,S at S=$01FF reads $000201, not $000101.
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.s = 0x01FF;
        cpu.pbr = 0;
        cpu.pc = 0x8000;
        bus.load(0x008000, &[0xA3, 0x02]); // LDA $02,S
        bus.load(0x000201, &[0x77]);
        bus.load(0x000101, &[0x55]); // would be read if it wrongly wrapped
        run_one(&mut cpu, &mut bus);
        assert_eq!(cpu.a & 0xFF, 0x77);
    }

    #[test]
    fn jsl_rtl_roundtrip_native() {
        // JSL pushes PBR + 16-bit return; RTL restores both. Verify across banks.
        let mut bus = FlatBus::new();
        let mut cpu = Cpu::new();
        cpu.emulation = false;
        cpu.s = 0x1FFF;
        cpu.pbr = 0x12;
        cpu.pc = 0x8000;
        bus.load(0x128000, &[0x22, 0x34, 0x12, 0x7E]); // JSL $7E1234
        bus.load(0x7E1234, &[0x6B]); // RTL
        run_one(&mut cpu, &mut bus); // JSL
        assert_eq!(cpu.pbr, 0x7E);
        assert_eq!(cpu.pc, 0x1234);
        assert_eq!(cpu.s, 0x1FFC); // pushed PBR + 2 return bytes
        run_one(&mut cpu, &mut bus); // RTL
        assert_eq!(cpu.pbr, 0x12);
        assert_eq!(cpu.pc, 0x8004); // return addr ($8003, last JSL byte) + 1
        assert_eq!(cpu.s, 0x1FFF);
    }

    #[test]
    fn flags_roundtrip() {
        let mut p = Flags(0);
        p.set_n(true);
        p.set_c(true);
        assert_eq!(p.0, 0x81);
        assert!(p.n() && p.c() && !p.z());
        p.set_n(false);
        assert_eq!(p.0, 0x01);
    }
}
