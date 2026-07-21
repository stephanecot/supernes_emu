//! Standard controllers: $4016/$4017 serial protocol + shared OUT0 strobe.
//! The bus composes the open-bus/driven upper bits and drives the $4218-$421F
//! auto-read snapshot; this module only holds the per-pad shift state.

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct JoypadState {
    pub a: bool,
    pub b: bool,
    pub x: bool,
    pub y: bool,
    pub l: bool,
    pub r: bool,
    pub start: bool,
    pub select: bool,
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
}

impl JoypadState {
    /// Hardware serial order (bit 15 first): B Y Select Start Up Down Left Right
    /// A X L R 0 0 0 0.
    pub fn to_bits(self) -> u16 {
        let mut v = 0u16;
        let mut push = |b: bool| {
            v = (v << 1) | b as u16;
        };
        push(self.b);
        push(self.y);
        push(self.select);
        push(self.start);
        push(self.up);
        push(self.down);
        push(self.left);
        push(self.right);
        push(self.a);
        push(self.x);
        push(self.l);
        push(self.r);
        v << 4
    }
}

pub struct Joypad {
    pub state: JoypadState,
    /// $4016 bit0 (OUT0) latch line. While high the controller is continuously
    /// parallel-loaded and every read returns the first serial bit (B).
    pub strobe: bool,
    /// Snapshot taken while OUT0 was high; serial reads shift out of it MSB-first
    /// (bit15 = B) once OUT0 goes low.
    latched: u16,
    /// Number of serial bits already clocked out (0..=16); after 16 a standard
    /// pad drives the data line to 1.
    index: u8,
}

impl Joypad {
    pub fn new() -> Self {
        Joypad { state: JoypadState::default(), strobe: false, latched: 0, index: 0 }
    }

    /// $4016/$4017 read: returns the data1 serial line (0 or 1) in bit0. Bit1
    /// (data2/multitap) and the open-bus/driven upper bits are handled by the
    /// bus. Reading clocks the shift register one position while OUT0 is low.
    pub fn read(&mut self) -> u8 {
        if self.strobe {
            // OUT0 high: register is re-loaded every cycle, so the first bit
            // (B) is presented continuously.
            return ((self.state.to_bits() >> 15) & 1) as u8;
        }
        let bit = if self.index < 16 {
            (self.latched >> (15 - self.index)) & 1
        } else {
            1
        };
        self.index = self.index.saturating_add(1);
        bit as u8
    }

    /// Model the auto-joypad read's effect on the shared serial line: the pad
    /// is strobed and physically clocked 16 times, so afterwards the manual
    /// shift register is exhausted (index = 16, subsequent manual reads see 1)
    /// (timing.md ยง8). Called by the bus when it latches the auto-read result.
    pub fn auto_read_shift(&mut self) {
        self.latched = self.state.to_bits();
        self.index = 16;
    }

    /// $4016 write (bit0 = OUT0/latch). While high the current pad state is
    /// snapshotted and the shift position reset to bit0 (= B).
    pub fn write_strobe(&mut self, value: u8) {
        let s = value & 1 != 0;
        if s {
            self.latched = self.state.to_bits();
            self.index = 0;
        }
        self.strobe = s;
    }
}

impl Default for Joypad {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_shift_order() {
        let mut jp = Joypad::new();
        jp.state = JoypadState { b: true, right: true, ..Default::default() };
        jp.write_strobe(1);
        jp.write_strobe(0);
        // 12 button bits in order B Y Sel St Up Dn Lf Rt A X L R.
        let expect = [1, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        for (i, &e) in expect.iter().enumerate() {
            assert_eq!(jp.read(), e, "button bit {i}");
        }
        // Signature bits 12-15 are 0.
        for i in 0..4 {
            assert_eq!(jp.read(), 0, "signature bit {i}");
        }
        // After 16 clocks a standard pad drives the line to 1 indefinitely.
        assert_eq!(jp.read(), 1);
        assert_eq!(jp.read(), 1);
    }

    #[test]
    fn strobe_high_presents_first_bit() {
        let mut jp = Joypad::new();
        jp.state = JoypadState { b: true, ..Default::default() };
        jp.write_strobe(1);
        assert_eq!(jp.read(), 1);
        assert_eq!(jp.read(), 1);
        jp.state = JoypadState { b: false, y: true, ..Default::default() };
        jp.write_strobe(1);
        // B released -> first serial bit is now 0.
        assert_eq!(jp.read(), 0);
    }

    #[test]
    fn relatch_resets_shift_position() {
        let mut jp = Joypad::new();
        jp.state = JoypadState { b: true, ..Default::default() };
        jp.write_strobe(1);
        jp.write_strobe(0);
        assert_eq!(jp.read(), 1); // B
        assert_eq!(jp.read(), 0); // Y
        jp.write_strobe(1);
        jp.write_strobe(0);
        assert_eq!(jp.read(), 1); // back to B
    }
}
