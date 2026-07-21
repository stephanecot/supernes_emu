# SNES Timing Reference

Sources: fullsnes (problemkaputt.de/fullsnes.htm), snes.nesdev.org/wiki/Timing, snes.nesdev.org/wiki/DMA, anomie's SNES Timing Doc rev 1126.

## 1. Master clocks and derived clocks

| Clock | NTSC | PAL |
|---|---|---|
| Master clock | 21477272.7 Hz (315/88 MHz x6 = 945/44 MHz; fullsnes: 21.4772700 MHz) | 21281370 Hz (17.7344750 MHz x 6/5) |
| Dot clock (master/4) | 5.3693175 MHz | 5.3203425 MHz |
| CPU fast (master/6) | 3.579545 MHz | 3.546895 MHz |
| CPU slow (master/8) | 2.6846588 MHz | 2.6601713 MHz |
| CPU joypad-port (master/12) | 1.7897725 MHz | 1.7734475 MHz |
| Frame rate (exact) | 60.09880627 Hz = 21477270/(262x1364-2) | 50.00697891 Hz = 21281370/(312x1364) |

- 1 dot = 4 master cycles. 1 scanline = 1364 master cycles = 340 dots + 4 extra master cycles.
- The 4 extra cycles sit at H=323 and H=327 (long dots, 6 master cycles each per anomie; fullsnes describes the same +4 as "four 5-cycle dots"). Ignorable for most emulation: model the line as flat 1364 cycles.
- CPU memory cycle costs: 6 (FastROM via $420D + bank >= $80, most MMIO $4200-$5FFF/$2100-$21FF, internal/IO cycles), 8 (SlowROM, WRAM), 12 (entire $4000-$41FF region in banks $00-$3F/$80-$BF — any access there, including open-bus reads and DMA, not just $4016/$4017).

## 2. Frame layout

| | NTSC | PAL |
|---|---|---|
| Lines per frame | 262 (V=$0-$105) | 312 (V=$0-$137) |
| Visible lines | V=1..224 (V=1..239 overscan) | same |
| VBlank start | V=225 ($E1), or V=240 ($F0) if overscan ($2133 bit2) | same |
| VBlank end | after V=261; V=0 is next frame | after V=311 |
| Interlace | field 0 has 263 lines | field 0 has 313 lines |

- V=0: pre-render line (rendering pipeline runs, nothing displayed; OBJ prefetch for line 1).
- V=1..224/239: drawing period.
- Short line: NTSC, interlace OFF, field=1, V=240 -> 1360 cycles (340 dots x4).
- Long line: PAL, interlace ON, field=1, V=311 -> 1368 cycles (341 dots).
- Both occur only in vblank; safe to ignore (treat every line as 1364) at the cost of a tiny drift vs hardware.

## 3. Scanline structure (H = dot 0..339)

| Event | Position |
|---|---|
| HBlank flag ($4212.6) cleared | H=1 |
| Picture left edge | master clock 88 of line (dot 22) |
| Visible pixel output | dots 22..277 (approx), lines 1..224/239 |
| Picture right edge | master clock 1112 (dot 278) |
| HBlank flag ($4212.6) set | H=274 |
| HDMA per-line transfer point | H=278 |
| WRAM refresh | begins ~H=133.5 (~536 cycles into line), lasts 40 cycles |
| Last dot | H=339 (H=340 exists only on the PAL long line) |

## 4. Key H/V events (fullsnes detailed list)

| H | V | Event |
|---|---|---|
| 0 | 0 | Clear vblank flag, auto-acknowledge/reset NMI flag |
| 0 | 225/240 | Set vblank flag ($4212.7) |
| 0.5 | 225/240 | Set NMI flag ($4210.7) |
| 1 | every | Clear hblank flag |
| 1 | 0 | Toggle interlace FIELD flag ($213F.7) |
| 2.5 | V=VTIME | V-IRQ (mode 2, or HV-IRQ with HTIME=0) |
| HTIME+3.5 | every / V=VTIME | H-IRQ / HV-IRQ (HTIME=1..339) |
| 6 | 0 | HDMA init: reload HDMA registers for all enabled channels |
| 10 | 225/240 | Reload OAMADD (internal OAM addr := $2102/3, if not force-blank) |
| 32.5..95.5 | 225/240 | Auto-joypad read begins (duration 4224 master cycles) |
| 133.5 | every | WRAM refresh begins (40 cycles) |
| 274 | every | Set hblank flag |
| 278 | 0..224/239 | Perform HDMA transfers |

## 5. NMI ($4200 / $4210)

$4200 NMITIMEN (W): bit7 = NMI enable (0 at reset), bits5-4 = H/V IRQ mode, bit0 = auto-joypad enable.

$4210 RDNMI (R): bit7 = vblank NMI flag, bits3-0 = CPU version (1 or 2). Bits6-4 open bus.

- $4210.7 is set at H=0.5 of the first vblank line **even if NMI is disabled**; it is cleared automatically at end of vblank (V=0 H=0) and cleared by reading $4210 (read-ack).
- CPU /NMI is **edge-triggered**: an internal latch is set when ($4200.7 AND $4210.7) transitions 0->1, and cleared when the NMI is actually taken. Consequences:
  - Enabling $4200.7 mid-vblank while $4210.7=1 produces the 0->1 edge and triggers an NMI immediately.
  - Reading $4210 clears bit7 but does NOT cancel an already-latched NMI.
  - Disable-then-re-enable during the same vblank can re-trigger an old NMI; acknowledge by reading $4210 to avoid it.
- NMI handler entry: at the end of the instruction during which the edge occurred (check happens just before the instruction's final cycle; jump begins ~6-12 master cycles after the edge).
- If the edge occurs during DMA (CPU halted), NMI executes after the DMA completes, with a 24-30 master-cycle delay (possibly outside vblank, with $4210.7 already 0).

## 6. H/V IRQ ($4207-$420A, $4211)

$4207/$4208 HTIME (W, 9 bit, 0..339, 0=leftmost). $4209/$420A VTIME (W, 9 bit, 0..261 NTSC / 0..311 PAL, 0=top).

$4200 bits 5-4 (y,x):

| yx | Mode | Trigger point |
|---|---|---|
| 00 | disabled (also acknowledges pending IRQ) | — |
| 01 | H-IRQ: **every scanline** | H=HTIME+~3.5 |
| 10 | V-IRQ: **once per frame** | V=VTIME, H=~2.5 |
| 11 | HV-IRQ: **once per frame** | V=VTIME, H=HTIME+~3.5 (HTIME=0 behaves as V-IRQ) |

- Exact formula (anomie): if H=0, $4211.7 sets 1374 master cycles after dot 0.0 of the *previous* line; otherwise 14 + H*4 master cycles after dot 0.0 of the current line.
- $4211 TIMEUP (R): bit7 = IRQ flag, bits6-0 open bus. Read-clear (read-ack), EXCEPT a read landing exactly in the 4-8 master-cycle window when the compare condition is true returns bit7=1 without clearing it.
- The IRQ line is **level-held**: it stays asserted until acknowledged by reading $4211 or by writing $4200 with bits5-4=0. An unacknowledged IRQ re-enters the handler immediately after RTI.
- Enabling IRQs exactly on the trigger cycle still fires the IRQ.
- No IRQ triggers for dot 153 on the short scanline (non-interlace), nor for dot 153 on the last line of any frame (anomie; minor quirk, ignorable).

## 7. $4212 HVBJOY (R)

| Bit | Meaning | Timing |
|---|---|---|
| 7 | VBlank period flag | set H=0 at V=225/240; cleared H=0 V=0 |
| 6 | HBlank period flag | set H=274, cleared H=1, toggles on ALL lines (also during vblank/forced blank) |
| 5-1 | open bus | — |
| 0 | Auto-joypad read busy | set for 4224 master cycles (~3.1 scanlines) from read start |

## 8. Auto-joypad read

- Enabled by $4200 bit0; runs once per frame during vblank. Requires $4016 OUT0 (strobe) = 0.
- Begins between H=32.5 and H=95.5 of the first vblank line (V=225/240); exactly H=74.5 on the first frame, thereafter at a multiple of 256 master cycles after the previous read's start that falls in that window. Ends 4224 master cycles later; $4212.0 set for the duration.
- Results: $4218/9 JOY1, $421A/B JOY2, $421C/D JOY3, $421E/F JOY4. Bit layout (high to low): B Y Select Start Up Down Left Right A X L R 0 0 0 0 (bit15=B ... bit4=R, bits3-0=0; 1=pressed).
- Reading $4218-$421F (or touching $4016/$4017) while busy returns corrupted values — poll $4212.0 first.

## 9. WRAM refresh

- CPU stalls 40 master cycles once per scanline (every line, every frame), leaving 1324 usable CPU cycles per line.
- Position: begins ~536 master cycles into the line (~dot 133.5-134). Exact hardware rule: 538 cycles into the first line after reset, thereafter at the multiple of 8 cycles after the previous refresh closest to 536. Fixed dot ~133.5 is an adequate model.
- Refresh happens even during DMA (the DMA is paused for those 40 cycles).

## 10. General-purpose DMA timing ($420B)

Costs (all in master cycles):

| Item | Cost |
|---|---|
| Per byte transferred | 8 (regardless of memory region speed) |
| Per channel overhead | 8 |
| Whole-DMA overhead | 8 |
| Alignment before | 2-8 to reach a multiple of 8 master cycles since reset (DMA clock) |
| Alignment after | 2-8 to reach a whole CPU-cycle boundary since the pause |

- Sequence: after the $420B write, the CPU executes one more CPU cycle (e.g. next opcode fetch), then pauses; align to the 8-cycle DMA clock; 8 (DMA init) + per channel (8 + 8xbytes); realign to CPU clock; resume. Net overhead 12-24 cycles beyond 8/byte + 8/channel.
- Multiple channels run in order 0 first .. 7 last. $420B bits self-clear at completion. Byte counter $43x5/6: 1..$FFFF, $0000 = 65536 bytes; decremented to 0.
- HDMA has priority: an HDMA transfer point occurring mid-GDMA pauses the GDMA, runs the HDMA, then resumes.
- Same channel enabled in both $420B and $420C: unsupported — fullsnes: "Do not use channels for GP-DMA which are activated as H-DMA in HDMAEN". (Commonly reported behavior, not verified here: the HDMA event terminates that channel's in-progress GDMA.) Use distinct channels.
- NMI/IRQ edges during DMA are latched and taken 24-30 cycles after the DMA ends.

## 11. HDMA timing ($420C)

Init (once per frame, V=0 H=~6), for every channel enabled in $420C:

| Item | Cost (master cycles) |
|---|---|
| Overhead (if any channel enabled) | ~18 |
| Per direct-mode channel | 8 |
| Per indirect-mode channel | 24 |

Init actions per channel: table pointer $43x8/9 (A2A) := table start $43x2/3 (A1T, bank $43x4); read first line-count byte into $43xA (A2A++); if indirect, read 16-bit data pointer into $43x5/6 (A2A += 2; bank = $43x7, program-set); set internal do_transfer = true.

Per-line transfer (H=278 of lines V=0..224/239, i.e. **during hblank, before the line it affects**; the entry executed at end of line V takes effect on line V+1; the first table entry is executed at the end of the invisible line 0 and thus affects visible line 1):

| Item | Cost (master cycles) |
|---|---|
| Overhead if >=1 channel still active this frame | ~18 |
| Per active channel (even when not transferring) | 8 |
| Indirect address reload (when new table entry read) | 16 |
| Per byte transferred | 8 |

Per-line algorithm for each active (non-terminated) channel:
1. If do_transfer: transfer one unit (1/2/4 bytes per $43x0 mode; direct: from table at A2A, A2A += unit size; indirect: from $43x5/6/7, which increments).
2. Decrement line counter ($43xA bits 6-0).
3. do_transfer := repeat flag ($43xA bit7).
4. If line counter == 0: read next line-count byte from table into $43xA (A2A++); if indirect, load new 16-bit data pointer into $43x5/6 (A2A += 2, 16 cycles); if the byte read was $00, terminate this channel for the rest of the frame; do_transfer := true.

Line-count byte semantics: $00 = terminate channel; $01-$80 = transfer 1 unit now, then pause N-1 lines; $81-$FF = repeat mode, transfer 1 unit on each of N-$80 lines.

- Transfer step is always incrementing for HDMA (table and indirect data); $43x0 bits 4-3 are ignored.
- All HDMA channels are deactivated at the start of vblank; re-init occurs at V=0 H=6 of the next frame.
- Writing $420C mid-frame activates/deactivates channels immediately; a channel started mid-frame is NOT auto-initialized (software must set A2A/$43xA manually). Quirk (fullsnes): if $420C was already nonzero, a newly started channel begins with do_transfer=1.
- HDMA runs even during forced blank.
- Like GDMA, each HDMA pause aligns to the 8-cycle DMA clock (same alignment rules).

## 12. Multiply / divide unit ($4202-$4206, $4214-$4217)

| Op | Write | Result | Latency |
|---|---|---|---|
| Unsigned 8x8 multiply | $4202 = A, $4203 = B (write to $4203 starts it) | $4216/7 = A*B (16 bit) | 8 CPU cycles |
| Unsigned 16/8 divide | $4204/5 = dividend, $4206 = divisor (starts it) | $4214/5 = quotient, $4216/7 = remainder | 16 CPU cycles |

- Latency is counted in **CPU clock cycles** (whatever speed the CPU is running at), not master cycles — the same number of wait opcodes is needed at 3.58 or 2.68 MHz.
- A `LDA $4216`-style read itself spends 3 cycles fetching the opcode, so only 5 (mul) / 13 (div) cycles of padding are needed between the trigger write and the read opcode.
- Reading early returns intermediate garbage (hardware computes iteratively, 1 bit per cycle). Quirks: if the upper N bits of $4202 are zero the result is valid ~N cycles earlier; a WRAM-refresh stall during the computation also makes it ready in fewer executed opcodes.
- Writing $4203 also destroys $4214/5 (sets RDDIV = WRMPYB, high byte $00). Division by zero: quotient = $FFFF, remainder = dividend.
- Only writes to $4203/$4206 start an operation ($4202/$4204/$4205 may be left preloaded).
- (PPU mode-7 multiplier $211B/$211C -> $2134-6 is separate: signed 16x8, result available immediately, but invalid during mode-7 rendering outside blank.)

## 13. Handy per-frame totals

| | NTSC | PAL |
|---|---|---|
| Master cycles / frame (non-interlace) | 357368 (262x1364) | 425568 (312x1364) |
| VBlank lines (224-line mode) | 37 | 87 |
| VBlank master cycles (usable, 1324/line) | 48988 | 115188 |
| Max GDMA bytes per vblank (8/byte) | ~6123 | ~14398 |
