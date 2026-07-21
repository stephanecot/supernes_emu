# Émulateur Super Nintendo (SNES) complet en Rust

## Contexte

Projet greenfield dans `/Users/stephanecottin/dev/proto/13` (répertoire vide, pas de git). Objectif : un émulateur SNES complet écrit from scratch en Rust, capable de faire tourner les jeux commerciaux de la console de base (Super Mario World, Zelda ALttP, F-Zero, Super Metroid…) avec le son. C'est un projet de ~14 000–18 000 lignes réparti en jalons vérifiables sur jeux réels.

### Décisions actées
- **Frontend** : pur Rust — `winit` (fenêtre/clavier) + `pixels` (framebuffer) + `cpal` (audio). Pas de SDL2.
- **Audio** : APU complet — SPC700 + S-DSP (8 voix, BRR, ADSR, écho, interpolation gaussienne).
- **Puces d'extension** (SuperFX, SA-1, DSP-1…) : hors périmètre. Console de base, LoROM + HiROM, sauvegardes SRAM.
- **Validation** : jeux réels directement → infrastructure de debug (trace CPU format Mesen2, désassembleur) construite dès le début.

### ROMs de test disponibles (`roms/`)

| ROM | Mapping | Région | Rôle dans les tests |
|---|---|---|---|
| Super Mario All-Stars + Super Mario World (E) | LoROM, 2,5 Mo | PAL | Jeu de référence des jalons M1–M5 (menu multi-jeux + SMW) |
| Secret of Mana (F) | HiROM, 2 Mo | PAL | HiROM, Mode 1, HDMA, Mode 7 (carte du monde / vol de Flammie), écho DSP |
| Secret of Evermore (E) [t1] | HiROM, 3 Mo | PAL | Stress test compatibilité, gros driver son |

Conséquences intégrées au plan :
- **Support PAL obligatoire** (les 3 ROMs sont PAL) : 312 lignes/frame, ~50,007 Hz, horloge maître 21 281 370 Hz. Région détectée via l'octet destination du header ; scheduler et pacing frontend paramétrés NTSC/PAL.
- **Loader** : ouverture directe des `.zip` (crate `zip`) en plus des `.sfc`/`.smc` bruts ; détection/strip d'un header copieur de 512 octets (si `taille % 0x8000 == 512`). Les 3 fichiers fournis n'en ont pas.

## Architecture

Workspace Cargo à 2 crates : `core` (émulation pure, zéro dépendance I/O, testable headless) et `frontend` (binaire winit/pixels/cpal).

```
Cargo.toml                      # [workspace] members = ["core", "frontend"]
core/src/
  lib.rs, snes.rs               # console : Cpu + Bus, run_frame()
  scheduler.rs                  # horloge maître u64, next_event, position H/V
  bus.rs                        # map mémoire, dispatch MMIO, open bus (MDR)
  cartridge/{mod,mapping,sram}.rs  # header scoring, LoROM/HiROM, SRAM
  cpu/{mod,ops,addressing,algorithms}.rs   # 65C816
  ppu/{mod,render,background,mode7,sprites,window,color_math}.rs
  apu/{mod,spc700,dsp,brr,ipl}.rs
  dma.rs                        # GDMA + HDMA (8 canaux)
  joypad.rs
  debug/{mod,disasm,trace}.rs   # trace format Mesen2, désassembleur 65816
frontend/src/
  main.rs                       # boucle winit, pacing 50,007/60,0988 fps (PAL/NTSC) à deadline absolue
  video.rs                      # pixels, 256×224 BGR555→RGBA
  audio.rs                      # ring buffer SPSC + resampling à contrôle de débit dynamique
  input.rs                      # clavier → JoypadState (Z=B, X=A, A=Y, S=X, Q=L, W=R)
```

### Choix techniques clés

- **CPU 65C816** : dispatch par un grand `match` sur l'opcode (256 bras — rustc génère une jump table, greppable, sans conflit de borrow). Largeur 8/16 bits par branche runtime sur les flags M/X. Mode émulation complet (boot en E=1, vecteurs différents), BCD complet, `WAI`/`STP`, `MVN/MVP` ré-exécutés octet par octet (interruptibles).
- **Trait `CpuBus`** (`read`/`write`/`idle`/`take_nmi`/`irq_level`, monomorphisé) : permet plus tard un bus RAM plate pour les tests JSON TomHarte sans redesign.
- **Granularité de timing** : horloge avancée à chaque accès mémoire avec le **coût exact par région** (6/8/12 cycles maîtres, FastROM via $420D), mais les autres composants ne sont rattrapés qu'au franchissement d'un unique timestamp `next_event` (une comparaison u64 par accès). Suffisant pour les jeux commerciaux.
- **Open bus** : registre MDR unique mis à jour à chaque lecture/écriture + les 2 latches open-bus internes PPU1/PPU2 — fait dès le jour 1 (misérable à retrofitter).
- **PPU** : rendu **par scanline** (rejet du dot-based, 5-10× plus complexe pour rien ici). Pipeline par ligne : BG (2/4/8bpp, offset-per-tile, mosaïque) → Mode 7 (matrice 8.8 signée) → sprites (limites 32 sprites/34 tuiles par ligne, flags $213E) → composition par table de priorités par mode (y c. quirk BG3 du Mode 1) → fenêtres W1/W2 (AND/OR/XOR/XNOR) → color math (add/sub/half, couleur fixe) → brightness. Ports VRAM avec **buffer de prefetch** ($2139/$213A), latches CGRAM/OAM, compteurs H/V ($2137).
- **APU** : SPC700 (même style de core, bus privé 64KB + IPL 64 octets embarqué en `const`), S-DSP steppé **1 échantillon / 32 cycles SPC** (32kHz) : BRR (4 filtres), interpolation gaussienne (table 512 entrées), ADSR/GAIN exacts, bruit LFSR, pitch modulation, **écho complet** (buffer en RAM APU, FIR 8 taps). **Synchronisation lazy catch-up** (pas de lockstep) : `apu.catch_up(clock)` à chaque accès $2140-43 + une fois par scanline. Conversion d'horloge en point fixe 32.32 (ratio 1 024 000⁄21 477 272), zéro dérive.
- **Audio hôte** : ring buffer 32kHz → callback cpal avec resampling linéaire et **contrôle de débit dynamique** (±0,5 % selon le remplissage du buffer) — la dérive d'horloge devient un micro-vibrato inaudible au lieu de craquements.
- **GDMA** : exécuté immédiatement à l'écriture de $420B, CPU bloqué, 8 cycles/octet ajoutés à l'horloge (les événements PPU/APU tombent au bon moment en plein transfert). **HDMA** : init à V=0, transfert par ligne en début de H-blank **avant** le rendu de la ligne (c'est tout l'intérêt), mode indirect, sémantique repeat.
- **Timing NTSC/PAL** : NTSC = 21 477 272 Hz maître, 1364 cycles/ligne, 262 lignes ; PAL = 21 281 370 Hz, 312 lignes, ~50,007 Hz. NMI à V=225 dans les deux cas, auto-joypad ~3 lignes. Région choisie d'après le header cartouche. Vsync **désactivé** côté `pixels` ; pacing par deadline `Instant` absolue (sleep puis spin), resync si >3 frames de retard.

## Jalons (chacun vérifiable sur une des 3 ROMs fournies)

SMAS+SMW = Super Mario All-Stars + Super Mario World (E), SoM = Secret of Mana (F), SoE = Secret of Evermore (E).

| # | Livrable | ROM test | Preuve |
|---|---|---|---|
| M0 | Workspace, loader ROM (.zip + header copieur), détection LoROM/HiROM + région PAL, désassembleur CLI | SMAS+SMW (LoROM), SoM (HiROM) | Titre/mapping/région corrects ; le reset vector se désassemble en init plausible (SEI/CLC/XCE…) |
| M1 | 65C816 + bus + WRAM + trace logger ; PPU/APU stubs | SMAS+SMW | La trace montre un boot propre puis un spin sur $2140 (handshake APU) |
| M2 | SPC700 + IPL + ports + timers (sans DSP) | SMAS+SMW | Handshake $AA/$BB passe ; upload du driver son visible en trace, le jeu poursuit son init |
| M3 | Scheduler PAL/NTSC (NMI/vblank), ports VRAM/CGRAM/OAM, GDMA, BG Modes 0/1, fenêtre winit | SMAS+SMW | Écran de sélection All-Stars et fonds de l'écran titre s'affichent |
| M4 | Sprites (OAM, priorités, limites/ligne) | SMAS+SMW | Curseur/sprites du menu et Mario, correctement superposés |
| M5 | Joypad (auto-read + $4016) | SMAS+SMW | Sélectionner SMW, entrer dans un niveau, déplacer/sauter. **Jouable, muet, à 50 Hz** |
| M6 | HDMA, fenêtres, color math, mosaïque | SoM | Dégradés de ciel, fondus, transitions mosaïque, effets de fenêtre corrects |
| M7 | Mode 7 | SoM | Carte du monde / vol de Flammie rendus et jouables (matrice par ligne via HDMA) |
| M8 | S-DSP + sortie cpal | SMAS+SMW, puis SoM | Musique du menu correcte à la bonne vitesse (50 Hz) ; écho audible dans SoM ; zéro craquement sur 5 min |
| M9 | SRAM, FastROM/MEMSEL, IRQ H/V, audit open bus | SoM (save), SoE | Save SoM persistante après restart ; SoE boote et tourne à pleine vitesse |
| M10 | Passe de compatibilité | Les 3 ROMs | Premières séquences de jeu complètes sur les trois, image et son corrects |

Justification de l'ordre : l'APU vient en 2e car quasi tous les jeux bloquent sur le handshake des ports avant de toucher au PPU — un vrai SPC700 élimine toute une classe de « pourquoi ça hang ». Le DSP est différé à M8 car le driver son tourne avec un DSP stubé.

## Phase 0 — Outillage agents & skills (avant l'implémentation, à la demande de l'utilisateur)

Générer dans `.claude/` du projet un outillage optimisé pour ce build, utilisé ensuite pendant toute l'implémentation ultracode :

**Agents** (`.claude/agents/*.md`) :
- `snes-component-builder` — implémente un composant/module du core d'après une spec détaillée (registres, comportements, quirks matériels) fournie dans le prompt.
- `snes-hw-verifier` — relit un composant en adversaire contre la spec matérielle SNES (flags 65C816, quirks PPU, timing DMA/HDMA, open bus) et rapporte les écarts sans réécrire.
- `snes-debugger` — diagnostic : lance l'émulateur headless sur une ROM, lit traces/dumps, localise la première divergence, produit un rapport actionnable.

**Skills** (`.claude/skills/*/SKILL.md`) :
- `snes-build-test` — builder le workspace (`cargo build/test/clippy`), lancer une ROM headless N frames, générer une trace, où sont les ROMs et comment les charger.
- `snes-refs` — aide-mémoire matériel condensé : carte des registres MMIO ($21xx/$40xx-$43xx), coûts de cycles 65C816/SPC700, tables de priorités PPU par mode, format BRR, timings PAL/NTSC. Évite à chaque agent de re-déduire ces constantes (source d'erreurs classique).

**Implémentation ultracode** : l'utilisateur a opté explicitement pour ultracode → orchestration par workflows multi-agents, un workflow par grande phase (M0–M2 fondations, M3–M5 vidéo+jouabilité, M6–M7 effets, M8 audio, M9–M10 finition). Schéma de chaque phase : builders en parallèle sur les modules indépendants → vérification adversariale (`snes-hw-verifier`) → correction → gate de jalon sur ROM réelle (headless + capture d'écran).

## Infrastructure de debug (M0–M1, pas après)

- **Trace logger au format Mesen2** : on peut lancer la même ROM dans Mesen2 et `diff` les traces pour trouver la première instruction divergente — trouve ~90 % des bugs CPU. Flags `--trace`, `--trace-start-frame N`.
- Hotkeys frontend : pause, avance frame par frame, avance scanline.
- Dumps WRAM/VRAM/CGRAM/OAM sur hotkey, `--log-mmio` (écritures $21xx/$43xx nommées), `--watch bank:addr`.

## Risques principaux (ordre décroissant)

1. **PPU** : tables de priorités par mode, quirk BG3, limites sprites/ligne, prefetch VRAM, combinaison des fenêtres, arrondis Mode 7 (~3 500–5 000 loc).
2. **HDMA** : timing init/reload et mode indirect — source classique de dégradés décalés d'une ligne.
3. **Latches NMI/IRQ** ($4210/$4211) : races que certains jeux exercent.
4. **Open bus / miroirs mémoire** : erreurs aux symptômes lointains et bizarres.

## Vérification

- À chaque jalon : lancer la ROM cible (`cargo run --release -p snes-frontend -- "roms/….zip"`) et constater la preuve listée dans le tableau ; en mode agent, exécution headless N frames + dump du framebuffer en PNG pour inspection.
- Bugs CPU : diff de trace contre Mesen2 sur la même ROM.
- `cargo test` : tests unitaires sur les briques pures (décodeur BRR, BCD ADC/SBC, décodage d'adresses LoROM/HiROM, désassembleur).
- M8 : écoute prolongée (5 min) pour valider le contrôle de débit audio.
- L'utilisateur fournit ses propres ROMs (non incluses au repo).
