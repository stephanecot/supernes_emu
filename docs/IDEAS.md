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
