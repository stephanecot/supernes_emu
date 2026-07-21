//! 65C816 opcode dispatch: full 256-entry decode plus instruction bodies.

use super::addressing::{Ea, Pen};
use super::{algorithms, Cpu, CpuBus, Flags};

impl Cpu {
    // ---- Immediate operands ----

    fn imm_m<B: CpuBus>(&mut self, bus: &mut B) -> u16 {
        if self.m8() {
            self.fetch8(bus) as u16
        } else {
            self.fetch16(bus)
        }
    }

    fn imm_x<B: CpuBus>(&mut self, bus: &mut B) -> u16 {
        if self.x8() {
            self.fetch8(bus) as u16
        } else {
            self.fetch16(bus)
        }
    }

    // ---- ALU / load / compare cores (value already fetched) ----

    fn op_ora(&mut self, v: u16) {
        let r = self.a | v;
        self.set_a(r);
        self.set_nz_m(if self.m8() { r & 0x00FF } else { r });
    }

    fn op_and(&mut self, v: u16) {
        let r = self.a & v;
        self.set_a(r);
        self.set_nz_m(if self.m8() { r & 0x00FF } else { r });
    }

    fn op_eor(&mut self, v: u16) {
        let r = self.a ^ v;
        self.set_a(r);
        self.set_nz_m(if self.m8() { r & 0x00FF } else { r });
    }

    fn op_adc(&mut self, v: u16) {
        let eight = self.m8();
        let r = algorithms::adc(self.a, v, &mut self.p, eight);
        self.set_a(r);
    }

    fn op_sbc(&mut self, v: u16) {
        let eight = self.m8();
        let r = algorithms::sbc(self.a, v, &mut self.p, eight);
        self.set_a(r);
    }

    fn op_cmp(&mut self, v: u16) {
        let eight = self.m8();
        algorithms::cmp(self.a, v, &mut self.p, eight);
    }

    fn op_cpx(&mut self, v: u16) {
        let eight = self.x8();
        algorithms::cmp(self.x, v, &mut self.p, eight);
    }

    fn op_cpy(&mut self, v: u16) {
        let eight = self.x8();
        algorithms::cmp(self.y, v, &mut self.p, eight);
    }

    fn op_lda(&mut self, v: u16) {
        self.set_a(v);
        self.set_nz_m(v);
    }

    fn op_bit(&mut self, v: u16) {
        let (sign, mask) = if self.m8() { (0x0080, 0x00FF) } else { (0x8000, 0xFFFF) };
        self.p.set_z(self.a & v & mask == 0);
        self.p.set_n(v & sign != 0);
        self.p.set_v(v & (sign >> 1) != 0);
    }

    // ---- Memory read-modify-write ----

    fn rmw_m<B: CpuBus>(&mut self, bus: &mut B, ea: Ea, f: fn(u16, &mut Flags, bool) -> u16) {
        let v = self.load_m(bus, ea);
        bus.idle();
        let eight = self.m8();
        let r = f(v, &mut self.p, eight);
        self.store_m(bus, ea, r);
    }

    fn rmw_a<B: CpuBus>(&mut self, bus: &mut B, f: fn(u16, &mut Flags, bool) -> u16) {
        bus.idle();
        let eight = self.m8();
        let r = f(self.a, &mut self.p, eight);
        self.set_a(r);
    }

    fn op_tsb<B: CpuBus>(&mut self, bus: &mut B, ea: Ea) {
        let v = self.load_m(bus, ea);
        bus.idle();
        let mask = if self.m8() { 0x00FF } else { 0xFFFF };
        self.p.set_z(self.a & v & mask == 0);
        let r = v | (self.a & mask);
        self.store_m(bus, ea, r);
    }

    fn op_trb<B: CpuBus>(&mut self, bus: &mut B, ea: Ea) {
        let v = self.load_m(bus, ea);
        bus.idle();
        let mask = if self.m8() { 0x00FF } else { 0xFFFF };
        self.p.set_z(self.a & v & mask == 0);
        let r = v & !self.a & mask;
        self.store_m(bus, ea, r);
    }

    // ---- Stack width helpers ----

    fn push_m<B: CpuBus>(&mut self, bus: &mut B, v: u16) {
        if self.m8() {
            self.push8(bus, v as u8);
        } else {
            self.push16(bus, v);
        }
    }

    fn push_x<B: CpuBus>(&mut self, bus: &mut B, v: u16) {
        if self.x8() {
            self.push8(bus, v as u8);
        } else {
            self.push16(bus, v);
        }
    }

    fn pull_m<B: CpuBus>(&mut self, bus: &mut B) -> u16 {
        if self.m8() {
            self.pull8(bus) as u16
        } else {
            self.pull16(bus)
        }
    }

    fn pull_x<B: CpuBus>(&mut self, bus: &mut B) -> u16 {
        if self.x8() {
            self.pull8(bus) as u16
        } else {
            self.pull16(bus)
        }
    }

    // ---- Branches ----

    fn branch<B: CpuBus>(&mut self, bus: &mut B, cond: bool) {
        let disp = self.fetch8(bus) as i8 as i16;
        if cond {
            bus.idle();
            let old = self.pc;
            self.pc = (self.pc as i16).wrapping_add(disp) as u16;
            if self.emulation && (old & 0xFF00) != (self.pc & 0xFF00) {
                bus.idle();
            }
        }
    }

    // ---- Block moves (one byte per invocation; re-executed until A == $FFFF) ----

    fn block_move<B: CpuBus>(&mut self, bus: &mut B, forward: bool) {
        let dest_bank = self.fetch8(bus);
        let src_bank = self.fetch8(bus);
        self.dbr = dest_bank;
        let val = bus.read(((src_bank as u32) << 16) | self.x as u32);
        bus.write(((dest_bank as u32) << 16) | self.y as u32, val);
        bus.idle();
        bus.idle();
        if forward {
            self.x = self.x.wrapping_add(1);
            self.y = self.y.wrapping_add(1);
        } else {
            self.x = self.x.wrapping_sub(1);
            self.y = self.y.wrapping_sub(1);
        }
        if self.x8() {
            self.x &= 0x00FF;
            self.y &= 0x00FF;
        }
        self.a = self.a.wrapping_sub(1);
        if self.a != 0xFFFF {
            // Rewind to the opcode so an interrupt can be serviced mid-move.
            self.pc = self.pc.wrapping_sub(3);
        }
    }

    /// Decode and execute one instruction.
    pub(crate) fn execute<B: CpuBus>(&mut self, bus: &mut B, opcode: u8) {
        match opcode {
            // ---- ORA ----
            0x01 => {
                let ea = self.ea_dp_ind_x(bus);
                let v = self.load_m(bus, ea);
                self.op_ora(v);
            }
            0x03 => {
                let ea = self.ea_stack(bus);
                let v = self.load_m(bus, ea);
                self.op_ora(v);
            }
            0x05 => {
                let ea = self.ea_dp(bus);
                let v = self.load_m(bus, ea);
                self.op_ora(v);
            }
            0x07 => {
                let ea = self.ea_dp_long(bus);
                let v = self.load_m(bus, ea);
                self.op_ora(v);
            }
            0x09 => {
                let v = self.imm_m(bus);
                self.op_ora(v);
            }
            0x0D => {
                let ea = self.ea_abs(bus);
                let v = self.load_m(bus, ea);
                self.op_ora(v);
            }
            0x0F => {
                let ea = self.ea_long(bus);
                let v = self.load_m(bus, ea);
                self.op_ora(v);
            }
            0x11 => {
                let ea = self.ea_dp_ind_y(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_ora(v);
            }
            0x12 => {
                let ea = self.ea_dp_ind(bus);
                let v = self.load_m(bus, ea);
                self.op_ora(v);
            }
            0x13 => {
                let ea = self.ea_stack_ind_y(bus);
                let v = self.load_m(bus, ea);
                self.op_ora(v);
            }
            0x15 => {
                let ea = self.ea_dp_x(bus);
                let v = self.load_m(bus, ea);
                self.op_ora(v);
            }
            0x17 => {
                let ea = self.ea_dp_long_y(bus);
                let v = self.load_m(bus, ea);
                self.op_ora(v);
            }
            0x19 => {
                let ea = self.ea_abs_y(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_ora(v);
            }
            0x1D => {
                let ea = self.ea_abs_x(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_ora(v);
            }
            0x1F => {
                let ea = self.ea_long_x(bus);
                let v = self.load_m(bus, ea);
                self.op_ora(v);
            }

            // ---- AND ----
            0x21 => {
                let ea = self.ea_dp_ind_x(bus);
                let v = self.load_m(bus, ea);
                self.op_and(v);
            }
            0x23 => {
                let ea = self.ea_stack(bus);
                let v = self.load_m(bus, ea);
                self.op_and(v);
            }
            0x25 => {
                let ea = self.ea_dp(bus);
                let v = self.load_m(bus, ea);
                self.op_and(v);
            }
            0x27 => {
                let ea = self.ea_dp_long(bus);
                let v = self.load_m(bus, ea);
                self.op_and(v);
            }
            0x29 => {
                let v = self.imm_m(bus);
                self.op_and(v);
            }
            0x2D => {
                let ea = self.ea_abs(bus);
                let v = self.load_m(bus, ea);
                self.op_and(v);
            }
            0x2F => {
                let ea = self.ea_long(bus);
                let v = self.load_m(bus, ea);
                self.op_and(v);
            }
            0x31 => {
                let ea = self.ea_dp_ind_y(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_and(v);
            }
            0x32 => {
                let ea = self.ea_dp_ind(bus);
                let v = self.load_m(bus, ea);
                self.op_and(v);
            }
            0x33 => {
                let ea = self.ea_stack_ind_y(bus);
                let v = self.load_m(bus, ea);
                self.op_and(v);
            }
            0x35 => {
                let ea = self.ea_dp_x(bus);
                let v = self.load_m(bus, ea);
                self.op_and(v);
            }
            0x37 => {
                let ea = self.ea_dp_long_y(bus);
                let v = self.load_m(bus, ea);
                self.op_and(v);
            }
            0x39 => {
                let ea = self.ea_abs_y(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_and(v);
            }
            0x3D => {
                let ea = self.ea_abs_x(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_and(v);
            }
            0x3F => {
                let ea = self.ea_long_x(bus);
                let v = self.load_m(bus, ea);
                self.op_and(v);
            }

            // ---- EOR ----
            0x41 => {
                let ea = self.ea_dp_ind_x(bus);
                let v = self.load_m(bus, ea);
                self.op_eor(v);
            }
            0x43 => {
                let ea = self.ea_stack(bus);
                let v = self.load_m(bus, ea);
                self.op_eor(v);
            }
            0x45 => {
                let ea = self.ea_dp(bus);
                let v = self.load_m(bus, ea);
                self.op_eor(v);
            }
            0x47 => {
                let ea = self.ea_dp_long(bus);
                let v = self.load_m(bus, ea);
                self.op_eor(v);
            }
            0x49 => {
                let v = self.imm_m(bus);
                self.op_eor(v);
            }
            0x4D => {
                let ea = self.ea_abs(bus);
                let v = self.load_m(bus, ea);
                self.op_eor(v);
            }
            0x4F => {
                let ea = self.ea_long(bus);
                let v = self.load_m(bus, ea);
                self.op_eor(v);
            }
            0x51 => {
                let ea = self.ea_dp_ind_y(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_eor(v);
            }
            0x52 => {
                let ea = self.ea_dp_ind(bus);
                let v = self.load_m(bus, ea);
                self.op_eor(v);
            }
            0x53 => {
                let ea = self.ea_stack_ind_y(bus);
                let v = self.load_m(bus, ea);
                self.op_eor(v);
            }
            0x55 => {
                let ea = self.ea_dp_x(bus);
                let v = self.load_m(bus, ea);
                self.op_eor(v);
            }
            0x57 => {
                let ea = self.ea_dp_long_y(bus);
                let v = self.load_m(bus, ea);
                self.op_eor(v);
            }
            0x59 => {
                let ea = self.ea_abs_y(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_eor(v);
            }
            0x5D => {
                let ea = self.ea_abs_x(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_eor(v);
            }
            0x5F => {
                let ea = self.ea_long_x(bus);
                let v = self.load_m(bus, ea);
                self.op_eor(v);
            }

            // ---- ADC ----
            0x61 => {
                let ea = self.ea_dp_ind_x(bus);
                let v = self.load_m(bus, ea);
                self.op_adc(v);
            }
            0x63 => {
                let ea = self.ea_stack(bus);
                let v = self.load_m(bus, ea);
                self.op_adc(v);
            }
            0x65 => {
                let ea = self.ea_dp(bus);
                let v = self.load_m(bus, ea);
                self.op_adc(v);
            }
            0x67 => {
                let ea = self.ea_dp_long(bus);
                let v = self.load_m(bus, ea);
                self.op_adc(v);
            }
            0x69 => {
                let v = self.imm_m(bus);
                self.op_adc(v);
            }
            0x6D => {
                let ea = self.ea_abs(bus);
                let v = self.load_m(bus, ea);
                self.op_adc(v);
            }
            0x6F => {
                let ea = self.ea_long(bus);
                let v = self.load_m(bus, ea);
                self.op_adc(v);
            }
            0x71 => {
                let ea = self.ea_dp_ind_y(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_adc(v);
            }
            0x72 => {
                let ea = self.ea_dp_ind(bus);
                let v = self.load_m(bus, ea);
                self.op_adc(v);
            }
            0x73 => {
                let ea = self.ea_stack_ind_y(bus);
                let v = self.load_m(bus, ea);
                self.op_adc(v);
            }
            0x75 => {
                let ea = self.ea_dp_x(bus);
                let v = self.load_m(bus, ea);
                self.op_adc(v);
            }
            0x77 => {
                let ea = self.ea_dp_long_y(bus);
                let v = self.load_m(bus, ea);
                self.op_adc(v);
            }
            0x79 => {
                let ea = self.ea_abs_y(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_adc(v);
            }
            0x7D => {
                let ea = self.ea_abs_x(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_adc(v);
            }
            0x7F => {
                let ea = self.ea_long_x(bus);
                let v = self.load_m(bus, ea);
                self.op_adc(v);
            }

            // ---- SBC ----
            0xE1 => {
                let ea = self.ea_dp_ind_x(bus);
                let v = self.load_m(bus, ea);
                self.op_sbc(v);
            }
            0xE3 => {
                let ea = self.ea_stack(bus);
                let v = self.load_m(bus, ea);
                self.op_sbc(v);
            }
            0xE5 => {
                let ea = self.ea_dp(bus);
                let v = self.load_m(bus, ea);
                self.op_sbc(v);
            }
            0xE7 => {
                let ea = self.ea_dp_long(bus);
                let v = self.load_m(bus, ea);
                self.op_sbc(v);
            }
            0xE9 => {
                let v = self.imm_m(bus);
                self.op_sbc(v);
            }
            0xED => {
                let ea = self.ea_abs(bus);
                let v = self.load_m(bus, ea);
                self.op_sbc(v);
            }
            0xEF => {
                let ea = self.ea_long(bus);
                let v = self.load_m(bus, ea);
                self.op_sbc(v);
            }
            0xF1 => {
                let ea = self.ea_dp_ind_y(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_sbc(v);
            }
            0xF2 => {
                let ea = self.ea_dp_ind(bus);
                let v = self.load_m(bus, ea);
                self.op_sbc(v);
            }
            0xF3 => {
                let ea = self.ea_stack_ind_y(bus);
                let v = self.load_m(bus, ea);
                self.op_sbc(v);
            }
            0xF5 => {
                let ea = self.ea_dp_x(bus);
                let v = self.load_m(bus, ea);
                self.op_sbc(v);
            }
            0xF7 => {
                let ea = self.ea_dp_long_y(bus);
                let v = self.load_m(bus, ea);
                self.op_sbc(v);
            }
            0xF9 => {
                let ea = self.ea_abs_y(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_sbc(v);
            }
            0xFD => {
                let ea = self.ea_abs_x(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_sbc(v);
            }
            0xFF => {
                let ea = self.ea_long_x(bus);
                let v = self.load_m(bus, ea);
                self.op_sbc(v);
            }

            // ---- CMP ----
            0xC1 => {
                let ea = self.ea_dp_ind_x(bus);
                let v = self.load_m(bus, ea);
                self.op_cmp(v);
            }
            0xC3 => {
                let ea = self.ea_stack(bus);
                let v = self.load_m(bus, ea);
                self.op_cmp(v);
            }
            0xC5 => {
                let ea = self.ea_dp(bus);
                let v = self.load_m(bus, ea);
                self.op_cmp(v);
            }
            0xC7 => {
                let ea = self.ea_dp_long(bus);
                let v = self.load_m(bus, ea);
                self.op_cmp(v);
            }
            0xC9 => {
                let v = self.imm_m(bus);
                self.op_cmp(v);
            }
            0xCD => {
                let ea = self.ea_abs(bus);
                let v = self.load_m(bus, ea);
                self.op_cmp(v);
            }
            0xCF => {
                let ea = self.ea_long(bus);
                let v = self.load_m(bus, ea);
                self.op_cmp(v);
            }
            0xD1 => {
                let ea = self.ea_dp_ind_y(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_cmp(v);
            }
            0xD2 => {
                let ea = self.ea_dp_ind(bus);
                let v = self.load_m(bus, ea);
                self.op_cmp(v);
            }
            0xD3 => {
                let ea = self.ea_stack_ind_y(bus);
                let v = self.load_m(bus, ea);
                self.op_cmp(v);
            }
            0xD5 => {
                let ea = self.ea_dp_x(bus);
                let v = self.load_m(bus, ea);
                self.op_cmp(v);
            }
            0xD7 => {
                let ea = self.ea_dp_long_y(bus);
                let v = self.load_m(bus, ea);
                self.op_cmp(v);
            }
            0xD9 => {
                let ea = self.ea_abs_y(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_cmp(v);
            }
            0xDD => {
                let ea = self.ea_abs_x(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_cmp(v);
            }
            0xDF => {
                let ea = self.ea_long_x(bus);
                let v = self.load_m(bus, ea);
                self.op_cmp(v);
            }

            // ---- CPX / CPY ----
            0xE0 => {
                let v = self.imm_x(bus);
                self.op_cpx(v);
            }
            0xE4 => {
                let ea = self.ea_dp(bus);
                let v = self.load_x(bus, ea);
                self.op_cpx(v);
            }
            0xEC => {
                let ea = self.ea_abs(bus);
                let v = self.load_x(bus, ea);
                self.op_cpx(v);
            }
            0xC0 => {
                let v = self.imm_x(bus);
                self.op_cpy(v);
            }
            0xC4 => {
                let ea = self.ea_dp(bus);
                let v = self.load_x(bus, ea);
                self.op_cpy(v);
            }
            0xCC => {
                let ea = self.ea_abs(bus);
                let v = self.load_x(bus, ea);
                self.op_cpy(v);
            }

            // ---- LDA ----
            0xA1 => {
                let ea = self.ea_dp_ind_x(bus);
                let v = self.load_m(bus, ea);
                self.op_lda(v);
            }
            0xA3 => {
                let ea = self.ea_stack(bus);
                let v = self.load_m(bus, ea);
                self.op_lda(v);
            }
            0xA5 => {
                let ea = self.ea_dp(bus);
                let v = self.load_m(bus, ea);
                self.op_lda(v);
            }
            0xA7 => {
                let ea = self.ea_dp_long(bus);
                let v = self.load_m(bus, ea);
                self.op_lda(v);
            }
            0xA9 => {
                let v = self.imm_m(bus);
                self.op_lda(v);
            }
            0xAD => {
                let ea = self.ea_abs(bus);
                let v = self.load_m(bus, ea);
                self.op_lda(v);
            }
            0xAF => {
                let ea = self.ea_long(bus);
                let v = self.load_m(bus, ea);
                self.op_lda(v);
            }
            0xB1 => {
                let ea = self.ea_dp_ind_y(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_lda(v);
            }
            0xB2 => {
                let ea = self.ea_dp_ind(bus);
                let v = self.load_m(bus, ea);
                self.op_lda(v);
            }
            0xB3 => {
                let ea = self.ea_stack_ind_y(bus);
                let v = self.load_m(bus, ea);
                self.op_lda(v);
            }
            0xB5 => {
                let ea = self.ea_dp_x(bus);
                let v = self.load_m(bus, ea);
                self.op_lda(v);
            }
            0xB7 => {
                let ea = self.ea_dp_long_y(bus);
                let v = self.load_m(bus, ea);
                self.op_lda(v);
            }
            0xB9 => {
                let ea = self.ea_abs_y(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_lda(v);
            }
            0xBD => {
                let ea = self.ea_abs_x(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_lda(v);
            }
            0xBF => {
                let ea = self.ea_long_x(bus);
                let v = self.load_m(bus, ea);
                self.op_lda(v);
            }

            // ---- LDX ----
            0xA2 => {
                let v = self.imm_x(bus);
                self.set_x_reg(v);
                self.set_nz_x(v);
            }
            0xA6 => {
                let ea = self.ea_dp(bus);
                let v = self.load_x(bus, ea);
                self.set_x_reg(v);
                self.set_nz_x(v);
            }
            0xAE => {
                let ea = self.ea_abs(bus);
                let v = self.load_x(bus, ea);
                self.set_x_reg(v);
                self.set_nz_x(v);
            }
            0xB6 => {
                let ea = self.ea_dp_y(bus);
                let v = self.load_x(bus, ea);
                self.set_x_reg(v);
                self.set_nz_x(v);
            }
            0xBE => {
                let ea = self.ea_abs_y(bus, Pen::Read);
                let v = self.load_x(bus, ea);
                self.set_x_reg(v);
                self.set_nz_x(v);
            }

            // ---- LDY ----
            0xA0 => {
                let v = self.imm_x(bus);
                self.set_y_reg(v);
                self.set_nz_x(v);
            }
            0xA4 => {
                let ea = self.ea_dp(bus);
                let v = self.load_x(bus, ea);
                self.set_y_reg(v);
                self.set_nz_x(v);
            }
            0xAC => {
                let ea = self.ea_abs(bus);
                let v = self.load_x(bus, ea);
                self.set_y_reg(v);
                self.set_nz_x(v);
            }
            0xB4 => {
                let ea = self.ea_dp_x(bus);
                let v = self.load_x(bus, ea);
                self.set_y_reg(v);
                self.set_nz_x(v);
            }
            0xBC => {
                let ea = self.ea_abs_x(bus, Pen::Read);
                let v = self.load_x(bus, ea);
                self.set_y_reg(v);
                self.set_nz_x(v);
            }

            // ---- STA ----
            0x81 => {
                let ea = self.ea_dp_ind_x(bus);
                self.store_m(bus, ea, self.a);
            }
            0x83 => {
                let ea = self.ea_stack(bus);
                self.store_m(bus, ea, self.a);
            }
            0x85 => {
                let ea = self.ea_dp(bus);
                self.store_m(bus, ea, self.a);
            }
            0x87 => {
                let ea = self.ea_dp_long(bus);
                self.store_m(bus, ea, self.a);
            }
            0x8D => {
                let ea = self.ea_abs(bus);
                self.store_m(bus, ea, self.a);
            }
            0x8F => {
                let ea = self.ea_long(bus);
                self.store_m(bus, ea, self.a);
            }
            0x91 => {
                let ea = self.ea_dp_ind_y(bus, Pen::Always);
                self.store_m(bus, ea, self.a);
            }
            0x92 => {
                let ea = self.ea_dp_ind(bus);
                self.store_m(bus, ea, self.a);
            }
            0x93 => {
                let ea = self.ea_stack_ind_y(bus);
                self.store_m(bus, ea, self.a);
            }
            0x95 => {
                let ea = self.ea_dp_x(bus);
                self.store_m(bus, ea, self.a);
            }
            0x97 => {
                let ea = self.ea_dp_long_y(bus);
                self.store_m(bus, ea, self.a);
            }
            0x99 => {
                let ea = self.ea_abs_y(bus, Pen::Always);
                self.store_m(bus, ea, self.a);
            }
            0x9D => {
                let ea = self.ea_abs_x(bus, Pen::Always);
                self.store_m(bus, ea, self.a);
            }
            0x9F => {
                let ea = self.ea_long_x(bus);
                self.store_m(bus, ea, self.a);
            }

            // ---- STX / STY ----
            0x86 => {
                let ea = self.ea_dp(bus);
                self.store_x(bus, ea, self.x);
            }
            0x8E => {
                let ea = self.ea_abs(bus);
                self.store_x(bus, ea, self.x);
            }
            0x96 => {
                let ea = self.ea_dp_y(bus);
                self.store_x(bus, ea, self.x);
            }
            0x84 => {
                let ea = self.ea_dp(bus);
                self.store_x(bus, ea, self.y);
            }
            0x8C => {
                let ea = self.ea_abs(bus);
                self.store_x(bus, ea, self.y);
            }
            0x94 => {
                let ea = self.ea_dp_x(bus);
                self.store_x(bus, ea, self.y);
            }

            // ---- STZ ----
            0x64 => {
                let ea = self.ea_dp(bus);
                self.store_m(bus, ea, 0);
            }
            0x74 => {
                let ea = self.ea_dp_x(bus);
                self.store_m(bus, ea, 0);
            }
            0x9C => {
                let ea = self.ea_abs(bus);
                self.store_m(bus, ea, 0);
            }
            0x9E => {
                let ea = self.ea_abs_x(bus, Pen::Always);
                self.store_m(bus, ea, 0);
            }

            // ---- ASL ----
            0x06 => {
                let ea = self.ea_dp(bus);
                self.rmw_m(bus, ea, algorithms::asl);
            }
            0x0A => self.rmw_a(bus, algorithms::asl),
            0x0E => {
                let ea = self.ea_abs(bus);
                self.rmw_m(bus, ea, algorithms::asl);
            }
            0x16 => {
                let ea = self.ea_dp_x(bus);
                self.rmw_m(bus, ea, algorithms::asl);
            }
            0x1E => {
                let ea = self.ea_abs_x(bus, Pen::Always);
                self.rmw_m(bus, ea, algorithms::asl);
            }

            // ---- LSR ----
            0x46 => {
                let ea = self.ea_dp(bus);
                self.rmw_m(bus, ea, algorithms::lsr);
            }
            0x4A => self.rmw_a(bus, algorithms::lsr),
            0x4E => {
                let ea = self.ea_abs(bus);
                self.rmw_m(bus, ea, algorithms::lsr);
            }
            0x56 => {
                let ea = self.ea_dp_x(bus);
                self.rmw_m(bus, ea, algorithms::lsr);
            }
            0x5E => {
                let ea = self.ea_abs_x(bus, Pen::Always);
                self.rmw_m(bus, ea, algorithms::lsr);
            }

            // ---- ROL ----
            0x26 => {
                let ea = self.ea_dp(bus);
                self.rmw_m(bus, ea, algorithms::rol);
            }
            0x2A => self.rmw_a(bus, algorithms::rol),
            0x2E => {
                let ea = self.ea_abs(bus);
                self.rmw_m(bus, ea, algorithms::rol);
            }
            0x36 => {
                let ea = self.ea_dp_x(bus);
                self.rmw_m(bus, ea, algorithms::rol);
            }
            0x3E => {
                let ea = self.ea_abs_x(bus, Pen::Always);
                self.rmw_m(bus, ea, algorithms::rol);
            }

            // ---- ROR ----
            0x66 => {
                let ea = self.ea_dp(bus);
                self.rmw_m(bus, ea, algorithms::ror);
            }
            0x6A => self.rmw_a(bus, algorithms::ror),
            0x6E => {
                let ea = self.ea_abs(bus);
                self.rmw_m(bus, ea, algorithms::ror);
            }
            0x76 => {
                let ea = self.ea_dp_x(bus);
                self.rmw_m(bus, ea, algorithms::ror);
            }
            0x7E => {
                let ea = self.ea_abs_x(bus, Pen::Always);
                self.rmw_m(bus, ea, algorithms::ror);
            }

            // ---- INC / DEC memory ----
            0xE6 => {
                let ea = self.ea_dp(bus);
                self.rmw_m(bus, ea, algorithms::inc);
            }
            0xEE => {
                let ea = self.ea_abs(bus);
                self.rmw_m(bus, ea, algorithms::inc);
            }
            0xF6 => {
                let ea = self.ea_dp_x(bus);
                self.rmw_m(bus, ea, algorithms::inc);
            }
            0xFE => {
                let ea = self.ea_abs_x(bus, Pen::Always);
                self.rmw_m(bus, ea, algorithms::inc);
            }
            0xC6 => {
                let ea = self.ea_dp(bus);
                self.rmw_m(bus, ea, algorithms::dec);
            }
            0xCE => {
                let ea = self.ea_abs(bus);
                self.rmw_m(bus, ea, algorithms::dec);
            }
            0xD6 => {
                let ea = self.ea_dp_x(bus);
                self.rmw_m(bus, ea, algorithms::dec);
            }
            0xDE => {
                let ea = self.ea_abs_x(bus, Pen::Always);
                self.rmw_m(bus, ea, algorithms::dec);
            }
            0x1A => self.rmw_a(bus, algorithms::inc),
            0x3A => self.rmw_a(bus, algorithms::dec),

            // ---- INX/INY/DEX/DEY ----
            0xE8 => {
                bus.idle();
                let v = self.x.wrapping_add(1);
                self.set_x_reg(v);
                self.set_nz_x(self.x);
            }
            0xC8 => {
                bus.idle();
                let v = self.y.wrapping_add(1);
                self.set_y_reg(v);
                self.set_nz_x(self.y);
            }
            0xCA => {
                bus.idle();
                let v = self.x.wrapping_sub(1);
                self.set_x_reg(v);
                self.set_nz_x(self.x);
            }
            0x88 => {
                bus.idle();
                let v = self.y.wrapping_sub(1);
                self.set_y_reg(v);
                self.set_nz_x(self.y);
            }

            // ---- BIT ----
            0x24 => {
                let ea = self.ea_dp(bus);
                let v = self.load_m(bus, ea);
                self.op_bit(v);
            }
            0x2C => {
                let ea = self.ea_abs(bus);
                let v = self.load_m(bus, ea);
                self.op_bit(v);
            }
            0x34 => {
                let ea = self.ea_dp_x(bus);
                let v = self.load_m(bus, ea);
                self.op_bit(v);
            }
            0x3C => {
                let ea = self.ea_abs_x(bus, Pen::Read);
                let v = self.load_m(bus, ea);
                self.op_bit(v);
            }
            0x89 => {
                let v = self.imm_m(bus);
                let mask = if self.m8() { 0x00FF } else { 0xFFFF };
                self.p.set_z(self.a & v & mask == 0);
            }

            // ---- TSB / TRB ----
            0x04 => {
                let ea = self.ea_dp(bus);
                self.op_tsb(bus, ea);
            }
            0x0C => {
                let ea = self.ea_abs(bus);
                self.op_tsb(bus, ea);
            }
            0x14 => {
                let ea = self.ea_dp(bus);
                self.op_trb(bus, ea);
            }
            0x1C => {
                let ea = self.ea_abs(bus);
                self.op_trb(bus, ea);
            }

            // ---- Branches ----
            0x10 => self.branch(bus, !self.p.n()),
            0x30 => self.branch(bus, self.p.n()),
            0x50 => self.branch(bus, !self.p.v()),
            0x70 => self.branch(bus, self.p.v()),
            0x80 => self.branch(bus, true),
            0x90 => self.branch(bus, !self.p.c()),
            0xB0 => self.branch(bus, self.p.c()),
            0xD0 => self.branch(bus, !self.p.z()),
            0xF0 => self.branch(bus, self.p.z()),
            0x82 => {
                let disp = self.fetch16(bus) as i16;
                bus.idle();
                self.pc = (self.pc as i16).wrapping_add(disp) as u16;
            }

            // ---- Jumps / calls / returns ----
            0x4C => {
                self.pc = self.fetch16(bus);
            }
            0x5C => {
                let a = self.fetch24(bus);
                self.pbr = (a >> 16) as u8;
                self.pc = a as u16;
            }
            0x6C => {
                let ptr = self.fetch16(bus);
                let lo = bus.read(ptr as u32) as u16;
                let hi = bus.read(ptr.wrapping_add(1) as u32) as u16;
                self.pc = lo | (hi << 8);
            }
            0x7C => {
                let ptr = self.fetch16(bus);
                bus.idle();
                let base = ptr.wrapping_add(self.x);
                let lo = bus.read(((self.pbr as u32) << 16) | base as u32) as u16;
                let hi =
                    bus.read(((self.pbr as u32) << 16) | base.wrapping_add(1) as u32) as u16;
                self.pc = lo | (hi << 8);
            }
            0xDC => {
                let ptr = self.fetch16(bus);
                let lo = bus.read(ptr as u32) as u32;
                let mid = bus.read(ptr.wrapping_add(1) as u32) as u32;
                let hi = bus.read(ptr.wrapping_add(2) as u32) as u32;
                self.pbr = hi as u8;
                self.pc = (lo | (mid << 8)) as u16;
            }
            0x20 => {
                let target = self.fetch16(bus);
                bus.idle();
                let ret = self.pc.wrapping_sub(1);
                self.push16(bus, ret);
                self.pc = target;
            }
            0x22 => {
                let addr = self.fetch24(bus);
                self.push8(bus, self.pbr);
                bus.idle();
                let ret = self.pc.wrapping_sub(1);
                self.push16(bus, ret);
                self.pbr = (addr >> 16) as u8;
                self.pc = addr as u16;
            }
            0xFC => {
                let ptr = self.fetch16(bus);
                bus.idle();
                let ret = self.pc.wrapping_sub(1);
                self.push16(bus, ret);
                let base = ptr.wrapping_add(self.x);
                let lo = bus.read(((self.pbr as u32) << 16) | base as u32) as u16;
                let hi =
                    bus.read(((self.pbr as u32) << 16) | base.wrapping_add(1) as u32) as u16;
                self.pc = lo | (hi << 8);
            }
            0x60 => {
                bus.idle();
                bus.idle();
                let v = self.pull16(bus);
                bus.idle();
                self.pc = v.wrapping_add(1);
            }
            0x6B => {
                bus.idle();
                bus.idle();
                let lo = self.pull16(bus);
                self.pc = lo.wrapping_add(1);
                self.pbr = self.pull8(bus);
            }
            0x40 => {
                bus.idle();
                bus.idle();
                let v = self.pull8(bus);
                if self.emulation {
                    self.p.0 = (v & 0xCF) | 0x30;
                } else {
                    self.p.0 = v;
                }
                self.pc = self.pull16(bus);
                if !self.emulation {
                    self.pbr = self.pull8(bus);
                }
                self.apply_flag_constraints();
            }

            // ---- Stack pushes / pulls ----
            0x08 => {
                bus.idle();
                let mut v = self.p.0;
                if self.emulation {
                    v |= 0x30;
                }
                self.push8(bus, v);
            }
            0x28 => {
                bus.idle();
                bus.idle();
                let v = self.pull8(bus);
                if self.emulation {
                    self.p.0 = (v & 0xCF) | 0x30;
                } else {
                    self.p.0 = v;
                }
                self.apply_flag_constraints();
            }
            0x48 => {
                bus.idle();
                self.push_m(bus, self.a);
            }
            0x68 => {
                bus.idle();
                bus.idle();
                let v = self.pull_m(bus);
                self.set_a(v);
                self.set_nz_m(v);
            }
            0xDA => {
                bus.idle();
                self.push_x(bus, self.x);
            }
            0xFA => {
                bus.idle();
                bus.idle();
                let v = self.pull_x(bus);
                self.set_x_reg(v);
                self.set_nz_x(v);
            }
            0x5A => {
                bus.idle();
                self.push_x(bus, self.y);
            }
            0x7A => {
                bus.idle();
                bus.idle();
                let v = self.pull_x(bus);
                self.set_y_reg(v);
                self.set_nz_x(v);
            }
            0x8B => {
                bus.idle();
                self.push8(bus, self.dbr);
            }
            0xAB => {
                bus.idle();
                bus.idle();
                let v = self.pull8(bus);
                self.dbr = v;
                self.p.set_n(v & 0x80 != 0);
                self.p.set_z(v == 0);
            }
            0x4B => {
                bus.idle();
                self.push8(bus, self.pbr);
            }
            0x0B => {
                bus.idle();
                self.push16(bus, self.d);
            }
            0x2B => {
                bus.idle();
                bus.idle();
                let v = self.pull16(bus);
                self.d = v;
                self.set_nz16(v);
            }
            0xF4 => {
                let v = self.fetch16(bus);
                self.push16(bus, v);
            }
            0xD4 => {
                let off = self.fetch8(bus) as u16;
                if self.d & 0x00FF != 0 {
                    bus.idle();
                }
                let a = self.d.wrapping_add(off);
                let lo = bus.read(a as u32) as u16;
                let hi = bus.read(a.wrapping_add(1) as u32) as u16;
                self.push16(bus, lo | (hi << 8));
            }
            0x62 => {
                let disp = self.fetch16(bus) as i16;
                bus.idle();
                let v = (self.pc as i16).wrapping_add(disp) as u16;
                self.push16(bus, v);
            }

            // ---- Transfers ----
            0xAA => {
                bus.idle();
                self.set_x_reg(self.a);
                self.set_nz_x(self.x);
            }
            0xA8 => {
                bus.idle();
                self.set_y_reg(self.a);
                self.set_nz_x(self.y);
            }
            0x8A => {
                bus.idle();
                let v = self.x;
                self.set_a(v);
                self.set_nz_m(v);
            }
            0x98 => {
                bus.idle();
                let v = self.y;
                self.set_a(v);
                self.set_nz_m(v);
            }
            0xBA => {
                bus.idle();
                self.set_x_reg(self.s);
                self.set_nz_x(self.x);
            }
            0x9A => {
                bus.idle();
                if self.emulation {
                    self.s = 0x0100 | (self.x & 0x00FF);
                } else {
                    self.s = self.x;
                }
            }
            0x9B => {
                bus.idle();
                self.set_y_reg(self.x);
                self.set_nz_x(self.y);
            }
            0xBB => {
                bus.idle();
                self.set_x_reg(self.y);
                self.set_nz_x(self.x);
            }
            0x5B => {
                bus.idle();
                self.d = self.a;
                self.set_nz16(self.a);
            }
            0x7B => {
                bus.idle();
                self.a = self.d;
                self.set_nz16(self.a);
            }
            0x1B => {
                bus.idle();
                if self.emulation {
                    self.s = 0x0100 | (self.a & 0x00FF);
                } else {
                    self.s = self.a;
                }
            }
            0x3B => {
                bus.idle();
                self.a = self.s;
                self.set_nz16(self.a);
            }
            0xEB => {
                bus.idle();
                bus.idle();
                let lo = self.a & 0x00FF;
                let hi = (self.a >> 8) & 0x00FF;
                self.a = (lo << 8) | hi;
                self.p.set_n(hi & 0x80 != 0);
                self.p.set_z(hi == 0);
            }

            // ---- Flag ops ----
            0x18 => {
                bus.idle();
                self.p.set_c(false);
            }
            0x38 => {
                bus.idle();
                self.p.set_c(true);
            }
            0x58 => {
                bus.idle();
                self.p.set_i(false);
            }
            0x78 => {
                bus.idle();
                self.p.set_i(true);
            }
            0xB8 => {
                bus.idle();
                self.p.set_v(false);
            }
            0xD8 => {
                bus.idle();
                self.p.set_d(false);
            }
            0xF8 => {
                bus.idle();
                self.p.set_d(true);
            }
            0xC2 => {
                let m = self.fetch8(bus);
                bus.idle();
                self.p.0 &= !m;
                self.apply_flag_constraints();
            }
            0xE2 => {
                let m = self.fetch8(bus);
                bus.idle();
                self.p.0 |= m;
                self.apply_flag_constraints();
            }
            0xFB => {
                bus.idle();
                let new_e = self.p.c();
                self.p.set_c(self.emulation);
                self.emulation = new_e;
                self.apply_flag_constraints();
            }

            // ---- Block moves ----
            0x54 => self.block_move(bus, true),
            0x44 => self.block_move(bus, false),

            // ---- Interrupts / control ----
            0x00 => {
                let _sig = self.fetch8(bus);
                self.service_interrupt(bus, 0xFFE6, 0xFFFE, true, true);
            }
            0x02 => {
                let _sig = self.fetch8(bus);
                self.service_interrupt(bus, 0xFFE4, 0xFFF4, true, true);
            }
            0xCB => {
                bus.idle();
                bus.idle();
                self.waiting = true;
            }
            0xDB => {
                bus.idle();
                bus.idle();
                self.stopped = true;
            }
            0x42 => {
                let _ = self.fetch8(bus);
            }
            0xEA => {
                bus.idle();
            }
        }
    }
}
