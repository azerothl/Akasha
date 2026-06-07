# Démo compétitive — Akasha vs Letta (MemGPT)

Walkthrough opérateur (~15 min) — Letta met l’accent sur **agents stateful** avec mémoire en blocs et handoff ; Akasha sur **daemon 24/7**, graphe de faits et gouvernance opérateur.

## Prérequis

- Daemon Akasha actif (port 3876)
- Desktop UI ou TUI pour visualiser tâches / mémoire
- Connaissance légère Letta : core memory, archival memory, agents serveur

## Narratif produit

| Critère | Letta (typ.) | Akasha |
|---------|--------------|--------|
| Déploiement | Serveur Letta + agents configurés | Daemon Rust + data_dir unique |
| Mémoire agent | Blocs éditables (persona, human, …) | `agent_identity.yaml` + épisodes + faits SPO |
| Mémoire archival | Recherche dans archival store | Long terme SQLite + embeddings + consolidation |
| Multi-agent | Agents Letta communicants | Orchestrateur, délégation, inter-agent module |
| Identité persistante | Agent config serveur | Bootstrap identité + export/import bundle |

## Scénario 1 — Identité agent (vs core memory Letta)

1. Éditer `data_dir/agent_identity.yaml` (nom, rôle, règles).

2. Redémarrer daemon ou attendre promote auto au boot.

3. Demander : « Qui es-tu et quelles règles suis-tu ? » — comparer à un bloc `persona` Letta statique.

4. Montrer **formality** dans `agent_profile.json` (v0.8+) — ton utilisateur sans redéployer l’agent Letta.

## Scénario 2 — Mémoire archival + compaction

1. Longue conversation (> seuil compaction 75 % contexte).

2. Observer compaction court terme → résumé système + promotion LT automatique.

3. `GET /api/memory/short-term?session_id=…` vs recall LT injecté.

**Point de vente :** compaction **locale** sans quota serveur Letta ; plafond `MAX_COMPACTIONS_PER_SESSION`.

## Scénario 3 — Knowledge graph (faits SPO)

1. Stocker des faits relationnels via conversation ou outil.

2. Montrer table `facts` + expansion graphe au recall (`graph_expand_hops` profil enriched).

3. Contraste Letta : retrieval surtout archival textuel ; Akasha expose **relations** explicites (`spec/46_memory_facts_knowledge_graph.md`).

## Scénario 4 — Cycle de vie / héritage (partiel vs Letta persistence)

1. `GET /api/memory/export` → sauvegarde JSON schema v1.

2. Simuler « nouvelle instance » : `POST /api/memory/import` sur data_dir de test.

3. Mentionner **fork session** Code Studio (`session_fork_created`) — branche conversation sans perdre audit.

**Limite honnête :** pas de serveur Letta-style multi-tenant ; héritage inter-upgrade = P2 ([memory_landscape roadmap E2](../../spec/dev/roadmap/memory_landscape_roadmap_matrix.md)).

## Scénario 5 — Gouvernance opérateur (différenciation)

1. Action sensible → entrée `GET /api/permissions/queue?status=pending`.

2. Après tâche : `GET /api/tasks/:id/report` (`done`, `needs_review`, `next_steps`).

Letta n’expose pas ce modèle queue + rapport post-tâche intégré au daemon.

## Clôture démo

- Akasha = **second brain local** + outils machine + Code Studio ; Letta = **plateforme agents** avec API dédiée.
- Pour intégration Letta-like externe : HTTP [memory_api_external.md](../../spec/dev/integrations/memory_api_external.md).

## Références

- [spec/06_memory_model.md](../../spec/06_memory_model.md)
- [kinbot-akasha-parity-matrix.md](../../spec/dev/roadmap/kinbot-akasha-parity-matrix.md) (profils, inter-agent)
