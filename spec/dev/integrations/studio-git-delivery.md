# Livraison Git / GitHub / CI — #160

Première tranche Git et GitHub.com via Git et `gh` préinstallés, autorisés et authentifiés. Les API GitHub Enterprise/GitLab ne sont pas prises en charge. Le profil doit autoriser explicitement les nouveaux outils lorsqu’il utilise une liste restrictive.

## Outils et API

- `git_delivery_status {remote:"origin"}` : fichiers, versions SHA-256, HEAD/branche, identité Git, remote, lecture de la branche distante et accès GitHub. Une erreur d’accès reste distincte d’une authentification confirmée.
- `git_commit {paths:[...],expected_versions:{path:sha256|"missing"},expected_head,message}` : sélection explicite de 1 à 128 fichiers. HEAD et versions sont revalidés.
- `git_push {remote,branch,expected_head,expected_remote_head}` : publie uniquement ce SHA vers cette branche. Aucun force, tag ou sous-module publié implicitement.
- `github_create_pr {remote,head,base,expected_head,title,body,draft:true}` : exige un push préalable du même commit. Recherche d’une PR ouverte existante, puis POST GitHub structuré ; aucun fork/push/merge implicite. Une réponse incertaine est recherchée avant de signaler le résultat inconnu.
- `github_pr_status {remote,number?}` : PR et contrôles, commit exact, date de lecture. `not_run` (aucun contrôle remonté), `pending`, `failed`, `passed`, `passed_with_skipped_or_neutral`, `unknown` et `stale_for_local_head` sont distincts. Ne lance aucun workflow CI.

Enveloppes Studio v1 natives ou `TOOL_JSON:` ; les `TOOL:` historiques préservent leur objet JSON. `git_push` et `github_create_pr` exigent une approbation dans le mode interactif. Le mode global `allow_all` reste une décision d’autonomie de l’opérateur. Les approbations persistantes lient le workspace et les arguments exacts ; les descriptions de publication ne recopient pas le corps de la PR. `git_commit` hérite aussi de l’approbation de `write_file`.

API utilisateur : `GET /api/studio/projects/:id/delivery/status?remote=origin`, `GET .../pr?remote=origin&number=...&task_id=...`, puis `POST .../commit`, `.../push`, `.../pr` avec `{task_id,operation_id,request}`. La tâche doit appartenir au projet et être arrêtée ; chaque bouton est une autorisation explicite de cette action. L’identifiant permet de retrouver le résultat sans effet supplémentaire. Les opérations inconnues exigent une inspection/résolution ; aucun retry automatique.

`GET /api/tasks/:id/delivery` retourne l’historique récent et l’activité. Les actions et consultations CI liées sont également enregistrées comme événements et preuves JSON. Une erreur d’archivage est affichée séparément de l’effet Git constaté.

## Préservation du travail

Le commit utilise un index temporaire construit depuis HEAD, stage uniquement les fichiers sélectionnés, écrit un objet commit, puis met à jour la référence de branche avec comparaison de l’ancien SHA. Les changements sans rapport déjà présents dans l’index ne sont pas inclus. Après le commit, seuls les chemins sélectionnés sont réconciliés dans l’index réel si celui-ci et le checkout n’ont pas changé entre-temps. Sinon le résultat indique `committed_index_reconciliation_needed` avec le SHA réellement créé ; le travail concurrent n’est pas écrasé. Les répertoires/sous-modules, chemins externes et noms non UTF-8 ne sont pas acceptés par cette sélection. Un renommage apparaît avec sa source et sa cible : choisir les deux pour le livrer en entier.

Les règles de répertoire sont chargées avant un commit agent ou l’application d’un renommage LSP, avec report de la mutation si des instructions nouvelles viennent d’être injectées. Les effets de publication inconnus arrêtent la boucle sans succès annoncé.

Git tourne sans hooks, signature interactive, fsmonitor ou diff externe pour cette opération. Les filtres et assistants de credentials configurés restent des programmes du dépôt/utilisateur ; aucune sandbox OS n’est revendiquée. La métadonnée Git doit résider dans le répertoire de données du daemon, ce qui couvre ses projets et worktrees. Le flux UI porte sur le projet sélectionné ; l’intégration préalable d’un worktree utilise l’action existante.

## Validation et acceptation restante

Compilation Windows : `cargo check -p akasha-daemon --no-default-features --features embedded`. Build TypeScript/Vite de l’interface. Aucun commit, push, création de PR ou scénario distant exécuté via ces nouvelles API, aucun test local ajouté/exécuté.

À valider avant clôture : index contenant des changements sans rapport, fichier modifié pendant staging, ref concurrente, index concurrent après commit, dépôt sans premier commit, refus d’identité/credentials/droits, branche distante modifiée, timeout après effet, PR déjà existante et réponse incertaine, CI absente/en cours/échouée/ignorée et SHA périmé. Ces scénarios complètent #143 ; le ticket reste ouvert.

Références : [Git commit](https://git-scm.com/docs/git-commit), [Git push](https://git-scm.com/docs/git-push), [GitHub CLI create](https://cli.github.com/manual/gh_pr_create). Le POST GitHub explicite évite les comportements de fork/push de l’assistant interactif CLI.
