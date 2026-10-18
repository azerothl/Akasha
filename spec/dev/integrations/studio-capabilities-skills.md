# Capacités, règles et skills Code Studio

Suivi : #153, #154, #155 ; renforcement des rôles : #152. Le contrat des appels typés (#157), la reprise durable (#158), navigateur/PTY/MCP (#156) restent des chantiers distincts.

## Manifeste contextuel

`GET /api/tools/effective?project_id=<uuid>` décrit le prochain rôle racine. Ajouter `code_mode=plan` sélectionne le planner ; les autres modes utilisent le manager. `agent=<rôle>` permet d’examiner un spécialiste en prévisualisation.

`GET /api/tools/effective?project_id=<uuid>&task_id=<uuid>` utilise le projet de la lignée et le rôle persisté. Une tentative de les remplacer par d’autres valeurs est rejetée. Le projet doit exister et rester dans la racine Studio. La réponse sans paramètres demeure le catalogue global, avec `scope: global` ; elle ne prouve pas les capacités d’une tâche.

Le contrat version 1 contient `scope: studio`, projet, tâche, rôle, `preview`, date de calcul, outils, skills, règles et activations. Chaque outil distingue :

- `allowed` : autorisé par le rôle et la politique ;
- `runnable` : prérequis statiques/détectables présents ;
- `announced` : inclus dans le prompt ;
- sources, motifs, approbation prévue par la politique, prérequis et schéma de transport des arguments.

Le prompt et les deux voies d’exécution (boucle spéciale et dispatcher) consultent ce même calcul. Une liste vide ne réintroduit aucun outil de repli. Les arguments, chemins, décisions d’approbation et budgets restent vérifiés à l’invocation ; le préflight ne constitue pas une exécution réussie. Les schémas détaillés natifs sont suivis dans #157 ; ce lot décrit le transport historique `argv`.

Les workers en worktree conservent le contexte Studio. Les tickets de ces tâches sont rattachés au projet persisté, et non à un répertoire de tickets propre au worktree. La politique globale s’applique également à ces outils.

## Résolution des skills

Priorité croissante :

1. `spec_dir/skills` : pack livré ;
2. `data_dir/skills` : installation utilisateur Akasha ;
3. `<workspace>/.agents/skills` : skills du dépôt ;
4. `<workspace>/.akasha/skills` : adaptation Akasha du dépôt.

Le registre global est publié atomiquement après rechargement. Chaque tâche construit une copie avec les surcharges de son projet ; elle ne les expose pas aux autres projets. Aucun parcours des parents extérieurs au workspace ni chargement des skills personnels Codex/Cursor n’est effectué. Les ancêtres d’un fichier sont traités à l’intérieur du workspace pour les instructions du dépôt.

Nom et description servent à sélectionner un skill pertinent. Le corps se charge avec `TOOL: read_skill nom`. Le champ `agents` est appliqué à la découverte, la lecture et l’invocation. Les descriptions, source, version `metadata.version` et SHA-256 sont exposés par le manifeste. Un skill ne peut pas remplacer un outil builtin par une autre capacité.

`TOOL: read_skill nom references/fichier.md` lit une ressource UTF-8 relative au dossier du skill. Chemins absolus, traversal, flux alternatifs Windows et symlinks sortants sont refusés. Le fichier d’entrée est borné à 64 KiB, une ressource à 32 KiB ; le contenu d’entrée doit correspondre à son hash chargé. Si le skill change pendant la tâche, recharger/démarrer une nouvelle tâche. Les ressources ne sont jamais exécutées implicitement.

Les alias CLI héritent de `run_command`, de son périmètre d’approbation et de la politique de commande. Les rôles restreints ne peuvent pas utiliser un skill pour retrouver shell, MCP, plugins, délégation ou écritures applicatives. Les inspections de fichiers/Git de ces rôles sont bornées à leur workspace ; la recherche dans les graphes externes n’y est pas exposée.

## Instructions du dépôt

Le `AGENTS.md` racine et les fichiers Markdown de `.akasha/rules/` sont chargés avant le premier appel modèle. Lors de l’accès à un fichier, les `AGENTS.md` de ses sous-dossiers sont chargés du plus général au plus spécifique. Une mutation est différée au tour suivant lorsque de nouvelles instructions viennent d’être fournies au modèle.

La portée reste le workspace canonique, sans symlink sortant. Les lectures doivent être autorisées par la politique. Limites : 32 règles, 16 KiB par fichier, 48 KiB par résolution. Une erreur de lecture ou de persistance arrête la tâche avec `context_unavailable`. Pour une commande shell affectant plusieurs sous-arbres, l’agent doit lire leurs instructions avant d’agir : le moteur ne déduit pas les fichiers qu’un programme arbitraire modifiera.

Ces textes sont des consignes de dépôt, subordonnées à la demande et aux permissions. Ils ne constituent pas une sandbox OS et ne peuvent pas étendre les outils d’un rôle.

## Activations et pack

Les métadonnées d’activation sont conservées atomiquement sous `data_dir/studio-task-context/<task-id>.json`, séparément du petit anneau d’événements. Les lectures de skills ajoutent aussi un événement `skill_activated`. Le manifeste d’une tâche expose cet historique, y compris l’origine et les hashes des ressources lues. Les corps complets ne sont pas copiés dans cet historique.

Sept workflows sont livrés dans `spec/skills` : `studio-explore`, `studio-implement`, `studio-debug`, `studio-verify`, `studio-review`, `studio-verify-ui`, `studio-deliver`. Ils décrivent prérequis, permissions et preuves attendues. Ils n’installent ni navigateur, ni serveur de langage, ni connecteur. Le packaging Windows/Linux/macOS copie le répertoire des skills ; les scripts d’installation existants conservent ce répertoire `spec`.

## Validation de ce lot

- Compilation daemon `--no-default-features --features embedded` sous Windows.
- Validation du format des sept skills par le validateur Skill Creator ; ce contrôle ne prouve pas la qualité de leurs décisions.
- Build TypeScript/Vite de l’interface associée.
- Aucun lancement local de suite de tests, de recette modèle ou de release dans ce lot.

Restent à exécuter : deux projets avec skills homonymes, priorité utilisateur/pack/projet, champ agents refusé, ressource sortante et source modifiée, AGENTS imbriqué avant mutation, annulation/reprise d’une tâche, manifeste task/project falsifié, outils annoncés/acceptés cohérents, packaging installé. Les tickets demeurent ouverts pendant revue et validation.

Un profil explicite doit lister les outils Kanban requis (studio_list_tickets, studio_create_ticket, studio_update_ticket). Leur ancien passe-droit de dispatch ne contourne plus le contrat commun : aucun outil ne peut élargir une liste explicite, et une liste vide refuse tout. Le manifeste explique le refus ; adapter le profil pour les workflows PM/reviewer qui utilisent Kanban.
