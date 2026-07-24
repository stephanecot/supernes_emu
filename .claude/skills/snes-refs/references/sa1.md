# SNES SA-1 (Super Accelerator 1) Reference

Sources: fullsnes (nocash SNES specs, cartridge coprocessor list),
Super Famicom Development Wiki `sa-1-registers`, SnesLab `SA-1`,
VitorVilela7 `SNES-SA-1-doc`, PeterLemon `SNES_SA-1.INC`, nesdev wiki ROM header,
and **bsnes `sfc/coprocessor/sa1/` source** (arithmetic, MMC bank bits, BWPA storage —
verified 2026-07 pass). Values transcribed, not guessed. Remaining unverified items are
flagged **[VERIFY]**; the arithmetic div-by-0, MCNT bits, MMC LoROM fixed blocks, and
BWPA formula [VERIFY]s were resolved against bsnes this pass.

The SA-1 is a cartridge coprocessor: a second **65C816** CPU (same ISA as the main
S-CPU core in `core/src/cpu/`) plus a custom "Super MMC" mapper, an arithmetic unit,
a variable-length bit reader, an H/V timer, a DMA controller, and 2 KB of on-chip RAM.

---

## 1. Clocks & stepping

| Clock            | Rate                     | Notes                                              |
|------------------|--------------------------|----------------------------------------------------|
| Master           | 21.477 MHz (NTSC)        | Shared system master clock                         |
| SA-1 CPU         | **10.74 MHz** (master/2) | Fixed; "always 10.74 MHz", 1 SA-1 cyc = 2 master   |
| S-CPU fast       | 3.58 MHz (6 master)      | = 3 SA-1 cycles                                    |
| S-CPU slow       | 2.68 MHz (8 master)      | = 4 SA-1 cycles                                    |

The SA-1 runs continuously and independently of the S-CPU. Emulate by **catch-up**:
when the main CPU advances N master cycles, step the SA-1 `N/2` cycles. The SA-1 does
not share the S-CPU's bus; each has its own view of ROM / BW-RAM / I-RAM via the MMC.

### SA-1 memory access wait states (its own bus)

| Target                | Bus         | Rate       | SA-1 cycles / access |
|-----------------------|-------------|------------|----------------------|
| ROM                   | 16-bit      | 5.37 MHz   | 2                    |
| I-RAM (on-chip)       | internal    | 10.74 MHz  | 1                    |
| BW-RAM                | 8-bit       | 5.37 MHz   | 2                    |

Bus conflicts: if both CPUs want the same resource, `DCNT`/`CCNT` priority bits and the
MMC arbitrate; the loser stalls. Commonly simplified to "no stall" in emulators.

---

## 2. Memory map

### S-CPU side

| Banks       | Offset        | Content                                               |
|-------------|---------------|-------------------------------------------------------|
| $00-$3F/$80-$BF | $0000-$07FF | mirror WRAM (as normal SNES)                        |
| $00-$3F/$80-$BF | $2200-$23FF | SA-1 I/O registers (see §3)                         |
| $00-$3F/$80-$BF | $3000-$37FF | SA-1 I-RAM (2 KB), shared                            |
| $00-$3F/$80-$BF | $6000-$7FFF | BW-RAM 8 KB window, block selected by BMAPS $2224    |
| $00-$3F/$80-$BF | $8000-$FFFF | ROM (LoROM-style, via MMC; see §4)                  |
| $40-$4F     | $0000-$FFFF   | BW-RAM, linear (up to **256 KB**)                     |
| $C0-$FF     | $0000-$FFFF   | ROM (HiROM-style 64 KB banks, via MMC CXB/DXB/EXB/FXB)|

### SA-1 side

Same layout, plus:

| Banks       | Offset        | Content                                               |
|-------------|---------------|-------------------------------------------------------|
| $00-$3F/$80-$BF | $0000-$07FF | SA-1 I-RAM (2 KB) — **SA-1 only** mirror of $3000-$37FF |
| $40-$4F     | $0000-$FFFF   | BW-RAM, linear                                        |
| $60-$6F     | $0000-$FFFF   | BW-RAM **bitmap virtual memory** (2bpp/4bpp expansion; see §7) |
| $6000-$7FFF | window        | BW-RAM 8 KB window, block via BMAP $2225              |

- **I-RAM**: 2048 bytes on-chip. Address space $3000-$37FF (both CPUs) and additionally
  $0000-$07FF on the SA-1 side. Write-protectable per 256-byte page via SIWP/CIWP.
- **BW-RAM**: up to 256 KB, battery-backed. Linear in banks $40-$4F; also an 8 KB window
  at $6000-$7FFF whose backing block is chosen independently by each CPU.

---

## 3. I/O register map $2200-$230E

Convention: `$2200-$22FF` are **write-only** (SA-1 config), `$2300-$23FF` are
**read-only** (status). Bit layouts are MSB-first (`bit7..bit0`); `-` = unused.

### 3.1 CPU control / interrupts / message ports (write, $2200-$220F)

| Addr  | Name | Bits       | Meaning                                                                 |
|-------|------|------------|-------------------------------------------------------------------------|
| $2200 | CCNT | `IRrNmmmm` | S-CPU→SA-1 control. I=SA-1 IRQ req, R=SA-1 ready/wait (1=wait/halt), r=SA-1 reset (1=hold reset), N=SA-1 NMI req, mmmm=4-bit message to SA-1 |
| $2201 | SIE  | `I-C-----` | S-CPU IRQ enable. I=enable IRQ-from-SA-1, C=enable char-conv DMA IRQ     |
| $2202 | SIC  | `I-C-----` | S-CPU IRQ clear (write 1 to acknowledge). I=clear SA-1 IRQ, C=clear DMA IRQ |
| $2203 | CRVL | `aaaaaaaa` | SA-1 reset vector, low byte                                             |
| $2204 | CRVH | `aaaaaaaa` | SA-1 reset vector, high byte (16-bit addr, executed in bank $00)         |
| $2205 | CNVL | `aaaaaaaa` | SA-1 NMI vector, low                                                     |
| $2206 | CNVH | `aaaaaaaa` | SA-1 NMI vector, high                                                    |
| $2207 | CIVL | `aaaaaaaa` | SA-1 IRQ vector, low                                                     |
| $2208 | CIVH | `aaaaaaaa` | SA-1 IRQ vector, high                                                    |
| $2209 | SCNT | `IS-Nmmmm` | SA-1→S-CPU control. I=S-CPU IRQ req, S=S-CPU IRQ vector select (0=ROM,1=SIV $220E), N=S-CPU NMI vector select (0=ROM,1=SNV $220C), mmmm=4-bit message to S-CPU |
| $220A | CIE  | `ITDN----` | SA-1 IRQ enable. I=IRQ-from-S-CPU, T=timer IRQ, D=IRQ after SA-1 DMA, N=NMI-from-S-CPU |
| $220B | CIC  | `ITDN----` | SA-1 IRQ clear (write 1). I/T/D/N clear the matching source              |
| $220C | SNVL | `aaaaaaaa` | S-CPU NMI vector override, low  (used when SCNT.N=1)                     |
| $220D | SNVH | `aaaaaaaa` | S-CPU NMI vector override, high                                          |
| $220E | SIVL | `aaaaaaaa` | S-CPU IRQ vector override, low  (used when SCNT.S=1)                     |
| $220F | SIVH | `aaaaaaaa` | S-CPU IRQ vector override, high                                          |

Vector overrides let the SA-1 intercept the S-CPU's NMI/IRQ so the two CPUs can
cooperate. When SCNT.N/S = 0 the normal cartridge ROM vectors at $00:FFEA/$FFEE are used.

### 3.2 H/V timer (write, $2210-$2215)

| Addr  | Name  | Bits       | Meaning                                                              |
|-------|-------|------------|---------------------------------------------------------------------|
| $2210 | TMC   | `T-----VH` | T=timer mode (0=H/V timer, 1=linear 18-bit), V=enable V-compare, H=enable H-compare |
| $2211 | CTR   | write      | Writing restarts the timer counters to 0                             |
| $2212 | HCNTL | `HHHHHHHH` | H-count compare, low                                                 |
| $2213 | HCNTH | `-------H` | H-count compare, high bit (H/V mode 0-340; linear 9 low bits 0-511) |
| $2214 | VCNTL | `VVVVVVVV` | V-count compare, low                                                 |
| $2215 | VCNTH | `-------V` | V-count compare, high (H/V 0-261 NTSC/0-311 PAL; linear 9 high bits) |

Timer IRQ (CIE.T) fires when the counters match. Linear mode is a single 18-bit
free-running counter; H/V mode mirrors the PPU dot/scanline counters.

### 3.3 Super MMC bank mapping (write, $2220-$2225) — see §4

| Addr  | Name  | Bits       | Meaning                                                             |
|-------|-------|------------|--------------------------------------------------------------------|
| $2220 | CXB   | `B----AAA` | ROM 1 MB block for S-CPU banks $C0-$CF (+ LoROM $00-$1F). AAA=block 0-7, B=LoROM projection (see §4) |
| $2221 | DXB   | `B----AAA` | ROM block for $D0-$DF (+ LoROM $20-$3F)                            |
| $2222 | EXB   | `B----AAA` | ROM block for $E0-$EF (+ LoROM $80-$9F)                            |
| $2223 | FXB   | `B----AAA` | ROM block for $F0-$FF (+ LoROM $A0-$BF)                            |
| $2224 | BMAPS | `---BBBBB` | S-CPU BW-RAM window block: BBBBB selects one of 32 8 KB blocks at $6000-$7FFF |
| $2225 | BMAP  | `SBBBBBBB` | SA-1 BW-RAM window: S=source (0=$40-$43 as 32 blocks, 1=$60-$6F bitmap as 128 blocks), BBBBBBB=block |

### 3.4 BW-RAM / I-RAM write protection (write, $2226-$222A)

| Addr  | Name  | Bits       | Meaning                                                             |
|-------|-------|------------|--------------------------------------------------------------------|
| $2226 | SBWE  | `P-------` | S-CPU BW-RAM write enable. P: 0=protect, 1=writes allowed          |
| $2227 | CBWE  | `P-------` | SA-1 BW-RAM write enable. P: 0=protect, 1=writes allowed           |
| $2228 | BWPA  | `----AAAA` | BW-RAM write-protected area = first **256·2^AAAA** bytes (= `0x100 << AAAA`) of BW-RAM. Verified points: AAAA=0 → 256 B ($40:0000-$40:00FF); AAAA=2 → 1024 B ($40:0000-$40:03FF). AAAA=$F → 256·2^15 = 8 MB, i.e. all BW-RAM protected (does **not** disable). bsnes stores only the raw 4-bit value (`mmio.bwp = data & 0x0f`). **NOTE: the Super Famicom Wiki prints "1024·2^(AAAA+1)" — that formula contradicts its own worked example (256 B at AAAA=0) and is wrong; 256·2^AAAA is correct.** |
| $2229 | SIWP  | `76543210` | S-CPU I-RAM write protect: each bit enables writes to one 256-byte page $30xx-$37xx |
| $222A | CIWP  | `76543210` | SA-1 I-RAM write protect: per-page enable ($30xx-$37xx / $00xx-$07xx) |

### 3.5 DMA controller (write, $2230-$224F) — see §6, §7

| Addr  | Name  | Bits       | Meaning                                                             |
|-------|-------|------------|--------------------------------------------------------------------|
| $2230 | DCNT  | `CPMT-DSS` | C=DMA enable, P=priority (0=SA-1 CPU,1=DMA), M=mode (0=normal,1=char-conv), T=char-conv type (0=type1 auto,1=type2), D=dest (0=I-RAM,1=BW-RAM), SS=source (00=ROM,01=BW-RAM,10=I-RAM) |
| $2231 | CDMA  | `E--SSSCC` | Char-conv params. E=end-of-conversion (set by S-CPU to stop), SSS=VRAM tiles-per-row = 2^SSS, CC=color depth (00=8bpp,01=4bpp,10=2bpp) |
| $2232 | SDAL  | `aaaaaaaa` | DMA source address, bits 0-7                                        |
| $2233 | SDAH  | `aaaaaaaa` | DMA source address, bits 8-15                                       |
| $2234 | SDAB  | `aaaaaaaa` | DMA source address, bits 16-23                                      |
| $2235 | DDAL  | `aaaaaaaa` | DMA destination address, bits 0-7                                   |
| $2236 | DDAH  | `aaaaaaaa` | DMA dest bits 8-15. **Write here triggers a normal DMA to I-RAM**    |
| $2237 | DDAB  | `aaaaaaaa` | DMA dest bits 16-23. **Write here triggers a normal DMA to BW-RAM**  |
| $2238 | DTCL  | `cccccccc` | DMA byte count, low                                                 |
| $2239 | DTCH  | `cccccccc` | DMA byte count, high (0-65535)                                      |
| $223F | BBF   | `C-------` | BW-RAM bitmap format: C=0 → 16-color (4bpp), C=1 → 4-color (2bpp)   |
| $2240-$2247 | BRF0-7  | `xxxxxxxx` | Bitmap register file, buffer 1 (char-conv type-1 staging)     |
| $2248-$224F | BRF8-15 | `xxxxxxxx` | Bitmap register file, buffer 2                                |

### 3.6 Arithmetic unit (write, $2250-$2254) — see §5

| Addr  | Name  | Bits       | Meaning                                                             |
|-------|-------|------------|--------------------------------------------------------------------|
| $2250 | MCNT  | `------AM` | Two independent bits: **M** = bit0 `md` (divide select, meaningful only when acm=0: 0=multiply, 1=divide); **A** = bit1 `acm` (0=multiply/divide, 1=cumulative multiply-accumulate). Writing $2250 with **acm=1 (bit1) resets the 40-bit result accumulator MR to 0** — this is the fresh-sum start, NOT bit0. Resulting op encodings: 00=multiply, 01=divide, 10/11=cumulative sum. |
| $2251 | MAL   | `nnnnnnnn` | Multiplicand / dividend, low  (signed 16-bit)                       |
| $2252 | MAH   | `nnnnnnnn` | Multiplicand / dividend, high. Writing $2252 does **not** start op   |
| $2253 | MBL   | `nnnnnnnn` | Multiplier / divisor, low (signed for multiply, unsigned for divide)|
| $2254 | MBH   | `nnnnnnnn` | Multiplier / divisor, high. **Write here starts the operation**      |

### 3.7 Variable-length bit processing (write, $2258-$225B) — see §5

| Addr  | Name  | Bits       | Meaning                                                             |
|-------|-------|------------|--------------------------------------------------------------------|
| $2258 | VBD   | `H---VVVV` | H=read mode (1=auto-increment after read, 0=fixed), VVVV=bit length (1-15; 0000 means 16) |
| $2259 | VDAL  | `aaaaaaaa` | ROM byte address of bit stream, bits 0-7                            |
| $225A | VDAH  | `aaaaaaaa` | bits 8-15                                                           |
| $225B | VDAB  | `aaaaaaaa` | bits 16-23. **Write here (re)initializes the bit reader**           |

### 3.8 Status registers (read, $2300-$230E)

| Addr  | Name  | Bits       | Meaning                                                             |
|-------|-------|------------|--------------------------------------------------------------------|
| $2300 | SFR   | `IVDNmmmm` | S-CPU flag read. I=SA-1 IRQ pending, V=S-CPU IRQ vector source (0=ROM,1=SIV), D=char-conv DMA IRQ pending, N=S-CPU NMI vector source (0=ROM,1=SNV), mmmm=message from SA-1 (mirror of SCNT.mmmm) |
| $2301 | CFR   | `ITDNmmmm` | SA-1 flag read. I=IRQ-from-S-CPU pending, T=timer IRQ pending, D=DMA IRQ pending, N=NMI-from-S-CPU pending, mmmm=message from S-CPU (mirror of CCNT.mmmm) |
| $2302 | HCRL  | `hhhhhhhh` | Latched H-count, low                                                |
| $2303 | HCRH  | `-------h` | Latched H-count, high                                               |
| $2304 | VCRL  | `vvvvvvvv` | Latched V-count, low                                                |
| $2305 | VCRH  | `-------v` | Latched V-count, high                                               |
| $2306 | MR1   | result     | Arithmetic result, bits 0-7                                         |
| $2307 | MR2   | result     | bits 8-15                                                           |
| $2308 | MR3   | result     | bits 16-23                                                          |
| $2309 | MR4   | result     | bits 24-31                                                          |
| $230A | MR5   | result     | bits 32-39 (only used by cumulative sum; 40-bit accumulator)        |
| $230B | OF    | `O-------` | Arithmetic overflow (set on divide-by-0 / cumulative-sum overflow)  |
| $230C | VDPL  | `dddddddd` | Variable-length data read port, low                                 |
| $230D | VDPH  | `dddddddd` | Variable-length data read port, high (16-bit window into bit stream)|
| $230E | VC    | version    | Version code. **On real carts this reads open bus** — do not rely on it |

> NOTE: the task brief listed MR as $2306-$230B, overflow $230C, VDP $230D/$230E. The
> verified hardware layout (Super Famicom Wiki, SnesLab, PeterLemon INC) is
> **MR $2306-$230A, OF $230B, VDP $230C-$230D, VC $230E** — used above.

---

## 4. Super MMC ROM banking

ROM (up to 8 MB) is divided into eight **1 MB blocks** (index 0-7). Four registers each
bind one block to a 1 MB region of the S-CPU/SA-1 address space:

| Register | HiROM region (64 KB banks) | Paired LoROM region (32 KB halves) |
|----------|----------------------------|------------------------------------|
| CXB $2220 | $C0-$CF                    | $00-$1F ($8000-$FFFF)              |
| DXB $2221 | $D0-$DF                    | $20-$3F ($8000-$FFFF)              |
| EXB $2222 | $E0-$EF                    | $80-$9F ($8000-$FFFF)              |
| FXB $2223 | $F0-$FF                    | $A0-$BF ($8000-$FFFF)              |

- `AAA` (bits 0-2) selects the 1 MB block (0-7) for the HiROM region.
- `B` (bit 7, bsnes `cbmode`/`dbmode`/`ebmode`/`fbmode`) controls the paired LoROM region:
  **B=1** → LoROM maps the same `AAA` block as the HiROM region ("projection"); **B=0** →
  LoROM maps a fixed default block — **0, 1, 2, 3** for CXB/DXB/EXB/FXB respectively,
  independent of `AAA`. (Confirmed: bsnes stores `cb = data & 0x07`, `cbmode = data & 0x80`;
  SnesLab: "When cleared, LoROM banks default to {$00,$01,$02,$03}." [VERIFY resolved.])
- HiROM banks map linearly: within a bound 1 MB block, bank $C0..$CF = block offset
  $00000..$FFFFF. LoROM maps each 32 KB half-bank to consecutive 32 KB ROM slices within
  the block.

---

## 5. Arithmetic & variable-length units

### Arithmetic unit ($2250-$2254 → $2306-$230B)

Set `MCNT` ($2250), load `MA`/`MB`; the operation starts when **$2254 (MBH)** is written.
Exact behavior transcribed from bsnes (`sfc/coprocessor/sa1/io.cpp`, `$2254` handler):

| acm/md | Operation                    | Inputs                     | Result / side effects                                                                                     |
|--------|------------------------------|----------------------------|----------------------------------------------------------------------------------------------------------|
| 0/0    | signed 16×16 multiply        | MA (s16) × MB (s16)        | `MR = (u32)((s16)MA * (s16)MB)` → 32-bit product in MR1-MR4 ($2306-$2309). **MB cleared to 0** after.     |
| 0/1    | signed÷unsigned divide       | MA (s16) ÷ MB (u16)        | quotient (16-bit) → MR1-MR2 ($2306-$2307), remainder (u16) → MR3-MR4 ($2308-$2309). **MA and MB cleared to 0** after. |
| 1/-    | cumulative multiply-add (MAC)| Σ (s16 MA × s16 MB)        | `MR += (s16)MA*(s16)MB`; then `OF = (MR>>40)&1`; MR kept to 40 bits → MR1-MR5 ($2306-$230A). **MB cleared to 0** after. |

Divide algorithm (bsnes, rounds toward −∞): `d = (s16)MA + (u16)MB*65536; rem = d % MB;
quot = d / MB − 65536; MR = rem<<16 | quot`.

- **Division by zero (MB==0)**: `MR = 0` (both quotient and remainder read 0). **OF is NOT
  touched** by multiply or divide — only the cumulative-sum path writes `OF`. (Resolves the
  earlier [VERIFY]: there is no divide-by-zero overflow flag.)
- **Cumulative sum**: each MBH write does `MR += MA*MB`; `OF` ($230B) = bit 40 of the running
  sum (set when the 40-bit accumulator overflows), then MR is truncated to 40 bits. Start a
  fresh string by writing $2250 with acm=1, which zeroes MR (see §3.6).
- **Latency**: real hardware needs a few cycles before MR is valid (~5 cyc mul/div, ~6 cyc
  sum — *not verified upstream this pass*). bsnes computes the result **instantly** on the
  $2254 write and models no latency; do the same unless a self-test requires otherwise.

### Variable-length bit processing ($2258-$225B → $230C-$230D)

Reads an arbitrary bit stream from ROM MSB-first. Write the 24-bit ROM byte address to
VDA ($2259-$225B; write to $225B arms it). `VBD` ($2258) sets the field length (1-16)
and mode. Reading VDP ($230C-$230D) returns the next `length` bits right-justified in a
16-bit window.

- **Auto-increment mode (VBD.H=1)**: reading VDP high byte ($230D) advances the internal
  bit pointer by `length` bits automatically.
- **Fixed mode (VBD.H=0)**: the pointer only advances when a new length/address is
  written; used to re-read or manually control the stream.

Used for fast decompression of Huffman/variable-width encoded data.

---

## 6. Normal DMA

Configured via DCNT ($2230), source SDA ($2232-$2234), dest DDA ($2235-$2237), count DTC
($2238-$2239). Source can be ROM/BW-RAM/I-RAM; destination I-RAM or BW-RAM. Writing the
final destination byte triggers the transfer: **$2236 → destination is I-RAM**, **$2237 →
destination is BW-RAM**. On completion, if CIE.D is set, a SA-1 DMA-end IRQ fires
(status D in CFR). Byte-copy; no address-mode fan-out like the S-CPU DMA.

---

## 7. Character-conversion DMA

Converts linear ("bitmap") BW-RAM pixel data into SNES planar tile (bitplane) format,
because the S-CPU/PPU expect bitplane tiles but a bitmap framebuffer is linear.

- **Type 1 (auto, DCNT.M=1, DCNT.T=0)**: on-the-fly. The S-CPU reads tile data from the
  BW-RAM window; the SA-1 intercepts and converts each 8×8 tile from bitmap to bitplane
  using the BRF register file ($2240-$224F, double-buffered) and CDMA ($2231) parameters
  (color depth CC, row width SSS). A char-conv DMA IRQ (SFR.D / SIE.C) signals readiness.
- **Type 2 (DCNT.M=1, DCNT.T=1)**: block conversion of a region from BW-RAM to I-RAM in
  one shot, driven by the SA-1.

`CDMA.E` is set by the S-CPU to signal end of the conversion sequence. `BBF` ($223F)
picks 2bpp vs 4bpp bitmap source format for the virtual-memory banks $60-$6F.

---

## 8. Cartridge detection (ROM header)

| Header field | Offset (LoROM $7FDx / HiROM $FFDx) | SA-1 value                    |
|--------------|------------------------------------|-------------------------------|
| map_mode     | $FFD5 (`001smmmm`, s=speed)        | low nibble `mmmm = $3` → SA-1. Full byte typically **$23** (slow) or **$33** (fast) |
| chipset      | $FFD6 (`hhhhllll`)                 | high nibble `h = $3` → SA-1. Full byte commonly **$32-$35** (low nibble = ROM/RAM/battery config: $2=ROM+RAM+batt via coproc path, $3/$4/$5 variants) |

SA-1 carts use a HiROM-ish header located at **$00:FFC0** region; the map byte $23 is the
canonical SA-1 marker. Confirm with the chipset high nibble $3x.

Coprocessor high-nibble table ($FFD6): $0x DSP, $1x SuperFX/GSU, $2x OBC1, **$3x SA-1**,
$4x S-DD1, $5x S-RTC, $Ex Other, $Fx Custom.

---

## 9. Commonly simplified in emulators

- Bus-conflict stalls between S-CPU and SA-1 (priority bits) — usually ignored; run SA-1
  full-speed with catch-up.
- Arithmetic 5/6-cycle latency — usually treated as instant.
- BW-RAM/I-RAM write-protection registers — often ignored (games rarely rely on faults).
- `VC` $230E version register — open bus on hardware; safe to return open bus / 0.
- Char-conversion DMA type-1 exact BRF double-buffer timing — approximated.
- `BWPA` protected-area size = 256·2^AAAA (resolved, §3.4); bsnes stores the raw value and
  modern builds do not enforce the fault — safe to ignore unless a title depends on it.

## 10. Timing details that matter most for correctness

1. **Catch-up ratio**: SA-1 = master/2. Step it `master_cycles/2` per S-CPU advance, or
   games' SA-1 code runs at the wrong speed and self-tests / frame pacing break.
2. **MMC bank remapping**: CXB/DXB/EXB/FXB must take effect immediately for subsequent
   fetches by both CPUs; the SA-1 typically runs code out of remapped ROM.
3. **Message/IRQ handshake ports** (CCNT/SCNT/SIE/SIC/CIE/CIC, SFR/CFR): the two CPUs
   synchronize through these. IRQ enable/clear semantics (write-1-to-clear) must be exact
   or a CPU deadlocks waiting on the other. This is the #1 thing to get right for boot.
4. **Reset/NMI/IRQ vector overrides** ($2203-$220F): the SA-1 boots from CRV, and vector
   selects (SCNT.S/N) redirect the S-CPU — wrong routing hangs at startup.
5. **DMA-completion IRQ and the trigger addresses** ($2236 I-RAM / $2237 BW-RAM): games
   poll the DMA-end flag; missing the IRQ/flag stalls asset loading.
