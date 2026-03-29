# Guide utilisateur Akasha

Ce guide s'adresse aux utilisateurs qui ont téléchargé les **binaires précompilés** d'Akasha (sans compiler l'application). Il décrit les commandes, la configuration et les interfaces disponibles. Cette documentation est affichée dans l'onglet **Doc** des interfaces lorsque le daemon est démarré depuis le dossier d'extraction contenant le dossier `docs`.

---

## 1. Obtenir et lancer Akasha

### Téléchargement

1. Rendez-vous sur la page **Releases** du dépôt Akasha (ex. GitHub).
2. Pour une installation complète en une étape, téléchargez l'archive **« Akasha full »** correspondant à votre système (ex. `akasha-full-windows-x86_64.zip`, `akasha-full-linux-x86_64.zip`, `akasha-full-macos-x86_64.zip`). Sinon, téléchargez l'archive CLI (akasha, daemon, TUI) et, si besoin, l'archive de l'application desktop (Tauri) séparément.
3. Décompressez l'archive dans un dossier (ex. `C:\Akasha` ou `~/Akasha`).

Vous obtenez les exécutables **akasha** (ou akasha.exe), **akasha-daemon** et **akasha-tui**, le dossier **scripts** (install et setup), **docs**, et dans le zip « full » un sous-dossier **ui** contenant l'installateur de l'application desktop.

### Installation recommandée (installeur unifié)

Après avoir extrait le zip **full** :

- **Windows (PowerShell)** : exécutez `.\scripts\setup.ps1`. Le script vous demandera : installer l'application desktop (interface web) ? Démarrer le daemon à chaque connexion ? Il installe les binaires, lance le premier `akasha init`, démarre le daemon une fois et, si vous le souhaitez, enregistre le daemon au démarrage de Windows et installe l'app Tauri.
- **Linux / macOS** : exécutez `./scripts/setup.sh` (ou `bash scripts/setup.sh`). Mêmes choix (application desktop, daemon au démarrage). Le script installe les binaires, lance l'init, démarre le daemon une fois et, sous Linux (systemd) ou macOS (launchd), peut enregistrer le daemon pour qu'il démarre à chaque connexion.

**Redémarrage en cas de crash** : si vous avez activé le démarrage automatique, le daemon est relancé en cas d'arrêt — sous Windows via le superviseur intégré à `akasha start`, sous Linux/macOS via systemd ou launchd.

### Installation manuelle (sans setup)

**Windows (PowerShell ou CMD)**  
Ouvrez un terminal dans le dossier où vous avez extrait l'archive, puis :

```
.\scripts\install.ps1
.\akasha.exe start
```

(Optionnel : `-NoAutoStart` pour ne pas enregistrer le daemon à la connexion.)

**Linux / macOS**  
Dans un terminal, depuis le dossier d'extraction :

```bash
chmod +x akasha akasha-daemon akasha-tui scripts/*.sh
./scripts/install.sh
./akasha start
```

(Optionnel : `--no-auto-start` pour ne pas enregistrer le daemon au démarrage.)

Pour afficher l'interface en terminal : `akasha tui` (ou `.\akasha.exe tui` sous Windows).

### Prérequis

- **Modèle embarqué** : par défaut Akasha utilise un modèle LLM intégré (akasha_embedded). Aucune installation externe n'est obligatoire pour recevoir des réponses.
- **Ollama** (optionnel) : pour utiliser d'autres modèles locaux. Configurez-le lors de l'initialisation ou plus tard via `akasha config models set conversation ollama <modèle>`.
- **Cloud** (optionnel) : OpenAI ou OpenRouter, configurés lors de l'init (clés dans le vault ou variables d'environnement).
- Aucune installation de Rust ou Node n'est nécessaire pour utiliser les binaires.

**Important** : lancez `akasha start` depuis le dossier d'installation (ou après avoir ajouté ce dossier au PATH) afin que l'onglet **Doc** des interfaces affiche cette documentation.

---

## 2. Où se trouve la configuration

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
| `agent_profile.json` | Profil de l'agent : nom, rôle, personnalité, règles. Éditable dans Paramètres → Profil de l'agent (interface web) ou en modifiant le fichier puis en redémarrant le daemon. |

Le répertoire de données est créé automatiquement par `akasha init` ou `akasha doctor --fix` s'il est absent.

---

## 3. Commandes principales

### Daemon

| Commande | Description |
|----------|-------------|
| `akasha start` | Démarre le daemon en arrière-plan (avec redémarrage automatique en cas de crash). |
| `akasha start --foreground` | Démarre le daemon au premier plan (logs dans le terminal). |
| `akasha stop` | Arrête le daemon. |

### Premier lancement et diagnostic

| Commande | Description |
|----------|-------------|
| `akasha init` | Assistant interactif : choix du fournisseur LLM, vault, canaux (Telegram, Slack, Discord). Crée les fichiers de config dans le data_dir. |
| `akasha init --defaults` | Initialisation minimale sans questions (modèle embarqué par défaut, aucun canal). |
| `akasha doctor` | Vérifie l'état du système et du daemon (binaires, Ollama, vault, modèle embarqué, etc.). |
| `akasha doctor --json` | Même vérifications, sortie en JSON. |
| `akasha doctor --advice` | Conseils de diagnostic (nécessite que le daemon tourne). |
| `akasha doctor --fix` | Corrige les éléments manquants : crée le data_dir si besoin, et des fichiers de config minimaux (llm_router.yaml, tools_policy.yaml, connectors.env) s'ils sont absents. |
| `akasha paths` | Affiche le data_dir et les chemins des fichiers de configuration. |

### Secrets (vault)

| Commande | Description |
|----------|-------------|
| `akasha vault list` | Liste les noms des clés enregistrées. |
| `akasha vault set KEY [value]` | Enregistre un secret ; si `value` est omis, la valeur est lue depuis l'entrée standard. |
| `akasha vault get KEY` | Affiche la valeur d'une clé (à utiliser avec précaution). |
| `akasha vault delete KEY` | Supprime une clé. |

### Config (modèles, variables, fournisseurs)

| Commande | Description |
|----------|-------------|
| `akasha config models get [CATEGORY]` | Affiche les modèles configurés par catégorie (primary et fallback). |
| `akasha config models routes` | Affiche pour chaque catégorie le modèle principal et les modèles de secours. |
| `akasha config models set CATEGORY PROVIDER MODEL` | Définit le modèle principal pour une catégorie (ex. `conversation ollama llama3.2`). |
| `akasha config models fetch` | Récupère les infos des modèles Ollama et les enregistre dans la config. |
| `akasha config models add MODEL [--ollama-url URL]` | Ajoute les infos d'un modèle Ollama. |
| `akasha config env list` | Liste les variables dans `akasha.env`. |
| `akasha config env get KEY` | Affiche la valeur d'une variable. |
| `akasha config env set KEY [value]` | Définit une variable (valeur optionnelle, lue depuis l'entrée standard si omise). |
| `akasha config provider list` | Liste les fournisseurs définis dans `llm_router.yaml`. |
| `akasha config provider set-ollama [--url URL] [--category CAT] [--model MODÈLE]` | Définit l'URL Ollama et optionnellement le modèle pour une catégorie. |
| `akasha config provider add-openai [--api-key KEY] [--category CAT] [--model MODÈLE]` | Ajoute OpenAI (clé dans le vault) et optionnellement une route. |
| `akasha config provider add-openrouter [--api-key KEY] [--category CAT] [--model MODÈLE]` | Ajoute OpenRouter (clé dans le vault) et optionnellement une route. |

### Routeur LLM et plugins

| Commande | Description |
|----------|-------------|
| `akasha router metrics` | Affiche les métriques du routeur (requêtes, latence, fallbacks). |
| `akasha router discover` | Découvre les instances Ollama (local et réseau local). |
| `akasha router show MODEL` | Affiche les infos d'un modèle Ollama. |
| `akasha plugin list` | Liste les plugins installés. |
| `akasha plugin reload` | Recharge les plugins sans redémarrer le daemon. |
| `akasha plugin install CHEMIN` | Installe un plugin depuis un répertoire. |
| `akasha plugin uninstall ID` | Désinstalle un plugin. |
| `akasha plugin catalog` | Affiche le catalogue local des plugins. |

### Interfaces

| Commande | Description |
|----------|-------------|
| `akasha tui` | Lance l'interface en terminal (Chat, Routeur, Doc, Tâches, Calendrier, Mémoire). |

Si vous avez installé l'**application desktop** (Akasha UI), lancez-la ; elle se connecte au daemon sur le port 3876 (configurable via la variable d'environnement `AKASHA_PORT`).

### Mise à jour

| Commande | Description |
|----------|-------------|
| `akasha update check` | Vérifie si une nouvelle version est disponible (interroge le site vitrine). Affiche l'URL de téléchargement si une mise à jour existe. |
| `akasha update install` | Ouvre la page de téléchargement de la dernière release dans le navigateur. |

L'URL de l'API est configurable via la variable d'environnement `AKASHA_APP_BASE_URL` (défaut : `https://azerothl.github.io/Akasha_app`).

---

## 4. Parcours type (onboarding)

1. **Initialisation** : `akasha init` (ou `akasha init --defaults` pour une config minimale).
2. **Secrets** : si besoin, `akasha vault set KEY value` pour les clés API non demandées par init.
3. **Démarrer** : `akasha start` ou `akasha start --foreground`.
4. **Vérifier** : `akasha doctor`, puis `akasha doctor --advice` pour des conseils.
5. **Interface** : `akasha tui` pour le chat et la doc dans le terminal, ou l'application desktop si installée.

En cas de fichier manquant, `akasha doctor --fix` crée le data_dir et des fichiers de config minimaux.

---

## 5. Variables d'environnement utiles

| Variable | Description | Défaut |
|----------|-------------|--------|
| `AKASHA_PORT` | Port du daemon | 3876 |
| `AKASHA_DATA_DIR` | Répertoire de données | %USERPROFILE%\akasha (Windows) / ~/akasha (Linux/macOS) |
| `AKASHA_LOG` | Niveau de log (trace, debug, info, warn, error) | info |
| `AKASHA_LANG` | Langue de l'interface TUI (`en` = anglais, sinon français) | Détection via LANG / LC_ALL |
| `AKASHA_MAX_RESPONSE_TOKENS` | Nombre max de tokens pour les réponses chat | 4096 |
| `AKASHA_APP_BASE_URL` | URL du site des releases (pour `akasha update check`) | https://azerothl.github.io/Akasha_app |
| `AKASHA_SYSTEM_TASK_MAX_TOKENS` | Tokens max pour les tâches système (mémoire, décomposition). À augmenter (ex. 8192) si un modèle « thinking » renvoie des réponses vides | 4096 |
| `OLLAMA_HOST` | URL d'Ollama si non configuré ailleurs | http://localhost:11434 |
| `AKASHA_TELEGRAM_ENABLED` | `1` pour activer Telegram | — |
| `AKASHA_DISCORD_ENABLED` | `1` pour activer Discord | — |
| `AKASHA_SLACK_ENABLED` | `1` pour activer Slack | — |
| `OPENROUTER_API_KEY` | Clé API OpenRouter | — |
| `OPENAI_API_KEY` | Clé API OpenAI | — |

Les variables définies via `akasha config env set` sont enregistrées dans le fichier `akasha.env` du data_dir et rechargées au démarrage du daemon. Pour qu'OpenRouter affiche votre usage dans son dashboard, définissez `OPENROUTER_SITE_URL` et `OPENROUTER_APP_TITLE` (ou les champs correspondants dans `llm_router.yaml`).

---

## 6. Interfaces et onglets

### Interface terminal (TUI)

- **Onglets** : Chat, **Retours planifiés** (réponses des tâches récurrentes), Routeur (métriques), Doc (cette documentation), Tâches, Calendrier, Mémoire.
- **Chat** : uniquement la conversation avec l’agent (messages envoyés et réponses). **Retours planifiés** : uniquement les réponses de l’agent pour les rappels / tâches planifiées (schedules), sans les mélanger au fil du chat.
- **Raccourcis** : Tab (changer d'onglet), Entrée (envoyer un message), R (rafraîchir Routeur, Retours planifiés ou liste des tâches), ↑/↓ PgUp/PgDn Home/End (défilement), Échap ou Ctrl+Q (quitter). **Onglet Tâches** : ↑/↓ (sélectionner une tâche), D (filtrer racines uniquement). **Onglet Mémoire** : / ou S (recherche), G (basculer vue graphe), D ou Suppr (supprimer l'entrée long terme sélectionnée).

### Interface web / desktop (si installée)

- **Onglets** : Chat, **Retours planifiés**, Routeur, Documentation, Tâches, Calendrier, Mémoire, Paramètres. **Chat** = conversation uniquement ; **Retours planifiés** = réponses de l’agent pour les tâches planifiées (rappels récurrents), dans un onglet dédié.
- **Raccourcis** : touches **1 à 8** pour basculer vers l'onglet correspondant (inactif si le focus est dans un champ de saisie ou une modale).
- **Pièces jointes** : dans le Chat, vous pouvez joindre des images ou des documents (texte, PDF) ; l'agent les reçoit pour analyse.
- **RAG utilisateur** : dans Paramètres, section « Mes documents (RAG utilisateur) », vous pouvez ajouter ou supprimer des documents ; les extraits pertinents sont utilisés par l'agent lors des réponses.
- **Profil de l'agent** : dans Paramètres → Profil de l'agent, vous pouvez définir le nom, le rôle, la personnalité, les règles et les comportements autorisés/interdits ; des modèles (Neutre, Bienveillant, Concis/technique, etc.) sont proposés.

Le daemon écoute par défaut sur le port **3876**. Pour que l'onglet Doc affiche ce guide, lancez `akasha start` depuis le dossier où vous avez extrait l'archive (contenant le dossier `docs`).

---

## 7. Commandes slash (dans le chat)

Dans le chat (TUI ou interface web), les messages commençant par **/** sont des commandes (non envoyées au LLM). Tapez **/help** ou **/?** dans le chat pour afficher la liste complète.

| Commande | Description |
|----------|-------------|
| `/help`, `/?` | Aide des commandes (liste complète). |
| `/task create "message"` | Crée une tâche (envoie le message au daemon comme un message chat). |
| `/schedule create NOM INTERVAL_SEC "description"` | Crée une récurrence (ex. rappel périodique). |
| `/schedule delete SCHEDULE_ID` | Supprime une récurrence. |
| `/stop TASK_ID`, `/cancel TASK_ID` | Annule une tâche (en cours ou en attente). |
| `/newsession` | Nouvelle session : contexte court terme effacé, le prochain message repart de zéro. |
| `/status` | État du daemon. |
| `/doctor` | Diagnostic (daemon, Ollama, vault, modèle embarqué). |
| `/advice` | Conseils de diagnostic (RAG + modèle, nécessite le daemon). |
| `/embedded` | Statut du modèle local embarqué. |
| `/embedded reload` | Décharge le modèle embarqué (rechargé au prochain appel). |
| `/metrics` | Métriques du routeur LLM. |
| `/models` | Liste des modèles (tous les fournisseurs). |
| `/models list` | Modèles par catégorie (primary + fallback). |
| `/models set CATÉGORIE PROVIDER MODÈLE` | Définit le modèle pour une catégorie (ex. `/models set conversation ollama llama3.2`). |
| `/routes` | Primary et fallback par catégorie. |
| `/config list` | Variables (akasha.env). |
| `/config get KEY` | Valeur d'une variable. |
| `/config set KEY value` | Définir une variable. |
| `/vault list` | Clés du vault (noms uniquement). |
| `/plugins` | Liste des plugins installés. |
| `/reload` | Recharger les plugins. |
| `/skills`, `/skills list` | Liste des skills installés (nom et description). |
| `/skills reload` | Recharger les skills (après en avoir ajouté ou modifié). |
| `/skills uninstall <nom>` | Désinstaller un skill (ex. `/skills uninstall bankr`). |
| `/restart` | Redémarrer le daemon (via le superviseur). |

Pour **ajouter** une clé dans le vault : utilisez le CLI `akasha vault set KEY [value]` (pas d'équivalent slash pour des raisons de sécurité). Pour supprimer : `akasha vault delete KEY`.

---

## 8. Politique des outils (tools_policy.yaml)

Le fichier **tools_policy.yaml** dans le data_dir contrôle ce que l'agent peut faire sur votre machine :

- **allowed_read_paths** / **allowed_write_paths** : répertoires ou fichiers autorisés en lecture et en écriture. Par défaut (fichier vide ou absent), tout est refusé. Ex. `["."]` pour le répertoire courant, ou des chemins précis pour limiter l'accès.
- **allowed_commands** : noms d'exécutables autorisés pour les commandes (ex. `cargo`, `npm`, `git`). Les commandes requises par les skills installés sont ajoutées automatiquement.
- **web_search_enabled** : `true` pour que l'agent utilise la recherche web (Brave API) pour répondre aux questions (météo, actualités, etc.). Nécessite une clé : `akasha vault set brave_api_key VOTRE_CLÉ` ou variable `BRAVE_API_KEY`.
- **allowed_skill_install_hosts** : hôtes autorisés pour l'installation de skills (défaut : GitHub uniquement). Ex. `["*"]` pour tout hôte HTTPS.

En cas de fichier absent, `akasha doctor --fix` crée un fichier minimal ; éditez-le selon vos besoins.

---

## 9. Skills (capacités supplémentaires)

Vous pouvez **demander à l'agent d'installer un skill** depuis une URL. Par exemple, dans le chat : « Installe le skill bankr depuis https://github.com/BankrBot/skills/tree/main/bankr ». L'agent utilisera l'outil d'installation, téléchargera le skill, puis le rechargera.

- Par défaut, seuls les hôtes **GitHub** sont autorisés. Pour autoriser d'autres sites (GitLab, votre propre serveur, etc.), éditez le fichier **tools_policy.yaml** dans le data_dir et ajoutez la clé **allowed_skill_install_hosts** avec la liste des hôtes (ex. `["github.com", "raw.githubusercontent.com", "gitlab.com"]`). Utilisez `["*"]` pour autoriser tout hôte HTTPS.
- Après avoir ajouté ou modifié des skills manuellement (fichiers dans le data_dir), tapez **/skills reload** dans le chat pour les recharger sans redémarrer le daemon.

---

## 10. Canaux (Telegram, Slack, Discord)

- **Telegram** : enregistrez le token du bot avec `akasha vault set telegram_bot_token VOTRE_TOKEN`, puis définissez la variable d'environnement `AKASHA_TELEGRAM_ENABLED=1`. Le bot répond aux commandes `/akasha <message>` ou `/start`.
- **Slack** : vault `slack_signing_secret`, puis `AKASHA_SLACK_ENABLED=1`. Configurez la slash command vers l'URL fournie par votre déploiement.
- **Discord** : vault `discord_bot_token`, puis `AKASHA_DISCORD_ENABLED=1`. Le bot répond au préfixe `!akasha <message>`.

Les variables d'activation sont chargées depuis le fichier **connectors.env** du data_dir (créé ou complété par `akasha init`).

---

## 11. Où sont stockés les modèles

- **Modèles d'embeddings** (mémoire long terme) : dans le data_dir, sous `embedding_model/` (sous-dossiers type `models--<org>--<nom>/`).
- **Modèles LLM embarqués** (Qwen, Baguettotron) : cache Hugging Face par défaut (`~/.cache/huggingface/hub` ou `%USERPROFILE%\.cache\huggingface\hub`). Vous pouvez rediriger avec la variable **HF_HOME** (ex. `HF_HOME=~/akasha/hf_cache`).

---

## 12. Dépannage

- **Le daemon ne démarre pas** : vérifiez avec `akasha doctor`. Utilisez `akasha doctor --fix` pour créer le data_dir et les fichiers de config manquants.
- **Pas de réponses ou timeouts** : par défaut le modèle embarqué est utilisé ; vérifiez avec `akasha doctor` (section embedded_llm) ou `/embedded` dans le chat. Si vous utilisez Ollama, assurez-vous qu'il tourne ; pour le cloud, vérifiez les clés (vault ou variables). Consultez `akasha config models routes` et l'onglet Routeur pour voir les modèles actifs.
- **Réponses vides avec un modèle « thinking »** (ex. certains modèles OpenRouter) : augmentez `AKASHA_SYSTEM_TASK_MAX_TOKENS` (ex. 8192) via `akasha config env set AKASHA_SYSTEM_TASK_MAX_TOKENS 8192`, puis redémarrez le daemon.
- **L'onglet Doc est vide** : lancez `akasha start` depuis le dossier où vous avez extrait l'archive (celui qui contient le dossier `docs`). Vérifiez que le fichier `docs/user_guide.md` est bien présent.
- **Conseils personnalisés** : `akasha doctor --advice` (le daemon doit être démarré).

---

## 13. Référence rapide

| Action | Commande ou moyen |
|--------|-------------------|
| Démarrer le daemon | `akasha start` ou `akasha start --foreground` |
| Arrêter le daemon | `akasha stop` |
| Premier lancement | `akasha init` |
| Vérifier l'état | `akasha doctor` / `akasha doctor --advice` |
| Corriger les fichiers manquants | `akasha doctor --fix` |
| Voir les chemins de config | `akasha paths` |
| Enregistrer un secret | `akasha vault set KEY value` |
| Définir une variable | `akasha config env set KEY value` |
| Voir les modèles par catégorie | `akasha config models routes` ou `/models list` dans le chat |
| Changer le modèle d'une catégorie | `akasha config models set CATEGORY PROVIDER MODEL` ou `/models set ...` dans le chat |
| Lancer l'interface terminal | `akasha tui` |
| Créer une tâche depuis le chat | `/task create "message"` |
| Créer / supprimer une récurrence | `/schedule create NOM INTERVAL_SEC "desc"` / `/schedule delete ID` |
| Annuler une tâche | `/stop TASK_ID` ou `/cancel TASK_ID` |
| Nouvelle session (contexte effacé) | `/newsession` dans le chat |
| Recharger les skills | `/skills reload` dans le chat |
| Désinstaller un skill | `/skills uninstall <nom>` dans le chat |
| Redémarrer le daemon | `/restart` dans le chat |
| Configurer un fournisseur LLM (sans init) | `akasha config provider set-ollama` / `add-openai` / `add-openrouter` |
| Vérifier une mise à jour | `akasha update check` |
| Ouvrir la page de téléchargement | `akasha update install` |

Cette documentation est également affichée dans l'**onglet Doc** des interfaces lorsque le daemon est démarré depuis le dossier d'extraction contenant le dossier `docs`. Pour les contributeurs et développeurs, la documentation technique (spécifications, architecture, runbooks) est disponible dans le dépôt source (dossier `spec/` et README à la racine).

---

## 14. Nouveautés de la version 0.7.0

### Orchestration multi-agents

- **Route `orchestrator` dédiée** : si votre `llm_router.yaml` contient un bloc `task_types.orchestrator`, l’orchestrateur l’utilise pour la décomposition de requêtes complexes. Sans cette route, la route `system` est utilisée (compatibilité ascendante).
- **Livrables vérifiés sur disque** : l’orchestrateur s’assure que les fichiers attendus (`workspace:/rapport.md`, etc.) ont bien été créés avant de valider une étape.
- **Trace de plan** : un fichier `.akasha/plan_trace_<id>.md` est maintenu en temps réel dans le workspace pour les requêtes multi-étapes — consultable à tout moment.

### Sécurité et correctifs

- **Politique de chemins** : comparaison stricte par composant de chemin (`Path::starts_with`) — évite la confusion entre `/data` et `/database`.
- **Envoi de documents RAG** : l’upload de documents fonctionne désormais correctement depuis l’interface Tauri.
- **Paramètres LLM** : les valeurs `max_tokens` / `top_k` / `num_ctx` / `num_gpu` très grandes dans `llm_router.yaml` sont limitées proprement (plus de comportement imprévisible).
- **Azure OpenAI** : `max_tokens` par défaut aligné à 4 096 (comme les autres providers).
