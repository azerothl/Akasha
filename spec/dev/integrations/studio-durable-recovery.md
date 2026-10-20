# Reprise durable Studio — #158

Cette tranche prolonge le journal d’opérations de #157. Les checkpoints SQLite version 1 conservent tâche/projet/workspace/rôle, session, routage, incognito, tour, contexte de boucle, résultats de vérification et attente humaine. Le prompt est borné à 2 Mio ; un échec de sauvegarde arrête la progression avant le modèle suivant. Sauvegarde avant modèle, après outils et avant une attente humaine. Les arguments sensibles ne sont pas copiés dans le journal d’opérations (digest uniquement) ; le contexte privé de la tâche peut contenir des fichiers ou sorties déjà lus.

Au démarrage, les exécutions et attentes humaines qui ont perdu leur processus passent à `interrupted`. Les reprises Studio exigent un checkpoint. Le bouton/API de reprise conserve la session et le budget de tours ; la boucle recharge les politiques, règles et skills courants. Le workspace enregistré doit correspondre au projet ou au worktree persistant validé. Si le worktree a disparu, la reprise échoue : aucun repli sur le dossier principal. Les états arrêtés ne sont pas remplacés par `running` lors d’une course avec l’annulation. Une exécution encore active doit finir de s’arrêter avant d’être reprise.

Les opérations `started` sans résultat restent inconnues. Les outils de lecture permettent l’inspection, mais les nouvelles mutations et commandes sont bloquées. Les délégations et mutations de workflow sont également journalisées sans garder le verrou du workspace pendant l’exécution d’un enfant. Les opérations terminées ne sont pas rejouées par le mécanisme de reprise. Une génération de modèle peut proposer une nouvelle opération ; les versions de fichiers, permissions et états inconnus continuent de la contraindre.

## Opérateur

- `GET /api/tasks/:id/recovery` : métadonnées de checkpoint, activité courante, enfants, journal récent et nombre total d’opérations inconnues. Le prompt complet n’est pas exposé.
- Après inspection et arrêt effectif : `POST /api/tasks/:id/recovery/operations/:operation/resolve` avec `{"decision":"applied"|"not_applied","note":"constat humain de 8 à 2000 caractères"}`.
- Cette décision est persistée avec l’opération et n’exécute rien. Elle ne restaure pas l’ancienne approbation. Reprendre ensuite explicitement via l’action habituelle.
- Une question ou approbation humaine interrompue est présentée comme attente historique ; le modèle doit la poser à nouveau avec les permissions courantes. Les anciens canaux oneshot ne sont pas réutilisés.

Les sessions Playwright/PTY et images en mémoire ne sont pas restaurées. Les fichiers/preuves persistés restent consultables. Le pipeline orchestré possède son propre checkpoint ; une racine qui n’est jamais passée dans la boucle Studio ne devient pas reprenable par ce module. DAP et reprise automatique sans intervention ne sont pas livrés.

## Validation observée et recettes pour #143

Compilation Windows `cargo check -p akasha-daemon --no-default-features --features embedded`. Aucun arrêt/redémarrage réel ni test local exécuté pour cette tranche. Les recettes suivantes sont des critères à exécuter, pas des résultats acquis :

| Recette | Point d’arrêt | Résultat attendu |
| --- | --- | --- |
| REC-01 | Avant l’outil | Checkpoint disponible, aucun effet supposé |
| REC-02 | Effet écrit avant enregistrement du résultat | Opération inconnue, mutations bloquées, inspection puis décision explicite |
| REC-03 | Après résultat, avant modèle suivant | Résultat visible dans le journal ; aucune répétition automatique |
| REC-04 | Question/approbation en attente | Demande historique visible ; nouvelle demande obligatoire |
| REC-05 | Worktree déplacé/supprimé | Refus de reprise, aucun changement dans le projet principal |
| REC-06 | Annulation pendant préparation de reprise | Transition conditionnelle refusée, annulation conservée |
| REC-07 | Pause pendant commande longue | Reprise refusée tant que l’ancien processus de boucle est actif |
| REC-08 | Politique modifiée pendant arrêt | Nouveau refus appliqué au prochain appel |
| REC-09 | Délégation interrompue | Enfants conservés et visibles, effet de délégation à inspecter |
