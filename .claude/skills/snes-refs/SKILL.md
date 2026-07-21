---
name: snes-refs
description: Condensed SNES hardware reference for this emulator — MMIO register map, 65C816 & SPC700 cycle rules, PPU priority tables, BRR/DSP formats, PAL/NTSC timing. Single source of truth for hardware constants; read the relevant references/*.md before implementing or reviewing any hardware behavior.
---

# SNES hardware reference — index

**Rule: never guess a hardware constant.** If a value is missing or uncertain here, fetch it upstream — per-topic pages at https://snes.nesdev.org/wiki/ (fetchable, preferred) or the full reference https://problemkaputt.de/fullsnes.htm — then correct this reference so the next agent benefits.

Detailed references (read only the one(s) relevant to your task):

- `references/cpu-65c816.md` — registers, flags, all 24 addressing modes with cycle rules, opcode matrix, interrupt vectors (native/emulation), E-mode quirks, BCD arithmetic, WAI/STP, MVN/MVP.
- `references/mmio.md` — full register map $2100–$21FF, $4016/$4017, $4200–$421F, $4300–$437F with bit fields and R/W behavior; system memory map, mirrors, open bus, access-speed table; cartridge header layout and LoROM/HiROM address decode.
- `references/ppu.md` — VRAM/CGRAM/OAM formats, 2/4/8bpp tile and tilemap formats, per-mode layer & priority ordering tables (incl. Mode 1 BG3-priority quirk), sprite evaluation and per-line limits, windows, color math pipeline, mosaic, Mode 7 math.
- `references/apu.md` — SPC700 ISA with cycle counts, $00F0–$00FF control registers, timers, IPL boot ROM (64 bytes + upload protocol), S-DSP register map, BRR block format and filters, ADSR/GAIN rate tables, Gaussian interpolation, echo.
- `references/timing.md` — master clocks, scanline/frame geometry NTSC vs PAL, NMI/IRQ latch semantics ($4210/$4211), auto-joypad window, DMA/HDMA scheduling points and cycle costs.

## Core constants (used everywhere)

- Master clock: NTSC **21_477_272 Hz**, PAL **21_281_370 Hz**. 1 dot = 4 master cycles; 1 scanline = **1364** master cycles.
- Frame: NTSC **262** lines, ~60.0988 fps; PAL **312** lines, ~50.007 fps. Visible picture 256×224, lines V=1..224 (V=0 is pre-render). NMI fires at start of V=225 (both regions, no overscan).
- CPU memory access cost (master cycles): **6** fast (`$21xx`, `$42xx`, internal/idle, FastROM banks $80+ when $420D bit0=1), **8** slow (WRAM, `$6000-$7FFF`, SlowROM), **12** (`$4000-$41FF` joypad region).
- APU: SPC700 nominal **1_024_000 Hz**; S-DSP outputs 1 stereo sample per **32** SPC cycles = 32_000 Hz.
- 65C816 vectors — native: COP $FFE4, BRK $FFE6, NMI $FFEA, IRQ $FFEE; emulation: COP $FFF4, NMI $FFFA, RESET $FFFC, IRQ/BRK $FFFE (bank 0).
- Reset state: E=1, M=X=1, D=$0000, DBR=PBR=$00, S=$01FF, I=1.
