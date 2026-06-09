# Configuration

## ## 2. Où se trouve la configuration


Tous les fichiers de configuration sont dans le **répertoire de données** (data_dir). Pour connaître son emplacement :

```
akasha paths
```

Affichage typique :  
- **Windows** : `C:\Users\<Vous>\akasha` (ou `%USERPROFILE%\akasha`)  
- **Linux / macOS** : `~/akasha`

Vous pouvez modifier les fichiers suivants dans ce répertoire (avec un éditeur de texte) :

| Fichier | Rôle |
|--------|------|
| `llm_router.yaml` | Fournisseurs LLM (Ollama, OpenAI, OpenRouter) et modèles par type de tâche (conversation, code_generation, etc.). |
| `tools_policy.yaml` | Autorisations des outils : chemins lecture/écriture (`allowed_read_paths`, `allowed_write_paths`), commandes autorisées (`allowed_commands`), recherche web (Brave), hôtes pour l'installation de skills (`allowed_skill_install_hosts`). |
| `voice_router.yaml` | (Optionnel) Voix TTS/STT : URLs des services de synthèse (`tts.base_url`) et de transcription (`stt.base_url`). Si STT est configuré, l’interface web affiche un bouton **Message vocal** (micro). Si TTS est aussi configuré, la réponse à un message vocal est affichée en texte et lue en audio. |
| `akasha.env` | Variables d'environnement persistantes (éditables aussi via `akasha config env`). |
| `connectors.env` | Activation des canaux (Telegram, Slack, Discord). |
| `agent_profile.json` | Profil de l'agent : nom, rôle, personnalité, règles, **formalité** (tutoiement / vouvoiement, champs optionnels). Éditable dans Paramètres → Profil de l'agent (interface web) ou en modifiant le fichier puis en redémarrant le daemon. |
| `autonomous_mission.yaml` | (Optionnel) **Mission autonome** : objectif, contexte, règles de fonctionnement, rôles, intervalle de heartbeat, répertoire des rapports, `session_id`, type d’agent pour le premier pas de chaque heartbeat. Éditable dans l’onglet **Mission** de l’interface web ou via `GET` / `PUT /api/autonomous-mission`. |

Le répertoire de données est créé automatiquement par `akasha init` ou `akasha doctor --fix` s'il est absent.

### Dossier `spec` à côté des binaires

Les archives de release incluent un sous-dossier **`spec/`** avec des **fichiers d'exemple** (politique d'outils, routeur vocal, routeur LLM) utilisés par `akasha init` pour générer la configuration par défaut lorsque ces fichiers sont présents. Le daemon résout le dossier `spec` utilisé à l'exécution dans cet ordre : variable d'environnement **`AKASHA_SPEC_DIR`** (si elle pointe vers un répertoire existant) → **`spec/` à côté du binaire `akasha-daemon`** → **`data_dir/spec`** s'il existe → sinon le chemin relatif **`spec`** (installation depuis les sources). La commande **`akasha paths`** affiche le chemin retenu et la source (variable, binaire, données, ou relatif).

L'onglet **Doc** des interfaces charge le guide depuis **`docs/user_guide.md`** à côté des binaires (puis, si besoin, depuis `data_dir/docs/user_guide.md`).

---

## 2bis. Exemples et référence exhaustive des propriétés de configuration

Cette section complète la vue d’ensemble de la section 2 avec des **exemples de fichiers** et un **tableau exhaustif des propriétés** que vous pouvez définir dans les fichiers de configuration utilisateur.

### `llm_router.yaml`

Exemple minimal :

```yaml
version: "1.0"
global:
  enable_metrics: true
  enable_fallback: true
  default_timeout_secs: 300
  default_max_retries: 2
providers:
  ollama:
    base_url: "http://localhost:11434"
  openai:
    api_key_ref: "vault://openai_api_key"
task_types:
  conversation:
    primary:
      provider: akasha_embedded
      model: default
    fallback:
      - provider: akasha_core
        model: core
```

Propriétés supportées :

| Propriété | Type | Description |
|---|---|---|
| `version` | string | Version indicative du fichier. |
| `global.enable_metrics` | bool | Active les métriques du routeur. |
| `global.enable_fallback` | bool | Active le fallback entre providers. |
| `global.default_timeout_secs` | int | Timeout par requête LLM (secondes). |
| `global.default_max_retries` | int | Nombre de retries par requête. |
| `providers.<nom>.base_url` | string | URL du provider (`ollama`, `openai`, `azure_openai`, `openrouter`, `bitnet`). |
| `providers.<nom>.api_key_ref` | string | Référence de clé (`vault://...` ou nom de variable d’environnement). |
| `providers.<nom>.organization` | string | Organisation (provider compatible). |
| `providers.<nom>.version` | string | Version API (provider compatible). |
| `providers.openrouter.site_url` | string | HTTP-Referer OpenRouter (sinon `OPENROUTER_SITE_URL`). |
| `providers.openrouter.app_title` | string | X-Title OpenRouter (sinon `OPENROUTER_APP_TITLE`). |
| `model_options.<modele>.context_length_max` | int | Contexte max du modèle. |
| `model_options.<modele>.num_ctx` | int | Contexte effectif (notamment Ollama). |
| `model_options.<modele>.family` | string | Famille du modèle (ex. llama). |
| `model_options.<modele>.parameter_size` | string | Taille paramètre (ex. `3B`). |
| `task_types.<type>.primary.provider` | string | Provider principal (`akasha_embedded`, `akasha_core`, `ollama`, `openai`, `azure_openai`, `openrouter`, `bitnet`). |
| `task_types.<type>.primary.model` | string | Modèle principal. |
| `task_types.<type>.primary.config.<clé>` | objet libre | Paramètres provider (ex. `temperature`, `top_p`, `top_k`, `max_tokens`, `thinking_level`, etc.). |
| `task_types.<type>.fallback[]` | liste | Routes de secours (`provider`, `model`, `config?`). |
| `task_types.<type>.constraints.max_cost_per_request` | number | Coût max/requête. |
| `task_types.<type>.constraints.max_latency_secs` | int | Latence max/requête. |

Types de tâche reconnus : `conversation`, `code_generation`, `creative_writing`, `scientific_analysis`, `data_analysis`, `system_diagnostic`, `system`, `orchestrator`.

### `tools_policy.yaml`

Exemple minimal :

```yaml
allowed_read_paths:
  - "."
allowed_write_paths:
  - "."
allowed_commands:
  - "git"
  - "cargo"
command_timeout_secs: 60
web_search_enabled: false
browser_enabled: false
```

Propriétés supportées :

| Propriété | Type | Description |
|---|---|---|
| `allowed_read_paths` | list[string] | Préfixes autorisés en lecture. |
| `allowed_write_paths` | list[string] | Préfixes autorisés en écriture. |
| `allowed_commands` | list[string] | Exécutables autorisés (`run_command`). |
| `blocked_commands` | list[string] | Commandes interdites (prioritaires). |
| `command_timeout_secs` | int | Timeout commandes shell. |
| `run_command_default_cwd_workspace` | bool | Définit le cwd par défaut au workspace de tâche. |
| `allowed_web_domains` | list[string] | Domaines autorisés pour `web_fetch`. |
| `blocked_web_domains` | list[string] | Domaines interdits pour `web_fetch` (prioritaires). |
| `web_search_enabled` | bool | Active la recherche web (Brave). |
| `web_crawl_enabled` | bool | Active le crawl web Cloudflare (`web_crawl`). |
| `cloudflare_account_id` | string | Account ID Cloudflare Browser Rendering. |
| `cloudflare_api_key_ref` | string | Clé API Cloudflare (vault/env). |
| `tool_profiles` | map[string,list[string]] | Profils d’outils nommés. |
| `default_profile` | string | Profil d’outils actif par défaut. |
| `allowed_skill_install_hosts` | list[string] | Hôtes autorisés pour `install_skill`. |
| `allowed_device_interfaces` | list[string] | Interfaces appareil autorisées (`device_discover`/`device_invoke`). |
| `blocked_device_interfaces` | list[string] | Interfaces appareil interdites (prioritaires). |
| `browser_enabled` | bool | Active l’outil navigateur (`browser`). |
| `browser_allowed_domains` | list[string] | Domaines autorisés pour `browser navigate`. |
| `browser_blocked_domains` | list[string] | Domaines interdits navigateur (prioritaires). |
| `browser_headless` | bool | Exécution headless Playwright. |
| `browser_action_timeout_secs` | int | Timeout par action navigateur. |
| `browser_session_timeout_secs` | int | Timeout session navigateur. |
| `require_approval` | list[string] | Outils nécessitant approbation utilisateur. |

### `voice_router.yaml` (optionnel)

Exemple minimal :

```yaml
tts:
  base_url: "http://localhost:8765"
stt:
  base_url: "http://localhost:8766"
```

Propriétés supportées :

| Propriété | Type | Description |
|---|---|---|
| `tts.base_url` | string | URL du service TTS (`POST /tts`). |
| `stt.base_url` | string | URL du service STT (`POST /stt`). |

### `agent_profile.json`

Exemple minimal :

```json
{
  "name": "Akasha",
  "role": "concise technical assistant",
  "personality": "You are a concise technical assistant.",
  "gender": "neutral",
  "formality": "formal",
  "rules": [],
  "can_do": [],
  "cannot_do": []
}
```

Propriétés supportées :

| Propriété | Type | Description |
|---|---|---|
| `name` | string | Nom agent (défaut produit : `Akasha`). |
| `personality` | string | Description de ton/personnalité (anglais recommandé). |
| `role` | string | Rôle explicite de l’agent. |
| `gender` | string | `male`, `female`, `neutral`. |
| `formality` | string/null | `formal`, `informal`, ou null. |
| `avatar` | string | URL/data URL avatar (affichage UI). |
| `rules` | list[string] | Règles comportementales. |
| `can_do` | list[string] | Comportements autorisés. |
| `cannot_do` | list[string] | Comportements interdits. |
| `traits_override` | map[string,number] | Surcharge traits (0..1). |
| `preferred_mode` | string | `assistant`, `operator`, `architect`, `onboarding`. |

### `autonomous_mission.yaml` (optionnel)

Exemple minimal :

```yaml
enabled: false
global_context: ""
horizon: medium
objective: ""
heartbeat_interval_minutes: 120
report_dir: autonomous_mission/reports
session_id: autonomous:default
status: paused
```

Propriétés supportées :

| Propriété | Type | Description |
|---|---|---|
| `enabled` | bool | Active la mission autonome. |
| `global_context` | string | Contexte global injecté en mission. |
| `horizon` | string | `short`, `medium`, `long`. |
| `objective` | string | Objectif mission. |
| `heartbeat_interval_minutes` | int | Fréquence des cycles mission. |
| `report_dir` | string | Dossier de rapports (relatif data_dir). |
| `session_id` | string | Session associée à la mission. |
| `status` | string | `active`, `paused`, `completed`. |
| `operating_rules` | string | Règles d’exécution mission. |
| `role_definitions` | list[object] | Rôles d’orchestration (nom, responsabilité, type préféré). |
| `heartbeat_preferred_task_type` | string | Type d’agent privilégié au heartbeat. |

### `akasha.env`

Exemple minimal :

```env
AKASHA_PORT=3876
AKASHA_LOG=info
AKASHA_SYSTEM_TASK_MAX_TOKENS=4096
AKASHA_TELEGRAM_ENABLED=1
```

Clés supportées (principales et documentées) :

| Clé | Type / valeurs | Description |
|---|---|---|
| `AKASHA_PORT` | int | Port HTTP daemon. |
| `AKASHA_LOG` | string | Niveau log (`trace`,`debug`,`info`,`warn`,`error`). |
| `AKASHA_DATA_DIR` | path | Répertoire données. |
| `AKASHA_MAX_RESPONSE_TOKENS` | int | Tokens max réponses chat. |
| `AKASHA_SYSTEM_TASK_MAX_TOKENS` | int | Budget tokens tâches internes. |
| `AKASHA_LLM_TIMEOUT_SECS` | int | Timeout global LLM. |
| `AKASHA_LLM_STREAM_IDLE_SECS` | int | Timeout inactivité stream LLM. |
| `AKASHA_LLM_FIRST_CHUNK_SECS` | int | Timeout premier chunk stream LLM. |
| `AKASHA_LOG_LLM_RESPONSE` | `1`/vide | Log réponse LLM brute. |
| `AKASHA_EMBEDDED_MODEL` | string | Modèle embarqué (`qwen3_0_6b`, `baguettotron`). |
| `AKASHA_VAULT_MASTER_KEY` | string | Clé maître vault. |
| `AKASHA_SPEC_DIR` | path | Dossier `spec` forcé. |
| `AKASHA_APP_BASE_URL` | URL | Base URL vérification release. |
| `AKASHA_TELEGRAM_ENABLED` | `1`/vide | Active Telegram. |
| `AKASHA_SLACK_ENABLED` | `1`/vide | Active Slack. |
| `AKASHA_DISCORD_ENABLED` | `1`/vide | Active Discord. |
| `AKASHA_TELEGRAM_NOTIFY_CHAT_ID` | string | Chat de notification Telegram. |
| `AKASHA_DEGRADED_MODE` | `1`/vide | Mode dégradé local. |
| `AKASHA_CLUSTER_ENABLED` | `1`/vide | Active mode cluster. |
| `AKASHA_NODE_ID` | string | Identifiant nœud cluster. |
| `AKASHA_NATS_TLS_CA` | path | CA NATS TLS. |
| `AKASHA_NATS_CLIENT_CERT` | path | Certificat client NATS. |
| `AKASHA_NATS_CLIENT_KEY` | path | Clé client NATS. |

### `connectors.env`

Exemple minimal :

```env
AKASHA_TELEGRAM_ENABLED=1
AKASHA_SLACK_ENABLED=
AKASHA_DISCORD_ENABLED=
```

Propriétés supportées :

| Clé | Type / valeurs | Description |
|---|---|---|
| `AKASHA_TELEGRAM_ENABLED` | `1`/vide | Active le connecteur Telegram. |
| `AKASHA_SLACK_ENABLED` | `1`/vide | Active le connecteur Slack. |
| `AKASHA_DISCORD_ENABLED` | `1`/vide | Active le connecteur Discord. |

### Où trouver les exemples complets officiels

- `llm_router.yaml`
- `tools_policy.yaml`
- `autonomous_mission.yaml`
- cette documentation

---



## ## 5. Variables d'environnement utiles


| Variable | Description | Défaut |
|----------|-------------|--------|
| `AKASHA_PORT` | Port du daemon | 3876 |
| `AKASHA_DATA_DIR` | Répertoire de données | %USERPROFILE%\akasha (Windows) / ~/akasha (Linux/macOS) |
| `AKASHA_LOG` | Niveau de log (trace, debug, info, warn, error) | info |
| `AKASHA_LANG` | Langue de l'interface TUI (`en` = anglais, sinon français) | Détection via LANG / LC_ALL |
| `AKASHA_MAX_RESPONSE_TOKENS` | Nombre max de tokens pour les réponses chat | 4096 |
| `AKASHA_APP_BASE_URL` | URL du site des releases (pour `akasha update check`) | https://azerothl.github.io/Akasha_app |
| `AKASHA_SYSTEM_TASK_MAX_TOKENS` | Tokens max pour les tâches système (mémoire, décomposition). À augmenter (ex. 8192) si un modèle « thinking » renvoie des réponses vides | 4096 |
| `AKASHA_PLUGIN_SELECT_MAX_TOKENS` | Tokens max pour la réponse JSON du sélecteur de plugins (route `system`) | 512 |
| `OLLAMA_HOST` | URL d'Ollama si non configuré ailleurs | http://localhost:11434 |
| `AKASHA_TELEGRAM_ENABLED` | `1` pour activer Telegram | — |
| `AKASHA_DISCORD_ENABLED` | `1` pour activer Discord | — |
| `AKASHA_SLACK_ENABLED` | `1` pour activer Slack | — |
| `OPENROUTER_API_KEY` | Clé API OpenRouter | — |
| `OPENAI_API_KEY` | Clé API OpenAI | — |

Les variables définies via `akasha config env set` sont enregistrées dans le fichier `akasha.env` du data_dir et rechargées au démarrage du daemon. Pour qu'OpenRouter affiche votre usage dans son dashboard, définissez `OPENROUTER_SITE_URL` et `OPENROUTER_APP_TITLE` (ou les champs correspondants dans `llm_router.yaml`).

---

## ## 8. Politique des outils (tools_policy.yaml)


Le fichier **tools_policy.yaml** dans le data_dir contrôle ce que l'agent peut faire sur votre machine :

- **allowed_read_paths** / **allowed_write_paths** : répertoires ou fichiers autorisés en lecture et en écriture. Par défaut (fichier vide ou absent), tout est refusé. Ex. `["."]` pour le répertoire courant, ou des chemins précis pour limiter l'accès.
- **allowed_commands** : noms d'exécutables autorisés pour les commandes (ex. `cargo`, `npm`, `git`). Les commandes requises par les skills installés sont ajoutées automatiquement.
- **web_search_enabled** : `true` pour que l'agent utilise la recherche web (Brave API) pour répondre aux questions (météo, actualités, etc.). Nécessite une clé : `akasha vault set brave_api_key VOTRE_CLÉ` ou variable `BRAVE_API_KEY`.
- **allowed_skill_install_hosts** : hôtes autorisés pour l'installation de skills (défaut : GitHub uniquement). Ex. `["*"]` pour tout hôte HTTPS.

En cas de fichier absent, `akasha doctor --fix` crée un fichier minimal ; éditez-le selon vos besoins.

---
