# Graphes de connaissances « projet » (multi-workspaces)

Ce document décrit le **graphe projet** : indexation locale d’un ou plusieurs dossiers de code / documentation dans SQLite (`akasha.db`), avec artefacts sur disque et usage par les agents.

## Modèle de données (SQLite)

- Table **`workspace_graph_workspaces`** : `id` (UUID), `name` (unique), `root_path`, `created_at`.
- Table **`workspace_graph_build`** : une ligne par workspace (`workspace_id`, `root_path` indexé, `built_at`, `file_count`).
- Tables **`workspace_graph_nodes`** / **`workspace_graph_edges`** : données scoping par `workspace_id` (clés composites, `ON DELETE CASCADE`).

Les identifiants de nœuds sont préfixés par workspace dans l’indexeur (`akasha-workspace-graph`) pour éviter les collisions entre projets.

## Fichiers sur disque

Pour chaque workspace : **`{data_dir}/workspace_graph/out/{workspace_id}/`** — `graph.json`, `GRAPH_REPORT.md`, `graph.html`.

La suppression d’un workspace (API ou UI) efface les lignes SQL associées et le sous-dossier `out/{id}/` lorsque présent.

## API HTTP (daemon)

Préfixe : **`/api/workspace-graph`**.

| Méthode | Route | Rôle |
|--------|--------|------|
| GET | `/workspaces` | Liste des workspaces + compteurs (nœuds, arêtes, `built_at`, etc.) |
| POST | `/workspaces` | Corps `{ "name", "root_path", "rebuild": true/false }` — crée et optionnellement indexe |
| DELETE | `/workspaces/:id` | Supprime BDD + artefacts disque |
| POST | `/workspaces/:id/rebuild` | Corps optionnel `{ "root_path" }` pour corriger la racine puis reconstruire |
| GET | `/workspaces/:id/export` | Export JSON du graphe |
| GET | `/workspaces/:id/report` | Rapport Markdown |
| GET | `/workspaces/:id/html` | Visualisation HTML |

Routes **legacy** (`GET /api/workspace-graph`, `PUT .../config`, `POST .../rebuild` sans id) : ciblent le premier workspace ou importent `workspace_graph.yaml` si la base est vide.

## Interface desktop (Tauri)

**Paramètres → Données** : deux sous-onglets — **RAG utilisateur** et **Graphe projet** (zone scrollable). Le graphe permet d’ajouter des workspaces (nom + chemin), de reconstruire, d’ouvrir le HTML par id, et de supprimer (avec confirmation).

## Agents

1. **Injection automatique** (profil mémoire « enrichi », comme le RAG utilisateur) : recherche lexicale sur les champs `label` / `path` des nœuds ; jusqu’à **5** lignes courtes injectées sous le préfixe `[Project knowledge graphs — …]`. Désactivé pour les sous-agents et le profil mémoire minimal (fast path).

2. **Outil** **`workspace_graph_search`** : `workspace_graph_search <requête> [--workspace <uuid>]` — jusqu’à ~20 lignes ; lecture seule, lane parallèle sûre dans `akasha-tools`. À inclure dans `tool_profiles` si `default_profile` est utilisé.

## Implémentation

- Persistance et recherche : `crates/akasha-store/src/workspace_graph.rs` (`WorkspaceGraphStore::search_graph_context`).
- Indexation : `crates/akasha-workspace-graph`.
- HTTP : `crates/akasha-daemon/src/api_workspace_graph.rs`.
- Prompt : `crates/akasha-daemon/src/api.rs` (`MemoryProfile::workspace_graph_top_k`, construction de `user_prefix`).
