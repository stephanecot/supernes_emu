# Brief de refonte visuelle — Prisme - SuperNes

## Le problème de fond

L'interface a été construite **sans jamais être vue** : l'environnement de développement n'a pas
d'écran. Résultat : elle fonctionne, mais aucune décision visuelle n'a été jugée. Avant toute
retouche, il faut **fermer cette boucle**.

### Livrable n°0 (préalable, bloquant) : rendre l'UI visible en headless

Un rendu **hors écran** des vues egui vers un PNG, par exemple :

```
prisme --ui-shot library out.png     # accueil / bibliothèque
prisme --ui-shot game-sheet out.png  # fiche de jeu
prisme --ui-shot settings out.png    # réglages
```

Techniquement : `egui` peut tourner sans fenêtre (contexte + `egui-wgpu` sur une texture hors écran,
ou `egui_kittest` pour du test d'instantané). Alimenter les vues avec un état factice réaliste
(une dizaine de jeux, dont certains sans vignette, des titres longs, des favoris).

Sans ce dispositif, toute consigne esthétique reste un pari — c'est précisément ce qui a produit le
résultat actuel.

---

## Défauts signalés par l'utilisateur

| # | Constat | Cause probable (relevée dans le code) |
|---|---|---|
| 1 | « L'icône Prisme n'est pas la bonne » | `ui/theme.rs` dessine une marque à partir de « the icon's four squares », alors que l'icône réelle est un **prisme réfractant quatre faisceaux**. L'in-app ne correspond pas à l'application. |
| 2 | « Les tuiles n'ont pas la même taille » | Les cartes ont pourtant `CARD_W`/`CARD_INNER_H` fixes → l'irrégularité vient des **vignettes**, insérées sans recadrage à une boîte constante (ratios variables, vignettes manquantes). |
| 3 | « Scroll horizontal pas pratique » | La grille est déclarée verticale mais les cartes sont posées à la main ; si la largeur disponible est mal calculée, le contenu déborde et egui ajoute une barre horizontale. |
| 4 | « Il manque un côté pro » | Absence de système visuel : pas d'icônes, pas d'états au survol, hiérarchie typographique plate. |
| 5 | Demandes explicites | **icônes**, **états au survol** sur les tuiles, **meilleur agencement**, **onglets**. |

---

## Direction artistique

**Ancrage dans le sujet.** Le produit s'appelle *Prisme* : un prisme décompose la lumière blanche en
ses composantes — exactement ce que fait l'émulateur avec une image (couches BG, sprites, color math)
et ce que fera le mode « rayons X ». Le système visuel doit venir de **la lumière réfractée** et du
monde de la console 16 bits, pas d'un habillage générique.

**À éviter absolument** (ce sont les réflexes par défaut, pas des choix) : fond crème + serif
contrasté + accent terracotta ; noir quasi total + un seul accent vert acide ; mise en page
« journal » avec filets et colonnes denses.

### Couleurs
Fond **ardoise profonde légèrement bleutée** (pas un noir pur : moins dur, plus « appareil »), avec
les **quatre couleurs du prisme** comme système signifiant, pas comme décoration :

| Rôle | Couleur |
|---|---|
| Fond | `#16171F` (base) / `#1E2029` (surfaces) |
| Texte | `#E8E9F0` primaire / `#9AA0B4` secondaire |
| Rouge | `#E45C5C` |
| Jaune | `#F0C24A` |
| Vert | `#5BC15B` |
| Bleu | `#4C86E0` |

**Les quatre accents portent du sens** : chaque **coprocesseur** a sa couleur (SuperFX, SA-1, DSP-1,
CX4) sur la pastille de la carte, et le **favori** est jaune. Un accent n'apparaît jamais sans raison.

### Typographie
Deux rôles au minimum, chargés en TTF embarqué (les polices par défaut d'egui font « prototype ») :
- **Interface / titres** : une grotesque géométrique avec du caractère (ex. *Space Grotesk*).
- **Données techniques** : une monospace (ex. *IBM Plex Mono*) pour région, mapping, somme de
  contrôle, taille — cela **distingue visuellement la donnée machine du texte humain**, ce qui est
  juste pour un émulateur.

Échelle de type explicite (ex. 24 / 17 / 14 / 12) avec des graisses tranchées — pas trois tailles
presque identiques.

### Élément signature (le seul endroit où l'on ose)
Un **filet spectral** : une hairline qui se décompose en quatre segments colorés, utilisée **une
seule fois** — sous l'onglet actif. Elle rejoue la réfraction du prisme et sert de repère de
navigation. Tout le reste reste sobre.

### Agencement
- **Onglets** en haut (demande explicite) : `Bibliothèque` · `Favoris` · `Récents` · `Réglages`,
  soulignés par le filet spectral.
- **Grille responsive** : nombre de colonnes calculé depuis la largeur disponible, **scroll
  vertical uniquement**, jamais de barre horizontale (à vérifier sur capture).
- **Cartes strictement uniformes** : la vignette est recadrée dans une boîte fixe au ratio 256:224
  (letterbox si besoin) ; une vignette absente affiche un **placeholder dessiné** (silhouette de
  cartouche + le prisme), jamais un vide de taille différente.
- **États au survol** (demande explicite) : élévation légère, vignette éclaircie, apparition d'un
  bouton *Jouer* et d'une étoile *Favori*, transition courte (~120 ms). Le focus clavier doit être
  visible aussi.

### Icônes (demande explicite)
Un jeu restreint, **dessiné au vecteur avec le painter d'egui** plutôt qu'une police d'icônes
(zéro dépendance, style cohérent, et cela évite les questions de licence) : lecture, étoile,
engrenage, dossier, puce, loupe. Trait uniforme, coins nets.

### Écriture
Libellés à l'infinitif ou à l'impératif, en minuscules-capitale de phrase, sans emphase inutile :
« Jouer », « Ajouter aux favoris », « Choisir un dossier de ROMs ». Un écran vide n'est pas un vide :
« Aucun jeu ici. Choisissez le dossier qui contient vos ROMs. » + le bouton correspondant.

---

## Aperçu par slot de sauvegarde (demande explicite)

Quand on enregistre un état (`F5` / menu / panneau), écrire **à côté du fichier d'état une capture
du framebuffer au moment exact de la sauvegarde** : `<jeu>.state3` → `<jeu>.state3.png`
(image brute 256×224, comme `--dump-frame`). Le même mécanisme vaut pour l'état de reprise
instantanée (`.resume.png`).

À l'affichage :
- **Fiche de jeu** : la liste des slots montre pour chacun sa **vignette**, l'horodatage et le
  numéro — un slot vide affiche un emplacement neutre, jamais un trou de taille différente.
- **En jeu** : le retour visuel après `F5` peut afficher brièvement la vignette du slot écrit.

Points d'attention :
- Écriture **atomique** comme le reste (module `atomic`), et **suppression conjointe** : effacer un
  slot doit effacer son aperçu, sinon la fiche affichera l'image d'une partie qui n'existe plus.
- L'aperçu est **facultatif** : son absence (ancien slot, échec d'écriture) ne doit jamais empêcher
  de charger l'état.
- Ne pas gonfler le stockage inutilement : un PNG 256×224 pèse quelques dizaines de Ko, c'est
  négligeable devant les ~529 Ko de l'état lui-même.

---

## Critère d'acceptation

La passe n'est pas terminée tant que **les captures headless** des trois vues n'ont pas été
regardées et jugées : cartes de taille rigoureusement identique, aucune barre de défilement
horizontale, marque cohérente avec l'icône de l'application, hiérarchie typographique lisible.
