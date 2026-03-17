# Akasha

Assistant personnel **sécurisé**, **local-first**, conçu comme une infrastructure agentique autonome 24/7. Orchestration d’agents spécialisés, multi-canaux (texte, Slack, Discord, Telegram), protection des secrets (vault), routeur LLM avec fallback.

**Vision et principes** : [spec/00_vision.md](spec/00_vision.md)

---

## Sommaire

- [Prérequis](#prérequis)
- [Structure du projet](#structure-du-projet)
- [Démarrage rapide](#démarrage-rapide)
- [Interfaces](#interfaces)
- [Configuration](#configuration)
- [Documentation](#documentation)
- [Phases (état d’avancement)](#phases-état-davancement)
- [Licence](#licence)

---

## Prérequis

- **Rust** 1.70+ ([rustup](https://rustup.rs))
- **Node.js** 18+ et npm (pour l’UI Tauri)
- **Tauri CLI** (installé via npm dans `apps/akasha-ui`)
- **Modèles locaux** : à l’init, choix entre **Ollama** (GPU, nombreux modèles) et **modèles locaux Akasha** (Qwen3 0.6B, Baguettotron intégrés). Voir [spec/user_guide.md](spec/user_guide.md).

**Utilisateurs sans Rust (binaires tout prêts)** : téléchargez l’archive pour votre OS depuis la page **Releases** du dépôt, décompressez, puis lancez `akasha init` puis `akasha start`. Voir [spec/distribution.md](spec/distribution.md).

---

## Structure du projet

```
akasha/
├── crates/
│   ├── akasha-core/         # Types partagés, Event Envelope, Spec Loader, sécurité
│   ├── akasha-embedded-llm/ # POC : LLM intégré (Qwen3 0.6B ou Baguettotron 321M) — onboarding, diagnostics, conversation ; WSL2 recommandé sous Windows
│   ├── akasha-embeddings/   # Embeddings locaux (fastembed/ONNX, modèle porté par l’app)
│   ├── akasha-store/        # SQLite (tâches, mémoire long terme), log immuable
│   ├── akasha-vault/        # Secrets (keyring OS + fichier chiffré)
│   ├── akasha-tools/        # Outils machine (fichiers, commandes, diff, conteneur), politique
│   ├── akasha-plugin-api/   # Plugin API (channel, tool, skill, manifest)
│   ├── akasha-plugin-host/  # Sandbox WASM (Wasmtime)
│   ├── akasha-llm/          # LLM Router (classifier, providers, fallback, métriques)
│   ├── akasha-rag/          # RAG : spec + runbooks, retrieval
│   ├── akasha-cluster/      # Mode cluster : NATS, élection de leader, mTLS
│   ├── akasha-daemon/       # Daemon 24/7, API, agents, canaux, plugins, skills
│   ├── akasha-cli/          # CLI : start, stop, doctor, vault, config, router, plugin, init, tui
│   ├── akasha-tui/          # Interface terminal (Chat, Routeur, Doc)
│   └── akasha-evals/        # Suite d’évals (sécurité, runbooks, hallucinations)
├── apps/
│   └── akasha-ui/           # Application Tauri (React + TypeScript)
├── spec/                    # Spécifications, guide utilisateur, runbooks
└── docs/                    # Point d’entrée vers la doc (voir spec/ et guide)
```

---

## Démarrage rapide

```bash
# 1. Build
cargo build
# Binaires : target/debug/akasha (CLI), target/debug/akasha-daemon

# Sous Windows : si la liaison du daemon échoue (ort_sys / ONNX Runtime), compiler sans la mémoire long terme :
#   cargo build -p akasha-daemon --no-default-features
#   cargo build -p akasha-cli -p akasha-tui
# Pour avoir la mémoire long terme sur Windows : utiliser WSL2 ou voir spec/06_memory_model.md (§ Mémoire long terme sur Windows).

# 2. Premier lancement (recommandé)
.\target\debug\akasha.exe init
# Configure LLM (Ollama/OpenAI/OpenRouter), vault, connecteurs ; crée llm_router.yaml, connectors.env
# En fin de wizard : option d'installer les services Docker (Ollama, TTS/STT, BitNet) depuis akasha-models
# Ou plus tard : akasha services install --ollama --voice --bitnet (voir AKASHA_MODELS_DIR)

# 3. Démarrer le daemon
.\target\debug\akasha.exe start
# ou en premier plan (logs visibles) :
.\target\debug\akasha.exe start --foreground

# 4. Vérifier
.\target\debug\akasha.exe doctor
.\target\debug\akasha.exe doctor --advice   # conseil basé sur les runbooks + LLM (daemon requis)
```

**Arrêter** : `akasha stop`

**Chemins** : `akasha paths` affiche le répertoire de données et les fichiers de config (utile sous WSL).

Détail de l’onboarding : [spec/onboarding.md](spec/onboarding.md).

---

## Interfaces

### Interface terminal (TUI)

```bash
cargo build -p akasha-cli -p akasha-tui
akasha tui
```

- **Onglets** : Chat, Routeur (métriques LLM), Doc (guide utilisateur servi par le daemon), Activité (tâches et événements).
- **Chat** : envoi de messages au daemon ; réponses via l’orchestrateur (ack immédiat, traitement en arrière-plan). Défilement : ↑↓, PgUp/PgDn, Home/End.
- **Commandes slash** (dans le chat, identiques en TUI et Tauri) : `/help`, `/status`, `/doctor`, `/advice`, `/embedded`, `/embedded reload`, `/metrics`, `/models`, `/models list`, `/models set CATÉGORIE PROVIDER MODÈLE`, `/routes`, `/config list` / `get` / `set`, `/vault list`, `/plugins`, `/reload`, `/skills reload`, `/skills uninstall <nom>`, `/restart`, `/task create "msg"`, `/schedule create` / `delete`, `/stop TASK_ID`, `/cancel TASK_ID`, `/newsession`. Liste complète : taper `/help` dans le chat.
- **Raccourcis** : Tab = changer d’onglet, R = rafraîchir (métriques ou doc), Échap / Ctrl+Q = quitter.

### Interface web (Tauri)

```bash
cd apps/akasha-ui
npm install
npm run tauri dev
```

- Connexion au daemon sur le port **3876** (`AKASHA_PORT`).
- **Onglets** : Chat, Routeur, Documentation, Paramètres.
- Mêmes commandes slash que la TUI dans le champ de chat.

### Flux des demandes (orchestrateur)

L’utilisateur ne parle qu’à l’**orchestrateur**. À chaque message (TUI, Web ou canal) :

1. Le daemon renvoie immédiatement « Je prends en compte votre demande » avec un `task_id`.
2. La demande est déléguée à l’agent adéquat (ex. conversation/LLM) de façon **non bloquante**.
3. La réponse finale est affichée dans le Chat (ou le canal) lorsque la tâche est terminée.

Feuille de route agents/outils/skills : [spec/33_agents_tools_orchestrator_skills.md](spec/33_agents_tools_orchestrator_skills.md).

---

## Configuration

| Variable | Description | Défaut |
|----------|-------------|--------|
| `AKASHA_PORT` | Port du daemon | 3876 |
| `AKASHA_DATA_DIR` | Répertoire de données (vault, plugins, config) | %LOCALAPPDATA%\akasha / ~/.local/share/akasha |
| `AKASHA_LOG` | Niveau de log (trace, debug, info, warn, error) | info |
| `AKASHA_MAX_RESPONSE_TOKENS` | Tokens max pour les réponses chat | 4096 |
| `AKASHA_MAX_CONTEXT_TOKENS` | Contexte max (déclenchement compaction mémoire court terme) | 8192 |
| `OLLAMA_HOST` | URL Ollama si pas de llm_router.yaml | http://localhost:11434 |
| `AKASHA_SLACK_ENABLED` | 1 = activer Slack | — |
| `AKASHA_DISCORD_ENABLED` | 1 = activer Discord | — |
| `AKASHA_TELEGRAM_ENABLED` | 1 = activer Telegram | — |
| `AKASHA_DEGRADED_MODE` | 1 = routeur limité aux providers locaux | — |
| `AKASHA_CLUSTER_ENABLED` | 1 = mode cluster (NATS) | — |
| `NATS_URL` | URL NATS (cluster) | nats://127.0.0.1:4222 |

**Variables persistantes** : `akasha config env set KEY [value]` écrit dans `data_dir/akasha.env`, chargé au démarrage du daemon. Voir aussi `akasha config env list`, `get KEY`.

**Fichiers principaux** :

- **llm_router.yaml** (data_dir ou racine) : providers (Ollama, OpenAI, OpenRouter, akasha_embedded), modèles par type de tâche. Par défaut le routeur utilise le modèle embarqué (akasha_embedded/default). Exemple : [spec/llm_router.example.yaml](spec/llm_router.example.yaml). Depuis la TUI : `/models set CATÉGORIE PROVIDER MODÈLE` pour changer à chaud.
- **connectors.env** : activation des canaux ; chargé par `akasha start`.
- **akasha.env** : variables persistantes (config env).
- **tools_policy.yaml** (data_dir, optionnel) : politique des outils machine (chemins/autorisations). Exemple : [spec/tools_policy.example.yaml](spec/tools_policy.example.yaml).
- **voice_router.yaml** (data_dir, optionnel) : TTS/STT — URLs des services synthèse et transcription ; active le bouton message vocal dans l’interface web quand STT est configuré. Exemple : [spec/voice_router.example.yaml](spec/voice_router.example.yaml).

Liste complète des commandes et options : [spec/user_guide.md](spec/user_guide.md) (également accessible dans la TUI et l’UI web via l’onglet Doc).

---

## Documentation

| Document | Contenu |
|----------|---------|
| [spec/README.md](spec/README.md) | Index des spécifications et de la doc technique |
| [spec/00_vision.md](spec/00_vision.md) | Vision et principes du projet |
| [spec/user_guide.md](spec/user_guide.md) | **Guide utilisateur** : commandes, config, interfaces, slash, canaux |
| [spec/onboarding.md](spec/onboarding.md) | Premier lancement pas à pas |
| [spec/33_agents_tools_orchestrator_skills.md](spec/33_agents_tools_orchestrator_skills.md) | Feuille de route agents, outils machine, skills, conteneur, UI |
| [spec/runbooks/](spec/runbooks/) | Runbooks (diagnostic, redémarrage daemon) |
| [docs/user_guide.md](docs/user_guide.md) | Renvoi vers le guide complet (spec + API) |

Quand le daemon tourne, le guide utilisateur est servi en markdown via **GET /api/docs** (onglet Doc de la TUI et de l’UI web).

---

## Phases (état d’avancement)

- **Phase 0** — Workspace, Spec Loader, Event Envelope, CLI start/stop/doctor, Tauri skeleton, daemon + healthcheck ✅  
- **Phase 1** — Watchdog, crash loop detection, SQLite tâches, log append-only ✅  
- **Phase 2** — Main Agent (ack &lt; 500 ms), Orchestrator, workers, POST /api/message, GET /api/tasks/:id, event bus ✅  
- **Phase 3** — Vault (keyring + fichier chiffré), RBAC, redaction, prompt injection, trust store, CLI vault ✅  
- **Phase 4** — Canaux Slack, Discord, Telegram (slash / commande, vault, polling tâche) ✅  
- **Phase 5** — Plugin API, sandbox WASM, registre, réputation, CLI plugin ✅  
- **Phase 6** — LLM Router (classifier, Ollama/OpenAI/OpenRouter, fallback, métriques, mode dégradé) ✅  
- **Phase 7** — Cluster NATS, élection de leader, mTLS optionnel, réplication log ✅  
- **Phase 8** — RAG (spec + runbooks), doctor --advice, evals (sécurité, runbooks, hallucinations) ✅  
- **Phase 9** — Polish UI : TUI (Chat, Routeur, Doc, slash), UI Tauri (Chat, Routeur, Doc, Paramètres), commandes slash ✅  
- **Agents / outils / skills** — Orchestrateur seul point d’entrée, délégation non bloquante, outils machine (akasha-tools), politique, skills chargeables, conteneur pour code, API /api/router/models, GET /api/tasks, /api/tasks/:id/events. Voir [spec/33_agents_tools_orchestrator_skills.md](spec/33_agents_tools_orchestrator_skills.md). ✅
- **Voix (TTS/STT)** — Synthèse et transcription via services HTTP externes (voice_router.yaml) ; outils `speech_synthesize` / `speech_transcribe` ; API `/api/voice/status`, `/api/voice/tts`, `/api/voice/stt` ; bouton message vocal dans l’interface web lorsque STT est configuré ; réponse en texte + audio si la question est envoyée en vocal (TTS configuré). Voir [spec/35_configuration_reference.md](spec/35_configuration_reference.md) et [akasha-models/README.md](akasha-models/README.md). ✅
- **Mémoire** — Court terme (session, compaction par résumé LLM), long terme (SQLite `memory.db`, embeddings **portés par l’app** via `akasha-embeddings` / fastembed, pas d’app tierce), promotion automatique des résumés, récupération par similarité. Voir [spec/06_memory_model.md](spec/06_memory_model.md). ✅

---

## Licence

Propriétaire / À définir
