# Navigation sémantique — #159

> Mise à jour après validation : voir [les parcours réels du 18 octobre](../quality/STUDIO_AGENTIC_VALIDATION_2026_10_18.md). Les mentions « aucun test » ci-dessous décrivent le lot initial, avant cette validation.

Première tranche LSP stdio : TypeScript/JavaScript (`typescript-language-server --stdio`), Rust (`rust-analyzer`) et Python (`pyright-langserver --stdio`). Les exécutables doivent être préinstallés sur le PATH et autorisés par `allowed_commands` et les outils du profil. Rien n’est téléchargé. Le manifeste distingue ce prérequis d’un démarrage réussi ; `launch_checked:false` dans le statut est explicite. Les capacités sont négociées à l’initialisation et contrôlées à chaque opération.

## Contrats

- `lsp_query` reçoit un objet JSON `action`, `path`, `position:{line,character}` (UTF-16, base zéro), éventuellement `new_name` et `text` pour un buffer courant. Actions : `diagnostics`, `definition`, `references`, `rename`.
- `lsp_apply_rename <plan_uuid>` applique un plan de cette tâche après inspection. Approbation héritée de `write_file` ; interdit aux rôles planner/reviewer. Le lancement de serveur via `lsp_query` est également exclu de ces deux rôles, car un serveur peut lancer des outils du projet.
- API projet : `GET /api/studio/projects/:id/lsp/status`, `POST .../query`, `POST .../rename/:uuid/apply`, `DELETE .../sessions`. Les plans utilisateur et agent sont distincts et liés au workspace.
- Résultats structurés avec serveur, langage, version de document, empreinte du texte et statut. Un diagnostic `pending` ne veut pas dire « zéro erreur ». Définition/références hors workspace sont exclues et comptées ; les chemins lus/modifiés doivent respecter la politique.

Client JSON-RPC avec framing `Content-Length`, taille et délais bornés, requêtes serveur traitées sans autoriser `workspace/applyEdit`. Un lecteur indépendant conserve les trames partielles pendant une attente de diagnostic. Synchronisation des buffers et versions de diagnostics, positions UTF-16 contrôlées. 12 sessions maximum, 128 documents par session, expiration après 15 minutes d’inactivité, fermeture explicite ou à la suppression du projet. Une erreur de transport exige un redémarrage de session au prochain appel.

Le renommage retourne d’abord un plan avant/après (32 fichiers, 1000 éditions par fichier, 4 Mio de texte, durée 2 heures). Les buffers concernés doivent être enregistrés. Éditions superposées, versions périmées et opérations de ressources sont refusées. Le plan agent est également archivé comme preuve JSON. L’application revalide chaque chemin/version et sérialise les opérations du workspace. Chaque fichier est remplacé via temporaire ; en cas d’échec, un rollback est tenté seulement si le contenu est encore celui écrit par cette application. Les éventuels fichiers non restaurés sont signalés. Ce n’est pas une transaction atomique avec les éditeurs externes.

Le serveur est un processus autorisé, pas une sandbox OS. Rust Analyzer reçoit les options désactivant build scripts et macros procédurales ; leur comportement dépend de sa version. Les bibliothèques et outils internes du serveur restent de sa responsabilité. Les téléchargements/compilations doivent être explicitement configurés par l’opérateur. DAP, complétion et code actions ne sont pas livrés par cette tranche.

## Validation

`cargo check -p akasha-daemon --no-default-features --features embedded` : compilation Windows. Pas de serveur LSP réellement lancé ni de tests locaux. À valider avant clôture : installation manquante/refusée, diagnostics push/pull différés, F12/références, CRLF/Unicode UTF-16, renommage multi-fichier, plan périmé, chemin externe/symlink, refus planner/reviewer, échec partiel et fermeture.

Références primaires : [LSP 3.17](https://github.com/microsoft/language-server-protocol/blob/gh-pages/_specifications/lsp/3.17/specification.md), [rename](https://github.com/microsoft/language-server-protocol/blob/gh-pages/_specifications/lsp/3.17/language/rename.md), [pull diagnostics](https://github.com/microsoft/language-server-protocol/blob/gh-pages/_specifications/lsp/3.17/language/pullDiagnostics.md).

Configuration et limites de confiance Rust Analyzer : <https://rust-analyzer.github.io/book/configuration>, <https://rust-analyzer.github.io/book/security.html>.

Une prévisualisation de renommage agent de plus de 48 Kio est refusée avant publication du plan pour ne pas être tronquée par le résultat outil. Préparer et revoir ce renommage dans l'éditeur Studio ; aucune application agent à l'aveugle. Les versions LSP sont comparées aux documents ouverts ; les autres fichiers restent bornés par leur contenu disque et leur hash attendu.
