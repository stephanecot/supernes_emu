# Rapport de construction — Émulateur Super Nintendo en Rust

*Temps de développement et consommation de tokens*

## Méthode de mesure

L'émulateur a été développé par **orchestration multi-agents** : des workflows lançant
des sous-agents spécialisés (constructeur, vérificateur adversarial, débogueur) en
parallèle, chacun rapportant à sa fin ses **tokens consommés** (`subagent_tokens`) et sa
**durée** (`duration_ms`). Le tableau ci-dessous agrège ces chiffres réels, phase par phase.

**Portée et limites — à lire avant d'interpréter les totaux :**
- Les tokens comptés sont ceux des **sous-agents** (qui ont fait l'essentiel du travail :
  écriture du code, vérifications, débogage). Les tokens de la **boucle d'orchestration**
  (le fil principal qui pilote les workflows) ne sont pas inclus — il n'existe pas de
  compteur de session global accessible.
- Les **durées sont le temps horloge (wall-clock) de chaque workflow**, parallélisme
  interne déjà pris en compte. Elles ne s'additionnent proprement que parce que les
  workflows ont été lancés **séquentiellement**.
- **Deux valeurs sont fortement gonflées par du temps d'attente, pas de calcul** : les deux
  phases « M3–M5 vidéo » (≈ 182 et 162 min). La première a été **interrompue par une
  expiration de session** (l'agent est resté bloqué), la reprise a subi des **relances de
  watchdog** sur de longs runs headless. Le temps de *travail effectif* de ces phases est
  bien inférieur (de l'ordre de 30–40 min chacune).
- Le **temps calendaire** s'étale du 20 au 22 juillet 2026, avec de longues pauses
  (utilisateur absent) : ce n'est pas du temps de travail continu.

## Détail par phase

| Phase | Livrable | Tokens (sous-agents) | Durée |
|---|---|---:|---:|
| Architecture | Plan d'implémentation détaillé | 13,9 K | 4,3 min |
| Références matérielles | 5 docs (CPU, MMIO, PPU, APU, timing) vérifiées vs nesdev/fullsnes | 1 122,7 K | 20,7 min |
| M0 — Scaffold | Workspace, loader ROM (.zip), détection LoROM/HiROM/PAL, CLI | 96,8 K | 11,2 min |
| M1–M2 — Fondations | 65C816 complet, SPC700+IPL, bus/scheduler, désassembleur, traces | 846,7 K | 72,7 min |
| M3–M5 — Vidéo (1ᵉ run) | PPU BG/sprites, GDMA, joypad *(interrompu — expiration session)* | 362,3 K | 182,1 min* |
| M3–M5 — Vidéo (reprise) | Reprise + intégration + vérif + gate *(watchdog sur longs runs)* | 927,3 K | 162,2 min* |
| M6–M7 — Effets | Color math, fenêtres, HDMA, mosaïque, Mode 7 | 533,8 K | 32,4 min |
| M8 — Audio | S-DSP complet (BRR, gaussienne, ADSR, écho) + sortie cpal | 526,9 K | 46,1 min |
| M9–M10 — Finition | SRAM disque, IRQ H/V, FastROM, open-bus, compatibilité | 750,2 K | 48,3 min |
| Bug SoM | Correction caractères écran de saisie (Mode 5 hires) | 83,8 K | 12,4 min |
| Sélecteur de jeu | Dialogue natif macOS (rfd) + touche O | 61,3 K | 6,0 min |
| Barre de menus | Menu natif macOS (muda) — Ouvrir/Pause/Reset | 115,0 K | 11,2 min |
| Save states | Sérialisation d'état complète + menu + gate déterministe | 438,3 K | 33,2 min |
| **Total mesuré** | | **≈ 5,88 M** | **≈ 642 min (10,7 h)** |

\* Durée gonflée par du temps bloqué/en attente (voir limites ci-dessus).

## Non comptabilisé (estimé)

- **SA-1 et SuperFX** (puces de cartouche, mises en pause) : phases de référence + une
  partie du cœur GSU réalisées avant l'arrêt — **≈ 0,4–0,7 M tokens** non rapportés (les
  workflows arrêtés ne renvoient pas de total final).
- **Tentatives de débogage de l'intro SMW** (3 sessions coupées par l'infra) : quelques
  centaines de milliers de tokens, sans rapport final propre.
- **Boucle d'orchestration principale** (pilotage, lecture des résultats, décisions) :
  non mesurée.

## Synthèse

- **Consommation de tokens** : **≈ 5,9 M mesurés** côté sous-agents ; en incluant les
  chantiers arrêtés, les débogages coupés et l'orchestration, un total réaliste se situe
  autour de **6,5–7 M tokens**.
- **Temps de calcul effectif** : le total brut de 10,7 h est trompeur — en retirant le
  temps bloqué des deux phases vidéo (expiration de session + watchdog), le **travail
  actif des agents avoisine 5–6 heures**, réparties sur 3 jours calendaires.
- **Résultat** : **≈ 16 000 lignes de Rust**, **219 tests unitaires**, un émulateur qui
  fait tourner 3 jeux commerciaux (image + son + sauvegardes), plus une application macOS
  double-cliquable (icône, barre de menus, sélecteur de jeu, save states).

### Répartition des tokens (les plus gros postes)

Les postes dominants reflètent bien la difficulté réelle du matériel émulé :
1. **Références matérielles (1,12 M)** — écriture *et* vérification adversariale octet par
   octet contre les sources de référence : c'est l'assurance anti-« constante inventée ».
2. **M3–M5 vidéo, reprise (0,93 M)** et **M1–M2 fondations (0,85 M)** — le PPU et les deux
   CPU (65C816 + SPC700) sont les composants les plus volumineux et les plus délicats.
3. **M9–M10 (0,75 M)**, **M6–M7 (0,53 M)**, **M8 audio (0,53 M)** — effets, timing et audio.

*Chiffres issus de la télémétrie des workflows ; voir les limites de mesure en tête de document.*
