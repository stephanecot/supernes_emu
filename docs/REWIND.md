# Rembobinage — dimensionnement mesuré

Le plan (Phase 5) posait le bon problème mais sur des chiffres estimés. Ils
sont maintenant mesurés, sur un vrai état de Terranigma, et ils changent les
conclusions dans le bon sens.

## Ce que coûte réellement un instantané

| | plan | mesuré |
|---|---|---|
| un état complet | 529 Ko | **273 Ko** |
| compressé | « 3–5× » | **101 Ko** (zlib niveau 1) |
| coût de compression | non mesuré | **1,9 ms** |

Un état pèse donc **la moitié** de ce que le plan supposait. Et le niveau de
compression le plus rapide est aussi le bon : passer du niveau 1 au niveau 6
gagne 4 % de place pour 84 % de temps en plus. Le choix est tranché par la
mesure, pas par le goût.

## Le budget qui en découle

Un instantané toutes les 6 images, en PAL (50 images/s), fait 8,3 instantanés
par seconde — soit un toutes les **120 ms**. La compression en consomme 1,9,
c'est-à-dire **1,6 %** du temps disponible entre deux instantanés, et environ
9 % d'une seule image. Invisible.

Pour 30 secondes d'historique, 250 instantanés :

| | mémoire |
|---|---|
| sans compression | 65 Mo |
| **compressés (niveau 1)** | **25 Mo** |

25 Mo pour une demi-minute de retour en arrière : c'est acquis, et cela laisse
la place d'aller plus loin si l'usage le demande.

## Décisions

**zlib niveau 1 plutôt que lz4**, contrairement à ce que proposait le plan.
Non par principe, mais parce que la mesure montre que la compression n'est pas
le goulot : à 1,9 ms tous les 120 ms, gagner un millième de seconde n'achète
rien, et zlib évite une dépendance de plus. Si un jour le budget se resserre —
instantanés plus fréquents, machines plus lentes — lz4 reste la porte de
sortie évidente.

**Un anneau de taille fixe en nombre d'instantanés**, pas en mégaoctets : la
durée d'historique est ce que la personne se représente, la mémoire est une
conséquence. Une seconde de jeu doit toujours valoir une seconde de
rembobinage.

**L'anneau se vide** au changement de jeu, au chargement d'un état et à une
réinitialisation : après l'un de ces trois événements, l'histoire enregistrée
n'est plus celle de la partie en cours, et rembobiner vers elle produirait un
saut inexplicable.

## Ce qui reste à trancher à l'implémentation

- **Le geste.** Maintenir une touche et voir le jeu reculer, ou reculer d'un
  cran par pression ? Le maintien est ce que font les émulateurs qui ont
  popularisé la fonction, et il se prête mieux à « je viens de rater mon saut ».
- **Entre deux instantanés.** Reculer par pas de 6 images donne un mouvement
  saccadé à 8 images par seconde. Le rendre fluide demande de repartir de
  l'instantané précédent et de réavancer les images intermédiaires — faisable
  puisque l'émulation est déterministe, mais c'est du travail en plus, et la
  saccade n'est peut-être pas gênante pendant un rembobinage.
- **L'audio.** Rembobiner en silence est le plus simple et probablement le
  mieux : personne n'attend d'entendre la musique à l'envers.
