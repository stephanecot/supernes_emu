# Punch-list — carried-over accuracy items

Minor findings from adversarial verification + gate notes, deferred to the milestone where they matter. None block current milestones; fold each into the noted phase.

## For M5 (input) — bus.rs / joypad.rs
- `$4212` bit0 (auto-joypad busy) hardwired to 0 — should be set for ~4224 master cycles (~3.1 lines) from auto-read start at vblank.
- Auto-joypad snapshots pads whenever `$4200` bit0=1 regardless of `$4016` strobe — real hardware only auto-reads when OUT0 (strobe) = 0.
- `$4016` bits7-2 / `$4017` bits7-5 should read open-bus (prior MDR); `$4017` bits4-2 always driven; currently returns raw joypad read.
- `$4213` RDIO bits5-0 should loop back `$4201` (WRIO), not CPU open bus.

## For CPU (fix opportunistically — foundational)
- `push8`/`pull8` unconditionally re-impose page-1 wrap; the "new" 65C816 stack ops (PEA/PEI/PER/PHD/PLD/JSL/RTL, stack-relative) must NOT wrap to page 1 in native mode with 16-bit stack. Potential real bug for deep-stack games.
- COP in emulation mode pushes B=1 like BRK; reference documents only BRK pushing B=1 (IRQ/NMI push B=0). Verify COP behavior.
- JSL ($22) operand/push ordering: hardware fetches AAL,AAH, pushes PBR, internal cycle, THEN fetches AAB. Current code fetches all 3 first. Cycle-order only; result correct.

## For M8 (audio) — apu
- SPC CONTROL power-on value should be `$80` (bit7 IPL enable set); currently 0 with `ipl_enabled` tracked separately (harmless now).
- Timer enable 0→1 transition should reset only stage-2 counter + 4-bit TnOUT, not the stage-1 prescaler.
- `Apu::reset()` should restore CONTROL to power-on ($80) too, not only re-vector the SPC.

## For M6/M3 — ppu timing
- STAT78 `$213F` reports fixed PPU2 version 1; does not toggle interlace-field (bit7) or counter-latch (bit6), and reading it does not reset OPHCT/OPVCT flip-flops. Implement with H/V counter latches.
- NTSC short-line (1360 cycles at V=240) and overscan-shifted vblank/NMI line ($2133 bit2 → V=240) not modeled; frame length drifts ~1 dot/frame. Cosmetic for now.

## M3–M5 status (verified) + the color-math dependency
- The BG/OBJ rendering engine is complete and proven: SMAS intro renders the Nintendo logo + gold Mario medallion correctly at frame ~120 (main_screen=0x10, OBJ-only). BG tile decode is unit-tested (2/4/8bpp, flips, scroll, Mode 0 offset).
- The SMAS **outer All-Stars menu** goes black from ~frame 240 NOT because of a rendering bug. Diagnosed: at a black frame, forced_blank=0, brightness=15, bg_mode=3, main_screen=0x02 (BG2), sub_screen=0x11 (BG1+OBJ), VRAM 67% full, OAM full, CGRAM has a gradient. The menu composites **main (BG2) + sub (BG1+OBJ) via color math** ($2131 CGADSUB=0x20, $2130 CGWSEL, $2132 COLDATA all written heavily). Our compositor renders only the main screen → the subscreen graphics are invisible → black.
- CONCLUSION: this screen is gated on **M6 color math + subscreen compositing**, not on M3/M4. The M6 workflow must add subscreen compositing; re-gate the SMAS menu there. If the menu is STILL black after color math lands, then (and only then) suspect BG decode on real HiROM data.

## M6 status (verified PASS) + M7
- M6 color math + subscreen compositing + windows + HDMA + mosaic: DONE and visually verified. SMAS "SELECT GAME" menu renders in full color (was black); Secret of Mana title + layered French intro render correctly (HiROM + HDMA). 169 core tests pass.
- M7 (Mode 7): code-complete in ppu/mode7.rs + 3 unit tests, but NOT gated on a real in-game screen — neither SMAS nor SoM reaches Mode 7 in a headless budget (SoM's Flammie world map is deep in gameplay). Revisit opportunistically: script SMW to a Bowser fight (Mode 7) or a longer SoM run.

## SMW attract-mode intro hang (KNOWN ISSUE, narrowed, unsolved)
Super Mario World reaches gameplay (Mario runs/jumps, camera scrolls — proven) but its attract-mode INTRO cutscene ("Welcome! This is Dinosaur Land… Bowser is at it again!") never advances to the overworld.

Established (not the bug): CPU alive; per-frame NMI-sync flag $00:0010 set/consumed normally; NMI every frame; H/V IRQ taken (vector $00:FFEE, VTIME splits 55/36, NMITIMEN toggles $A1/$81); auto-joypad + input all work. So NMI/IRQ/input are NOT the cause — the intro state machine advances each frame but its completion condition is never met.

Narrowed to: the intro-advance gate depends on WRAM $1426 and $13BF (and $13D2 downstream). At the hang, the dump shows $1426=1 while the ROM's decision logic given ($1426=1, $13BF=0) correctly yields $13D2=0 — so the divergence is UPSTREAM in how $1426/$13BF get set during message setup. The write of $1426=1 comes from a bank not yet watched (00/30/35 were only seen writing them =0). Hot intro-handler addresses: 30:8E0C-8E30, 30:AE4A-AE4E.

Next step for a fresh session (with stable infra): --watch $00:1426 and $00:13BF across ALL banks during the message-setup window, find the instruction that writes $1426=1, and trace back what condition it reflects (likely a mistimed PPU/IRQ/APU event or an open-bus/counter-latch read the message-setup polls). Three automated debug attempts were killed by API/infra errors mid-investigation, not by lack of a lead.

Impact: also blocks the Mode 7 real-screen gate (SMW's Mode 7 is the Bowser fight, behind this intro).

## Tooling
- `--trace-spc` is a no-op: expose an SPC700 trace hook in the APU core (needed for M8 debugging). DO THIS IN M8.
- `--log-mmio` matches on low-16-bits only, so WRAM shadow writes at `$7E/$7F:21xx`/`:42xx` are logged as FAKE `$21xx/$42xx` register events — actively misleading. Fix: only log when the access is to a real mapped register bank ($00-$3F/$80-$BF). DO THIS IN M8 (audio debugging depends on trustworthy MMIO logs).
- Frontend prepends `target/debug-out/` to `--trace`/`--dump-frame`; pass BARE filenames to avoid doubled paths.
