# Données, RAG et mémoire

## RAG utilisateur

Paramètres → **Données** → **RAG utilisateur** : indexez des documents texte ; l'agent reçoit des extraits pertinents dans le contexte.

API : `/api/user-rag/...`

## Mémoire

Onglet **Mémoire** : mémoire court terme (session) et long terme (persistante). Recherche, vue graphe, suppression d'entrées.

## Graphe projet

Paramètres → **Données** → **Graphe projet** : enregistrez plusieurs racines de projet ; index SQLite ; rapports HTML sous `workspace_graph/out/<id>/` dans le data_dir.

Outil agent : `workspace_graph_search` (si autorisé dans `tools_policy.yaml`).

API : `/api/workspace-graph/workspaces`
