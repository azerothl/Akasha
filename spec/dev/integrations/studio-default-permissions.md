# Accès par défaut aux projets Code Studio

## Incident corrigé

Une configuration `tools_policy.yaml` ne contenant que des options web produisait des listes de chemins et de commandes vides. L’agent recevait bien sa racine de projet, mais ses outils refusaient cette même racine (`cached/current path denied by current policy`). Il pouvait alors demander à tort un nouveau chemin absolu à l’utilisateur.

Le daemon conserve les **champs absents** sans accorder de droits à son exécuteur global. Une copie de la politique reçoit les valeurs Studio uniquement après résolution de l’identité persistée de la tâche et de son projet ou worktree. Cette copie est recalculée depuis la politique courante avant les appels d’outils et les vérifications. Le chargeur générique `akasha-tools::ToolsPolicy` et sa valeur `Default` restent en refus par défaut; une métadonnée interne non sérialisée conserve les omissions lors de la sauvegarde de la configuration.

| Champ absent | Valeur du daemon |
| --- | --- |
| `allowed_read_paths` | Racine enregistrée du seul projet/worktree de la tâche |
| `allowed_write_paths` | La même racine de tâche |
| `allowed_commands` | `cargo`, `npm`, `npx`, `node`, `git`, `python`, `python3`, `uv` |
| `command_timeout_secs` | 60 secondes |
| `run_command_default_cwd_workspace` | `true` : répertoire de la tâche si aucun `cwd` n’est fourni |

Une configuration absente, vide ou constituée seulement de commentaires reçoit ces valeurs uniquement pour une tâche Studio. Une conversation hors Studio ne reçoit ni accès aux projets ni commandes supplémentaires. GET /api/tools/policy et les sauvegardes préservent les champs omis; GET /api/tools/effective?project_id=... expose les droits dérivés du projet. Une erreur de lecture autre qu’un fichier absent, un YAML invalide ou une valeur racine autre qu’un objet ne déclenche pas de droits de secours. Le daemon doit pouvoir résoudre son répertoire de données.

## Restrictions conservées

- Une liste explicite, y compris `[]`, est prioritaire. `allowed_write_paths: []` continue d’interdire les écritures, y compris après reprise ou rechargement.
- Les commandes bloquées, profils d’outils, approbations et politiques des rôles restent applicables. Un audit n’obtient pas de droit de mutation.
- Ces valeurs ne donnent pas accès aux fichiers de configuration, à la base du daemon ou à l’ensemble du disque.
- Le contrat d’outil continue de confiner chaque tâche à son projet ou son worktree enregistré, avec validation des chemins et des versions de fichiers. Les valeurs héritées elles-mêmes ne couvrent aucun projet voisin. L’exécuteur global ne reçoit jamais ces valeurs.
- L’autorisation d’un exécutable ne garantit pas sa présence sur la machine. Les processus conservent les permissions du compte qui exécute le daemon; ceci n’ajoute pas une sandbox OS.
- Les refus d’accès aux chemins précisent désormais le champ de politique concerné et rappellent que le workspace est déjà connu. Ils ne demandent pas un chemin de remplacement.

## Validation

Les tests unitaires de `studio_policy` couvrent la configuration web seule, les racines gérées, l’absence d’accès aux autres dossiers, les refus explicites, les commandes bloquées et les profils, le round-trip API, les fichiers absents/vides et les données invalides.

La recette Code Studio `integration/agentic-real.mjs` contient un parcours dédié : écriture d’une configuration web seule, redémarrage isolé du daemon, lecture de CODE_STUDIO_PLAN.md, création d’un fichier et commande Git sans `cwd` fourni, puis restauration de la configuration de recette. Les scénarios existants de révocation et de reprise restent exécutés dans la même campagne.

Pour une inférence Ollama isolée avec la même configuration partielle, définir `CODE_STUDIO_TEST_DEFAULT_POLICY=1` puis lancer `npm run test:daemon:goal` dans Code Studio avec `CODE_STUDIO_DAEMON_BIN` et `CODE_STUDIO_REAL_MODEL`. Le rapport distingue cette configuration des chemins explicitement autorisés de la recette originale. Ces recettes n’utilisent pas le répertoire de données de l’utilisateur.

La recette reproduit aussi une conversation hors Studio qui tente de lire un secret du projet, écrire un fichier et lancer Node : aucune donnée privée ne rejoint le modèle et aucun fichier n’est créé. La revue P1 sur la portée globale est couverte par ce parcours.
