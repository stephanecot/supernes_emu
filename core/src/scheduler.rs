//! Master-clock scheduler: single u64 clock, one `next_event` timestamp checked
//! per memory access; events are end-of-scanline boundaries. Also tracks the
//! NMI/H-V-IRQ latches and the auto-joypad trigger pulse that the bus consumes
//! to give $4200-$4212 real semantics (timing.md ยง4-8).

/// One scanline = 1364 master cycles (both regions).
pub const CYCLES_PER_LINE: u64 = 1364;

/// Default vblank/NMI line (V=225) in both regions. Overscan ($2133 bit2)
/// moves it to V=240; see `Scheduler::vblank_line` / `set_overscan`.
pub const NMI_LINE: u16 = 225;

/// Vblank/NMI line when overscan ($2133 bit2) selects 239 visible lines
/// (timing.md ยง2/ยง4).
pub const OVERSCAN_NMI_LINE: u16 = 240;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Region {
    Ntsc,
    Pal,
}

impl Region {
    pub fn lines_per_frame(self) -> u16 {
        match self {
            Region::Ntsc => 262,
            Region::Pal => 312,
        }
    }

    pub fn master_clock_hz(self) -> u32 {
        match self {
            Region::Ntsc => 21_477_272,
            Region::Pal => 21_281_370,
        }
    }

    pub fn frames_per_second(self) -> f64 {
        self.master_clock_hz() as f64 / (self.lines_per_frame() as f64 * CYCLES_PER_LINE as f64)
    }
}

#[derive(Serialize, Deserialize)]
pub struct Scheduler {
    pub region: Region,
    /// Master clock, monotonically increasing for the lifetime of the console.
    pub clock: u64,
    /// Timestamp of the next end-of-line event.
    pub next_event: u64,
    /// Current scanline (V). V=0 is the pre-render line; visible picture is V=1..=224.
    pub v: u16,
    /// Scanline at which vblank starts / the NMI-occurred flag is set / the
    /// auto-joypad read begins: 225 normally, 240 in overscan ($2133 bit2).
    /// The bus updates this on $2133 writes (timing.md ยง2/ยง4).
    pub vblank_line: u16,
    /// Master-clock timestamp at which the current line started.
    pub line_start: u64,
    /// Set when V wraps to 0 (frame complete); consumer clears it.
    pub frame_done: bool,
    /// NMI latch consumed by `CpuBus::take_nmi`: the CPU's edge-triggered
    /// /NMI line. Set on the 0->1 transition of (nmi_enable AND
    /// vblank_nmi_flag) (timing.md ยง5); cleared when the CPU takes it.
    pub nmi_pending: bool,
    /// $4212.7 VBlank flag mirror: set H=0 at V=225, cleared H=0 at V=0
    /// (real hardware semantics, independent of NMI enable).
    pub in_vblank: bool,
    /// $4200.7 mirror: NMI enable, set by the bus on every $4200 write.
    pub nmi_enable: bool,
    /// $4210.7 "NMI occurred" latch: set unconditionally at the start of
    /// vblank (V=225) even if NMI is disabled; auto-cleared at V=0 H=0 and
    /// read-cleared by the bus on $4210 reads. Distinct from `nmi_pending`,
    /// which is the CPU-taken edge latch (timing.md ยง5).
    pub vblank_nmi_flag: bool,
    /// Pulses true for one `tick()` call when V reaches 225 (vblank start);
    /// the bus consumes it to run the auto-joypad latch hook.
    pub auto_joypad_pending: bool,
    /// Pulses true for one `tick()` call whenever a scanline boundary was
    /// crossed; the bus consumes it to drive one APU catch-up per line.
    pub line_boundary_crossed: bool,
    /// $4200 bits5-4 mirror: 0=off, 1=H-IRQ every line, 2=V-IRQ once/frame,
    /// 3=HV-IRQ once/frame (mmio.md ยง7, timing.md ยง6).
    pub irq_mode: u8,
    /// $4207/$4208 HTIME, 9-bit (0..339).
    pub htime: u16,
    /// $4209/$420A VTIME, 9-bit (0..261 NTSC / 0..311 PAL).
    pub vtime: u16,
    /// $4211.7 TIMEUP latch: level-held until read-ack (`$4211` read) or
    /// until $4200 bits5-4 are set to 0 (mmio.md ยง8, timing.md ยง6).
    pub irq_pending: bool,
    /// Absolute master-clock timestamp of the next H/V-IRQ trigger for the
    /// current line/frame, or `None` if the configured mode doesn't fire
    /// this line. Re-armed once per line in `end_of_line` and whenever the
    /// mode/HTIME/VTIME registers are written.
    irq_target: Option<u64>,
}

impl Scheduler {
    pub fn new(region: Region) -> Self {
        Scheduler {
            region,
            clock: 0,
            next_event: CYCLES_PER_LINE,
            v: 0,
            vblank_line: NMI_LINE,
            line_start: 0,
            frame_done: false,
            nmi_pending: false,
            in_vblank: false,
            nmi_enable: false,
            vblank_nmi_flag: false,
            auto_joypad_pending: false,
            line_boundary_crossed: false,
            irq_mode: 0,
            htime: 0,
            vtime: 0,
            irq_pending: false,
            irq_target: None,
        }
    }

    /// Advance the master clock by `cycles`, process any line boundaries
    /// crossed, and check the armed H/V-IRQ target. `check_irq` runs before
    /// each `end_of_line` so a target armed for the line being left is not
    /// silently overwritten by `rearm_irq` before it can fire.
    pub fn tick(&mut self, cycles: u64) {
        self.clock += cycles;
        while self.clock >= self.next_event {
            self.check_irq();
            self.end_of_line();
        }
        self.check_irq();
    }

    fn end_of_line(&mut self) {
        self.line_start = self.next_event;
        self.next_event += CYCLES_PER_LINE;
        self.v += 1;
        self.line_boundary_crossed = true;
        if self.v == self.vblank_line {
            // H=0, V=225/240: vblank flag set unconditionally (timing.md ยง4/ยง7).
            self.in_vblank = true;
            let was_set = self.vblank_nmi_flag;
            // H=0.5: NMI-occurred flag set unconditionally, even if NMI is
            // disabled at $4200.7 (timing.md ยง5).
            self.vblank_nmi_flag = true;
            if !was_set && self.nmi_enable {
                // 0->1 edge of (nmi_enable AND vblank_nmi_flag): NMI enable
                // was already set when the flag rose, so the edge fires here.
                self.nmi_pending = true;
            }
            // Auto-joypad read begins within this vblank line (timing.md
            // ยง8); the bus decides whether $4200.0 actually enables it.
            self.auto_joypad_pending = true;
        }
        if self.v >= self.region.lines_per_frame() {
            self.v = 0;
            self.in_vblank = false;
            // H=0, V=0: NMI-occurred flag auto-clears (timing.md ยง5).
            self.vblank_nmi_flag = false;
            self.frame_done = true;
        }
        self.rearm_irq();
    }

    fn check_irq(&mut self) {
        if let Some(target) = self.irq_target {
            if self.clock >= target {
                self.irq_pending = true;
                // One-shot for this line; re-armed by the next `end_of_line`
                // (H-mode) or the next matching V (V/HV-mode).
                self.irq_target = None;
            }
        }
    }

    /// Compute the H/V-IRQ trigger timestamp for the current line from
    /// `irq_mode`/`htime`/`vtime`, or `None` if the mode does not fire this
    /// line. Per-line granularity: does not model the sub-line 4-8 cycle
    /// read-ack window of timing.md ยง6.
    fn compute_irq_target(&self) -> Option<u64> {
        match self.irq_mode {
            0 => None,
            1 => Some(self.line_start + Self::h_irq_offset(self.htime)),
            2 => {
                if self.v == self.vtime {
                    Some(self.line_start + Self::h_irq_offset(0))
                } else {
                    None
                }
            }
            3 => {
                if self.v == self.vtime {
                    Some(self.line_start + Self::h_irq_offset(self.htime))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// End-of-line rearm: unconditional. Must arm even a target that is already
    /// <= `clock`, because a long/DMA `tick` can jump `clock` past the trigger
    /// point of the line just entered; `check_irq` then fires it correctly.
    fn rearm_irq(&mut self) {
        self.irq_target = self.compute_irq_target();
    }

    /// Register-write rearm ($4200 mode / HTIME / VTIME). The H/V comparator
    /// matches the live counter: once H has already passed the (new) trigger
    /// position on the current line, no match occurs until H wraps at the next
    /// scanline (timing.md ยง6). Arm `None` when the computed trigger is strictly
    /// in the past this line so that writing a smaller HTIME mid-line does not
    /// spuriously retrigger; the unconditional `end_of_line` rearm handles all
    /// subsequent lines. A trigger landing exactly on `clock` still fires
    /// (timing.md ยง6: "enabling IRQs exactly on the trigger cycle still fires").
    fn rearm_irq_write(&mut self) {
        self.irq_target = match self.compute_irq_target() {
            Some(t) if t < self.clock => None,
            other => other,
        };
    }

    /// Master-cycle offset from line start to the H/V-IRQ trigger point.
    /// anomie's formula (timing.md ยง6): H=0 fires 1374 cycles after the
    /// *previous* line's dot 0 (= 1374 - CYCLES_PER_LINE = 10 cycles into
    /// the current line); H>0 fires 14 + H*4 cycles into the current line.
    fn h_irq_offset(htime: u16) -> u64 {
        if htime == 0 {
            1374 - CYCLES_PER_LINE
        } else {
            14 + htime as u64 * 4
        }
    }

    /// $4200 bit7 mirror, set by the bus. Detects the mid-vblank enable edge:
    /// if `vblank_nmi_flag` is already set when NMI is enabled, the
    /// (enable AND flag) product rises 0->1 here and must fire immediately
    /// (timing.md ยง5).
    pub fn set_nmi_enable(&mut self, enable: bool) {
        if !self.nmi_enable && enable && self.vblank_nmi_flag {
            self.nmi_pending = true;
        }
        self.nmi_enable = enable;
    }

    /// $2133 bit2 (overscan) mirror, set by the bus. Moves vblank start / the
    /// NMI-occurred flag / the auto-joypad read from V=225 to V=240 (timing.md
    /// ยง2/ยง4).
    pub fn set_overscan(&mut self, overscan: bool) {
        self.vblank_line = if overscan { OVERSCAN_NMI_LINE } else { NMI_LINE };
    }

    /// $4200 bits5-4. Setting mode 0 (disabled) also acknowledges a pending
    /// TIMEUP flag (mmio.md ยง7).
    pub fn set_irq_mode(&mut self, mode: u8) {
        self.irq_mode = mode & 0x3;
        if self.irq_mode == 0 {
            self.irq_pending = false;
        }
        self.rearm_irq_write();
    }

    pub fn set_htime_lo(&mut self, value: u8) {
        self.htime = (self.htime & 0x100) | value as u16;
        self.rearm_irq_write();
    }

    pub fn set_htime_hi(&mut self, value: u8) {
        self.htime = (self.htime & 0x0FF) | (((value & 1) as u16) << 8);
        self.rearm_irq_write();
    }

    pub fn set_vtime_lo(&mut self, value: u8) {
        self.vtime = (self.vtime & 0x100) | value as u16;
        self.rearm_irq_write();
    }

    pub fn set_vtime_hi(&mut self, value: u8) {
        self.vtime = (self.vtime & 0x0FF) | (((value & 1) as u16) << 8);
        self.rearm_irq_write();
    }

    /// Horizontal position in master cycles within the current line.
    pub fn h_cycles(&self) -> u64 {
        self.clock - self.line_start
    }

    /// $4212.6 HBlank flag: set at H=274 (dot), cleared at H=1; toggles on
    /// every line, including during vblank/forced blank (timing.md ยง7).
    /// 1 dot = 4 master cycles.
    pub fn hblank(&self) -> bool {
        let h = self.h_cycles();
        h < 4 || h >= 274 * 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pal_frame_terminates_and_latches_nmi() {
        let mut s = Scheduler::new(Region::Pal);
        s.set_nmi_enable(true);
        while !s.frame_done {
            s.tick(CYCLES_PER_LINE);
        }
        assert!(s.nmi_pending);
        assert_eq!(s.v, 0);
        assert_eq!(s.clock, 312 * CYCLES_PER_LINE);
    }

    #[test]
    fn ntsc_frame_length() {
        let mut s = Scheduler::new(Region::Ntsc);
        while !s.frame_done {
            s.tick(100);
        }
        assert!(s.clock >= 262 * CYCLES_PER_LINE);
    }

    #[test]
    fn nmi_disabled_sets_flag_but_not_cpu_latch() {
        let mut s = Scheduler::new(Region::Pal);
        for _ in 0..NMI_LINE {
            s.tick(CYCLES_PER_LINE);
        }
        assert!(s.vblank_nmi_flag);
        assert!(!s.nmi_pending);
    }

    #[test]
    fn mid_vblank_enable_edge_triggers_nmi() {
        let mut s = Scheduler::new(Region::Pal);
        for _ in 0..NMI_LINE {
            s.tick(CYCLES_PER_LINE);
        }
        assert!(s.vblank_nmi_flag && !s.nmi_pending);
        // Enabling NMI while the vblank flag is already set is a 0->1 edge:
        // it must fire immediately (timing.md ยง5).
        s.set_nmi_enable(true);
        assert!(s.nmi_pending);
    }

    #[test]
    fn vblank_nmi_flag_autoclears_at_frame_wrap() {
        let mut s = Scheduler::new(Region::Pal);
        while !s.frame_done {
            s.tick(CYCLES_PER_LINE);
        }
        assert!(!s.vblank_nmi_flag);
        assert!(!s.in_vblank);
    }

    #[test]
    fn auto_joypad_pulses_once_at_vblank_start() {
        let mut s = Scheduler::new(Region::Pal);
        let mut pulses = 0;
        for _ in 0..NMI_LINE {
            s.tick(CYCLES_PER_LINE);
            if s.auto_joypad_pending {
                pulses += 1;
                s.auto_joypad_pending = false;
            }
        }
        assert_eq!(pulses, 1);
    }

    #[test]
    fn hblank_toggles_within_a_line() {
        let mut s = Scheduler::new(Region::Pal);
        assert!(s.hblank()); // H=0: leftover set state from before line 0
        s.tick(4);
        assert!(!s.hblank()); // H=1: cleared
        s.tick(274 * 4 - 4 - 1);
        assert!(!s.hblank()); // just before H=274
        s.tick(1);
        assert!(s.hblank()); // H=274: set
    }

    #[test]
    fn h_irq_fires_every_line() {
        let mut s = Scheduler::new(Region::Pal);
        s.set_htime_lo(10); // HTIME = 10 dots
        s.set_htime_hi(0);
        s.set_irq_mode(1); // H-IRQ, every line
        s.tick(14 + 10 * 4); // reach the trigger offset within line 0
        assert!(s.irq_pending);
        s.irq_pending = false;
        s.tick(CYCLES_PER_LINE); // next line: fires again
        assert!(s.irq_pending);
    }

    #[test]
    fn v_irq_fires_once_per_frame_at_target_line() {
        let mut s = Scheduler::new(Region::Pal);
        s.set_vtime_lo(5);
        s.set_vtime_hi(0);
        s.set_irq_mode(2); // V-IRQ
        for _ in 0..5 {
            s.tick(CYCLES_PER_LINE);
            assert!(!s.irq_pending);
        }
        s.tick(20); // past the H=~2.5-dot trigger point of line 5
        assert!(s.irq_pending);
    }

    #[test]
    fn rewriting_past_htime_midline_does_not_retrigger() {
        let mut s = Scheduler::new(Region::Pal);
        s.set_htime_lo(10); // trigger at 14 + 10*4 = 54 cycles into the line
        s.set_htime_hi(0);
        s.set_irq_mode(1);
        s.tick(54);
        assert!(s.irq_pending);
        s.irq_pending = false;
        // Mid-line, past the trigger position: rewriting a <= current HTIME must
        // NOT arm a past timestamp that re-fires this line (timing.md ยง6).
        s.tick(100); // now ~154 cycles into the line, past H=10's trigger
        s.set_htime_lo(5); // trigger would be at 34 cycles, already passed
        s.tick(1);
        assert!(!s.irq_pending);
        // The next line's unconditional rearm still fires H-mode.
        s.tick(CYCLES_PER_LINE);
        assert!(s.irq_pending);
    }

    #[test]
    fn rewriting_future_htime_midline_arms_this_line() {
        let mut s = Scheduler::new(Region::Pal);
        s.set_htime_lo(10);
        s.set_htime_hi(0);
        s.set_irq_mode(1);
        s.tick(54);
        assert!(s.irq_pending);
        s.irq_pending = false;
        // Still before a later trigger position: rewriting a larger HTIME arms a
        // future same-line match, which the live comparator would still hit.
        s.tick(20); // ~74 cycles into the line
        s.set_htime_lo(200); // trigger at 14 + 200*4 = 814 cycles
        s.tick(814 - 74);
        assert!(s.irq_pending);
    }

    #[test]
    fn disabling_irq_mode_acks_pending_timeup() {
        let mut s = Scheduler::new(Region::Pal);
        s.set_irq_mode(1);
        s.tick(CYCLES_PER_LINE);
        assert!(s.irq_pending);
        s.set_irq_mode(0);
        assert!(!s.irq_pending);
    }
}
