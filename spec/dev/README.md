# Documentation dev par blocs fonctionnels

## Blocs

- `core/`: architecture centrale et decisions transverses
- `interfaces/`: TUI, web/desktop, UX technique
- `integrations/`: MCP, webhooks, bridges externes  
  - Voir aussi [pi_mono_backend_parity_check.md](integrations/pi_mono_backend_parity_check.md) (streaming outils, tokens/coût vs pi-ai).
- `plugins/`: host plugin, schemas, strategie plugin
- `runtime/`: execution, terminal backends, cache, hooks runtime  
  - Voir aussi [agent_client_event_contract.md](runtime/agent_client_event_contract.md) (événements agent côté client, alignement Pi).
- `ops/`: exploitation interne, SLO, runbooks d'operation
- `quality/`: tests, benchmarks, qualification technique
- `releases/`: notes techniques par version
- `roadmap/`: alignement produit/technique, plans et migration doc  
  - Voir aussi [pi_mono_alignment_priorities.md](roadmap/pi_mono_alignment_priorities.md) (axes prioritaires post-analyse pi-mono).
