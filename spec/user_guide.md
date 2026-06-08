# Guide utilisateur Akasha

Documentation accessible depuis l'interface TUI et l'interface web. Elle décrit les commandes, l'onboarding, les options de configuration et le lancement des différentes interfaces.

**Utilisateurs des binaires uniquement** : le guide dédié aux utilisateurs qui n'ont pas accès au code source est dans [docs/user_guide_final.md](../docs/user_guide_final.md). Il est servi dans l'onglet Doc des interfaces lorsque vous lancez le daemon depuis le dossier d'extraction contenant `docs/user_guide.md` (voir [spec/distribution.md](distribution.md)). **En dev** depuis ce dépôt, `GET /api/docs` lit ce fichier (`spec/user_guide.md`) en priorité — garder les commandes et onglets alignés avec `user_guide_final.md` sauf sections réservées au build / contributeurs.

---

## 1. Prérequis

- **Rust** 1.70+ ([rustup](https://rustup.rs))
- **Node.js** 18+ et npm (pour l'UI Tauri)
- **Modèles locaux** : au choix lors de l’init — **Ollama** (recommandé si installé : GPU, nombreux modèles) ou **modèles locaux Akasha** (Qwen3 0.6B, Baguettotron intégrés, sans installation). Plus tard, un modèle entraîné spécifiquement pour Akasha pourra s’ajouter à l’offre locale.

---

## 2. Build

À la racine du projet :

```bash
cargo build
```

Binaires générés dans `target/debug/` : **akasha** (CLI) et **akasha-daemon** (daemon).

Pour l'interface TUI : `cargo build -p akasha-cli -p akasha-tui`

---

## 2.1 Premier pas (après init)

1. **Envoyer un message** : lancer l’UI Tauri ou la TUI, ouvrir l’onglet Chat, saisir un message et envoyer. La tâche apparaît dans l’onglet Tâches avec sa progression.
2. **Créer une récurrence** : via l’API (schedules) ou un message du type « rappelle-moi chaque jour de … » selon les capacités configurées.
3. **Consulter l’onglet Tâches** : voir l’état des tâches, les sous-tâches, et répondre aux questions en attente (human-in-the-loop) depuis la bannière ou le modal.

En premier lancement, l’UI peut proposer un guide court (premier objectif) ; option « Ne plus afficher » (localStorage).

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
| `akasha init` | Assistant interactif : choix du provider LLM (**Ollama** ou **modèles locaux Akasha** Qwen3 0.6B / Baguettotron, puis OpenAI/OpenRouter), vault, connecteurs ; génère `llm_router.yaml` et `connectors.env`. En fin de wizard, proposition d'installer les services Docker (Ollama, TTS/STT, BitNet) si le répertoire akasha-models est trouvé (variable AKASHA_MODELS_DIR ou --compose-dir). Si vous choisissez Ollama et qu’il n’est pas détecté, l’app peut ouvrir la page de téléchargement et proposer de télécharger un modèle léger par défaut une fois Ollama installé. |
| `akasha init --defaults` | Initialisation minimale sans questions : Ollama si disponible (avec modèle par défaut), sinon modèles locaux Akasha ; aucun connecteur ; pas de proposition Docker |

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
| `akasha vault delete KEY` | Supprime une clé du vault |

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

### Services Docker (Ollama, TTS/STT, BitNet)

Les services du projet **akasha-models** (Docker Compose) peuvent être installés et démarrés depuis le CLI ; les fichiers `llm_router.yaml` et `voice_router.yaml` sont alors mis à jour pour pointer vers localhost.

| Commande | Description |
|----------|-------------|
| `akasha services install --ollama` | Démarre Ollama (port 11434) et met à jour `llm_router.yaml` |
| `akasha services install --voice` | Démarre TTS + STT (8765, 8766) et crée/met à jour `voice_router.yaml` |
| `akasha services install --bitnet` | Démarre BitNet (8080) et ajoute le provider dans `llm_router.yaml` |
| `akasha services install --all` | Démarre tous les services et met à jour les configs |
| `akasha services install --compose-dir CHEMIN` | Utilise le répertoire indiqué (contenant `docker-compose.yml`) au lieu de la découverte automatique |
| `akasha services status` | Affiche l’état des conteneurs (`docker compose ps`) |
| `akasha services stop` | Arrête les services (`docker compose down`) |

**Répertoire compose** : le CLI cherche le dossier akasha-models dans l’ordre : variable d’environnement **`AKASHA_MODELS_DIR`**, répertoire du binaire + `akasha-models`, répertoire de travail courant + `akasha-models`. Si aucun n’est trouvé, utiliser `--compose-dir`. Voir [akasha-models/README.md](../akasha-models/README.md).

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
| `akasha tui` | Lance la TUI : Chat, Retours planifiés, Routeur, Doc, Tâches, Calendrier, Mémoire |

Pour une description détaillée de la gestion des interfaces (TUI, desktop Tauri, onglets, raccourcis) et des **interfaces matérielles du poste client** (clavier, affichage, souris, accessibilité, prérequis terminal), voir [38_interfaces.md](38_interfaces.md).

---

## 4. Guide d'onboarding (résumé)

1. **Build** : `cargo build`
2. **Initialisation** : `akasha init` (ou `akasha init --defaults` pour le minimal)
3. **Secrets** : si besoin, `akasha vault set ...` pour les tokens non demandés par init
4. **Environnement** : les fichiers `connectors.env` et `akasha.env` sont chargés automatiquement par `akasha start`
5. **Démarrer** : `akasha start` ou `akasha start --foreground`
6. **Vérifier** : `akasha doctor` puis `akasha doctor --advice`

Fichiers créés dans le data_dir (ex. `%USERPROFILE%\akasha` (Windows) ou `~/akasha` (Linux/macOS)) :
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
| `AKASHA_MAX_CONCURRENT_DELEGATIONS` | Nombre max de délégations (sous-tâches) traitées en parallèle ; au-delà, « système surchargé » | 15 |
| `AKASHA_MAX_COST_PER_SESSION_USD` | Plafond de coût LLM (USD) par session ; au-delà, la tâche s'arrête avec « Budget dépassé » | — |
| `AKASHA_MAX_TOKENS_PER_SESSION` | Plafond de tokens par session ; au-delà, la tâche s'arrête avec « Quota dépassé » | — |
| `AKASHA_DATA_DIR` | Répertoire de données (vault, plugins, llm_router.yaml, etc.) | %USERPROFILE%\akasha (Windows) / ~/akasha (Linux/macOS) |
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
| `AKASHA_SYSTEM_TASK_MAX_TOKENS` | Max tokens pour tâches system (décomposition, extraction, compaction). Modèles « thinking » (ex. glm-4.7-flash) peuvent nécessiter 4096+ | 4096 |
| `NATS_URL` | URL du serveur NATS (mode cluster) | nats://127.0.0.1:4222 |
| `OLLAMA_HOST` | URL Ollama si pas de `llm_router.yaml` | http://localhost:11434 |
| `OPENROUTER_API_KEY` | Clé API OpenRouter (permet d’utiliser openrouter en primary même sans section `providers.openrouter`) | — |
| `OPENAI_API_KEY` | Clé API OpenAI (idem pour `providers.openai`) | — |
| `AKASHA_APP_BASE_URL` | URL de base du site des releases (pour la vérification de mise à jour : `api/latest.json`) | https://azerothl.github.io/Akasha_app |
| `AKASHA_LANG` | Langue de l’interface TUI (prioritaire sur `LANG`). Valeurs commençant par `en` = anglais, sinon français | (détection via `LANG` / `LC_ALL`) |

**PowerShell** : `$env:AKASHA_TELEGRAM_ENABLED="1"` (et non `set`).  
**CMD** : `set AKASHA_TELEGRAM_ENABLED=1`.

Les variables définies via `akasha config env set` sont enregistrées dans `data_dir/akasha.env` et chargées au démarrage du daemon.

*Variables optionnelles (debug / avancé)* : `AKASHA_LOG_LLM_RESPONSE=1` (log détaillé des réponses LLM) ; `AKASHA_SPEC_DIR` (chemin vers le dossier `spec` pour la suite d’évals) ; `AKASHA_VAULT_MASTER_KEY` (clé maître pour le vault fichier, si utilisé) ; `AKASHA_LLM_TIMEOUT_SECS` (timeout en secondes pour les appels LLM, défaut 300) ; `AKASHA_LLM_STREAM_IDLE_SECS` (timeout d’inactivité en secondes entre deux chunks, défaut 60) ; `AKASHA_LLM_FIRST_CHUNK_SECS` (délai max pour recevoir le **premier** chunk du modèle embarqué, défaut min(300, AKASHA_LLM_TIMEOUT_SECS) — le premier token peut être lent : chargement modèle, inférence CPU) ; `AKASHA_EMBEDDED_BACKEND` (`auto`, `llama_cpp`, `candle`, `baguettotron`) ; `AKASHA_EMBEDDED_GGUF_PATH` (chemin `.gguf` pour llama-cpp) ; `AKASHA_EMBEDDED_N_GPU_LAYERS` (offload GPU llama-cpp, défaut 99) ; `AKASHA_EMBEDDED_MODEL` (modèle Candle : `qwen3_0_6b` par défaut, `baguettotron` si compilé avec la feature `embedded-baguettotron`). Pour tenter d’accélérer les modèles Candle **sans CUDA** : **`--features embedded-mkl`** (Intel MKL). Si le link échoue avec `undefined symbol: hgemm_`, retirer `embedded-mkl` et utiliser le build par défaut (CPU), l’artifact **`akasha-windows-x86_64-cuda`**, ou Ollama.

### Fichiers de configuration

Pour les **formats, types de données et exemples** de chaque fichier, voir [35_configuration_reference.md](35_configuration_reference.md).

- **llm_router.yaml** : recherché dans l'ordre : data_dir, puis racine du projet. Définit les providers (Ollama, OpenAI, OpenRouter) et les modèles par type de tâche. **Section `providers` vide** : ce n'est pas la cause de timeouts. Le daemon enregistre Ollama (URL = `OLLAMA_HOST` ou découverte auto), le modèle embarqué, et **OpenRouter dès qu’une clé API est disponible** (env `OPENROUTER_API_KEY` ou vault). Pour OpenAI, conserver une section `providers.openai` (avec `api_key_ref`) dans `llm_router.yaml`. Pour les modèles avec « thinking » (ex. glm-4.7-flash) qui renvoient une réponse vide (done_reason: length), augmenter **AKASHA_SYSTEM_TASK_MAX_TOKENS** (défaut 4096). Voir `spec/llm_router.example.yaml`.
- **voice_router.yaml** (dans le data_dir, optionnel) : configuration TTS/STT. Créé automatiquement par `akasha services install --voice` ou par `akasha init` si vous choisissez d’installer les services Docker (option Voice). Sinon, copier `spec/voice_router.example.yaml` vers `data_dir/voice_router.yaml` et renseigner `tts.base_url` et/ou `stt.base_url`. Lorsque STT est configuré, l’interface web affiche un bouton **Message vocal** (micro). Voir [35_configuration_reference.md](35_configuration_reference.md) et [akasha-models/README.md](../akasha-models/README.md).
- **connectors.env** : variables d'activation des connecteurs (chargé par `akasha start`).
- **akasha.env** : variables persistantes (chargé après connectors.env).
- **tools_policy.yaml** (dans le data_dir) : politique de sécurité des **outils machine** (lecture/écriture de fichiers, commandes). Utilisé par l’agent pour `read_file`, `write_file`, `search_files`, `run_command`, etc. Si le fichier est absent, le daemon peut le créer à partir de `spec/tools_policy.example.yaml` au premier démarrage. Pour autoriser l’écriture de fichiers (ex. génération de code sur disque), éditez ce fichier et ajoutez les répertoires sous **allowed_write_paths** (et **allowed_read_paths** pour la lecture). Par défaut, tout est refusé si le fichier est vide ou manquant. Pour que l'agent utilise spontanément la recherche web (météo, actualités, etc.) au lieu de suggérer des sites, activez **web_search_enabled: true** et configurez une clé Brave (`BRAVE_API_KEY` ou vault `brave_api_key`).

### Où sont stockés les modèles

| Type | Emplacement | Remarque |
|------|-------------|----------|
| **Modèles d'embeddings** (mémoire long terme) | `data_dir/embedding_model/` | Utilisé par le daemon (fastembed). Sous-dossiers type `models--<org>--<nom>/`. |
| **Modèles LLM embarqués** (Candle Qwen / Baguettotron) | Cache Hugging Face | Par défaut : **`~/.cache/huggingface/hub`** (Linux/macOS) ou **`%USERPROFILE%\.cache\huggingface\hub`** (Windows). Rediriger avec **`HF_HOME`**. |
| **Modèle GGUF embarqué** (llama-cpp) | `data_dir/models/embedded/default.gguf` | Télécharger avec **`akasha config models embedded-download`** (manifeste `spec/embedded_models.json`). |

Le **data_dir** s'affiche avec `akasha paths` ; par défaut : `~/akasha` (Linux/macOS) ou `%USERPROFILE%\akasha` (Windows), sauf si `AKASHA_DATA_DIR` est défini.

### Windows

Sous **Windows**, le data_dir par défaut est **`%USERPROFILE%\akasha`** (souvent `C:\Users\<user>\akasha`). Les chemins dans `tools_policy.yaml` (allowed_read_paths, allowed_write_paths) utilisent des barres obliques ou des backslashes selon le contexte ; le daemon normalise les chemins. Si le build par défaut du daemon échoue à lier les **embeddings** (mémoire long terme) à cause d’ONNX Runtime (ort_sys), compiler avec **`--no-default-features --features embedded,embeddings-tract`** pour utiliser tract-onnx (pur Rust) à la place de fastembed/ONNX.

**Releases Windows** : télécharger **`akasha-windows-x86_64`** (Candle CPU, fallback) ou **`akasha-windows-x86_64-cuda`** (llama-cpp-4 + GPU NVIDIA, recommandé pour `akasha_embedded` / `akasha_core` rapides). Après installation CUDA : `akasha config models embedded-download`, puis `AKASHA_EMBEDDED_BACKEND=auto` (défaut). Les modèles Candle (Qwen, Baguettotron) utilisent le cache Hugging Face ; définir **`HF_HOME`** (ex. `%USERPROFILE%\akasha\hf_cache`) pour garder le cache dans le data_dir si souhaité.

### Mise à jour

L’application vérifie la dernière version disponible sur le site Akasha (**api/latest.json**) au démarrage du daemon et environ **deux fois par jour** tant que le daemon tourne. Si une mise à jour est disponible, l’interface (Tauri) affiche une bannière proposant de **télécharger** la nouvelle version et rappelle les **étapes pour valider les configs** après installation : vérifier `llm_router.yaml`, `tools_policy.yaml`, `connectors.env` ; relancer le daemon si besoin (`akasha stop` puis `akasha start`) ; lancer `akasha doctor` pour vérifier. L’URL utilisée pour la vérification est configurable via **`AKASHA_APP_BASE_URL`** (défaut : `https://azerothl.github.io/Akasha_app`). En ligne de commande, `akasha update check` affiche si une nouvelle version est disponible ; `akasha update install` ouvre la page de téléchargement dans le navigateur.

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

La langue d’affichage de la TUI (onglets, messages, aide) suit la variable d’environnement **`AKASHA_LANG`** si elle est définie, sinon **`LANG`** ou **`LC_ALL`**. Une valeur commençant par `en` (ex. `en`, `en_US`) affiche l’interface en anglais ; sinon le français est utilisé.

```bash
cargo build -p akasha-cli -p akasha-tui
akasha tui
```

**Onglets** : Chat, **Retours planifiés** (réponses des tâches récurrentes), Routeur (métriques), Doc (documentation utilisateur), Tâches, Calendrier, Mémoire.

Raccourcis TUI :
- **Tab** : changer d’onglet (ordre ci-dessus)
- **Entrée** : envoyer le message (mode Chat)
- **↑ / ↓, PgUp / PgDn, Home / End** : défilement (Chat, Doc, listes)
- **R** : rafraîchir Routeur, Retours planifiés ou liste des tâches selon l’onglet
- **Échap** ou **Ctrl+Q** : quitter — **Tâches** : ↑/↓ (sélection), D (racines seules) — **Mémoire** : / ou S (recherche), G (graphe), D ou Suppr (supprimer entrée long terme)

### Interface web (Tauri)

```bash
cd apps/akasha-ui
npm install
npm run tauri dev
```

L'UI se connecte au daemon sur le port 3876 (configurable via `AKASHA_PORT`). **Onglets** : Chat, **Retours planifiés**, Routeur (métriques), Documentation, Tâches, Calendrier, Mémoire, **Mission**, Paramètres. Touches **1–9** pour changer d’onglet si le focus n’est pas dans un champ (Mission = touche **8**, Paramètres = **9**). **Paramètres** : quatre sections (Affichage, Système, Agent, Data). Dans **Data** : deux sous-onglets — **RAG utilisateur** (documents texte indexés) et **Graphe projet** (un ou plusieurs dossiers de projet indexés dans SQLite + fichiers sous `workspace_graph/out/<id>/`). Dans Agent : sous-onglets pour le profil (Identité, Personnalité, Règles, Autorisé, Interdit), sélecteur de template de personnalité (Neutre, Bienveillant, Concis/technique, Créatif, Strict/sécurisé), réglage **tutoiement / vouvoiement** (`formality` : formel, informel ou défaut — API `GET`/`POST /api/agent-profile`), limites de caractères affichées.

**Mission autonome (interface web uniquement)** : permet de fixer un **objectif** de fond, un **contexte**, des **règles de fonctionnement** et des **rôles** (comme une petite organisation). Tant que le mode est activé et le statut **actif**, le daemon déclenche périodiquement un **heartbeat** : une tâche confiée à l’orchestrateur (type d’agent configurable, défaut *chef de projet* / `project_manager`) qui peut ensuite **déléguer** à d’autres types d’agents selon les rôles décrits. Les rapports d’avancement sont à écrire en Markdown sous le répertoire configuré (`report_dir`, relatif au data_dir). La configuration est persistée dans **`autonomous_mission.yaml`** dans le data_dir ; l’API expose l’état courant via **`GET /api/autonomous-mission`** et **`PUT /api/autonomous-mission`** (champs partiels acceptés). **Journal** : `GET /api/autonomous-mission/events` avec query optionnelle `limit` (défaut 100, max 1000) et `since` (horodatage RFC3339). Pour que le **chat** sur une session reçoive aussi le rappel « ne pas poser de questions » en mode mission, utilisez le même **`session_id`** que celui défini pour la mission (sinon seuls les heartbeats appliquent le contexte mission). **Pause / reprise** : `POST /api/autonomous-mission/pause` et `POST /api/autonomous-mission/resume`. **Exemple** : objectif « tenir à jour un fichier `STATUS.md` dans le dépôt X avec les changements de la semaine », contexte « dépôt cloné sous `projects/X`, branche `main` », horizon moyen, heartbeat toutes les 120 minutes, rapports sous `autonomous_mission/reports` ; après quelques cycles, les fichiers `report_*.md` s’y accumulent. La liste de rôles dans l’interface **guide** l’orchestrateur mais la décomposition concrète reste **interne** à l’orchestrateur (pas d’agents séparés persistés par rôle). Persistance SQLite : snapshot + événements (voir crate `akasha-store`).

**Pièces jointes (interface web uniquement)** : dans le Chat, le bouton « Joindre » permet d’ajouter des images ou des documents (texte, PDF). Les images sont envoyées au modèle (vision) ; les documents texte et PDF sont extraits et inclus dans le message pour l’agent. Utile pour « analyse ce document » ou pour fournir un fichier sans le copier-coller.

**Message vocal (interface web, lorsque STT est configuré)** : si `voice_router.yaml` contient `stt.base_url`, un bouton micro (Message vocal) apparaît à côté du champ de saisie. Premier clic : démarrage de l’enregistrement au micro. Second clic : arrêt, transcription par le service STT, puis envoi du message comme un envoi classique. Les réponses de l’agent peuvent inclure des lecteurs audio lorsque l'agent utilise l'outil TTS (data URL audio dans le markdown). Si la question a été envoyée en vocal et que TTS est configuré (tts.base_url), la réponse est en outre lue automatiquement en audio.

**RAG utilisateur (interface web uniquement)** : dans l’onglet Paramètres → Données → sous-onglet RAG, vous pouvez ajouter ou supprimer des documents (texte). Ces documents sont indexés et les extraits pertinents sont injectés dans le contexte des agents lors des réponses. En TUI ou sans interface web, le RAG utilisateur peut être géré via l’API : `GET/POST/DELETE /api/user-rag/documents`.

**Graphe projet (interface web / desktop)** : sous-onglet **Graphe projet** dans Données. Vous enregistrez des workspaces (nom + chemin racine absolu du dossier) ; chaque indexation produit `graph.json`, `GRAPH_REPORT.md` et `graph.html` sous `workspace_graph/out/<id>/` dans le répertoire de données. Les agents reçoivent automatiquement (profil mémoire enrichi) quelques lignes issues du graphe quand la requête matche des libellés ou chemins indexés ; l’outil **`workspace_graph_search`** permet une recherche ciblée (`--workspace <uuid>` optionnel). API : `GET/POST /api/workspace-graph/workspaces`, `DELETE /api/workspace-graph/workspaces/:id`, `POST .../:id/rebuild`, `GET .../:id/html` (etc.). Détail : [54_workspace_project_knowledge_graph.md](54_workspace_project_knowledge_graph.md).

**Contrat pièces jointes / RAG (API)** :
- **Chat** (`POST /api/message`) : le champ `attachments` attend une liste d’objets `{ "name": "...", "type": "image|document", "content_base64": "...", "mime_type": "..." }`.  
  - `type: "image"` : transmis au modèle sous forme de data URL vision.  
  - `type: "document"` : texte extrait puis injecté dans le message.
- **RAG utilisateur** (`POST /api/user-rag/documents`) : body `{ "name": "...", "content_base64": "...", "mime_type": "..." }` (`content_base64` requis).

**OpenRouter** : pour que l’application apparaisse dans le dashboard OpenRouter (usage, identification), définir `site_url` et `app_title` dans `providers.openrouter` du fichier `llm_router.yaml`, ou les variables d’environnement `OPENROUTER_SITE_URL` et `OPENROUTER_APP_TITLE`. Voir [35_configuration_reference.md](35_configuration_reference.md).

**Différences TUI / Web** : la TUI propose les onglets Chat, **Retours planifiés**, Routeur, Doc, Tâches, Calendrier, Mémoire (pas d’onglets Paramètres ni **Mission**). L’envoi de pièces jointes, le **message vocal** (micro, lorsque STT est configuré), la gestion du RAG utilisateur, des **graphes projet** et l’onglet **Mission autonome** sont réservés à l’interface web ; en TUI, les messages sont envoyés sans pièces jointes ni message vocal, le RAG, les graphes projet et la mission autonome se configurent via l’API (`/api/user-rag/...`, `/api/workspace-graph/...`, `PUT /api/autonomous-mission`) ou l’interface web.

### Flux des demandes (orchestrateur)

L'utilisateur ne parle qu'à l'**agent orchestrateur**. Quand vous envoyez un message (TUI, Web ou canal) :

1. Le daemon répond immédiatement : « Je prends en compte votre demande » avec un `task_id`.
2. La demande est transmise à l'orchestrateur, qui la délègue à l'agent adéquat (ex. conversation / LLM) dans un flux **non bloquant**.
3. Vous pouvez enchaîner plusieurs demandes ; chacune est traitée en parallèle.
4. La réponse finale est disponible via le suivi de la tâche (polling `GET /api/tasks/:id` ou affichage dans le Chat quand la tâche est terminée).

Voir `spec/33_agents_tools_orchestrator_skills.md` pour la feuille de route (outils machine, skills, sous-agents, onglets Agents/Actions). **Liste des outils de base** : `read_file`, `write_file`, `search_files`, `run_command`, `file_diff` — détaillée dans la spec 33 et exposée via **GET /api/tools** (JSON).

### Commandes slash (TUI et interface web)

Dans le chat (TUI et interface web Tauri), les messages commençant par **/** sont interprétés comme des commandes (pas envoyés au LLM). **Les mêmes commandes sont disponibles dans les deux interfaces.** Tapez **/help** ou **/?** pour afficher la liste complète.

| Commande | Description |
|----------|-------------|
| `/help`, `/?` | Aide des commandes (liste complète). |
| `/task create "message"` | Crée une tâche (envoie le message au daemon comme un message chat). |
| `/schedule create NOM INTERVAL_SEC "description"` | Crée une récurrence (ex. rappel périodique). |
| `/schedule delete SCHEDULE_ID` | Supprime une récurrence. |
| `/stop TASK_ID`, `/cancel TASK_ID` | Annule une tâche (en cours ou en attente). |
| `/newsession` | Nouvelle session : contexte court terme effacé, le prochain message repart de zéro. |
| `/status` | État du daemon. |
| `/doctor` | Diagnostic (daemon, Ollama, vault, spec, modèle embarqué) ; si le daemon tourne, affiche les checks côté daemon (embedded_llm, etc.). |
| `/advice` | Conseil diagnostic (RAG + modèle LLM). |
| `/embedded` | Statut du modèle local embarqué (disponible, chargé ou non). |
| `/embedded reload` | Décharge le modèle embarqué (rechargé au prochain appel). |
| `/metrics` | Métriques du routeur LLM. |
| `/models` | Liste des modèles (tous les providers : Ollama, OpenAI, OpenRouter, akasha_embedded, etc.). |
| `/models list` | Modèles par catégorie (primary + fallback). |
| `/models set CATÉGORIE PROVIDER MODÈLE` | Définit le modèle pour une catégorie (ex. `/models set conversation ollama llama3.2`) ; l'ancien primary passe en fallback ; pris en compte immédiatement et sauvegardé. |
| `/routes` | Primary et fallback par catégorie (identique à `/models list`). |
| `/config list` | Variables (akasha.env). |
| `/config get KEY` | Valeur d'une variable. |
| `/config set KEY value` | Définir une variable. |
| `/vault list` | Clés du vault (noms uniquement). |
| `/plugins` | Liste des plugins installés. |
| `/reload` | Recharger les plugins. |
| `/skills`, `/skills list` | Liste des skills installés (nom et description). |
| `/skills reload` | Recharger les skills (data_dir/skills, spec/skills) après ajout ou modification. |
| `/skills uninstall <nom>` | Désinstaller un skill (ex. `/skills uninstall bankr`). |
| `/restart` | Redémarrer le daemon (superviseur). |

Pour ajouter une clé au vault : utiliser le CLI `akasha vault set KEY [value]`. Pour supprimer : `akasha vault delete KEY` ou `DELETE /api/vault` avec body `{"key": "KEY"}` (pas d’équivalent slash pour la sécurité).

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

### Code Studio (API daemon + UI dédiée)

- **Interface** : dépôt **[Akasha-code-studio](https://github.com/azerothl/Akasha-code-studio)** (Vite/React). Spec de référence : `docs/CODE_STUDIO_SPEC.md` dans ce dépôt.
- **Espace disque** : chaque projet = dossier `<data_dir>/studio-projects/<UUID>/` (fichier métadonnées `.akasha-studio.json`).
- **`POST /api/message` (champs optionnels)** :
  - `studio_project_id` : UUID du projet — les outils (`workspace:/`, `run_command`, git) utilisent ce disque comme racine pour la lignée de tâche ;
  - `studio_assigned_agent` : `studio_scaffold` | `studio_frontend` | `studio_backend` | `studio_fullstack` (routage direct vers l’agent spécialisé) ;
  - `studio_evolution_branch` : branche Git active (contexte agent) ;
  - `studio_evolution_id` : si renseigné avec `studio_project_id`, le daemon résout la branche depuis les évolutions enregistrées.
- **API REST studio** : `GET`/`POST /api/studio/projects` (liste avec **nom** lisible + chemin), `PATCH /api/studio/projects/:id` (`{ "name": "…" }` pour renommer l’affichage), `GET /api/studio/projects/:id`, `GET .../files`, `GET .../raw?path=`, `POST .../git/clone`, `POST .../build` (corps JSON `argv`, `timeout_sec`), `GET`/`POST .../evolutions`, `POST .../evolutions/:id/merge`, `POST .../evolutions/:id/abandon`. Limite de parallélisme : variable `AKASHA_STUDIO_MAX_PARALLEL_OPS` (défaut 4).
- **Sandbox** : confinement disque côté daemon pour les chemins studio ; les builds via `/build` exécutent la commande sur la machine (hôte). Pour une isolation forte, utilisez l’outil agent **`run_in_container`** si la politique (`tools_policy.yaml`) l’autorise.
- **Windows / build** : en cas d’erreurs de liaison **ONNX Runtime** (embeddings), compiler avec  
  `cargo build -p akasha-daemon --no-default-features --features embedded,embeddings-tract`  
  (voir `crates/akasha-daemon/Cargo.toml`).

---

## 9. Nouveautés (0.8.0 depuis v0.7.0)

### Mission autonome

- Fichier **`autonomous_mission.yaml`**, boucle **heartbeat** dans le daemon, persistance SQLite (snapshot + événements).
- API : `GET`/`PUT /api/autonomous-mission`, `POST .../pause`, `POST .../resume`, `GET .../events` (`limit`, `since`).
- Onglet **Mission** dans l’UI web ; alignement **`session_id`** avec le chat pour le mode sans questions.

### Graphe projet

- **Multi-workspaces** : plusieurs racines indexées ; artefacts `workspace_graph/out/<uuid>/` ; API `/api/workspace-graph/...` (voir [54_workspace_project_knowledge_graph.md](54_workspace_project_knowledge_graph.md)).
- Enrichissement du prompt (top‑k nœuds) + outil **`workspace_graph_search`** ; améliorations UI (liste, HTML, recherche mémoire).

### Profil agent

- Champ **`formality`** (`formal` | `informal` | omis) dans `agent_profile.json` et **`/api/agent-profile`** ; ligne de prompt dans `personality` / orchestrateur.

### Orchestration, outils, plugins, réponses

- **Strict tools-first** : tentative déterministe d’un premier outil lorsque le routeur l’exige (`akasha-daemon` / boucle d’outils dans `api.rs`).
- **Small talk** : chemins allégés pour les messages légers sans enchaînement LLM inutile.
- **Suggestions de projet** : détection / logs améliorés.
- **Contrats JSON** : validation « contrat significatif » avant strip ; strip des blocs JSON purement contractuels pour le résumé utilisateur (`strip_trailing_contract` / `is_meaningful_contract`).
- **Plugins** : exécution des appels d’outils plugin (`execute_tool_call`) — chemins d’erreur et enchaînements revus.
- **AXI** : textes d’aide **`run_command`** et prompts pointant vers des CLI optionnels token‑efficients ([axi.md](https://axi.md/)) — pas de dépendance packaging.

### Observabilité et release

- **Logs agent** : progression des tâches et sessions (niveaux configurables via `AKASHA_LOG`).
- **`akasha doctor` / `--fix`** : scénarios **binaires + zip** et résolution **`spec/`** (`AKASHA_SPEC_DIR`, à côté du daemon, `data_dir/spec`).
- **CI** : libération d’espace disque runners ; **E2E Playwright** UI (`apps/akasha-ui`) ; workflow **sync version** + `verify-release-version` sur tag (voir [AGENTS.md](../AGENTS.md)).
- **Notes détaillées contributeurs** : [dev/releases/internal_release_0.8.md](dev/releases/internal_release_0.8.md).

---

## 10. Nouveautés de la version 0.7.0 (rappel)

### Orchestration renforcée

- **Route `orchestrator` dans le routeur LLM** : ajoutez un bloc `task_types.orchestrator` dans `llm_router.yaml` pour dédier un modèle à la décomposition de requêtes complexes (multi-agents). Sans cette route, l’orchestrateur utilise automatiquement la route `system` (compatibilité ascendante).
- **Livrables vérifiés** : pour les plans multi-étapes avec `deliverables` (ex. `workspace:/rapport.md`), l’orchestrateur vérifie que les fichiers existent avant de marquer l’étape comme terminée.
- **Sécurité des chemins de livrables** : les chemins absolus et les sorties du workspace (`..`) sont rejetés ; les chemins `workspace:/...` restent obligatoirement résolus dans le workspace autorisé.
- **Retry ciblé si livrable manquant** : lorsqu’un agent termine sans produire un livrable attendu, l’orchestrateur relance une tentative focalisée sur la production du fichier manquant.
- **Trace de plan persistée** : l’orchestrateur écrit un fichier `.akasha/plan_<task_id>.md` dans le workspace à chaque mise à jour du plan — consultable pendant et après l’exécution.
- **Détection de réponses méta** : les sous-agents qui renvoient une réponse méta (ex. « Je suis prêt à commencer la Phase 2 ») au lieu d’un vrai résultat déclenchent automatiquement un retry.

### Sécurité

- **Politique de chemins renforcée** : `can_read`/`can_write` utilisent `Path::starts_with` (comparaison par composant) au lieu d’un préfixe textuel, éliminant la confusion `/data` ↔ `/database`.
- **Livrables absolus rejetés** : les chemins absolus Unix (ex. `/tmp/out.md`) dans les livrables sont rejetés pour éviter qu’un plan puisse « satisfaire » un livrable en pointant vers un fichier système préexistant.
- **Suppression des approbations terminales automatiques** : le fichier `.code-workspace` ne contient plus de règles `chat.tools.terminal.autoApprove` qui activaient silencieusement l’exécution de commandes terminales pour tous les développeurs ouvrant le workspace.

### Robustesse des appels LLM

- **Clamping des entiers de config** : `max_tokens`, `top_k`, `num_ctx`, `num_gpu` supérieurs à `u32::MAX` dans `llm_router.yaml` sont limités à `u32::MAX` (plus de troncature silencieuse).
- **Azure `max_tokens`** : la valeur par défaut du provider Azure OpenAI est alignée à 4 096 (cohérence avec les autres providers OpenAI-compatible).
- **Niveau de log** : la troncature de réponse (due à `max_tokens`) est maintenant journalisée en `WARN` et non `ERROR`.

### Correctifs UI

- **Envoi de documents RAG** : les clés `content_base64` / `mime_type` dans le formulaire d’upload correspondent désormais à la signature Rust de la commande Tauri.
- **Affichage du plan et des livrables** : les vues de sous-agents affichent les étapes du plan et les livrables attendus.
- **Plugins (routing/réputation)** : l’UI expose le statut des plugins, les règles de routage dynamiques, et les actions de reset de réputation (plugin unique ou global) via l’API (`POST /api/plugins/reputation/reset`).
