# Documentation dev par blocs fonctionnels

## Blocs

- `architecture/`: carte interactive crates/runtime/flows ([architecture-diagram.html](architecture/architecture-diagram.html), [architecture-graph.json](architecture/architecture-graph.json)) — **à régénérer à chaque release** (voir [architecture/README.md](architecture/README.md)).
- `core/`: architecture centrale et decisions transverses
- `interfaces/`: TUI, web/desktop, UX technique
- `integrations/`: MCP, webhooks, bridges externes  
  - Voir aussi [pi_mono_backend_parity_check.md](integrations/pi_mono_backend_parity_check.md) (streaming outils, tokens/coût vs pi-ai).
  - [akasha-os-sibling-skills-modules.md](integrations/akasha-os-sibling-skills-modules.md) — P9 interop skills/plugins daemon ↔ skills/modules Preview (sans fusion binaire).
- `plugins/`: host plugin, schemas, strategie plugin
- `runtime/`: execution, terminal backends, cache, hooks runtime  
  - Voir aussi [agent_client_event_contract.md](runtime/agent_client_event_contract.md) (événements agent côté client, alignement Pi).
- `ops/`: exploitation interne, SLO, runbooks d'operation
- `quality/`: tests, benchmarks, qualification technique
- `releases/`: notes techniques par version
- `roadmap/`: alignement produit/technique, plans et migration doc  
  - Voir aussi [pi_mono_alignment_priorities.md](roadmap/pi_mono_alignment_priorities.md) (axes prioritaires post-analyse pi-mono).
  - [memory_landscape_roadmap_matrix.md](roadmap/memory_landscape_roadmap_matrix.md) — rapport « mémoire agents 2026 » vs implémentation Akasha (Fait/Partiel/Gap + phases P0–P3).
  - [memory_phase4_backlog.md](roadmap/memory_phase4_backlog.md) — backlog CMA / GraphRAG / plugins.
  - [memory_api_external.md](integrations/memory_api_external.md) — contrat HTTP mémoire externe.
