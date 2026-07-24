# Idées à implémenter plus tard

Backlog de fonctionnalités envisagées (hors périmètre des tâches en cours). À prioriser au fil de l'eau.

## Divers
- trouve un nouveau titre / logo original
- Afficher la version dans le menu
- **Confirmation avant de quitter** — `Échap` ne doit pas quitter directement l'émulateur, mais
  afficher une confirmation (« Quitter ? Oui / Non »). Mettre l'émulation en pause pendant la
  boîte de dialogue ; sauvegarder la SRAM dans tous les cas de sortie ; option « ne plus demander ».
  
## Affichage / options

- **Modes de zoom ×2 et ×4** — permettre d'agrandir la fenêtre de rendu (256×224 → 512×448, 1024×896),
  sélectionnables dans les options / le menu.
  - À décider : filtre de mise à l'échelle. Deux directions possibles selon l'intention :
    - **Net (pixel-perfect)** : mise à l'échelle entière au plus proche voisin — pixels francs, look « pixel art ».
    - **Dégradé / rétro (souhait exprimé)** : appliquer volontairement un filtre qui *adoucit / dégrade* le rendu
      pour un rendu plus proche d'une vieille télé (lissage bilinéaire, léger flou, ou filtre CRT :
      scanlines, aberration, courbure). Option à part entière « qualité dégradée / rétro ».
  - Idéalement : garder le ×1 pixel-perfect par défaut, et proposer ×2/×4 + un choix de filtre
    (Aucun / Lissé / CRT) indépendant du facteur de zoom.
  - Implémentation : côté frontend (`pixels`/GPU), pas dans le cœur. `pixels` gère déjà la mise à l'échelle
    de la surface ; ajouter un shader/option de filtrage et redimensionner la fenêtre selon le facteur.

## Vitesse de jeu

- **Mode accéléré (fast-forward) ×2 / ×3 / ×4** — augmenter la vitesse du jeu, via une touche (maintenue ou à bascule) et/ou le menu.
  - Comportement : exécuter N images émulées par image affichée (ou réduire l'échéance de pacing) selon le facteur choisi.
  - Audio : à ×N le son sort N fois plus vite → soit le **couper** en accéléré (simple), soit le lisser/ré-échantillonner
    (le ring buffer + contrôle de débit existants absorbent déjà une partie). Décider : mute par défaut en fast-forward.
  - Idéalement : une touche « turbo » maintenue (accéléré tant que pressée) + un choix de facteur ×2/×3/×4 dans les options.
    Éventuellement un mode ralenti (½×) en complément.
  - Implémentation : côté frontend (la boucle de pacing dans `video.rs`), le cœur `run_frame` est déjà rejouable à volonté.
    Attention à ne pas dépasser le budget CPU réel : à ×4 il faut émuler 4 trames dans le temps d'une — vérifier que ça tient.

## Suggestions (proposées par l'assistant)

- **Support d'une vraie manette (USB/Bluetooth)** — le clavier est limitant pour un émulateur de console.
  Via la crate `gilrs` : détecter les manettes, mapper les boutons vers `JoypadState`. Probablement le gain d'usage le plus fort.
- **Rewind (rembobinage)** — garder un tampon circulaire de save states récents (ex. les 30 dernières secondes) et
  rembobiner en maintenant une touche. Très apprécié, et déjà faisable : les save states sont rapides, complets et déterministes.
- **Configuration/remapping des touches** — laisser l'utilisateur redéfinir clavier + manette, sauvegardé dans les préférences.
  Complément naturel du support manette.
- **Codes de triche (Game Genie / Pro Action Replay)** — appliquer des codes (patch mémoire/ROM) via une entrée de menu.
  Classique des émulateurs.
- **Slots de save state multiples + quick save/load** — étendre le save state actuel (slot unique) à plusieurs slots numérotés,
  avec touches rapides (ex. F5 sauver slot courant / F7 changer de slot / F9 charger).
- **Coprocesseurs SA‑1 et DSP‑1** — après le SuperFX, débloquerait une grosse partie de la ludothèque :
  SA‑1 (Super Mario RPG, Kirby Super Star, Kirby's Dream Land 3), DSP‑1 (Super Mario Kart, Pilotwings). Gros chantier mais fort impact.
- **Correction du ratio d'aspect (PAR)** — les pixels de la SNES ne sont pas carrés ; une option d'aspect « authentique télé »
  (8:7 → ~4:3) complète bien les modes de zoom/filtre.
- **Périphériques d'entrée additionnels** — multitap (4 joueurs), SNES Mouse (Mario Paint), Super Scope — pour les jeux qui les utilisent.
- **Corriger le hang de l'intro de Super Mario World** (bug connu, documenté dans PUNCHLIST) et **valider le Mode 7 sur un écran de jeu réel**
  — deux trous d'exactitude restants sur la console de base.

## Idées innovantes (proposées par l'assistant, 2ᵉ vague)

Idées qui exploitent trois atouts déjà en place et rarement réunis : **état déterministe et
entièrement sérialisable** (round-trip byte-identique prouvé), **introspection complète**
(traces CPU/SPC700/GSU/SA-1, log MMIO, watchpoints, désassembleur), et **mode headless scriptable**.

- **Rejeu déterministe de session (« replay »)** — enregistrer une partie comme *(état initial + flux
  d'entrées)* au lieu d'une vidéo : quelques Ko pour une session entière, rejouable à l'identique.
  Faisable **parce que** le déterminisme est déjà prouvé. Double bénéfice majeur : le même mécanisme
  devient le **banc de test de non-régression** de l'émulateur (rejouer une partie enregistrée et
  comparer les empreintes de framebuffer → détecte toute régression d'exactitude). Bonus : « ghost »
  (courir contre son propre run dans Mario Kart). **M**

- **Netplay à rollback (style GGPO)** — jouer à deux en ligne en n'échangeant que les entrées, avec
  correction par rembobinage. Techniquement, la partie difficile (déterminisme + snapshots rapides et
  complets) est **déjà faite et vérifiée** ; il reste le réseau et la prédiction. Le plus ambitieux,
  mais c'est là que l'investissement d'exactitude paye le plus. **L**

- **Mode « rayons X » pédagogique** — au lieu d'un débogueur d'expert, un mode *apprenant* : activer/
  désactiver chaque couche (BG1-4, sprites) en direct, visualiser la VRAM en tuiles, la palette,
  les boîtes de sprites… et surtout **cliquer un pixel pour savoir ce qui l'a produit** (quelle
  couche, quelle tuile, quelle entrée de palette, quelle priorité). Prolonge directement le PDF
  pédagogique déjà écrit : l'émulateur devient un **instrument d'enseignement**, pas juste un
  lecteur de jeux. Différenciant. **M/L**

- **Profil technique automatique d'un jeu** — à partir du log MMIO, générer une fiche : modes BG
  utilisés, effets détectés (HDMA, color math, fenêtres, Mode 7), coprocesseur, pic de sprites,
  usage du DSP audio… Unique, peu coûteux (l'instrumentation existe), et cohérent avec l'identité
  pédagogique du projet. **S/M**

- **Export SPC (musique)** — un fichier `.spc` **est** exactement l'état de l'APU (64 Ko de RAM +
  registres DSP + registres SPC700), c'est-à-dire un sous-ensemble de ce que le save state capture
  déjà. Permet d'exporter la musique du jeu en cours pour l'écouter dans un lecteur SPC. Quasi gratuit
  vu l'infrastructure. **S/M**

- **Reprise instantanée (suspend/resume)** — sauvegarder automatiquement l'état à la fermeture et
  reprendre exactement où on s'était arrêté au lancement suivant (comportement « console moderne »).
  Trivial une fois les save states là, et très apprécié à l'usage. **S**

- **Rembobinage à la mort / modes d'assistance** — pour les joueurs débutants (et le mode enfant
  déjà prévu) : rembobiner automatiquement quelques secondes en arrière quand on perd une vie,
  ralenti à la demande (½×). Rend les jeux difficiles de l'époque accessibles sans les dénaturer.
  **S** une fois le rewind en place.

- **Découverte automatique de codes de triche** — plutôt que de saisir des codes Game Genie,
  *trouver* l'adresse mémoire d'une valeur (vies, temps, énergie) en comparant automatiquement des
  états successifs pendant que l'événement se produit. Faisable grâce aux snapshots complets et rapides.
  **M**

- **Environnement pour l'IA (façon Gym)** — le cœur est déterministe, headless, scriptable et sans
  I/O : l'exposer comme environnement d'apprentissage par renforcement (bindings Python), à la manière
  des émulateurs NES devenus des bancs d'essai de recherche. Cohérent avec l'origine du projet. **M**

## Rendu amélioré par IA (exploratoire)

- **Amélioration du rendu en temps réel** — question ouverte : peut-on améliorer l'image via l'IA ?
  Conclusion de l'analyse : oui, mais **pas par un upscaler neuronal appliqué à l'image finale**.
  - Pourquoi l'approche naïve déçoit : (1) **instabilité temporelle** — le réseau hallucine des détails
    légèrement différents à chaque image, d'où un scintillement sur les décors qui défilent ;
    (2) **latence** ajoutée (5-10 ms/image) là où les joueurs la ressentent ; (3) le **dithering** SNES
    était conçu pour être fondu par une télé cathodique — le « nettoyer » casse l'intention d'origine.
  - **Approche recommandée, propre à un émulateur** : exploiter la sémantique interne dont on dispose
    (couches BG1-4 et sprites séparées, identité des tuiles, palettes, décalages de défilement exacts).
    → **Cache de tuiles HD** : agrandir hors ligne, une seule fois, chaque tuile 8x8 unique avec un gros
    modèle de qualité, indexée par empreinte de contenu ; à l'exécution ce n'est plus qu'un blit.
    Coût runtime quasi nul et **stabilité temporelle parfaite** (même tuile -> même version HD).
    Traiter les couches séparément avant composition, en utilisant le défilement exact (pas besoin
    d'estimer un flux optique). Agrandir la *forme* en espace d'indices de palette puis appliquer la
    palette après, pour gérer gratuitement les permutations de palettes (cycles de couleurs, flashs).
  - **Précédent** : c'est le principe des packs de textures HD de Dolphin (textures identifiées par
    empreinte, remplacées par des versions upscalées à l'IA).
  - **Limites** : le Mode 7 et les sprites mis à l'échelle cassent l'hypothèse du blit de tuiles ;
    les tuiles écrites dynamiquement (décompression VRAM, animations) imposent une génération à la
    volée la première fois ; et « mieux » reste **subjectif** -> toujours une option, jamais un défaut.
  - **À faire d'abord** (moins cher, non contesté) : les filtres de la Phase 2 (CRT/scanlines) et un
    upscaler pixel-art classique type **xBRZ** — non neuronal, temps réel, sans latence.

## Audio

- **Mode muet** — couper le son via une touche (ex. M) et une entrée de menu (« Muet »), état reflété (coché) et mémorisé dans les préférences.
  - Implémentation : côté frontend — ne pas pousser les échantillons dans le ring buffer (ou multiplier le volume par 0),
    sans arrêter l'émulation de l'APU (le son doit reprendre instantanément au démuet). Idéalement, prévoir aussi
    un **réglage de volume** (0–100 %) tant qu'on y est.

## Sauvegardes

- **Choix du répertoire de sauvegarde** — pouvoir configurer où sont écrits les fichiers `.srm` (SRAM pile)
  et `.state` (save states), au lieu du dossier de la ROM par défaut.
  - Réglage dans les options (menu) : un sélecteur de dossier (dialogue natif `rfd`), mémorisé dans les préférences du frontend.
  - Comportement : si un dossier est défini, y écrire/relire les sauvegardes en nommant par la ROM (ex. `<dossier>/<jeu>.srm`) ;
    sinon, garder le comportement actuel (à côté de la ROM). Le flag CLI `--save` existant reste prioritaire pour une session ponctuelle.
  - Penser : migration/lecture des sauvegardes existantes, collision de noms entre jeux de même titre, création du dossier s'il manque.
  - Implémentation : côté frontend uniquement (le cœur reste sans I/O).

## Captures d'écran

- **Fonctionnalité de screenshot** — capturer l'image courante en mode fenêtré, via une touche (ex. F12) et une entrée de menu.
  - Écrire un PNG de la frame courante (256×224, ou à la résolution zoomée si un facteur est actif — à décider).
  - Destination : un dossier « Screenshots » configurable (voir l'idée « choix du répertoire de sauvegarde »),
    nom horodaté + titre du jeu (ex. `Yoshi's Island_2026-07-23_231500.png`).
  - Notes : la brique existe déjà côté cœur (le frontend produit le framebuffer, et le PNG headless `--dump-frame`
    montre le chemin d'écriture) ; il ne reste qu'à l'exposer en fenêtré (touche + menu). Décider si l'overlay FPS
    éventuel apparaît ou non dans la capture (probablement non → capturer le framebuffer brut).
  - Implémentation : côté frontend uniquement.

## Contrôle parental

- **Mode enfant** — limiter le temps de jeu à un nombre d'heures maximum par jour, protégé par un mot de passe.
  - Réglages : durée max/jour (ex. 1 h, 2 h…), définie et modifiable uniquement après saisie d'un **mot de passe parent**.
  - Comportement : cumuler le temps de jeu de la journée (remise à zéro à minuit) ; à l'atteinte de la limite,
    mettre en pause / bloquer le lancement d'un jeu, avec un message. Déblocage anticipé possible via le mot de passe.
  - Persistance : stocker le temps consommé du jour + la config (limite, hash du mot de passe) dans un fichier
    de préférences du frontend (ne jamais stocker le mot de passe en clair — hash).
  - Implémentation : côté frontend (options + un compteur de temps de session), rien dans le cœur d'émulation.
    Penser aux cas : changement de fuseau/horloge, plusieurs sessions dans la journée, fermeture/réouverture de l'app.
