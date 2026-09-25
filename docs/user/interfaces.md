# Interfaces

## ## 6. Interfaces et onglets


### Interface terminal (TUI)

- **Onglets** : Chat, **Retours planifiés** (réponses des tâches récurrentes), Routeur (métriques), Doc (cette documentation), Tâches, Calendrier, Mémoire. L’onglet **Mission autonome** et les **Paramètres** complets sont disponibles dans l’interface web / desktop uniquement ; la mission peut toutefois être configurée via **`autonomous_mission.yaml`** ou l’API.
- **Chat** : uniquement la conversation avec l’agent (messages envoyés et réponses). **Retours planifiés** : uniquement les réponses de l’agent pour les rappels / tâches planifiées (schedules), sans les mélanger au fil du chat.
- **Raccourcis** : Tab (changer d'onglet), Entrée (envoyer un message), R (rafraîchir Routeur, Retours planifiés ou liste des tâches), ↑/↓ PgUp/PgDn Home/End (défilement), Échap ou Ctrl+Q (quitter). **Onglet Tâches** : ↑/↓ (sélectionner une tâche), D (filtrer racines uniquement). **Onglet Mémoire** : / ou S (recherche), G (basculer vue graphe), D ou Suppr (supprimer l'entrée long terme sélectionnée).

### Interface web / desktop (si installée)

- **Onglets** : Chat (1), Comparer (2), Recherche (3), Cookbook, Notes, Tâches (4), Retours planifiés, Routeur (5), Calendrier (6), Mémoire (7), Documentation (8), Mission (9), Paramètres. **Chat** = conversation uniquement ; **Retours planifiés** = réponses de l’agent pour les tâches planifiées (rappels récurrents), dans un onglet dédié.
- **Raccourcis** : touches **1 à 9** pour basculer vers l'onglet correspondant (Documentation = **8**, Mission = **9** ; inactif si le focus est dans un champ de saisie ou une modale).
- **Pièces jointes** : dans le Chat, vous pouvez joindre des images ou des documents (texte, PDF) ; l'agent les reçoit pour analyse.
- **Modes composer (v0.10)** : barre sous le champ — **Ask** (lecture seule), **Architecte** (conception), **Code** (implémentation), **Agent** (défaut). Envoyé au daemon via `composer_mode`.
- **Travail actif** : bandeau listant les tâches en cours (ouvrir / annuler / pause) ; indicateur sur les sessions liées.
- **Sessions** : épingler, forker le transcript, option double panneau.
- **Usage** : Paramètres → Système → Usage — agrégats tokens / coût estimé sur 7 ou 30 jours.
- **Données** : dans Paramètres → **Données**, deux sous-onglets — **RAG utilisateur** (documents texte indexés, extraits injectés dans le contexte de l’agent) et **Graphe projet** (plusieurs dossiers de projet enregistrés, index SQLite + rapports sous `workspace_graph/out/<id>/` ; ouverture du HTML par workspace ; agents enrichis automatiquement et outil `workspace_graph_search` si autorisé). Sans interface web, gérer via `/api/user-rag/...` et `/api/workspace-graph/workspaces` (voir les autres pages de cette documentation).
- **Profil de l'agent** : dans Paramètres → Profil de l'agent, vous pouvez définir le nom, le rôle, la personnalité, les règles et les comportements autorisés/interdits ; des modèles (Neutre, Bienveillant, Concis/technique, etc.) sont proposés. Depuis la version **0.8.0**, un réglage **Tutoiement / vouvoiement** (formel, informel ou par défaut) oriente le registre de l'agent — en français, cela correspond au vouvoiement ou au tutoiement ; dans les autres langues, le registre s'adapte de la même manière.
- **Mission autonome** : onglet **Mission** pour définir un objectif de fond, le contexte, des règles, des rôles (organisation) et la fréquence des **heartbeats**. Tant que la mission est activée et **active**, le daemon lance périodiquement une tâche orchestrée (type d’agent du premier pas configurable, souvent *chef de projet*) ; l’orchestrateur peut déléguer à d’autres agents. Les rapports Markdown vont dans le répertoire configuré (relatif au data_dir). Fichier **`autonomous_mission.yaml`** ; API **`GET` / `PUT /api/autonomous-mission`**, pause/reprise **`POST`** sur `/api/autonomous-mission/pause` et `/resume`. L’historique des événements de mission est consultable via **`GET /api/autonomous-mission/events`** (paramètres optionnels `limit`, `since` en date ISO). Pour appliquer aussi au **chat** le mode « sans questions » lié à la mission, utilisez le même **`session_id`** que dans la fiche mission. *Exemple* : maintenir un fichier `CHANGELOG_HEBDO.md` à jour dans un dépôt — renseignez l’objectif et le contexte (chemin du dépôt), horizon moyen, heartbeat 120 min, consultez les rapports sous le dossier indiqué après quelques cycles.
- **Plugins** : la vue plugins affiche l’état d’activation et permet de réinitialiser la réputation d’un plugin (ou de tous les plugins) si nécessaire. Les règles `routing_rules` des manifests ne sont plus appliquées pour forcer ou bloquer des outils en runtime (sélection par description via le modèle `system` + `tools_policy.yaml` pour l’exécution).

Le daemon écoute par défaut sur le port **3876**. Pour que l'onglet Doc affiche ce guide, lancez `akasha start` depuis le dossier où vous avez extrait l'archive (contenant le dossier `docs`).

---

Voir aussi la page **Workspace** pour Cookbook, Comparer et Recherche.
