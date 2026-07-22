---
name: snes-build-test
description: Build, test, lint and run the SNES emulator (Rust workspace) — run a ROM headless N frames, script inputs, produce CPU/SPC traces, MMIO logs and PNG framebuffer dumps. Read before any build, run or debug session on this project.
---

# Build & test

**PATH note (this machine):** `cargo` is not on the login-shell PATH. Use `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$HOME/.cargo/bin:$PATH"` at the start of your shell commands (rustc/cargo 1.93.0).

- `cargo check --workspace` — fast gate; must be error-free before any agent returns.
- `cargo test -p snes-core` — unit tests on pure logic.
- `cargo build --release -p snes-frontend` — release build (use `--release` for any run beyond a few frames; debug builds are ~20× too slow for full-speed emulation).
- `cargo clippy --workspace` — final-pass lint only, not a per-change gate.

# Frontend CLI contract

`cargo run --release -p snes-frontend -- <rom> [flags]`

`<rom>` accepts `.sfc`/`.smc` raw or `.zip` (first ROM entry inside). If omitted and `--headless` is not set, a native file-open dialog (rfd, filtered to `.sfc`/`.smc`/`.zip`, starting in `roms/` if present) is shown instead — not usable from a headless/agent shell; agents must always pass `<rom>` explicitly. `--headless` still requires `<rom>` explicitly (errors otherwise). **This contract is what all agents rely on — if you change a flag, update this file in the same change.**

| Flag | Behavior |
|---|---|
| `--info` | Print parsed header (title, mapping LoROM/HiROM, region, ROM/SRAM size, checksum), then exit |
| `--disasm [--addr BB:AAAA] [--count N]` | Disassemble N instructions (default 30) from address (default: reset vector), then exit |
| `--headless --frames N` | No window, no audio; emulate N frames, then exit 0 |
| `--dump-frame PATH.png` | Write the final framebuffer as PNG on exit (with `--headless`) |
| `--dump-frame-every N --dump-dir DIR` | Write DIR/frame_XXXXX.png every N frames |
| `--trace PATH --trace-start-frame A --trace-end-frame B` | Mesen2-format 65C816 trace for frames A..B (unbounded traces are gigabytes — always bound) |
| `--trace-spc PATH` | SPC700 trace, same frame bounds |
| `--log-mmio` | Log named MMIO writes ($21xx/$42xx/$43xx) to stderr |
| `--watch BB:AAAA` | Log every read/write at a bus address |
| `--script PATH` | Headless input script; each line: `<frame> <button> <frames_held>` with buttons `A B X Y L R Start Select Up Down Left Right` |
| `--dump-state DIR` | On exit dump `wram.bin vram.bin cgram.bin oam.bin apuram.bin` into DIR |
| `--load-state FILE` | Headless: `Snes::load_state` from FILE before emulating frame 0 (rejects a state saved from a different ROM) |
| `--save-state-at FRAME FILE` | Headless: write `Snes::save_state` to FILE right after emulating frame FRAME |

# ROMs (all PAL — the emulator must run them at 50 Hz)

Located in `roms/` (paths contain spaces — always quote):

- `roms/Super Mario All-Stars + Super Mario World (E) [!].zip` — LoROM, 2.5 MB. Reference game for milestones M1–M5.
- `roms/Secret of Mana (F).zip` — HiROM, 2 MB. HDMA/windows/color-math and Mode 7 gates (M6–M7), DSP echo (M8).
- `roms/Secret of Evermore (E) [t1].zip` — HiROM, 3 MB. Compatibility stress test (M9–M10).

# Gate recipes (proof per milestone)

- **M0**: `--info` on both SMAS+SMW and SoM prints correct mapping/region; `--disasm` from reset vector shows plausible init (SEI, CLC, XCE, REP…).
- **M1**: SMAS+SMW `--headless --frames 60 --trace ...` — end of trace shows a tight loop reading `$2140` (APU handshake spin).
- **M2**: same run — trace shows handshake `$AA/$BB` then block upload to APU, CPU proceeds past the spin.
- **M3/M4**: `--headless --frames 600 --dump-frame out.png` — All-Stars select screen visible (M3: backgrounds, M4: + sprites).
- **M5**: `--script` pressing Start/A to select SMW and enter a level; periodic dumps show gameplay responding to input.
- **M8**: add `--dump-audio PATH.wav` if implemented, or listen live; music at correct 50 Hz speed, no crackle over 5 min.

# Output hygiene

Write traces/dumps to `target/debug-out/` (create it), never to the repo root. Traces are huge — always bound frames and delete afterwards.
