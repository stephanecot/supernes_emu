//! 65C816 addressing modes: effective-address computation, bank/direct-page
//! wrap rules, and per-mode internal cycles (DL!=0, index add, page cross).

use super::{Cpu, CpuBus};

/// A resolved effective address plus how its 16-bit high byte wraps.
/// `bank0` = the +1 for a 16-bit access wraps within bank $00 (direct-page and
/// stack data); otherwise the +1 uses full 24-bit arithmetic and crosses banks.
#[derive(Clone, Copy)]
pub struct Ea {
    pub addr: u32,
    pub bank0: bool,
}

/// Index-add cycle penalty policy. Reads pay the extra internal cycle only on a
/// page cross (or when the index is 16-bit); stores and RMW always pay it.
#[derive(Clone, Copy, PartialEq)]
pub enum Pen {
    Read,
    Always,
}

impl Cpu {
    fn dl_nonzero(&self) -> bool {
        self.d & 0x00FF != 0
    }

    /// Direct-page effective address for base `off` plus `index`, honoring the
    /// emulation-mode DL=$00 page-wrap quirk.
    fn dp_addr(&self, off: u16, index: u16) -> u16 {
        if self.emulation && self.d & 0x00FF == 0 {
            (self.d & 0xFF00) | ((off + index) & 0x00FF)
        } else {
            self.d.wrapping_add(off).wrapping_add(index)
        }
    }

    /// Read a 16-bit pointer stored in bank $00 at (D + off + index), with the
    /// emulation-mode DL=$00 page-wrap for the high byte.
    fn dp_ptr16<B: CpuBus>(&mut self, bus: &mut B, off: u16, index: u16) -> u16 {
        if self.emulation && self.d & 0x00FF == 0 {
            let base = self.d & 0xFF00;
            let l = (off + index) & 0x00FF;
            let lo = bus.read((base | l) as u32) as u16;
            let hi = bus.read((base | ((l + 1) & 0x00FF)) as u32) as u16;
            lo | (hi << 8)
        } else {
            let a = self.d.wrapping_add(off).wrapping_add(index);
            let lo = bus.read(a as u32) as u16;
            let hi = bus.read(a.wrapping_add(1) as u32) as u16;
            lo | (hi << 8)
        }
    }

    /// Read a 24-bit pointer stored in bank $00 at (D + off); [dir] is a "new"
    /// mode and never page-wraps even in emulation mode.
    fn dp_ptr24<B: CpuBus>(&mut self, bus: &mut B, off: u16) -> u32 {
        let a = self.d.wrapping_add(off);
        let lo = bus.read(a as u32) as u32;
        let mid = bus.read(a.wrapping_add(1) as u32) as u32;
        let hi = bus.read(a.wrapping_add(2) as u32) as u32;
        lo | (mid << 8) | (hi << 16)
    }

    // ---- Effective-address resolvers ----

    pub fn ea_abs<B: CpuBus>(&mut self, bus: &mut B) -> Ea {
        let base = self.fetch16(bus);
        Ea { addr: ((self.dbr as u32) << 16) | base as u32, bank0: false }
    }

    pub fn ea_abs_x<B: CpuBus>(&mut self, bus: &mut B, pen: Pen) -> Ea {
        let base = self.fetch16(bus);
        self.ea_abs_indexed(bus, base, self.x, pen)
    }

    pub fn ea_abs_y<B: CpuBus>(&mut self, bus: &mut B, pen: Pen) -> Ea {
        let base = self.fetch16(bus);
        self.ea_abs_indexed(bus, base, self.y, pen)
    }

    fn ea_abs_indexed<B: CpuBus>(&mut self, bus: &mut B, base: u16, index: u16, pen: Pen) -> Ea {
        let crossed = (base & 0x00FF) + (index & 0x00FF) > 0x00FF;
        if pen == Pen::Always || !self.p.x() || crossed {
            bus.idle();
        }
        let eff = (((self.dbr as u32) << 16) + base as u32 + index as u32) & 0xFF_FFFF;
        Ea { addr: eff, bank0: false }
    }

    pub fn ea_long<B: CpuBus>(&mut self, bus: &mut B) -> Ea {
        let addr = self.fetch24(bus);
        Ea { addr, bank0: false }
    }

    pub fn ea_long_x<B: CpuBus>(&mut self, bus: &mut B) -> Ea {
        let base = self.fetch24(bus);
        Ea { addr: (base + self.x as u32) & 0xFF_FFFF, bank0: false }
    }

    pub fn ea_dp<B: CpuBus>(&mut self, bus: &mut B) -> Ea {
        let off = self.fetch8(bus) as u16;
        if self.dl_nonzero() {
            bus.idle();
        }
        Ea { addr: self.dp_addr(off, 0) as u32, bank0: true }
    }

    pub fn ea_dp_x<B: CpuBus>(&mut self, bus: &mut B) -> Ea {
        let off = self.fetch8(bus) as u16;
        if self.dl_nonzero() {
            bus.idle();
        }
        bus.idle();
        Ea { addr: self.dp_addr(off, self.x) as u32, bank0: true }
    }

    pub fn ea_dp_y<B: CpuBus>(&mut self, bus: &mut B) -> Ea {
        let off = self.fetch8(bus) as u16;
        if self.dl_nonzero() {
            bus.idle();
        }
        bus.idle();
        Ea { addr: self.dp_addr(off, self.y) as u32, bank0: true }
    }

    pub fn ea_dp_ind<B: CpuBus>(&mut self, bus: &mut B) -> Ea {
        let off = self.fetch8(bus) as u16;
        if self.dl_nonzero() {
            bus.idle();
        }
        let ptr = self.dp_ptr16(bus, off, 0);
        Ea { addr: ((self.dbr as u32) << 16) | ptr as u32, bank0: false }
    }

    pub fn ea_dp_ind_x<B: CpuBus>(&mut self, bus: &mut B) -> Ea {
        let off = self.fetch8(bus) as u16;
        if self.dl_nonzero() {
            bus.idle();
        }
        bus.idle();
        let ptr = self.dp_ptr16(bus, off, self.x);
        Ea { addr: ((self.dbr as u32) << 16) | ptr as u32, bank0: false }
    }

    pub fn ea_dp_ind_y<B: CpuBus>(&mut self, bus: &mut B, pen: Pen) -> Ea {
        let off = self.fetch8(bus) as u16;
        if self.dl_nonzero() {
            bus.idle();
        }
        let ptr = self.dp_ptr16(bus, off, 0);
        let crossed = (ptr & 0x00FF) + (self.y & 0x00FF) > 0x00FF;
        if pen == Pen::Always || !self.p.x() || crossed {
            bus.idle();
        }
        let eff = (((self.dbr as u32) << 16) + ptr as u32 + self.y as u32) & 0xFF_FFFF;
        Ea { addr: eff, bank0: false }
    }

    pub fn ea_dp_long<B: CpuBus>(&mut self, bus: &mut B) -> Ea {
        let off = self.fetch8(bus) as u16;
        if self.dl_nonzero() {
            bus.idle();
        }
        let ptr = self.dp_ptr24(bus, off);
        Ea { addr: ptr, bank0: false }
    }

    pub fn ea_dp_long_y<B: CpuBus>(&mut self, bus: &mut B) -> Ea {
        let off = self.fetch8(bus) as u16;
        if self.dl_nonzero() {
            bus.idle();
        }
        let ptr = self.dp_ptr24(bus, off);
        Ea { addr: (ptr + self.y as u32) & 0xFF_FFFF, bank0: false }
    }

    pub fn ea_stack<B: CpuBus>(&mut self, bus: &mut B) -> Ea {
        let off = self.fetch8(bus) as u16;
        bus.idle();
        Ea { addr: self.s.wrapping_add(off) as u32, bank0: true }
    }

    pub fn ea_stack_ind_y<B: CpuBus>(&mut self, bus: &mut B) -> Ea {
        let off = self.fetch8(bus) as u16;
        bus.idle();
        let base = self.s.wrapping_add(off);
        let lo = bus.read(base as u32) as u16;
        let hi = bus.read(base.wrapping_add(1) as u32) as u16;
        let ptr = lo | (hi << 8);
        bus.idle();
        let eff = (((self.dbr as u32) << 16) + ptr as u32 + self.y as u32) & 0xFF_FFFF;
        Ea { addr: eff, bank0: false }
    }

    // ---- Width-aware data access via a resolved Ea ----

    fn hi_addr(&self, ea: Ea) -> u32 {
        if ea.bank0 {
            (ea.addr as u16).wrapping_add(1) as u32
        } else {
            (ea.addr + 1) & 0xFF_FFFF
        }
    }

    /// Load 8 or 16 bits (accumulator/memory width, P.M).
    pub fn load_m<B: CpuBus>(&mut self, bus: &mut B, ea: Ea) -> u16 {
        if self.p.m() {
            bus.read(ea.addr) as u16
        } else {
            let lo = bus.read(ea.addr) as u16;
            let hi = bus.read(self.hi_addr(ea)) as u16;
            lo | (hi << 8)
        }
    }

    /// Load 8 or 16 bits (index width, P.X).
    pub fn load_x<B: CpuBus>(&mut self, bus: &mut B, ea: Ea) -> u16 {
        if self.p.x() {
            bus.read(ea.addr) as u16
        } else {
            let lo = bus.read(ea.addr) as u16;
            let hi = bus.read(self.hi_addr(ea)) as u16;
            lo | (hi << 8)
        }
    }

    pub fn store_m<B: CpuBus>(&mut self, bus: &mut B, ea: Ea, v: u16) {
        if self.p.m() {
            bus.write(ea.addr, v as u8);
        } else {
            bus.write(ea.addr, v as u8);
            bus.write(self.hi_addr(ea), (v >> 8) as u8);
        }
    }

    pub fn store_x<B: CpuBus>(&mut self, bus: &mut B, ea: Ea, v: u16) {
        if self.p.x() {
            bus.write(ea.addr, v as u8);
        } else {
            bus.write(ea.addr, v as u8);
            bus.write(self.hi_addr(ea), (v >> 8) as u8);
        }
    }
}
