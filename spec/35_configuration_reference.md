# Référence des fichiers de configuration

Ce document décrit **tous les fichiers de configuration** utilisés par Akasha : emplacement, format, types de données acceptés et exemples pour chaque cas d’usage. Les exemples complets sont dans `spec/*.example.yaml` et référencés ci‑dessous.

---

## 1. llm_router.yaml

**Emplacement** : `data_dir/llm_router.yaml` (ou racine du projet si absent du data_dir).  
**Format** : YAML.  
**Utilisé par** : daemon (routeur LLM), CLI (`akasha config models`).

### Structure et types


| Section / clé                               | Type         | Obligatoire | Description                                                                                                                                                                     |
| ------------------------------------------- | ------------ | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `version`                                   | string       | Non         | Ex. `"1.0"` (indicatif).                                                                                                                                                        |
| `global`                                    | objet        | Non         | Options globales.                                                                                                                                                               |
| `global.enable_metrics`                     | booléen      | Non         | Activer les métriques (défaut : true).                                                                                                                                          |
| `global.enable_fallback`                    | booléen      | Non         | Activer le fallback entre providers (défaut : true).                                                                                                                            |
| `global.default_timeout_secs`               | entier (u64) | Non         | Timeout par requête LLM en secondes (défaut : 300).                                                                                                                             |
| `global.default_max_retries`                | entier (u32) | Non         | Nombre max de tentatives (défaut : 2).                                                                                                                                          |
| `providers`                                 | objet        | Non         | Config par provider (clé = nom : `ollama`, `openai`, `openrouter`, `bitnet`).                                                                                                    |
| `providers.<nom>.base_url`                  | string       | Non         | URL de base (ex. `http://localhost:11434` pour Ollama).                                                                                                                         |
| `providers.<nom>.api_key_ref`               | string       | Non         | Référence de la clé API : `vault://nom_cle` (résolution via vault en priorité), ou nom de clé sans préfixe (résolution via variable d'environnement, ex. `openrouter_api_key`). |
| `providers.<nom>.organization`              | string       | Non         | Ex. OpenAI organization.                                                                                                                                                        |
| `providers.<nom>.version`                   | string       | Non         | Ex. version API.                                                                                                                                                                |
| `providers.<nom>.site_url`                  | string       | Non         | (OpenRouter) URL du site pour l’en-tête HTTP-Referer ; sinon env `OPENROUTER_SITE_URL`. Défaut : `https://Akasha.local`.                                                                                         |
| `providers.<nom>.app_title`                 | string       | Non         | (OpenRouter) Nom de l’app pour l’en-tête X-Title ; défaut « Akasha ». Sinon env `OPENROUTER_APP_TITLE`.                                                              |
| `model_options`                             | objet        | Non         | Métadonnées par modèle (remplies par `akasha config models fetch/add`).                                                                                                         |
| `model_options.<modele>.context_length_max` | entier (u64) | Non         | Taille max de contexte.                                                                                                                                                         |
| `model_options.<modele>.num_ctx`            | entier (u64) | Non         | Contexte effectif (Ollama).                                                                                                                                                     |
| `model_options.<modele>.family`             | string       | Non         | Famille du modèle (ex. `llama`).                                                                                                                                                |
| `model_options.<modele>.parameter_size`     | string       | Non         | Ex. `3B`.                                                                                                                                                                       |
| `task_types`                                | objet        | Non         | Une entrée par type de tâche (voir ci‑dessous).                                                                                                                                 |


**Entrée par type de tâche** (ex. `conversation`, `code_generation`, `system_diagnostic`, `system`) :


| Clé                                | Type           | Obligatoire            | Description                                                                           |
| ---------------------------------- | -------------- | ---------------------- | ------------------------------------------------------------------------------------- |
| `primary`                          | objet          | Non                    | Route principale.                                                                     |
| `primary.provider`                 | string         | Oui si primary présent | Nom du provider : `ollama`, `openai`, `openrouter`, `bitnet`, `akasha_embedded`, `akasha_core`. |
| `primary.model`                    | string         | Oui si primary présent | Nom du modèle (ex. `default`, `llama3.2`, `gpt-4`).                                   |
| `primary.config`                   | objet          | Non                    | Options libres (max_tokens, temperature, etc.).                                       |
| `fallback`                         | liste d’objets | Non                    | Liste de `{ provider, model, config? }` en cas d’échec du primary.                    |
| `constraints`                      | objet          | Non                    | Contraintes optionnelles.                                                             |
| `constraints.max_cost_per_request` | nombre (f64)   | Non                    | Coût max en USD.                                                                      |
| `constraints.max_latency_secs`     | entier (u64)   | Non                    | Latence max en secondes.                                                              |


**Types de tâche reconnus** : `conversation`, `code_generation`, `creative_writing`, `scientific_analysis`, `data_analysis`, `system_diagnostic`, `system` (tâches internes : extraction mémoire, décomposition).

### Exemple complet

Voir [llm_router.example.yaml](llm_router.example.yaml).

### Cas d’usage

- **Premier lancement** : `akasha init` ou `akasha doctor --fix` génère un fichier par défaut (akasha_embedded + akasha_core).
- **Utiliser Ollama** : ajouter `providers.ollama.base_url` et `akasha config models set conversation ollama llama3.2`.
- **Utiliser BitNet** (serveur local type llama-server / BitNet) : ajouter `providers.bitnet.base_url: "http://127.0.0.1:8080"` (optionnel, défaut 8080) et définir la route avec `provider: bitnet`, `model: default` (voir [36_bitnet_integration_study.md](36_bitnet_integration_study.md)).
- **Utiliser OpenAI** : `providers.openai.api_key_ref: "vault://openai_api_key"` puis définir la route pour une catégorie.
- **Modèle système (mémoire, décomposition)** : la catégorie `system` doit exister ; par défaut elle pointe vers `akasha_embedded` (voir [06_memory_model.md](06_memory_model.md)).
- **OpenRouter / OpenAI en primary** : le daemon enregistre le provider OpenRouter (resp. OpenAI) dès qu’une clé API est disponible : variable d’environnement `OPENROUTER_API_KEY` (resp. `OPENAI_API_KEY`) ou vault `vault://openrouter_api_key` (resp. `vault://openai_api_key`). Il n’est pas obligatoire d’avoir une section `providers.openrouter` (resp. `providers.openai`) dans `llm_router.yaml` ; définir la route (ex. `task_types.conversation.primary: { provider: openrouter, model: "..." }`) via la TUI ou le fichier suffit une fois la clé définie.
- **Identifier l’app auprès d’OpenRouter** : pour apparaître dans le dashboard OpenRouter, définir `providers.openrouter.site_url` (URL du site, en-tête HTTP-Referer) et `providers.openrouter.app_title` (nom de l’app, en-tête X-Title), ou les variables d’environnement `OPENROUTER_SITE_URL` et `OPENROUTER_APP_TITLE`.

---

## 2. tools_policy.yaml

**Emplacement** : `data_dir/tools_policy.yaml`.  
**Format** : YAML.  
**Utilisé par** : daemon (outils machine : read_file, write_file, run_command, etc.). Fichier absent ou vide = tout refusé.

### Structure et types


| Clé                           | Type             | Obligatoire                      | Description                                                                                                                                                                                         |
| ----------------------------- | ---------------- | -------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `allowed_read_paths`          | liste de strings | Non (défaut : [])                | Préfixes de chemins autorisés pour la lecture (répertoires ou fichiers).                                                                                                                            |
| `allowed_write_paths`         | liste de strings | Non (défaut : [])                | Préfixes de chemins autorisés pour l’écriture.                                                                                                                                                      |
| `allowed_commands`            | liste de strings | Non (défaut : [])                | Noms d’exécutables autorisés pour `run_command` (ex. `cargo`, `npm`, `git`).                                                                                                                        |
| `command_timeout_secs`        | entier (u64)     | Non (défaut : 0)                 | Timeout en secondes pour l’exécution d’une commande (ex. 60).                                                                                                                                       |
| `allowed_web_domains`         | liste de strings | Non (défaut : [])                | Domaines autorisés pour `web_fetch` (feature « web »). Utiliser `["*"]` pour tout autoriser (sous réserve de `blocked_web_domains`).                                                                |
| `blocked_web_domains`         | liste de strings | Non (défaut : [])                | Domaines interdits pour `web_fetch` ; prioritaire sur `allowed_web_domains`.                                                                                                                        |
| `web_search_enabled`          | booléen          | Non (défaut : false)             | Activer la recherche web (Brave API). Clé : vault `brave_api_key` ou env `BRAVE_API_KEY`.                                                                                                           |
| `web_crawl_enabled`          | booléen          | Non (défaut : false)            | Activer le crawl de sites via Cloudflare Browser Rendering (/crawl). Nécessite `cloudflare_account_id` et une clé API (vault ou env).                                                              |
| `cloudflare_account_id`      | string           | Non                             | Identifiant du compte Cloudflare (pour l’URL d’appel à l’API Browser Rendering).                                                                                                                    |
| `cloudflare_api_key_ref`     | string           | Non                             | Référence de la clé API Cloudflare : `vault://cloudflare_api_token` ou nom de variable d’environnement (ex. `CLOUDFLARE_API_TOKEN`).                                                                |
| `tool_profiles`               | objet            | Non                              | Profils d’outils : clé = nom du profil, valeur = liste de noms d’outils (ex. `coding: [read_file, write_file, run_command]`).                                                                       |
| `default_profile`             | string           | Non                              | Nom du profil actif ; si défini, seuls les outils listés dans `tool_profiles[default_profile]` sont autorisés.                                                                                      |
| `allowed_skill_install_hosts` | liste de strings | Non (défaut : GitHub uniquement) | Hôtes autorisés pour `install_skill` (ex. `github.com`, `gitlab.com`, `raw.githubusercontent.com`, `mon-site.com`). Utiliser `["*"]` pour autoriser tout hôte HTTPS. Par défaut : GitHub seulement. |
| `allowed_device_interfaces`   | liste de strings | Non (défaut : [])                | Interfaces appareil autorisées pour `device_discover` / `device_invoke` (caméra, micro, imprimantes, etc.). Valeurs possibles : `local_media`, `system`, `synthetic_input`, `usb`, etc. Utiliser `["*"]` pour tout autoriser (sous réserve de `blocked_device_interfaces`). |
| `blocked_device_interfaces`   | liste de strings | Non (défaut : [])                | Interfaces appareil interdites ; prioritaire sur `allowed_device_interfaces`. Ex. `[usb]` pour tout autoriser sauf USB. |
| `browser_enabled`             | booléen          | Non (défaut : false)             | Activer l’automation navigateur (outil `browser` : navigate, snapshot). Nécessite Node/npm, le runner `scripts/playwright-runner` (ou `AKASHA_PLAYWRIGHT_RUNNER`). Au premier usage, si Chromium Playwright est absent, le daemon lance automatiquement `npm install` puis `npx playwright install chromium` dans ce répertoire (désactiver : env `AKASHA_PLAYWRIGHT_AUTO_INSTALL=0`). |
| `browser_allowed_domains`     | liste de strings | Non (défaut : [])                | Domaines autorisés pour `browser navigate`. Utiliser `["*"]` pour tout autoriser (sous réserve de `browser_blocked_domains`). |
| `browser_blocked_domains`     | liste de strings | Non (défaut : [])                | Domaines interdits pour le navigateur ; prioritaire sur `browser_allowed_domains`. |
| `browser_headless`            | booléen          | Non (défaut : true)              | Lancer le navigateur en mode headless (sans fenêtre). |
| `browser_action_timeout_secs` | entier (u64)     | Non (défaut : 30)                | Timeout en secondes pour chaque action (navigate, etc.). |
| `browser_session_timeout_secs` | entier (u64)   | Non (défaut : 300)               | Timeout de session (une instance par tâche ; session fermée à la fin de la tâche). |


Les chemins peuvent être relatifs (ex. `.`) ou absolus ; sous Windows, utiliser des backslashes échappés ou des chemins normaux.

### Exemple complet

Voir [tools_policy.example.yaml](tools_policy.example.yaml).

### Cas d’usage

- **Autoriser le répertoire courant** : `allowed_read_paths: ["."]`, `allowed_write_paths: ["."]`.
- **Autoriser des commandes** : `allowed_commands: ["cargo", "npm", "node", "git"]`. Utiliser `["*"]` pour autoriser toutes les commandes (sous réserve de `blocked_commands`). Pour les skills qui s’exécutent via une CLI (ex. bankr), ajouter le nom de l’exécutable : `allowed_commands: ["bankr"]` afin que l’agent puisse exécuter `TOOL: bankr whoami`. Lors de l'installation d'un skill, les commandes requises sont ajoutées automatiquement et la politique est rechargée à chaud (pas de redémarrage).
- **Clé du vault dans une commande** : l’agent peut injecter une clé du vault comme variable d’environnement pour `run_command` en préfixant les arguments : `TOOL: run_command VAULT:bankr_api_key=BANKR_API_KEY bankr whoami` (le daemon résout la clé `bankr_api_key` depuis le vault et lance la commande avec `BANKR_API_KEY` définie).
- **Restreindre l’écriture** : n’ajouter que des répertoires précis dans `allowed_write_paths`.
- **Projets longs (roman, BD, projet de code)** : pour que le projet n'impacte pas le reste du système, créer un **répertoire dédié** par projet (ex. `~/akasha_projects/mon_roman`, `~/projets/code/ma_app`) et l'ajouter seul dans `allowed_read_paths` et `allowed_write_paths`. Ne pas autoriser `"."` ou un répertoire parent large si l'on veut isoler. Voir [projects_long_running.md](projects_long_running.md).
- **Initiative recherche web (météo, actualités)** : pour que l'agent utilise spontanément `web_search` pour répondre aux demandes d'information externes (météo, prévisions, actualités, horaires, etc.) au lieu de suggérer des sites à l'utilisateur, définir `web_search_enabled: true` et configurer une clé Brave (variable d'environnement `BRAVE_API_KEY` ou vault `brave_api_key`). Sans cela, l'agent pourra au mieux suggérer des sites ou expliquer comment activer la recherche web.
- **Crawl web optionnel (Cloudflare)** : pour permettre à l'agent de lancer un crawl sur un site entier (ex. documentation, blog) et d'en récupérer le contenu (HTML, Markdown ou JSON), définir `web_crawl_enabled: true`, `cloudflare_account_id` et une clé API (vault ou `CLOUDFLARE_API_TOKEN`). Les domaines crawlables restent limités par `allowed_web_domains` et `blocked_web_domains`. Voir [53_web_crawl_cloudflare.md](53_web_crawl_cloudflare.md).
- **Interfaces appareil (device)** : pour autoriser la découverte et l’invocation d’appareils (caméra, micro, imprimantes, etc.) via `device_discover` et `device_invoke`, définir `allowed_device_interfaces`. Ex. `["*"]` pour tout autoriser (avec `blocked_device_interfaces: [usb]` pour exclure l’USB) ; ou `[local_media, system]` pour uniquement média local et imprimantes ; ou `[local_media, synthetic_input]` pour ajouter les entrées synthétiques (raccourcis clavier, clics/déplacements souris — jeux, logiciels de dessin). Pour `synthetic_input`, il est recommandé d’ajouter `device_invoke` dans `require_approval` afin que l’utilisateur confirme chaque action (human-in-the-loop). Voir [tools_policy.example.yaml](tools_policy.example.yaml).
- **Automation navigateur (spec 39)** : définir `browser_enabled: true` et optionnellement `browser_allowed_domains` / `browser_blocked_domains`. Le daemon peut installer automatiquement les dépendances npm et Chromium Playwright au premier appel à l’outil `browser` (sauf si `AKASHA_PLAYWRIGHT_AUTO_INSTALL=0`). En environnement restreint (sans réseau, sans npm), installer manuellement dans `scripts/playwright-runner` : `npm install` puis `npx playwright install chromium`. Voir [39_browser_automation.md](39_browser_automation.md).

---

## 2b. agent_profile.json

**Emplacement** : `data_dir/agent_profile.json`.  
**Format** : JSON.  
**Utilisé par** : daemon (contexte injecté en tête du prompt LLM), CLI (`akasha init` pour les templates), UI (Paramètres → Profil de l'agent).

Définit l’**identité et la personnalité** de l’agent : nom, ton, règles et contraintes. Ce bloc est formaté par `format_for_prompt()` (ou par la couche personnalité 5 niveaux quand les YAML sont présents) et injecté en tête du contexte à chaque tour de conversation, afin que l’agent adopte ce profil de façon stable. Voir [52_personality_architecture.md](52_personality_architecture.md) pour l'architecture en 5 niveaux et les fichiers spec/personality_core.yaml, spec/personality_modes.yaml, spec/initiative_policy.yaml.

### Structure et types

| Clé          | Type            | Obligatoire | Description                                                                 |
| ------------ | --------------- | ----------- | --------------------------------------------------------------------------- |
| `name`       | string          | Non         | Nom de l’agent (ex. « Akasha », « Assistant »). Si vide ou absent, « Akasha » est utilisé dans le prompt et, à la sauvegarde (POST ou init), persisté par défaut. |
| `personality`| string          | Non         | Description du ton et de la personnalité, **rédigée en anglais**, incluant le **rôle** (ex. « You are a joyful assistant. Upbeat, light humor… »). Pour un comportement stable du modèle, toujours préciser le rôle dans le texte (e.g. « You are a concise, technical assistant… »). |
| `role`       | string          | Non         | Rôle explicite de l’assistant (ex. « joyful assistant », « clever assistant », « friendly advisor »). Renforcé dans le prompt par une ligne « Your role: … ». |
| `gender`     | string          | Non         | Genre pour la cohérence des pronoms : `"male"`, `"female"` ou `"neutral"`. Injecté dans le prompt (he/she/they) si présent. |
| `avatar`     | string          | Non         | URL ou data URL de l’image d’avatar (affichée dans les interfaces à côté des réponses de l’agent). Non utilisé dans le prompt. |
| `rules`      | liste de strings | Non         | Règles à respecter (une par ligne).                                        |
| `can_do`     | liste de strings | Non         | Comportements autorisés.                                                    |
| `cannot_do`   | liste de strings | Non         | Comportements interdits.                                                    |
| `traits_override` | objet (clé → nombre 0–1) | Non | Surcharge des traits (verbosity, warmth, pedagogy, rigor, humor, proactivity, cautiousness, initiative). Vide = valeurs par défaut du personality_core. |
| `preferred_mode` | string | Non | Mode par défaut : `"assistant"`, `"operator"`, `"architect"`, `"onboarding"`. |

### Limites (UI)

L'interface web applique des limites : **name** et **role** 128 caractères, **personality** 2 000, **rules** 30 entrées max (500 car. par règle), **can_do** / **cannot_do** 30 entrées max (300 car. par entrée). Avatar : images jusqu’à 200 Ko. Compteurs affichés dans chaque champ.

### Lien avec le prompt

Le daemon charge le profil au démarrage (et après chaque POST `/api/agent-profile`). À chaque tour, il produit un bloc `[Agent profile and instructions]` en anglais avec : identité (nom toujours présent), rôle (si défini), personnalité, consigne de pronoms (si `gender` défini : he/she/they), règles, can_do, cannot_do. Ce bloc est injecté en tête du contexte système avant la mémoire et le RAG. L’avatar n’est pas inclus dans le prompt ; il sert uniquement à l’affichage dans les interfaces.

### Édition

- **CLI** : `akasha init` propose des templates de personnalité (Neutre, Kind/coach, Concis/technique, Créatif, Strict/sécurisé, Joyful & fun, Friendly advisor, Geek & nerdy) en anglais avec rôle ; écrit `agent_profile.json`.
- **UI** : Paramètres → section « Profil de l’agent » : champs Nom, Rôle, Genre (male/female/neutral), Avatar (image), Personnalité, Règles, Autorisé, Interdit ; sélecteur de template (8 templates) ; bouton Enregistrer (POST `/api/agent-profile`). En Affichage : « Votre avatar » (stocké en localStorage, affiché à côté des messages utilisateur dans le chat). Paramètres organisés en 4 onglets : Affichage, Système, Agent, Data.
- **Manuel** : éditer `data_dir/agent_profile.json` puis redémarrer le daemon (ou envoyer POST pour invalider le cache).

### Exemple

```json
{
  "name": "Akasha",
  "role": "concise technical assistant",
  "personality": "You are a concise, technical assistant. Short, precise answers. No long intros or unnecessary politeness.",
  "gender": "neutral",
  "rules": ["Prioritize concrete output: commands, code snippets, paths.", "Avoid long introductions."],
  "can_do": [],
  "cannot_do": []
}
```

Le champ `avatar` (optionnel) peut contenir une data URL d’image (ex. `data:image/png;base64,...`) pour l’affichage dans le chat.

---

## 2c. voice_router.yaml

**Emplacement** : `data_dir/voice_router.yaml`.  
**Format** : YAML.  
**Utilisé par** : daemon (TTS/STT), API `GET /api/voice/status`, outils `speech_synthesize` et `speech_transcribe`. Fichier optionnel ; s’il est absent ou sans URL, TTS et STT sont désactivés.

### Structure et types

| Section / clé | Type   | Obligatoire | Description |
|---------------|--------|-------------|-------------|
| `tts`         | objet  | Non         | Configuration TTS (synthèse vocale). |
| `tts.base_url` | string | Non        | URL de base du service TTS (ex. `http://localhost:8765`). Le daemon appelle `POST {base_url}/tts` avec body `{"text": "..."}`, réponse = corps binaire WAV. |
| `stt`         | objet  | Non         | Configuration STT (transcription). |
| `stt.base_url` | string | Non        | URL de base du service STT (ex. `http://localhost:8766`). Le daemon envoie l’audio en corps de requête à `POST {base_url}/stt`, réponse JSON `{"text": "..."}`. |

TTS est considéré configuré si `tts.base_url` est défini et non vide ; idem pour STT avec `stt.base_url`. L’interface web affiche le bouton « Message vocal » (micro) dans le chat lorsque STT est configuré ; si TTS est aussi configuré, la réponse à un message vocal est affichée en texte et lue automatiquement en audio.

### Exemple complet

Voir [voice_router.example.yaml](voice_router.example.yaml).

### Cas d’usage

- **Activer TTS** : copier `spec/voice_router.example.yaml` vers `data_dir/voice_router.yaml`, renseigner `tts.base_url` (ex. Pocket TTS, moshi-server TTS sur le port 8765). Les réponses de l’agent peuvent alors inclure des pièces jointes audio (data URL) via l’outil `speech_synthesize`.
- **Activer STT** : renseigner `stt.base_url` (ex. moshi-server STT sur le port 8766). Le bouton message vocal apparaît dans l’interface web ; l’utilisateur peut enregistrer au micro puis envoyer le message transcrit. L’outil `speech_transcribe` est disponible pour l’agent.
- **Services externes** : TTS et STT sont des services HTTP externes (Kyutai, Pocket TTS, etc.) ; voir [akasha-models/README.md](../akasha-models/README.md) pour Docker Compose et URLs.

---

## 3. akasha.env

**Emplacement** : `data_dir/akasha.env`.  
**Format** : fichier d’environnement (lignes `KEY=value`, pas de guillemets requis, caractères spéciaux selon les conventions du shell).  
**Utilisé par** : CLI (chargé au spawn du daemon), daemon (GET/POST `/api/config` lisent/écrivent ce fichier).

### Types de données

- Chaque ligne : `CLE=valeur` (espaces autour de `=` possibles selon l’implémentation).
- Valeurs : chaînes ; pas de type numérique côté fichier (le daemon/CLI interprètent selon la clé).

### Clés connues (référence)


| Clé                                                                       | Type / valeurs | Description                                                                                                                                                                                                                                                            |
| ------------------------------------------------------------------------- | -------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `AKASHA_PORT`                                                             | entier         | Port HTTP du daemon (défaut : 3876).                                                                                                                                                                                                                                   |
| `AKASHA_LOG`                                                              | string         | Niveau de log : `trace`, `debug`, `info`, `warn`, `error` (défaut : info).                                                                                                                                                                                             |
| `AKASHA_DATA_DIR`                                                         | chemin         | Répertoire de données (vault, config, etc.).                                                                                                                                                                                                                           |
| `AKASHA_MAX_RESPONSE_TOKENS`                                              | entier         | Nombre max de tokens pour les réponses chat (défaut : 4096).                                                                                                                                                                                                           |
| `AKASHA_SLACK_ENABLED`                                                    | `1` / vide     | Activer l’adaptateur Slack.                                                                                                                                                                                                                                            |
| `AKASHA_DISCORD_ENABLED`                                                  | `1` / vide     | Activer le bot Discord.                                                                                                                                                                                                                                                |
| `AKASHA_TELEGRAM_ENABLED`                                                 | `1` / vide     | Activer le bot Telegram.                                                                                                                                                                                                                                               |
| `AKASHA_TELEGRAM_NOTIFY_CHAT_ID`                                          | string         | ID du chat pour notification « bot connecté ».                                                                                                                                                                                                                         |
| `AKASHA_DEGRADED_MODE`                                                    | `1` / vide     | Routeur limité aux providers locaux.                                                                                                                                                                                                                                   |
| `AKASHA_CLUSTER_ENABLED`                                                  | `1` / vide     | Activer le mode cluster (NATS).                                                                                                                                                                                                                                        |
| `AKASHA_NODE_ID`                                                          | string         | Identifiant du nœud (défaut : HOSTNAME ou UUID).                                                                                                                                                                                                                       |
| `AKASHA_NATS_TLS_CA`, `AKASHA_NATS_CLIENT_CERT`, `AKASHA_NATS_CLIENT_KEY` | chemin         | Certificats mTLS pour NATS.                                                                                                                                                                                                                                            |
| `AKASHA_LLM_TIMEOUT_SECS`                                                 | entier         | Timeout global des appels LLM (secondes).                                                                                                                                                                                                                              |
| `AKASHA_LLM_STREAM_IDLE_SECS`                                             | entier         | Timeout d’inactivité entre deux chunks (streaming).                                                                                                                                                                                                                    |
| `AKASHA_LLM_FIRST_CHUNK_SECS`                                             | entier         | Délai max pour le premier chunk (modèle embarqué).                                                                                                                                                                                                                     |
| `AKASHA_EMBEDDED_MODEL`                                                   | string         | Modèle embarqué : `qwen3_0_6b` (défaut) ou `baguettotron`.                                                                                                                                                                                                             |
| `AKASHA_SYSTEM_TASK_MAX_TOKENS`                                           | entier         | Nombre max de tokens pour les tâches « system » (décomposition, extraction mémoire, compaction). Défaut : 4096. À augmenter si un modèle avec « thinking » (ex. glm-4.7-flash) renvoie une réponse vide car le thinking consomme tout le budget (done_reason: length). |
| `AKASHA_LOG_LLM_RESPONSE`                                                 | `1` / vide     | Logger la réponse LLM complète (debug).                                                                                                                                                                                                                                |
| `AKASHA_VAULT_MASTER_KEY`                                                 | string         | Clé maître du vault (si utilisé).                                                                                                                                                                                                                                      |
| `AKASHA_SPEC_DIR`                                                         | chemin         | Dossier `spec` (evals, etc.).                                                                                                                                                                                                                                          |


### Exemple

```env
AKASHA_PORT=3876
AKASHA_LOG=info
AKASHA_TELEGRAM_ENABLED=1
```

### Cas d’usage

- **Modifier des variables** : `akasha config env set AKASHA_PORT 4000` ou édition manuelle de `akasha.env`.
- **Lister / lire** : `akasha config env list`, `akasha config env get AKASHA_PORT`.

---

## 4. connectors.env

**Emplacement** : `data_dir/connectors.env`.  
**Format** : fichier d’environnement (lignes `KEY=value`).  
**Utilisé par** : CLI (chargé au spawn du daemon avec les autres variables). Contenu libre selon les connecteurs (Telegram, Slack, Discord) ; le daemon lit les variables d’environnement déjà chargées.

### Cas d’usage

- Généré ou complété par `akasha init` ; peut contenir des variables spécifiques connecteurs (ex. `AKASHA_TELEGRAM_ENABLED=1`). Pas de schéma strict côté application.

---

## 5. cluster.yaml (optionnel)

**Emplacement** : `data_dir/cluster.yaml` (ex. `%USERPROFILE%\akasha\cluster.yaml` sous Windows).  
**Format** : YAML.  
**Utilisé par** : mode cluster (NATS, mTLS). Les variables d’environnement `NATS_URL`, `AKASHA_NODE_ID`, `AKASHA_NATS_TLS_CA`, etc. peuvent surcharger ce fichier.

### Structure et types


| Clé               | Type   | Obligatoire | Description                                      |
| ----------------- | ------ | ----------- | ------------------------------------------------ |
| `nats_url`        | string | Non         | URL NATS (ex. `nats://127.0.0.1:4222`).          |
| `node_id`         | string | Non         | Identifiant du nœud (défaut : HOSTNAME ou UUID). |
| `tls`             | objet  | Non         | mTLS (chemins relatifs au data_dir ou absolus).  |
| `tls.ca`          | string | Non         | Chemin vers le certificat CA.                    |
| `tls.client_cert` | string | Non         | Certificat client.                               |
| `tls.client_key`  | string | Non         | Clé privée client.                               |


### Exemple complet

Voir [cluster.example.yaml](cluster.example.yaml).

### Cas d’usage

- **Cluster local** : `nats_url: "nats://127.0.0.1:4222"`.
- **Avec mTLS** : décommenter et renseigner la section `tls`.

---

## 6. Fichiers de skills (data_dir/skills et spec/skills)

**Emplacement** : `data_dir/skills/` et/ou `spec/skills/`.  
**Utilisé par** : daemon (SkillRegistry, GET `/api/skills`, POST `/api/skills/reload`).

Deux formats supportés (alignés sur la [spécification Agent Skills](https://agentskills.io/specification)) : (1) **répertoire** avec `SKILL.md` (front matter YAML + corps Markdown) ; (2) **fichier** `.yaml` / `.yml` par skill (rétrocompatibilité). Rechargement à chaud : **POST /api/skills/reload** ou commande `/skills reload` (TUI/Web), à l’installation d’un skill, les commandes requises sont ajoutées automatiquement à `allowed_commands` ; redémarrer le daemon pour prendre en compte la nouvelle politique.

### Structure et types (exemple)


| Clé           | Type             | Description                                |
| ------------- | ---------------- | ------------------------------------------ |
| `name`        | string           | Identifiant du skill.                      |
| `description` | string           | Description exposée à l’agent.             |
| `parameters`  | liste            | Liste de `{ name, param_type, required }`. |
| `tool_ref`    | string           | Référence à l’outil (ex. `read_file`).     |
| `agents`      | liste de strings | Liste d’agents autorisés ; vide = tous.    |


### Exemple

Voir [spec/skills/read_file_skill.yaml](skills/read_file_skill.yaml).

---

## Récapitulatif des exemples


| Fichier           | Exemple                                                         | Description                                          |
| ----------------- | --------------------------------------------------------------- | ---------------------------------------------------- |
| llm_router.yaml   | [llm_router.example.yaml](llm_router.example.yaml)              | Routeur LLM (global, providers, task_types, system).  |
| tools_policy.yaml | [tools_policy.example.yaml](tools_policy.example.yaml)           | Politique des outils (chemins, commandes, timeout).  |
| voice_router.yaml | [voice_router.example.yaml](voice_router.example.yaml)           | Voix TTS/STT (URLs des services synthèse et transcription). |
| cluster.yaml      | [cluster.example.yaml](cluster.example.yaml)                     | Cluster NATS (optionnel, mTLS).                       |
| akasha.env        | (voir section 3 ci‑dessus)                                      | Variables d’environnement (pas de fichier .example). |
| skills            | [spec/skills/read_file_skill.yaml](skills/read_file_skill.yaml)  | Exemple de skill (read_file).                         |


---

## Chemins et ordre de recherche

- **data_dir** : par défaut `~/akasha` (Linux/macOS) ou `%USERPROFILE%\akasha` (Windows), sauf si `AKASHA_DATA_DIR` est défini. Affiché par `akasha paths`.
- **llm_router.yaml** : recherché dans `data_dir` puis à la racine du projet.
- **tools_policy.yaml**, **voice_router.yaml**, **akasha.env**, **connectors.env**, **cluster.yaml** : dans `data_dir` uniquement.

