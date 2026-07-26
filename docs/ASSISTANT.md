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

Plus délicat, parce qu'il s'agit de la partie en cours. La première version
lançait ici aussi un second processus, amorcé par un état sauvegardé, et
rendait l'état final à la fin. Elle marchait et elle était **invisible** : le
joueur qui demande à l'IA de jouer un passage demande à la *voir* jouer, et il
regardait une fenêtre à l'arrêt pendant qu'une console fantôme faisait le
travail. C'est le seul reproche qui compte ici, et il est décisif.

L'assistant pilote donc **la console déjà ouverte dans la fenêtre**
(`frontend/src/live.rs`) :

- L'application ouvre un **port TCP sur `127.0.0.1`** au début de la demande —
  TCP et non une socket Unix, parce que `std` n'a pas d'`AF_UNIX` sous Windows
  et que l'application y est livrée. Le port se ferme avec la demande.
- **Un secret est tiré par demande** et donné à l'assistant : un port en écoute
  sur la boucle locale est ouvert à tous les processus de la machine, et aucun
  d'eux n'a été invité à jouer à la partie de quelqu'un.
- L'assistant s'y branche avec `--agent-attach`, un mode client du même binaire,
  qui ne fait que transporter les lignes. **Le protocole ne change pas** : mêmes
  commandes, mêmes réponses, analysées par le même code (`agent.rs`).
- Une commande qui coûte des images (`step`, `press`) est **étalée sur autant
  d'itérations de la boucle d'événements**, une image chacune. La fenêtre
  continue de dessiner, le son continue de couler, et la réponse ne part qu'à la
  dernière image. Une commande qui bloquerait la boucle figerait précisément la
  fenêtre que cette fonction existe pour garder vivante.
- **Entre deux commandes, la partie est tenue à l'arrêt.** Les secondes que
  l'assistant passe à lire une capture ne sont pas des secondes de jeu : sans
  cela le personnage avancerait tout seul, sans personne aux commandes, ce qui
  dans la plupart des jeux veut dire tomber dans le premier trou. Un badge le
  dit à l'écran, plutôt que de laisser une image figée passer pour un plantage.

D'où les mêmes deux garde-fous qu'avant, inchangés :

- **Un état est écrit avant**, toujours, et l'opération est annulable d'un
  geste. Quelqu'un qui n'aime pas la façon dont l'IA a joué doit pouvoir
  revenir exactement où il en était — l'assistant reçoit d'ailleurs le chemin de
  ce fichier, pour se corriger lui-même.
- **Le joueur peut arrêter à tout moment.** Un agent qui observe image par
  image est lent ; une minute d'attente sans moyen d'interrompre est
  insupportable, et une barre de progression qui ment l'est davantage. On
  montre ce qu'il fait — la partie elle-même, maintenant — plutôt qu'un
  pourcentage inventé.

**Pendant la demande, la manette appartient à l'assistant.** Les touches de jeu
du joueur ne vont nulle part. Deux mains sur une manette n'est un état que
personne n'a demandé, et l'autre choix — qu'une touche arrête l'assistant —
casserait une partie en plein saut sur une main posée par mégarde sur le clavier,
en laissant l'assistant piloter une console qui ne lui répond plus. Arrêter est
un geste délibéré : le bouton `Arrêter` de la fiche, à un Échap de là. Tout ce
qui n'est pas un bouton de manette reste vivant — pause, capture, sauvegarde
d'état, plein écran.

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
4. Le canal en direct (`live.rs`) : la même demande, jouée dans la fenêtre.
