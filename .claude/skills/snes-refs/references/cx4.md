# SNES CX4 Reference (HLE)

The **CX4** (Capcom Custom Chip 4) is a **Hitachi HG51B169** 32-bit RISC DSP running at
~20 MHz, used by **Mega Man X2** and **Mega Man X3** (both LoROM) for 3-D wireframe/vector
graphics, sprite scale/rotate, OAM building, and trigonometric math. Its internal 24-bit
microprogram lives inside the cartridge but is Capcom-copyrighted, so every mainstream
emulator that shipped CX4 support before ~2015 ran it **HLE**: it reimplements the
reverse-engineered *command set* (each command byte maps to a hand-written C routine that
reproduces the observed input→output transform of the on-chip program), not the DSP core.

Sources transcribed here (no math invented, no register bit guessed):
- **snes9x `c4emu.cpp` / `c4.cpp` / `c4.h`** (Overload's reverse-engineered command math,
  as cleaned up in snes9x-git) — the command dispatch, the two 512-entry Q15 sin/cos tables,
  and every operation body below are copied verbatim from this source.
- **superfamicom.org wiki `capcom-cx4-hitachi-hg51b169`** and **nesdev forum thread 14647**
  ("Some tidbits about the Cx4") — the LLE register map ($7F40–$7F5E), internal data-ROM
  table layout, and hardware timing (used only for the register/memory-map documentation;
  HLE ignores the internal ROM/RAM banking).
- **problemkaputt.de/fullsnes.htm** — expansion overview line "`7F40h-7FAFh CX4 I/O Ports
  (with 3K SRAM at 6000h..6BFFh)`", and the cartridge-header chipset byte.

Items that could not be cross-verified against a second source, or where the HLE is a
documented approximation of the real hardware, are flagged `⚠`.

**LLE fallback:** if HLE proves insufficient, the alternative is to run the real HG51B169
core with the 1024×24-bit microprogram + 1024×24-bit data ROM extracted from the game ROM
(this is what bsnes/higan and modern snes9x do). The LLE register map in §2.2 is the
interface that path would need. HLE is attempted first per the task; it is sufficient for
X2/X3's graphics.

---

## 1. Cartridge detection & ROM/RAM mapping

CX4 is **LoROM / Mode 20**. Detection is by the internal header (LoROM base `$7FC0`;
byte `$16` = absolute `$7FD6`):

| Header byte `$16` | Chip | Notes |
|---|---|---|
| **`$F3`** | **CX4** | Mega Man X2, Mega Man X3. `$Fx` high nibble = "custom" coprocessor family. |
| `$F5` | SA-1 (ROM+RAM+batt variant) / other custom | **NOT CX4** |
| `$F6` | other custom (e.g. ST-01x DSP boards) | **NOT CX4** |
| `$F9` | SPC7110 + RTC / other custom | **NOT CX4** |

Detection must match **`$16 == $F3`** exactly (plus map-mode `$15 == $20`, LoROM). Do not
infer CX4 from the `$Fx` nibble alone — the other `$F5/$F6/$F9` boards are different chips.

Address decode (banks `$00–$3F` and mirror `$80–$BF`):

| Range | Contents |
|---|---|
| `$8000–$FFFF` | game ROM (LoROM: `ROM + ((bank&0x7F)<<15) + (addr&0x7FFF)`) |
| `$6000–$7FFF` | **CX4 window** (see §2) — 8 KB region `C4RAM[0x0000..0x1FFF]`, indexed `addr − $6000` |
| `$70–$77:0000–7FFF` | cartridge SRAM (optional, 1×256 Kbit; X2/X3 have battery save) |

The CX4 window is a flat 8 KB RAM (`C4RAM`, `0x2000` bytes) that the SNES reads/writes
directly; writes to a few specific addresses **also** trigger side effects (DMA load /
command execution). ROM is fetched by the CX4 through the LoROM mapping
`ROM + ((A & $FF0000) >> 1) + (A & $7FFF)` (`C4GetMemPointer`).

---

## 2. CX4 memory / register window (`$6000–$7FFF`)

`C4RAM` offset = `address − $6000`. So `$7F80` ↔ `C4RAM[0x1F80]`, `$6000` ↔ `C4RAM[0x0000]`.

| SNES address | C4RAM off | Role |
|---|---|---|
| `$6000–$6BFF` | `0x0000–0x0BFF` | **3 KB (`0xC00`) data RAM** — general work area, sprite lists, decoded tiles, wireframe I/O, per-scanline output. Fullsnes calls it "3K SRAM". |
| `$6C00–$7F3F` | `0x0C00–0x1F3F` | rest of the 8 KB window (scratch; wireframe output planes are written into `$6300+`, trapezoid tables into `$6800/$6900`, wave source `$6A00/$6B00`, line tables `$6B00`). |
| `$7F40–$7FAF` | `0x1F40–0x1FAF` | **I/O port block** (registers + command parameter/result words). |

### 2.1 I/O port block used by the HLE

The game pokes parameters into the `$7F80+` words, sets a sub-mode in `$7F4D`, then writes a
command byte to `$7F4F` (or triggers a ROM→RAM load via `$7F47`); results are read back from
the same `$7F80+` words. All multi-byte values are **little-endian**; 24-bit values ("3WORD")
occupy 3 consecutive bytes.

| Address | C4RAM off | Meaning |
|---|---|---|
| `$7F40–$7F42` | `0x1F40` | **DMA/transfer source** 24-bit ROM address (LE) |
| `$7F43–$7F44` | `0x1F43` | transfer **length** (16-bit, bytes) |
| `$7F45–$7F46` | `0x1F45` | transfer **dest** 16-bit (masked `& $1FFF` into C4RAM) |
| **`$7F47`** | `0x1F47` | **write → trigger ROM→RAM load** (`memmove`; value written is normally `$00`) |
| `$7F4D` | `0x1F4D` | **sub-command / mode selector** (read by command `$00` and by the "test" commands) |
| **`$7F4F`** | `0x1F4F` | **command register — writing a byte triggers the operation** (§3–§4) |
| `$7F5E` | `0x1F5E` | **status register** (read). HLE returns **`$00`** = idle/ready. Real HW: bit 6 = "CX4 running/busy" (game may poll it; HLE completes instantly so it is always clear). |
| `$7F80–$7FAF` | `0x1F80–0x1FAF` | command parameter / result words (per-command layout in §4) |

> Any read of `$7F5E` returns `$00` in the HLE. Every other address in the window reads back
> the raw `C4RAM` byte last written (the ports are plain RAM plus the trigger side effects).

### 2.2 Real-hardware (LLE) register map `$7F40–$7F5E` — reference only

Not used by the HLE, but documented for the LLE fallback (from superfamicom.org / nesdev):

| Addr | LLE function |
|---|---|
| `$7F40–$7F47` | GPDMA: `40/41/42`=src, `43/44`=len, `45/46`=dst, `47`=dst bank / **trigger** (bus↔internal) |
| `$7F48` | program page cache trigger (bit0 = page 0/1) |
| `$7F49–$7F4B` | ROM offset for cached program fetch |
| `$7F4C` | cache page locks (bit0=page0, bit1=page1) |
| `$7F4D–$7F4E` | page select |
| `$7F4F` | instruction pointer / program start (write = run) |
| `$7F50` | wait-states (bits6-4=WS1 ROM, bits2-0=WS2 RAM) |
| `$7F51` | IRQ enable/acknowledge |
| `$7F52` | ROM config select |
| `$7F53` | status (CPU-access / running / IRQ-pending / suspended) |
| `$7F55–$7F5C` | suspend controls (cycle counts) |
| `$7F5D` | clear suspend flag |
| `$7F5E` | **busy flag: bit 6 set while running**, cleared on completion; clear-IRQ-pending |

LLE internals: program ROM 256×16-bit pages, program RAM 2×256×16, **data ROM 1024×24-bit**
(`$000–$0FF` inverse, `$100–$1FF` sqrt, `$200–$27F` sine Q1, `$280–$2FF` arcsine,
`$300–$37F` tangent, `$380–$3FF` cosine), data RAM 4×384×16. HLE replaces all of this with
the C routines in §4 and the two tables in §3.

---

## 3. Fixed-point conventions & tables

| Name | Format | Meaning |
|---|---|---|
| Angle (table) | 9-bit, `index & $1FF` | **512 = full circle (2π)**. `128`=90°, `256`=180°, `384`=270°. Used by all `C4SinTable`/`C4CosTable` lookups (scale/rotate, cmd `$10/$13/$22`). |
| Angle (byte, wireframe) | signed 8-bit | **128 = full circle (2π)** in the wireframe rotate: `θ = −byte · 2π / 128`. `$40`(64)=180°, `$20`=90°. Different scale from the table angle. |
| Q15 | signed 16-bit, `>>15` after product | sin/cos table entries: `$7FFF`≈+1.0, `$8000`≈−1.0, `$4000`=+0.5. |
| Q12 (12.4 fixed) | signed, low 12 bits fractional | scale/rotate matrix coefficients (A,B,C,D) and accumulators (`X>>12` = integer pixel). |
| 24-bit signed | 3 LE bytes ("3WORD") | multiply/square operands & results; sign bit = bit 23. |

`SAR(x, n)` = arithmetic (sign-preserving) shift right by `n`.
`READ_WORD`/`WRITE_WORD` = 16-bit LE; `READ_3WORD`/`WRITE_3WORD` = 24-bit LE.

### `C4SinTable[512]` / `C4CosTable[512]` (Q15, 9-bit angle)

512 signed-16 entries each, one full circle. `C4CosTable[i] == C4SinTable[(i+128) & 0x1FF]`.
First octant of sine (index 0→63), transcribed verbatim from `c4emu.cpp`:
```
0,402,804,1206,1607,2009,2410,2811, 3211,3611,4011,4409,4808,5205,5602,5997,
6392,6786,7179,7571,7961,8351,8739,9126, 9512,9896,10278,10659,11039,11416,11793,12167,
12539,12910,13278,13645,14010,14372,14732,15090, 15446,15800,16151,16499,16846,17189,17530,17869,
18204,18537,18868,19195,19519,19841,20159,20475, 20787,21097,21403,21706,22005,22301,22594,22884
```
(peak `32767` at index 128; the full 512-entry tables must be copied verbatim from
`c4emu.cpp` — sine starts at 0, cosine is the same table rotated by +128, negated in the
lower half exactly as listed there). These are the **only** trig tables; the HLE does the
wireframe rotate with C-library `sin/cos` on doubles instead (§4.1).

---

## 4. Command set

Two trigger addresses:

- **Write `$7F47`** → `memmove(C4RAM[dst & $1FFF], ROM[src24], len)` (load model/line/tile data
  from ROM into the data RAM). No command dispatch.
- **Write byte `B` to `$7F4F`** → dispatch on `B` (table below). **Special case:** if the
  sub-mode `C4RAM[0x1F4D] == $0E` **and** `B < $40` **and** `(B & 3) == 0`, the write is a
  *test/parameter poke*: `C4RAM[0x1F80] = B >> 2` (no operation). Otherwise `B` selects:

| `B` (`$7F4F`) | `$7F4D` | Name | Summary |
|---|---|---|---|
| `$00` | selector | **Sprite functions** — sub-dispatch on `$7F4D` (see §4.5) | OAM build / scale-rotate / lines / wireframe / disintegrate / wave |
| `$01` | `08` | **Draw wireframe** | clear planes `$6300+`, then `C4DrawWireFrame` |
| `$05` | `02` | **Propulsion / reciprocal-scale** | `$7F80 = ((0x10000 / $7F83) · $7F81) >> 8` (if `$7F83≠0`) |
| `$0D` | `02` | **Set vector length** (normalize to length) | scale (X,Y) to magnitude `$7F86` (`C4Op0D`) |
| `$10` | `02` | **Polar→rect (16-bit)** | X=`r·cos>>15` , Y=`r·sin>>15` minus 1/64 (see body) |
| `$13` | `02` | **Polar→rect (24-bit)** | X=`r·cos`, Y=`r·sin`, `>>8`, 24-bit results |
| `$15` | `02` | **Pythagorean / hypotenuse** | `$7F80 = √(X² + Y²)` |
| `$1F` | `02` | **Arctangent (vector→angle)** | `$7F86 = atan2(Y,X)` in 9-bit angle (`C4Op1F`) |
| `$22` | `02` | **Trapezoid** (per-scanline left/right spans) | fill `$6800`/`$6900` from two edge angles |
| `$25` | `02` | **Multiply** (24×24→48, low 24 kept) | `$7F80 = ($7F80 · $7F83)` truncated to 24-bit |
| `$2D` | `02` | **Transform coordinates** | one point through `C4TransfWireFrame2` |
| `$40` | `0E` | **Sum** (test) | `$7F80 = Σ C4RAM[0..0x7FF]` (byte sum, 16-bit) |
| `$54` | `0E` | **Square** (test) | 24-bit signed square → 48-bit at `$7F83`/`$7F86` |
| `$5C` | `0E` | **Immediate register** (test) | copy 48-byte `C4TestPattern` into `$6000` |
| `$89` | `0E` | **Immediate ROM** (test) | `$7F80/81/82 = $36,$43,$05` (fixed ID) |

> Command bytes not listed do nothing (snes9x prints "Unknown C4 command"). The list above is
> the complete set X2/X3 exercise. The `$7F4D` column is the value the game is expected to
> have set (snes9x only *warns* on mismatch in DEBUGGER builds; the operation runs regardless).

### 4.1 Wireframe transform primitives (in `c4.cpp`)

Shared 3-D rotate/project, `C4_PI = 3.14159265`. Angles are **byte-scale** (128 = 2π).
Static state registers: `C4WFXVal,C4WFYVal,C4WFZVal` (input point), `C4WFX2Val,C4WFY2Val,
C4WFDist` (X/Y/Z rotation angles, bytes), `C4WFScale`.

**`C4TransfWireFrame`** (used by Transform Lines, subtracts a `$95` Z bias, perspective divide):
```c
c4x=X; c4y=Y; c4z=Z-0x95;
t=-X2·2π/128;  c4y2=c4y·cos t − c4z·sin t;  c4z2=c4y·sin t + c4z·cos t;   // rot X
t=-Y2·2π/128;  c4x2=c4x·cos t + c4z2·sin t; c4z =−c4x·sin t + c4z2·cos t; // rot Y
t=-Dist·2π/128;c4x =c4x2·cos t − c4y2·sin t;c4y =c4x2·sin t + c4y2·cos t; // rot Z
X = c4x·Scale / (0x90·(c4z+0x95)) · 0x95;   // perspective project
Y = c4y·Scale / (0x90·(c4z+0x95)) · 0x95;
```
**`C4TransfWireFrame2`** (used by Draw wireframe & cmd `$2D`): identical rotations but **no Z
bias** and **orthographic** scale `X = c4x·Scale/0x100`, `Y = c4y·Scale/0x100`.

**`C4CalcWireFrame`** — Bresenham setup between (X,Y) and (X2,Y2): sets `C4WFDist` = span
length and (X,Y) = per-step increment (256-scaled), major axis pinned to ±256.

### 4.2 `C4Op0D` (Set vector length), `C4Op15` (Pythagorean), `C4Op1F` (atan)
```c
// Op0D: scale (X,Y) so magnitude = DistVal, with the chip's factors 0.98/0.99
t = DistVal / sqrt(Y² + X²);  Y = Y·t·0.99;  X = X·t·0.98;
// Op15: Dist = (int16) sqrt(X² + Y²);
// Op1F: atan2 into 9-bit angle (512 = full circle):
if (X==0) Angle = (Y>0) ? 0x80 : 0x180;
else { Angle = atan(Y/X)/(2π)·512; if (X<0) Angle += 0x100; Angle &= 0x1FF; }
```

### 4.3 Cmd `$10` / `$13` Polar→Rectangular
```c
// $10 (16-bit), r = $7F83 sign-extended from bit15:
X = SAR(r · Cos[θ] · 2, 16);  write 24-bit at $7F86
Y = SAR(r · Sin[θ] · 2, 16);  write (Y − SAR(Y,6)) 24-bit at $7F89   // ×(1−1/64) skew
// $13 (24-bit), r = $7F83:
X = SAR(r · Cos[θ] · 2, 8) at $7F86 ;  Y = SAR(r · Sin[θ] · 2, 8) at $7F89
θ = $7F80 & 0x1FF
```

### 4.4 Cmd `$22` Trapezoid, `$25` Multiply, `$54` Square
```c
// $22: two edge slopes from angles $7F8C,$7F8F; for scanline y=0..224 fill left/right x
tan = (Cos[a]!=0) ? ((Sin[a]<<16)/Cos[a]) : 0x80000000;
left  = SAR(tan1·y,16) − $7F80 + $7F86;
right = SAR(tan2·y,16) − $7F80 + $7F86 + $7F93;   // clamp to [0,255]; store $6800[y],$6900[y]
// $25: foo=READ_3WORD($7F80); bar=READ_3WORD($7F83); WRITE_3WORD($7F80, foo·bar);  // low 24 bits
// $54: a=sign-extend24(READ_3WORD($7F80)); a*=a; WRITE_3WORD($7F83,a); WRITE_3WORD($7F86,a>>24);
```

### 4.5 Cmd `$00` Sprite sub-functions (dispatch on `$7F4D`)

| `$7F4D` | Routine | Effect |
|---|---|---|
| `$00` | `C4ConvOAM` | Build OAM: transform a sprite list at `$6220+` into SNES OAM entries at `$6000+`, applying global (X,Y) at `$6621/$6623`, H/V flip, on-screen cull (−16..272 / −16..224), and the OAM high-table (size/x-bit) at `$6200+`. |
| `$03` | `C4DoScaleRotate(0)` | Affine scale+rotate a tile bitmap (source `$6600+`) into 4bpp planes; matrix A/B/C/D from XScale `$7F8F`, YScale `$7F92`, angle `$7F80` (table lookup, or exact 0/90/180/270 special-cases). Center `$7F83/$7F86`. |
| `$05` | `C4TransformLines` | Rotate/project a vertex list (`C4TransfWireFrame`) and build the line table at `$6600+`/`$6B00+` via `C4CalcWireFrame`. |
| `$07` | `C4DoScaleRotate(64)` | as `$03` with 64-byte row padding (larger tile). |
| `$08` | `C4DrawWireFrame` | Draw all model edges: for each of `$6295` lines read endpoint indices, fetch 3-D points from ROM (`$7F82`:hi bank), `C4TransfWireFrame2` + `C4CalcWireFrame` + `C4DrawLine` into planes `$6300+`. `$FFFF` index = reuse previous point. |
| `$0B` | `C4SprDisintegrate` | Scale a sprite toward/away with per-axis scale `$7F86/$7F8F` (disintegration effect). |
| `$0C` | `C4BitPlaneWave` | Sinusoidal bitplane "wave" distortion using height table at `$6A00/$6A10`, source `$6B00`. |

**`C4DrawLine`** transforms both endpoints via `C4TransfWireFrame2`, offsets by `+48`,
`C4CalcWireFrame` for the step, then plots `C4WFDist` pixels into the two 1bpp planes at
`$6300`/`$6301` (color bits 0/1), clipping to the 8×8-tile-addressed 96×? framebuffer.

---

## 5. State that persists across commands

The CX4 HLE keeps **all** its state in `C4RAM` (the 8 KB window) plus the file-scope
`C4WF*`/`C41F*` scratch globals in `c4.cpp` (transient within one command, not read across
commands — they are always re-loaded from `C4RAM` before use). For save states, only the
8 KB `C4RAM` array needs to be serialized (`#[derive(Serialize,Deserialize)]` or
`#[serde(skip)]`+`Default` for pointer-like fields). No hidden latches survive between
commands beyond `C4RAM` itself; the busy/status is instantaneous.

---

## 6. Game relevance (Mega Man X3 specifically)

- **Wireframe path — `$01` / `$00:$08` Draw wireframe, `$00:$05` Transform Lines, `$2D`
  Transform coords, `$0D` vector length, `$10/$13` polar→rect, `$1F` atan, `$15` hypot:**
  the intro/title vector globe, the **Sigma virus** wireframe, and boss/ride-armor 3-D
  effects. This is the hot, must-be-correct path for X2 *and* X3.
- **Sprite scale/rotate — `$00:$03` / `$00:$07` `C4DoScaleRotate`, `$00:$00` Build OAM:**
  rotating/scaling bosses and the heavy OAM management X3 uses throughout normal stages.
  Build OAM (`$00:$00`) runs almost every frame.
- **`$22` Trapezoid, `$00:$0B` Disintegrate, `$00:$0C` Wave:** special stage effects
  (enemy disintegration, screen-wave transitions) — used, but less often.
- **`$05` Propulsion, `$25` Multiply, `$54` Square:** general math helpers, called ad hoc.
- **Test/utility — `$40` Sum, `$5C` Immediate reg, `$89` Immediate ROM, and the `$7F4D==$0E`
  parameter-poke path:** boot/self-test and parameter staging; rarely on the critical path.

---

## 7. Verification status

| Item | Source | Confidence |
|---|---|---|
| Register window `$6000–$7FFF`, offset `addr−$6000`, 3 KB data RAM `$6000–$6BFF` | fullsnes + snes9x c4emu | high |
| Trigger addresses `$7F47` (DMA), `$7F4F` (command); status `$7F5E`→`$00` | snes9x `S9xSetC4`/`S9xGetC4` | high |
| Command dispatch table (`$00,$01,$05,$0D,$10,$13,$15,$1F,$22,$25,$2D,$40,$54,$5C,$89`) and the `$7F4D==$0E && B<$40 && (B&3)==0` poke special-case | snes9x `S9xSetC4` (verbatim) | high |
| `$00` sprite sub-dispatch on `$7F4D` (`00/03/05/07/08/0B/0C`) | snes9x `C4ProcessSprites` | high |
| Wireframe math (`C4TransfWireFrame`/`2`, `C4CalcWireFrame`, `C4DrawLine`, `C4DrawWireFrame`) | snes9x `c4.cpp`/`c4emu.cpp` (verbatim) | high |
| `C4Op0D`/`C4Op15`/`C4Op1F`, cmd `$10/$13/$22/$25/$54` math | snes9x (verbatim) | high |
| `C4SinTable`/`C4CosTable` (512×Q15) | snes9x c4emu (first octant transcribed; **copy full 512-entry tables from `c4emu.cpp`**) | high (partial transcription) |
| Header byte `$16 == $F3` = CX4; `$F5/$F6/$F9` = other custom | task brief + common header tables | high |
| Real-HW LLE register map `$7F48–$7F5D`, internal ROM/RAM banks | superfamicom wiki + nesdev 14647 | reference-only (not needed for HLE) |

**Flagged / approximations (`⚠`):**
- The HLE performs the wireframe rotate with **C-library `sin/cos` on `double`** (`c4.cpp`),
  not the on-chip Q15 tables; results are visually correct but not bit-exact to real
  hardware. `⚠` If bit-exactness vs. hardware is required, switch that path to LLE.
- The `Op0D` `×0.98`/`×0.99` and the `$10` `−1/64` skew are the reverse-engineered chip
  constants exactly as in snes9x; they reflect the on-chip program's own approximation.
- `C4ConvOAM`, `C4DoScaleRotate`, `C4TransformLines`, `C4BitPlaneWave`, `C4SprDisintegrate`
  carry several snes9x `XXX:` comments (exact masking/center/padding edge cases). They match
  observed game behavior but a few corner bits are unverified by a second source `⚠`.
- The full 512-entry sin/cos tables and the `C4TestPattern[48]` array are **not reproduced in
  full here** — copy them verbatim from `c4emu.cpp` at implementation time and unit-test
  `C4CosTable[i] == C4SinTable[(i+128)&0x1FF]` and peak `32767` at index 128.

### Suggested unit-test vectors (derivable from the formulas)
- Table symmetry: `C4SinTable[0]=0`, `C4SinTable[128]=32767`, `C4SinTable[256]=0`,
  `C4SinTable[384]=-32767`; `C4CosTable[i]==C4SinTable[(i+128)&0x1FF]`.
- `Op15` hypot: `X=3,Y=4 → Dist=5`; `X=0,Y=0 → 0`.
- `Op1F` atan: `X=0,Y>0 → $080`; `X=0,Y<0 → $180`; `X>0,Y=0 → $000`; `X<0,Y=0 → $100`.
- Cmd `$25` multiply: `$7F80=2, $7F83=3 → $7F80=6` (24-bit); wraps mod 2²⁴.
- Cmd `$54` square: `$7F80=$000003 → $7F83=9`; negative `$FFFFFF (−1) → 1`.
- Cmd `$05`: `$7F83=$0100, $7F81=$0200 → $7F80 = ((0x10000/0x100)·0x200)>>8 = $0200`.
</content>
</invoke>
