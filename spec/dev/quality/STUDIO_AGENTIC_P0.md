# Code Studio — terminaison et rôles restreints

> Mise à jour après validation : voir [les parcours réels du 18 octobre](STUDIO_AGENTIC_VALIDATION_2026_10_18.md). Les mentions « aucun test » ci-dessous décrivent le lot initial, avant cette validation.

Travail associé : [#151](https://github.com/azerothl/Akasha/issues/151) et [#152](https://github.com/azerothl/Akasha/issues/152). Audit initial : Code Studio `7ead866`, daemon `4f3ee5f`.

## Contrat de résultat

La boucle LLM persiste ensemble le statut et `outcome_json` avant d’annoncer l’événement terminal. `GET /api/tasks/:id` et `/report` exposent `outcome`. Les tâches historiques n’ont pas de résultat structuré ; aucune réussite vérifiée ne doit en être déduite.

Le contrat version 1 contient :

- `termination_reason` : `success`, `model_empty`, `tool_loop`, `budget_exhausted`, `provider_error`, `timeout`, `tool_round_limit`, `verification_failed` ; les arrêts préexistants conservent leur statut/motif (`failed`, `paused`, `cancelled`, `interrupted`, `waiting_user_input`).
- `result` : `succeeded` signifie que la boucle s’est terminée normalement ; `partial` signifie qu’elle a échoué après des appels réussis ; `failed` ou `stopped` distinguent les autres cas. Un appel réussi peut être une lecture : ce champ ne garantit pas qu’un fichier a été produit.
- `verification` : résultats du build post-tâche, critères mécaniques/manuels et commandes de la boucle. `not_run` n’est pas un succès. Les codes de sortie non disponibles dans le contrat actuel de dispatch restent `null` ; #157 doit compléter ce contrat.
- `acceptance_verified` : preuve conservative des critères configurés, indépendante d’une simple réponse modèle. Une vérification manuelle ou non confirmée ne vaut jamais validation automatique.
- `successful_tool_calls` / `failed_tool_calls` : compteurs de la boucle, pas un pourcentage de satisfaction métier.

Réponse vide, limite, boucle, timeout et erreur provider ne produisent plus de `TaskCompleted`. Un build Vite de secours ne transforme plus l’échec du build configuré comprenant TypeScript en réussite. Les hooks, l’apprentissage et le passage d’un ticket en revue utilisent le statut final réellement persisté. Une synthèse lisible ne masque plus l’échec d’une sous-tâche orchestrée. Les résultats et diffs partiels restent consultables.

Le stockage transactionnel préserve les échecs de watchdog et les arrêts utilisateur qui précèdent la finalisation. Un échec de persistance interrompt l’annonce de réussite. Cela ne constitue pas une reprise durable des opérations après crash (#158).

## Contrat planner/reviewer

La même liste fermée de capacités est utilisée pour les outils annoncés et les invocations, avant les branches spéciales de la boucle et avant le dispatch générique. Les alias de skills sont résolus avant cette vérification ; les skills documentaires avec arguments ne peuvent pas servir de voie CLI détournée.

- `studio_reviewer` : exploration locale, outils Git de lecture, consultation de skills, interaction utilisateur, métadonnées todos/tickets et retour de revue. Shell, processus, conteneurs, navigateur, MCP, plugins, installation, délégation et modifications du dépôt sont refusés.
- `studio_planner` : mêmes lectures et métadonnées de planification ; seule exception disque : `write_file workspace:/CODE_STUDIO_PLAN.md` (ou son nom relatif exact). Autres outils d’édition, chemins absolus, traversals et chemins descendants sont refusés.
- L’écriture du plan vérifie la racine canonique et refuse symlink/dossier ; elle remplace le fichier via un fichier temporaire dans le même dossier, pour ne pas tronquer une cible de lien physique. La politique générale doit aussi autoriser cette écriture.
- Aucun script de build, critère `command_ok` ou autofix n’est exécuté automatiquement après un rôle restreint. Une tâche QA/implémentation distincte doit porter ces contrôles.
- Les outils Git de lecture désactivent fsmonitor, hooks, verrous facultatifs, diff externe et textconv. Les pathspecs du diff sont séparés des options par `--`.

Il s’agit d’un contrôle des capacités au niveau du daemon, pas d’une sandbox système. Les agents d’implémentation conservent leurs capacités autorisées ; les exécutables disponibles et les politiques système restent des prérequis. La frontière canonique et la concurrence des worktrees restent suivies dans #145.

## Vérifications effectuées le 18 octobre 2026

- `cargo check -p akasha-daemon --no-default-features --features embedded` : réussi sous Windows ; avertissements préexistants conservés.
- Le profil sans aucune feature ne compile pas déjà à cause de `embedded_apply_runtime` dans `daemon.rs` ; il n’est pas présenté comme une validation réussie. La vérification a utilisé le profil `embedded` disponible.
- Relecture du chemin de dispatch, des branches spéciales, de la finalisation et du build de secours. Aucune suite de tests ni recette modèle supplémentaire exécutée dans ce lot.

## Scénarios de validation restant à exécuter

1. Modèle vide après zéro outil puis après une écriture : statut failed, motif model_empty, diff conservé ; aucun événement ou hook de réussite.
2. Répétition d’outil, quota, timeout et provider en erreur : motif exact, résultat partiel si des étapes ont réussi, DB/API/événements cohérents.
3. Échec de build TypeScript avec bundle Vite réussi : tâche en échec tant que le build configuré échoue.
4. Annulation, pause et échec de watchdog concurrents avec la fin de boucle : ne jamais réécrire completed.
5. Sous-tâche échouée et synthèse non vide : racine en échec, synthèse et sous-résultats conservés.
6. Reviewer et planner : commandes directes/arrière-plan, conteneurs, process, MCP, plugin, délégation et alias CLI refusés sans effet de bord ; lectures autorisées.
7. Planner : plan exact autorisé selon politique ; source applicative, chemin absolu, traversal, symlink/dossier refusés ; lien physique remplacé sans modifier l’autre lien.
8. Dépôt contenant un fsmonitor, un diff externe ou textconv : inspection Git sans invocation de ces extensions.
9. Projet muni d’un script build qui écrit un marqueur : une tâche reviewer/planner ne lance ni ce script ni les critères command_ok ou autofix.

Les tickets restent ouverts tant que la correction n’est pas intégrée et ses critères de validation confirmés.
