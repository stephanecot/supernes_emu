# Punch-list — carried-over accuracy items

Minor findings from adversarial verification + gate notes, deferred to the milestone where they matter. None block current milestones; fold each into the noted phase.

## For M5 (input) — bus.rs / joypad.rs
- `$4212` bit0 (auto-joypad busy) hardwired to 0 — should be set for ~4224 master cycles (~3.1 lines) from auto-read start at vblank.
- Auto-joypad snapshots pads whenever `$4200` bit0=1 regardless of `$4016` strobe — real hardware only auto-reads when OUT0 (strobe) = 0.
- `$4016` bits7-2 / `$4017` bits7-5 should read open-bus (prior MDR); `$4017` bits4-2 always driven; currently returns raw joypad read.
- `$4213` RDIO bits5-0 should loop back `$4201` (WRIO), not CPU open bus.

## For CPU (fix opportunistically — foundational)
- `push8`/`pull8` unconditionally re-impose page-1 wrap; the "new" 65C816 stack ops (PEA/PEI/PER/PHD/PLD/JSL/RTL, stack-relative) must NOT wrap to page 1 in native mode with 16-bit stack. Potential real bug for deep-stack games.
- COP in emulation mode pushes B=1 like BRK; reference documents only BRK pushing B=1 (IRQ/NMI push B=0). Verify COP behavior.
- JSL ($22) operand/push ordering: hardware fetches AAL,AAH, pushes PBR, internal cycle, THEN fetches AAB. Current code fetches all 3 first. Cycle-order only; result correct.

## For M8 (audio) — apu
- SPC CONTROL power-on value should be `$80` (bit7 IPL enable set); currently 0 with `ipl_enabled` tracked separately (harmless now).
- Timer enable 0→1 transition should reset only stage-2 counter + 4-bit TnOUT, not the stage-1 prescaler.
- `Apu::reset()` should restore CONTROL to power-on ($80) too, not only re-vector the SPC.

## For M6/M3 — ppu timing
- STAT78 `$213F` reports fixed PPU2 version 1; does not toggle interlace-field (bit7) or counter-latch (bit6), and reading it does not reset OPHCT/OPVCT flip-flops. Implement with H/V counter latches.
- NTSC short-line (1360 cycles at V=240) and overscan-shifted vblank/NMI line ($2133 bit2 → V=240) not modeled; frame length drifts ~1 dot/frame. Cosmetic for now.

## M3–M5 status (verified) + the color-math dependency
- The BG/OBJ rendering engine is complete and proven: SMAS intro renders the Nintendo logo + gold Mario medallion correctly at frame ~120 (main_screen=0x10, OBJ-only). BG tile decode is unit-tested (2/4/8bpp, flips, scroll, Mode 0 offset).
- The SMAS **outer All-Stars menu** goes black from ~frame 240 NOT because of a rendering bug. Diagnosed: at a black frame, forced_blank=0, brightness=15, bg_mode=3, main_screen=0x02 (BG2), sub_screen=0x11 (BG1+OBJ), VRAM 67% full, OAM full, CGRAM has a gradient. The menu composites **main (BG2) + sub (BG1+OBJ) via color math** ($2131 CGADSUB=0x20, $2130 CGWSEL, $2132 COLDATA all written heavily). Our compositor renders only the main screen → the subscreen graphics are invisible → black.
- CONCLUSION: this screen is gated on **M6 color math + subscreen compositing**, not on M3/M4. The M6 workflow must add subscreen compositing; re-gate the SMAS menu there. If the menu is STILL black after color math lands, then (and only then) suspect BG decode on real HiROM data.

## M6 status (verified PASS) + M7
- M6 color math + subscreen compositing + windows + HDMA + mosaic: DONE and visually verified. SMAS "SELECT GAME" menu renders in full color (was black); Secret of Mana title + layered French intro render correctly (HiROM + HDMA). 169 core tests pass.
- M7 (Mode 7): code-complete in ppu/mode7.rs + 3 unit tests, but NOT gated on a real in-game screen — neither SMAS nor SoM reaches Mode 7 in a headless budget (SoM's Flammie world map is deep in gameplay). Revisit opportunistically: script SMW to a Bowser fight (Mode 7) or a longer SoM run.

## SMW attract-mode intro hang (KNOWN ISSUE, narrowed, unsolved)
Super Mario World reaches gameplay (Mario runs/jumps, camera scrolls — proven) but its attract-mode INTRO cutscene ("Welcome! This is Dinosaur Land… Bowser is at it again!") never advances to the overworld.

Established (not the bug): CPU alive; per-frame NMI-sync flag $00:0010 set/consumed normally; NMI every frame; H/V IRQ taken (vector $00:FFEE, VTIME splits 55/36, NMITIMEN toggles $A1/$81); auto-joypad + input all work. So NMI/IRQ/input are NOT the cause — the intro state machine advances each frame but its completion condition is never met.

Narrowed to: the intro-advance gate depends on WRAM $1426 and $13BF (and $13D2 downstream). At the hang, the dump shows $1426=1 while the ROM's decision logic given ($1426=1, $13BF=0) correctly yields $13D2=0 — so the divergence is UPSTREAM in how $1426/$13BF get set during message setup. The write of $1426=1 comes from a bank not yet watched (00/30/35 were only seen writing them =0). Hot intro-handler addresses: 30:8E0C-8E30, 30:AE4A-AE4E.

Next step for a fresh session (with stable infra): --watch $00:1426 and $00:13BF across ALL banks during the message-setup window, find the instruction that writes $1426=1, and trace back what condition it reflects (likely a mistimed PPU/IRQ/APU event or an open-bus/counter-latch read the message-setup polls). Three automated debug attempts were killed by API/infra errors mid-investigation, not by lack of a lead.

Impact: also blocks the Mode 7 real-screen gate (SMW's Mode 7 is the Bowser fight, behind this intro).

## Secret of Mana — garbled characters on the name-entry screen (USER-REPORTED BUG)
When SoM prompts for the character's name, strange/garbled characters are displayed. Real rendering bug on a base-console game. Likely candidates to check: the name-entry screen's text tiles (BG mode/priority on that screen), a variable-width-font or dynamic-tile-upload path the game uses for the name grid, or a VRAM/DMA timing issue that corrupts the font tiles for that screen specifically. To diagnose: script SoM into a new game to reach name entry, dump the frame + VRAM/CGRAM state, compare the font tiles against what the game uploaded. Not yet investigated.

## Terranigma — silent APU, then a freeze after "Chapitre 1" (USER-REPORTED, FIXED)
One bug, two faces. The S-CPU sat in `86:8F76 LDA $2140 / BNE $8F76` forever — 100 % of 607 954 traced instructions — waiting for a sound driver that had derailed to $0000; and the driver, while it still ran, never wrote the master volume nor grew past 3.2 KB of APU RAM.

**Rule we had wrong:** an SPC700 instruction does its memory access on its **last** cycle. `Apu::run_budget` started an instruction as soon as *one* of its cycles was due and charged the cost afterwards, so every SPC-side effect — writes to the comm ports $F4-$F7 included — became visible to the S-CPU up to `len-1` SPC cycles early: ~62 master cycles for a 4-cycle `MOV $F4,Y`.

Only Terranigma could see it, because it uploads through its own receiver rather than the IPL. The IPL reads the data byte *before* acking (`$FFDD: CMP Y,$F4 / MOV A,$F5 / MOV $F4,$Y`) and so cannot race. Terranigma's receiver at $041D acks **first** (`MOV $F4,Y` then `MOV A,$F5`) and leans on the ~14 master cycles of margin the SPC700 really has; a 62-cycle early ack inverted it, and the S-CPU overwrote $2141 before the SPC had read it. 2 743 of 7 316 transferred bytes were wrong, the sequence pointer eventually became $0000, and the driver walked off into zero page.

**Fix:** `spc700::OPCODE_CYCLES` plus a `run_budget` that refuses to start an instruction until the budget covers its whole cycle window. No fudge factor, no per-game test. The long-term SPC rate is unchanged — only *when inside its window* an instruction publishes its effects moves, from the first cycle to the last, which is where the hardware does its bus access.

**Measured:** Terranigma audio peak 0 → 5038 at 900 frames (13339 over a scripted 5000-frame run), MVOL $00 → $5F, APU RAM 5334 → 60611 bytes, 0 wrong bytes of 2387 transferred. The freeze is gone from a cold boot: "Chapitre 1" at frame 2000, Ark's room with dialogue at 4800. The seven other test ROMs drift by ±1 % in peak amplitude — every SPC instruction now lands one window later — and none lost audio.

Two caveats. `roms/Terranigma (F).resume` stays broken by construction: it was captured on the old build with the SPC already derailed. And the code landed inside commit `0d50036`, whose message is about CI — a `git add -A` swept an agent's in-flight work; the fix is `core/src/apu/mod.rs` and `core/src/apu/spc700.rs` in that commit.

## Cartridge coprocessor decision (UPDATED)
User provided a Yoshi's Island ROM = **SuperFX** (GSU-2), so the target chip pivoted from SA-1 to **SuperFX** (testable against a real ROM). SA-1 reference doc (references/sa1.md) was written and is kept for a future SA-1 pass; SA-1 core was never started. SuperFX is a from-scratch GSU CPU (new instruction set) — larger than SA-1 but now game-validatable on Yoshi's Island.

## Menu bar: Cmd+Q / Quit menu does not flush SRAM
The native macOS "Quit" (App menu / File menu / Cmd+Q) calls AppKit terminate: directly, bypassing the SRAM-flush-on-exit code that runs after event_loop.run_app() returns. Esc and the red close button still save correctly. Fix: install an NSApplicationWillTerminate observer (or a muda-driven custom Quit that flushes then exits) so battery saves are never lost on Cmd+Q. Do this together with save-states (both need a clean shutdown hook).

## SuperFX / GSU — WORKING (Yoshi's Island boots)
The GSU coprocessor is integrated AND functional. Yoshi's Island is detected as SuperFX, the GSU executes its decompressors, and the game **boots to the LANGUAGE SELECT screen with correctly GSU-decompressed graphics** (border, flags, text). 255 core tests pass; base ROMs unaffected.

Bugs found and fixed along the way (all verified vs bsnes/snes9x source): GSU disassembler + `--trace-gsu` tooling added; PLOT/COLOR bitplane format; ROM/RAM bank decode ($60-7F = Game Pak RAM); RAM word byte-swap (addr^1); ROM/RAM read-ahead buffer (romdr/romcl); and the decisive one — **the GSU opcode fetch pre-advanced R15**, making every R15-derived value (branch targets, LINK, delay-slot immediates) one byte too high and cascading into wrong decompressor loop counts. Fixed by splitting opcode fetch (no R15 advance) from operand fetch, plus an r15_written flag for the implicit end-of-instruction R15++.

Remaining (not blocking boot): deeper Yoshi's Island gameplay past LANGUAGE SELECT is not yet exercised and may surface further GSU edge cases; cycle-accurate GSU/SNES clock arbitration is approximated (per-instruction, not per-clock). Other SuperFX games (Star Fox, Doom, Stunt Race FX) untested. A live bsnes GSU trace remains the fastest way to chase any future GSU divergence.

## Tooling
- `--trace-spc` is a no-op: expose an SPC700 trace hook in the APU core (needed for M8 debugging). DO THIS IN M8.
- `--log-mmio` matches on low-16-bits only, so WRAM shadow writes at `$7E/$7F:21xx`/`:42xx` are logged as FAKE `$21xx/$42xx` register events — actively misleading. Fix: only log when the access is to a real mapped register bank ($00-$3F/$80-$BF). DO THIS IN M8 (audio debugging depends on trustworthy MMIO logs).
- Frontend prepends `target/debug-out/` to `--trace`/`--dump-frame`; pass BARE filenames to avoid doubled paths.

## Phase 2 — CRT/Lissé coûteux sur une très grande fenêtre (mesuré)
Le compositing (zoom + filtre + ratio + letterbox) est fait **sur CPU** : `pixels` 0.15 impose
`FilterMode::Nearest` sans point d'extension public, et monter un second pipeline wgpu/WGSL n'a pas
été jugé rentable à ce stade. Coûts mesurés (Apple Silicon, release) :

| Fenêtre | Filtre | Coût/image | Verdict |
|---|---|---|---|
| 1172x896 (zoom x4, TV) | CRT | 4,1 ms | OK |
| 3840x2160 (4K plein écran) | Aucun (défaut) | **4,0 ms** | OK |
| 3840x2160 (4K plein écran) | CRT + TV | **23,4 ms** | dépasse le budget de 20 ms a 50 Hz |

Le défaut (`Aucun`) est sûr à toute taille ; seuls `CRT`/`Lissé` en plein écran 4K posent problème.
**Correctif suggéré si le cas se présente** : composer l'effet CRT à une résolution intermédiaire
(~2-3x le natif, ex. 1024x896) puis agrandir au plus proche voisin — visuellement quasi identique
(les scanlines n'ont pas besoin de la résolution native de l'écran) pour un coût divisé par ~6.
Alternative plus lourde : un vrai shader wgpu.

## CRASH — toute modale native ouverte DEPUIS la boucle winit tue l'application
**Symptôme signalé :** le menu « À propos » fait planter l'application (reproduit en 0.1.0 puis en
0.2.0, donc la première correction — passer des métadonnées à `PredefinedMenuItem::about` — était
une hypothèse fausse).

**Cause racine (prouvée) :** `winit-0.30.13/src/platform_impl/macos/event_handler.rs` panique
volontairement en cas de réentrance :
```
58:  unreachable!("tried to set handler while another was already set");
64:  unreachable!("tried to set handler that is currently in use");
135: panic!("tried to handle event while another event is currently being handled");
```
La pile de crash montre `EventHandler::set` juste sous `-[NSApplication run]`. Le panneau « À propos »
d'AppKit ouvre une **boucle d'événements imbriquée** → réentrance dans le gestionnaire winit →
panique Rust traversant les cadres Objective-C → `objc_exception_rethrow` → abort.

**Portée : ce n'est pas propre à « À propos ».** Toute modale native déclenchée *depuis* un callback
de la boucle est concernée :
- `PredefinedMenuItem::about` (planté, confirmé) ;
- la **confirmation de sortie** (`rfd::MessageDialog` sur Échap / Quitter) — à vérifier, même
  mécanisme (NSAlert `runModal`) ;
- le **sélecteur de ROM** ouvert par la touche `O` / menu Ouvrir — à vérifier. *(Celui du démarrage
  est sûr : il s'exécute AVANT `event_loop.run_app()`.)*

**Correctifs possibles, par ordre de préférence :**
1. **Remplacer ces modales par des fenêtres in-app egui** (Phase 8, en cours) — supprime la classe de
   bug entière : plus aucune boucle imbriquée. C'est la bonne réponse pour « À propos » et la
   confirmation de sortie.
2. Pour ce qui doit rester natif (sélecteur de fichiers), **différer l'ouverture hors du callback**
   (`dispatch_async` sur la file principale) pour que la modale s'exécute sur une pile propre.
3. À défaut, retirer l'entrée « À propos » : un menu qui plante à coup sûr est pire que pas de menu.

**Leçon de méthode :** ne plus annoncer ce type de correctif comme acquis sans vérification à
l'écran — l'environnement de développement n'a pas d'affichage, donc ces chemins ne sont testables
que par l'utilisateur ou par une analyse de la pile de crash.

**État après la Phase 8 (code, non vérifié à l'écran) :**
- `PredefinedMenuItem::about` n'est plus installé (`menu::install`) ; l'information est dans
  `Réglages… > À propos`, dessiné par egui.
- La confirmation de sortie est une `egui::Modal` (`ui::confirm`) : plus de `rfd::MessageDialog`,
  donc plus de `NSAlert runModal` depuis un callback.
- Les sélecteurs natifs restants (ROM, dossier des ROMs, dossier des captures) passent par
  `crate::dialog` : la demande est mise en file, puis postée sur la file principale libdispatch
  (`dispatch_async_f`) et exécutée entre deux callbacks winit, pile propre. `about_to_wait` est
  lui-même un callback winit, donc un simple report « à la prochaine itération » n'aurait pas suffi.
- Garde automatisée : `dialog::tests::the_event_loop_never_calls_the_native_picker_itself` échoue si
  `video.rs` mentionne `picker::` ou `rfd::`.
