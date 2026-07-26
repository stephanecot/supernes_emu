# Prisme

A Super Nintendo (SNES / Super Famicom) emulator written from scratch in Rust — CPU, PPU, a full audio path, four cartridge coprocessors, a library shell in French and English, and a JSON control channel that lets an outside program drive the console, with no platform SDKs beyond a pure-Rust window/input/audio stack.

A plain-language walkthrough of how the whole thing works — from the 65C816 to the cartridge
coprocessors, the timing bug that silenced Terranigma, the application around the emulator and the
two experiments that were dropped — is in
[`docs/emulateur-snes-explique.pdf`](docs/emulateur-snes-explique.pdf) (French, 26 pages; the
`.html` source next to it is what generates it, via WeasyPrint).

![SMAS select menu](docs/screenshots/smas_menu.png)

*Super Mario All-Stars "SELECT GAME" menu, rendered by the emulator (background layers + sprites + color-math subscreen compositing).*

## Status

Version **1.4.0**. Playable rendering and audio for LoROM/HiROM games (NTSC and PAL), plus all four
implemented cartridge coprocessors.

| Area | State |
|---|---|
| 65C816 CPU | Full instruction set, emulation/native modes, BCD, interrupts, native-stack ops |
| SPC700 + IPL | Complete; runs games' real sound drivers |
| S-DSP audio | BRR, Gaussian interpolation, ADSR/GAIN, noise, pitch modulation, echo |
| PPU | BG modes 0–7 (2/4/8bpp), sprites, windows, color math, mosaic, HDMA, offset-per-tile, hires (5/6), interlace |
| Mode 7 | Rotation/scaling; exercised in-game by Super Mario Kart's DSP-1 track (per-scanline matrix via HDMA) |
| Cartridge coprocessors | SuperFX (GSU) and SA-1 as full CPU cores; DSP-1 and CX4 as HLE command sets — all four boot & render their real games |
| DMA | GDMA + HDMA (indirect, per-line) |
| Timing / IRQ | NMI, H/V IRQ ($4207–$420A), FastROM ($420D), open-bus (MDR) |
| Cartridge | LoROM / HiROM mapping and coprocessor detection from the internal header, region detection, battery SRAM |
| Frontend | winit + pixels window (resizable, fullscreen, window-size/filter/aspect presets), cpal audio, native macOS menu bar, ROM picker, save states, FPS overlay, headless PNG/WAV/trace dumps |
| Shell | Game library with search, sort, favourites and tabbed per-game sheets; games addable from anywhere by button or drag-and-drop; drawn SNES pad that doubles as a controller tester; French and English interface |
| Library metadata | CRC32 fingerprint → No-Intro canonical name → catalogue facts, box art and an attributed Wikipedia summary; opt-in, cached, no account or API key ([`docs/METADATA.md`](docs/METADATA.md)) |
| Agent channel | `--agent`: one JSON object per line on stdin/stdout — step, press, screenshot, read/write memory, save/load state, cheats |
| Cheats | Found by memory search, not entered as codes; stored per game in `<game>.cheats.json` ([`docs/CHEATS.md`](docs/CHEATS.md)) |
| Assistant | Optional — drives the agent channel through a locally installed `claude` CLI; absent tool disables the feature with a reason ([`docs/ASSISTANT.md`](docs/ASSISTANT.md)) |

**758 tests pass**: 336 in `snes-core` (`cargo test -p snes-core --lib`) and 422 in the frontend
(`cargo test -p prisme`; 6 further tests are `#[ignore]`d). Verified end-to-end on eight commercial
games: backgrounds, sprites, color-math menus, real in-game music (WAV-analysed), input-driven
gameplay (Mario runs, jumps and scrolls a level), H/V-IRQ raster splits, battery saves,
byte-identical save-state round-trips, and all four cartridge coprocessors booting and rendering
their real games — Yoshi's Island (SuperFX), Super Mario RPG (SA-1), Super Mario Kart (DSP-1) and
Mega Man X3 (CX4).

<p>
  <img src="docs/screenshots/smrpg_sa1.png" width="47%" alt="Super Mario RPG (SA-1)">
  <img src="docs/screenshots/mmx3_cx4.png" width="47%" alt="Mega Man X3 (CX4)">
</p>
<p>
  <img src="docs/screenshots/yoshi_superfx.png" width="47%" alt="Yoshi's Island (SuperFX)">
  <img src="docs/screenshots/smk_dsp1.png" width="47%" alt="Super Mario Kart (DSP-1)">
</p>

All four coprocessors are validated in real gameplay: Yoshi's Island (SuperFX) boots and renders,
Super Mario RPG (SA-1) reaches forest-field gameplay and the beanstalk cutscene, Mega Man X3 (CX4)
reaches the title (with the CX4
scale/rotate logo animation) and opening-stage gameplay, and Super Mario Kart (DSP-1) renders a
live Mode 7 race — its perspective-projection math, the highest-risk part, works on a real track.

Known gaps: the Super Mario World *attract-mode intro* reaches gameplay but its cutscene state
machine doesn't advance to the overworld (diagnosed, root cause not yet isolated); Secret of Mana's
name-entry screen draws garbled characters (reported, not yet investigated); offset-per-tile, hires
and interlace are implemented and unit-tested but not yet gated on a real in-game screen; and an
SA-1 cartridge's battery data lives in the SA-1's own BW-RAM, which the `.srm` sidecar does not
carry — such a save survives in a save state but not across sessions (`frontend/src/save.rs`
persists `cart.sram` only, which SA-1 carts leave unused). See
[`docs/PUNCHLIST.md`](docs/PUNCHLIST.md) for the full list — including the accuracy items that are
knowingly deferred — and [`docs/IDEAS.md`](docs/IDEAS.md) / [`docs/ROADMAP.md`](docs/ROADMAP.md)
for planned and abandoned features.

## Build & run

Requires a recent stable Rust toolchain.

```sh
cargo build --release
cargo run --release -p prisme -- path/to/game.sfc   # or .smc / .zip
cargo run --release -p prisme                        # no path: opens a ROM picker
```

Launching without a ROM path (and without `--headless`) opens a native
file-open dialog filtered to `.sfc`/`.smc`/`.zip`, starting in `roms/` if that
directory exists. Cancelling the dialog exits cleanly. `--headless` still
requires an explicit ROM path (there is no window to attach a dialog to).

Controls:

| SNES | Key |
|---|---|
| D-pad | Arrow keys |
| B / A / Y / X | Z / X / A / S |
| L / R | Q / W |
| Start / Select | Enter / Right-Shift |

Emulator hotkeys (all platforms — these do not depend on the macOS-only menu
bar below): `P` pause, `N` frame-advance (while paused), `O` open a different
ROM (native file dialog; saves the current game's SRAM first, cancelling
keeps the current game running), `F5` save state, `F9` load state, `F7` next
save-state slot, `0`-`9` jump straight to that slot, `F6` reset (power-on
reset, keeps battery SRAM), `F8` export the current music as `.spc`, `F10`
toggle instant-resume-on-launch, `[`/`]` step the fast-forward factor (2/3/4×,
held with `Tab`), `F` toggle the FPS overlay, `M` mute, `+`/`-` volume, `F12`
screenshot, `F1`-`F4` set the window size (512×448, 768×672, 1024×896,
1280×1120; the native 256×224 is offered in the settings screen only, and the
size a fresh install opens at is the largest of these that fits the monitor),
`V` cycle the display
filter (None / Smooth / CRT), `R` toggle the pixel-aspect-ratio mode
(pixel-perfect / authentic TV), `F11` toggle fullscreen (also Ctrl+Cmd+F on
macOS, that platform's own convention), `C` toggle the quit confirmation,
`Esc` exits fullscreen first if active, otherwise quits (asks for
confirmation unless disabled with `C`).

**Display: zoom, filter, aspect ratio (Phase 2).** The window is freely
resizable by dragging an edge/corner — `F1`-`F4` are convenience shortcuts
that jump straight to a given size, not the only way to resize. At any window
size the emulated picture is scaled without deformation: black letterbox/
pillarbox bars fill whatever the aspect ratio doesn't cover.
- **Aspect** (`R`, settings > Display): `Pixel-perfect (1:1)` snaps to the
  largest *whole-number* zoom that fits the window, so pixels stay perfectly
  sharp under the `None` filter; `Authentic TV (8:7)` stretches the
  picture to the SNES's actual non-square-pixel geometry (each native pixel
  is 8:7 wide:tall — a period CRT stretched this into the ~4:3 picture
  people remember) and fills the window continuously, not snapped to a whole
  factor.
- **Filter** (`V`, settings > Display), independent of size/aspect: `None`
  (nearest-neighbor, sharp — the default), `Smooth` (bilinear), `CRT`
  (bilinear plus darkened alternating scanlines, for an "old TV" look).
  Implemented as a CPU post-processing pass over the RGBA output buffer
  (`frontend/src/render.rs`) rather than a custom wgpu shader — `pixels`
  0.15's built-in scaling renderer hard-codes nearest-neighbor sampling with
  no public option to switch it, and a full custom-shader integration was
  judged more surface than three filters need. Measured cost (Apple Silicon,
  `--release`): ~4ms/frame for the most expensive combination (CRT, zoom x4)
  at typical window sizes, well inside a 50-60fps budget; a window
  maximized on a 4K display with `CRT`/`Smooth` can exceed that budget (the
  default `None` stays cheap at any size) — see
  `frontend/src/render.rs`'s `compose_frame_cost_*` tests for the numbers.
- **Fullscreen** (`F11` / Ctrl+Cmd+F on macOS): borderless fullscreen on the
  window's current monitor, same no-deformation scaling as windowed. Not
  persisted — the app always starts windowed. `Esc` exits fullscreen before
  it does anything else.
- All three (zoom, filter, ratio) are memorized in the preferences file;
  fullscreen is not.

The FPS overlay (off by default) draws the measured display frame rate in the
top-right corner, e.g. `FPS 60/50` (frames actually presented per
wall-second, averaged over a rolling ~0.5s window, versus the cartridge
region's native field rate — 60.0988 Hz NTSC / 50.007 Hz PAL). The number is
green while the emulator keeps up with the target rate and red if it falls
behind. It's drawn directly onto the presented `pixels` frame buffer (a tiny
built-in 3x5 bitmap font, no font asset) — and drawn *after* zoom/filter/PAR
scaling, at a fixed 1 output pixel per glyph pixel, so its on-screen size
stays small and constant regardless of window size instead of growing with
zoom/fullscreen. It never touches the core's own framebuffer, so it never
appears in `--dump-frame`/`--dump-frame-every` PNGs, the F12 screenshot, or
any other headless output. The transient bottom-left status messages (slot
saved, screenshot taken…) follow the same after-scaling, constant-size rule.

**The shell: library, game sheets, settings.** Launched without a ROM, or with
`Esc` from a running game, the application opens on its own screens rather than
on a bare window.

- **Library.** A grid of uniform cards — the picture is letterboxed inside a
  fixed 256:224 box and the title is a two-row elided galley, so no title
  length, picture ratio or missing thumbnail can change a card's size. Search,
  sort, favourites, and three tabs (all games / favourites / recently played).
  The grid never scrolls sideways: the column count comes from the available
  width *minus* the scroll bar, and the cards then shrink to fit it exactly.
  Thumbnails are generated by actually emulating each game for a few hundred
  frames, on a background thread, and cached.
- **Adding a game from anywhere.** The library scans one folder, but games are
  not confined to it: `Add a game…` picks one wherever it lives, and dropping a
  `.sfc`/`.smc`/`.zip` on the window does the same. A dropped file is *added*,
  never started, so it cannot interrupt a running game. The file is only
  referenced — nothing is copied or moved. A game added this way whose file
  later moves stays listed as `File not found`, with the choice of relocating
  or forgetting it; forgetting never deletes the file.
- **Game sheet.** Everything already known about one game, in four tabs — what
  the game *is* (cartridge header: title, region, mapping, sizes, battery SRAM,
  detected coprocessor; plus the catalogue facts and description if fetched),
  where you are (save states *with a picture of what each one holds*), what you
  can change (an assistant request, then the cheats it found), and what you did
  with it (your own screenshots). Clicking a screenshot promotes it as the
  game's thumbnail; the generated one is kept, so the choice is reversible. The
  sheet of the game *currently running* shows that session's picture in place of
  its thumbnail.
- **Controls.** The bindings table sits beside a **drawn SNES pad** that is not
  decoration: hovering either one highlights the other, clicking a button on
  the drawing starts rebinding it, the shape being rebound is ringed, and
  **buttons physically pressed light up** — which makes the screen a controller
  tester, the fastest way to find out that a pad is half dead.

**Language.** The interface speaks French and English, chosen at the top of the
settings screen; the default follows the host's language. The change applies on
the next frame — no restart. The two languages are declared side by side in
`frontend/src/i18n.rs` and rendered through an exhaustive match, so a string
added without its translation does not compile; a catalogue file would have let
the omission through to the screen instead. Keyboard key names (`Arrow Up`,
`Right Shift`) stay English in both, like the pad's own letters: a key is named
by what is engraved on it. Command-line help and diagnostic output are English
only — that audience reads it, and the wording is an anchor for scripts.

**Library metadata (opt-in, no account).** A game is identified by the **CRC32
of its de-headered ROM**, not by its filename: the checksum resolves to a
No-Intro canonical name *with certainty*, and every catalogue lookup keys off
that same checksum. From there the sheet can carry genre, developer, publisher,
player count, year and age rating, the official box art, and an English
Wikipedia summary — the one step matched by *title*, and therefore visibly
attributed and marked as English-only. Nothing is fetched at scan time or at
startup: two explicit buttons trigger it, catalogue files are downloaded once
and then read offline for the whole collection, and a network failure leaves the
sheet exactly as it was. Design and the measured per-source coverage:
[`docs/METADATA.md`](docs/METADATA.md).

**Agent channel, cheats, assistant.** `--agent` runs the emulator as a
line-oriented JSON server (one request object per line on stdin, one response
per line on stdout): `step`, `press`, `screenshot`, `read-mem`, `write-mem`,
`save-state`, `load-state`, `info`, and the `cheat-add` / `cheat-list` /
`cheat-enable` / `cheat-remove` family. Observation costs no frames — only
`step` and `press` advance the console — which is what makes the memory search
reproducible. On top of that channel, cheats are *found* rather than entered:
successive intersection over the 128 KB of WRAM with an arithmetic predicate,
replayed from a save state, narrowing 131 072 candidates to a handful in three
to five rounds, then settled by writing to each survivor and looking at the
screen. The method, its traps and a worked end-to-end example are in
[`docs/CHEATS.md`](docs/CHEATS.md). Results land in `<game>.cheats.json` beside
the save, and the windowed application applies and lists them. Driving that
search from the UI needs a `claude` CLI on the host; when it is absent the
feature is disabled with a stated reason rather than failing on click
([`docs/ASSISTANT.md`](docs/ASSISTANT.md)).

On macOS the windowed build also installs a native menu bar (top of screen).
Every item there also has a plain-keyboard equivalent (listed above) that
works on every platform, including Windows/Linux where this menu doesn't
exist:

| Menu | Item | Shortcut | Also reachable via |
|---|---|---|---|
| Prisme | Settings… | Cmd+, | `,` |
| Prisme | Quit | Cmd+Q | `Esc` |
| File | Home | — | `Esc` |
| File | Open a ROM… | Cmd+O | `O` |
| File | Screenshot | — | `F12` |
| File | Export the music (.spc)… | — | `F8` |
| Emulation | Pause / Resume | Cmd+P | `P` |
| Emulation | Reset | Cmd+R | `F6` |
| Emulation | Save state | Cmd+S | `F5` |
| Emulation | Load state | Cmd+L | `F9` |
| Emulation | Next slot | — | `F7` |
| Display | Full screen | Ctrl+Cmd+F | `F11` |

The menu deliberately carries **actions only**. Every *setting* — window size,
filter, aspect, volume, mute, FPS overlay, fast-forward factor, save slot,
instant resume, quit confirmation — lives on the settings screen, where the
current value is visible at a glance instead of being buried behind a
submenu's checkmark. The keyboard shortcuts above still reach all of them.

Keyboard hotkeys keep working alongside the menu; the checkable items
(mute, confirm-on-quit, show FPS, resume-on-launch, the slot,
fast-forward-factor, zoom, filter and ratio radio groups) stay in sync
whichever path is used.

Save states snapshot the whole console (CPU/PPU/APU/DSP/DMA and all RAM) to a
`.state` sidecar next to the ROM (e.g. `game.sfc` -> `game.state`; for a
`.zip`, next to the zip using its base name). `F5`/Cmd+S writes it; `F9`/Cmd+L
restores it. The blob stores no ROM image (the running ROM is reattached on
load) and carries the ROM's checksum, so loading a state saved from a different
game is rejected and the running game is left untouched. Any load error is
printed and emulation continues. Each write also drops the framebuffer of that
exact moment next to the state, as `<state>.png` (raw 256×224 RGBA, the same
picture `--dump-frame` writes): `game.state3` -> `game.state3.png`, and the same
for the instant-resume file `game.resume`. The game sheet shows it as the slot's
preview. The picture is optional — a state written before this existed, or one
whose picture could not be written, still loads — and deleting a slot from the
sheet deletes its picture with it.

Battery-backed cartridges save to a `.srm` sidecar next to the ROM (e.g.
`game.sfc` -> `game.srm`; for a `.zip`, next to the zip using its base name).
The save loads on startup and is written back on exit — including when you quit
via Cmd+Q or the Quit menu item, not only via `Esc`/window-close — but only if
SRAM contents actually changed (an untouched save is never rewritten). Override
the path with `--save PATH`.

### Headless / debugging

```sh
cargo run --release -p prisme -- game.sfc --info                 # header, mapping, region
cargo run --release -p prisme -- game.sfc --headless --frames 600 --dump-frame out.png
cargo run --release -p prisme -- game.sfc --headless --frames 1500 --dump-audio out.wav
cargo run --release -p prisme -- game.sfc --headless --frames 900 --dump-state statedir/  # WRAM/VRAM/CGRAM/OAM
cargo run --release -p prisme -- game.sfc --disasm                # 65C816 disassembly from the reset vector
cargo run --release -p prisme -- game.sfc --trace t.log --trace-start-frame 0 --trace-end-frame 2      # 65C816
cargo run --release -p prisme -- game.sfc --trace-spc s.log --trace-start-frame 0 --trace-end-frame 2  # SPC700
cargo run --release -p prisme -- superfx.sfc --trace-gsu g.log --trace-start-frame 0 --trace-end-frame 2  # SuperFX GSU
cargo run --release -p prisme -- sa1.sfc --trace-sa1 a.log --trace-start-frame 0 --trace-end-frame 2     # SA-1 65C816
cargo run --release -p prisme -- game.sfc --headless --frames 300 --script inputs.txt  # scripted joypad
cargo run --release -p prisme -- game.sfc --headless --frames 900 --save-state-at 900 out.state
cargo run --release -p prisme -- game.sfc --load-state out.state --headless --frames 60
cargo run --release -p prisme -- game.sfc --headless --frames 600 --watch 7E:0DBE   # every write to an address, all mirrors
cargo run --release -p prisme -- game.sfc --save /path/to/slot1.srm  # override the default .srm sidecar
cargo run --release -p prisme -- game.sfc --agent            # JSON control channel on stdin/stdout
cargo run --release -p prisme -- --ui-shot settings-display@en out.png   # render a UI screen headless
```

The 65C816 trace is Mesen2-compatible for diffing against a reference emulator; the SPC700, GSU and
SA-1 traces use the same idea for the audio CPU and the two coprocessors that are real CPU cores.
`--ui-shot` renders any interface screen without a window — the mechanism that made the UI
reviewable at all on a headless machine (see [`docs/UI-BRIEF.md`](docs/UI-BRIEF.md)); the `@en` /
`@fr` suffix picks the language, since a screen validated in one language is validated by half.

### macOS app bundle

`scripts/make-app.sh` builds a double-clickable `Prisme.app` (with icon) into `dist/`:

```sh
./scripts/make-app.sh              # release-build, then bundle
INSTALL=1 ./scripts/make-app.sh    # also copy into /Applications
SKIP_BUILD=1 ./scripts/make-app.sh # bundle the existing release binary (no rebuild)
```

Launched from Finder with no arguments, the app opens the ROM picker.

## Layout

- `core/` — `snes-core`, the pure emulation library (no I/O), fully testable headless.
  - `cpu/`, `ppu/`, `apu/`, `bus.rs`, `scheduler.rs`, `dma.rs`, `cartridge/`, `debug/`
  - `coprocessor/` — `superfx/` (GSU core), `sa1/` (second 65C816 + Super MMC, arithmetic unit, I-RAM/BW-RAM), `dsp1/` (HLE command set), `cx4/` (HLE command set)
- `frontend/` — `prisme`, the winit/pixels/cpal binary and CLI (picker, menu bar, save states, FPS overlay, `render.rs` zoom/filter/aspect compositing).
  - `ui/` — the egui shell: `library_view.rs`, `game_sheet.rs`, `settings.rs`, `pad_art.rs` (the drawn pad), `icons.rs`, `theme.rs`, `shot.rs` (`--ui-shot` offscreen rendering).
  - `agent.rs` (JSON control channel), `cheats.rs`, `assistant.rs`, `metadata.rs` + `net.rs` (catalogues, box art), `i18n.rs`, `library.rs`, `thumbs.rs`, `paths.rs`.
  - `frontend/assets/fonts/` — the two typefaces embedded in the binary (`include_bytes!`, see `ui/theme.rs`): **Space Grotesk** Regular/Bold for the interface and **IBM Plex Mono** Regular for machine data (region, mapping, checksum, sizes, key bindings, paths). Both are under the SIL Open Font License 1.1, whose text ships beside them (`SpaceGrotesk-OFL.txt`, `IBMPlexMono-OFL.txt`).
- `scripts/` — `make-app.sh` (macOS `.app` bundler); `packaging/` — app icon assets.
- `docs/` — the pedagogical walkthrough (`emulateur-snes-explique.html` / `.pdf`, French) and the
  notes that record the decisions behind each area:
  | Document | What it records |
  |---|---|
  | `ARCHITECTURE.md` | The original plan, milestones and key technical choices |
  | `ROADMAP.md` | Phase-by-phase status, including the phases that were **abandoned after being tried** |
  | `PUNCHLIST.md` | Known accuracy gaps, and the diagnosed bugs — the Terranigma SPC700 timing story lives here |
  | `IDEAS.md` | The backlog the roadmap was derived from |
  | `UI-BRIEF.md` | The interface redesign brief, and why the UI had to be made visible headless first |
  | `I18N.md` | How the two languages are declared so a missing translation fails to compile |
  | `METADATA.md` | The CRC32 → No-Intro → catalogue chain, with per-source coverage measured |
  | `CHEATS.md` | The memory-search method, written for the agent that runs it |
  | `ASSISTANT.md` | What the assistant may do, what it may not, and what was removed |
  | `REWIND.md` | Measured sizing for a rewind buffer (design only — not implemented) |
- `.claude/` — development tooling: subagent definitions and a condensed, source-verified SNES hardware reference (`skills/snes-refs/references/`).

## ROMs

No game ROMs are included — they are copyrighted. Supply your own `.sfc`/`.smc`/`.zip` dumps of games you own. `roms/` is git-ignored.

## License

No license granted yet; all rights reserved by the author pending a choice of open-source license.

Third-party assets shipped in this repository and embedded in the binary:

| Asset | Author | License |
|---|---|---|
| `frontend/assets/fonts/SpaceGrotesk-{Regular,Bold}.ttf` | © 2020 The Space Grotesk Project Authors | SIL Open Font License 1.1 (`SpaceGrotesk-OFL.txt`) |
| `frontend/assets/fonts/IBMPlexMono-Regular.ttf` | © 2017 IBM Corp., Reserved Font Name "Plex" | SIL Open Font License 1.1 (`IBMPlexMono-OFL.txt`) |

The OFL permits embedding the faces in a program and redistributing them with it; the icon set is
drawn with egui's painter (`frontend/src/ui/icons.rs`), so no icon font is bundled.
