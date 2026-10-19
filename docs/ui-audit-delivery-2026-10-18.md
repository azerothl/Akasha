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

## Complément du 19 octobre 2026 — #185 à #188

Branche `codex/ui-audit-followup`, basée sur `main` à `773f57f` (PR #189
fusionnée). Ces corrections locales ne sont pas encore publiées ni fusionnées.

### Corrections

- **#185 — Simple/Expert :** retirer les enveloppes d’instructions avant de
  tronquer le titre d’une tâche, y compris dans le Calendrier ; protéger aussi
  l’interface contre les anciens titres déjà tronqués. Traduire les états du
  travail actif. En Simple, présenter les demandes principales et masquer les
  sous-tâches, événements bruts et détails de routage. Un bouton donne accès au
  mode Expert, où ces détails restent disponibles. Les questions nécessitant
  une réponse restent accessibles depuis le résumé du Chat.
- **#186 — Recherche :** une recherche sans preuve exploitable termine avec
  `research_no_evidence`, sans faux rapport exportable. La suffisance des
  sources dépend des sources retenues, pas du nombre de lectures tentées.
  L’interface reconnaît aussi les anciens rapports vides, explique comment
  réessayer et distingue un rapport partiel. Le rapport est lisible directement
  dans la page, avant le formulaire ; la visualisation n’occupe de place que
  pendant l’exécution. Les annulations sont identifiées dans l’historique.
  Correction connexe : `default_max_retries: 0` effectue maintenant l’appel
  initial au modèle au lieu de supprimer tous les appels.
- **#187 — Mesures :** ajout d’une sonde native explicitement activée en
  développement et d’un lanceur reproductible. La sonde est désactivée dans
  les binaires de production ; elle n’ajoute aucune télémétrie. Le lancement
  refuse d’écraser une observation et vérifie l’isolation du profil WebView.
- **#188 — Commandes :** état `aria-pressed` du double panneau, rendu actif
  associé, contrôles de focus, clavier et toucher synthétisé. Les cibles ont
  été mesurées avec un vrai rendu Chromium en émulation `pointer: coarse`.

### Vérifications exécutées

- Compilation UI de production et de test réussie ; compilation Tauri et daemon
  réussie. Avertissements préexistants conservés.
- **55 tests unitaires UI réussis.** Les tests des aides et du thème utilisent
  maintenant le contexte de langue et le nom accessible actuels.
- **20 tests Playwright réussis**, configuration
  `apps/akasha-ui/playwright.audit.config.ts`, sans daemon ni données utilisateur :
  matrice FR/EN × Simple/Expert × préférences neuves/existantes ; présence des
  13 destinations et conservation d’un brouillon après navigation ; accès
  clavier au détail Expert ; récupération après échec de chargement d’un
  panneau ; recherche réussie/partielle/vide/erreur/annulée.
- Deux contrôles Rust ciblés réussis : extraction du titre avant troncature
  (instruction longue, pièce jointe, sous-tâche orchestrée), et appel du
  fournisseur avec zéro ou deux essais configurés.
- Recherche **réelle**, avec le daemon compilé et Ollama `qwen3.5:9b` sur un
  port distinct, budget volontairement borné à 60 secondes : aucune source
  retenue, phase finale `error`, code `research_no_evidence`, aucun rapport.
  [Résultat du contrôle](screenshots/ui-audit-followup-2026-10-19/research-live-result.json).
  Le modèle était chargé en VRAM. Ce contrôle valide le traitement de l’absence
  de preuve ; il ne valide pas la qualité d’un rapport sourcé en conditions
  réelles. Les résultats positifs/partiels des tests UI utilisent des fixtures
  explicites et ne sont pas présentés comme des recherches réelles.

Commandes des contrôles ciblés :

```text
cd apps/akasha-ui
npm test
npx vite build --mode e2e
npx playwright test --config playwright.audit.config.ts
```

### Démarrage natif : protocole et observations

Windows 11 Professionnel Insider Preview 10.0.26220, Ryzen 7 9800X3D,
62 Gio de RAM utilisable, RTX 4080 SUPER 16 Go ;
Rust 1.94.1, Node 22.20.0, Vite 5.4.21, Tauri 2.12.4.
Même exécutable Tauri **debug**, mêmes préférences synthétiques, même URL locale
`127.0.0.1:5297`, fenêtre 800 × 600, frontends **minifiés de production** :
baseline `4f3ee5f` puis correction de cette branche. Pas de compilation ni
d’inférence lancée par cet audit pendant la série retenue.

Mesure monotone depuis l’entrée dans `run()` côté Rust jusqu’à un champ
`#chat-input` activé, visible, ayant effectivement reçu le focus, après deux
frames. Ce n’est ni un temps d’inférence ni une mesure de frappe/envoi du
premier message. Le défilement nécessaire pour rendre le champ visible fait
partie de la mesure pour les deux versions.

Chaque lancement « profil neuf » possède un répertoire WebView distinct.
Les trois lancements « profil réutilisé » reprennent le troisième de ces
profils. Cela mesure le cache du profil/HTTP ; **le cache disque de Windows
n’est pas vidé**. Aucun profil utilisateur n’est nécessaire au protocole final.
Les premières tentatives où Tauri ignorait le chemin absolu de la configuration
ont été écartées ; le lanceur utilise maintenant `data_directory` du builder
et refuse une série dont le profil isolé n’est pas créé.

| Frontend | Profil | n | Minimum | Médiane | Maximum |
|---|---|---:|---:|---:|---:|
| Avant | Neuf | 3 | 706,1 ms | 761,9 ms | 816,7 ms |
| Après | Neuf | 3 | 556,1 ms | 557,6 ms | 676,3 ms |
| Avant | Réutilisé | 3 | 580,5 ms | 604,7 ms | 788,0 ms |
| Après | Réutilisé | 3 | 473,8 ms | 480,9 ms | 522,2 ms |

[Les 12 observations brutes](screenshots/ui-audit-followup-2026-10-19/startup-observations.json).
Cette série est plus rapide après correction, mais trois observations par cas,
l’ordre avant/après et une charge système non verrouillée ne permettent pas de
généraliser le gain à une version installée. Le protocole utilise un shell debug
et des ressources servies en HTTP local : il reste à le répéter sur les paquets
de release avant toute promesse de gain natif en production.

Le JavaScript principal reste autour de 660 kB minifiés en production, contre
2 120 kB avant découpage. Cela ne mesure pas le temps de compilation Rust.

Lanceur (compiler le shell debug avec `build.devUrl` réglé sur cette URL ; fournir
deux distributions natives de production, puis un nouveau dossier de résultats) :

```text
node scripts/benchmark-native-startup.mjs <akasha-ui.exe-debug> <dist-avant> <dist-apres> <nouveau-dossier>
```

### Cibles bureau et tactiles

Chromium 147.0.7727.15 ; formats 400 × 600, 800 × 600 et 1280 × 720.
Chaque format est contrôlé avec pointeur fin puis `hasTouch: true`, avec
vérification de `matchMedia('(pointer:coarse)')`. Mesures après stabilisation
de l’ouverture du tiroir mobile, focus visibles, activation du double panneau
au clavier et au toucher synthétisé, centre du repli non recouvert.

| Commande | Pointeur fin | Émulation tactile |
|---|---|---|
| Aide de l’en-tête | 32 × 32 px | 44 × 44 px |
| Repli de navigation | 32 × 32 px | 44 × 44 px |
| Double panneau | 109,72 × 32 px | 109,72 × 44 px |

Les six relevés JSON et six captures sont dans
[`screenshots/ui-audit-followup-2026-10-19/`](screenshots/ui-audit-followup-2026-10-19/).
Ce sont des contrôles de rendu et d’entrée synthétisée : aucun écran tactile
physique n’a été testé, et aucune certification WCAG globale n’est revendiquée.

- [Rapport affiché dans la page — fixture](screenshots/ui-audit-followup-2026-10-19/research-ready.png)
- [Absence de preuve — fixture de compatibilité historique](screenshots/ui-audit-followup-2026-10-19/research-empty.png)
- [Navigation tactile à 400 px](screenshots/ui-audit-followup-2026-10-19/targets-touch-400.png)
