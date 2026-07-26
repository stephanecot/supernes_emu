---
name: snes-build-test
description: Build, test, lint and run the SNES emulator (Rust workspace) — run a ROM headless N frames, script inputs, produce CPU/SPC traces, MMIO logs and PNG framebuffer dumps. Read before any build, run or debug session on this project.
---

# Build & test

**PATH note (this machine):** `cargo` is not on the login-shell PATH. Use `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$HOME/.cargo/bin:$PATH"` at the start of your shell commands (rustc/cargo 1.93.0).

- `cargo check --workspace` — fast gate; must be error-free before any agent returns.
- `cargo test -p snes-core` — unit tests on pure logic.
- `cargo build --release -p prisme` — release build (use `--release` for any run beyond a few frames; debug builds are ~20× too slow for full-speed emulation).
- `cargo clippy --workspace` — final-pass lint only, not a per-change gate. **Not installed on this machine** (`error: no such command: clippy`); skip the gate and say so rather than installing it.

# Frontend CLI contract

`cargo run --release -p prisme -- <rom> [flags]`

`<rom>` accepts `.sfc`/`.smc` raw or `.zip` (first ROM entry inside). If omitted and `--headless` is not set, the app opens a window on its **home screen** (egui shell, Phase 8) with no cartridge loaded — not usable from a headless/agent shell; agents must always pass `<rom>` explicitly. The home screen scans a ROM folder (`library_dir` preference, else `last_rom_dir`, else `roms/`) on a background thread and generates a thumbnail per game by emulating it headless; both results are cached under the app's config directory (`…/Prisme/library.json`, `…/Prisme/Thumbnails/*.png`) and can be deleted at any time — they are derived data, never player data. The exception is `--info`/`--disasm` without `<rom>`, which still show a native file-open dialog (rfd, filtered to `.sfc`/`.smc`/`.zip`, starting in `roms/` if present). `--headless` still requires `<rom>` explicitly (errors otherwise). Every user option (display, audio, inputs/remapping, emulation, folders) lives in the windowed **settings view** (`,` hotkey, `Réglages…`/Cmd+, in the macOS menu, `Réglages` tab of the home screen), not in the native menu, which now carries actions only; it is a full-width screen with its own tab bar — Escape (or `Retour`) leaves it for the library tab that was showing, or for the running game — and it reads and writes `prefs.json` exclusively, so a headless run is unaffected by it. **This contract is what all agents rely on — if you change a flag, update this file in the same change.**

| Flag | Behavior |
|---|---|
| `--version` / `-V` | Print `Prisme - SuperNes <version>` and exit (no ROM needed) |
| `--info` | Print parsed header (title, mapping LoROM/HiROM, region, ROM/SRAM size, checksum), then exit |
| `--disasm [--addr BB:AAAA] [--count N]` | Disassemble N instructions (default 30) from address (default: reset vector), then exit |
| `--headless --frames N` | No window, no audio; emulate N frames, then exit 0 |
| `--agent` | JSON control channel on stdin/stdout, one object per line, one response per line, `id` echoed, errors as values (`{"error":"…"}`) — the channel never goes silent and never exits except on `quit` or EOF. Headless by construction, honors `--load-state`, and **never writes the battery SRAM**. Commands: `step`, `press`, `screenshot`, `read-mem`, `write-mem`, `save-state`, `load-state`, `cheat-list`, `cheat-add`, `cheat-remove`, `cheat-enable`, `state`, `ping`, `help`, `quit`. `read-mem`/`write-mem` cap at 4096 bytes and refuse `$2000-$5FFF` of the system banks (MMIO). Unnamed screenshots/states land in `target/debug-out/agent/`. |
| `--dump-frame PATH.png` | Write the final framebuffer as PNG on exit (with `--headless`) |
| `--dump-frame-every N --dump-dir DIR` | Write DIR/frame_XXXXX.png every N frames |
| `--trace PATH --trace-start-frame A --trace-end-frame B` | Mesen2-format 65C816 trace for frames A..B (unbounded traces are gigabytes — always bound) |
| `--trace-spc PATH` | SPC700 trace, same frame bounds |
| `--trace-gsu PATH` | GSU/SuperFX trace, same frame bounds; needs a SuperFX cart (prints a note and skips otherwise) |
| `--log-mmio` | Log named MMIO writes ($21xx/$42xx/$43xx) to stderr |
| `--watch BB:AAAA` | Log every read/write at a bus address |
| `--script PATH` | Headless input script; each line: `<frame> <button> <frames_held>` with buttons `A B X Y L R Start Select Up Down Left Right` |
| `--dump-state DIR` | On exit dump `wram.bin vram.bin cgram.bin oam.bin ppu.txt` into DIR |
| `--dump-spc PATH.spc` | On exit write the APU state as a 66048-byte `.spc` music file (headless only) |
| `--load-state FILE` | Headless: `Snes::load_state` from FILE before emulating frame 0 (rejects a state saved from a different ROM) |
| `--save-state-at FRAME FILE` | Headless: write `Snes::save_state` to FILE right after emulating frame FRAME |
| `--ui-shot VIEW OUT.png` | Render one screen of the interface offscreen and exit. VIEW is `library`, `favorites`, `game-sheet`, `empty`, `library-hover`, or one settings section — `settings-display`, `settings-audio`, `settings-inputs`, `settings-emulation`, `settings-folders`, `settings-about` (`settings` is an alias of `settings-display`); the settings view shows one section at a time, so one view per section is the only way to look at them all. The positional argument is the **output PNG**, not a ROM. Needs no display, no ROM and no `prefs.json`: it draws the application's own `ui::home`/`ui::settings` screen code (one of them owns the window, as in the application) on a fake library (a dozen games, missing thumbnails, long titles, favourites, the four coprocessors, save slots with and without a preview picture) into an offscreen wgpu texture. `library-hover` injects a pointer over one tile, which is the only way the hover state can be looked at. This is how the UI is *looked at* on a machine with no screen. |
| `--ui-shot-size WxH` | Size of that capture in points, `320..=4096` per side (default `1280x800`) |

**Output path handling:** only `--trace`/`--trace-spc`/`--trace-gsu`/`--trace-sa1` auto-root a
relative PATH under `target/debug-out/` (traces can reach gigabytes — this is the output-hygiene
rule below). Every other output flag — `--dump-frame`, `--dump-frame-every`/`--dump-dir`,
`--dump-state`, `--dump-spc`, `--dump-audio`, `--save-state-at` — honors PATH exactly as given,
relative to the current directory; agents should pass an explicit `target/debug-out/...` path for
those themselves if they want the same hygiene. `.srm`/`.state*`/`.resume`/`.cheats.json` sidecars sit **beside the ROM** by default; a windowed run puts them in
the `save_dir` preference's folder instead when the player set one (`Réglages > Dossiers`), named
after the *game* there (`library::game_id` = cartridge title + header checksum, e.g.
`SUPER_MARIOWORLD-A0DA.srm`) so two ROM files of the same name cannot share one save, and still
reading the ROM-file-named file of that folder, then the folder configured before it
(`previous_save_dir`), then the beside-the-ROM file, as fallbacks. `--save` keeps
priority over both for the `.srm`, and a **headless run never reads the preferences file**, so its
paths are exactly `--save` or `<rom>.srm`. A windowed save-state write also drops the framebuffer
beside the state as `<state>.png` (raw 256x224, e.g. `game.state3.png`, `game.resume.png`); it is
optional at load and deleted with its slot. Those writes (battery saves,
save states, prefs) are atomic (temp file + `rename`); a `.srm` whose size doesn't exactly match
the cart's declared SRAM is rejected at load (fresh SRAM is used instead), so don't hand-edit a
`.srm` to a different length when testing.

# Cheats (`cheat-*` on the agent channel)

No Game Genie codes: a cheat is the *result* of a memory search an agent runs
through this channel. The full procedure — intersection method, how many rounds,
how to tell the counter from its display mirror — is `docs/CHEATS.md`; read it
before searching for an address.

```json
{"cmd":"cheat-add","name":"Vies infinies","addr":"7E:0DBE","hex":"63","kind":"freeze","enabled":true}
{"cmd":"cheat-list"}
{"cmd":"cheat-enable","name":"Vies infinies","enabled":false}
{"cmd":"cheat-remove","name":"Vies infinies"}
```

`kind` is `freeze` (rewritten after every emulated frame, the default) or `once`
(written a single time; rearmed by `load-state`). `addr` takes `BB:AAAA` or bare
hex and refuses MMIO; the payload goes in `hex` or `bytes`, 1..=64 bytes. `name`
is the identity — adding the same name replaces that cheat. Every cheat response
carries the whole list, its `count` and the `path` written.

They persist in `<game>.cheats.json`, a sidecar beside the `.srm`/`.state`
(**not** in `prefs.json`: a headless run must never write the player's
preferences). The windowed application reads it when the game loads, applies the
frozen ones after every frame exactly as the channel does, and lists them on the
game sheet where the player can untick or remove one.

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
