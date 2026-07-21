//! 65C816 CPU core. Register file, status flags, bus trait, reset sequence.
//! Instruction execution arrives in M1.

pub mod addressing;
pub mod algorithms;
pub mod ops;

/// Processor status register P. Bit layout: N V M X D I Z C (bit7..bit0).
/// In emulation mode, bit5 (M position) is unused and bit4 (X position) is the B flag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
