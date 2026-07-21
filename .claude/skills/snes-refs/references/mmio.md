# SNES Bus & MMIO Reference

Sources: snes.nesdev.org wiki (MMIO registers, DMA registers, PPU registers, ROM header,
Multiplication, Division, Standard controller) and fullsnes (memory map, timing, open bus).

## 1. System memory map

### Bank overview

| Bank      | Offset        | Content                                   | Speed (master cyc) |
|-----------|---------------|-------------------------------------------|--------------------|
| $00-$3F   | $0000-$7FFF   | System area (see below)                   | see below          |
| $00-$3F   | $8000-$FFFF   | WS1 LoROM (max 2048 KB, 64x32K)           | 8                  |
| $40-$7D   | $0000-$FFFF   | WS1 HiROM (max 3968 KB, 62x64K)           | 8                  |
| $7E-$7F   | $0000-$FFFF   | WRAM (128 KB, linear)                     | 8                  |
| $80-$BF   | $0000-$7FFF   | System area (mirror of $00-$3F layout)    | see below          |
| $80-$BF   | $8000-$FFFF   | WS2 LoROM                                 | 8 or 6 (MEMSEL)    |
| $C0-$FF   | $0000-$FFFF   | WS2 HiROM (max 4096 KB, 64x64K)           | 8 or 6 (MEMSEL)    |

$00:$FFE0-$FFFF = CPU exception vectors. Banks $80-$BF/$C0-$FF are normally cartridge
mirrors of $00-$3F/$40-$7D (WS1/WS2 usually wired identically on real carts).

### System area (banks $00-$3F and $80-$BF, offsets $0000-$7FFF)

| Offset        | Content                                        | Speed (master cyc) |
|---------------|------------------------------------------------|--------------------|
| $0000-$1FFF   | Mirror of $7E:0000-$7E:1FFF (first 8K WRAM)    | 8                  |
| $2000-$20FF   | Unused (open bus)                              | 6                  |
| $2100-$21FF   | I/O ports (B-bus: PPU, APU, WRAM port)         | 6                  |
| $2200-$3FFF   | Unused (open bus) / expansion (A-bus)          | 6                  |
| $4000-$41FF   | I/O ports, manual joypad ($4016/$4017); $4000-$4015 and $4018-$41FF open bus | 12 |
| $4200-$5FFF   | CPU I/O ports ($4200-$421F, $4300-$437F); rest open bus | 6         |
| $6000-$7FFF   | Expansion (HiROM SRAM window)                  | 8                  |

### Memory access speed

Master clock = 21.47727 MHz (NTSC). Cycles per bus access:

| Speed  | Master cycles | Effective rate | Regions                                        |
|--------|---------------|----------------|-----------------------------------------------|
| Fast   | 6             | 3.58 MHz       | $2000-$5FFF in $00-$3F/$80-$BF; WS2 ROM when MEMSEL.0=1 |
| Slow   | 8             | 2.68 MHz       | WRAM (all mirrors), $6000-$7FFF, all WS1 ROM, WS2 ROM when MEMSEL.0=0 |
| XSlow  | 12            | 1.79 MHz       | $4000-$41FF in $00-$3F/$80-$BF (joypad ports)  |

FastROM: MEMSEL $420D bit0=1 switches "Memory-2" ($80-$BF:$8000-$FFFF and $C0-$FF:$0000-$FFFF)
from 8 to 6 cycles. Banks $00-$3F/$40-$7D ROM stays 8 cycles regardless. Internal CPU cycles
(no bus access) are always 6 master cycles.

## 2. Open bus

- Reading unused addresses, write-only registers, or unimplemented bits returns the CPU's
  MDR: the last value driven on the data bus. Typically the last opcode byte for direct
  addressing (`LDA $21C0` -> garbage = $21), or the last operand/indirect-pointer byte for
  indirect reads (`LDA ($NN),Y` -> garbage = [$NN+1]).
- During DMA, open-bus reads reflect DMA-related bus traffic (unpredictable for HDMA).
- PPU read ports have their own PPU1/PPU2 open-bus latches (NOT the CPU MDR): PPU1 open bus
  feeds $2104-$2106/$2108-$210A/$2114-$2116/$2118-$211A/$2124-$2126/$2128-$212A and
  $213E.4; PPU2 open bus feeds $213B/$213C/$213D high bits and $213F.5. Details in ppu.md.
- Write-only CPU registers $4200-$420D read as open bus; unused bits of $4210/$4211/$4212
  are open bus (see below).

## 3. B-bus registers $2100-$213F (PPU)

Full bit semantics in ppu.md; summary (all write regs are W8 unless noted; region NOT
mirrored elsewhere within the bank — only the whole bank layout repeats across $00-$3F/$80-$BF):

| Addr  | Name        | R/W  | Function / bit layout                                        |
|-------|-------------|------|--------------------------------------------------------------|
| $2100 | INIDISP     | W    | `F...BBBB` forced blank (F), brightness 0-15                  |
| $2101 | OBSEL       | W    | `SSSNNBBB` OBJ size select, name select, name base            |
| $2102 | OAMADDL     | W    | OAM word address low                                          |
| $2103 | OAMADDH     | W    | `P......B` priority rotation, OAM addr bit8                   |
| $2104 | OAMDATA     | W x2 | OAM data write (word latch, auto-inc)                         |
| $2105 | BGMODE      | W    | `4321PMMM` tile size BG4-1 (bits7-4), BG3 priority (bit3), BG mode 0-7 (bits2-0) |
| $2106 | MOSAIC      | W    | `SSSS4321` mosaic size, enable BG4-1                          |
| $2107-$210A | BG1SC-BG4SC | W | `AAAAAAYX` tilemap base addr, vertical/horizontal size    |
| $210B | BG12NBA     | W    | `BBBBAAAA` BG2/BG1 chr base (4K-word units)                   |
| $210C | BG34NBA     | W    | `DDDDCCCC` BG4/BG3 chr base                                   |
| $210D-$210E | BG1HOFS/BG1VOFS | W x2 | BG1 scroll (10-bit) / M7HOFS-M7VOFS (13-bit), write-twice |
| $210F-$2114 | BG2-4 H/VOFS | W x2 | BG2-4 scroll, write-twice                              |
| $2115 | VMAIN       | W    | `M...RRII` inc on high/low, addr remap, step 1/32/128         |
| $2116-$2117 | VMADDL/H | W   | VRAM word address                                             |
| $2118-$2119 | VMDATAL/H | W  | VRAM data write (auto-inc per VMAIN)                          |
| $211A | M7SEL       | W    | `RF....YX` tilemap repeat/fill, flip                          |
| $211B-$211E | M7A-M7D | W x2 | Mode 7 matrix, 8.8 signed, write-twice; M7A*M7B(8-bit signed) -> MPY |
| $211F-$2120 | M7X/M7Y | W x2 | Mode 7 center, 13-bit signed, write-twice                     |
| $2121 | CGADD       | W    | CGRAM word address                                            |
| $2122 | CGDATA      | W x2 | CGRAM data `.BBBBBGGGGGRRRRR`, write-twice                    |
| $2123-$2125 | W12SEL/W34SEL/WOBJSEL | W | window 1/2 enable+invert per layer (2 bits each) |
| $2126-$2129 | WH0-WH3 | W    | window 1 left/right, window 2 left/right                      |
| $212A-$212B | WBGLOG/WOBJLOG | W | window combine logic OR/AND/XOR/XNOR                    |
| $212C/$212D | TM/TS   | W    | `...O4321` main/sub screen layer enable                       |
| $212E/$212F | TMW/TSW | W    | main/sub screen window masking enable                         |
| $2130 | CGWSEL      | W    | `MMSS..AD` clip/prevent regions, addend select, direct color  |
| $2131 | CGADSUB     | W    | `MHBO4321` add/sub, half, backdrop, OBJ, BG4-1 enable         |
| $2132 | COLDATA     | W    | `BGRCCCCC` fixed color plane select + intensity               |
| $2133 | SETINI      | W    | `EX..HOiI` ext sync, extbg, pseudo-hires, overscan, OBJ-interlace, interlace |
| $2134-$2136 | MPYL/M/H | R  | signed 24-bit M7A*M7B product                                 |
| $2137 | SLHV        | R    | strobe: latches H/V counters; returns CPU open bus            |
| $2138 | OAMDATAREAD | R x2 | OAM read (auto-inc)                                           |
| $2139-$213A | VMDATALREAD/H | R | VRAM read via prefetch buffer                            |
| $213B | CGDATAREAD  | R x2 | CGRAM read (2nd read bit7 = PPU2 open bus)                    |
| $213C | OPHCT       | R x2 | latched H counter, 9-bit (2nd read bits7-1 = PPU2 open bus)   |
| $213D | OPVCT       | R x2 | latched V counter, 9-bit (2nd read bits7-1 = PPU2 open bus)   |
| $213E | STAT77      | R    | `TRM.VVVV` OBJ time over, range over, master/slave, PPU1 version (bit4=PPU1 open bus) |
| $213F | STAT78      | R    | `FL.MVVVV` field, counter-latch flag, NTSC/PAL, PPU2 version (bit5=PPU2 open bus) |

## 4. APU ports $2140-$2143

| Addr        | Name      | R/W | Function                                             |
|-------------|-----------|-----|------------------------------------------------------|
| $2140-$2143 | APUIO0-3  | RW8 | CPU<->SPC700 mailbox. Read returns what SPC wrote to $F4-$F7; write sets what SPC reads at $F4-$F7. Two independent directions. |

Mirrored every 4 bytes throughout $2140-$217F (e.g. $2144=$2140).

## 5. WRAM port $2180-$2183

| Addr  | Name   | R/W | Function                                                     |
|-------|--------|-----|--------------------------------------------------------------|
| $2180 | WMDATA | RW8 | Read/write WRAM byte at WMADD, then WMADD increments by 1    |
| $2181 | WMADDL | W   | WRAM address bits 7-0                                        |
| $2182 | WMADDM | W   | WRAM address bits 15-8                                       |
| $2183 | WMADDH | W   | WRAM address bit 16 only (17-bit address, 128 KB)            |

WRAM-to-WRAM DMA via $2180 is not possible (same chip on both buses).

## 6. Joypad serial $4016/$4017 (12-cycle region)

| Addr | Dir | Name   | Bits                                                            |
|------|-----|--------|-----------------------------------------------------------------|
| $4016| W   | JOYWR  | bit0 = latch (OUT0, strobe standard controllers while 1); bits1-2 = OUT1/OUT2 (unconnected); bits 7-3 unused |
| $4016| R   | JOYA   | bit0 = port1 data1 line, bit1 = port1 data2 line; bits 7-2 open bus. Read pulses port1 clock (shifts next bit) |
| $4017| R   | JOYB   | bit0 = port2 data1 line, bit1 = port2 data2 line; bits 4-2 always read 1 (tied to GND, active-low); bits 7-5 open bus. Read pulses port2 clock |

Standard controller serial order (bit clocked out 1st..16th):
B, Y, Select, Start, Up, Down, Left, Right, A, X, L, R, 0, 0, 0, 0.

## 7. CPU registers $4200-$420D (write-only; reads = open bus)

| Addr  | Name     | Reset | Bits                                                        |
|-------|----------|-------|-------------------------------------------------------------|
| $4200 | NMITIMEN | $00   | bit7 = VBlank NMI enable; bits5-4 = H/V IRQ mode: 0=disabled, 1=IRQ at H=HTIME (every line), 2=IRQ at V=VTIME H=0, 3=IRQ at H=HTIME and V=VTIME; bit0 = auto-joypad read enable. Setting bits5-4=0 also acknowledges a pending IRQ |
| $4201 | WRIO     | $FF   | bit7 = port2 IOBit (also PPU H/V counter latch on 1->0), bit6 = port1 IOBit, bits5-0 unconnected. Open-collector: write 1 to allow input via $4213 |
| $4202 | WRMPYA   | $FF   | 8-bit unsigned multiplicand                                 |
| $4203 | WRMPYB   | -     | 8-bit unsigned multiplier; write starts multiply            |
| $4204 | WRDIVL   | $FF   | dividend bits 7-0                                           |
| $4205 | WRDIVH   | $FF   | dividend bits 15-8 (16-bit unsigned)                        |
| $4206 | WRDIVB   | -     | 8-bit unsigned divisor; write starts divide                 |
| $4207 | HTIMEL   | $FF   | H-timer target bits 7-0                                     |
| $4208 | HTIMEH   | $01   | H-timer target bit 8 (range 0-339)                          |
| $4209 | VTIMEL   | $FF   | V-timer target bits 7-0                                     |
| $420A | VTIMEH   | $01   | V-timer target bit 8 (range 0-261 NTSC / 0-311 PAL)         |
| $420B | MDMAEN   | $00   | bits7-0 = enable GP-DMA channel 7-0; write starts transfer immediately (CPU halted); cleared per channel on completion |
| $420C | HDMAEN   | $00   | bits7-0 = enable HDMA channel 7-0 (transfers 1 unit/scanline during H-blank) |
| $420D | MEMSEL   | $00   | bit0 = FastROM: WS2 area ($80-$BF:$8000-$FFFF, $C0-$FF) 0=8 cyc, 1=6 cyc; bits7-1 unused |

### Multiply/divide latency (5A22)

| Op       | Start           | Result valid after | Results                                            |
|----------|-----------------|--------------------|----------------------------------------------------|
| Multiply | write $4203     | 8 CPU cycles       | $4216-$4217 = WRMPYA * WRMPYB (16-bit unsigned)    |
| Divide   | write $4206     | 16 CPU cycles      | $4214-$4215 = quotient; $4216-$4217 = remainder    |

- Latency counts CPU cycles from the write; the cycles of the instruction that reads the
  result count toward the wait (e.g. `LDA abs` spends 3 cycles before its data fetch).
- Reading early returns intermediate shift/accumulate state, not garbage and not the result.
- Divide by zero: quotient = $FFFF, remainder = dividend.
- Multiply and divide share $4216-$4217; do not start one while the other is running.

## 8. CPU registers $4210-$421F (read-only)

| Addr  | Name   | Bits                                                                |
|-------|--------|---------------------------------------------------------------------|
| $4210 | RDNMI  | bit7 = VBlank NMI flag (set at VBlank start even if NMI disabled; auto-cleared at VBlank end AND cleared by reading this register); bits6-4 open bus; bits3-0 = CPU version (1 or 2) |
| $4211 | TIMEUP | bit7 = H/V-timer IRQ flag; cleared by reading (must be read in IRQ handler to acknowledge) and by disabling IRQ via $4200 bits5-4=0; bits6-0 open bus |
| $4212 | HVBJOY | bit7 = VBlank flag, bit6 = HBlank flag (toggle every scanline, always); bit0 = auto-joypad read busy; bits5-1 open bus |
| $4213 | RDIO   | bit7 = port2 IOBit input, bit6 = port1 IOBit input, bits5-0 unconnected (read as set by $4201) |
| $4214 | RDDIVL | quotient bits 7-0                                                   |
| $4215 | RDDIVH | quotient bits 15-8                                                  |
| $4216 | RDMPYL | product/remainder bits 7-0                                          |
| $4217 | RDMPYH | product/remainder bits 15-8                                         |
| $4218-$4219 | JOY1L/H | auto-read port1 controller (data1 line)                       |
| $421A-$421B | JOY2L/H | auto-read port2 controller (data1 line)                       |
| $421C-$421D | JOY3L/H | auto-read port1 (data2 line, multitap/2nd controller)         |
| $421E-$421F | JOY4L/H | auto-read port2 (data2 line)                                  |

### Auto-joypad result bit layout ($4218/$4219 shown; same for JOY2-4)

| Bit | 15| 14| 13    | 12   | 11| 10  | 9   | 8    | 7 | 6 | 5 | 4 | 3-0        |
|-----|---|---|-------|------|---|-----|-----|------|---|---|---|---|------------|
| Btn | B | Y | Select| Start| Up| Down| Left| Right| A | X | L | R | 0000 (sig) |

High byte = JOYxH, low byte = JOYxL. Signature bits 3-0 = 0000 for a standard pad.
Auto-read runs at VBlank start when $4200 bit0=1; poll $4212 bit0=0 before reading
$4218-$421F (or reading $4016/$4017 manually — auto-read uses the same serial lines).

## 9. DMA channel registers $43x0-$43xB (x = channel 0-7, all R/W; reset $FF, except $43x4 A1Bx which resets to an undefined value)

| Addr  | Name  | GP-DMA meaning                          | HDMA meaning                          |
|-------|-------|-----------------------------------------|---------------------------------------|
| $43x0 | DMAPx | parameters (below)                      | parameters (below)                    |
| $43x1 | BBADx | B-bus address (port $21xx)              | same                                  |
| $43x2 | A1TxL | A-bus address low (updated during DMA)  | table start address low               |
| $43x3 | A1TxH | A-bus address high                      | table start address high              |
| $43x4 | A1Bx  | A-bus bank (fixed during DMA)           | table bank                            |
| $43x5 | DASxL | byte count low                          | indirect data address low             |
| $43x6 | DASxH | byte count high ($0000 = 65536 bytes)   | indirect data address high            |
| $43x7 | DASBx | (unused)                                | indirect data bank (set by program)   |
| $43x8 | A2AxL | (unused)                                | current table address low (auto)      |
| $43x9 | A2AxH | (unused)                                | current table address high (auto)     |
| $43xA | NLTRx | (unused)                                | `RLLLLLLL` repeat flag + line counter |
| $43xB | UNUSEDx | free R/W byte, no effect; mirrored at $43xF | same                            |
| $43xC-$43xE | - | open bus                               | open bus                              |

### DMAPx ($43x0) bit fields

| Bit  | Field                                                                     |
|------|---------------------------------------------------------------------------|
| 7    | Direction: 0 = A-bus -> B-bus (CPU mem to $21xx), 1 = B-bus -> A-bus      |
| 6    | HDMA addressing: 0 = direct table, 1 = indirect table (HDMA only)         |
| 5    | Unused (readable/writable, ignored)                                       |
| 4-3  | A-bus address step (GP-DMA only): 0 = increment, 2 = decrement, 1/3 = fixed |
| 2-0  | Transfer unit select (below); HDMA transfers one unit per scanline        |

### Transfer unit patterns (B-bus address offsets per byte)

| Mode | Bytes | B-bus addresses          | Typical use                     |
|------|-------|--------------------------|---------------------------------|
| 0    | 1     | xx                       | WRAM $2180, $2118-only          |
| 1    | 2     | xx, xx+1                 | VRAM $2118/$2119                |
| 2    | 2     | xx, xx                   | OAM $2104, CGRAM $2122          |
| 3    | 4     | xx, xx, xx+1, xx+1       | BGnxOFS, M7x (write-twice pairs)|
| 4    | 4     | xx, xx+1, xx+2, xx+3     | BGnSC, windows, APU ports       |
| 5    | 4     | xx, xx+1, xx, xx+1       | (undocumented)                  |
| 6    | 2     | xx, xx                   | same as mode 2                  |
| 7    | 4     | xx, xx, xx+1, xx+1       | same as mode 3                  |

GP-DMA: 16-bit counter, up to $10000 bytes (not units); MDMAEN bit clears when the
channel finishes. HDMA on a channel overrides GP-DMA on the same channel. HDMA
initialization copies A1Tx -> A2Ax and loads NLTRx from the table.

## 10. Cartridge header ($00:FFB0-$00:FFDF as mapped; +vectors $FFE0-$FFFF)

File offset of the header block: LoROM $7FB0-$7FFF, HiROM $FFB0-$FFFF, ExHiROM $40FFB0.

| CPU addr | Size | Field                                                            |
|----------|------|------------------------------------------------------------------|
| $FFB0    | 2    | Maker code (ASCII) — extended header, only when $FFDA = $33      |
| $FFB2    | 4    | Game code (ASCII)                                                |
| $FFB6    | 6    | Reserved (zero)                                                  |
| $FFBC    | 1    | Expansion FLASH size (1<<n KB)                                   |
| $FFBD    | 1    | Expansion RAM size (1<<n KB) (GSU carts)                         |
| $FFBE    | 1    | Special version (usually 0)                                      |
| $FFBF    | 1    | Chipset subtype, used when $FFD6 = $Fx: $00=SPC7110, $01=ST010/011, $02=ST018, $10=CX4 |
| $FFC0    | 21   | Title, JIS ASCII $20-$7E, space-padded                           |
| $FFD5    | 1    | Map mode: `001SMMMM` — bit4 S: 0=Slow(200ns), 1=Fast(120ns); bits3-0: $0=LoROM(mode $20), $1=HiROM($21), $2=LoROM+S-DD1($22), $3=LoROM+SA-1($23), $5=ExHiROM($25), $A=HiROM+SPC7110 |
| $FFD6    | 1    | Chipset: low nibble $0=ROM, $1=ROM+RAM, $2=ROM+RAM+battery, $3=ROM+coproc, $4=+RAM, $5=+RAM+battery, $6=+battery, $9=+RAM+battery+RTC-4513; high nibble = coproc: $0x=DSP, $1x=GSU/SuperFX, $2x=OBC1, $3x=SA-1, $4x=S-DD1, $5x=S-RTC, $Ex=other(SGB/BS-X), $Fx=custom via $FFBF |
| $FFD7    | 1    | ROM size = 1<<n KB, rounded up ($08=256KB .. $0C=4MB)            |
| $FFD8    | 1    | RAM size = 1<<n KB ($00=none, $01=2KB, $03=8KB, $05=32KB)        |
| $FFD9    | 1    | Country code (implies video standard, below)                     |
| $FFDA    | 1    | Developer ID ($00=none/homebrew, $01=Nintendo, $33=see extended header) |
| $FFDB    | 1    | ROM version ($00 = first)                                        |
| $FFDC    | 2    | Checksum complement = checksum XOR $FFFF                         |
| $FFDE    | 2    | Checksum: 16-bit sum of all ROM bytes (computed with $FFDC-$FFDF taken as $FF,$FF,$00,$00) |

### Country codes ($FFD9)

| Code | Region                    | Std  | Code | Region                | Std  |
|------|---------------------------|------|------|-----------------------|------|
| $00  | Japan (or international)  | NTSC | $09  | Germany/Austria/Switz | PAL  |
| $01  | USA and Canada            | NTSC | $0A  | Italy                 | PAL  |
| $02  | Europe/Oceania/Asia       | PAL  | $0B  | China/Hong Kong       | PAL  |
| $03  | Sweden/Scandinavia        | PAL  | $0C  | Indonesia             | PAL  |
| $04  | Finland                   | PAL  | $0D  | South Korea           | NTSC |
| $05  | Denmark                   | PAL  | $0E  | Common (region-free)  | ?    |
| $06  | France                    | SECAM 50Hz (treat PAL) | $0F | Canada | NTSC |
| $07  | Holland                   | PAL  | $10  | Brazil                | PAL-M 60Hz (treat NTSC timing) |
| $08  | Spain                     | PAL  | $11  | Australia             | PAL  |

$12-$14 ($X/$Y/$Z) = other variations. Emulator rule: PAL timing for $02-$0C and $11;
NTSC for $00, $01, $0D, $0F, $10.

### Checksum of non-power-of-2 ROMs

Split into largest power-of-2 part + remainder; the remainder is repeated (mirrored)
until the total reaches the next power of 2, then all bytes summed (e.g. 10 Mbit =
8 Mbit + 4 x last 2 Mbit). Coprocessor on-chip ROM is not included.

### Header scoring heuristic (LoROM vs HiROM vs ExHiROM detection)

Score each candidate location ($7FC0, $FFC0, $40FFC0 file offsets), highest wins:
- checksum ($FFDE) + complement ($FFDC) == $FFFF (strong); checksum matches computed sum (strong)
- map mode ($FFD5) low nibble consistent with candidate location (LoROM<->$7FC0, HiROM<->$FFC0, ExHiROM<->$40FFC0)
- header ROM size >= file size; ROM/RAM size codes reasonable ($FFD7 <= $0D, $FFD8 <= $09)
- title bytes all ASCII $20-$7E
- reset vector ($FFFC in emulation-mode block) >= $8000
- byte at reset vector is a plausible first opcode (SEI/CLC/SEC/STZ/JMP/JML/LDA/LDX...);
  penalize BRK($00)/COP($02)/STP($DB)/WDM($42)/$FF
- Strip a 512-byte copier header first if filesize % 1024 == 512.

### Interrupt vectors ($00:FFE0-$00:FFFF)

| Native (65C816 mode) | Addr  | Emulation (6502 mode) | Addr  |
|----------------------|-------|-----------------------|-------|
| (unused)             | $FFE0 | (unused)              | $FFF0 |
| COP                  | $FFE4 | COP                   | $FFF4 |
| BRK                  | $FFE6 | (unused)              | $FFF6 |
| ABORT (unused)       | $FFE8 | ABORT (unused)        | $FFF8 |
| NMI (VBlank)         | $FFEA | NMI                   | $FFFA |
| (unused)             | $FFEC | RESET (always 6502 mode at reset) | $FFFC |
| IRQ (H/V timer, ext) | $FFEE | IRQ/BRK               | $FFFE |

## 11. LoROM / HiROM address decoding

### LoROM (mode $20)

- ROM: banks $00-$7D and $80-$FF, offsets $8000-$FFFF (System area occupies $00-$3F/$80-$BF
  below $8000): `rom_offset = ((bank & $7F) << 15) | (addr & $7FFF)`
- Undersized ROM mirrors: `rom_offset %= rom_size` (with non-power-of-2 ROMs mirrored as in
  the checksum rule: big part at 0, small part repeated after it).
- SRAM: banks $70-$7D and $F0-$FF, offsets $0000-$7FFF (32K per bank):
  `sram_offset = (((bank & $7F) - $70) << 15 | addr) % sram_size`.
  Board variants exist: SRAM sometimes limited to banks $70-$71/$70-$77, sometimes also
  answering at $8000-$FFFF; do not map SRAM over $7E-$7F (WRAM wins).
- Many boards also mirror ROM into $40-$6F:$0000-$7FFF (same content as $8000-$FFFF);
  safe default: mirror ROM there when no SRAM is mapped.

### HiROM (mode $21)

- ROM: banks $C0-$FF and $40-$7D, offsets $0000-$FFFF:
  `rom_offset = ((bank & $3F) << 16) | addr`, then `% rom_size`.
- Upper halves also appear in system banks: $00-$3F and $80-$BF, offsets $8000-$FFFF map to
  the same `rom_offset = ((bank & $3F) << 16) | addr` — this is what places the vectors of
  ROM offset $xFFE0 at $00:FFE0.
- SRAM: banks $30-$3F and $B0-$BF, offsets $6000-$7FFF (8K per bank):
  `sram_offset = (((bank & $0F) << 13) | (addr - $6000)) % sram_size`.
  Often mirrored down to banks $20-$2F (sometimes $10-$1F).

### ExHiROM (mode $25)

- Banks $C0-$FF -> ROM offset $000000-$3FFFFF; banks $40-$7D -> ROM offset $400000+
  (`rom_offset = ((bank & $3F) << 16) | addr | (bank < $80 ? $400000 : 0)`), header and
  vectors read from file offset $40FFB0+. Used only by Dai Kaiju Monogatari 2 and
  Tales of Phantasia (JP).

### General mirroring rules

- Banks $80-$BF mirror the system area of $00-$3F exactly (WRAM mirror, all MMIO).
- WS1/WS2 cartridge areas are mirrors of each other on standard carts; only MEMSEL speed differs.
- WRAM $7E-$7F is never mirrored above $1FFF in system banks; $00:0000-$1FFF == $7E:0000-$1FFF.
