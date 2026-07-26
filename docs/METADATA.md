# Jaquettes et métadonnées, sans compte ni clé

Enrichir la bibliothèque avec la jaquette officielle et les faits d'un jeu, en
complément des vignettes que l'émulateur produit lui-même. Aucune inscription,
aucune clé d'API : tout ce qui suit a été vérifié accessible en l'état.

## L'appariement d'abord, le reste ensuite

Le défaut de toutes les solutions par nom de fichier est qu'elles devinent.
`Super Mario Kart (E) [!].zip` ressemble à beaucoup de choses. On supprime la
devinette en passant par une empreinte :

1. **CRC32 de la ROM**, en-tête copieur retiré — le chargeur le fait déjà.
2. **DAT No-Intro** (`libretro-database/metadat/no-intro/`) : CRC → nom
   canonique. `Super Mario Kart (Europe)`, de façon **certaine**.
3. Tout le reste se lit avec ce nom comme clé — **correction relevée à
   l'implémentation** : les fichiers de catégorie sont en réalité indexés sur
   le **même CRC** (`rom ( crc XXXXXXXX )`, le nom canonique n'y figurant qu'en
   `comment`). C'est mieux que prévu : plus aucun appariement par nom dans la
   chaîne, sauf Wikipédia. Le nom canonique ne sert donc qu'à deux choses : le
   fichier de jaquette et le titre interrogé sur Wikipédia.

Un jeu absent du DAT (dump modifié, traduction amateur, homebrew) n'est pas une
erreur : il garde simplement sa fiche telle qu'elle est aujourd'hui.

## Ce que chaque source donne réellement

**`libretro-database`** — fichiers texte bruts sur GitHub, un par plateforme,
quelques centaines de kilooctets. Relevé pour la SNES :

| catégorie | entrées |
|---|---|
| `genre` | 3 851 |
| `developer` | 3 850 |
| `publisher` | 3 845 |
| `maxusers` (nombre de joueurs) | 3 759 |
| `releaseyear` | 3 305 |
| `esrb` (classification d'âge) | 1 156 |

S'y ajoutent `releasemonth`, `franchise`, `origin`, `serial`, `bbfc` et `elspa`
(deux autres organismes de classification), `enhancement_hw` (le coprocesseur —
que nous détectons déjà nous-mêmes, donc utile seulement en recoupement).

**Ce que cette base n'a pas : la moindre description.** Aucune catégorie de
prose, vérifié en listant l'arborescence complète. C'est le point dur, et il
faut une seconde source.

**Wikipédia** — l'API de résumé (`/api/rest_v1/page/summary/<titre>`) répond
sans clé et rend un paragraphe propre : 636 caractères pour Super Mario World,
495 pour Terranigma, 617 pour Secret of Mana. C'est exactement le registre
recherché.

Trois réserves à assumer, plutôt qu'à découvrir :

- **L'appariement redevient nominal.** Le CRC nous a donné un nom certain, mais
  Wikipédia s'interroge par titre : un jeu homonyme d'un film existe. La
  description doit donc être **attribuée visiblement** à Wikipédia, pour qu'une
  erreur se voie et s'explique au lieu de passer pour une affirmation de
  l'application.
- **Licence CC BY-SA** : l'attribution n'est pas une politesse, c'est la
  condition d'usage. Le lien vers l'article accompagne le texte.
- **Anglais seulement.** L'interface est bilingue, cette description ne l'est
  pas. Elle est marquée comme telle plutôt que présentée comme du contenu
  traduit manquant.

**Jaquettes** — `libretro-thumbnails`, un dépôt par plateforme, nommé par le nom
canonique. Comme celui-ci vient du CRC et non d'une ressemblance, l'appariement
est fiable.

**Ce que personne ne donne : la difficulté.** Elle n'existe dans aucune base
ouverte ; c'est une donnée propre à ScreenScraper, qui exige un compte. Elle
sera donc estimée par IA, et **présentée comme une estimation**, jamais alignée
avec les faits catalogués.

## Réseau

L'application ne fait aujourd'hui aucun accès réseau, et cela reste vrai tant
que personne ne le demande. Deux déclencheurs, tous deux explicites : un bouton
« Compléter la fiche » sur un jeu, et une action de rattrapage sur toute la
bibliothèque. Rien au scan, rien au démarrage.

Tout ce qui est récupéré est mis en cache sur disque et n'est jamais redemandé.
Les fichiers de `libretro-database` se téléchargent une fois pour toutes et
servent ensuite hors ligne à toute la collection — un seul aller-retour pour des
milliers de jeux, ce qui est aussi la façon la plus sobre de traiter ces
serveurs.

Un échec réseau laisse la fiche exactement dans l'état où elle était.
