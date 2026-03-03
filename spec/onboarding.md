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
- **Provider LLM** : Ollama seul, OpenAI, OpenRouter, ou combinaison (URL Ollama, modèles, clés API → enregistrées dans le vault).
- **Connecteurs** : tokens Telegram, Slack, Discord (stockés dans le vault).
- **Activation** : quels connecteurs activer (génère `connectors.env` dans le data_dir).

Fichiers créés dans le data_dir (ex. `%LOCALAPPDATA%\akasha`) :
- `llm_router.yaml` — configuration routeur LLM. Si Ollama est joignable, les infos de chaque modèle (contexte max, num_ctx, family, etc.) sont récupérées et enregistrées dans la section `model_options`.
- `connectors.env` — variables pour activer Telegram/Slack/Discord ; chargé automatiquement par `akasha start`.

Mode sans questions (défauts : Ollama uniquement, aucun connecteur) :

```bash
akasha init --defaults
```

## 4. Config (modèles + variables d’environnement)

Après l’init (ou sans init), vous pouvez :

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

## 6. Ollama et LLM (si non configuré par init) (réponses au chat)

Pour que les messages (Telegram, UI, etc.) reçoivent une vraie réponse du modèle :

1. **Installer Ollama** : [ollama.ai](https://ollama.ai) et lancer Ollama en local.
2. **Configurer le routeur** : copier ou créer `llm_router.yaml` à la racine du projet ou dans le data_dir (ex. `%LOCALAPPDATA%\akasha`). Voir `spec/llm_router.example.yaml`.
3. **Modèles** : dans `llm_router.yaml`, indiquer un modèle installé (ex. `llama3.2`). Lister les modèles disponibles : une fois le daemon démarré, `GET http://127.0.0.1:3876/api/router/ollama/models` ou consulter les logs au premier `doctor --advice`.

Variables utiles :

- `OLLAMA_HOST` : URL d’Ollama si différent de `http://localhost:11434`.

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

(Le daemon doit être démarré et Ollama / LLM configuré.)

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
