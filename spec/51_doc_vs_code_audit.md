# Audit documentation vs code

Comparaison de la documentation (README, spec, user_guide) avec l’implémentation réelle. Dernière mise à jour : avril 2026.

---

## 1. Structure du projet (crates)

| Crate documenté (README) | Présent dans le workspace | Remarque |
|-------------------------|---------------------------|----------|
| akasha-core | ✅ | |
| akasha-store | ✅ | |
| akasha-vault | ✅ | |
| akasha-tools | ✅ | |
| akasha-plugin-api | ✅ | |
| akasha-plugin-host | ✅ | |
| akasha-llm | ✅ | |
| akasha-rag | ✅ | |
| akasha-cluster | ✅ | |
| akasha-daemon | ✅ | |
| akasha-cli | ✅ | |
| akasha-tui | ✅ | Membre du workspace |
| akasha-evals | ✅ | |

Tous les crates listés dans le README sont bien présents dans `Cargo.toml` (workspace members).

---

## 2. Commandes CLI

| Commande documentée | Implémentée (akasha-cli) | Remarque |
|---------------------|---------------------------|----------|
| `akasha start` | ✅ | |
| `akasha start --foreground` | ✅ | |
| `akasha stop` | ✅ | |
| `akasha doctor` | ✅ | |
| `akasha doctor --json` | ✅ | |
| `akasha doctor --advice` | ✅ | |
| `akasha doctor --fix` | ✅ | Crée data_dir, fichiers minimaux si absents |
| `akasha init` | ✅ | |
| `akasha init --defaults` | ✅ | |
| `akasha vault list/set/get/delete` | ✅ | |
| `akasha config models fetch` | ✅ | |
| `akasha config models add MODEL [--ollama-url URL]` | ✅ | `--ollama-url` en long uniquement |
| `akasha config models get` / `routes` / `set …` | ✅ | |
| `akasha config env list/get/set` | ✅ | |
| `akasha config provider list` / `set-ollama` / `add-openai` / `add-openrouter` | ✅ | |
| `akasha config paths` | ✅ | Alias des chemins (voir aussi `akasha paths`) |
| `akasha router metrics` | ✅ | |
| `akasha router discover` | ✅ | |
| `akasha router show MODEL` | ✅ | Nécessite le daemon (appel API) |
| `akasha plugin list/reload/install/uninstall/catalog` | ✅ | |
| `akasha tui` | ✅ | |
| `akasha paths` | ✅ | Affiche data_dir et chemins des fichiers de config |
| `akasha update check` / `install` | ✅ | API showcase (`AKASHA_APP_BASE_URL` ou défaut GitHub Pages) |
| `akasha services install/status/stop` | ✅ | Docker compose (akasha-models) |

Les guides utilisateur (`docs/user_guide_final.md`, `spec/user_guide.md`) doivent rester alignés sur cette liste pour les utilisateurs.

---

## 3. Variables d’environnement

### Documentées dans README / guides (principales)

| Variable documentée | Utilisée dans le code | Fichier / usage |
|---------------------|------------------------|------------------|
| `AKASHA_PORT` | ✅ | daemon (port HTTP), CLI, TUI |
| `AKASHA_DATA_DIR` | ✅ | daemon, CLI (`akasha_data_dir`) |
| `AKASHA_LOG` | ✅ | daemon main, CLI (daemon_env) |
| `AKASHA_MAX_RESPONSE_TOKENS` | ✅ | api.rs (réponses chat) |
| `AKASHA_MAX_CONTEXT_TOKENS` | ✅ | api.rs (compaction mémoire court terme) |
| `OLLAMA_HOST` | ✅ | daemon (config Ollama), CLI |
| `AKASHA_SLACK_ENABLED` | ✅ | daemon |
| `AKASHA_DISCORD_ENABLED` | ✅ | daemon |
| `AKASHA_TELEGRAM_ENABLED` | ✅ | daemon |
| `AKASHA_TELEGRAM_NOTIFY_CHAT_ID` | ✅ | daemon |
| `AKASHA_TEAMS_ENABLED` | ✅ | daemon (canal Teams) |
| `AKASHA_DEGRADED_MODE` | ✅ | daemon |
| `AKASHA_CLUSTER_ENABLED` | ✅ | daemon |
| `NATS_URL` | ✅ | akasha-cluster, CLI pass-through |
| `AKASHA_NODE_ID` | ✅ | akasha-cluster |
| `AKASHA_NATS_TLS_CA` / `_CLIENT_CERT` / `_CLIENT_KEY` | ✅ | akasha-cluster |
| `AKASHA_APP_BASE_URL` | ✅ | daemon (URLs dans messages canaux), CLI `akasha update` |

### Optionnelles / avancées (souvent dans spec/user_guide § variables)

| Variable | Usage |
|----------|--------|
| `AKASHA_LOG_LLM_RESPONSE` | Si `1`, log de la réponse LLM complète (debug, api.rs) |
| `AKASHA_SPEC_DIR` | akasha-evals : dossier spec |
| `AKASHA_VAULT_MASTER_KEY` | akasha-vault : clé maître fichier |
| `AKASHA_SUPERVISOR` | Positionné par le CLI (superviseur) — ne pas définir à la main |
| `AKASHA_EMBEDDED_PRELOAD` | Si `1`, précharge le modèle embarqué au démarrage (daemon) |
| `AKASHA_LLM_TIMEOUT_SECS` / `AKASHA_LLM_STREAM_IDLE_SECS` / `AKASHA_LLM_FIRST_CHUNK_SECS` | Timeouts LLM / streaming |
| `AKASHA_SYSTEM_TASK_MAX_TOKENS` | Plafond tokens pour tâches système / résumés |
| `AKASHA_MAX_TOOL_ROUNDS` / `AKASHA_MAX_TOOL_ROUNDS_ORCH_DELIVERABLES` | Limite tours d’outils |
| `AKASHA_MAX_CONCURRENT_DELEGATIONS` | Délégations parallèles |
| `AKASHA_PLUGIN_MAX_FUEL` | akasha-plugin-host (WASM) |
| `AKASHA_TOOLS_JOURNAL_PATH` | Journal outils (api.rs) |
| `AKASHA_MESSAGE_WEBHOOK_URL` | Webhook messages sortants |
| `BRAVE_API_KEY` | Recherche web (tools) |
| `OPENROUTER_API_KEY` / `OPENROUTER_SITE_URL` | Providers LLM |

---

## 4. API HTTP (daemon)

Référence : `crates/akasha-daemon/src/api.rs`. Les préfixes `/api/tasks/`, `/api/schedules/`, `/api/memory/`, etc. couvrent plusieurs méthodes HTTP (voir code).

### Cœur, config, doc

| Surface | Méthodes (indicatif) |
|---------|------------------------|
| `GET /`, `GET /api/status`, `GET /api/doctor` | GET |
| `GET/POST /api/config` | GET, POST (akasha.env) |
| `GET /api/vault/keys`, `DELETE /api/vault` | GET, DELETE |
| `POST /api/restart` | POST |
| `GET /api/docs` | GET (markdown utilisateur) |

### Chat, tâches, planning

| Surface | Remarque |
|---------|----------|
| `POST /api/message` | Chat + pièces jointes |
| `GET /api/tasks`, `GET /api/tasks/:id`, `GET /api/tasks/:id/events` | Liste / détail / événements |
| `GET /api/pending-human-input` | Human-in-the-loop |
| `GET/POST/PUT/DELETE /api/schedules`, exceptions | Planning et exceptions |
| `GET /api/calendar/events` | Vue calendrier |
| `GET /api/task_runs`, `GET /api/task_runs/...` | Exécutions |
| `GET /api/schedule_run_reports` | Rapports |

### Router LLM

| Surface | Remarque |
|---------|----------|
| `GET /api/router/metrics` | Métriques |
| `GET /api/router/models`, `GET /api/router/routes` | Modèles et routes |
| `POST /api/router/route` | Définir route par catégorie |
| `GET /api/router/embedded-status`, `POST /api/router/embedded/reload` | Modèle embarqué |
| `GET /api/router/ollama/models`, `GET /api/router/ollama/show?model=` | Ollama |

### Skills, plugins, outils

| Surface | Remarque |
|---------|----------|
| `GET /api/skills`, `POST /api/skills/reload` | Liste / rechargement |
| `POST /api/skills/install`, `POST /api/skills/uninstall` | Corps JSON (url / name) |
| `GET /api/plugins`, `POST /api/plugins/reload`, `POST /api/plugins/reputation/reset` | Plugins |
| `GET/POST /api/plugins/routing_rules`, `POST .../match` | Règles de routage plugins |
| `GET /api/tools` | Liste des outils machine déclarés |

### Mémoire, profils, voix, RAG utilisateur

| Surface | Remarque |
|---------|----------|
| `GET/DELETE /api/memory/short-term`, `DELETE /api/memory/session/...` | Session |
| `GET /api/memory/search`, `GET/DELETE /api/memory/long-term/...`, `POST /api/memory/rebuild-relations` | Long terme |
| `GET/POST /api/agent-profile`, `GET/POST /api/user-profile` | Profils |
| `POST /api/personality-memory`, `POST /api/chat/suggest-thread-title` | Personnalité / titres |
| `GET/POST /api/voice/status`, `tts`, `stt` | Voix |
| `GET/POST/DELETE /api/user-rag/documents` | Documents RAG utilisateur |
| `POST /api/complete` | Complétion via routeur |

### Observabilité et diagnostic

| Surface | Remarque |
|---------|----------|
| `GET /api/timeline`, `GET /api/metrics` | Timeline, métriques |
| `GET /api/agents` | Agents chargés |
| `GET/POST /api/diagnostic/advice` | Conseils diagnostic |

### Session UI, mise à jour, device

| Surface | Remarque |
|---------|----------|
| `GET /api/session-state`, `GET /api/first-message` | État session / premier message |
| `GET /api/update/status` | Mise à jour |
| `GET /api/device/pending`, `POST /api/device/result` | Pairing device (si activé) |

### Canaux

| Surface | Remarque |
|---------|----------|
| `POST /channels/slack/command` | Slack |
| `POST /channels/teams`, `/channels/teams/message` | Microsoft Teams |

---

## 5. Commandes slash (TUI et Web)

Les commandes documentées dans les user guides sont implémentées en TUI et dans l’UI web (liste complète : `/help` dans le chat).

Correction historique : le texte d’aide de `/models` indique « liste des modèles (tous les providers) » (`GET /api/router/models`).

---

## 6. Fichiers de configuration

| Fichier documenté | Chargé / utilisé dans le code |
|-------------------|-------------------------------|
| llm_router.yaml | ✅ daemon : data_dir puis racine projet |
| connectors.env | ✅ CLI au spawn du daemon |
| akasha.env | ✅ CLI au spawn ; GET/POST /api/config |
| tools_policy.yaml | ✅ daemon : data_dir |
| voice_router.yaml | ✅ si présent (voir config référence) |
| data_dir/skills/*.yaml | ✅ SkillRegistry |
| spec/skills/*.yaml | ✅ idem (spec_skills_dir) |

---

## 7. Data dir (CLI vs daemon)

- **Daemon** : `AKASHA_DATA_DIR` si défini, sinon `dirs::data_local_dir()/akasha` (ou `.akasha` en secours).
- **CLI** : utilise le même `AKASHA_DATA_DIR` que le daemon lorsqu’il est défini (vault, config, init, plugin, etc.).

---

## 8. Spec 33 (agents, outils, skills)

L’état d’implémentation décrit dans `spec/33_agents_tools_orchestrator_skills.md` (§ implémentation) reste la référence pour les phases outils, orchestrateur, skills et UI associée.

---

## 9. GET /api/docs — sources du guide affiché

Ordre de résolution dans `api.rs` :

1. `spec_dir/user_guide.md` (dépôt cloné / dev)
2. `spec_dir/../docs/user_guide.md` (racine projet)
3. `data_dir/docs/user_guide.md` (extrait release)

Les releases copient `docs/user_guide_final.md` → `staging/docs/user_guide.md` (workflow Release).

---

## 10. Résumé des corrections documentées (historique)

1. **TUI / Web** : aide `/models` alignée sur tous les providers.
2. **CLI** : `akasha_data_dir()` respecte `AKASHA_DATA_DIR`.
3. **Audit** : extension avril 2026 — CLI complète (update, services), API regroupée, variables optionnelles, résolution GET /api/docs.

---

## 11. Pistes pour la doc produit

- Garder `docs/user_guide_final.md` et `spec/user_guide.md` synchrones sur les commandes et endpoints « utilisateur » ; le détail développeur (build, evals) reste dans `spec/user_guide.md` uniquement.
- Site public `Akasha_app/docs.html` : checklist anti-dérive dans le dépôt **Akasha_app**, fichier `docs/DOCUMENTATION_SYNC.md`.
