# Guide utilisateur Akasha

Documentation accessible depuis l'interface TUI et l'interface web. Elle décrit les commandes, l'onboarding, les options de configuration et le lancement des différentes interfaces.

---

## 1. Prérequis

- **Rust** 1.70+ ([rustup](https://rustup.rs))
- **Node.js** 18+ et npm (pour l'UI Tauri)
- Optionnel : **Ollama** pour les réponses LLM locales

---

## 2. Build

À la racine du projet :

```bash
cargo build
```

Binaires générés dans `target/debug/` : **akasha** (CLI) et **akasha-daemon** (daemon).

Pour l'interface TUI : `cargo build -p akasha-cli -p akasha-tui`

---

## 3. Commandes exposées à l'utilisateur

### Daemon

| Commande | Description |
|----------|-------------|
| `akasha start` | Démarre le daemon en arrière-plan (avec superviseur et redémarrage auto) |
| `akasha start --foreground` | Démarre le daemon au premier plan (logs visibles dans le terminal) |
| `akasha stop` | Arrête le daemon |

### Premier lancement (onboarding)

| Commande | Description |
|----------|-------------|
| `akasha init` | Assistant interactif : provider LLM, vault, connecteurs (Telegram, Slack, Discord), génère `llm_router.yaml` et `connectors.env` |
| `akasha init --defaults` | Initialisation minimale sans questions (Ollama uniquement, aucun connecteur) |

### Diagnostic

| Commande | Description |
|----------|-------------|
| `akasha doctor` | Vérifie : Rust, Node, binaire daemon, specs, santé du daemon ; si le daemon tourne, affiche aussi les checks côté daemon (Ollama, vault, spec_dir, embedded_llm) et les chemins de config |
| `akasha doctor --json` | Sortie JSON |
| `akasha doctor --advice` | Conseil diagnostic basé sur les runbooks et le modèle LLM (daemon requis) |
| `akasha doctor --fix` | Corrige les données manquantes : crée data_dir si absent, llm_router.yaml (défaut + providers.ollama), tools_policy.yaml (depuis spec ou minimal), connectors.env (vide) |
| `akasha paths` | Affiche le data_dir et les chemins des fichiers de config (llm_router.yaml, akasha.env, etc.) |

### Vault (secrets)

| Commande | Description |
|----------|-------------|
| `akasha vault list` | Liste les clés secrètes (noms uniquement) |
| `akasha vault set KEY [value]` | Enregistre un secret ; si `value` est omis, lu depuis l'entrée standard |
| `akasha vault get KEY` | Affiche la valeur d'une clé (à utiliser avec précaution) |

### Config (modèles + variables d'environnement)

| Commande | Description |
|----------|-------------|
| `akasha config models get [CATEGORY]` | Affiche les modèles configurés par catégorie ; sans argument, liste toutes les catégories avec primary (et fallback si une catégorie est fournie) |
| `akasha config models routes` | Affiche pour chaque catégorie le primary et la liste des fallback (modèles utilisés en cas d'échec du primary) |
| `akasha config models set CATEGORY PROVIDER MODEL` | Définit le modèle principal pour une catégorie ; l'ancien primary est ajouté en tête de la liste de fallback |
| `akasha config models fetch` | Récupère les infos (contexte max, num_ctx, etc.) pour tous les modèles Ollama présents dans `llm_router.yaml` |
| `akasha config models add MODEL [--ollama-url URL]` | Ajoute les infos d'un modèle Ollama au fichier de config |
| `akasha config env list` | Affiche les variables dans `akasha.env` |
| `akasha config env get KEY` | Affiche la valeur d'une variable |
| `akasha config env set KEY [value]` | Définit une variable (valeur optionnelle, lue depuis stdin si omise) |

### Routeur LLM

| Commande | Description |
|----------|-------------|
| `akasha router metrics` | Affiche les métriques du routeur (requêtes, latence, fallbacks) |
| `akasha router discover` | Découvre les instances Ollama (local + réseau local) |
| `akasha router show MODEL` | Affiche les infos d'un modèle Ollama (contexte max, num_ctx) |

### Plugins

| Commande | Description |
|----------|-------------|
| `akasha plugin list` | Liste les plugins installés (via le daemon) |
| `akasha plugin reload` | Recharge les plugins sans redémarrer le daemon |
| `akasha plugin install CHEMIN` | Installe un plugin depuis un répertoire |
| `akasha plugin uninstall ID` | Désinstalle un plugin par son ID |
| `akasha plugin catalog` | Affiche le catalogue local des plugins |

### Interfaces

| Commande | Description |
|----------|-------------|
| `akasha tui` | Lance l'interface terminal (TUI) : Chat + métriques routeur + Documentation |

---

## 4. Guide d'onboarding (résumé)

1. **Build** : `cargo build`
2. **Initialisation** : `akasha init` (ou `akasha init --defaults` pour le minimal)
3. **Secrets** : si besoin, `akasha vault set ...` pour les tokens non demandés par init
4. **Environnement** : les fichiers `connectors.env` et `akasha.env` sont chargés automatiquement par `akasha start`
5. **Démarrer** : `akasha start` ou `akasha start --foreground`
6. **Vérifier** : `akasha doctor` puis `akasha doctor --advice`

Fichiers créés dans le data_dir (ex. `%LOCALAPPDATA%\akasha` sous Windows) :
- `llm_router.yaml` — configuration du routeur LLM (et `model_options` si Ollama est joignable lors de l'init)
- `connectors.env` — activation Telegram / Slack / Discord
- `akasha.env` — variables persistantes (optionnel, géré par `akasha config env`)

---

## 5. Options et paramètres de configuration

### Variables d'environnement

| Variable | Description | Défaut |
|----------|-------------|--------|
| `AKASHA_PORT` | Port du daemon | 3876 |
| `AKASHA_LOG` | Niveau de log (trace, debug, info, warn, error) | info |
| `AKASHA_MAX_RESPONSE_TOKENS` | Nombre max de tokens pour les réponses chat | 4096 |
| `AKASHA_DATA_DIR` | Répertoire de données (vault, plugins, llm_router.yaml, etc.) | %LOCALAPPDATA%\akasha (Windows) / ~/.local/share/akasha (Linux/macOS) |
| `AKASHA_SLACK_ENABLED` | `1` pour activer l'adaptateur Slack | — |
| `AKASHA_DISCORD_ENABLED` | `1` pour activer le bot Discord | — |
| `AKASHA_TELEGRAM_ENABLED` | `1` pour activer le bot Telegram | — |
| `AKASHA_TELEGRAM_NOTIFY_CHAT_ID` | ID du chat Telegram pour la notification « bot connecté » | — |
| `AKASHA_DEGRADED_MODE` | `1` = routeur n'utilise que les providers locaux (Ollama, Akasha Core) | — |
| `AKASHA_CLUSTER_ENABLED` | `1` = mode cluster (NATS, élection de leader) | — |
| `AKASHA_NODE_ID` | Identifiant du nœud (cluster) | HOSTNAME / COMPUTERNAME ou UUID |
| `AKASHA_NATS_TLS_CA` | Chemin vers le certificat CA pour NATS (mTLS) | — |
| `AKASHA_NATS_CLIENT_CERT` | Chemin vers le certificat client NATS (mTLS) | — |
| `AKASHA_NATS_CLIENT_KEY` | Chemin vers la clé privée client NATS (mTLS) | — |
| `NATS_URL` | URL du serveur NATS (mode cluster) | nats://127.0.0.1:4222 |
| `OLLAMA_HOST` | URL Ollama si pas de `llm_router.yaml` | http://localhost:11434 |

**PowerShell** : `$env:AKASHA_TELEGRAM_ENABLED="1"` (et non `set`).  
**CMD** : `set AKASHA_TELEGRAM_ENABLED=1`.

Les variables définies via `akasha config env set` sont enregistrées dans `data_dir/akasha.env` et chargées au démarrage du daemon.

*Variables optionnelles (debug / avancé)* : `AKASHA_LOG_LLM_RESPONSE=1` (log détaillé des réponses LLM) ; `AKASHA_SPEC_DIR` (chemin vers le dossier `spec` pour la suite d’évals) ; `AKASHA_VAULT_MASTER_KEY` (clé maître pour le vault fichier, si utilisé) ; `AKASHA_LLM_TIMEOUT_SECS` (timeout en secondes pour les appels LLM, défaut 300) ; `AKASHA_LLM_STREAM_IDLE_SECS` (timeout d’inactivité en secondes entre deux chunks, défaut 60) ; `AKASHA_LLM_FIRST_CHUNK_SECS` (délai max pour recevoir le **premier** chunk du modèle embarqué, défaut min(300, AKASHA_LLM_TIMEOUT_SECS) — le premier token peut être lent : chargement modèle, inférence CPU) ; `AKASHA_EMBEDDED_MODEL` (modèle embarqué : `qwen3_0_6b` par défaut, `baguettotron` si compilé avec la feature `embedded-baguettotron`). Pour tenter d’accélérer les modèles locaux **sans CUDA** : compiler avec **`--features embedded-mkl`** (Intel MKL). Si le link échoue avec `undefined symbol: hgemm_`, retirer `embedded-mkl` et utiliser le build par défaut (CPU) ou Ollama.

### Fichiers de configuration

- **llm_router.yaml** : recherché dans l'ordre : data_dir, puis racine du projet. Définit les providers (Ollama, OpenAI, OpenRouter) et les modèles par type de tâche. **Section `providers` vide** : ce n'est pas la cause de timeouts. Le daemon enregistre quand même Ollama (URL = `OLLAMA_HOST` ou découverte auto ou `http://localhost:11434`) et le modèle embarqué. Pour éviter les timeouts : vérifier qu'Ollama tourne si vous l'utilisez ; pour le modèle local, le premier appel peut être long (téléchargement + chargement) — précharge au démarrage ou augmenter `default_timeout_secs` / `AKASHA_LLM_TIMEOUT_SECS`. Ajouter une section `providers` avec au moins `ollama.base_url` rend la config explicite (voir `spec/llm_router.example.yaml`).
- **connectors.env** : variables d'activation des connecteurs (chargé par `akasha start`).
- **akasha.env** : variables persistantes (chargé après connectors.env).
- **tools_policy.yaml** (dans le data_dir) : politique de sécurité des **outils machine** (lecture/écriture de fichiers, commandes). Utilisé par l’agent pour `read_file`, `write_file`, `search_files`, `run_command`, etc. Si le fichier est absent, le daemon peut le créer à partir de `spec/tools_policy.example.yaml` au premier démarrage. Pour autoriser l’écriture de fichiers (ex. génération de code sur disque), éditez ce fichier et ajoutez les répertoires sous **allowed_write_paths** (et **allowed_read_paths** pour la lecture). Par défaut, tout est refusé si le fichier est vide ou manquant.

### Où sont stockés les modèles

| Type | Emplacement | Remarque |
|------|-------------|----------|
| **Modèles d'embeddings** (mémoire long terme) | `data_dir/embedding_model/` | Utilisé par le daemon (fastembed). Sous-dossiers type `models--<org>--<nom>/`. |
| **Modèles LLM embarqués** (Qwen, Baguettotron) | Cache Hugging Face | Par défaut : **`~/.cache/huggingface/hub`** (Linux/macOS) ou **`%USERPROFILE%\.cache\huggingface\hub`** (Windows). Rediriger avec **`HF_HOME`** (ex. `HF_HOME=%LOCALAPPDATA%\akasha\hf_cache`). |

Le **data_dir** s'affiche avec `akasha paths` ; par défaut : `~/.local/share/akasha` (Linux/macOS) ou `%LOCALAPPDATA%\akasha` (Windows), sauf si `AKASHA_DATA_DIR` est défini.

---

## 6. Lancement des interfaces et du daemon

### Daemon

```bash
# Arrière-plan (superviseur + redémarrage auto)
akasha start

# Premier plan (logs dans le terminal)
akasha start --foreground
```

Le daemon écoute par défaut sur le port **3876** (`AKASHA_PORT`).

### Interface terminal (TUI)

```bash
cargo build -p akasha-cli -p akasha-tui
akasha tui
```

Raccourcis TUI :
- **Tab** : basculer entre Chat, Routeur (métriques), Doc (documentation) et Activité (tâches et événements)
- **Entrée** : envoyer le message (mode Chat)
- **↑ / ↓, PgUp / PgDn, Home / End** : défilement du contenu (Chat, Doc, panneau détail Activité)
- **R** : rafraîchir les métriques (Routeur), la doc (Doc) ou la liste des tâches (Activité)
- **Échap** ou **Ctrl+Q** : quitter

### Interface web (Tauri)

```bash
cd apps/akasha-ui
npm install
npm run tauri dev
```

L'UI se connecte au daemon sur le port 3876 (configurable via `AKASHA_PORT`). Onglets : Chat, Routeur (métriques), Documentation, Paramètres.

### Flux des demandes (orchestrateur)

L'utilisateur ne parle qu'à l'**agent orchestrateur**. Quand vous envoyez un message (TUI, Web ou canal) :

1. Le daemon répond immédiatement : « Je prends en compte votre demande » avec un `task_id`.
2. La demande est transmise à l'orchestrateur, qui la délègue à l'agent adéquat (ex. conversation / LLM) dans un flux **non bloquant**.
3. Vous pouvez enchaîner plusieurs demandes ; chacune est traitée en parallèle.
4. La réponse finale est disponible via le suivi de la tâche (polling `GET /api/tasks/:id` ou affichage dans le Chat quand la tâche est terminée).

Voir `spec/33_agents_tools_orchestrator_skills.md` pour la feuille de route (outils machine, skills, sous-agents, onglets Agents/Actions). **Liste des outils de base** : `read_file`, `write_file`, `search_files`, `run_command`, `file_diff` — détaillée dans la spec 33 et exposée via **GET /api/tools** (JSON).

### Commandes slash (TUI et interface web)

Dans le chat, les messages commençant par **/** sont interprétés comme des commandes (pas envoyés au LLM) :

| Commande | Description |
|----------|-------------|
| `/help`, `/?` | Aide des commandes |
| `/status` | État du daemon |
| `/doctor` | Diagnostic (daemon, Ollama, vault, spec, modèle embarqué) ; si le daemon tourne, affiche aussi les checks côté daemon (embedded_llm, etc.) |
| `/advice` | Conseil diagnostic (RAG + modèle LLM) |
| `/embedded` | Statut du modèle local embarqué (disponible, chargé ou non) |
| `/embedded reload` | Décharge le modèle embarqué (rechargé au prochain appel) |
| `/metrics` | Métriques du routeur LLM |
| `/models` | Liste des modèles (tous les providers : Ollama, OpenAI, OpenRouter, akasha_embedded, etc.) |
| `/models list` | Modèles par catégorie (primary + fallback) |
| `/routes` | Idem : primary et fallback par catégorie |
| `/models set CATÉGORIE PROVIDER MODÈLE` | Définit le modèle pour une catégorie (ex. `/models set conversation ollama llama3.2`) ; l'ancien primary passe en fallback ; pris en compte immédiatement et sauvegardé |
| `/config list` | Variables (akasha.env) |
| `/config get KEY` | Valeur d'une variable |
| `/config set KEY value` | Définir une variable |
| `/vault list` | Clés du vault (noms uniquement) |
| `/plugins` | Liste des plugins installés |
| `/reload` | Recharger les plugins |
| `/restart` | Redémarrer le daemon (superviseur) |

Pour ajouter une clé au vault : utiliser le CLI `akasha vault set KEY [value]` (pas d’équivalent slash pour la sécurité).

### Suite d'évals

```bash
cargo run -p akasha-evals
```

À exécuter depuis la racine du projet (ou avec `AKASHA_SPEC_DIR` pointant vers le dossier `spec`).

---

## 7. Canaux (Telegram, Slack, Discord)

- **Telegram** : vault `telegram_bot_token`, puis `AKASHA_TELEGRAM_ENABLED=1`. Commande `/akasha <message>` (ou `/start` pour l'aide). Optionnel : `AKASHA_TELEGRAM_NOTIFY_CHAT_ID` pour recevoir « bot connecté » au démarrage.
- **Slack** : vault `slack_signing_secret`, `AKASHA_SLACK_ENABLED=1`. Slash command configurée vers `POST /channels/slack/command`.
- **Discord** : vault `discord_bot_token`, `AKASHA_DISCORD_ENABLED=1`. Bot avec préfixe `!akasha <message>`.

---

## 8. Référence rapide

| Action | Commande / moyen |
|--------|-------------------|
| Démarrer le daemon | `akasha start` ou `akasha start --foreground` |
| Arrêter le daemon | `akasha stop` |
| Premier lancement | `akasha init` |
| Vérifier l'état | `akasha doctor` / `akasha doctor --advice` |
| Enregistrer un secret | `akasha vault set KEY value` |
| Définir une variable persistante | `akasha config env set KEY value` |
| Infos modèles Ollama dans la config | `akasha config models fetch` ou `akasha config models add MODEL` |
| Voir les modèles par catégorie (primary + fallback) | `akasha config models routes` ou `akasha config models get` ; dans la TUI : `/models list` ou `/routes` |
| Changer le modèle par catégorie | `akasha config models set CATEGORY PROVIDER MODEL` ou TUI : `/models set CATÉGORIE PROVIDER MODÈLE` (l'ancien primary est ajouté en fallback) |
| Chemins de config (data_dir, llm_router, etc.) | `akasha paths` (affiché aussi dans `akasha doctor`) |
| Lancer l'interface terminal | `akasha tui` |
| Lancer l'interface web | `cd apps/akasha-ui && npm run tauri dev` |

**Où trouver cette doc** : dans le dépôt : [spec/user_guide.md](user_guide.md), [spec/onboarding.md](onboarding.md), [spec/README.md](README.md) (index des specs), [README.md](../README.md) à la racine. Quand le daemon tourne : `GET /api/docs` ou onglet Doc (TUI / UI web).
