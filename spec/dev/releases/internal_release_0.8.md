# Notes internes — release 0.8.0 (depuis v0.7.0)

Document à l’usage des **contributeurs** et de l’équipe release. Complète [tests_and_benchmarks.md](tests_and_benchmarks.md), [AGENTS.md](../AGENTS.md) et l’index [README.md](README.md). Le guide utilisateur final reste [user_guide_final.md](user_guide_final.md).

---

## Périmètre

Référence git : état du dépôt étiqueté **v0.7.0** → **0.8.0** (`[workspace.package].version`). Les thèmes majeurs : mission autonome, graphe projet multi-workspaces, profil agent (formalité), renforts orchestration / outils / plugins, qualité des réponses (contrats JSON, small talk), observabilité, release engineering, UI.

---

## Mission autonome

- **Persistance** : fichier `autonomous_mission.yaml` dans le data_dir ; tables SQLite dédiées (snapshot + **journal d’événements**) dans `akasha-store` (`AutonomousMissionStore`).
- **Boucle** : heartbeat périodique côté daemon ; première étape configurable (type d’agent, ex. chef de projet) ; orchestration existante pour la délégation.
- **API HTTP** (daemon) :
  - `GET` / `PUT /api/autonomous-mission` — lecture / fusion partielle de l’état.
  - `POST /api/autonomous-mission/pause` | `resume`.
  - `GET /api/autonomous-mission/events` — historique paginé : query `limit` (défaut 100, max 1000), `since` (RFC3339).
- **UI** : onglet **Mission** (web / desktop uniquement) : configuration, activité, alignement avec `session_id` pour le mode « sans questions » côté chat.
- **Spécification utilisateur** : [spec/user_guide.md](../spec/user_guide.md) § interface web ; détail produit à documenter dans une spec dédiée si besoin (événements, schéma YAML).

---

## Graphe projet (workspace graph)

- **Multi-workspaces** : enregistrement nom + racine absolue ; index SQLite scoping par `workspace_id` ; artefacts sous `{data_dir}/workspace_graph/out/<id>/` (`graph.json`, `GRAPH_REPORT.md`, `graph.html`).
- **API** : préfixe `/api/workspace-graph` — référence canonique [spec/54_workspace_project_knowledge_graph.md](../spec/54_workspace_project_knowledge_graph.md).
- **Agents** : injection contextuelle (jusqu’à 5 lignes, recherche sur label/path) dans le profil mémoire enrichi ; outil **`workspace_graph_search`** (≈20 lignes, `--workspace <uuid>` optionnel). Implémentations : `akasha-store`, `akasha-workspace-graph`, `api_workspace_graph.rs`.
- **UI** : Paramètres → Données → **Graphe projet** : liste, reconstruction, ouverture HTML, suppression ; affichage des résultats de recherche mémoire retravaillé (styles / lisibilité).

---

## Profil agent : formalité (`formality`)

- Champ optionnel dans `agent_profile.json` et API `GET` / `POST /api/agent-profile` : `formality` ∈ `null` | `"formal"` | `"informal"`.
- Branché sur la ligne de prompt via `agent_profile::formality_prompt_line` et consignes orchestrateur (tutoiement / vouvoiement FR, registre ailleurs).
- UI : sélecteur dans Paramètres → Profil de l’agent (clés i18n `settings.agent_profile_formality*`).

---

## Orchestration, outils et plugins

- **Plugins — sélection LLM** : plus de blocage d’outils sur toute la durée d’une tâche via les `routing_rules` des manifests ; sélection des plugins tool pertinents par une requête courte `system` + descriptions ; catalogue injecté dans le prompt ; exécution toujours filtrée par `tools_policy.yaml` uniquement.
- **Petit parleur / requêtes légères** : refactor du traitement « small talk » et génération de réponses pour réduire les allers-retours inutiles vers le LLM lourd quand c’est pertinent.
- **Suggestions de projet** : heuristiques et logs améliorés pour la détection de suggestions de workspace / projet (alignement UI graphe et agent).
- **Contrat JSON (résumés utilisateur)** : validation qu’un bloc JSON clôturé est un « contrat » significatif avant strip ; strip des blocs purement JSON en fin de message pour le résumé affiché — `strip_trailing_contract` / `is_meaningful_contract` (crate concerné selon PR : contract).
- **Plugins** : exécution des appels d’outils plugin (`execute_tool_call`) — gestion d’erreurs et chemins d’exécution consolidés (éviter états incohérents).
- **AXI** : descriptions d’outils / prompts `run_command` orientés **CLI agent-ergonomiques** (`gh-axi`, `chrome-devtools-axi`, liens [axi.md](https://axi.md/)) — optionnel côté machine utilisateur ; pas de dépendance runtime.

---

## Mémoire et relations

- Ajustements sur les **relations mémoire** et la **documentation API** associée (endpoints / comportements listés côté daemon). Voir commits « memory relation » / « API documentation » sur la branche 0.8.

---

## Observabilité

- **Journalisation agent** : logs structurés autour de la progression des tâches et de la gestion de session (diagnostic et suivi sans exposer de PII inutilement — respecter les niveaux `AKASHA_LOG`).

---

## CLI, `doctor` et résolution `spec/`

- **`akasha doctor` / `--fix`** : comportement assoupli ou corrigé pour les **installations packagées** (zip) : détection du checkout source vs binaires, dossier `spec` vide, chemins relatifs ; création d’`akasha.env` documentée.
- **`akasha paths`** et résolution **`AKASHA_SPEC_DIR`** → binaire → `data_dir/spec` → `spec` relatif : alignement avec les scripts d’installation et les archives release (voir commits « spec directory resolution »).

---

## Release engineering et CI

- **Alignement de version** : workflow GitHub « Sync release version » + script `scripts/sync-release-version.py` ; job `verify-release-version` sur les tags. Secret `VERSION_SYNC_TOKEN` si branche protégée. Décrit dans [AGENTS.md](../AGENTS.md).
- **CI** : libération d’espace disque et réduction de la taille des artefacts de debug pour éviter les OOM/OOD sur les runners ; correctifs associés.
- **UI E2E** : Playwright dans `apps/akasha-ui` (`npm run test:e2e`), serveur custom, timeouts alignés ; génération de captures sous `docs/screenshots/` (voir README AGENTS).

---

## Fichiers et crates utiles pour creuser

| Sujet | Emplacements indicatifs |
|--------|-------------------------|
| Mission autonome | `crates/akasha-daemon/src/api.rs` (routes), store mission, boucle heartbeat |
| Workspace graph | `api_workspace_graph.rs`, `akasha-store/.../workspace_graph.rs`, `akasha-workspace-graph` |
| Profil / formalité | `agent_profile.rs`, `personality.rs`, `App.tsx` (settings) |
| Strict tools-first | `api.rs` (prompt loop, tool routing) |
| Contrat / strip JSON | modules `contract` (voir historique git) |
| Plugins | `execute_tool_call`, plugin host |

---

## Maintenance de la doc utilisateur

- **Binaires** : le workflow Release copie `docs/user_guide_final.md` vers `docs/user_guide.md` dans l’archive.
- **Dépôt source** : `GET /api/docs` lit en priorité `spec/user_guide.md` — garder **cohérence** des sections communes avec `user_guide_final.md` (onglets, commandes, fichiers de config).

---

## Historique 0.7.0 (rappel)

Les nouveautés **0.7.0** (route `orchestrator`, livrables vérifiés, politique de chemins, clamping LLM, correctifs RAG Tauri, etc.) restent décrites dans [spec/user_guide.md](../spec/user_guide.md) § **10** (rappel) ; le guide final utilisateur porte l’équivalent en § **14** (0.8.0 + rappel 0.7.0).
