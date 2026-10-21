# Données, RAG et mémoire

## RAG utilisateur

Paramètres → **Données** → **RAG utilisateur** : indexez des documents texte ; l'agent reçoit des extraits pertinents dans le contexte. L'indexation peut être **asynchrone** (tâche en arrière-plan) : suivez l'état dans l'interface ou via l'API.

API : `/api/user-rag/...`

## Mémoire

Onglet **Mémoire** : mémoire **court terme** (session en cours) et **long terme** (persistante dans `memory.db` sous le data_dir).

### Court terme et compaction

Lorsque le contexte de session approche la limite (`AKASHA_MAX_CONTEXT_TOKENS`, défaut 8192), le daemon **compacte** l'historique : résumé LLM des messages anciens, conservation des messages récents. Vous n'avez en général rien à configurer ; en cas de réponses qui « oublient » le début d'une longue conversation, augmentez la limite via `akasha config env set` ou réduisez la taille des pièces jointes dans le chat.

### Long terme (rappel sémantique)

Les faits utiles peuvent être **promus** en mémoire long terme (automatiquement ou via les outils agent selon la politique). Le rappel utilise des **embeddings locaux** (modèle sous `data_dir/embedding_model/`, fourni avec l'application — pas d'API tierce obligatoire). Recherche par similarité dans l'onglet Mémoire (**/** ou **S**), suppression avec **D** ou **Suppr**.

### Vue graphe (knowledge graph)

Dans l'onglet Mémoire, **G** bascule une **vue graphe** des entrées long terme et de leurs liens. Cela complète le **graphe projet** (fichiers indexés) décrit ci-dessous.

### Gouvernance du rappel (optionnel)

Copiez `docs/constitution.yaml.example` vers `~/akasha/constitution.yaml` pour filtrer ce qui peut être rappelé (règles de gouvernance mémoire).

### Limites connues

Sur **Windows**, la mémoire long terme peut être absente si le binaire a été compilé sans la feature store (voir dépannage). Sous Linux/macOS ou zip **full**, la mémoire LT est en général disponible.

## Graphe projet

Paramètres → **Données** → **Graphe projet** : enregistrez plusieurs racines de projet ; index SQLite ; rapports HTML sous `workspace_graph/out/<id>/` dans le data_dir.

Outil agent : `workspace_graph_search` (si autorisé dans `tools_policy.yaml`).

API : `/api/workspace-graph/workspaces`
