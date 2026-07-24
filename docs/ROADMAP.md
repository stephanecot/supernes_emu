# Plan d'implémentation — backlog `IDEAS.md`

Plan dérivé de `docs/IDEAS.md`, ordonné par **dépendances** puis par **valeur/effort**.
Tout est côté **frontend** sauf mention contraire : le cœur (`snes-core`) reste sans I/O.

Effort : **S** ≈ une passe courte · **M** ≈ une demi-journée d'agent · **L** ≈ chantier à part entière.

---

## Constat de départ (état réel du code)

| Fait | Conséquence sur le plan |
|---|---|
| **Aucune infrastructure de préférences** (`grep prefs/config/toml` → vide) | C'est le **verrou commun** : mute, volume, zoom, filtre, répertoires, remapping, mode enfant, mémorisation du FPS en dépendent tous → à faire **en premier** |
| `save::state_path(rom, slot)` **gère déjà les slots** (`game.state`, `game.state1`…) | Le multi-slot est à moitié fait : il ne manque que les raccourcis + le menu → quick win |
| Un save state = **529 Ko** | Dimensionne le rewind (voir Phase 5) : le stockage brut est trop lourd, compression nécessaire |
| `frontend/` : 2 100 lignes, deps `winit/pixels/cpal/rfd/muda/png/zip` | Pas de `serde`/`gilrs` encore → à ajouter aux phases concernées |
| Version figée à `0.1.0`, jamais affichée | Quick win immédiat |

---

## Phase 0 — Socle : préférences persistées **[S/M — à faire en premier]**

Sans ça, chaque option suivante réinvente sa propre persistance.

- Nouveau `frontend/src/prefs.rs` : struct `Prefs` sérialisée (serde + JSON ou TOML) dans le
  répertoire de config de l'OS (macOS : `~/Library/Application Support/<app>/prefs.json`).
- Chargement au démarrage, écriture à la sortie (et après chaque changement d'option, pour survivre
  à un crash). Valeurs par défaut si le fichier manque ou est corrompu (ne **jamais** planter dessus).
- Champs prévus dès maintenant (même si les features arrivent après) : `mute`, `volume`,
  `show_fps`, `zoom`, `filter`, `aspect`, `save_dir`, `screenshot_dir`, `keymap`, `pad_map`,
  `parental` (limite, hash, compteur du jour), `last_rom_dir`.
- Ajouter `serde`/`serde_json` (ou `toml`) aux deps du frontend.

**Décision :** JSON (lisible/éditable à la main) — recommandé — ou TOML.

---

## Phase 1 — Quick wins **[S chacun]**

Petits, indépendants, forte valeur perçue. À enchaîner dans une seule passe.

1. **Afficher la version dans le menu** — `env!("CARGO_PKG_VERSION")` dans l'entrée « À propos »
   du menu App (muda `PredefinedMenuItem::about` accepte des métadonnées) + dans le titre de fenêtre
   ou `--version` en CLI. *(Fixer aussi une vraie version, ex. `0.2.0`.)*
2. **Mode muet + volume** — touche `M` + menu « Audio > Muet » (coché), et un réglage de volume
   (0–100 %, ou pas de 10 % via menu). Implémentation : facteur de gain appliqué à la sortie
   (`audio.rs`), **sans arrêter l'APU** (reprise instantanée). Mémorisé (Phase 0).
3. **Captures d'écran** — touche `F12` + menu. Écrit un PNG horodaté nommé d'après le jeu.
   Réutiliser le code d'écriture PNG existant (`--dump-frame`). **Capturer le framebuffer brut**
   (256×224, sans l'overlay FPS, avant zoom/filtre) — décision retenue par défaut.
4. **Slots de save state multiples** — la brique existe (`state_path(rom, slot)`). Ajouter : slot
   courant en mémoire, `F5` sauver / `F9` charger / `F7` slot suivant (0–9), affichage bref du slot
   à l'écran (réutiliser le rendu de texte de l'overlay FPS), entrées de menu. Mémorisé.
5. **Confirmation avant de quitter** — `Échap` ne doit plus quitter directement mais afficher une
   **demande de confirmation** (« Quitter l'émulateur ? Oui / Non »), via `rfd::MessageDialog`
   (déjà en dépendance). Points d'attention : mettre l'émulation **en pause** pendant la boîte de
   dialogue ; « Non » reprend la partie ; la **SRAM doit être sauvegardée** dans tous les cas de
   sortie (Échap confirmé, croix rouge, Cmd+Q) ; éventuellement une préférence « ne plus demander ».
6. **Fast-forward ×2/×3/×4** — touche « turbo » maintenue (ex. `Tab`) + choix du facteur au menu.
   Implémentation : N appels `run_frame` par image présentée dans la boucle de `video.rs`.
   **Audio coupé pendant l'accéléré** (décision retenue). Vérifier que ×4 tient le budget CPU
   (sinon dégrader silencieusement au facteur atteignable).
7. **Reprise instantanée (suspend/resume)** *(idée retenue)* — à la fermeture, écrire
   automatiquement un save state « de session » ; au lancement suivant du même jeu, reprendre
   exactement où on s'était arrêté (comportement « console moderne »). Trivial : les save states
   existent et sont fiables. Le stocker à part des slots manuels (ex. `<jeu>.resume`) pour ne
   jamais les écraser. Préférence : reprise automatique ou proposée.
8. **Export SPC (musique)** *(idée retenue)* — entrée de menu « Exporter la musique (.spc) ».
   Un fichier `.spc` **est** exactement : en-tête ID666 + 64 Ko de RAM APU + 128 octets de registres
   DSP + les registres du SPC700 — c'est-à-dire un sous-ensemble de ce que le save state capture
   déjà. Il ne reste qu'à le réécrire au bon format et remplir l'en-tête (titre du jeu). **S/M**

---

## Phase 2 — Affichage : zoom, filtres, ratio **[M]**

Les trois touchent la même zone (surface `pixels`) → à faire ensemble.

- **Zoom ×1/×2/×3/×4** : redimensionne la fenêtre et la surface. ×1 pixel-perfect par défaut.
- **Filtres** (indépendant du zoom) : `Aucun` (plus proche voisin, net) / `Lissé` (bilinéaire) /
  `CRT` (scanlines + léger flou/vignettage) — c'est le « rendu dégradé rétro » souhaité.
  Implémentation : shader sur la surface `pixels` (wgpu), ou post-traitement CPU si plus simple pour
  scanlines. Commencer par `Aucun` + `Scanlines` puis enrichir.
- **Ratio d'aspect (PAR)** : option « Pixel-perfect (1:1) » vs « Authentique TV (8:7 → ~4:3) ».
  Complète naturellement le zoom/filtre.
- Le tout dans un menu « Affichage », mémorisé.

**Décision :** filtre par défaut au premier lancement — je recommande **Aucun** (net), le CRT restant
un choix explicite.

---

## Phase 3 — Manette + remapping **[M/L]**

Le plus gros gain d'usage réel (le clavier est frustrant pour un jeu de console).

- **Manette** : ajouter `gilrs`, détecter les manettes branchées (USB/Bluetooth), mapper les boutons
  vers `JoypadState`. Mapping par défaut sensé (croix/stick → D-pad, A/B/X/Y, L/R, Start/Select),
  branchement/débranchement à chaud géré proprement.
- **Remapping** : redéfinir clavier **et** manette, mémorisé (Phase 0). Interface : commencer par
  une entrée de menu « Configurer les touches » qui capture les appuis un par un (simple), plutôt
  qu'un éditeur complet.
- Bonus naturel une fois la manette là : support **2 joueurs** (le cœur gère déjà 2 manettes).

---

## Phase 4 — Répertoires configurables **[S/M]**

- Réglages « Dossier des sauvegardes » et « Dossier des captures » (sélecteur natif `rfd`), mémorisés.
- Comportement : si défini → `<dossier>/<jeu>.srm` / `.state` ; sinon comportement actuel (à côté de
  la ROM). Le flag CLI `--save` reste prioritaire.
- Points d'attention : créer le dossier s'il manque, collisions de noms entre jeux homonymes,
  et **ne pas perdre les sauvegardes existantes** (au minimum : les lire à l'ancien emplacement en
  repli, ou proposer une migration).

---

## Phase 5 — Rewind (rembobinage) **[L]**

Faisable car les save states sont **complets, rapides et déterministes** — mais le dimensionnement
est le vrai sujet : **529 Ko par état**.

| Stratégie | Fréquence | 30 s d'historique | Mémoire brute | Compressée (~3–5×) |
|---|---|---|---|---|
| Toutes les frames | 50/s | 1500 états | ~790 Mo | inenvisageable |
| Toutes les 6 frames | ~8/s | 250 états | ~132 Mo | **~30–45 Mo** ✅ |
| Toutes les 10 frames | 5/s | 150 états | ~79 Mo | ~20 Mo |

- **Approche retenue** : tampon circulaire, snapshot toutes les ~6 frames, **compression lz4**
  (rapide, la RAM émulée est très compressible), rembobinage en maintenant une touche (ex. `R`),
  avec réavance fluide entre deux snapshots si besoin.
- Mesurer le coût CPU du snapshot périodique (doit rester invisible à 50/60 fps).

---

## Phase 6 — Mode enfant (contrôle parental) **[M]**

- Réglages protégés par **mot de passe parent** (stocké **haché**, jamais en clair) : limite de
  temps/jour (ex. 1 h, 2 h), activation.
- Compteur de temps de jeu cumulé sur la journée, remise à zéro à minuit, persisté (Phase 0).
- À l'atteinte de la limite : mise en pause + message ; déblocage anticipé par mot de passe.
- Cas à traiter : fermeture/réouverture de l'app, sessions multiples, changement d'horloge/fuseau
  (se baser sur la date locale, tolérer un recul d'horloge sans « offrir » du temps).

---

## Phase 7 — Identité : nom + logo original **[✅ FAIT]**

Point à considérer sérieusement : le nom actuel **« SuperNES » est très proche de la marque
« Super NES » de Nintendo**. Pour un dépôt public, un nom **original** est plus sain (et plus
identifiable). Le logo/icône actuel (4 boutons colorés) peut être conservé ou retravaillé autour du
nouveau nom.

Pistes de noms (à trancher) : **Chrono16**, **Aurora16**, **Kestrel**, **Nova16**, **Mode7**,
**Pixelith**, **Ranger16**. Une fois choisi : renommer le bundle `.app`, `CFBundleName`, l'identifiant
`com.…`, le titre de fenêtre, le README, et regénérer l'icône.

---

## Phase 8 — Refonte de l'interface : une vraie application **[L]** *(idée retenue — phase phare)*

Aujourd'hui, lancer l'émulateur ouvre une boîte de dialogue de fichiers. L'objectif : **un véritable
écran d'accueil**, complet et soigné, qui fait passer le projet d'« émulateur qui marche » à
« application qu'on a envie d'ouvrir ».

**Écran d'accueil / bibliothèque**
- Grille de jeux avec **miniatures**, recherche, tri, **favoris** (épinglés en tête), section
  « repris récemment » (branchée sur la reprise instantanée, Phase 1).
- **Fiche de jeu** au clic : miniature, titre, région, mapping, taille, sauvegarde, **coprocesseur
  détecté**, temps de jeu cumulé, save states existants (avec vignette de chacun).
- **Tout est auto-alimenté** : l'en-tête de cartouche est déjà lu par le cœur (titre/mapping/région/
  SRAM/puce), et les **miniatures sont générées par l'émulateur lui-même** — lancer la ROM en
  headless quelques centaines de frames et capturer une image (exactement ce que font déjà les
  gates), puis mettre en cache. **Aucune base de données ni jaquette externe nécessaire.**
- Synergie : la fiche peut afficher le **profil technique automatique** du jeu (modes BG utilisés,
  HDMA, color math, Mode 7, pic de sprites…) — voir l'idée du même nom dans `IDEAS.md`. C'est le
  genre de détail qui distingue vraiment l'application.

**Panneaux de réglages** — remplacer les réglages éparpillés dans le menu natif par un vrai panneau :
affichage (zoom/filtre/ratio), audio (volume/muet), entrées (**remapping** clavier+manette),
répertoires, mode enfant, triches.

**Identité visuelle** — reprendre celle de *Prisme* : fond sombre, accents aux quatre couleurs du
prisme (rouge/jaune/vert/bleu), typographie sobre. Cohérent avec l'icône et le PDF pédagogique.

**Décision d'architecture (le vrai sujet de cette phase) :** tout ceci demande une **interface
graphique** que le frontend actuel (framebuffer brut + menu natif macOS) ne sait pas faire.
Recommandation : intégrer **`egui`** (s'intègre nativement à winit/wgpu, donc à `pixels`), en mode
immédiat — rapide à écrire, thémable, et qui cohabite avec le rendu du jeu.
C'est **l'investissement qui débloque d'un coup** : la bibliothèque, les panneaux de réglages, le
remapping (Phase 3), le gestionnaire de triches (Phase 9) et les réglages du mode enfant (Phase 6),
tous aujourd'hui bridés par l'absence d'UI. À faire tôt plutôt que tard, pour ne pas bâtir cinq
demi-solutions dans le menu natif.

---

## Phase 9 — Triches assistées par l'IA **[M]** *(idée retenue — dépend de la Phase 10)*

**Pas de codes Game Genie** (ni saisie, ni décodage, ni base de codes) : c'est **l'IA qui trouve et
applique** la triche, en langage naturel.

- **Usage visé :** l'utilisateur demande « donne-moi des vies infinies » ou « remets mon énergie au
  maximum ». L'agent s'en charge.
- **Méthode de l'agent** (via le canal de contrôle de la Phase 10, qui donne accès à la mémoire et
  aux snapshots) : recherche par **intersections successives** — prendre un instantané, laisser
  l'événement se produire (perdre une vie), reprendre un instantané, ne garder que les adresses dont
  la valeur a évolué comme attendu ; répéter 3–5 fois jusqu'à isoler l'adresse. L'agent pilote
  lui-même ces itérations au lieu de faire cliquer l'utilisateur.
- **Application :** figer la valeur (écriture continue) ou la fixer une fois, avec possibilité
  d'annuler. Mémoriser les triches trouvées **par jeu** (dans les préférences) pour les réactiver
  sans refaire la recherche.
- **Faisable parce que** les snapshots complets sont rapides, toute la WRAM est accessible, et
  l'émulation est déterministe (on peut rejouer le même événement à l'identique).
- L'UI de la Phase 8 sert à présenter les triches trouvées et à les (dés)activer.

---

## Phase 10 — Mode « IA » : faire jouer un agent **[M]** *(idée retenue)*

Permettre à une instance **Claude Code locale de jouer à la place de l'utilisateur** sur certains
passages (un boss trop dur, un niveau bloquant, une phase de grind — ou simplement une démo).

- **Ce qui existe déjà et rend ça faisable** : le cœur est déterministe, headless, piloté par script
  d'entrées, et sait exporter une image (`--dump-frame`) et l'état mémoire (`--dump-state`).
- **À construire : un canal de contrôle** — un mode `--agent` exposant un protocole simple en
  JSON par ligne (stdin/stdout ou socket local) :
  `step N` (avancer N frames), `press <bouton> <durée>`, `screenshot` (renvoie un PNG),
  `read-mem <adr> <len>`, `save-state` / `load-state`. L'agent boucle alors :
  **observer l'image → décider → envoyer des entrées**.
- **Intégration côté app** : une entrée de menu « Laisser l'IA jouer » qui met la partie en pause,
  lance l'agent sur l'état courant, puis rend la main (avec un save state avant/après pour pouvoir
  annuler). Très complémentaire du **mode enfant** (Phase 6) : franchir un passage bloquant sans
  frustration.
- **Bénéfice secondaire** : le même canal fait de l'émulateur un banc d'essai pour l'IA de jeu
  (l'idée « environnement type Gym » du backlog), et un outil de test automatisé pour l'émulateur
  lui-même.
- Points d'attention : bien délimiter ce que l'agent voit (image seule ? mémoire aussi ?), garder
  l'utilisateur maître (arrêt à tout moment), et rester honnête sur les performances (un agent qui
  observe image par image est lent — viser des séquences courtes et ciblées).

---

## Hors périmètre immédiat (à replanifier plus tard)

- **Périphériques exotiques** : multitap (4 joueurs), SNES Mouse, Super Scope. **M/L chacun**
- **Exactitude restante** (cf. `PUNCHLIST.md`) : hang de l'intro attract de Super Mario World,
  gate Mode 7 « pur » sur écran réel, validation des chemins profonds des coprocesseurs. **L**
- *(Fait)* Coprocesseurs SA-1 / DSP-1 — **terminés et validés en jeu réel**, à retirer du backlog.

---

## Ordre recommandé

*(Les numéros sont des étiquettes, pas un ordre : l'arbre ci-dessous fait foi.)*

```
Phase 7 (nom + logo)                                          ✅ FAIT — « Prisme »
Phase 0 (socle prefs JSON)                                    ← débloque 7 autres items
   └─ Phase 1 (version, muet, captures, slots, Échap, turbo,
               reprise instantanée, export SPC)                ← meilleur rapport valeur/effort
        ├─ Phase 8 (REFONTE UI : accueil, bibliothèque,        ← phase phare ; à faire TÔT,
        │           favoris, fiches, panneaux de réglages)        elle conditionne 3 et 6
        │      ├─ Phase 3 (manette + remapping)                ← plus gros gain d'usage
        │      └─ Phase 6 (mode enfant)
        ├─ Phase 2 (zoom + filtres + ratio)
        ├─ Phase 4 (répertoires)
        │      └─ Phase 5 (rewind)
        └─ Phase 10 (canal agent : l'IA joue)                  ← socle technique de la triche
               └─ Phase 9 (triches assistées par l'IA)         (UI de la Phase 8 pour l'affichage)
```

**Pourquoi la Phase 8 remonte si haut :** le remapping (3), les réglages du mode enfant (6) et
l'affichage des triches (9) ont tous besoin d'une vraie UI. Les faire avant la refonte reviendrait à
bricoler des demi-solutions dans le menu natif, puis à les refaire. La séquence économe est donc :
socle de préférences → quick wins → **UI** → tout ce qui en dépend.

## Décisions attendues avant de lancer

1. **Format des préférences** : JSON.
2. **Filtre par défaut** : Aucun/net.
3. **Audio en accéléré** : coupé
4. **Rewind** : budget mémoire acceptable (~30–45 Mo compressé pour 30 s).
5. **Nom du projet** : **Prisme** ✅ (renommage appliqué : bundle, identifiant, binaire, icône, docs)
