# Spécification — Agents, outils machine, orchestrateur, skills et UI

Ce document décrit les évolutions pour que les agents puissent interagir avec la machine, que l’orchestrateur soit le seul point d’entrée utilisateur avec délégation non bloquante, et que les interfaces affichent les échanges et actions des agents.

---

## 1. Vision et principes

- **Point d’entrée unique** : l’utilisateur ne parle qu’à l’**agent orchestrateur**. Toute demande passe par lui.
- **Réponse immédiate puis délégation** : l’orchestrateur accuse réception (« Je prends en compte votre demande »), délègue à l’agent adéquat dans un flux asynchrone, et transmet la réponse à l’utilisateur quand l’agent a terminé.
- **Non-blocage** : plusieurs demandes consécutives vers différents agents sont possibles sans bloquer l’application ; chaque délégation est traitée en parallèle (thread/tâche dédiée).
- **Sous-agents** : un agent à qui une tâche a été déléguée peut créer des **sous-agents** pour décomposer la tâche (trop grosse ou parallélisable).
- **Outils machine** : les agents peuvent lire/écrire/chercher des fichiers, lancer des commandes, utiliser le terminal, faire des recherches web, comparer/mettre à jour des fichiers, etc.
- **Sécurité** : un agent de code qui génère une application doit la faire tourner dans un **conteneur** (ex. Docker) pour isoler l’exécution.
- **Skills chargeables** : on doit pouvoir **charger des skills** pour les agents (capacités supplémentaires définies et activables par l’orchestrateur ou par agent).

---

## 2. Capacités des agents (outils machine)

Les agents doivent pouvoir utiliser les outils suivants, de manière contrôlée et tracée :

| Capacité | Description | Contraintes / sécurité |
|----------|-------------|--------------------------|
| **Lire des fichiers** | Lire le contenu de fichiers (texte ou binaire encodé) sur la machine hôte | Restriction par répertoire autorisé (ex. `AKASHA_ALLOWED_PATHS`), pas de lecture arbitraire |
| **Chercher des fichiers** | Recherche par nom, motif, type (glob), ou contenu (grep) | Même périmètre que lecture ; index optionnel pour performance |
| **Écrire des fichiers** | Créer ou modifier des fichiers | Périmètre restreint ; pas d’écriture système critique |
| **Comparer des fichiers** | Diff entre deux fichiers ou deux révisions | Lecture seule sur les chemins autorisés |
| **Mettre à jour des fichiers** | Patch, search-replace, ou édition guidée (ex. par LLM) | Même règles qu’écriture ; journal des modifications |
| **Lancer des commandes** | Exécuter des commandes shell (ou API process) | Liste de commandes autorisées et/ou répertoire d’exécution ; timeouts ; logs |
| **Utiliser le terminal** | Session terminal (stdin/stdout/stderr) pour interaction | Optionnel ; session isolée, timeout, kill possible |
| **Recherche web** | Requêtes web (recherche, fetch URL) | Rate limit ; liste de domaines autorisés optionnelle ; pas d’exécution de contenu distant |
| **Exécution en conteneur** | Lancer du code ou une app générée dans un conteneur (Docker/podman) | **Obligatoire** pour exécution de code généré par un agent (agent de code) ; image et ressources définies |

Implémentation prévue :

- **Crate `akasha-tools`** (ou module dans `akasha-daemon`) : définitions des outils (traits, types de paramètres/résultats), implémentations pour lecture/écriture/recherche de fichiers, commandes, diff, recherche web (client HTTP + optional search API).
- **Politique de sécurité** : fichier ou config (ex. `tools_policy.yaml`) listant répertoires autorisés, commandes autorisées, timeouts, activation par agent.
- **Conteneur** : pour « agent de code », un module **container runner** (Docker ou podman) qui build/run une image à partir du code généré (ou d’une image de base + injection du code), avec limites CPU/RAM et timeout ; résultat (stdout/stderr, exit code) remonté à l’agent.

### Outils disponibles de base (implémentés)

Liste exposée dans le code (`AVAILABLE_TOOLS`) et via **GET /api/tools** (JSON `{ "tools": [ { "name", "description" } ] }`). Le LLM peut les invoquer en écrivant une ligne `TOOL: nom arg1 arg2 ...` dans sa réponse (boucle agentique).

| Outil | Usage | Description |
|-------|--------|-------------|
| `read_file` | `read_file <path>` | Lire le contenu d’un fichier texte (soumis à la politique de lecture). |
| `write_file` | `write_file <path> <content>` | Écrire du texte dans un fichier (path puis contenu ; politique d’écriture). |
| `search_files` | `search_files <dir> <pattern>` | Chercher des fichiers par motif glob sous un répertoire. |
| `run_command` | `run_command <cmd> [arg1 arg2 ...]` | Exécuter une commande (liste de commandes autorisées dans `tools_policy.yaml`). |
| `file_diff` | `file_diff <path_a> <path_b>` | Diff texte entre deux fichiers. |
| `search_replace` | `search_replace <path> <search> \| <replace>` | Remplacer toutes les occurrences de `search` par `replace` dans le fichier (séparateur « \| »). |
| `web_fetch` | `web_fetch <url>` | Récupérer le contenu d’une URL (domaine autorisé dans `allowed_web_domains`). |
| `run_in_container` | `run_in_container <work_dir> <image> <command> [args...]` | Exécuter une commande dans un conteneur (work_dir monté, image Docker/podman). |
| `grep_content` | `grep_content <dir> <pattern> [file_glob]` | Chercher un motif dans le contenu des fichiers. |
| `edit_file` | `edit_file <path> <start_line> <end_line> <new_content>` | Remplacer les lignes start..end (1-based). |
| `apply_patch` | `apply_patch <path> <patch_content>` | Appliquer un patch unifié. |
| `run_terminal` | `run_terminal <cmd> [args...]` | Même sémantique que run_command. |
| `run_command_background` | `run_command_background <cmd> [args...]` | Lancer en arrière-plan ; retourne session_id. |
| `process` | `process list \| poll \| kill <session_id>` | Lister, consulter ou arrêter commandes en arrière-plan. |
| `web_search` | `web_search <query> [max_results]` | Recherche web (Brave API). Clé : vault `brave_api_key` ou env `BRAVE_API_KEY`. |
| `web_crawl` | `web_crawl <url> [options]` | Lancer un crawl de site via Cloudflare Browser Rendering (optionnel ; voir [53_web_crawl_cloudflare.md](53_web_crawl_cloudflare.md)). Domaine soumis à `allowed_web_domains` / `blocked_web_domains`. |
| `web_crawl_status` | `web_crawl_status <job_id>` | Consulter le statut et les résultats d’un job de crawl (optionnel ; voir spec 53). |
| `memory_search` | `memory_search <query> [top_k]` | Rechercher en mémoire long terme. |
| `memory_store` | `memory_store <content> <source>` | Stocker en mémoire long terme. |
| `sessions_list` | `sessions_list [limit]` | Lister les tâches récentes. |
| `sessions_spawn` | `sessions_spawn <message> [session_id]` | Créer une sous-tâche. |
| `session_status` | `session_status <task_id>` | Statut d'une tâche. |
| `message` | `message send <channel> <text>` | Envoyer un message (webhook). |
| `browser`, `image`, `pdf` | (stubs) | Prévu phase 3. |
| `device_discover` | `device_discover [interface]` | Lister les appareils accessibles (local_media, system, synthetic_input, network, usb, etc.). synthetic_input retourne keyboard, mouse. Filtre par `allowed_device_interfaces` / `blocked_device_interfaces`. |
| `device_invoke` | `device_invoke <interface> <device_id> <action> [params]` | Exécuter une action sur un appareil. `local_media` : caméra, micro (capture, record). `synthetic_input` : clavier/souris — device_id keyboard\|mouse, action shortcut\|key\|type\|mouse_move\|mouse_click\|mouse_double_click\|mouse_scroll\|mouse_drag, params JSON (ex. {\"keys\":[\"Control\",\"Shift\",\"S\"]} pour raccourci). Nécessite client UI. |
| `speech_synthesize` | `speech_synthesize <text>` | TTS : synthétiser le texte en audio. Retourne une data URL audio (WAV). Nécessite `voice_router.yaml` avec `tts.base_url`. |
| `speech_transcribe` | `speech_transcribe [data_url_audio]` | STT : transcrire l’audio en texte. Passer la data URL de l’audio (ex. après enregistrement micro) ou laisser vide si l’audio est fourni par le contexte. Nécessite `voice_router.yaml` avec `stt.base_url`. |

**Device bridge et accès appareils** : l’agent peut interagir avec **tout appareil accessible** (périphériques réseau, USB, interfaces locales). Modèle générique : `device_discover` liste les appareils (optionnellement par interface) ; `device_invoke` envoie une action à un appareil. Les interfaces (ex. `local_media`, `system`, `synthetic_input`, `network`, `usb`) sont extensibles. Pour les appareils qui nécessitent consentement ou capture côté utilisateur (caméra, micro, lecture audio), le daemon enregistre une requête dans le **device bridge** ; l’UI (Tauri) interroge `GET /api/device/pending`, exécute l’action (getUserMedia, etc.) et envoie le résultat via `POST /api/device/result`. L’interface **synthetic_input** permet à l’agent d’utiliser clavier et souris (raccourcis OS, saisie, clics, déplacements, scroll, drag) : l’UI exécute les actions via enigo et renvoie le résultat. Exemples : screenshot (raccourci selon OS : Win+Shift+S, Cmd+Shift+4), jeu (mouse_click), dessin (mouse_move, mouse_click, mouse_drag), saisie (type). Recommandation : inclure `device_invoke` dans `require_approval` pour valider chaque action. Politique : `allowed_device_interfaces` (liste ou `["*"]` pour tout) et `blocked_device_interfaces` (prioritaire), même logique que `allowed_commands` / `blocked_commands`.

Politique : `tool_profiles`, `default_profile` ; détection de boucle (3 répétitions) ; journal des modifications si `AKASHA_TOOLS_JOURNAL_PATH`. Pour web_fetch : `allowed_web_domains` peut contenir `"*"` pour autoriser tous les domaines ; `blocked_web_domains` liste les domaines (et sous-domaines) interdits, prioritaire sur l'autorisation. Pour le crawl optionnel (Cloudflare), `web_crawl_enabled` et les clés Cloudflare dans `tools_policy.yaml` activent les outils ; les domaines restent contrôlés par `allowed_web_domains` / `blocked_web_domains`.

**Approbation utilisateur (require_approval)** : dans `tools_policy.yaml`, la liste `require_approval` (ex. `[write_file, run_command, run_in_container, apply_patch, edit_file]`) impose une confirmation explicite avant exécution. Pour chaque outil listé, le daemon enregistre une entrée `TaskWaitingUserInput` avec question « Approuver l'action : &lt;outil&gt; — &lt;args&gt; ? » et choix « Approuver » / « Refuser ». Si l'utilisateur refuse ou timeout (300 s), le résultat est « Action refusée par l'utilisateur (approbation requise). ».

**Escalade et retry** : en cas d'échec (timeout LLM, budget dépassé, boucle détectée, etc.), le daemon émet `task_escalated_to_human` avec `reason` pour affichage dans l'UI. Le routeur LLM applique un backoff exponentiel (1 s, 2 s, 4 s… plafonné à 16 s) entre tentatives sur le même provider.

Activation : placer un fichier **tools_policy.yaml** dans le data_dir (voir `spec/tools_policy.example.yaml`) avec `allowed_read_paths`, `allowed_write_paths`, `allowed_commands`. Sans politique chargée, aucun outil n’est exécuté (conversation sans boucle d’outils).

---

**Projets longs** : pour des livrables substantiels (roman, BD, projet de code), contexte, isolation des fichiers et règles pour ne pas signaler « terminé » prématurément sont décrits dans [projects_long_running.md](projects_long_running.md).

## 3. Exécution en conteneur (agent de code)

- **Objectif** : lorsqu’un agent de code génère une application (ou un script), l’exécution se fait **toujours** dans un conteneur pour éviter les risques sur l’hôte.
- **Flux** : agent génère le code → le runner prépare un contexte (Dockerfile ou image prébuild + montage du code) → lance `docker run` (ou équivalent) avec limites → récupère sortie et statut → renvoie à l’agent.
- **Configuration** : image de base par défaut, répertoire de build, limites (CPU, mémoire, durée), réseau (bloqué ou whitelist). Optionnel : support podman pour environnements sans Docker.
- **Sécurité** : pas de montage de répertoires sensibles (ex. home, .ssh) sauf liste explicite ; conteneur éphémère (supprimé après exécution).

---

## 4. Skills chargeables

- **Skill** : ensemble de **définitions de capacités** (nom, description, paramètres, type de résultat) et optionnellement de **implémentations** (ex. script, WASM, ou appel à un outil existant).
- **Chargement** : au démarrage du daemon et à chaud (POST `/api/skills/reload` ou `/skills reload`), le système charge les skills depuis `data_dir/skills/` et `spec/skills/`. Formats : répertoire avec `SKILL.md` ([Agent Skills](https://agentskills.io/specification)), ou fichier `.yaml` par skill. Les skills peuvent être ajoutés pendant la session sans redémarrage.
- **Structure (Agent Skills)** : un skill peut contenir, en plus de `SKILL.md`, les dossiers optionnels `scripts/`, `references/`, `assets/` (cf. [What are skills?](https://agentskills.io/what-are-skills)). Lors de l’installation via l’outil `install_skill <url>`, le daemon télécharge `SKILL.md` puis récupère récursivement tous les fichiers du même chemin GitHub (scripts, références, assets), écrit le tout dans `data_dir/skills/<nom>/`, recharge le registre, et renvoie le contenu du skill ainsi que le chemin du répertoire du skill pour que l’agent puisse résoudre les références de fichiers (ex. `read_file` sur `references/REFERENCE.md`).
- **Sources d’installation** : un skill peut être hébergé ailleurs que sur GitHub (site web, GitLab, Bitbucket, etc.). Dans `tools_policy.yaml`, la clé **`allowed_skill_install_hosts`** liste les hôtes autorisés (ex. `gitlab.com`, `mon-site.com`). Utiliser `["*"]` pour autoriser toute URL HTTPS. Par défaut, seuls les hôtes GitHub sont autorisés. Pour les hôtes non-GitHub, seul le fichier `SKILL.md` est téléchargé (pas de listing de répertoire) ; l’URL doit pointer vers un fichier `.md` ou vers un chemin dont le contenu `SKILL.md` est accessible (ex. `https://example.com/skills/mon-skill/SKILL.md`).
- **Attribution aux agents** : l’orchestrateur (ou la config par type d’agent) associe à chaque agent un sous-ensemble de skills. Lors de la délégation, l’agent peut voir et utiliser uniquement les skills qui lui sont attribués.
- **Compatibilité plugins** : réutiliser si possible le mécanisme de plugins existant (Phase 5) pour des skills en WASM, ou définir un type « skill » dans le manifest et l’API plugin.

---

## 5. Orchestrateur comme seul point d’entrée — flux non bloquant

### 5.1 Comportement attendu

1. **Utilisateur** envoie un message (TUI, Web, Slack, etc.) → **API** reçoit le message.
2. **Orchestrateur** (point d’entrée unique) :
   - Répond **immédiatement** à l’utilisateur : « Je prends en compte votre demande » (ou variante), avec un **identifiant de demande** (ex. `request_id` / `task_id`).
   - Enregistre la demande comme **tâche racine** et la met en file (ou la traite dans un **thread/tâche asynchrone** dédié).
3. **Délégation** : dans ce thread/tâche, l’orchestrateur :
   - Choisit l’**agent cible** (classification de la demande : code, recherche, fichier, etc.).
   - Délègue la tâche à cet agent (sans bloquer les autres demandes).
4. **Agent cible** (et éventuellement ses sous-agents) :
   - Utilise outils machine et skills si besoin.
   - Pour un agent de code qui génère une app : exécution dans un conteneur.
   - Renvoie le résultat (et éventuellement des événements intermédiaires) à l’orchestrateur.
5. **Orchestrateur** reçoit la réponse finale → **transmet à l’utilisateur** (via le canal d’origine : Web, TUI, Slack, etc.) et met à jour le statut de la tâche.

Tout le chemin « délégation → agent → sous-agents → résultat » est **non bloquant** : l’API et l’orchestrateur peuvent accepter d’autres messages et créer d’autres tâches en parallèle.

### 5.2 Modèle technique (aligné avec l’existant)

- **Entrée** : `POST /api/message` (ou équivalent canal) crée toujours une **tâche racine** et envoie son `task_id` à l’orchestrateur (via une file, ex. `orchestrator_tx`).
- **Réponse immédiate** : le handler API renvoie tout de suite `{ "ack": true, "task_id": "...", "message": "Je prends en compte votre demande." }` (sans attendre la fin du traitement).
- **Traitement asynchrone** : un worker (orchestrateur) consomme la file des `task_id`, pour chaque tâche :
  - Décompose la demande via un appel LLM (agent_type|message) ; le type d’agent (conversation, code, search, schedule, financial, documentalist, project_manager, technical_writer, research, security_audit, creative) est entièrement déterminé par le LLM, sans fallback par mots-clés.
  - Délègue à l’agent (nouvelle sous-tâche ou envoi sur une file dédiée à l’agent).
  - Les agents (et sous-agents) s’exécutent dans des tâches asynchrones (tokio::spawn ou équivalent).
- **Remontée du résultat** : quand l’agent final a terminé, il met à jour la tâche racine (statut, résultat) et envoie un événement (ex. `TaskCompleted` avec le texte de réponse). Le **progress subscriber** (ou un composant dédié) pousse ce résultat au canal utilisateur (polling `GET /api/tasks/:id` ou WebSocket si ajouté).

Aujourd’hui, `POST /api/message` appelle `main_agent.handle_message(..., false)` et fait ensuite le LLM dans un spawn ; il ne passe pas par l’orchestrateur pour la réponse. L’évolution consiste à :
- Toujours passer par l’orchestrateur (`forward_to_orchestrator: true`) pour les demandes « conversation / travail agent ».
- Faire en sorte que l’orchestrateur décide (classification) et délègue à un agent spécialisé (dont un agent « conversation / LLM ») au lieu de faire un chemin court direct LLM dans l’API.

### 5.3 Comportement conversationnel — tâches longues

Quand une tâche est longue :

- **Main agent** envoie un **ACK immédiat** : « Ok, je lance ça » (ou variante) + `task_id`.
- Il propose le lien : « Tu peux suivre l’avancement dans l’onglet Tâches. »
- L’utilisateur peut **continuer à parler** ; le main agent gère la discussion sans attendre la fin de la tâche.
- **Mises à jour** : option « chips » dans le chat (ex. Task #1234 running 35 %) ; flux complet dans le Task Center (onglet Tâches).

Voir [36_ui_architecture.md](36_ui_architecture.md) pour les onglets Chat et Tâches.

### 5.4 Types d'agents et prompt [Role]

Types d'agents reconnus : **conversation** (chat général), **code** (génération de code), **search** (recherche d'information), **schedule** (création de tâche récurrente dans l'app, flux dédié), **financial** (budget, coûts, rapports), **documentalist** (réponses basées sur la base RAG / documents utilisateur), **project_manager** (suivi de projet, jalons, planning), **technical_writer** (rédaction technique, doc, procédures), **research** (recherche approfondie, synthèse multi-sources), **security_audit** (revue sécurité code/config), **creative** (rédaction créative, copywriting).

Chaque type spécialisé (tous sauf **schedule**) reçoit un **prompt système [Role]** en anglais injecté en tête du contexte LLM pour guider son comportement. L'outil `delegate_to_agent` accepte les types : search, code, conversation, financial, documentalist, project_manager, technical_writer, research, security_audit, creative (un seul niveau de délégation).

---

## 6. Sous-agents et décomposition

- Un agent qui a reçu une tâche peut **créer des sous-tâches** et les attribuer à des **sous-agents** (réels ou logiques) pour décomposer le travail ou paralléliser.
- Modèle de données : tâche avec `parent_task_id` (déjà présent dans `Task`). Un agent « parent » crée des tâches enfants, les assigne à des workers ou à d’autres agents, et agrège les résultats.
- L’orchestrateur actuel fait déjà une décomposition fixe (2 sous-tâches). Il s’agit de généraliser : décomposition **dynamique** (nombre et type de sous-tâches selon la demande et le résultat de la classification), et permettre à **tout agent** ayant la capacité « spawn sub-agents » de créer des enfants.
- Événements à émettre : `SubAgentSpawned`, `TaskDecomposed`, etc., pour alimenter l’UI (onglets agents / actions).

---

## 7. Interfaces TUI et Web — onglets agents et actions

- **Onglet « Agents » (ou « Échanges »)** : afficher les **échanges** entre utilisateur et orchestrateur, et entre orchestrateur et agents (délégations, réponses intermédiaires, réponse finale). Idéalement par demande (task_id) ou par fil de conversation.
- **Onglet « Actions »** : afficher les **actions** effectuées par les agents (outils utilisés : fichier lu/écrit, commande lancée, recherche web, conteneur exécuté, etc.) avec horodatage, agent, et résumé (succès/échec).
- Données : les événements existants (`TaskCreated`, `TaskDecomposed`, `SubAgentSpawned`, `ProgressUpdate`, `TaskCompleted`, `TaskFailed`) plus de **nouveaux types d’événements** (ex. `ToolInvoked`, `AgentDelegated`, `AgentReplied`) à produire côté daemon et à exposer via API (ex. `GET /api/tasks/:id/events` ou stream d’événements).
- TUI : au moins un onglet « Agents » ou « Activité » listant les tâches récentes et les événements associés ; optionnel onglet « Actions » (liste des outils appelés).
- Web : mêmes onglets avec liste / fil d’événements et liste d’actions, avec rafraîchissement (polling ou WebSocket).

---

## 8. Plan de mise en œuvre par phases

### Phase A — Outils machine et politique (fondation)

- Créer le module ou crate **outils** : read_file, write_file, search_files, run_command (avec timeout et répertoire autorisé), optional file_diff, optional web_search (client HTTP).
- Introduire une **politique** (config ou YAML) : chemins autorisés, commandes autorisées, timeouts.
- Exposer ces outils aux agents via une interface commune (ex. `AgentContext` ou `ToolRegistry`) utilisable depuis l’orchestrateur et les workers.

### Phase B — Orchestrateur comme seul entrée, délégation non bloquante

- Faire en sorte que **toutes** les demandes utilisateur (TUI, Web, canaux) passent par l’orchestrateur (`handle_message(..., true)`).
- Réponse immédiate côté API : « Je prends en compte votre demande » + `task_id`, sans attendre la fin du traitement.
- Adapter l’orchestrateur pour : classification de la demande (règles ou LLM) → choix de l’agent (pour l’instant au moins : « conversation » → agent LLM actuel, autres types → workers ou stubs).
- S’assurer que plusieurs tâches peuvent être en cours en parallèle (file + spawn par tâche).

### Phase C — Conteneur pour agent de code

- Implémenter un **container runner** (Docker ou podman) : build optionnel (Dockerfile généré ou image de base), run avec limites, capture stdout/stderr, timeout.
- L’agent de code (ou le worker qui exécute du code généré) appelle ce runner au lieu d’exécuter sur l’hôte.
- Config : image de base, répertoire de build, limites (CPU, mémoire, durée).

### Phase D — Skills chargeables

- Définir le format de **skill** (YAML/TOML : nom, description, paramètres, liaison à un outil ou plugin).
- Charger les skills au démarrage (et optionnellement à chaud) depuis un répertoire ou un registre.
- Associer les skills aux agents (config ou attributs par type d’agent) et exposer les skills disponibles dans le contexte d’exécution de chaque agent.

### Phase E — Sous-agents dynamiques

- Généraliser l’orchestrateur (et à terme tout agent autorisé) pour créer un **nombre variable** de sous-tâches selon la demande.
- Utiliser la classification / le LLM pour proposer une décomposition (ex. « étapes 1, 2, 3 ») et créer les sous-tâches et sous-agents en conséquence.
- Agrégation des résultats des sous-tâches pour produire la réponse finale de la tâche parente.

### Phase F — UI : onglets Agents et Actions

- Émettre des événements **ToolInvoked** (et si besoin **AgentDelegated** / **AgentReplied**) côté daemon.
- API : `GET /api/tasks/:id/events` (et optionnellement `GET /api/tasks/:id/actions` ou inclus dans events).
- TUI : onglet « Agents » ou « Activité » (liste tâches + événements), optionnel onglet « Actions ».
- Web : onglets « Agents » et « Actions » avec listes / fils d’événements et d’actions.

---

## 9. Résumé des livrables cibles

| Livrable | Description |
|----------|-------------|
| Outils machine | read_file, write_file, search_files, run_command, (diff, web_search), avec politique de sécurité |
| Container runner | Exécution de code généré dans un conteneur (Docker/podman), limites et timeout |
| Skills | Format skill, chargement, attribution aux agents |
| Orchestrateur unique | Toutes les demandes passent par l’orchestrateur ; ack immédiat ; délégation asynchrone |
| Sous-agents | Décomposition dynamique de tâches et création de sous-agents par un agent parent |
| UI Agents / Actions | Onglets TUI et Web pour échanges avec les agents et actions (outils) effectuées |

Ce document sert de référence pour les implémentations futures ; chaque phase peut être détaillée en tâches techniques dans le suivi de projet.

---

## 10. État d’implémentation

| Phase | Statut | Détail |
|-------|--------|--------|
| **A** | Fait | `ToolExecutor` branché dans le flux conversation : `run_message_via_llm` accepte `tools_executor` optionnel, parse les lignes `TOOL: tool_name args`, exécute read_file, write_file, search_files, run_command, file_diff, search_replace, web_fetch (feature web), run_in_container (feature container) et réinjecte les résultats (boucle agentique, max 3 tours). |
| **B** | Fait | Orchestrateur seul point d’entrée ; ack immédiat ; délégation non bloquante via conversation worker. |
| **C** | Fait | Module `container` dans `akasha-tools` (feature `container`) : `run_container`, `run_code_in_container`. Outil **run_in_container** exposé aux agents (work_dir, image, commande, args) ; exécution dans le flux conversation. |
| **D** | Fait | Module `skills` : `SkillRegistry`, chargement YAML, `GET /api/skills`. Liaison skill → outil : les skills sont listés dans le prompt ; lors de l’invocation par le LLM, le nom du skill est résolu en `tool_ref` et l’outil sous-jacent est exécuté. |
| **E** | Fait | `decompose_request` appelle le LLM pour décomposer la demande en sous-tâches (format `agent_type|message`) ; multi-enfants délégués au worker conversation ; agrégateur remonte les messages des enfants vers la tâche racine (ProgressUpdate) puis marque la racine complétée. |
| **F** | Fait | Cache d’événements, `GET /api/tasks`, `GET /api/tasks/:id/events`. Événements **ToolInvoked** émis à chaque appel d’outil (tool, args, result_preview, success) pour l’onglet Actions. Onglet **Activité** dans la TUI et l’UI web (liste tâches, événements). |
| **Session terminal** | Prévu | « Utiliser le terminal » (session stdin/stdout/stderr) est optionnel dans la spec ; non implémenté, prévu pour une version ultérieure. |
