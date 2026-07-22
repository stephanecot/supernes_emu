# SNES SuperFX / GSU (Graphics Support Unit) Reference

Source: fullsnes (nocash SNES specs) section "SNES Cart GSU-n (programmable RISC
CPU) (aka Super FX/Mario Chip)", plus nesdev wiki `Super_FX` and ROM header page.
All hex values transcribed from fullsnes, not guessed. Items fullsnes itself marks
uncertain, or that it does not state, are flagged **[VERIFY]**.

The GSU is a custom **16-bit RISC** cartridge coprocessor (NOT a 65C816 — do not
reuse the 65C816 core). 14 general-purpose 16-bit registers + PC + one scratch,
98 instructions, 512-byte code cache, native pixel-plotting for bitmap graphics.

Variants: **MC1**/Mario Chip 1 (10.74 MHz), **GSU1** (10.74 MHz, ≤1 MB ROM),
**GSU2 / GSU2-SP1** (adds 21 MHz mode, ≤2 MB ROM). Test ROM (Yoshi's Island) is
GSU-2 (VCR=`04h`). Master clock 21.477 MHz; GSU base clock 10.74 MHz (=master/2),
optional 21.4 MHz via CLSR/CFGR.

---

## 1. Register file R0-R15 (all 16-bit, at SNES $3000-$301F)

| Reg | SNES addr | Role |
|-----|-----------|------|
| R0  | $3000/1 | Default source (Sreg) AND destination (Dreg) when no prefix selects one |
| R1  | $3002/3 | PLOT: X coordinate. `0000h` on reset |
| R2  | $3004/5 | PLOT: Y coordinate. `0000h` on reset |
| R3  | $3006/7 | General purpose |
| R4  | $3008/9 | LMULT: lower 16 bits of 32-bit product |
| R5  | $300A/B | General purpose |
| R6  | $300C/D | FMULT / LMULT: the multiplier operand |
| R7  | $300E/F | MERGE source (high bytes) |
| R8  | $3010/1 | MERGE source (low bytes) |
| R9  | $3012/3 | General purpose |
| R10 | $3014/5 | General purpose (conventionally stack pointer) |
| R11 | $3016/7 | LINK: destination (return address) |
| R12 | $3018/9 | LOOP: counter |
| R13 | $301A/B | LOOP: branch target address |
| R14 | $301C/D | GETB/GETC ROM address pointer (auto-prefetches [ROMBR:R14] on write) |
| R15 | $301E/F | Program Counter. **Writing MSB ($301F) sets GO=1 and starts the GSU** |

R15 always holds the address of the *next* opcode; it may be used as Sreg (jump
by MOV/ALU into R15). Register 16-bit write latch: writes to even $3000-$301E set
LATCH=data (byte); writes to odd $3001-$301F apply LSB=LATCH, MSB=data (16-bit
commit). Writing $301F additionally sets GO=1.

---

## 2. SFR — Status/Flag Register ($3030/$3031, R; bits 1-5 R/W)

| Bit | Name | Meaning |
|-----|------|---------|
| 0   | -    | Always 0 |
| 1   | Z    | Zero flag (1 = zero/equal) |
| 2   | CY   | Carry (1 = carry / no-borrow) |
| 3   | S    | Sign (1 = negative, = result bit15) |
| 4   | OV   | Overflow |
| 5   | GO   | GSU running (set by GO start, cleared by STOP; SNES may force-clear) |
| 6   | R    | ROM[R14] being read (1 = a GETxx ROM read in progress) (R) |
| 7   | -    | Always 0 |
| 8   | ALT1 | ALT1 prefix active |
| 9   | ALT2 | ALT2 prefix active |
| 10  | IL   | Immediate lower-8 flag (internal, set/reset while decoding imm operands) |
| 11  | IH   | Immediate upper-8 flag (internal) |
| 12  | B    | WITH prefix active (makes next `1n`/`Bn` byte act as MOVE/MOVES) |
| 13  | -    | Always 0 |
| 14  | -    | Always 0 |
| 15  | IRQ  | Interrupt flag. Set on STOP; **cleared when SFR is read** |

SFR is R/W even while GSU runs. Reading is mainly for polling GO and IRQ. Writing
SFR with GO=0 aborts the program AND forces **CBR=0000h** and marks all cache
lines empty (it also clobbers the other SFR bits, so no pause/resume).

ALT3 prefix = ALT1+ALT2 both set. IL/IH are internal; emulators rarely expose them.

---

## 3. SNES-visible control registers ($3032-$303F)

| Addr | Reg | Acc | Description |
|------|-----|-----|-------------|
| $3033 | BRAMR | W | Backup-RAM enable bit0 (0=protect,1=enable). No effect on shipped PCBs |
| $3034 | PBR   | R/W | Program Bank (8-bit, banks $00-$5F ROM, $70-$71 RAM, or cache) |
| $3036 | ROMBR | R   | ROM Bank for GETxx (8-bit, $00-$5F). Written via ROMB opcode |
| $3037 | CFGR  | W   | Config: bit5 MS0 multiplier-speed (0=std,1=high), bit7 IRQ mask (1=disable STOP IRQ) |
| $3038 | SCBR  | W   | Screen Base, in 1 KB units. Base = $700000 + N*$400 |
| $3039 | CLSR  | W   | Clock Select: bit0 CLS (0=10.7 MHz, 1=21.4 MHz) |
| $303A | SCMR  | W   | Screen Mode (see §6) |
| $303B | VCR   | R   | Version Code (1=MC1/Blob, 4=GSU2; others unassigned/**[VERIFY]**) |
| $303C | RAMBR | R   | RAM Bank (1-bit, $70/$71). Written via RAMB opcode |
| $303E/F | CBR | R   | Cache Base (upper 12 bits; low 4 unused). Read-only; SNES clears it to 0 via SFR GO=0 |

CFGR MS0 **must** be 0 in 21 MHz mode. CFGR IRQ=1 masks the STOP interrupt but
STOP still sets SFR.IRQ. `$3020-$302F` and `$3035`,`$3032`,`$303D` are unused/mirror.

During GSU operation, only **SFR, SCMR, and VCR** may be safely accessed by SNES.

---

## 4. Memory map & bus arbitration

### GSU2, SNES side (banks $00-$3F unless noted)
| Range | Contents |
|-------|----------|
| $00-3F/80-BF:3000-34FF | GSU I/O ports (see mirrors below) |
| $00-3F/80-BF:6000-7FFF | Mirror of $70:0000-1FFF (first 8 KB of Game Pak RAM) |
| $00-3F:8000-FFFF | Game Pak ROM, LoROM mapping (2 MB max) |
| $40-5F:0000-FFFF | Game Pak ROM, HiROM mapping (linear mirror of above) |
| $70-71:0000-FFFF | Game Pak RAM (128 KB max; usually 32 KB or 64 KB) |
| $78-79:0000-FFFF | Additional backup RAM (usually none) |

Both LoROM and HiROM windows are linear (bank $40 = mirror of banks $00-$01).
Header + vectors live at ROM offset `7Fxxh` (LoROM fashion); header declares
LoROM. Fast banks $80-$FF ROM are **unused** → GSU games are Slow-ROM only.

### GSU side (as seen by the GSU)
| Range | Contents |
|-------|----------|
| $00-3F:0000-7FFF | Mirror of LoROM $..:8000-FFFF (for "GETB R15" vectors) |
| $00-3F:8000-FFFF | Game Pak ROM LoROM (2 MB) |
| $40-5F:0000-FFFF | Game Pak ROM HiROM (mirror) |
| $70-71:0000-FFFF | Game Pak RAM |
| PBR:0000-01FF | Code-cache (when opcodes have been stored there) |

PBR may point to ROM, RAM, or cache; ROMBR only ROM ($00-$5F); RAMBR only RAM
($70-$71). Existing carts have ≤64 KB RAM so RAMBR is effectively always 0.

### Arbitration (the critical shared-bus rule)
SCMR bits RON (ROM) and RAN (RAM) grant the bus: **0 = SNES owns, 1 = GSU owns**.
Only one side at a time may touch Game Pak ROM/RAM. While the GSU runs with a
resource granted to it, the SNES CPU is locked out of that resource. If RON/RAN
are cleared mid-run, the GSU enters **WAIT** on its next ROM/RAM access and
resumes when they are re-set. Four internal buses (SNES, ROM, RAM, cache) let the
GSU overlap opcode-fetch / ROM-prefetch / RAM-store.

### GSU exception vectors (visible to SNES while GO=1 & RON=1)
ROM is unmapped from the SNES; fixed values appear based on address low nibble:
`[..E4h]=0104h` COP, `[..E6h]=0100h` BRK, `[..E8h]=0100h` ABT, `[..EAh]=0108h`
NMI, `[..EEh]=010Ch` IRQ (H/V-IRQ & GSU-STOP). Games should set their real ROM
vectors to the same addresses so vectors don't shift when the GSU runs.

---

## 5. GO / run mechanism & IRQ

Start: SNES writes R15 (set PC), the write of the **MSB at $301F** sets SFR.GO=1
and the GSU begins fetching at PBR:R15. Stop: the **STOP** opcode clears GO, sets
SFR.IRQ, and (unless CFGR bit7 masks it) raises the GSU→SNES IRQ line. IRQ is
cleared when the SNES reads SFR. SNES can abort a running GSU by writing SFR with
GO=0 (also forces CBR=0, empties cache). Restart-after-STOP by re-setting GO is
possible (used by Dirt Trax FX).

---

## 6. Bitmap plotting

### SCMR ($303A)
| Bit | Name | Meaning |
|-----|------|---------|
| 0-1 | MD0-1 | Color depth: 0=4-color, 1=16-color, 2=reserved, 3=256-color |
| 2   | HT0   | Screen height LSB |
| 3   | RAN   | Game Pak RAM bus owner (0=SNES, 1=GSU) |
| 4   | RON   | Game Pak ROM bus owner (0=SNES, 1=GSU) |
| 5   | HT1   | Screen height MSB |
| 6-7 | -     | Unused |

Height (HT1:HT0): 0=128 px, 1=160 px, 2=192 px, 3=OBJ mode (256 px). OBJ mode can
also be forced by POR bit4 (then HT0/HT1 ignored).

### COLR — Color Register (not SNES-addressable)
8-bit current plot color CD0-7. Set by COLOR/GETC opcodes.

### POR — Plot Option Register (set by CMODE opcode, bits 0-4)
| Bit | Effect |
|-----|--------|
| 0 | Transparent: 0 = do NOT plot color 0 (PLOT still increments R1), 1 = plot color 0 |
| 1 | Dither: 1 = if (R1.b0 XOR R2.b0)=1 use COLR>>4 as color (4/16-color only; ignored in 256-color) |
| 2 | High-nibble: 1 = COLOR/GETC replaces incoming LSB nibble by incoming MSB nibble |
| 3 | Freeze-high: 1 = COLOR/GETC writes only COLR low byte (protect MSB); also forces OBJ mode |
| 4 | OBJ mode: 1 = force OBJ mapping, ignore SCMR HT0/HT1 |

Color-0 transparency test checks the low 2/4/8 bits per depth; with Freeze-high it
checks only low 2/4 bits (ignores upper nibble even in 256-color).

### Tile number from pixel (X,Y)
```
Height 128 : (X/8)*10h + (Y/8)
Height 160 : (X/8)*14h + (Y/8)
Height 192 : (X/8)*18h + (Y/8)
OBJ mode   : (Y/80h)*200h + (X/80h)*100h + (Y/8 AND 0Fh)*10h + (X/8 AND 0Fh)
```

### Tile-row byte address (bitplane storage in Game Pak RAM)
```
4-color   : TileNo*10h + SCBR*400h + (Y AND 7)*2
16-color  : TileNo*20h + SCBR*400h + (Y AND 7)*2
256-color : TileNo*40h + SCBR*400h + (Y AND 7)*2
```
Plane0/1 at Addr+0, plane2/3 at Addr+$10, plane4/5 at Addr+$20, plane6/7 at
Addr+$30. Column X selects bit `7-(X AND 7)` within each plane byte. The BG map
in these three heights is just columns of increasing tile numbers; OBJ mode maps
to SNES 2-D OBJ layout (entries 0..3FF; SNES OBJ supports only 0..1FF).

---

## 7. Caches

### Code cache (512 bytes, $3100-$32FF SNES / PBR:0000-01FF GSU)
32 lines × 16 bytes. Used only for **opcode fetches** from ROM/RAM. SNES address
of a cached byte: `(CBR AND 1FFh) + 3100h`. Cache is ~3× faster than uncached
ROM/RAM in 10 MHz mode (reportedly 6× in 21 MHz mode) **[VERIFY exact factor]**.

Cache-emptying / CBR-set events:
- `CACHE` opcode → CBR = R15 AND FFF0h (R15 = addr after CACHE), mark all empty.
- `LJMP` → CBR = R15 AND FFF0h (R15 = jump target), mark all empty.
- SNES writes SFR with GO=0 → CBR=0000h, mark all empty.
- `STOP` clears GO but **does NOT** empty the cache (allows re-use on restart).

Lines load *while* executing (partial line loads finish on jump/CACHE). SNES loads
code by: write SFR=0000h (CBR=0, empties), write 16-byte lines to $3100-$32FF —
writing the last byte `[3xxFh]` marks that line non-empty; then run from
R15=$0000-$01Fx.

### Pixel cache (two 8-pixel rows)
Primary cache written by PLOT; forwarded to Secondary, then to RAM. Each holds 8
pixels (2/4/8-bit) + 8 "plotted" flags. X/Y use low bits of R1/R2; `(X AND F8h)`
and `(Y AND FFh)` are memorized. Flush occurs when: cache full (all 8 flags set),
RPIX executed, or R1/R2 changed to a new tile-row **[VERIFY item 3]**. On partial
flush (<8 flags), data is merged with existing RAM. `RPIX` never reads from cache —
it forces both pixel caches to RAM first (and WAITs), then reads RAM; primary use
is flushing before STOP or before RPIX read. Always RPIX before STOP.

### ROM read-ahead (1 byte) & RAM write queue (1 byte/word) & RAM-address latch (1 word)
GETB/GETC read from [ROMBR:R14]; the 1-byte read-ahead is (re)loaded whenever an
opcode changes R14, so a following GETxx runs waitless. STB/STW/SM/SMS/SBK queue
one byte/word so the next opcode can fetch while the write drains. `SBK` writes a
word to the most-recently-used RAM address (latched by LM/LMS etc.), avoiding
repeated immediate operands.

---

## 8. Prefix system (ALT1 / ALT2 / ALT3 / TO / WITH / FROM)

| Byte | Prefix | Sets | Effect |
|------|--------|------|--------|
| `3D` | ALT1 | SFR.ALT1 | Selects the ALT1 variant of the following opcode |
| `3E` | ALT2 | SFR.ALT2 | Selects the ALT2 variant |
| `3F` | ALT3 | ALT1+ALT2 | Selects the ALT3 variant |
| `1n` | TO Rn | Dreg=Rn | Select Rn as destination |
| `2n` | WITH Rn | B, Sreg=Rn, Dreg=Rn | Select Rn as both src+dst; sets B-flag |
| `Bn` | FROM Rs | Sreg=Rn | Select Rn as source |

Rules:
- Prefixes are **reset after any normal opcode** (B=0, ALT1=0, ALT2=0, Sreg=R0,
  Dreg=R0). This reset applies to *all* opcodes incl. JMP/LOOP/NOP/MOVE — i.e.
  NOP is not literally a no-op (it clears prefix state).
- **Exception: Bxx branch opcodes ($05-$0F) leave prefixes unchanged**, allowing
  a prefix to precede a branch and apply to the byte after the branch.
- WITH sets B: while B=1, a following `1n`/`Bn` byte is executed as MOVE/MOVES
  instead of as a TO/FROM prefix.
- **Ignored prefixes**: if `3D xx` (or `3E xx`) is not a defined opcode, the CPU
  executes plain `xx`. ALT3 falls back to ALT1 if the ALT3 form is undefined, then
  to plain. TO/WITH/FROM are ignored if the next opcode uses no Dreg/Sreg.
- Default Sreg=Dreg=R0 when no prefix selects otherwise.

---

## 9. Full opcode map $00-$FF

Cycle counts are internal GSU clocks; ranges reflect ROM/RAM access latency and
cache state (fullsnes column "Clks"). Flags column `000vscz` = O/S/C/Z affected.

### Special / control
| Op | Mnemonic | Clks | Effect |
|----|----------|------|--------|
| 00 | STOP | 1 | GO=0, IRQ=1, R15=$+2 (prefetches but discards byte at $+1). MC1/GSU1 bug: hangs if run <2 cyc after a RAM write |
| 01 | NOP  | 1 | No-op (still resets prefix state) |
| 02 | CACHE | 1* | If CBR≠(PC AND FFF0h) then CBR=PC AND FFF0h, empty cache |
| 3C | LOOP | 1 | R12=R12-1; if Z=0 then R15=R13. Flags 000-s-z |
| 3D | ALT1 | 1 | Prefix |
| 3E | ALT2 | 1 | Prefix |
| 3F | ALT3 | 1 | Prefix |

### Branches $05-$0F (rel8, `op nn`, 2 clks; prefixes preserved). Target = R15 + signed(nn)
| Op | Mnemonic | Condition |
|----|----------|-----------|
| 05 | BRA | always |
| 06 | BGE | (S XOR OV)=0 |
| 07 | BLT | (S XOR OV)=1 |
| 08 | BNE | Z=0 |
| 09 | BEQ | Z=1 |
| 0A | BPL | S=0 |
| 0B | BMI | S=1 |
| 0C | BCC | CY=0 |
| 0D | BCS | CY=1 |
| 0E | BVC | OV=0 |
| 0F | BVS | OV=1 |

The BYTE after any branch/jump is fetched & executed before the target (pipeline).

### Prefix rows
`10-1F` TO Rn · `20-2F` WITH Rn · `B0-BF` FROM Rn (each 1 clk).

### Register / immediate moves
| Op | Mnemonic | Clks | Effect |
|----|----------|------|--------|
| 1n (B=1) | MOVE Rd,Rs | 2 | Rd=Rs (via `2s`WITH…; encoded as TO-row byte under B-flag) |
| Bs (B=1) | MOVES Rd,Rs | 2 | Rd=Rs, flags 000vs-z (OV=Rs bit7) |
| An pp | IBT Rn,#pp | 2 | Rn = sign-extend(pp) |
| Fn xx yy | IWT Rn,#yyxx | 3 | Rn = yyxx (16-bit immediate) |

(`2s 1d`=MOVE and `2d Bs`=MOVES: the WITH prefix's B-flag turns the `1n`/`Bn` byte
into the move; see §8.)

### ALU (Rd=Dreg, Rs=Sreg). n = register index in low nibble
| Base | ALT1 (3D) | ALT2 (3E) | ALT3 (3F) | Clks | Flags |
|------|-----------|-----------|-----------|------|-------|
| `5n` ADD Rn | ADC Rn | ADD #n | ADC #n | 1 (2 w/prefix) | 000vscz |
| `6n` SUB Rn | SBC Rn | SUB #n | CMP Rn | 1 (2) | 000vscz |
| `7n` AND Rn (n=1..15) | BIC Rn | AND #n | BIC #n | 1 (2) | 000-s-z |
| `Cn` OR Rn (n=1..15) | XOR Rn **[VERIFY]** | OR #n | XOR #n **[VERIFY]** | 1 (2) | 000-s-z |
| `70` MERGE | — | — | — | 1 | see below |
| `C0` HIB | — | — | — | 1 | 000-s-z (SF=bit7) |
| `4F` NOT | — | — | — | 1 | 000-s-z (Rd=Rs XOR FFFFh) |

ADD=Rs+Rn, ADC=Rs+Rn+CY, SUB=Rs-Rn, SBC=Rs-Rn-(CY XOR 1), CMP=Rs-Rn (flags only),
BIC=Rs AND NOT Rn. `#n` forms use the low nibble as a 0..15 immediate.
MERGE (`70`): Rd = (R7 AND FF00h) + (R8 >> 8); flags: S=(res AND 8080h)≠0,
V=(res AND C0C0h)≠0, C=(res AND E0E0h)≠0, Z=(res AND F0F0h)≠0 (note: Z set when
those bits nonzero, opposite of usual).

### Shift / rotate / inc / dec / byte ops
| Op | Mnemonic | Clks | Flags | Effect |
|----|----------|------|-------|--------|
| 03 | LSR  | 1 | 000-0cz | Rd = Rs >> 1 (logical) |
| 04 | ROL  | 1 | 000-scz | rotate left through carry |
| 96 | ASR  | 1 | 000-scz | Rd = Rs >> 1 (arithmetic) |
| 3D 96 | DIV2 | 2 | 000-scz | ASR but Rd=0 if Rs=-1 |
| 97 | ROR  | 1 | 000-scz | rotate right through carry |
| Dn | INC Rn (n=0..14) | 1 | 000-s-z | Rn=Rn+1 |
| En | DEC Rn (n=0..14) | 1 | 000-s-z | Rn=Rn-1 |
| 4D | SWAP | 1 | 000-s-z | Rd = Rs ROR 8 (byte swap) |
| 95 | SEX  | 1 | 000-s-z | Rd = sign-extend(Rs AND FFh) |
| 9E | LOB  | 1 | 000-s-z | Rd = Rs AND FFh (SF from bit7) |
| C0 | HIB  | 1 | 000-s-z | Rd = Rs >> 8 (SF from bit7) |

### Multiply
| Op | Mnemonic | Clks | Flags | Effect |
|----|----------|------|-------|--------|
| 8n | MULT Rn | 1/2 | 000-s-z | Rd = signed(Rs.lsb * Rn.lsb) |
| 3E 8n | MULT #n | 2/3 | 000-s-z | Rd = signed(Rs.lsb * n) |
| 3D 8n | UMULT Rn | 2/3 | 000-s-z | Rd = unsigned(Rs.lsb * Rn.lsb) |
| 3F 8n | UMULT #n **[VERIFY]** | 2/3 | 000-s-z | Rd = unsigned(Rs.lsb * n) |
| 9F | FMULT | 4/8 | 000-scz | Rd = signed(Rs * R6 / 10000h) (high word) |
| 3D 9F | LMULT | 5/9 | 000-scz | R4 = low word, Rd = high word of signed(Rs*R6) |

Clks pairs = high-speed / standard multiply (CFGR MS0). Do not use FMULT with
Dreg=R4 (leaves R4 unchanged); LMULT with Dreg=R4 makes R4 hold the MSB (LSB lost).

### Load / store (ROM & RAM)
| Op | Mnemonic | Clks | Effect |
|----|----------|------|--------|
| EF | GETB | 1-6 | Rd = zero-ext byte [ROMBR:R14] |
| 3D EF | GETBH | 2-6 | Rd.hi = byte [ROMBR:R14], lo unchanged |
| 3E EF | GETBL | 2-6 | Rd.lo = byte, hi unchanged |
| 3F EF | GETBS | 2-6 | Rd = sign-ext byte [ROMBR:R14] |
| DF | GETC | 1-6 | COLR = byte [ROMBR:R14] |
| 3E DF | RAMB | 2 | RAMBR = Rs AND 01h |
| 3F DF | ROMB | 2 | ROMBR = Rs AND FFh |
| 4n | LDW (Rn) | 7 | Rd = word [RAMBR:Rn] (n=0..11) |
| 3D 4n | LDB (Rn) | 6 | Rd = zero-ext byte [RAMBR:Rn] (n=0..11) |
| 3D Fn lo hi | LM Rn,(hilo) | 11 | Rn = word [RAMBR:hilo] |
| 3D An kk | LMS Rn,(kk) | 10 | Rn = word [RAMBR:kk*2] |
| 3n | STW (Rn) | 1-6 | word [RAMBR:Rn] = Rs (n=0..11) |
| 3D 3n | STB (Rn) | 2-5 | byte [RAMBR:Rn] = Rs.lo (n=0..11) |
| 3E Fn lo hi | SM (hilo),Rn | 4-9 | word [RAMBR:hilo] = Rn |
| 3E An kk | SMS (kk),Rn | 3-8 | word [RAMBR:kk*2] = Rn |
| 90 | SBK | 1-6 | word [last RAM addr] = Rs |

Word at odd address accesses (addr AND NOT 1) with LSB/MSB swapped. LDB zero-fills
hi; STB stores only Rs.lo.

### Bitmap opcodes
| Op | Mnemonic | Clks | Effect |
|----|----------|------|--------|
| 4E | COLOR | 1 | COLR = Rs AND FFh (through POR nibble/freeze logic) |
| 3D 4E | CMODE | 2 | POR = Rs AND 1Fh |
| 4C | PLOT | 1-48 | Plot pixel (R1,R2)=COLR into pixel cache; R1=R1+1 |
| 3D 4C | RPIX | 20-74 | Rd = pixel at (R1,R2) after flushing both pixel caches to RAM. Flags 000-s-z |

RPIX SF: fullsnes uncertain whether SF is always 0 **[VERIFY]**.

### Jump / link / cache-control
| Op | Mnemonic | Clks | Effect |
|----|----------|------|--------|
| 9n (n=8..13) | JMP Rn | 1 | R15 = Rn (98-9D) |
| 3D 9n (n=8..13) | LJMP Rn | 2 | R15 = Rs, PBR = Rn, sets CBR=(R15 AND FFF0h), empty cache |
| 9n (n=1..4) | LINK #n | 1 | R11 = R15 + n (91-94) |
| 3C | LOOP | 1 | (listed above) |
| 02 | CACHE | 1* | (listed above) |

### Opcode-byte → mnemonic quick index (base, no prefix)
```
00 STOP 01 NOP  02 CACHE 03 LSR  04 ROL  05 BRA  06 BGE  07 BLT
08 BNE  09 BEQ  0A BPL   0B BMI  0C BCC  0D BCS  0E BVC  0F BVS
10-1F TO Rn            20-2F WITH Rn
30-3B STW (Rn)  3C LOOP  3D ALT1  3E ALT2  3F ALT3
40-4B LDW (Rn)  4C PLOT  4D SWAP  4E COLOR 4F NOT
50-5F ADD Rn           60-6F SUB Rn
70 MERGE  71-7F AND Rn
80-8F MULT Rn
90 SBK 91-94 LINK#n 95 SEX 96 ASR 97 ROR 98-9D JMP Rn 9E LOB 9F FMULT
A0-AF IBT Rn,#pp       B0-BF FROM Rn
C0 HIB  C1-CF OR Rn
D0-DE INC Rn  DF GETC
E0-EE DEC Rn  EF GETB
F0-FF IWT Rn,#imm16
```

---

## 10. Cartridge detection (SNES ROM header)

Header at ROM offset `7Fxxh` (LoROM). Relative header-struct offsets: map_mode at
`$15` ($FFD5), chipset/cart-type at `$16` ($FFD6).

| Field | Value | Meaning |
|-------|-------|---------|
| map_mode ($FFD5) | `20h` | LoROM / Slow (GSU declares LoROM even though HiROM window exists) |
| chipset ($FFD6) | `13h` | ROM+MarioChip1+ExpansionRAM |
| chipset ($FFD6) | `14h` | ROM+GSU+RAM (≤1 MB → GSU1) |
| chipset ($FFD6) | `15h` | ROM+GSU+RAM+Battery (>1 MB → GSU2) — Yoshi's Island |
| chipset ($FFD6) | `1Ah` | ROM+GSU1+RAM+Battery+Fast (Stunt Race FX) |
| ($FFD8) SRAM size | `00h` | Normal SRAM = none (GSU uses the expansion entry instead) |
| ($FFBD) Exp-RAM size | `05h`/`06h` | (1<<n) KB → 32 KB / 64 KB. **Absent** in Star Fox, Powerslide, Star Fox 2 |

Chipset high-nibble `1x` = coprocessor is GSU. No header field distinguishes GSU1
vs GSU2 — infer from ROM size / VCR at runtime (2 MB ROM ⇒ typically GSU2, but
Star Fox 2 is 1 MB GSU2, so not a hard rule). Star Fox/Powerslide/Star Fox 2 lack
the extended header, so [FFBD] RAM size must be defaulted (Star Fox = 32 KB).

---

## 11. What emulators commonly simplify / what matters

- **Bus arbitration** most matters: locking SNES out of ROM/RAM while GO=1 &
  RON/RAN grant them; getting this wrong corrupts graphics or hangs games.
- **Prefix state machine** (ALT/TO/WITH/FROM, reset-except-branches, ignored-if-
  undefined) is essential for correct decode — Doom relies on the ignored-prefix
  behavior with conditional jumps.
- **Pipeline byte after jumps/branches** (one byte fetched from post-jump address,
  executed before target) is real hardware behavior needed by some code.
- **Pixel cache flush semantics** (flush on full / RPIX / coordinate change, merge
  on partial) affect plotted output correctness.
- Commonly simplified: exact uncached cycle counts (fullsnes itself is uncertain,
  see below), the 1-byte ROM read-ahead / RAM write-queue timing (often modeled as
  "instant" with a fixed per-access cost), and MC1/GSU1 STOP-after-RAM-write bug.
- **Cycle counts are approximate upstream.** fullsnes "Uncncached ROM/RAM timings":
  ROM read 5 cyc/byte @21 MHz or 3 @10 MHz; RAM write ~10 cyc/word @21 MHz; opcode
  byte read 3 cyc. It explicitly notes these "aren't well documented" — treat all
  multi-clk ranges (GETxx 1-6, PLOT 1-48, RPIX 20-74, LM/SM etc.) as approximate.

---

## Values I could NOT fully verify upstream (flagged [VERIFY] above)
1. XOR Rn (`3D Cn`) / XOR #n (`3F Cn`) and UMULT #n (`3F 8n`) — fullsnes marks the
   opcode encodings with "(?)"; they are undocumented in book2 but listed in the
   summary/index.
2. RPIX sign-flag behavior (fullsnes: "Unknown if RPIX always sets SF=0").
3. Pixel-cache flush on R1/R2 change ("really?" in fullsnes).
4. Cache speedup factor 3× vs 6× (fullsnes: unsure whether 6× is the 21 MHz figure).
5. VCR values for MC1-SMD, GSU1, GSU1A, GSU2-SP1 (only MC1/Blob=1 and GSU2=4 given).
6. Exact uncached cycle timings (fullsnes states they are poorly documented).
7. Whether RAMBR affects SCBR screen base (book2 p.258 claim, unconfirmed).
