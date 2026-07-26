# L'assistant, dans l'application

Le canal de contrôle (`--agent`) donne à un programme extérieur les mains et
les yeux : avancer, presser, regarder, lire et écrire la mémoire, sauvegarder
et recharger. Il est livré et vérifié. Ce qui manque est la **tête** — le
modèle qui décide — et le **bouton** qui la convoque.

## Ce qui rend l'intégration possible

`claude` est installé (`~/.local/bin/claude`, 2.1.220) et sait tourner **sans
interaction** (`-p`). L'application peut donc lancer un raisonnement local, sur
la machine du joueur, sans clé d'API ni compte à configurer dans l'émulateur —
c'est la session Claude Code déjà installée qui travaille.

**Conséquence assumée** : la fonction n'existe que si cet outil est présent.
Elle se détecte au démarrage et se désactive proprement, avec une phrase qui
dit pourquoi, plutôt que d'échouer au clic. C'est la même règle que partout
ailleurs ici.

## Deux usages, deux mécaniques différentes

**Trouver une triche** — « donne-moi des vies infinies ».

C'est le cas simple, et il faut le faire en premier : il ne rend **rien** à la
partie en cours. L'assistant travaille sur un processus émulateur séparé,
amorcé par un état sauvegardé, et son résultat est un fichier `<jeu>.cheats.json`
que l'application sait déjà lire. Aucune reprise de session, aucun risque pour
la partie du joueur : au pire il ne trouve rien.

**Franchir un passage** — « passe-moi ce boss ».

Plus délicat, parce qu'il faut *rendre* la partie. L'état est sauvegardé, le
processus séparé rejoue depuis là, et l'état final revient dans la session du
joueur. D'où deux garde-fous :

- **Un état est écrit avant**, toujours, et l'opération est annulable d'un
  geste. Quelqu'un qui n'aime pas la façon dont l'IA a joué doit pouvoir
  revenir exactement où il en était.
- **Le joueur peut arrêter à tout moment.** Un agent qui observe image par
  image est lent ; une minute d'attente sans moyen d'interrompre est
  insupportable, et une barre de progression qui ment l'est davantage. On
  montre ce qu'il fait — les images qu'il regarde — plutôt qu'un pourcentage
  inventé.

## Ce qu'on ne fera pas

**Pas de lecture des fichiers du joueur, pas d'envoi de la ROM.** L'assistant
reçoit un état, une image et l'accès au canal. Rien d'autre n'a de raison de
sortir.

**Pas de progression floue.** Si l'agent échoue à trouver l'adresse ou à
franchir le passage, il le dit. Un émulateur qui prétend avoir cherché est
pire qu'un émulateur qui n'a pas cherché.

**Pas d'assistant permanent.** Il est convoqué, il fait une chose, il rend la
main. Rien ne tourne en arrière-plan entre deux demandes.

## Ordre d'implémentation

1. Détection de `claude`, et le réglage qui expose l'état de cette détection.
2. La recherche de triche : le cas sans risque, avec son entrée dans la fiche
   de jeu à côté de la section « Triches » qui existe déjà.
3. Le franchissement de passage, avec l'état de sécurité et l'annulation.
