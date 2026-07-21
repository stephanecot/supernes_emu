# supernes_emu

A Super Nintendo (SNES / Super Famicom) emulator written from scratch in Rust — CPU, PPU, and a full audio path, no platform SDKs beyond a pure-Rust window/input/audio stack.

![SMAS select menu](docs/menu_m6.png)

*Super Mario All-Stars "SELECT GAME" menu, rendered by the emulator (background layers + sprites + color-math subscreen compositing).*

## Status

Playable rendering and audio for base-console (no cartridge coprocessors) LoROM/HiROM games, NTSC and PAL.

| Area | State |
|---|---|
| 65C816 CPU | Full instruction set, emulation/native modes, BCD, interrupts |
| SPC700 + IPL | Complete; runs games' real sound drivers |
| S-DSP audio | BRR, Gaussian interpolation, ADSR/GAIN, noise, pitch modulation, echo |
| PPU | BG modes 0–6 (2/4/8bpp), sprites, windows, color math, mosaic, HDMA |
| Mode 7 | Code-complete + unit-tested, not yet validated on an in-game screen |
| DMA | GDMA + HDMA (indirect, per-line) |
| Cartridge | LoROM / HiROM detection, SRAM, region detection |
| Frontend | winit + pixels window, cpal audio, headless mode with PNG/WAV/trace dumps |

183 core unit tests pass. Verified end-to-end on commercial games (backgrounds, sprites, color-math menus, and real in-game music confirmed by WAV analysis).

Not yet done: on-disk SRAM persistence, cycle-accurate FastROM timing, H/V IRQ edge cases, a broad compatibility pass, and an in-game Mode 7 gate. See `docs/PUNCHLIST.md`.

## Build & run

Requires a recent stable Rust toolchain.

```sh
cargo build --release
cargo run --release -p snes-frontend -- path/to/game.sfc   # or .smc / .zip
```

Controls:

| SNES | Key |
|---|---|
| D-pad | Arrow keys |
| B / A / Y / X | Z / X / A / S |
| L / R | Q / W |
| Start / Select | Enter / Right-Shift |

Emulator hotkeys: `P` pause, `N` frame-advance (while paused), `Esc` quit.

### Headless / debugging

```sh
cargo run --release -p snes-frontend -- game.sfc --info                 # header, mapping, region
cargo run --release -p snes-frontend -- game.sfc --headless --frames 600 --dump-frame out.png
cargo run --release -p snes-frontend -- game.sfc --headless --frames 1500 --dump-audio out.wav
cargo run --release -p snes-frontend -- game.sfc --disasm                # disassemble from reset vector
cargo run --release -p snes-frontend -- game.sfc --trace t.log --trace-start-frame 0 --trace-end-frame 2
```

Trace output is Mesen2-compatible for diffing against a reference emulator.

## Layout

- `core/` — `snes-core`, the pure emulation library (no I/O), fully testable headless.
  - `cpu/`, `ppu/`, `apu/`, `bus.rs`, `scheduler.rs`, `dma.rs`, `cartridge/`, `debug/`
- `frontend/` — `snes-frontend`, the winit/pixels/cpal binary and CLI.
- `docs/` — architecture, punch-list of known accuracy gaps.
- `.claude/` — development tooling: subagent definitions and a condensed, source-verified SNES hardware reference (`skills/snes-refs/references/`).

## ROMs

No game ROMs are included — they are copyrighted. Supply your own `.sfc`/`.smc`/`.zip` dumps of games you own. `roms/` is git-ignored.

## License

No license granted yet; all rights reserved by the author pending a choice of open-source license.
