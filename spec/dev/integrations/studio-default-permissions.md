# Accès par défaut aux projets Code Studio

## Incident corrigé

Une configuration `tools_policy.yaml` ne contenant que des options web produisait des listes de chemins et de commandes vides. L’agent recevait bien sa racine de projet, mais ses outils refusaient cette même racine (`cached/current path denied by current policy`). Il pouvait alors demander à tort un nouveau chemin absolu à l’utilisateur.

Le daemon applique désormais des valeurs par défaut aux **champs absents**, lors du démarrage, du rechargement et des échanges de politique via l’API. Le chargeur générique `akasha-tools::ToolsPolicy` et sa valeur `Default` restent inchangés.

| Champ absent | Valeur du daemon |
| --- | --- |
| `allowed_read_paths` | `<data_dir>/studio-projects`, `<data_dir>/studio-worktrees` |
| `allowed_write_paths` | Les mêmes dossiers gérés |
| `allowed_commands` | `cargo`, `npm`, `npx`, `node`, `git`, `python`, `python3`, `uv` |
| `command_timeout_secs` | 60 secondes |
| `run_command_default_cwd_workspace` | `true` : répertoire de la tâche si aucun `cwd` n’est fourni |

Une configuration absente, vide ou constituée seulement de commentaires reçoit ces valeurs. Une erreur de lecture autre qu’un fichier absent, un YAML invalide ou une valeur racine autre qu’un objet ne déclenche pas de droits de secours. Le daemon doit pouvoir résoudre son répertoire de données.

## Restrictions conservées

- Une liste explicite, y compris `[]`, est prioritaire. `allowed_write_paths: []` continue d’interdire les écritures, y compris après reprise ou rechargement.
- Les commandes bloquées, profils d’outils, approbations et politiques des rôles restent applicables. Un audit n’obtient pas de droit de mutation.
- Ces valeurs ne donnent pas accès aux fichiers de configuration, à la base du daemon ou à l’ensemble du disque.
- Le contrat d’outil continue de confiner chaque tâche à son projet ou son worktree enregistré, avec validation des chemins et des versions de fichiers. L’accès à un dossier géré ne permet pas de sortir du workspace de la tâche.
- L’autorisation d’un exécutable ne garantit pas sa présence sur la machine. Les processus conservent les permissions du compte qui exécute le daemon; ceci n’ajoute pas une sandbox OS.
- Les refus d’accès aux chemins précisent désormais le champ de politique concerné et rappellent que le workspace est déjà connu. Ils ne demandent pas un chemin de remplacement.

## Validation

Les tests unitaires de `studio_policy` couvrent la configuration web seule, les racines gérées, l’absence d’accès aux autres dossiers, les refus explicites, les commandes bloquées et les profils, le round-trip API, les fichiers absents/vides et les données invalides.

La recette Code Studio `integration/agentic-real.mjs` contient un parcours dédié : écriture d’une configuration web seule, redémarrage isolé du daemon, lecture de CODE_STUDIO_PLAN.md, création d’un fichier et commande Git sans `cwd` fourni, puis restauration de la configuration de recette. Les scénarios existants de révocation et de reprise restent exécutés dans la même campagne.

Pour une inférence Ollama isolée avec la même configuration partielle, définir `CODE_STUDIO_TEST_DEFAULT_POLICY=1` puis lancer `npm run test:daemon:goal` dans Code Studio avec `CODE_STUDIO_DAEMON_BIN` et `CODE_STUDIO_REAL_MODEL`. Le rapport distingue cette configuration des chemins explicitement autorisés de la recette originale. Ces recettes n’utilisent pas le répertoire de données de l’utilisateur.
