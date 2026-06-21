# Commandes

## ## 3. Commandes principales


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
| `akasha discover [service]` | Découvre services locaux (ollama, homeassistant). Sans argument : liste des profils. |
| `akasha router discover` | Alias → découverte Ollama (local et réseau local). |
| `akasha router show MODEL` | Affiche les infos d'un modèle Ollama. |
| `akasha plugin list` | Liste les plugins installés. |
| `akasha plugin reload` | Recharge les plugins sans redémarrer le daemon. |
| `akasha plugin install CHEMIN` | Installe un plugin depuis un répertoire. |
| `akasha plugin uninstall ID` | Désinstalle un plugin. |
| `akasha plugin catalog` | Affiche le catalogue local des plugins. |

**Sélection des plugins (daemon)** : pour chaque message utilisateur (hors petit-talk et tâches Code Studio disque), le daemon interroge brièvement le modèle configuré pour la route **`system`** dans `llm_router.yaml` afin de choisir quels plugins WASM (tool) sont pertinents d’après leur **description** ; un bloc récapitulatif est ajouté au prompt. Les anciennes règles `routing_rules` des manifests ne bloquent plus les autres outils (`write_file`, etc.) — seule la politique **`tools_policy.yaml`** s’applique à l’exécution.

### Interfaces

| Commande | Description |
|----------|-------------|
| `akasha tui` | Lance l'interface en terminal (Chat, Retours planifiés, Routeur, Doc, Tâches, Calendrier, Mémoire). |

Si vous avez installé l'**application desktop** (Akasha UI), lancez-la ; elle se connecte au daemon sur le port 3876 (configurable via la variable d'environnement `AKASHA_PORT`).

### Mise à jour

| Commande | Description |
|----------|-------------|
| `akasha update check` | Vérifie si une nouvelle version est disponible (interroge le site vitrine). Affiche l'URL de téléchargement si une mise à jour existe. |
| `akasha update install` | Ouvre la page de téléchargement de la dernière release dans le navigateur. |

L'URL de l'API est configurable via la variable d'environnement `AKASHA_APP_BASE_URL` (défaut : `https://azerothl.github.io/Akasha_app`).

---

## ## 7. Commandes slash (dans le chat)


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

## ## 13. Référence rapide


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

Cette documentation est également affichée dans l'**onglet Doc** des interfaces lorsque le daemon est démarré depuis le dossier d'extraction contenant le dossier `docs`.

---
