# Validation réelle Studio — 18 octobre 2026

Les validations postérieures à l'implémentation initiale des PR #162/#164/#166–#170 sont publiées dans [le rapport Code Studio](https://github.com/azerothl/Akasha-code-studio/blob/codex/studio-semantic-delivery/docs/validation/agentic-real-2026-10-18.md), avec le script reproductible `integration/agentic-real.mjs`, les rapports compacts et les captures.

## Résultats observés

- Windows CPU, profil daemon `--no-default-features --features embedded` : build réussi ; suite daemon et bibliothèques LLM/store/tools exécutées. Les nombres finaux figurent dans le rapport lié.
- Parcours daemon réel : filesystem/Git/SQLite, restrictions planner/reviewer, résultats structurés, appels natifs Unicode, lecture des règles imbriquées et activation de skill, PTY utilisateur et agent, processus MCP stdio avec découverte paginée, navigateur Chromium et artefacts.
- Serveurs LSP réels TypeScript/Python/Rust : diagnostics/navigation, références et renommage TypeScript ; aucun build.rs exécuté par rust-analyzer. Windows : comparaison des URI par fichier canonique.
- Livraison via API : commit sélectionné préservant l'index sans rapport, idempotence ; vrai push/PR brouillon/statut GitHub, puis fermeture du brouillon de test [Code Studio #36](https://github.com/azerothl/Akasha-code-studio/pull/36) sans fusion et suppression de sa branche.
- Arrêts/redémarrages physiques REC-01/02/03 : checkpoint explicite, conservation de session, résultat inconnu bloquant les mutations jusqu'à inspection, absence de rejeu d'un effet enregistré.
- Interface réelle : renommage par formulaire, résultats/capacités/preuves/reprise, bureau/mobile, absence de base64 PNG dans le texte du chat.

Le modèle de `agentic-real.mjs` est une **fixture déterministe de protocole**, distincte du moteur réel d'exécution et des essais Ollama. Les résultats Ollama et les échecs de modèles restent explicitement séparés dans le rapport. Un arrêt `tool_loop` après une écriture correcte conserve un résultat partiel ; il ne devient pas une réussite de tâche.

## Corrections issues des parcours

- Réponse vide arrêtée avant les relances PM/promise.
- Écriture Studio appliquée au disque avec politique sur le chemin résolu ; plus de succès mémoire seul.
- Résultats d'outils utilisés comme état courant après exécution, sans réinjecter la proposition d'appel précédente comme nouveau travail.
- URI Windows LSP canonique, captures navigateur dans le lecteur d'artefacts.
- Bouton de renommage UI réellement `submit` (dans Code Studio).

## Limites et tickets conservés

**#158 reste ouvert** : REC-04 à REC-09 doivent encore être exécutés par arrêt réel (attente humaine, worktree disparu, annulation/pause concurrentes, politique changée, délégation interrompue). Les tests unitaires ne remplacent pas ces recettes. Les racines de pipeline sans checkpoint Studio et les sessions PTY/navigateur/images en mémoire ne sont pas restaurées.

**#143** conserve les évaluations transverses et **#145** les frontières worktree. **#179** suit la compatibilité de conclusion des modèles Ollama observée pendant cette validation.

Aucun runner GPU n'est configuré : CUDA non exécuté. L'utilisateur a explicitement autorisé la fusion après contrôles CPU et parcours réels ; une CI GPU en attente n'est pas une validation GPU. Les contrôles CPU GitHub doivent être vérifiés sur les SHA publiés avant fusion.

LSP exige des serveurs préinstallés et autorisés (TypeScript 5.6.3 utilisé dans le parcours), MCP est testé en stdio, livraison sur GitHub.com, aucun DAP ni sandbox système supplémentaire livré.

## Corrections de revue et seconde validation

394 tests daemon réussis (1 test PTY historique ignoré), 101 tests des bibliothèques et 23 parcours réels après traitement des revues automatiques. Les nouveaux scénarios couvrent les critères d'acceptation indépendants, l'absence d'accès PTY depuis les routes globales, le profil MCP et son budget après erreur de schéma, le retour à une URL autorisée, les destinations avec espaces, les fichiers ignorés dans un commit et une attente humaine au plafond de tours suivie d'un crash/reprise.

Correctifs supplémentaires : rollback conditionnel du résultat lors d'une reprise en file d'attente, garde d'activité jusqu'à la fin de l'agrégation, résultat structuré en timeout, absence de croissance récursive du contexte de reprise, diagnostic de démarrage impossible, filtrage des appels JSON dans le raisonnement, contrats mixtes legacy/JSON, erreurs LSP applicatives sans tuer le serveur, synchronisation incrémentale et diagnostics en attente dans l'enveloppe normale. Les règles/skills du manifeste utilisent le worktree persisté de la tâche ; les profils explicites restent stricts, y compris Kanban.

La recette avec **Devstral via l'API OpenAI compatible d'Ollama** réussit sur le binaire corrigé (une écriture, contenu exact et diff vérifiés). Qwen a réussi un premier essai puis bouclé lors d'une répétition ; Qwen et LFM2 via le transport texte Ollama ne sont pas considérés fiables. #179 reste ouvert pour ces compatibilités et aucun échec modèle n'est effacé du rapport.
