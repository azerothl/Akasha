# Livraison Git / GitHub / CI — #160

> Mise à jour après validation : voir [les parcours réels du 18 octobre](../quality/STUDIO_AGENTIC_VALIDATION_2026_10_18.md). Les mentions « aucun test » ci-dessous décrivent le lot initial, avant cette validation.

Première tranche Git et GitHub.com via Git et `gh` préinstallés, autorisés et authentifiés. Les API GitHub Enterprise/GitLab ne sont pas prises en charge. Le profil doit autoriser explicitement les nouveaux outils lorsqu’il utilise une liste restrictive.

## Outils et API

- `git_delivery_status {remote:"origin"}` : fichiers, versions opaques, HEAD/branche, identité Git, remote, lecture de la branche distante et accès GitHub. Une erreur d’accès reste distincte d’une authentification confirmée.
- `git_commit {paths:[...],expected_versions:{path:version|"missing"},expected_head,message}` : sélection explicite de 1 à 128 fichiers. HEAD et versions sont revalidés. Reprendre les versions du statut : elles lient le contenu et le caractère exécutable Unix ; elles sont distinctes des versions de l’éditeur. Les fichiers non exécutables conservent leur token de contenu précédent.
- `git_push {remote,branch,expected_head,expected_remote_head}` : publie uniquement ce SHA vers cette branche. Aucun force, tag ou sous-module publié implicitement.
- `github_create_pr {remote,head,base,expected_head,title,body,draft:true}` : exige un push préalable du même commit. Recherche d’une PR ouverte existante, puis POST GitHub structuré ; aucun fork/push/merge implicite. Une réponse incertaine est recherchée avant de signaler le résultat inconnu.
- `github_pr_status {remote,number?}` : PR et contrôles, commit exact, date de lecture. `not_run` (aucun contrôle remonté), `pending`, `failed`, `passed`, `passed_with_skipped_or_neutral`, `unknown` et `stale_for_local_head` sont distincts. Ne lance aucun workflow CI.

Enveloppes Studio v1 natives ou `TOOL_JSON:` ; les `TOOL:` historiques préservent leur objet JSON. `git_push` et `github_create_pr` exigent une approbation dans le mode interactif. Le mode global `allow_all` reste une décision d’autonomie de l’opérateur. Les approbations persistantes lient le workspace et les arguments exacts ; les descriptions de publication ne recopient pas le corps de la PR. `git_commit` hérite aussi de l’approbation de `write_file`.

API utilisateur : `GET /api/studio/projects/:id/delivery/status?remote=origin`, `GET .../pr?remote=origin&number=...&task_id=...`, puis `POST .../commit`, `.../push`, `.../pr` avec `{task_id,operation_id,request,expected_repository}`. La tâche doit appartenir au projet et être arrêtée ; chaque bouton est une autorisation explicite de cette action. L’identifiant permet de retrouver le résultat sans effet supplémentaire. Les opérations inconnues exigent une inspection/résolution ; aucun retry automatique.

`GET /api/tasks/:id/delivery` retourne l’historique récent et l’activité. Les actions et consultations CI liées sont également enregistrées comme événements et preuves JSON. Une erreur d’archivage est affichée séparément de l’effet Git constaté.

## Préservation du travail

Le commit utilise un index temporaire construit depuis HEAD, stage uniquement les fichiers sélectionnés, écrit un objet commit, puis met à jour la référence de branche avec comparaison de l’ancien SHA. Les changements sans rapport déjà présents dans l’index ne sont pas inclus. Après le commit, seuls les chemins sélectionnés sont réconciliés dans l’index réel si celui-ci et le checkout n’ont pas changé entre-temps. Sinon le résultat indique `committed_index_reconciliation_needed` avec le SHA réellement créé ; le travail concurrent n’est pas écrasé. Les répertoires/sous-modules, chemins externes et noms non UTF-8 ne sont pas acceptés par cette sélection. Un renommage apparaît avec sa source et sa cible : choisir les deux pour le livrer en entier.

Les règles de répertoire sont chargées avant un commit agent ou l’application d’un renommage LSP, avec report de la mutation si des instructions nouvelles viennent d’être injectées. Les effets de publication inconnus arrêtent la boucle sans succès annoncé.

Git tourne sans hooks, signature interactive, fsmonitor ou diff externe pour cette opération. Les filtres et assistants de credentials configurés restent des programmes du dépôt/utilisateur ; aucune sandbox OS n’est revendiquée. La métadonnée Git doit résider dans le répertoire de données du daemon, ce qui couvre ses projets et worktrees. Le flux UI porte sur le projet sélectionné ; l’intégration préalable d’un worktree utilise l’action existante.

## Validation et acceptation restante

Compilation Windows : `cargo check -p akasha-daemon --no-default-features --features embedded`. Build TypeScript/Vite de l’interface. Aucun commit, push, création de PR ou scénario distant exécuté via ces nouvelles API, aucun test local ajouté/exécuté.

À valider avant clôture : index contenant des changements sans rapport, fichier modifié pendant staging, ref concurrente, index concurrent après commit, dépôt sans premier commit, refus d’identité/credentials/droits, branche distante modifiée, timeout après effet, PR déjà existante et réponse incertaine, CI absente/en cours/échouée/ignorée et SHA périmé. Ces scénarios complètent #143 ; le ticket reste ouvert.

Références : [Git commit](https://git-scm.com/docs/git-commit), [Git push](https://git-scm.com/docs/git-push), [GitHub CLI create](https://cli.github.com/manual/gh_pr_create). Le POST GitHub explicite évite les comportements de fork/push de l’assistant interactif CLI.

La publication opérateur (`push` / `pr`) exige `expected_repository` égal au dépôt `owner/repo` affiché par le préflight. La destination canonique est revérifiée sous le verrou du workspace, pour les appels agent après approbation comme pour ces publications opérateur. L’identité approuvée est une contrainte interne de l’enveloppe, liée au fingerprint ; elle ne confère aucune permission supplémentaire. La configuration Git locale est limitée à des métadonnées de dépôt/branches/remotes et à l’identité auteur. Toute autre clé (HTTP, helpers, includes…) ou erreur de lecture refuse la livraison ; aucune configuration utilisateur n’est réécrite. Les exécutables Git/gh sont résolus par chemin absolu hors du workspace.

Le contrôle des métadonnées vérifie les chemins de contrôle et les répertoires du magasin d’objets, y compris les shards, `info` et `pack`. Il ne parcourt pas chaque objet loose immuable : leur nombre ne consomme pas le plafond de contrôle. Les répertoires redirigés par symlink et les alternates externes restent refusés. Ce contrôle ne remplace pas une sandbox OS contre les écritures arbitraires de processus externes.

Le push et les vérifications distantes utilisent l’URL canonique de publication (HTTPS ou SSH) capturée par l’approbation, sans résoudre à nouveau un alias Git mutable pour publier. Un profil Git seul peut obtenir le statut local sans appeler GitHub ; l’accès distant est alors indisponible. Les sélections de symlinks sont refusées : la version ne peut pas être le hash du contenu d’une cible différente du blob réellement indexé. Les lectures d’approbation, de tâches et le flux SSE appliquent aussi le contrôle d’origine navigateur ; les clients locaux restent admis. Pour une demande limitée à des littéraux de fichiers, sans règles de dépôt actives et dans le contexte de fichiers explicites, le catalogue natif est réduit à la lecture et l’écriture ; les tâches comportant d’autres instructions conservent le catalogue complet, et les permissions du rôle ne sont pas élargies. La sélection effective est journalisée.
