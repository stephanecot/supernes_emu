//! SPC700 CPU core. Executes the full 256-opcode set over a private 64 KB bus
//! provided through the `Spc700Bus` trait (the $F0-$FF I/O overlay, timers, DSP
//! ports and IPL ROM live in the bus implementation, not here).
//!
//! Cycle counts, flag rules and addressing modes are transcribed from
//! references/apu.md (nesdev S-SMP / SPC-700 instruction set). 1 CPU cycle =
//! 2 SMP clocks = 1/1_024_000 s.

/// Memory access seen by the SPC700. The implementor decodes the I/O overlay,
/// IPL ROM and timers; the core only issues plain byte reads/writes.
pub trait Spc700Bus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, val: u8);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AluOp {
    Or,
    And,
    Eor,
    Adc,
    Sbc,
    Cmp,
}

#[derive(Clone, Copy)]
enum ShOp {
    Asl,
    Lsr,
    Rol,
    Ror,
    Inc,
    Dec,
}

/// Addressing modes that resolve to a 16-bit ARAM address.
#[derive(Clone, Copy)]
enum Am {
    Dp,
    DpX,
    DpY,
    Abs,
    AbsX,
    AbsY,
    IndX,
    IndY,
    XPtr,
}

pub struct Spc700 {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    // PSW flags (bit 7..0 = N V P B H I Z C).
    pub n: bool,
    pub v: bool,
    pub p: bool,
    pub b: bool,
    pub h: bool,
    pub i: bool,
    pub z: bool,
    pub c: bool,
    /// Set by SLEEP/STOP — the CPU halts forever (no wake source on the SNES).
    pub stopped: bool,
}

impl Spc700 {
    pub fn new() -> Self {
        Spc700 {
            a: 0,
            x: 0,
            y: 0,
            sp: 0,
            pc: 0,
            n: false,
            v: false,
            p: false,
            b: false,
            h: false,
            i: false,
            z: false,
            c: false,
            stopped: false,
        }
    }

    /// Fetch the reset vector from $FFFE/$FFFF and jump there.
    pub fn reset<B: Spc700Bus>(&mut self, bus: &mut B) {
        self.a = 0;
        self.x = 0;
        self.y = 0;
        self.sp = 0;
        self.set_psw(0);
        self.stopped = false;
        let lo = bus.read(0xFFFE) as u16;
        let hi = bus.read(0xFFFF) as u16;
        self.pc = lo | (hi << 8);
    }

    // --- PSW pack/unpack ---
    fn psw(&self) -> u8 {
        (self.n as u8) << 7
            | (self.v as u8) << 6
            | (self.p as u8) << 5
            | (self.b as u8) << 4
            | (self.h as u8) << 3
            | (self.i as u8) << 2
            | (self.z as u8) << 1
            | (self.c as u8)
    }

    fn set_psw(&mut self, v: u8) {
        self.n = v & 0x80 != 0;
        self.v = v & 0x40 != 0;
        self.p = v & 0x20 != 0;
        self.b = v & 0x10 != 0;
        self.h = v & 0x08 != 0;
        self.i = v & 0x04 != 0;
        self.z = v & 0x02 != 0;
        self.c = v & 0x01 != 0;
    }

    #[inline]
    fn set_nz(&mut self, v: u8) {
        self.n = v & 0x80 != 0;
        self.z = v == 0;
    }

    #[inline]
    fn ya(&self) -> u16 {
        (self.y as u16) << 8 | self.a as u16
    }

    #[inline]
    fn set_ya(&mut self, v: u16) {
        self.a = v as u8;
        self.y = (v >> 8) as u8;
    }

    /// Direct-page base: $00xx when P=0, $01xx when P=1.
    #[inline]
    fn dp(&self, d: u8) -> u16 {
        (if self.p { 0x0100 } else { 0 }) | d as u16
    }

    // --- fetch helpers ---
    #[inline]
    fn fetch8<B: Spc700Bus>(&mut self, bus: &mut B) -> u8 {
        let v = bus.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }

    #[inline]
    fn fetch16<B: Spc700Bus>(&mut self, bus: &mut B) -> u16 {
        let lo = self.fetch8(bus) as u16;
        let hi = self.fetch8(bus) as u16;
        lo | (hi << 8)
    }

    /// Read a 16-bit word from the direct page; the high byte wraps within the
    /// page ((d+1) & $FF).
    fn read_word_dp<B: Spc700Bus>(&mut self, bus: &mut B, d: u8) -> u16 {
        let lo = bus.read(self.dp(d)) as u16;
        let hi = bus.read(self.dp(d.wrapping_add(1))) as u16;
        lo | (hi << 8)
    }

    fn write_word_dp<B: Spc700Bus>(&mut self, bus: &mut B, d: u8, w: u16) {
        bus.write(self.dp(d), w as u8);
        bus.write(self.dp(d.wrapping_add(1)), (w >> 8) as u8);
    }

    // --- stack ---
    #[inline]
    fn push8<B: Spc700Bus>(&mut self, bus: &mut B, v: u8) {
        bus.write(0x0100 | self.sp as u16, v);
        self.sp = self.sp.wrapping_sub(1);
    }

    #[inline]
    fn pull8<B: Spc700Bus>(&mut self, bus: &mut B) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        bus.read(0x0100 | self.sp as u16)
    }

    fn push16<B: Spc700Bus>(&mut self, bus: &mut B, v: u16) {
        self.push8(bus, (v >> 8) as u8);
        self.push8(bus, v as u8);
    }

    fn pull16<B: Spc700Bus>(&mut self, bus: &mut B) -> u16 {
        let lo = self.pull8(bus) as u16;
        let hi = self.pull8(bus) as u16;
        lo | (hi << 8)
    }

    // --- addressing ---
    fn addr<B: Spc700Bus>(&mut self, bus: &mut B, mode: Am) -> u16 {
        match mode {
            Am::Dp => {
                let d = self.fetch8(bus);
                self.dp(d)
            }
            Am::DpX => {
                let d = self.fetch8(bus);
                self.dp(d.wrapping_add(self.x))
            }
            Am::DpY => {
                let d = self.fetch8(bus);
                self.dp(d.wrapping_add(self.y))
            }
            Am::Abs => self.fetch16(bus),
            Am::AbsX => self.fetch16(bus).wrapping_add(self.x as u16),
            Am::AbsY => self.fetch16(bus).wrapping_add(self.y as u16),
            Am::IndX => {
                let d = self.fetch8(bus);
                self.read_word_dp(bus, d.wrapping_add(self.x))
            }
            Am::IndY => {
                let d = self.fetch8(bus);
                self.read_word_dp(bus, d).wrapping_add(self.y as u16)
            }
            Am::XPtr => self.dp(self.x),
        }
    }

    fn am_read<B: Spc700Bus>(&mut self, bus: &mut B, mode: Am) -> u8 {
        let a = self.addr(bus, mode);
        bus.read(a)
    }

    // --- ALU primitives ---
    fn op_adc(&mut self, a: u8, b: u8) -> u8 {
        let carry = self.c as u16;
        let r = a as u16 + b as u16 + carry;
        self.h = ((a & 0x0f) as u16 + (b & 0x0f) as u16 + carry) > 0x0f;
        self.v = (!(a ^ b) & (a ^ r as u8) & 0x80) != 0;
        self.c = r > 0xff;
        let r8 = r as u8;
        self.set_nz(r8);
        r8
    }

    #[inline]
    fn op_sbc(&mut self, a: u8, b: u8) -> u8 {
        // Implemented as ADC of the one's complement; H/V/C fall out correctly.
        self.op_adc(a, !b)
    }

    fn op_cmp(&mut self, a: u8, b: u8) {
        let r = a.wrapping_sub(b);
        self.c = a >= b;
        self.set_nz(r);
    }

    fn alu(&mut self, op: AluOp, a: u8, b: u8) -> u8 {
        match op {
            AluOp::Or => {
                let r = a | b;
                self.set_nz(r);
                r
            }
            AluOp::And => {
                let r = a & b;
                self.set_nz(r);
                r
            }
            AluOp::Eor => {
                let r = a ^ b;
                self.set_nz(r);
                r
            }
            AluOp::Adc => self.op_adc(a, b),
            AluOp::Sbc => self.op_sbc(a, b),
            AluOp::Cmp => {
                self.op_cmp(a, b);
                a
            }
        }
    }

    fn shift(&mut self, op: ShOp, v: u8) -> u8 {
        let r = match op {
            ShOp::Asl => {
                self.c = v & 0x80 != 0;
                v << 1
            }
            ShOp::Lsr => {
                self.c = v & 0x01 != 0;
                v >> 1
            }
            ShOp::Rol => {
                let carry = self.c as u8;
                self.c = v & 0x80 != 0;
                (v << 1) | carry
            }
            ShOp::Ror => {
                let carry = self.c as u8;
                self.c = v & 0x01 != 0;
                (v >> 1) | (carry << 7)
            }
            ShOp::Inc => v.wrapping_add(1),
            ShOp::Dec => v.wrapping_sub(1),
        };
        self.set_nz(r);
        r
    }

    // --- 16-bit ALU (via chained 8-bit ops per higan) ---
    fn op_addw(&mut self, x: u16, y: u16) -> u16 {
        self.c = false;
        let lo = self.op_adc(x as u8, y as u8);
        let hi = self.op_adc((x >> 8) as u8, (y >> 8) as u8);
        let z = lo as u16 | (hi as u16) << 8;
        self.z = z == 0;
        z
    }

    fn op_subw(&mut self, x: u16, y: u16) -> u16 {
        self.c = true;
        let lo = self.op_sbc(x as u8, y as u8);
        let hi = self.op_sbc((x >> 8) as u8, (y >> 8) as u8);
        let z = lo as u16 | (hi as u16) << 8;
        self.z = z == 0;
        z
    }

    fn op_cmpw(&mut self, x: u16, y: u16) {
        let r = x as i32 - y as i32;
        self.c = r >= 0;
        let w = r as u16;
        self.n = w & 0x8000 != 0;
        self.z = w == 0;
    }

    // --- generic instruction helpers ---
    fn alu_a<B: Spc700Bus>(&mut self, bus: &mut B, op: AluOp, mode: Am) {
        let m = self.am_read(bus, mode);
        let r = self.alu(op, self.a, m);
        if op != AluOp::Cmp {
            self.a = r;
        }
    }

    fn alu_a_imm<B: Spc700Bus>(&mut self, bus: &mut B, op: AluOp) {
        let m = self.fetch8(bus);
        let r = self.alu(op, self.a, m);
        if op != AluOp::Cmp {
            self.a = r;
        }
    }

    /// `OP (dest_dp), (src_dp)` — memory order is source byte, then dest byte.
    fn alu_dd<B: Spc700Bus>(&mut self, bus: &mut B, op: AluOp) {
        let sd = self.fetch8(bus);
        let sval = bus.read(self.dp(sd));
        let dd = self.fetch8(bus);
        let da = self.dp(dd);
        let dval = bus.read(da);
        let r = self.alu(op, dval, sval);
        if op != AluOp::Cmp {
            bus.write(da, r);
        }
    }

    /// `OP (dest_dp), #imm` — memory order is immediate byte, then dest byte.
    fn alu_di<B: Spc700Bus>(&mut self, bus: &mut B, op: AluOp) {
        let imm = self.fetch8(bus);
        let dd = self.fetch8(bus);
        let da = self.dp(dd);
        let dval = bus.read(da);
        let r = self.alu(op, dval, imm);
        if op != AluOp::Cmp {
            bus.write(da, r);
        }
    }

    fn alu_xy<B: Spc700Bus>(&mut self, bus: &mut B, op: AluOp) {
        let xa = self.dp(self.x);
        let ya = self.dp(self.y);
        let xv = bus.read(xa);
        let yv = bus.read(ya);
        let r = self.alu(op, xv, yv);
        if op != AluOp::Cmp {
            bus.write(xa, r);
        }
    }

    fn rmw<B: Spc700Bus>(&mut self, bus: &mut B, op: ShOp, mode: Am) {
        let a = self.addr(bus, mode);
        let v = bus.read(a);
        let r = self.shift(op, v);
        bus.write(a, r);
    }

    fn branch<B: Spc700Bus>(&mut self, bus: &mut B, cond: bool, base: u32) -> u32 {
        let off = self.fetch8(bus) as i8;
        if cond {
            self.pc = self.pc.wrapping_add(off as u16);
            base + 2
        } else {
            base
        }
    }

    fn set1<B: Spc700Bus>(&mut self, bus: &mut B, bit: u8) {
        let a = self.addr(bus, Am::Dp);
        let v = bus.read(a) | (1 << bit);
        bus.write(a, v);
    }

    fn clr1<B: Spc700Bus>(&mut self, bus: &mut B, bit: u8) {
        let a = self.addr(bus, Am::Dp);
        let v = bus.read(a) & !(1 << bit);
        bus.write(a, v);
    }

    fn bbs<B: Spc700Bus>(&mut self, bus: &mut B, bit: u8, want_set: bool) -> u32 {
        let a = self.addr(bus, Am::Dp);
        let v = bus.read(a);
        let off = self.fetch8(bus) as i8;
        let bit_set = v & (1 << bit) != 0;
        if bit_set == want_set {
            self.pc = self.pc.wrapping_add(off as u16);
            7
        } else {
            5
        }
    }

    /// Decode `m.b`: operand bits 0-12 = address, bits 13-15 = bit number.
    fn abs_bit<B: Spc700Bus>(&mut self, bus: &mut B) -> (u16, u8) {
        let v = self.fetch16(bus);
        (v & 0x1fff, (v >> 13) as u8)
    }

    fn daa(&mut self) {
        if self.c || self.a > 0x99 {
            self.a = self.a.wrapping_add(0x60);
            self.c = true;
        }
        if self.h || (self.a & 0x0f) > 0x09 {
            self.a = self.a.wrapping_add(0x06);
        }
        self.set_nz(self.a);
    }

    fn das(&mut self) {
        if !self.c || self.a > 0x99 {
            self.a = self.a.wrapping_sub(0x60);
            self.c = false;
        }
        if !self.h || (self.a & 0x0f) > 0x09 {
            self.a = self.a.wrapping_sub(0x06);
        }
        self.set_nz(self.a);
    }

    fn div(&mut self) {
        let ya = self.ya();
        let x = self.x as u16;
        self.v = self.y >= self.x;
        self.h = (self.x & 0x0f) <= (self.y & 0x0f);
        if (self.y as u16) < (x << 1) {
            self.a = (ya / x) as u8;
            self.y = (ya % x) as u8;
        } else {
            self.a = (255 - (ya - (x << 9)) / (256 - x)) as u8;
            self.y = (x + (ya - (x << 9)) % (256 - x)) as u8;
        }
        self.set_nz(self.a);
    }

    /// Execute one instruction. Returns the CPU-cycle count consumed.
    pub fn step<B: Spc700Bus>(&mut self, bus: &mut B) -> u32 {
        if self.stopped {
            return 1;
        }
        let op = self.fetch8(bus);
        match op {
            // --- OR ---
            0x04 => {
                self.alu_a(bus, AluOp::Or, Am::Dp);
                3
            }
            0x05 => {
                self.alu_a(bus, AluOp::Or, Am::Abs);
                4
            }
            0x06 => {
                self.alu_a(bus, AluOp::Or, Am::XPtr);
                3
            }
            0x07 => {
                self.alu_a(bus, AluOp::Or, Am::IndX);
                6
            }
            0x08 => {
                self.alu_a_imm(bus, AluOp::Or);
                2
            }
            0x09 => {
                self.alu_dd(bus, AluOp::Or);
                6
            }
            0x14 => {
                self.alu_a(bus, AluOp::Or, Am::DpX);
                4
            }
            0x15 => {
                self.alu_a(bus, AluOp::Or, Am::AbsX);
                5
            }
            0x16 => {
                self.alu_a(bus, AluOp::Or, Am::AbsY);
                5
            }
            0x17 => {
                self.alu_a(bus, AluOp::Or, Am::IndY);
                6
            }
            0x18 => {
                self.alu_di(bus, AluOp::Or);
                5
            }
            0x19 => {
                self.alu_xy(bus, AluOp::Or);
                5
            }
            // --- AND ---
            0x24 => {
                self.alu_a(bus, AluOp::And, Am::Dp);
                3
            }
            0x25 => {
                self.alu_a(bus, AluOp::And, Am::Abs);
                4
            }
            0x26 => {
                self.alu_a(bus, AluOp::And, Am::XPtr);
                3
            }
            0x27 => {
                self.alu_a(bus, AluOp::And, Am::IndX);
                6
            }
            0x28 => {
                self.alu_a_imm(bus, AluOp::And);
                2
            }
            0x29 => {
                self.alu_dd(bus, AluOp::And);
                6
            }
            0x34 => {
                self.alu_a(bus, AluOp::And, Am::DpX);
                4
            }
            0x35 => {
                self.alu_a(bus, AluOp::And, Am::AbsX);
                5
            }
            0x36 => {
                self.alu_a(bus, AluOp::And, Am::AbsY);
                5
            }
            0x37 => {
                self.alu_a(bus, AluOp::And, Am::IndY);
                6
            }
            0x38 => {
                self.alu_di(bus, AluOp::And);
                5
            }
            0x39 => {
                self.alu_xy(bus, AluOp::And);
                5
            }
            // --- EOR ---
            0x44 => {
                self.alu_a(bus, AluOp::Eor, Am::Dp);
                3
            }
            0x45 => {
                self.alu_a(bus, AluOp::Eor, Am::Abs);
                4
            }
            0x46 => {
                self.alu_a(bus, AluOp::Eor, Am::XPtr);
                3
            }
            0x47 => {
                self.alu_a(bus, AluOp::Eor, Am::IndX);
                6
            }
            0x48 => {
                self.alu_a_imm(bus, AluOp::Eor);
                2
            }
            0x49 => {
                self.alu_dd(bus, AluOp::Eor);
                6
            }
            0x54 => {
                self.alu_a(bus, AluOp::Eor, Am::DpX);
                4
            }
            0x55 => {
                self.alu_a(bus, AluOp::Eor, Am::AbsX);
                5
            }
            0x56 => {
                self.alu_a(bus, AluOp::Eor, Am::AbsY);
                5
            }
            0x57 => {
                self.alu_a(bus, AluOp::Eor, Am::IndY);
                6
            }
            0x58 => {
                self.alu_di(bus, AluOp::Eor);
                5
            }
            0x59 => {
                self.alu_xy(bus, AluOp::Eor);
                5
            }
            // --- CMP A ---
            0x64 => {
                self.alu_a(bus, AluOp::Cmp, Am::Dp);
                3
            }
            0x65 => {
                self.alu_a(bus, AluOp::Cmp, Am::Abs);
                4
            }
            0x66 => {
                self.alu_a(bus, AluOp::Cmp, Am::XPtr);
                3
            }
            0x67 => {
                self.alu_a(bus, AluOp::Cmp, Am::IndX);
                6
            }
            0x68 => {
                self.alu_a_imm(bus, AluOp::Cmp);
                2
            }
            0x69 => {
                self.alu_dd(bus, AluOp::Cmp);
                6
            }
            0x74 => {
                self.alu_a(bus, AluOp::Cmp, Am::DpX);
                4
            }
            0x75 => {
                self.alu_a(bus, AluOp::Cmp, Am::AbsX);
                5
            }
            0x76 => {
                self.alu_a(bus, AluOp::Cmp, Am::AbsY);
                5
            }
            0x77 => {
                self.alu_a(bus, AluOp::Cmp, Am::IndY);
                6
            }
            0x78 => {
                self.alu_di(bus, AluOp::Cmp);
                5
            }
            0x79 => {
                self.alu_xy(bus, AluOp::Cmp);
                5
            }
            // --- ADC ---
            0x84 => {
                self.alu_a(bus, AluOp::Adc, Am::Dp);
                3
            }
            0x85 => {
                self.alu_a(bus, AluOp::Adc, Am::Abs);
                4
            }
            0x86 => {
                self.alu_a(bus, AluOp::Adc, Am::XPtr);
                3
            }
            0x87 => {
                self.alu_a(bus, AluOp::Adc, Am::IndX);
                6
            }
            0x88 => {
                self.alu_a_imm(bus, AluOp::Adc);
                2
            }
            0x89 => {
                self.alu_dd(bus, AluOp::Adc);
                6
            }
            0x94 => {
                self.alu_a(bus, AluOp::Adc, Am::DpX);
                4
            }
            0x95 => {
                self.alu_a(bus, AluOp::Adc, Am::AbsX);
                5
            }
            0x96 => {
                self.alu_a(bus, AluOp::Adc, Am::AbsY);
                5
            }
            0x97 => {
                self.alu_a(bus, AluOp::Adc, Am::IndY);
                6
            }
            0x98 => {
                self.alu_di(bus, AluOp::Adc);
                5
            }
            0x99 => {
                self.alu_xy(bus, AluOp::Adc);
                5
            }
            // --- SBC ---
            0xA4 => {
                self.alu_a(bus, AluOp::Sbc, Am::Dp);
                3
            }
            0xA5 => {
                self.alu_a(bus, AluOp::Sbc, Am::Abs);
                4
            }
            0xA6 => {
                self.alu_a(bus, AluOp::Sbc, Am::XPtr);
                3
            }
            0xA7 => {
                self.alu_a(bus, AluOp::Sbc, Am::IndX);
                6
            }
            0xA8 => {
                self.alu_a_imm(bus, AluOp::Sbc);
                2
            }
            0xA9 => {
                self.alu_dd(bus, AluOp::Sbc);
                6
            }
            0xB4 => {
                self.alu_a(bus, AluOp::Sbc, Am::DpX);
                4
            }
            0xB5 => {
                self.alu_a(bus, AluOp::Sbc, Am::AbsX);
                5
            }
            0xB6 => {
                self.alu_a(bus, AluOp::Sbc, Am::AbsY);
                5
            }
            0xB7 => {
                self.alu_a(bus, AluOp::Sbc, Am::IndY);
                6
            }
            0xB8 => {
                self.alu_di(bus, AluOp::Sbc);
                5
            }
            0xB9 => {
                self.alu_xy(bus, AluOp::Sbc);
                5
            }
            // --- CMP X / CMP Y ---
            0xC8 => {
                let m = self.fetch8(bus);
                self.op_cmp(self.x, m);
                2
            }
            0x3E => {
                let m = self.am_read(bus, Am::Dp);
                self.op_cmp(self.x, m);
                3
            }
            0x1E => {
                let m = self.am_read(bus, Am::Abs);
                self.op_cmp(self.x, m);
                4
            }
            0xAD => {
                let m = self.fetch8(bus);
                self.op_cmp(self.y, m);
                2
            }
            0x7E => {
                let m = self.am_read(bus, Am::Dp);
                self.op_cmp(self.y, m);
                3
            }
            0x5E => {
                let m = self.am_read(bus, Am::Abs);
                self.op_cmp(self.y, m);
                4
            }
            // --- shifts / rmw on A ---
            0x1C => {
                self.a = self.shift(ShOp::Asl, self.a);
                2
            }
            0x3C => {
                self.a = self.shift(ShOp::Rol, self.a);
                2
            }
            0x5C => {
                self.a = self.shift(ShOp::Lsr, self.a);
                2
            }
            0x7C => {
                self.a = self.shift(ShOp::Ror, self.a);
                2
            }
            0x9C => {
                self.a = self.shift(ShOp::Dec, self.a);
                2
            }
            0xBC => {
                self.a = self.shift(ShOp::Inc, self.a);
                2
            }
            0x1D => {
                self.x = self.shift(ShOp::Dec, self.x);
                2
            }
            0x3D => {
                self.x = self.shift(ShOp::Inc, self.x);
                2
            }
            0xDC => {
                self.y = self.shift(ShOp::Dec, self.y);
                2
            }
            0xFC => {
                self.y = self.shift(ShOp::Inc, self.y);
                2
            }
            // --- shifts / rmw on memory ---
            0x0B => {
                self.rmw(bus, ShOp::Asl, Am::Dp);
                4
            }
            0x1B => {
                self.rmw(bus, ShOp::Asl, Am::DpX);
                5
            }
            0x0C => {
                self.rmw(bus, ShOp::Asl, Am::Abs);
                5
            }
            0x2B => {
                self.rmw(bus, ShOp::Rol, Am::Dp);
                4
            }
            0x3B => {
                self.rmw(bus, ShOp::Rol, Am::DpX);
                5
            }
            0x2C => {
                self.rmw(bus, ShOp::Rol, Am::Abs);
                5
            }
            0x4B => {
                self.rmw(bus, ShOp::Lsr, Am::Dp);
                4
            }
            0x5B => {
                self.rmw(bus, ShOp::Lsr, Am::DpX);
                5
            }
            0x4C => {
                self.rmw(bus, ShOp::Lsr, Am::Abs);
                5
            }
            0x6B => {
                self.rmw(bus, ShOp::Ror, Am::Dp);
                4
            }
            0x7B => {
                self.rmw(bus, ShOp::Ror, Am::DpX);
                5
            }
            0x6C => {
                self.rmw(bus, ShOp::Ror, Am::Abs);
                5
            }
            0x8B => {
                self.rmw(bus, ShOp::Dec, Am::Dp);
                4
            }
            0x9B => {
                self.rmw(bus, ShOp::Dec, Am::DpX);
                5
            }
            0x8C => {
                self.rmw(bus, ShOp::Dec, Am::Abs);
                5
            }
            0xAB => {
                self.rmw(bus, ShOp::Inc, Am::Dp);
                4
            }
            0xBB => {
                self.rmw(bus, ShOp::Inc, Am::DpX);
                5
            }
            0xAC => {
                self.rmw(bus, ShOp::Inc, Am::Abs);
                5
            }
            // --- 16-bit ---
            0x1A => {
                let d = self.fetch8(bus);
                let w = self.read_word_dp(bus, d).wrapping_sub(1);
                self.write_word_dp(bus, d, w);
                self.n = w & 0x8000 != 0;
                self.z = w == 0;
                6
            }
            0x3A => {
                let d = self.fetch8(bus);
                let w = self.read_word_dp(bus, d).wrapping_add(1);
                self.write_word_dp(bus, d, w);
                self.n = w & 0x8000 != 0;
                self.z = w == 0;
                6
            }
            0x5A => {
                let d = self.fetch8(bus);
                let m = self.read_word_dp(bus, d);
                self.op_cmpw(self.ya(), m);
                4
            }
            0x7A => {
                let d = self.fetch8(bus);
                let m = self.read_word_dp(bus, d);
                let r = self.op_addw(self.ya(), m);
                self.set_ya(r);
                5
            }
            0x9A => {
                let d = self.fetch8(bus);
                let m = self.read_word_dp(bus, d);
                let r = self.op_subw(self.ya(), m);
                self.set_ya(r);
                5
            }
            0xBA => {
                let d = self.fetch8(bus);
                let w = self.read_word_dp(bus, d);
                self.set_ya(w);
                self.n = w & 0x8000 != 0;
                self.z = w == 0;
                5
            }
            0xDA => {
                let d = self.fetch8(bus);
                let w = self.ya();
                self.write_word_dp(bus, d, w);
                5
            }
            // --- MUL / DIV ---
            0xCF => {
                let r = self.y as u16 * self.a as u16;
                self.a = r as u8;
                self.y = (r >> 8) as u8;
                self.set_nz(self.y);
                9
            }
            0x9E => {
                self.div();
                12
            }
            // --- DAA / DAS / XCN ---
            0xDF => {
                self.daa();
                3
            }
            0xBE => {
                self.das();
                3
            }
            0x9F => {
                self.a = (self.a >> 4) | (self.a << 4);
                self.set_nz(self.a);
                5
            }
            // --- MOV loads (set N/Z) ---
            0xE8 => {
                self.a = self.fetch8(bus);
                self.set_nz(self.a);
                2
            }
            0xE6 => {
                self.a = self.am_read(bus, Am::XPtr);
                self.set_nz(self.a);
                3
            }
            0xBF => {
                self.a = bus.read(self.dp(self.x));
                self.x = self.x.wrapping_add(1);
                self.set_nz(self.a);
                4
            }
            0xE4 => {
                self.a = self.am_read(bus, Am::Dp);
                self.set_nz(self.a);
                3
            }
            0xF4 => {
                self.a = self.am_read(bus, Am::DpX);
                self.set_nz(self.a);
                4
            }
            0xE5 => {
                self.a = self.am_read(bus, Am::Abs);
                self.set_nz(self.a);
                4
            }
            0xF5 => {
                self.a = self.am_read(bus, Am::AbsX);
                self.set_nz(self.a);
                5
            }
            0xF6 => {
                self.a = self.am_read(bus, Am::AbsY);
                self.set_nz(self.a);
                5
            }
            0xE7 => {
                self.a = self.am_read(bus, Am::IndX);
                self.set_nz(self.a);
                6
            }
            0xF7 => {
                self.a = self.am_read(bus, Am::IndY);
                self.set_nz(self.a);
                6
            }
            0xCD => {
                self.x = self.fetch8(bus);
                self.set_nz(self.x);
                2
            }
            0xF8 => {
                self.x = self.am_read(bus, Am::Dp);
                self.set_nz(self.x);
                3
            }
            0xF9 => {
                self.x = self.am_read(bus, Am::DpY);
                self.set_nz(self.x);
                4
            }
            0xE9 => {
                self.x = self.am_read(bus, Am::Abs);
                self.set_nz(self.x);
                4
            }
            0x8D => {
                self.y = self.fetch8(bus);
                self.set_nz(self.y);
                2
            }
            0xEB => {
                self.y = self.am_read(bus, Am::Dp);
                self.set_nz(self.y);
                3
            }
            0xFB => {
                self.y = self.am_read(bus, Am::DpX);
                self.set_nz(self.y);
                4
            }
            0xEC => {
                self.y = self.am_read(bus, Am::Abs);
                self.set_nz(self.y);
                4
            }
            // --- MOV register/register ---
            0x7D => {
                self.a = self.x;
                self.set_nz(self.a);
                2
            }
            0xDD => {
                self.a = self.y;
                self.set_nz(self.a);
                2
            }
            0x5D => {
                self.x = self.a;
                self.set_nz(self.x);
                2
            }
            0xFD => {
                self.y = self.a;
                self.set_nz(self.y);
                2
            }
            0x9D => {
                self.x = self.sp;
                self.set_nz(self.x);
                2
            }
            0xBD => {
                self.sp = self.x;
                2
            }
            // --- MOV stores (no flags) ---
            0xC4 => {
                let a = self.addr(bus, Am::Dp);
                bus.write(a, self.a);
                4
            }
            0xD8 => {
                let a = self.addr(bus, Am::Dp);
                bus.write(a, self.x);
                4
            }
            0xCB => {
                let a = self.addr(bus, Am::Dp);
                bus.write(a, self.y);
                4
            }
            0xD4 => {
                let a = self.addr(bus, Am::DpX);
                bus.write(a, self.a);
                5
            }
            0xDB => {
                let a = self.addr(bus, Am::DpX);
                bus.write(a, self.y);
                5
            }
            0xD9 => {
                let a = self.addr(bus, Am::DpY);
                bus.write(a, self.x);
                5
            }
            0xC5 => {
                let a = self.addr(bus, Am::Abs);
                bus.write(a, self.a);
                5
            }
            0xC9 => {
                let a = self.addr(bus, Am::Abs);
                bus.write(a, self.x);
                5
            }
            0xCC => {
                let a = self.addr(bus, Am::Abs);
                bus.write(a, self.y);
                5
            }
            0xD5 => {
                let a = self.addr(bus, Am::AbsX);
                bus.write(a, self.a);
                6
            }
            0xD6 => {
                let a = self.addr(bus, Am::AbsY);
                bus.write(a, self.a);
                6
            }
            0xC6 => {
                let a = self.addr(bus, Am::XPtr);
                bus.write(a, self.a);
                4
            }
            0xAF => {
                bus.write(self.dp(self.x), self.a);
                self.x = self.x.wrapping_add(1);
                4
            }
            0xC7 => {
                let a = self.addr(bus, Am::IndX);
                bus.write(a, self.a);
                7
            }
            0xD7 => {
                let a = self.addr(bus, Am::IndY);
                bus.write(a, self.a);
                7
            }
            0xFA => {
                let sd = self.fetch8(bus);
                let val = bus.read(self.dp(sd));
                let dd = self.fetch8(bus);
                bus.write(self.dp(dd), val);
                5
            }
            0x8F => {
                let imm = self.fetch8(bus);
                let dd = self.fetch8(bus);
                bus.write(self.dp(dd), imm);
                5
            }
            // --- SET1 / CLR1 (bit = op>>5) ---
            0x02 => {
                self.set1(bus, 0);
                4
            }
            0x22 => {
                self.set1(bus, 1);
                4
            }
            0x42 => {
                self.set1(bus, 2);
                4
            }
            0x62 => {
                self.set1(bus, 3);
                4
            }
            0x82 => {
                self.set1(bus, 4);
                4
            }
            0xA2 => {
                self.set1(bus, 5);
                4
            }
            0xC2 => {
                self.set1(bus, 6);
                4
            }
            0xE2 => {
                self.set1(bus, 7);
                4
            }
            0x12 => {
                self.clr1(bus, 0);
                4
            }
            0x32 => {
                self.clr1(bus, 1);
                4
            }
            0x52 => {
                self.clr1(bus, 2);
                4
            }
            0x72 => {
                self.clr1(bus, 3);
                4
            }
            0x92 => {
                self.clr1(bus, 4);
                4
            }
            0xB2 => {
                self.clr1(bus, 5);
                4
            }
            0xD2 => {
                self.clr1(bus, 6);
                4
            }
            0xF2 => {
                self.clr1(bus, 7);
                4
            }
            // --- BBS / BBC (bit = op>>5) ---
            0x03 => self.bbs(bus, 0, true),
            0x23 => self.bbs(bus, 1, true),
            0x43 => self.bbs(bus, 2, true),
            0x63 => self.bbs(bus, 3, true),
            0x83 => self.bbs(bus, 4, true),
            0xA3 => self.bbs(bus, 5, true),
            0xC3 => self.bbs(bus, 6, true),
            0xE3 => self.bbs(bus, 7, true),
            0x13 => self.bbs(bus, 0, false),
            0x33 => self.bbs(bus, 1, false),
            0x53 => self.bbs(bus, 2, false),
            0x73 => self.bbs(bus, 3, false),
            0x93 => self.bbs(bus, 4, false),
            0xB3 => self.bbs(bus, 5, false),
            0xD3 => self.bbs(bus, 6, false),
            0xF3 => self.bbs(bus, 7, false),
            // --- carry / memory bit ops (m.b) ---
            0x0A => {
                let (a, b) = self.abs_bit(bus);
                let bit = bus.read(a) & (1 << b) != 0;
                self.c |= bit;
                5
            }
            0x2A => {
                let (a, b) = self.abs_bit(bus);
                let bit = bus.read(a) & (1 << b) != 0;
                self.c |= !bit;
                5
            }
            0x4A => {
                let (a, b) = self.abs_bit(bus);
                let bit = bus.read(a) & (1 << b) != 0;
                self.c &= bit;
                4
            }
            0x6A => {
                let (a, b) = self.abs_bit(bus);
                let bit = bus.read(a) & (1 << b) != 0;
                self.c &= !bit;
                4
            }
            0x8A => {
                let (a, b) = self.abs_bit(bus);
                let bit = bus.read(a) & (1 << b) != 0;
                self.c ^= bit;
                5
            }
            0xAA => {
                let (a, b) = self.abs_bit(bus);
                self.c = bus.read(a) & (1 << b) != 0;
                4
            }
            0xCA => {
                let (a, b) = self.abs_bit(bus);
                let mut v = bus.read(a);
                if self.c {
                    v |= 1 << b;
                } else {
                    v &= !(1 << b);
                }
                bus.write(a, v);
                6
            }
            0xEA => {
                let (a, b) = self.abs_bit(bus);
                let v = bus.read(a) ^ (1 << b);
                bus.write(a, v);
                5
            }
            // --- TSET1 / TCLR1 ---
            0x0E => {
                let a = self.fetch16(bus);
                let v = bus.read(a);
                self.set_nz(self.a.wrapping_sub(v));
                bus.write(a, v | self.a);
                6
            }
            0x4E => {
                let a = self.fetch16(bus);
                let v = bus.read(a);
                self.set_nz(self.a.wrapping_sub(v));
                bus.write(a, v & !self.a);
                6
            }
            // --- branches ---
            0x10 => self.branch(bus, !self.n, 2),
            0x30 => self.branch(bus, self.n, 2),
            0x50 => self.branch(bus, !self.v, 2),
            0x70 => self.branch(bus, self.v, 2),
            0x90 => self.branch(bus, !self.c, 2),
            0xB0 => self.branch(bus, self.c, 2),
            0xD0 => self.branch(bus, !self.z, 2),
            0xF0 => self.branch(bus, self.z, 2),
            0x2F => self.branch(bus, true, 2),
            // --- CBNE / DBNZ ---
            0x2E => {
                let a = self.addr(bus, Am::Dp);
                let v = bus.read(a);
                let off = self.fetch8(bus) as i8;
                if self.a != v {
                    self.pc = self.pc.wrapping_add(off as u16);
                    7
                } else {
                    5
                }
            }
            0xDE => {
                let a = self.addr(bus, Am::DpX);
                let v = bus.read(a);
                let off = self.fetch8(bus) as i8;
                if self.a != v {
                    self.pc = self.pc.wrapping_add(off as u16);
                    8
                } else {
                    6
                }
            }
            0x6E => {
                let a = self.addr(bus, Am::Dp);
                let v = bus.read(a).wrapping_sub(1);
                bus.write(a, v);
                let off = self.fetch8(bus) as i8;
                if v != 0 {
                    self.pc = self.pc.wrapping_add(off as u16);
                    7
                } else {
                    5
                }
            }
            0xFE => {
                self.y = self.y.wrapping_sub(1);
                let off = self.fetch8(bus) as i8;
                if self.y != 0 {
                    self.pc = self.pc.wrapping_add(off as u16);
                    6
                } else {
                    4
                }
            }
            // --- jumps / calls ---
            0x5F => {
                self.pc = self.fetch16(bus);
                3
            }
            0x1F => {
                let a = self.fetch16(bus).wrapping_add(self.x as u16);
                let lo = bus.read(a) as u16;
                let hi = bus.read(a.wrapping_add(1)) as u16;
                self.pc = lo | (hi << 8);
                6
            }
            0x3F => {
                let target = self.fetch16(bus);
                let ret = self.pc;
                self.push16(bus, ret);
                self.pc = target;
                8
            }
            0x4F => {
                let u = self.fetch8(bus) as u16;
                let ret = self.pc;
                self.push16(bus, ret);
                self.pc = 0xFF00 | u;
                6
            }
            0x6F => {
                self.pc = self.pull16(bus);
                5
            }
            0x7F => {
                let p = self.pull8(bus);
                self.set_psw(p);
                self.pc = self.pull16(bus);
                6
            }
            0x0F => {
                let ret = self.pc;
                self.push16(bus, ret);
                let p = self.psw();
                self.push8(bus, p);
                self.b = true;
                self.i = false;
                let lo = bus.read(0xFFDE) as u16;
                let hi = bus.read(0xFFDF) as u16;
                self.pc = lo | (hi << 8);
                8
            }
            // TCALL n: target = word at $FFDE - 2*n.
            0x01 | 0x11 | 0x21 | 0x31 | 0x41 | 0x51 | 0x61 | 0x71 | 0x81 | 0x91 | 0xA1 | 0xB1
            | 0xC1 | 0xD1 | 0xE1 | 0xF1 => {
                let n = (op >> 4) as u16;
                let tbl = 0xFFDEu16.wrapping_sub(2 * n);
                let lo = bus.read(tbl) as u16;
                let hi = bus.read(tbl.wrapping_add(1)) as u16;
                let ret = self.pc;
                self.push16(bus, ret);
                self.pc = lo | (hi << 8);
                8
            }
            // --- stack push/pop ---
            0x0D => {
                let p = self.psw();
                self.push8(bus, p);
                4
            }
            0x2D => {
                self.push8(bus, self.a);
                4
            }
            0x4D => {
                self.push8(bus, self.x);
                4
            }
            0x6D => {
                self.push8(bus, self.y);
                4
            }
            0x8E => {
                let p = self.pull8(bus);
                self.set_psw(p);
                4
            }
            0xAE => {
                self.a = self.pull8(bus);
                4
            }
            0xCE => {
                self.x = self.pull8(bus);
                4
            }
            0xEE => {
                self.y = self.pull8(bus);
                4
            }
            // --- flag ops ---
            0x20 => {
                self.p = false;
                2
            }
            0x40 => {
                self.p = true;
                2
            }
            0x60 => {
                self.c = false;
                2
            }
            0x80 => {
                self.c = true;
                2
            }
            0xE0 => {
                self.v = false;
                self.h = false;
                2
            }
            0xED => {
                self.c = !self.c;
                3
            }
            0xA0 => {
                self.i = true;
                3
            }
            0xC0 => {
                self.i = false;
                3
            }
            // --- misc ---
            0x00 => 2,
            0xEF => {
                self.stopped = true;
                3
            }
            0xFF => {
                self.stopped = true;
                2
            }
        }
    }
}

impl Default for Spc700 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestBus {
        ram: [u8; 0x10000],
    }
    impl TestBus {
        fn new() -> Self {
            TestBus { ram: [0; 0x10000] }
        }
    }
    impl Spc700Bus for TestBus {
        fn read(&mut self, addr: u16) -> u8 {
            self.ram[addr as usize]
        }
        fn write(&mut self, addr: u16, val: u8) {
            self.ram[addr as usize] = val;
        }
    }

    fn run(cpu: &mut Spc700, bus: &mut TestBus, prog: &[u8]) -> u32 {
        for (i, b) in prog.iter().enumerate() {
            bus.ram[0x0200 + i] = *b;
        }
        cpu.pc = 0x0200;
        cpu.step(bus)
    }

    #[test]
    fn mov_a_imm_sets_nz() {
        let mut cpu = Spc700::new();
        let mut bus = TestBus::new();
        let c = run(&mut cpu, &mut bus, &[0xE8, 0x00]); // MOV A,#$00
        assert_eq!(c, 2);
        assert_eq!(cpu.a, 0x00);
        assert!(cpu.z);
        assert!(!cpu.n);
        run(&mut cpu, &mut bus, &[0xE8, 0x80]); // MOV A,#$80
        assert!(cpu.n);
        assert!(!cpu.z);
    }

    #[test]
    fn adc_carry_and_overflow() {
        let mut cpu = Spc700::new();
        let mut bus = TestBus::new();
        cpu.a = 0x50;
        cpu.c = false;
        run(&mut cpu, &mut bus, &[0x88, 0x50]); // ADC A,#$50 -> $A0, V set (pos+pos=neg)
        assert_eq!(cpu.a, 0xA0);
        assert!(cpu.v);
        assert!(cpu.n);
        assert!(!cpu.c);
        // 0xFF + 0x01 = 0x00 carry out
        cpu.a = 0xFF;
        cpu.c = false;
        run(&mut cpu, &mut bus, &[0x88, 0x01]);
        assert_eq!(cpu.a, 0x00);
        assert!(cpu.c);
        assert!(cpu.z);
    }

    #[test]
    fn sbc_borrow() {
        let mut cpu = Spc700::new();
        let mut bus = TestBus::new();
        cpu.a = 0x05;
        cpu.c = true; // no borrow
        run(&mut cpu, &mut bus, &[0xA8, 0x03]); // SBC A,#$03 -> 0x02
        assert_eq!(cpu.a, 0x02);
        assert!(cpu.c); // no borrow out
        cpu.a = 0x00;
        cpu.c = true;
        run(&mut cpu, &mut bus, &[0xA8, 0x01]); // 0 - 1 = 0xFF, borrow
        assert_eq!(cpu.a, 0xFF);
        assert!(!cpu.c);
    }

    #[test]
    fn cmp_sets_carry_when_ge() {
        let mut cpu = Spc700::new();
        let mut bus = TestBus::new();
        cpu.a = 0x40;
        run(&mut cpu, &mut bus, &[0x68, 0x30]); // CMP A,#$30 -> A>=imm
        assert!(cpu.c);
        assert!(!cpu.z);
        cpu.a = 0x30;
        run(&mut cpu, &mut bus, &[0x68, 0x30]);
        assert!(cpu.c);
        assert!(cpu.z);
    }

    #[test]
    fn mul_ya() {
        let mut cpu = Spc700::new();
        let mut bus = TestBus::new();
        cpu.y = 0x10;
        cpu.a = 0x10;
        let c = run(&mut cpu, &mut bus, &[0xCF]); // YA = 0x100
        assert_eq!(c, 9);
        assert_eq!(cpu.y, 0x01);
        assert_eq!(cpu.a, 0x00);
        assert!(!cpu.z); // N/Z from Y (0x01)
    }

    #[test]
    fn div_ya_x() {
        let mut cpu = Spc700::new();
        let mut bus = TestBus::new();
        cpu.y = 0x00;
        cpu.a = 0x0A; // YA = 10
        cpu.x = 0x03;
        run(&mut cpu, &mut bus, &[0x9E]); // 10 / 3 = 3 rem 1
        assert_eq!(cpu.a, 3);
        assert_eq!(cpu.y, 1);
    }

    #[test]
    fn daa_adjust() {
        let mut cpu = Spc700::new();
        let mut bus = TestBus::new();
        // 0x09 + 0x01 = 0x0A binary; after DAA should be 0x10 BCD.
        cpu.a = 0x0A;
        cpu.c = false;
        cpu.h = false;
        run(&mut cpu, &mut bus, &[0xDF]);
        assert_eq!(cpu.a, 0x10);
    }

    #[test]
    fn branch_taken_extra_cycle() {
        let mut cpu = Spc700::new();
        let mut bus = TestBus::new();
        cpu.z = true;
        let c = run(&mut cpu, &mut bus, &[0xF0, 0x10]); // BEQ +$10 taken
        assert_eq!(c, 4);
        assert_eq!(cpu.pc, 0x0202u16.wrapping_add(0x10));
        cpu.z = false;
        let c = run(&mut cpu, &mut bus, &[0xF0, 0x10]); // not taken
        assert_eq!(c, 2);
    }

    #[test]
    fn movw_and_incw() {
        let mut cpu = Spc700::new();
        let mut bus = TestBus::new();
        bus.ram[0x0020] = 0xFF;
        bus.ram[0x0021] = 0x00; // word $00FF at dp $20
        run(&mut cpu, &mut bus, &[0x3A, 0x20]); // INCW $20 -> $0100
        assert_eq!(bus.ram[0x0020], 0x00);
        assert_eq!(bus.ram[0x0021], 0x01);
        run(&mut cpu, &mut bus, &[0xBA, 0x20]); // MOVW YA,$20
        assert_eq!(cpu.a, 0x00);
        assert_eq!(cpu.y, 0x01);
    }
}
