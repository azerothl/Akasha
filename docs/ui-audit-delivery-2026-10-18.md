# Livraison de l’audit d’interface — 18 octobre 2026

Périmètre : tickets #180 à #188. Branche : `codex/ui-audit-delivery`, basée sur
`origin/main` (`4f3ee5f`). Le travail Studio du dossier principal est séparé.

## Changements proposés

- #180 : zone de message multiligne, hauteur adaptative bornée, conservation de
  l’indentation et des retours à la ligne. Maj+Entrée ajoute une ligne ;
  Entrée envoie pendant la tâche active et Alt+Entrée met le message à sa suite.
  Le mode calculé au clavier est passé directement à l’envoi pour éviter de lire
  l’ancien état React.
- #181 : titre accessible du guide, focus initial et à chaque étape, boucle
  Tab/Maj+Tab, arrière-plan inerte et restitution du focus. Échap ferme le guide
  pour cette ouverture ; « Ne plus afficher » enregistre le choix.
- #182 : navigation fermée inerte et masquée aux aides techniques sous 768 px ;
  focus contenu dans le menu ouvert, fermeture par bouton/fond/Échap,
  retour au bouton du menu et traitement du redimensionnement.
- #183 : couleurs sémantiques des cinq thèmes, distinction texte sur accent /
  accent de lien, correction des boutons secondaires et synchronisation des
  sources canoniques. `node design-system/sync-design-system.mjs --ui-only`
  met à jour l’UI sans écrire dans les dépôts voisins.
- #184 : états `aria-pressed` de Web et Incognito. La préférence Web, auparavant
  uniquement locale, ajoute une consigne explicite au contexte envoyé à l’agent.
  Elle privilégie la recherche sans contourner `tools_policy` ni garantir
  qu’une recherche aura lieu. Incognito conserve le paramètre daemon existant.
- #185 : trois points de départ dans le chat vide, six destinations principales
  et un groupe « Plus d’outils » en mode Simple. Les 13 destinations restent
  accessibles. Panneau droit fermé par défaut, choix explicite enregistré,
  état vide des tâches simplifié, détails d’orchestration réservés au mode Expert,
  libellés FR/EN et aides clarifiés.
- #186 : sujet et lancement avant la visualisation de Recherche, visualisation
  différée au repos et paramètres avancés dans un volet dépliable.
- #187 : chargement différé des panneaux, carte, graphe, éditeur de notes,
  dialogue de création et éditeur de thème. Les panneaux visités conservent leur
  état ; la recherche continue après navigation. Une frontière d’erreur par
  panneau laisse le chat accessible. Si le navigateur conserve un échec d’import,
  le rechargement explicite restaure le brouillon de chat via un stockage de
  session temporaire. Si sa sauvegarde échoue, le rechargement est bloqué.
  L’adaptateur Notes en mode e2e utilise le proxy isolé plutôt que le port 3876.
- #188 : cibles des petites commandes d’au moins 32 px ; 44 px sur pointeur
  tactile, noms d’aide explicites, alignements et focus harmonisés. L’interface
  respecte aussi la préférence de réduction des animations.

## Vérifications observées

- Compilation TypeScript et build de production de l’UI réussis ; build e2e
  de production avec manifeste des imports réussi.
- Guide neuf : titre focalisé, quatre tabulations restent dans le dialogue,
  Échap restitue un point de départ utile et le choix de fermeture persistante
  est utilisable.
- Menu à 400 px : 20 tabulations restent dans le menu ouvert ; menu fermé,
  16 tabulations ne passent pas dans la navigation masquée. Borne 768 px :
  navigation masquée, bouton disponible, largeur de document égale à 768 px.
- Recherche à 800 × 600 : sujet et lancement visibles sans défilement initial
  (bouton vers y=355 avant la dernière réduction de l’en-tête).
- Collage multiligne et Maj+Entrée conservent le texte ; Web et Incognito
  exposent les états pressés. Aide et double panneau mesurés à 32 px de haut.
- Navigation Chat → Notes → Chat : brouillons conservés ; la note de vérification
  est enregistrée dans le daemon isolé.
- Import Comparer rendu indisponible dans une copie du build, puis restauré :
  erreur locale au panneau, retour au chat possible, rechargement réussi ;
  brouillon restauré exactement, y compris indentation et saut de ligne.
- FR/EN et modes Simple/Expert inspectés ; mesures de couleurs calculées après
  stabilisation des thèmes : 45 paires, toutes ≥ 4,5:1, minimum 4,77:1.
  Le JSON joint distingue échantillons CSS et couples de tokens au survol.

## Mesure du chargement

Builds Vite de production en mode e2e, même machine, mêmes dépendances et base UI.
Avant : fichier principal annoncé à 2 120,16 kB (619,21 kB gzip).
Après : environ 658 kB (195 kB gzip), soit environ 69 % de moins.
Le CSS principal passe d’environ 203 à 192 kB ; le CSS Leaflet est séparé.
Le manifeste confirme que Notes, Recherche, Comparer, carte et graphe sont des
imports dynamiques. Le petit chunk partagé Markdown reste initial.

Ces tailles ne mesurent pas le temps de démarrage Tauri. Le fichier principal
dépasse encore l’avertissement de 500 kB de Vite ; aucune disparition de cet
avertissement ni baisse du temps de compilation Rust n’est annoncée.

## Limites avant clôture

Pas de nouvelle suite de tests ajoutée ou exécutée : compilation et contrôles
manuels ciblés sur les critères des tickets. Les captures sont issues du mode
navigateur avec un daemon 0.11.0 et un répertoire de données d’audit isolé.

Restent à exécuter : lecteur d’écran réel, périphérique tactile physique,
parcours natif Tauri complet (dont envoi/queue/pièces jointes), recherches réelles
dans tous les états et mesures répétées du démarrage natif avant/après.
Les cinq thèmes n’ont pas été certifiés sur tous les composants et tous les
contenus. Les 45 paires mesurées ne constituent pas une certification globale.

Les tickets doivent rester ouverts tant que leurs critères de validation
restants ne sont pas satisfaits. Relier la PR et ses résultats CI à chacun.

## Suivi CI de la PR #189 — 18 octobre 2026

La première exécution GitHub Actions (39148820641) réussit les contrôles Rust
et le build llama.cpp Linux. Le parcours UI compte 14 réussites et un échec :
le test Calendrier cherche un onglet désormais masqué dans « Plus d’outils »
en mode Simple.

Le test ouvre maintenant ce menu, vérifie la visibilité puis la sélection
du Calendrier et contrôle les packs Life layer. Ce parcours est couvert en
français et en anglais, sans augmenter les délais ni ignorer de test.

Après correction : build UI e2e réussi et 16 tests Playwright réussis localement.
Cette exécution utilise le daemon publié 0.11.0 avec des données isolées et une
configuration temporaire remplaçant uniquement son démarrage et son arrêt.
La CI GitHub doit encore confirmer le résultat avec le daemon compilé de la PR.
Les limites de validation native et matérielle décrites plus haut subsistent.

## Captures

- [Recherche à 800 × 600](screenshots/ui-audit-2026-10-18/research-800.png)
- [Chat à 400 px](screenshots/ui-audit-2026-10-18/chat-400.png)
- [Erreur de chargement et récupération](screenshots/ui-audit-2026-10-18/panel-error.png)
- [Matrice de contrastes](screenshots/ui-audit-2026-10-18/theme-contrast.json)
