# Audit documentation vs code

Comparaison de la documentation (README, spec, user_guide) avec l’implémentation réelle. Dernière mise à jour : mars 2026.

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
| `akasha init` | ✅ | |
| `akasha init --defaults` | ✅ | |
| `akasha vault list/set/get` | ✅ | |
| `akasha config models fetch` | ✅ | |
| `akasha config models add MODEL [--ollama-url URL]` | ✅ | `--ollama-url` en long uniquement |
| `akasha config env list/get/set` | ✅ | |
| `akasha router metrics` | ✅ | |
| `akasha router discover` | ✅ | |
| `akasha router show MODEL` | ✅ | Nécessite le daemon (appel API) |
| `akasha plugin list/reload/install/uninstall/catalog` | ✅ | |
| `akasha tui` | ✅ | |
| `akasha paths` | ✅ | Affiche data_dir et chemins des fichiers de config. |
| `akasha doctor --fix` | ✅ | Crée data_dir, llm_router.yaml, tools_policy.yaml, connectors.env si absents. |

Aucune commande documentée n’est absente du code.

---

## 3. Variables d’environnement

| Variable documentée | Utilisée dans le code | Fichier / usage |
|---------------------|------------------------|------------------|
| `AKASHA_PORT` | ✅ | daemon (port HTTP), CLI (daemon_base_url), TUI |
| `AKASHA_DATA_DIR` | ✅ | daemon (main), CLI (akasha_data_dir depuis audit) |
| `AKASHA_LOG` | ✅ | daemon main, CLI (daemon_env) |
| `AKASHA_MAX_RESPONSE_TOKENS` | ✅ | api.rs (réponses chat) |
| `OLLAMA_HOST` | ✅ | daemon (config Ollama), CLI (pass-through) |
| `AKASHA_SLACK_ENABLED` | ✅ | daemon |
| `AKASHA_DISCORD_ENABLED` | ✅ | daemon |
| `AKASHA_TELEGRAM_ENABLED` | ✅ | daemon |
| `AKASHA_TELEGRAM_NOTIFY_CHAT_ID` | ✅ | daemon |
| `AKASHA_DEGRADED_MODE` | ✅ | daemon |
| `AKASHA_CLUSTER_ENABLED` | ✅ | daemon |
| `NATS_URL` | ✅ | akasha-cluster, CLI pass-through |
| `AKASHA_NODE_ID` | ✅ | akasha-cluster |
| `AKASHA_NATS_TLS_CA` / `_CLIENT_CERT` / `_CLIENT_KEY` | ✅ | akasha-cluster |

Variables utilisées mais non listées dans le README (optionnelles / internes) :

- `AKASHA_LOG_LLM_RESPONSE` : si `1`, log de la réponse LLM complète (api.rs).
- `AKASHA_VAULT_MASTER_KEY` : clé maître pour le vault fichier (akasha-vault).
- `AKASHA_SUPERVISOR` : positionné par le CLI lors du lancement du superviseur (ne pas définir manuellement).
- `AKASHA_SPEC_DIR` : utilisé par akasha-evals pour le dossier spec.

La doc utilisateur (spec/user_guide.md) contient un tableau plus complet ; le README cible les variables principales.

---

## 4. API HTTP (daemon)

| Endpoint documenté ou attendu | Implémenté (api.rs) |
|-------------------------------|----------------------|
| GET / | ✅ |
| GET /api/status | ✅ |
| GET /api/doctor | ✅ |
| GET /api/config | ✅ (vars depuis akasha.env) |
| POST /api/config | ✅ (écrit akasha.env) |
| GET /api/vault/keys | ✅ |
| DELETE /api/vault | ✅ (body `{"key": "KEY"}`) |
| POST /api/restart | ✅ |
| GET /api/docs | ✅ (guide utilisateur) |
| POST /channels/slack/command | ✅ |
| POST /api/message | ✅ |
| GET /api/tasks | ✅ |
| GET /api/tasks/:id | ✅ |
| GET /api/tasks/:id/events | ✅ |
| GET /api/plugins | ✅ |
| POST /api/plugins/reload | ✅ |
| GET /api/skills | ✅ |
| POST /api/skills/reload | ✅ |
| POST /api/message | ✅ (pièces jointes, PDF) |
| GET /api/calendar/events | ✅ (vue calendrier, tâches récentes, récurrentes) |
| GET /api/task_runs | ✅ |
| GET /api/user-rag/documents | ✅ (RAG utilisateur) |
| POST /api/complete | ✅ |
| GET /api/router/metrics | ✅ |
| GET /api/router/models | ✅ (tous les providers) |
| GET /api/router/ollama/models | ✅ |
| GET /api/router/ollama/show | ✅ (query param model) |
| POST/GET /api/diagnostic/advice | ✅ |

Cohérent avec la doc (user_guide, README).

---

## 5. Commandes slash (TUI et Web)

Toutes les commandes slash documentées dans le user_guide sont implémentées en TUI et dans l’UI web :

- `/help`, `/?`, `/status`, `/doctor`, `/advice`, `/embedded`, `/embedded reload`, `/metrics`, `/models`, `/models list`, `/routes`, `/models set ...`, `/config list|get|set`, `/vault list`, `/plugins`, `/reload`, `/skills reload`, `/restart`.

Correction effectuée : le texte d’aide intégré (TUI et Web) indiquait « liste des modèles Ollama » pour `/models` ; il a été remplacé par « liste des modèles (tous les providers) » pour refléter l’appel à `GET /api/router/models`.

---

## 6. Fichiers de configuration

| Fichier documenté | Chargé / utilisé dans le code |
|-------------------|-------------------------------|
| llm_router.yaml | ✅ daemon : data_dir puis spec_dir.parent() (racine projet) |
| connectors.env | ✅ CLI : chargé au spawn du daemon (daemon_env_load_connectors), pas par le daemon lui-même |
| akasha.env | ✅ CLI : chargé au spawn du daemon (daemon_env_load_akasha_env) ; daemon : GET/POST /api/config lisent/écrivent ce fichier |
| tools_policy.yaml | ✅ daemon : data_dir/tools_policy.yaml (optionnel) |
| data_dir/skills/*.yaml | ✅ daemon : SkillRegistry.load_from_dir |
| spec/skills/*.yaml | ✅ daemon : idem (spec_skills_dir) |

---

## 7. Data dir (CLI vs daemon)

- **Daemon** : utilise `AKASHA_DATA_DIR` si défini, sinon `dirs::data_local_dir()/akasha` (ou `.akasha` en secours).
- **CLI** (avant audit) : `akasha_data_dir()` utilisait toujours `dirs::data_local_dir()/akasha` et ignorait `AKASHA_DATA_DIR`.

**Correction** : le CLI utilise maintenant `AKASHA_DATA_DIR` lorsqu’il est défini, de sorte que vault, config, init, plugin, etc. partagent le même data dir que le daemon.

---

## 8. Spec 33 (agents, outils, skills)

L’état d’implémentation décrit dans `spec/33_agents_tools_orchestrator_skills.md` (§ 10) est aligné avec le code :

- Phase A (outils + politique) : crate akasha-tools, ToolExecutor, tools_policy.yaml.
- Phase B (orchestrateur, ack, délégation non bloquante) : main_agent, orchestrator, conversation worker.
- Phase C (conteneur) : feature `container` dans akasha-tools.
- Phase D (skills) : SkillRegistry, data_dir/skills et spec/skills, GET /api/skills.
- Phase E (sous-tâches) : decompose_request, agrégateur.
- Phase F (événements / onglets) : EventsCache, GET /api/tasks, GET /api/tasks/:id/events ; onglets dédiés « Agents » / « Activité » côté UI à brancher.

---

## 9. Résumé des corrections effectuées

1. **TUI** : message d’aide de `/models` — « liste des modèles Ollama » → « liste des modèles (tous les providers) ».
2. **Web UI** : même mise à jour pour le texte d’aide de `/models`.
3. **CLI** : `akasha_data_dir()` prend en compte `AKASHA_DATA_DIR` pour aligner CLI et daemon sur le même répertoire de données.

---

## 10. Points optionnels pour la doc

- Documenter **AKASHA_LOG_LLM_RESPONSE** (debug réponses LLM) et **AKASHA_SPEC_DIR** (evals) dans le guide utilisateur ou le README, si utile.
- **AKASHA_VAULT_MASTER_KEY** : déjà mentionné ou à préciser dans la section vault / déploiement selon le niveau de détail souhaité.
