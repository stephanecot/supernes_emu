# SNES PPU Reference (S-PPU1 + S-PPU2)

Verified against snes.nesdev.org wiki (PPU registers, Backgrounds, Tilemaps, Tiles, Sprites, Offset-per-tile) and fullsnes. `$` = hex.

## 1. Memories

| Memory | Size | Organization | CPU access |
|---|---|---|---|
| VRAM  | 64 KB = 32K words ($0000-$7FFF word addresses) | 16-bit words, two 32KB chips (low/high byte) | $2115-$2119, $2139/$213A; writable only during V-blank/F-blank (active-display writes are ignored) |
| CGRAM | 512 B = 256 words | 256 colors, 15-bit BGR555: `0BBBBBGG GGGRRRRR` (bit14-10 B, 9-5 G, 4-0 R) | $2121/$2122/$213B |
| OAM   | 544 B = 512 B table 1 + 32 B table 2 | 128 sprites | $2102-$2104/$2138 |

CGRAM map: colors 0-127 BG, 128-255 OBJ (8 OBJ palettes × 16). Color 0 (and color 0 of each palette) = transparent. Mode 0 quirk: BGn uses CGRAM base offset (n-1)*32 (BG1=0, BG2=32, BG3=64, BG4=96).

## 2. Tile (character) formats

Bit 7 of each plane byte = leftmost pixel of the row. 8 rows per 8×8 tile.

| Format | Bytes/tile | Layout |
|---|---|---|
| 2bpp | 16 | 8 words, one per row: low byte = plane 0, high byte = plane 1 |
| 4bpp | 32 | bytes 0-15 = planes 0/1 (2bpp block), bytes 16-31 = planes 2/3 (same row-interleaved structure) |
| 8bpp | 64 | four 2bpp blocks: planes 0/1, 2/3, 4/5, 6/7 at byte offsets 0/16/32/48 |

Pixel value = plane0<<0 | plane1<<1 | ... 16×16 BG tiles = 4 8×8 tiles at char +0, +1, +16, +17 (hex: +$00,+$01,+$10,+$11).

## 3. Tilemaps

Entry = 1 word:
```
15 14 13 12-10 9-0
 V  H  P  CCC  TTTTTTTTTT
```
V=vertical flip, H=horizontal flip, P=tile priority (0/1), CCC=palette 0-7, T=character number 0-1023.

Each tilemap = 32×32 entries = $400 words, row-major. BGnSC ($2107-$210A): bits 7-2 = base word address >>10 (i.e. base = AAAAAA<<10), bit1=Y (vertical count), bit0=X (horizontal count):

| SC bits YX | Size (tiles) | Quadrant word offsets from base |
|---|---|---|
| 00 | 32×32 | map0 only |
| 01 | 64×32 | left +$000, right +$400 |
| 10 | 32×64 | top +$000, bottom +$400 |
| 11 | 64×64 | TL +$000, TR +$400, BL +$800, BR +$C00 |

## 4. BG modes ($2105 BGMODE)

$2105: bit7-4 = BG4/BG3/BG2/BG1 char size (0=8×8, 1=16×16), bit3 = Mode 1 BG3 priority, bits 2-0 = mode.

| Mode | BG1 | BG2 | BG3 | BG4 | Notes |
|---|---|---|---|---|---|
| 0 | 2bpp | 2bpp | 2bpp | 2bpp | per-BG palette offset (see §1) |
| 1 | 4bpp | 4bpp | 2bpp | — | BGMODE bit3 lifts BG3-priority-1 tiles above everything |
| 2 | 4bpp | 4bpp | OPT | — | BG3 tilemap = offset-per-tile data |
| 3 | 8bpp | 4bpp | — | — | BG1 direct color possible |
| 4 | 8bpp | 2bpp | OPT | — | BG1 direct color; OPT single-row variant |
| 5 | 4bpp | 2bpp | — | — | hires 512; tiles forced 16 px wide |
| 6 | 4bpp | — | OPT | — | hires 512; tiles forced 16 px wide |
| 7 | 8bpp | EXTBG | — | — | rotation/scaling; BG2 only with SETINI bit6 |

### Offset-per-tile (modes 2, 4, 6)
BG3's tilemap holds offset entries instead of tiles. Modes 2/6: PPU reads 2 rows (row selected by BG3VOFS): first row = horizontal offsets, next row (+32 entries) = vertical offsets. Mode 4: only 1 row; each entry is H or V per its bit 15. Entry format:
```
15 = Mode 4 only: direction (0=horizontal, 1=vertical)
14 = apply to BG2
13 = apply to BG1
9-0 = replacement scroll value (H: low 3 bits ignored — replaces BGnHOFS except its low 3 bits; V: replaces BGnVOFS entirely)
```
Leftmost visible 8-px column is never affected; offset entry 0 applies to the second visible column. Fetch column within BG3 map is shifted by BG3HOFS>>3.

## 5. Scroll registers $210D-$2114 (write twice, low byte first)

Two shared latches for all 8 BG registers (`bgofs_latch` shared by all; `bghofs_latch` updated only by HOFS writes):
```
BGnHOFS write: BGnHOFS = (value<<8) | (bgofs_latch & ~7) | (bghofs_latch & 7);  bgofs_latch = value; bghofs_latch = value
BGnVOFS write: BGnVOFS = (value<<8) | bgofs_latch;                              bgofs_latch = value
```
Values are 10-bit. $210D/$210E additionally write M7HOFS/M7VOFS (13-bit signed) through the separate single Mode 7 latch: `M7xOFS = (value<<8) | mode7_latch; mode7_latch = value`.

## 6. VRAM port

| Reg | Name | Function |
|---|---|---|
| $2115 | VMAIN | bit7: increment after $2118/$2139 access (0) or $2119/$213A access (1); bits3-2 remap; bits1-0 step: 0=+1, 1=+32, 2=+128, 3=+128 (words) |
| $2116/$2117 | VMADDL/H | word address; writing either reloads the read prefetch buffer from the new address |
| $2118/$2119 | VMDATAL/H | write low/high byte of word at current address; address increments by step when the byte selected by VMAIN bit7 is written |
| $2139/$213A | VMDATALREAD/H | read prefetch buffer byte; on the increment-triggering read: return buffer, reload buffer from the CURRENT (pre-increment) address, then increment |

Remap modes (applied to VMADD before access — rotate low 8/9/10 bits left by 3):
```
1: rrrrrrrr YYYccccc -> rrrrrrrr cccccYYY   (2bpp)
2: rrrrrrrY YYcccccP -> rrrrrrrc ccccPYYY   (4bpp)
3: rrrrrrYY YcccccPP -> rrrrrrcc cccPPYYY   (8bpp)
```
Read sequence therefore needs one dummy read after setting VMADD: reads return VRAM[A], VRAM[A], VRAM[A+1], ... Writes do not touch the prefetch buffer.

## 7. CGRAM port

- $2121 CGADD: word address (color index); write resets the byte toggle for both $2122 and $213B.
- $2122 CGDATA (write twice): 1st write latches low byte; 2nd write stores word `latch | (value<<8)` to CGRAM[CGADD] and increments CGADD.
- $213B CGDATAREAD (read twice): 1st read = low byte, 2nd read = high byte (bit7 = PPU2 open bus) then CGADD increments.

## 8. OAM and sprites

### Addressing / port
- $2102 OAMADDL + $2103 OAMADDH (bit0 = table select, bit7 = priority rotation enable). Write to either reloads internal byte address = `(OAMADD & $1FF) << 1`.
- $2104 OAMDATA write: addr < $200 and even → latch byte only; addr < $200 and odd → write word {latch, value} to addr-1..addr; addr ≥ $200 → write byte directly. Address always increments by 1 per access.
- $2138 OAMDATAREAD: returns byte at internal address (no latch involvement), then increments.
- Table 2 region $200-$3FF mirrors its 32 bytes (addr & $21F).
- Internal address is reloaded from OAMADD at the start of V-blank (unless in forced blank). During active display, accesses hit whatever address sprite evaluation is using (corruption).

### Table 1 entry (4 bytes/sprite, sprites 0-127)
```
byte 0: X position low 8 bits (9-bit signed with table-2 X bit; range -256..255)
byte 1: Y position
byte 2: tile number low 8 bits
byte 3: vhoo pppN — bit7 V-flip, bit6 H-flip, bits5-4 priority 0-3, bits3-1 palette 0-7, bit0 tile bit 8 (name table select)
```
### Table 2 (32 bytes): 2 bits/sprite, 4 sprites/byte, sprite N uses bits (N&3)*2: bit0 = X bit 8, bit1 = size select (0=small, 1=large).

### $2101 OBSEL
```
bit7-5 size mode | bits4-3 name select NN | bits2-0 name base
tile base word address      = bbb << 13
tiles $100-$1FF word addr   = base + ((NN+1) << 12)
```
| Mode | Small/Large | Mode | Small/Large |
|---|---|---|---|
| 0 | 8×8 / 16×16 | 4 | 16×16 / 64×64 |
| 1 | 8×8 / 32×32 | 5 | 32×32 / 64×64 |
| 2 | 8×8 / 64×64 | 6 | 16×32 / 32×64 (undoc) |
| 3 | 16×16 / 32×32 | 7 | 16×32 / 32×32 (undoc) |

### Priority and limits
- Sprite-vs-sprite: lower index (in rotation order) always in front, regardless of OBJ priority bits; priority bits 0-3 only place sprites relative to BGs.
- Priority rotation: if $2103 bit7 = 1, FirstSprite = (OAMADDL & $FE) >> 1 (range 0-127; OAMADDH table-select bit not used); index order wraps 0-127 from there. Otherwise FirstSprite = 0.
- Range: first 32 sprites (index order from FirstSprite) intersecting the scanline; 33rd sets $213E bit6 (range over).
- Time: the in-range sprites are processed in REVERSE order for tile fetch; max 34 8×1 slivers; overflow sets $213E bit7 (time over) and drops the remaining (lower-index) sprites. Flags cleared at end of V-blank.
- X=-256 ($100 as 9-bit) sprites are offscreen but still consume range/time (hardware bug). Sprites never wrap horizontally; Y wraps mod 256.

## 9. Compositing priority, all modes (front → back)

`Sn` = OBJ priority n, `nH/nL` = BG n tile-priority 1/0. Backdrop (CGRAM color 0 on main, COLDATA fixed color on sub) is behind everything.

| Mode | Order (front → back) |
|---|---|
| 0 | S3 1H 2H S2 1L 2L S1 3H 4H S0 3L 4L |
| 1 (BGMODE bit3=0) | S3 1H 2H S2 1L 2L S1 3H S0 3L |
| 1 (BGMODE bit3=1) | **3H** S3 1H 2H S2 1L 2L S1 S0 3L |
| 2, 3, 4, 5 | S3 1H S2 2H S1 1L S0 2L |
| 6 | S3 1H S2 S1 1L S0 |
| 7 | S3 S2 S1 1 S0  (BG1 has no priority bit) |
| 7 + EXTBG | S3 S2 2H S1 1 S0 2L  (BG2 priority = pixel bit 7) |

## 10. Windows

Two 1-D windows, inclusive ranges: $2126 WH0 = W1 left, $2127 WH1 = W1 right, $2128 WH2 = W2 left, $2129 WH3 = W2 right. left > right ⇒ empty window.

Per-layer enable/invert (2 bits each: enable, invert; invert = area outside range counts as inside):
| Reg | bit1-0 | bit3-2 | bit5-4 | bit7-6 |
|---|---|---|---|---|
| $2123 W12SEL | BG1 W1 inv/en | BG1 W2 inv/en | BG2 W1 | BG2 W2 |
| $2124 W34SEL | BG3 W1 | BG3 W2 | BG4 W1 | BG4 W2 |
| $2125 WOBJSEL | OBJ W1 | OBJ W2 | Color W1 | Color W2 |
(bit order within each pair: low bit = invert, high bit = enable.)

Combine logic when both windows enabled for a layer (0=OR, 1=AND, 2=XOR, 3=XNOR):
$212A WBGLOG: bits1-0 BG1, 3-2 BG2, 5-4 BG3, 7-6 BG4. $212B WOBJLOG: bits1-0 OBJ, 3-2 Color. If only one window enabled, its (possibly inverted) area is used; if none, layer window = "never inside".

Layer enables: $212C TM (main screen) / $212D TS (subscreen): bit4 OBJ, bits3-0 BG4..BG1. $212E TMW / $212F TSW: same bit layout; 1 = window masking active for that layer on main/sub (inside window ⇒ layer pixel removed).

## 11. Color math

### $2130 CGWSEL
```
bits7-6 = force main screen black:      0=never, 1=outside color window, 2=inside color window, 3=always
bits5-4 = prevent color math:           0=never, 1=outside color window, 2=inside color window, 3=always
bit1    = addend: 0 = COLDATA fixed color, 1 = subscreen pixel
bit0    = direct color mode (8bpp BGs: modes 3/4 BG1, mode 7)
```
### $2131 CGADSUB
```
bit7 = 0 add / 1 subtract; bit6 = half; bit5 = backdrop math enable
bit4 = OBJ (palettes 4-7 only — OBJ palettes 0-3 never participate); bits3-0 = BG4..BG1
```
### $2132 COLDATA
bit7 = apply to blue, bit6 = green, bit5 = red, bits4-0 = 5-bit intensity (write channels selectively; fixed color also serves as subscreen backdrop).

### Pipeline (per main-screen pixel)
1. Pick topmost main-screen pixel (window-masked layers removed) and topmost subscreen pixel.
2. If inside "force black" region (CGWSEL 7-6): main color := 0 (math can still apply).
3. Math applies iff the main pixel's layer is enabled in CGADSUB, and not in the "prevent" region (CGWSEL 5-4). Addend = subscreen pixel if bit1=1 (if subscreen is transparent/backdrop, addend falls back to fixed color), else fixed color.
4. Per 5-bit channel: add saturates at 31; subtract clamps at 0; **half is applied after the clamp** (add+half = true average since 6-bit sum is halved). Half is suppressed when the main pixel was force-blacked, or when bit1=1 and the subscreen pixel was transparent.
5. Brightness (INIDISP) scales the final color.

Direct color (CGWSEL bit0, 8bpp pixel `BBGGGRRR`, tilemap palette bits `bgr`): R4-0 = RRRr0, G4-0 = GGGg0, B4-0 = BBb00 (mode 7: b=g=r=0).

Hires (modes 5/6, or pseudo-hires SETINI bit3): 512 half-dots. Column order is disputed: the nesdev wiki states the main screen appears on even (left) half-dots and the subscreen on odd (right); anomie/bsnes/Mesen implement the opposite (sub left, main right). fullsnes is silent.

## 12. Mosaic — $2106
bits7-4 = size (0=1×1 ... 15=16×16), bits3-0 = enable BG4..BG1. Pixel of each N×N block = its top-left pixel. Block grid restarts at the top of the frame; on a mid-frame size change the hardware first finishes the current block using the old vertical size, then applies the new size (vertical mosaic = subtract the vertical index within the current block from BGnVOFS). Applies to Mode 7 BG1 via bit0.

## 13. Mode 7

VRAM word interleave: low bytes = 128×128 tilemap (tile numbers 0-255), high bytes = 8×8 char data (256-color, linear 1 byte/pixel). Pixel at playfield (vx, vy), 0-1023:
`tile = VRAM_low[(vy>>3)*128 + (vx>>3)]; color = VRAM_high[tile*64 + (vy&7)*8 + (vx&7)]`

### $211A M7SEL
bits7-6 screen over: 0/1 = wrap (playfield repeats), 2 = outside 1024×1024 transparent, 3 = outside filled with tile $00. bit1 = V-flip, bit0 = H-flip (flip whole 256×224 screen: x→255-x, y→255-y before transform).

### Matrix/center $211B-$2120 (all write-twice, low byte first, shared single `mode7_latch`; also used by M7HOFS/M7VOFS)
| Reg | Name | Format |
|---|---|---|
| $211B/$211C/$211D/$211E | M7A/M7B/M7C/M7D | 16-bit signed, 8.8 fixed point |
| $211F/$2120 | M7X/M7Y | 13-bit signed pixel center |

### Per-pixel transform (anomie/fullsnes-verified, with hardware truncations)
```
CLIP(a) = (a & $2000) ? (a | ~$3FF) : (a & $3FF)        // 13-bit signed → ±1023 quirk
X[0,y] = ((M7A*CLIP(M7HOFS-M7X)) & ~63) + ((M7B*y) & ~63) + ((M7B*CLIP(M7VOFS-M7Y)) & ~63) + (M7X<<8)
Y[0,y] = ((M7C*CLIP(M7HOFS-M7X)) & ~63) + ((M7D*y) & ~63) + ((M7D*CLIP(M7VOFS-M7Y)) & ~63) + (M7Y<<8)
X[x,y] = X[x-1,y] + M7A         Y[x,y] = Y[x-1,y] + M7C
playfield pixel = (X>>8, Y>>8)   // then screen-over handling per M7SEL
```
y = screen line (flip applied), x = screen column (flip applied). Products are truncated to multiples of 64 (&~63) before summing — visible as Mode 7 jitter.

### Multiply $2134-$2136 (MPY, read-only, 24-bit signed)
MPY = M7A (16-bit signed) × most recent byte written to $211C (8-bit signed). $2134 low / $2135 mid / $2136 high. Valid immediately after writes, but garbage while Mode 7 is actively rendering (PPU uses the multiplier).

### EXTBG (SETINI bit6)
BG2 reuses the Mode 7 pixel; bit 7 of the 8-bit pixel = BG2 priority, low 7 bits = color (see §9). BG1 unchanged (full 8 bits).

## 14. Latches & status

- $2137 SLHV (read): if $4201 (WRIO) bit7 = 1, latches H and V counters into OPHCT/OPVCT and sets the latch flag; returns CPU open bus. A 1→0 transition on $4201 bit7 also latches.
- $213C OPHCT / $213D OPVCT: read twice each (independent low/high flip-flops): 1st read = bits 7-0, 2nd read = bit 8 (bits 7-1 = PPU2 open bus). H range 0-339; V range 0-261 NTSC / 0-311 PAL (+1 line in interlace odd frames).
- $213E STAT77 (PPU1): bit7 = OBJ time over (>34 slivers), bit6 = OBJ range over (>32 sprites), bit5 = master/slave, bit4 = PPU1 open bus, bits3-0 = PPU1 version (=1).
- $213F STAT78 (PPU2): bit7 = interlace field (toggles per frame), bit6 = counter latch flag, bit5 = PPU2 open bus, bit4 = 0 NTSC / 1 PAL, bits3-0 = PPU2 version (1-3). Reading resets both OPHCT/OPVCT flip-flops and clears the latch flag. All three latch triggers ($2137 read, $4201 bit7 1→0 transition, lightgun pin) work only if $4201 bit7 is (or was) set.

## 15. $2100 INIDISP / $2133 SETINI

$2100 INIDISP: bit7 = forced blank (screen black, VRAM/OAM/CGRAM freely accessible, rendering halted; NMI/V-blank timing unaffected); bits3-0 = brightness 0-15, output ≈ color × (N+1)/16. Disabling F-blank mid-frame corrupts OAM for that line region.

$2133 SETINI:
```
bit7 = external sync   bit6 = EXTBG (Mode 7 BG2)   bit3 = pseudo-hires (512 via subscreen)
bit2 = overscan: 0 = 224 display lines, V-blank starts line 225; 1 = 239 lines, V-blank starts line 240
bit1 = OBJ interlace (halves OBJ height sampling in interlace)   bit0 = screen interlace (modes 5/6 → 448 lines)
```
