# Référence des fichiers de configuration

Ce document décrit **tous les fichiers de configuration** utilisés par Akasha : emplacement, format, types de données acceptés et exemples pour chaque cas d’usage. Les exemples complets sont dans `spec/*.example.yaml` et référencés ci‑dessous.

---

## 1. llm_router.yaml

**Emplacement** : `data_dir/llm_router.yaml` (ou racine du projet si absent du data_dir).  
**Format** : YAML.  
**Utilisé par** : daemon (routeur LLM), CLI (`akasha config models`).

### Structure et types

| Section / clé | Type | Obligatoire | Description |
|---------------|------|-------------|-------------|
| `version` | string | Non | Ex. `"1.0"` (indicatif). |
| `global` | objet | Non | Options globales. |
| `global.enable_metrics` | booléen | Non | Activer les métriques (défaut : true). |
| `global.enable_fallback` | booléen | Non | Activer le fallback entre providers (défaut : true). |
| `global.default_timeout_secs` | entier (u64) | Non | Timeout par requête LLM en secondes (défaut : 300). |
| `global.default_max_retries` | entier (u32) | Non | Nombre max de tentatives (défaut : 2). |
| `providers` | objet | Non | Config par provider (clé = nom : `ollama`, `openai`, `openrouter`). |
| `providers.<nom>.base_url` | string | Non | URL de base (ex. `http://localhost:11434` pour Ollama). |
| `providers.<nom>.api_key_ref` | string | Non | Référence Clé API : `vault://nom_cle` (vault), ou nom de clé sans préfixe (vault puis env), ex. `openrouter_api_key` ; sinon variable. Toutes les clés API : vault d'abord, puis variable d'environnement d’env. |
| `providers.<nom>.organization` | string | Non | Ex. OpenAI organization. |
| `providers.<nom>.version` | string | Non | Ex. version API. |
| `model_options` | objet | Non | Métadonnées par modèle (remplies par `akasha config models fetch/add`). |
| `model_options.<modele>.context_length_max` | entier (u64) | Non | Taille max de contexte. |
| `model_options.<modele>.num_ctx` | entier (u64) | Non | Contexte effectif (Ollama). |
| `model_options.<modele>.family` | string | Non | Famille du modèle (ex. `llama`). |
| `model_options.<modele>.parameter_size` | string | Non | Ex. `3B`. |
| `task_types` | objet | Non | Une entrée par type de tâche (voir ci‑dessous). |

**Entrée par type de tâche** (ex. `conversation`, `code_generation`, `system_diagnostic`, `system`) :

| Clé | Type | Obligatoire | Description |
|-----|------|-------------|-------------|
| `primary` | objet | Non | Route principale. |
| `primary.provider` | string | Oui si primary présent | Nom du provider : `ollama`, `openai`, `openrouter`, `akasha_embedded`, `akasha_core`. |
| `primary.model` | string | Oui si primary présent | Nom du modèle (ex. `default`, `llama3.2`, `gpt-4`). |
| `primary.config` | objet | Non | Options libres (max_tokens, temperature, etc.). |
| `fallback` | liste d’objets | Non | Liste de `{ provider, model, config? }` en cas d’échec du primary. |
| `constraints` | objet | Non | Contraintes optionnelles. |
| `constraints.max_cost_per_request` | nombre (f64) | Non | Coût max en USD. |
| `constraints.max_latency_secs` | entier (u64) | Non | Latence max en secondes. |

**Types de tâche reconnus** : `conversation`, `code_generation`, `creative_writing`, `scientific_analysis`, `data_analysis`, `system_diagnostic`, `system` (tâches internes : extraction mémoire, décomposition).

### Exemple complet

Voir [llm_router.example.yaml](llm_router.example.yaml).

### Cas d’usage

- **Premier lancement** : `akasha init` ou `akasha doctor --fix` génère un fichier par défaut (akasha_embedded + akasha_core).
- **Utiliser Ollama** : ajouter `providers.ollama.base_url` et `akasha config models set conversation ollama llama3.2`.
- **Utiliser OpenAI** : `providers.openai.api_key_ref: "vault://openai_api_key"` puis définir la route pour une catégorie.
- **Modèle système (mémoire, décomposition)** : la catégorie `system` doit exister ; par défaut elle pointe vers `akasha_embedded` (voir [06_memory_model.md](06_memory_model.md)).
- **OpenRouter / OpenAI en primary** : le daemon enregistre le provider OpenRouter (resp. OpenAI) dès qu’une clé API est disponible : variable d’environnement `OPENROUTER_API_KEY` (resp. `OPENAI_API_KEY`) ou vault `vault://openrouter_api_key` (resp. `vault://openai_api_key`). Il n’est pas obligatoire d’avoir une section `providers.openrouter` (resp. `providers.openai`) dans `llm_router.yaml` ; définir la route (ex. `task_types.conversation.primary: { provider: openrouter, model: "..." }`) via la TUI ou le fichier suffit une fois la clé définie.

---

## 2. tools_policy.yaml

**Emplacement** : `data_dir/tools_policy.yaml`.  
**Format** : YAML.  
**Utilisé par** : daemon (outils machine : read_file, write_file, run_command, etc.). Fichier absent ou vide = tout refusé.

### Structure et types

| Clé | Type | Obligatoire | Description |
|-----|------|-------------|-------------|
| `allowed_read_paths` | liste de strings | Non (défaut : []) | Préfixes de chemins autorisés pour la lecture (répertoires ou fichiers). |
| `allowed_write_paths` | liste de strings | Non (défaut : []) | Préfixes de chemins autorisés pour l’écriture. |
| `allowed_commands` | liste de strings | Non (défaut : []) | Noms d’exécutables autorisés pour `run_command` (ex. `cargo`, `npm`, `git`). |
| `command_timeout_secs` | entier (u64) | Non (défaut : 0) | Timeout en secondes pour l’exécution d’une commande (ex. 60). |
| `allowed_web_domains` | liste de strings | Non (défaut : []) | Domaines autorisés pour `web_fetch` (feature « web »). |

Les chemins peuvent être relatifs (ex. `.`) ou absolus ; sous Windows, utiliser des backslashes échappés ou des chemins normaux.

### Exemple complet

Voir [tools_policy.example.yaml](tools_policy.example.yaml).

### Cas d’usage

- **Autoriser le répertoire courant** : `allowed_read_paths: ["."]`, `allowed_write_paths: ["."]`.
- **Autoriser des commandes** : `allowed_commands: ["cargo", "npm", "node", "git"]`.
- **Restreindre l’écriture** : n’ajouter que des répertoires précis dans `allowed_write_paths`.

---

## 3. akasha.env

**Emplacement** : `data_dir/akasha.env`.  
**Format** : fichier d’environnement (lignes `KEY=value`, pas de guillemets requis, caractères spéciaux selon les conventions du shell).  
**Utilisé par** : CLI (chargé au spawn du daemon), daemon (GET/POST `/api/config` lisent/écrivent ce fichier).

### Types de données

- Chaque ligne : `CLE=valeur` (espaces autour de `=` possibles selon l’implémentation).
- Valeurs : chaînes ; pas de type numérique côté fichier (le daemon/CLI interprètent selon la clé).

### Clés connues (référence)

| Clé | Type / valeurs | Description |
|-----|----------------|-------------|
| `AKASHA_PORT` | entier | Port HTTP du daemon (défaut : 3876). |
| `AKASHA_LOG` | string | Niveau de log : `trace`, `debug`, `info`, `warn`, `error` (défaut : info). |
| `AKASHA_DATA_DIR` | chemin | Répertoire de données (vault, config, etc.). |
| `AKASHA_MAX_RESPONSE_TOKENS` | entier | Nombre max de tokens pour les réponses chat (défaut : 4096). |
| `AKASHA_SLACK_ENABLED` | `1` / vide | Activer l’adaptateur Slack. |
| `AKASHA_DISCORD_ENABLED` | `1` / vide | Activer le bot Discord. |
| `AKASHA_TELEGRAM_ENABLED` | `1` / vide | Activer le bot Telegram. |
| `AKASHA_TELEGRAM_NOTIFY_CHAT_ID` | string | ID du chat pour notification « bot connecté ». |
| `AKASHA_DEGRADED_MODE` | `1` / vide | Routeur limité aux providers locaux. |
| `AKASHA_CLUSTER_ENABLED` | `1` / vide | Activer le mode cluster (NATS). |
| `AKASHA_NODE_ID` | string | Identifiant du nœud (défaut : HOSTNAME ou UUID). |
| `AKASHA_NATS_TLS_CA`, `AKASHA_NATS_CLIENT_CERT`, `AKASHA_NATS_CLIENT_KEY` | chemin | Certificats mTLS pour NATS. |
| `AKASHA_LLM_TIMEOUT_SECS` | entier | Timeout global des appels LLM (secondes). |
| `AKASHA_LLM_STREAM_IDLE_SECS` | entier | Timeout d’inactivité entre deux chunks (streaming). |
| `AKASHA_LLM_FIRST_CHUNK_SECS` | entier | Délai max pour le premier chunk (modèle embarqué). |
| `AKASHA_EMBEDDED_MODEL` | string | Modèle embarqué : `qwen3_0_6b` (défaut) ou `baguettotron`. |
| `AKASHA_SYSTEM_TASK_MAX_TOKENS` | entier | Nombre max de tokens pour les tâches « system » (décomposition, extraction mémoire, compaction). Défaut : 4096. À augmenter si un modèle avec « thinking » (ex. glm-4.7-flash) renvoie une réponse vide car le thinking consomme tout le budget (done_reason: length). |
| `AKASHA_LOG_LLM_RESPONSE` | `1` / vide | Logger la réponse LLM complète (debug). |
| `AKASHA_VAULT_MASTER_KEY` | string | Clé maître du vault (si utilisé). |
| `AKASHA_SPEC_DIR` | chemin | Dossier `spec` (evals, etc.). |

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

**Emplacement** : `data_dir/cluster.yaml` (ex. `%LOCALAPPDATA%\akasha\cluster.yaml` sous Windows).  
**Format** : YAML.  
**Utilisé par** : mode cluster (NATS, mTLS). Les variables d’environnement `NATS_URL`, `AKASHA_NODE_ID`, `AKASHA_NATS_TLS_CA`, etc. peuvent surcharger ce fichier.

### Structure et types

| Clé | Type | Obligatoire | Description |
|-----|------|-------------|-------------|
| `nats_url` | string | Non | URL NATS (ex. `nats://127.0.0.1:4222`). |
| `node_id` | string | Non | Identifiant du nœud (défaut : HOSTNAME ou UUID). |
| `tls` | objet | Non | mTLS (chemins relatifs au data_dir ou absolus). |
| `tls.ca` | string | Non | Chemin vers le certificat CA. |
| `tls.client_cert` | string | Non | Certificat client. |
| `tls.client_key` | string | Non | Clé privée client. |

### Exemple complet

Voir [cluster.example.yaml](cluster.example.yaml).

### Cas d’usage

- **Cluster local** : `nats_url: "nats://127.0.0.1:4222"`.
- **Avec mTLS** : décommenter et renseigner la section `tls`.

---

## 6. Fichiers de skills (data_dir/skills/*.yaml et spec/skills/*.yaml)

**Emplacement** : `data_dir/skills/*.yaml` et/ou `spec/skills/*.yaml`.  
**Format** : YAML, un fichier par skill.  
**Utilisé par** : daemon (SkillRegistry, GET `/api/skills`).

### Structure et types (exemple)

| Clé | Type | Description |
|-----|------|-------------|
| `name` | string | Identifiant du skill. |
| `description` | string | Description exposée à l’agent. |
| `parameters` | liste | Liste de `{ name, param_type, required }`. |
| `tool_ref` | string | Référence à l’outil (ex. `read_file`). |
| `agents` | liste de strings | Liste d’agents autorisés ; vide = tous. |

### Exemple

Voir [spec/skills/read_file_skill.yaml](skills/read_file_skill.yaml).

---

## Récapitulatif des exemples

| Fichier | Exemple | Description |
|---------|---------|-------------|
| llm_router.yaml | [llm_router.example.yaml](llm_router.example.yaml) | Routeur LLM (global, providers, task_types, system). |
| tools_policy.yaml | [tools_policy.example.yaml](tools_policy.example.yaml) | Politique des outils (chemins, commandes, timeout). |
| cluster.yaml | [cluster.example.yaml](cluster.example.yaml) | Cluster NATS (optionnel, mTLS). |
| akasha.env | (voir section 3 ci‑dessus) | Variables d’environnement (pas de fichier .example). |
| skills | [spec/skills/read_file_skill.yaml](skills/read_file_skill.yaml) | Exemple de skill (read_file). |

---

## Chemins et ordre de recherche

- **data_dir** : par défaut `~/.local/share/akasha` (Linux/macOS) ou `%LOCALAPPDATA%\akasha` (Windows), sauf si `AKASHA_DATA_DIR` est défini. Affiché par `akasha paths`.
- **llm_router.yaml** : recherché dans `data_dir` puis à la racine du projet.
- **tools_policy.yaml**, **akasha.env**, **connectors.env**, **cluster.yaml** : dans `data_dir` uniquement.
