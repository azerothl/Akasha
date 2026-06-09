# Nouveautés


## Nouveautés 0.9.0

- **Workspace** : Cookbook (Modèles + Recettes), Comparer, Recherche approfondie, Notes
- **Navigation** : barre latérale par groupes (Principal, Workspace, Opérations, Données, Aide)
- **Documentation** : pages thématiques dans l'onglet Doc
- **Plugins canaux** : Matrix et CalDAV (sidecar) dans le catalogue public
- **Code Studio** : `npx akasha-code-studio@0.9.0`


---

## ## 14. Nouveautés depuis la version 0.7.0 (version 0.8.0)


### Mission autonome

- **Objectif périodique** : définissez une mission de fond (objectif, contexte, règles, rôles) et une fréquence de **heartbeat** ; le daemon lance des cycles d’orchestration tant que la mission est active. Rapports Markdown dans le répertoire configuré ; fichier **`autonomous_mission.yaml`** dans le data_dir.
- **Interface** : onglet **Mission** (application web / desktop) pour configurer, suivre l’activité, mettre en pause ou reprendre. API : **`GET` / `PUT /api/autonomous-mission`**, **`POST .../pause`** et **`.../resume`**, journal **`GET /api/autonomous-mission/events`**.
- **Chat aligné sur la mission** : en réutilisant le même **`session_id`** que la mission, le comportement « sans questions inutiles » du mode mission s’applique aussi aux messages du chat.

### Graphe projet et mémoire

- **Plusieurs dossiers projet** : enregistrez plusieurs **workspaces** (nom + chemin racine) ; chaque indexation produit un graphe (fichiers sous `workspace_graph/out/<id>/` dans le data_dir, dont une visualisation HTML).
- **Aide à l’agent** : l’agent reçoit automatiquement des extraits pertinents du graphe quand votre question touche des fichiers ou symboles indexés ; l’outil **`workspace_graph_search`** permet une recherche ciblée (voir la politique d’outils si vous restreignez les outils).
- **Interface** : sous Paramètres → Données → **Graphe projet**, gestion des espaces, reconstruction et ouverture du rapport HTML ; présentation des résultats de recherche en **Mémoire** améliorée.

### Profil de l’agent

- **Tutoiement / vouvoiement** : réglage explicite (formel, informel ou par défaut) pour que l’agent vous parle avec le registre souhaité.

### Fiabilité et expérience

- **Orchestration et outils** : mode **outils d’abord** renforcé lorsque la configuration l’exige — premier appel d’outil plus prévisible pour certaines tâches.
- **Réponses plus lisibles** : lorsque le modèle termine par un bloc JSON de « contrat » technique, l’interface peut n’afficher dans le résumé que la partie utile pour vous, sans ce bloc brut.
- **Suggestions de projet** : meilleure détection et suivi lorsque l’agent propose d’associer un dossier à un graphe projet.
- **`akasha doctor`** : diagnostics et **`doctor --fix`** mieux adaptés aux installations à partir du **zip** (dossier `spec`, chemins à côté des binaires).
- **Vérification de version** : le produit et le site des releases doivent afficher la même version ; en cas de bannière de mise à jour incohérente, vérifiez que vous avez bien installé l’archive correspondant à la version annoncée.

### Pour les utilisateurs avancés (ligne de commande)

- Les invites intégrées à l’agent peuvent recommander des **CLI orientés agent** (par ex. pour GitHub ou le navigateur) si vous les installez vous-même — ce n’est pas obligatoire ; Akasha conserve ses outils intégrés navigateur et `run_command` habituels.

### Rappel — nouveautés déjà présentes en 0.7.0

- Route **`orchestrator`** dans `llm_router.yaml`, livrables vérifiés sur disque, sécurité des chemins de livrables, retry ciblé, fichier de plan `.akasha/plan_<id>.md`, politique de chemins renforcée pour les fichiers, uploads RAG/contrat pièces jointes, limites sûres sur les paramètres numériques du routeur LLM.
