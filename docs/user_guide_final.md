# Guide utilisateur Akasha

Ce guide s’adresse aux utilisateurs qui ont téléchargé les **binaires précompilés** d’Akasha (sans compiler l’application). Il décrit les commandes, la configuration et les interfaces disponibles.

---

## 1. Obtenir et lancer Akasha

### Téléchargement

1. Rendez-vous sur la page **Releases** du dépôt Akasha (ex. GitHub).
2. Téléchargez l’archive correspondant à votre système (Windows, Linux ou macOS).
3. Décompressez l’archive dans un dossier (ex. `C:\Akasha` ou `~/Akasha`).

Vous obtenez les exécutables **akasha** (ou akasha.exe), **akasha-daemon** et **akasha-tui**, ainsi que le dossier **docs** contenant ce guide.

### Prérequis

- **Ollama** (optionnel) : pour utiliser des modèles LLM locaux. Sinon, configurez un fournisseur cloud (OpenAI, OpenRouter) lors de l’initialisation.
- Aucune installation de Rust ou Node n’est nécessaire pour utiliser les binaires.

### Premier lancement

**Windows (PowerShell ou CMD)**  
Ouvrez un terminal dans le dossier où vous avez extrait l’archive, puis :

```
.\akasha.exe init
.\akasha.exe start
```

**Linux / macOS**  
Dans un terminal, depuis le dossier d’extraction :

```bash
chmod +x akasha akasha-daemon akasha-tui
./akasha init
./akasha start
```

Pour afficher l’interface en terminal : `akasha tui` (ou `.\akasha.exe tui` sous Windows).

**Important** : lancez `akasha start` depuis le dossier où vous avez extrait l’archive afin que l’onglet **Doc** des interfaces affiche cette documentation.

---

## 2. Où se trouve la configuration

Tous les fichiers de configuration sont dans le **répertoire de données** (data_dir). Pour connaître son emplacement :

```
akasha paths
```

Affichage typique :  
- **Windows** : `C:\Users\<Vous>\AppData\Local\akasha`  
- **Linux / macOS** : `~/.local/share/akasha`

Vous pouvez modifier les fichiers suivants dans ce répertoire (avec un éditeur de texte) :

| Fichier | Rôle |
|--------|------|
| `llm_router.yaml` | Fournisseurs LLM (Ollama, OpenAI, OpenRouter) et modèles par type de tâche. |
| `tools_policy.yaml` | Autorisations des outils (lecture/écriture de fichiers, commandes, recherche web, hôtes pour l’installation de skills). |
| `akasha.env` | Variables d’environnement persistantes (éditables aussi via `akasha config env`). |
| `connectors.env` | Activation des canaux (Telegram, Slack, Discord). |

Le répertoire de données est créé automatiquement par `akasha init` ou `akasha doctor --fix` s’il est absent.

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
| `akasha init --defaults` | Initialisation minimale sans questions (Ollama uniquement, aucun canal). |
| `akasha doctor` | Vérifie l’état du système et du daemon (binaires, Ollama, vault, modèle embarqué, etc.). |
| `akasha doctor --json` | Même vérifications, sortie en JSON. |
| `akasha doctor --advice` | Conseils de diagnostic (nécessite que le daemon tourne). |
| `akasha doctor --fix` | Corrige les éléments manquants : crée le data_dir si besoin, et des fichiers de config minimaux (llm_router.yaml, tools_policy.yaml, connectors.env) s’ils sont absents. |
| `akasha paths` | Affiche le data_dir et les chemins des fichiers de configuration. |

### Secrets (vault)

| Commande | Description |
|----------|-------------|
| `akasha vault list` | Liste les noms des clés enregistrées. |
| `akasha vault set KEY [value]` | Enregistre un secret ; si `value` est omis, la valeur est lue depuis l’entrée standard. |
| `akasha vault get KEY` | Affiche la valeur d’une clé (à utiliser avec précaution). |
| `akasha vault delete KEY` | Supprime une clé. |

### Config (modèles et variables)

| Commande | Description |
|----------|-------------|
| `akasha config models get [CATEGORY]` | Affiche les modèles configurés par catégorie (primary et fallback). |
| `akasha config models routes` | Affiche pour chaque catégorie le modèle principal et les modèles de secours. |
| `akasha config models set CATEGORY PROVIDER MODEL` | Définit le modèle principal pour une catégorie (ex. `conversation ollama llama3.2`). |
| `akasha config models fetch` | Récupère les infos des modèles Ollama et les enregistre dans la config. |
| `akasha config models add MODEL [--ollama-url URL]` | Ajoute les infos d’un modèle Ollama. |
| `akasha config env list` | Liste les variables dans `akasha.env`. |
| `akasha config env get KEY` | Affiche la valeur d’une variable. |
| `akasha config env set KEY [value]` | Définit une variable (valeur optionnelle, lue depuis l’entrée standard si omise). |

### Routeur LLM et plugins

| Commande | Description |
|----------|-------------|
| `akasha router metrics` | Affiche les métriques du routeur (requêtes, latence, fallbacks). |
| `akasha router discover` | Découvre les instances Ollama (local et réseau local). |
| `akasha router show MODEL` | Affiche les infos d’un modèle Ollama. |
| `akasha plugin list` | Liste les plugins installés. |
| `akasha plugin reload` | Recharge les plugins sans redémarrer le daemon. |
| `akasha plugin install CHEMIN` | Installe un plugin depuis un répertoire. |
| `akasha plugin uninstall ID` | Désinstalle un plugin. |
| `akasha plugin catalog` | Affiche le catalogue local des plugins. |

### Interfaces

| Commande | Description |
|----------|-------------|
| `akasha tui` | Lance l’interface en terminal (Chat, Routeur, Doc, Tâches, Calendrier, Mémoire). |

Si vous avez installé l’**application desktop** (Akasha UI), lancez-la ; elle se connecte au daemon sur le port 3876 (configurable via la variable d’environnement `AKASHA_PORT`).

### Mise à jour

| Commande | Description |
|----------|-------------|
| `akasha update check` | Vérifie si une nouvelle version est disponible (interroge le site vitrine). Affiche l’URL de téléchargement si une mise à jour existe. |
| `akasha update install` | Ouvre la page de téléchargement de la dernière release dans le navigateur. |

L’URL de l’API est configurable via la variable d’environnement `AKASHA_APP_BASE_URL` (défaut : `https://azerothl.github.io/Akasha_app`).

---

## 4. Parcours type (onboarding)

1. **Initialisation** : `akasha init` (ou `akasha init --defaults` pour une config minimale).
2. **Secrets** : si besoin, `akasha vault set KEY value` pour les clés API non demandées par init.
3. **Démarrer** : `akasha start` ou `akasha start --foreground`.
4. **Vérifier** : `akasha doctor`, puis `akasha doctor --advice` pour des conseils.
5. **Interface** : `akasha tui` pour le chat et la doc dans le terminal, ou l’application desktop si installée.

En cas de fichier manquant, `akasha doctor --fix` crée le data_dir et des fichiers de config minimaux.

---

## 5. Variables d’environnement utiles

| Variable | Description | Défaut |
|----------|-------------|--------|
| `AKASHA_PORT` | Port du daemon | 3876 |
| `AKASHA_DATA_DIR` | Répertoire de données | %LOCALAPPDATA%\akasha (Windows) / ~/.local/share/akasha (Linux/macOS) |
| `AKASHA_LOG` | Niveau de log (trace, debug, info, warn, error) | info |
| `OLLAMA_HOST` | URL d’Ollama si non configuré ailleurs | http://localhost:11434 |
| `AKASHA_TELEGRAM_ENABLED` | `1` pour activer Telegram | — |
| `AKASHA_DISCORD_ENABLED` | `1` pour activer Discord | — |
| `AKASHA_SLACK_ENABLED` | `1` pour activer Slack | — |
| `OPENROUTER_API_KEY` | Clé API OpenRouter | — |
| `OPENAI_API_KEY` | Clé API OpenAI | — |

Les variables définies via `akasha config env set` sont enregistrées dans le fichier `akasha.env` du data_dir et rechargées au démarrage du daemon.

---

## 6. Interfaces et onglets

### Interface terminal (TUI)

- **Onglets** : Chat, Routeur (métriques), Doc (cette documentation), Tâches, Calendrier, Mémoire.
- **Raccourcis** : Tab (changer d’onglet), Entrée (envoyer un message), R (rafraîchir), Échap ou Ctrl+Q (quitter).

### Interface web / desktop (si installée)

- **Onglets** : Chat, Routeur, Documentation, Tâches, Calendrier, Mémoire, Paramètres.
- **Pièces jointes** : dans le Chat, vous pouvez joindre des images ou des documents (texte, PDF) ; l’agent les reçoit pour analyse.
- **RAG utilisateur** : dans Paramètres, section « Mes documents (RAG utilisateur) », vous pouvez ajouter ou supprimer des documents ; les extraits pertinents sont utilisés par l’agent lors des réponses.

Le daemon écoute par défaut sur le port **3876**. Pour que l’onglet Doc affiche ce guide, lancez `akasha start` depuis le dossier où vous avez extrait l’archive (contenant le dossier `docs`).

---

## 7. Commandes slash (dans le chat)

Dans le chat (TUI ou interface web), les messages commençant par **/** sont des commandes (non envoyées au LLM) :

| Commande | Description |
|----------|-------------|
| `/help`, `/?` | Aide des commandes. |
| `/status` | État du daemon. |
| `/doctor` | Diagnostic (daemon, Ollama, vault, modèle embarqué). |
| `/advice` | Conseils de diagnostic (nécessite le daemon). |
| `/embedded` | Statut du modèle local embarqué. |
| `/embedded reload` | Décharge le modèle embarqué (rechargé au prochain appel). |
| `/metrics` | Métriques du routeur LLM. |
| `/models` | Liste des modèles (tous les fournisseurs). |
| `/models list` | Modèles par catégorie (primary + fallback). |
| `/routes` | Primary et fallback par catégorie. |
| `/models set CATÉGORIE PROVIDER MODÈLE` | Définit le modèle pour une catégorie (ex. `/models set conversation ollama llama3.2`). |
| `/config list` | Variables (akasha.env). |
| `/config get KEY` | Valeur d’une variable. |
| `/config set KEY value` | Définir une variable. |
| `/vault list` | Clés du vault (noms uniquement). |
| `/plugins` | Liste des plugins installés. |
| `/reload` | Recharger les plugins. |
| `/skills reload` | Recharger les skills (après en avoir ajouté ou modifié). |
| `/restart` | Redémarrer le daemon (via le superviseur). |

Pour ajouter ou supprimer une clé dans le vault, utilisez les commandes CLI `akasha vault set` et `akasha vault delete` (pas d’équivalent slash pour des raisons de sécurité).

---

## 8. Skills (capacités supplémentaires)

Vous pouvez **demander à l’agent d’installer un skill** depuis une URL. Par exemple, dans le chat : « Installe le skill bankr depuis https://github.com/BankrBot/skills/tree/main/bankr ». L’agent utilisera l’outil d’installation, téléchargera le skill, puis le rechargera.

- Par défaut, seuls les hôtes **GitHub** sont autorisés. Pour autoriser d’autres sites (GitLab, votre propre serveur, etc.), éditez le fichier **tools_policy.yaml** dans le data_dir et ajoutez la clé **allowed_skill_install_hosts** avec la liste des hôtes (ex. `["github.com", "raw.githubusercontent.com", "gitlab.com"]`). Utilisez `["*"]` pour autoriser tout hôte HTTPS.
- Après avoir ajouté ou modifié des skills manuellement (fichiers dans le data_dir), tapez **/skills reload** dans le chat pour les recharger sans redémarrer le daemon.

---

## 9. Canaux (Telegram, Slack, Discord)

- **Telegram** : enregistrez le token du bot avec `akasha vault set telegram_bot_token VOTRE_TOKEN`, puis définissez la variable d’environnement `AKASHA_TELEGRAM_ENABLED=1`. Le bot répond aux commandes `/akasha <message>` ou `/start`.
- **Slack** : vault `slack_signing_secret`, puis `AKASHA_SLACK_ENABLED=1`. Configurez la slash command vers l’URL fournie par votre déploiement.
- **Discord** : vault `discord_bot_token`, puis `AKASHA_DISCORD_ENABLED=1`. Le bot répond au préfixe `!akasha <message>`.

Les variables d’activation sont chargées depuis le fichier **connectors.env** du data_dir (créé ou complété par `akasha init`).

---

## 10. Dépannage

- **Le daemon ne démarre pas** : vérifiez avec `akasha doctor`. Utilisez `akasha doctor --fix` pour créer le data_dir et les fichiers de config manquants.
- **Pas de réponses ou timeouts** : assurez-vous qu’Ollama tourne (si vous l’utilisez) ou qu’une clé API cloud (OpenRouter, OpenAI) est configurée (vault ou variable d’environnement). Consultez `akasha config models routes` et l’onglet Routeur dans la TUI pour voir les modèles actifs.
- **L’onglet Doc est vide** : lancez `akasha start` depuis le dossier où vous avez extrait l’archive (celui qui contient le dossier `docs`). Vérifiez que le fichier `docs/user_guide.md` est bien présent.
- **Conseils personnalisés** : `akasha doctor --advice` (le daemon doit être démarré).

---

## 11. Référence rapide

| Action | Commande ou moyen |
|--------|-------------------|
| Démarrer le daemon | `akasha start` ou `akasha start --foreground` |
| Arrêter le daemon | `akasha stop` |
| Premier lancement | `akasha init` |
| Vérifier l’état | `akasha doctor` / `akasha doctor --advice` |
| Corriger les fichiers manquants | `akasha doctor --fix` |
| Voir les chemins de config | `akasha paths` |
| Enregistrer un secret | `akasha vault set KEY value` |
| Définir une variable | `akasha config env set KEY value` |
| Voir les modèles par catégorie | `akasha config models routes` ou `/models list` dans le chat |
| Changer le modèle d’une catégorie | `akasha config models set CATEGORY PROVIDER MODEL` ou `/models set ...` dans le chat |
| Lancer l’interface terminal | `akasha tui` |
| Recharger les skills | `/skills reload` dans le chat |
| Redémarrer le daemon | `/restart` dans le chat |
| Vérifier une mise à jour | `akasha update check` |
| Ouvrir la page de téléchargement | `akasha update install` |

Cette documentation est également affichée dans l’**onglet Doc** des interfaces lorsque le daemon est démarré depuis le dossier d’extraction contenant le dossier `docs`.
