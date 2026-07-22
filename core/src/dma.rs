//! GDMA + HDMA channel register decode, 8 channels ($43x0-$43xA, x = 0-7).
//! Register storage and typed accessors live here; the transfer LOOP (byte
//! copy, HDMA scanline stepping, CPU cycle stalls) lives in `bus.rs` to avoid
//! borrowing both `Dma` and the rest of the bus mutably at once.
//!
//! Register map (mmio.md #9), reset value $FF except $43x4 (undefined):
//! $43x0 DMAPx, $43x1 BBADx, $43x2/3 A1TxL/H, $43x4 A1Bx, $43x5/6 DASxL/H,
//! $43x7 DASBx, $43x8/9 A2AxL/H, $43xA NLTRx, $43xB-$43xF unused/mirror.

/// One of the 8 B-bus transfer-unit patterns selected by DMAPx bits 2-0.
/// Values are B-bus address offsets from BBADx, one entry per byte
/// transferred (HDMA: one *unit*, i.e. the whole pattern, per scanline).
/// Source: mmio.md #9 "Transfer unit patterns".
const TRANSFER_PATTERNS: [&[u8]; 8] = [
    &[0],          // mode 0: 1 byte  - xx            (WRAM $2180, $2118-only)
    &[0, 1],       // mode 1: 2 bytes - xx, xx+1       (VRAM $2118/$2119)
    &[0, 0],       // mode 2: 2 bytes - xx, xx         (OAM $2104, CGRAM $2122)
    &[0, 0, 1, 1], // mode 3: 4 bytes - xx,xx,xx+1,xx+1 (BGnxOFS, M7x pairs)
    &[0, 1, 2, 3], // mode 4: 4 bytes - xx,xx+1,xx+2,xx+3 (BGnSC, windows, APU)
    &[0, 1, 0, 1], // mode 5: 4 bytes - xx,xx+1,xx,xx+1 (undocumented)
    &[0, 0],       // mode 6: 2 bytes - same pattern as mode 2
    &[0, 0, 1, 1], // mode 7: 4 bytes - same pattern as mode 3
];

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Dma {
    /// $43x0-$43xA per channel (16 bytes reserved per channel; $B-$F mirror).
    pub channels: [[u8; 12]; 8],
    /// $420B GDMA enable / $420C HDMA enable, stored by the bus.
    pub mdmaen: u8,
    pub hdmaen: u8,
    /// HDMA per-channel "still running this frame" bitmask. A channel is
    /// marked active at frame init (V=0) if enabled in $420C; cleared when
    /// its table yields a $00 line-count terminator (timing.md #11).
    pub hdma_active: u8,
    /// HDMA per-channel internal do-transfer flag (whether the current
    /// scanline performs a unit transfer), one bit per channel.
    pub hdma_do_transfer: u8,
}

impl Dma {
    pub fn new() -> Self {
        Dma {
            channels: [[0xFF; 12]; 8],
            mdmaen: 0,
            hdmaen: 0,
            hdma_active: 0,
            hdma_do_transfer: 0,
        }
    }

    /// Read $43xx (offset = addr & 0x7F). `None` for the unused $x B-$xF slots.
    pub fn read(&self, offset: u8) -> Option<u8> {
        let ch = ((offset >> 4) & 7) as usize;
        let reg = (offset & 0x0F) as usize;
        if reg < 12 {
            Some(self.channels[ch][reg])
        } else {
            None
        }
    }

    pub fn write(&mut self, offset: u8, value: u8) {
        let ch = ((offset >> 4) & 7) as usize;
        let reg = (offset & 0x0F) as usize;
        if reg < 12 {
            self.channels[ch][reg] = value;
        }
    }

    // ---- DMAPx ($43x0) ------------------------------------------------

    fn dmap(&self, ch: usize) -> u8 {
        self.channels[ch][0]
    }

    /// DMAPx bit 7: 0 = A-bus -> B-bus (CPU memory to $21xx port), 1 = B-bus
    /// -> A-bus. Same meaning for GDMA and HDMA.
    pub fn direction_a_to_b(&self, ch: usize) -> bool {
        self.dmap(ch) & 0x80 == 0
    }

    /// DMAPx bit 6: HDMA table addressing, 0 = direct, 1 = indirect
    /// (indirect data address/bank taken from DASx/DASBx instead of the
    /// table itself). No effect on GDMA.
    pub fn hdma_indirect(&self, ch: usize) -> bool {
        self.dmap(ch) & 0x40 != 0
    }

    /// DMAPx bits 4-3: A-bus address step per byte transferred, GP-DMA only
    /// (HDMA always walks the table forward by the unit size, never uses
    /// this field). 0 = +1, 2 = -1, 1 and 3 = fixed (0).
    pub fn a_step(&self, ch: usize) -> i16 {
        match (self.dmap(ch) >> 3) & 3 {
            0 => 1,
            2 => -1,
            _ => 0,
        }
    }

    /// DMAPx bits 2-0: transfer unit select (0-7).
    pub fn transfer_unit(&self, ch: usize) -> u8 {
        self.dmap(ch) & 7
    }

    /// B-bus address offset sequence for channel `ch`'s selected transfer
    /// unit, one entry per byte (added to BBADx to form $21xx).
    pub fn transfer_pattern(&self, ch: usize) -> &'static [u8] {
        Self::pattern_for_unit(self.transfer_unit(ch))
    }

    /// Same lookup, addressable directly by unit number (0-7) without a
    /// channel, e.g. for tests.
    pub fn pattern_for_unit(unit: u8) -> &'static [u8] {
        TRANSFER_PATTERNS[(unit & 7) as usize]
    }

    // ---- BBADx ($43x1) -------------------------------------------------

    /// B-bus target low byte; full B-bus address is $21xx with xx = this
    /// value (+ the transfer-pattern offset for the current byte).
    pub fn bbad(&self, ch: usize) -> u8 {
        self.channels[ch][1]
    }

    // ---- A1TxL/H, A1Bx ($43x2-$43x4) -----------------------------------

    /// A-bus bank, fixed for the duration of a GP-DMA transfer. Also the
    /// HDMA table bank (A1Bx is reused for both meanings, mmio.md #9).
    pub fn a1_bank(&self, ch: usize) -> u8 {
        self.channels[ch][4]
    }

    /// A-bus 16-bit offset (A1TxL/H). Updated during GP-DMA as bytes are
    /// transferred; used as the HDMA table start address before the
    /// channel is initialized (copied into A2Ax at HDMA init).
    pub fn a1_offset(&self, ch: usize) -> u16 {
        u16::from_le_bytes([self.channels[ch][2], self.channels[ch][3]])
    }

    pub fn set_a1_offset(&mut self, ch: usize, offset: u16) {
        let [lo, hi] = offset.to_le_bytes();
        self.channels[ch][2] = lo;
        self.channels[ch][3] = hi;
    }

    /// Full 24-bit A-bus address (bank:offset) as a u32.
    pub fn a1_addr(&self, ch: usize) -> u32 {
        ((self.a1_bank(ch) as u32) << 16) | self.a1_offset(ch) as u32
    }

    /// Advance A1TxL/H by `step` (from `a_step`), wrapping within the bank
    /// (the 16-bit A1T register wraps $FFFF -> $0000 without carrying into
    /// A1Bx; GP-DMA never crosses a bank boundary on its own).
    pub fn advance_a1(&mut self, ch: usize, step: i16) {
        let cur = self.a1_offset(ch);
        let next = cur.wrapping_add(step as u16);
        self.set_a1_offset(ch, next);
    }

    // ---- DASxL/H ($43x5-$43x6) ------------------------------------------

    /// Raw 16-bit DASx register: GP-DMA byte counter, or HDMA indirect data
    /// address when DMAPx bit6 = 1.
    pub fn das(&self, ch: usize) -> u16 {
        u16::from_le_bytes([self.channels[ch][5], self.channels[ch][6]])
    }

    pub fn set_das(&mut self, ch: usize, value: u16) {
        let [lo, hi] = value.to_le_bytes();
        self.channels[ch][5] = lo;
        self.channels[ch][6] = hi;
    }

    /// GP-DMA byte count with hardware's $0000 = 65536 special case
    /// (mmio.md #9: "$0000 = 65536 bytes").
    pub fn byte_count(&self, ch: usize) -> u32 {
        let raw = self.das(ch);
        if raw == 0 {
            65536
        } else {
            raw as u32
        }
    }

    // ---- DASBx ($43x7) --------------------------------------------------

    /// HDMA indirect data bank (unused by GP-DMA).
    pub fn dasb(&self, ch: usize) -> u8 {
        self.channels[ch][7]
    }

    pub fn set_dasb(&mut self, ch: usize, value: u8) {
        self.channels[ch][7] = value;
    }

    /// Full 24-bit HDMA indirect data address (DASBx:DASx), valid when
    /// `hdma_indirect(ch)` is set.
    pub fn hdma_indirect_addr(&self, ch: usize) -> u32 {
        ((self.dasb(ch) as u32) << 16) | self.das(ch) as u32
    }

    // ---- A2AxL/H ($43x8-$43x9) ------------------------------------------

    /// HDMA current table address (auto-advances as the table is read);
    /// unused by GP-DMA. Initialized from A1Tx at HDMA setup.
    pub fn a2a(&self, ch: usize) -> u16 {
        u16::from_le_bytes([self.channels[ch][8], self.channels[ch][9]])
    }

    pub fn set_a2a(&mut self, ch: usize, value: u16) {
        let [lo, hi] = value.to_le_bytes();
        self.channels[ch][8] = lo;
        self.channels[ch][9] = hi;
    }

    /// Full 24-bit HDMA table pointer (A1Bx:A2Ax).
    pub fn hdma_table_addr(&self, ch: usize) -> u32 {
        ((self.a1_bank(ch) as u32) << 16) | self.a2a(ch) as u32
    }

    // ---- NLTRx ($43xA) ---------------------------------------------------

    fn nltr(&self, ch: usize) -> u8 {
        self.channels[ch][10]
    }

    /// Raw 8-bit NLTRx (repeat flag in bit7, line counter in bits6-0). HDMA
    /// decrements the full byte each scanline; bit7 doubles as the repeat
    /// flag while bits6-0 count down (timing.md #11).
    pub fn nltr_raw(&self, ch: usize) -> u8 {
        self.channels[ch][10]
    }

    pub fn set_nltr(&mut self, ch: usize, value: u8) {
        self.channels[ch][10] = value;
    }

    // ---- HDMA per-frame runtime state ----------------------------------

    pub fn hdma_channel_active(&self, ch: usize) -> bool {
        self.hdma_active & (1 << ch) != 0
    }

    pub fn set_hdma_channel_active(&mut self, ch: usize, on: bool) {
        if on {
            self.hdma_active |= 1 << ch;
        } else {
            self.hdma_active &= !(1 << ch);
        }
    }

    pub fn hdma_wants_transfer(&self, ch: usize) -> bool {
        self.hdma_do_transfer & (1 << ch) != 0
    }

    pub fn set_hdma_wants_transfer(&mut self, ch: usize, on: bool) {
        if on {
            self.hdma_do_transfer |= 1 << ch;
        } else {
            self.hdma_do_transfer &= !(1 << ch);
        }
    }

    /// NLTRx bits 6-0: HDMA line counter, reloaded from the table entry at
    /// the start of each table line; decremented once per scanline.
    pub fn hdma_line_counter(&self, ch: usize) -> u8 {
        self.nltr(ch) & 0x7F
    }

    /// NLTRx bit 7: repeat flag from the table entry. 0 = the following
    /// table entry (or indirect address) is fetched only once and repeated
    /// for `line_counter` scanlines; 1 = fetched fresh every scanline.
    pub fn hdma_repeat(&self, ch: usize) -> bool {
        self.nltr(ch) & 0x80 != 0
    }
}

impl Default for Dma {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_patterns_match_reference_table() {
        // mmio.md #9 "Transfer unit patterns" table, verified byte-for-byte.
        assert_eq!(Dma::pattern_for_unit(0), &[0]);
        assert_eq!(Dma::pattern_for_unit(1), &[0, 1]);
        assert_eq!(Dma::pattern_for_unit(2), &[0, 0]);
        assert_eq!(Dma::pattern_for_unit(3), &[0, 0, 1, 1]);
        assert_eq!(Dma::pattern_for_unit(4), &[0, 1, 2, 3]);
        assert_eq!(Dma::pattern_for_unit(5), &[0, 1, 0, 1]);
        assert_eq!(Dma::pattern_for_unit(6), &[0, 0]);
        assert_eq!(Dma::pattern_for_unit(7), &[0, 0, 1, 1]);
    }

    #[test]
    fn dmap_direction_and_indirect_bits() {
        let mut dma = Dma::new();
        dma.write(0x00, 0x00); // ch0 DMAP: A->B, direct
        assert!(dma.direction_a_to_b(0));
        assert!(!dma.hdma_indirect(0));

        dma.write(0x00, 0x80); // B->A
        assert!(!dma.direction_a_to_b(0));

        dma.write(0x00, 0x40); // indirect table
        assert!(dma.hdma_indirect(0));
    }

    #[test]
    fn a_step_decode() {
        let mut dma = Dma::new();
        dma.write(0x00, 0x00); // bits4-3 = 00 -> +1
        assert_eq!(dma.a_step(0), 1);
        dma.write(0x00, 0x08); // bits4-3 = 01 (bit3) -> fixed
        assert_eq!(dma.a_step(0), 0);
        dma.write(0x00, 0x10); // bits4-3 = 10 (bit4) -> -1
        assert_eq!(dma.a_step(0), -1);
        dma.write(0x00, 0x18); // bits4-3 = 11 (bit3+bit4) -> fixed
        assert_eq!(dma.a_step(0), 0);
    }

    #[test]
    fn transfer_unit_and_pattern_per_channel() {
        let mut dma = Dma::new();
        dma.write(0x10 * 3, 0x05); // ch3 DMAP unit = 5
        assert_eq!(dma.transfer_unit(3), 5);
        assert_eq!(dma.transfer_pattern(3), &[0, 1, 0, 1]);
    }

    #[test]
    fn bbad_readback() {
        let mut dma = Dma::new();
        dma.write(0x01, 0x18); // ch0 BBAD = $18 -> $2118 (VRAM data)
        assert_eq!(dma.bbad(0), 0x18);
    }

    #[test]
    fn a1_address_and_stepping() {
        let mut dma = Dma::new();
        // ch1 A1T = $1234, A1B = $7E
        dma.write(0x10 + 2, 0x34);
        dma.write(0x10 + 3, 0x12);
        dma.write(0x10 + 4, 0x7E);
        assert_eq!(dma.a1_bank(1), 0x7E);
        assert_eq!(dma.a1_offset(1), 0x1234);
        assert_eq!(dma.a1_addr(1), 0x7E1234);

        dma.advance_a1(1, 1);
        assert_eq!(dma.a1_offset(1), 0x1235);
        assert_eq!(dma.a1_bank(1), 0x7E); // bank untouched by offset wrap

        dma.set_a1_offset(1, 0xFFFF);
        dma.advance_a1(1, 1);
        assert_eq!(dma.a1_offset(1), 0x0000); // wraps within bank, no carry
        assert_eq!(dma.a1_bank(1), 0x7E);

        dma.advance_a1(1, -1);
        assert_eq!(dma.a1_offset(1), 0xFFFF);
    }

    #[test]
    fn byte_count_zero_means_65536() {
        let mut dma = Dma::new();
        dma.set_das(0, 0);
        assert_eq!(dma.byte_count(0), 65536);
        dma.set_das(0, 1);
        assert_eq!(dma.byte_count(0), 1);
        dma.set_das(0, 0x1000);
        assert_eq!(dma.byte_count(0), 0x1000);

        dma.set_das(0, 5);
        dma.set_das(0, 4); // decrement, as the bus loop would each byte
        assert_eq!(dma.das(0), 4);
    }

    #[test]
    fn hdma_indirect_and_table_addresses() {
        let mut dma = Dma::new();
        dma.write(0x04, 0x9A); // ch0 A1Bx = table bank $9A
        dma.set_a2a(0, 0x3456);
        assert_eq!(dma.hdma_table_addr(0), 0x9A3456);

        dma.set_dasb(0, 0x7E);
        dma.set_das(0, 0xABCD);
        assert_eq!(dma.hdma_indirect_addr(0), 0x7EABCD);
    }

    #[test]
    fn nltr_line_counter_and_repeat_flag() {
        let mut dma = Dma::new();
        dma.set_nltr(2, 0x85); // bit7 set, count = 5
        assert!(dma.hdma_repeat(2));
        assert_eq!(dma.hdma_line_counter(2), 5);

        dma.set_nltr(2, 0x05); // bit7 clear
        assert!(!dma.hdma_repeat(2));
        assert_eq!(dma.hdma_line_counter(2), 5);
    }

    #[test]
    fn hdma_runtime_state_bits() {
        let mut dma = Dma::new();
        assert!(!dma.hdma_channel_active(3));
        dma.set_hdma_channel_active(3, true);
        assert!(dma.hdma_channel_active(3));
        assert_eq!(dma.hdma_active, 1 << 3);
        dma.set_hdma_channel_active(3, false);
        assert!(!dma.hdma_channel_active(3));

        dma.set_hdma_wants_transfer(7, true);
        assert!(dma.hdma_wants_transfer(7));
        assert_eq!(dma.hdma_do_transfer, 1 << 7);
        dma.set_hdma_wants_transfer(7, false);
        assert!(!dma.hdma_wants_transfer(7));

        dma.set_nltr(0, 0x83);
        assert_eq!(dma.nltr_raw(0), 0x83);
    }
}
