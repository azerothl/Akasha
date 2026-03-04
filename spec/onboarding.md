# Premier lancement — Onboarding Akasha

Ce guide décrit les étapes pour faire tourner Akasha après un clone ou une installation.

## 1. Prérequis

- **Rust** 1.70+ (`rustup.rs`)
- **Node.js** 18+ et npm (pour l’UI Tauri)
- Optionnel : **Ollama** (pour les réponses LLM locales)

## 2. Build

À la racine du projet :

```bash
cargo build
```

Les binaires sont dans `target/debug/` : `akasha` (CLI) et `akasha-daemon`.

## 3. Initialisation (recommandé au premier lancement)

Une seule commande pour configurer providers LLM, vault, connecteurs et options RAG :

```bash
akasha init
```

Le wizard vous demande :
- **Provider LLM** : par défaut le modèle **embarqué** (akasha_embedded) est utilisé ; vous pouvez ajouter Ollama, OpenAI, OpenRouter ou une combinaison (URL Ollama, modèles, clés API → enregistrées dans le vault).
- **Connecteurs** : tokens Telegram, Slack, Discord (stockés dans le vault).
- **Activation** : quels connecteurs activer (génère `connectors.env` dans le data_dir).

Fichiers créés dans le data_dir (ex. `%LOCALAPPDATA%\akasha`) :
- `llm_router.yaml` — configuration routeur LLM (par défaut : akasha_embedded/default pour tous les types de tâche). Si Ollama est joignable, les infos de chaque modèle (contexte max, num_ctx, family, etc.) sont récupérées et enregistrées dans la section `model_options`.
- `connectors.env` — variables pour activer Telegram/Slack/Discord ; chargé automatiquement par `akasha start`.

Mode sans questions (défauts : modèle embarqué akasha_embedded, aucun connecteur) :

```bash
akasha init --defaults
```

## 4. Config (modèles + variables d’environnement)

Après l’init (ou sans init), vous pouvez :

- **Consulter ou définir le modèle par catégorie** (conversation, code_generation, etc.) :
  - `akasha config models get [CATEGORY]` — affiche les modèles configurés (toutes les catégories ou une seule).
  - `akasha config models set CATEGORY PROVIDER MODEL` — définit le modèle principal (ex. `akasha config models set conversation ollama llama3.2` ou `akasha config models set conversation akasha_embedded default`).
- **Récupérer les infos des modèles Ollama** (contexte max, num_ctx, etc.) et les écrire dans `llm_router.yaml` :
  - `akasha config models fetch` — pour tous les modèles Ollama déjà présents dans la config.
  - `akasha config models add <nom_modele> [--ollama-url URL]` — pour un modèle donné (ex. `glm-4.7-flash:latest`).

- **Gérer des variables d’environnement persistantes** (fichier `data_dir/akasha.env`, chargé au démarrage du daemon) :
  - `akasha config env set KEY [value]` — enregistre une variable (si value est omis, lu depuis l’entrée standard).
  - `akasha config env list` — affiche les variables.
  - `akasha config env get KEY` — affiche la valeur d’une clé.

## 5. Vault (secrets)

Les secrets (tokens Slack, Discord, Telegram, clés API LLM) sont stockés dans le vault.

- **Lister les clés** : `akasha vault list`
- **Enregistrer un secret** : `akasha vault set NOM_CLE [valeur]`  
  Si vous omettez la valeur, elle est lue depuis l’entrée standard.

Exemples :

```bash
# Token Telegram (pour le bot)
akasha vault set telegram_bot_token VOTRE_TOKEN

# Clé OpenAI (pour le routeur LLM)
akasha vault set openai_api_key sk-...
```

## 6. LLM et réponses au chat

Par défaut, le routeur utilise le **modèle embarqué** (akasha_embedded) : pas besoin d'Ollama ni de clé cloud pour recevoir des réponses. Vérifier que le modèle est prêt : `akasha doctor` (section « Daemon checks » → embedded_llm) ou dans la TUI : `/embedded`.

**Optionnel — Ollama ou cloud :**

1. **Ollama** : installer [ollama.ai](https://ollama.ai), lancer Ollama en local. Puis définir le modèle pour une catégorie : `akasha config models set conversation ollama llama3.2` ou via la TUI : `/models set conversation ollama llama3.2`.
2. **Config** : le data_dir et les chemins des fichiers (llm_router.yaml, akasha.env, etc.) s'affichent avec `akasha paths` ou dans la sortie de `akasha doctor`.
3. **Modèles** : lister les modèles (daemon démarré) : `GET http://127.0.0.1:3876/api/router/models` ou `/models` dans la TUI.

Variables utiles :

- `OLLAMA_HOST` : URL d’Ollama si différent de `http://localhost:11434`.

- `AKASHA_EMBEDDED_MODEL` : modèle embarqué (`qwen3_0_6b` par défaut, `baguettotron` si compilé avec la feature `embedded-baguettotron`). Voir `spec/34_embedded_small_model.md`.

## 7. Démarrer le daemon

```bash
# En arrière-plan (avec superviseur)
akasha start

# En premier plan (logs visibles)
akasha start --foreground
```

Le daemon écoute par défaut sur le port **3876** (`AKASHA_PORT`).

## 8. Vérifier l’état : doctor

```bash
akasha doctor
```

Vérifie : Rust, Node, binaire daemon, specs, santé du daemon.

Pour obtenir un **conseil diagnostic** basé sur les runbooks et le modèle configuré :

```bash
akasha doctor --advice
```

(Le daemon doit être démarré. Le modèle embarqué ou Ollama/cloud doit être disponible pour une réponse complète.)

## 9. Canaux (si non configurés par init)

- **Telegram** : `akasha vault set telegram_bot_token <token>`, puis `$env:AKASHA_TELEGRAM_ENABLED="1"` (PowerShell) avant de lancer le daemon. Optionnel : `AKASHA_TELEGRAM_NOTIFY_CHAT_ID` pour recevoir « bot connecté » au démarrage.
- **Slack / Discord** : voir README (vault `slack_signing_secret`, `discord_bot_token`, variables `AKASHA_SLACK_ENABLED` / `AKASHA_DISCORD_ENABLED`).

## 10. Suite d’évals (Phase 8)

Pour lancer les tests d’évaluation (sécurité, runbooks, hallucinations) :

```bash
cargo run -p akasha-evals
```

À exécuter depuis la racine du projet (ou avec `AKASHA_SPEC_DIR` pointant vers le dossier `spec`).

## Résumé

1. `cargo build`
2. **`akasha init`** — wizard providers, vault, connecteurs (ou `akasha init --defaults` pour le minimal)
3. Si besoin : `akasha vault set ...` pour des secrets non demandés par init
4. Charger `connectors.env` si vous avez activé Telegram/Slack/Discord (voir sortie de init)
5. `akasha start` ou `akasha start --foreground`
6. `akasha doctor` puis `akasha doctor --advice` pour vérifier
