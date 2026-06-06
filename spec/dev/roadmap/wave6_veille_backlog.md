# Veille long terme — roadmap consolidée (vague 6)

Statut **surveillance** — pas de sprint actif. Références : [`memory_phase4_backlog.md`](memory_phase4_backlog.md), [`memory_encryption_rfc.md`](memory_encryption_rfc.md), [`pi_mono_alignment_priorities.md`](pi_mono_alignment_priorities.md).

| ID | Item | Prochaine étape |
|----|------|-----------------|
| A4 | Chiffrement `memory.db` | RFC [`memory_encryption_rfc.md`](memory_encryption_rfc.md) → décision SQLCipher vs vault wrapper |
| B2 | BM25 explicite | Bench FTS5 sur corpus interne avant toute crate BM25 |
| D4/D6 | GraphRAG communautés / hypergraphes | Pilote `project:*` si demande produit |
| E1 | Gouvernance CMA 2 niveaux | Constitution YAML + couche opérationnelle DB (spec 06) |
| E2/E3 | Héritage mémoire / fork identité | Hook upgrade daemon + export/import branché |
| G2 | `MemoryPlugin` trait | Brancher sur HTTP interne `/api/memory/*` |
| G1 | Interop LangGraph | Exemple Python dans `spec/dev/integrations/` |
| pi-mono | Handoff modèle explicite | Prochaine priorité pi-mono ([`pi_mono_alignment_priorities.md`](./pi_mono_alignment_priorities.md)) ; steering/follow-up livré |
| pi-mono | `toolcall_delta` streaming | Backend + [`agent_client_event_contract.md`](../runtime/agent_client_event_contract.md) |
| pi-mono | RPC stdio JSONL | Optionnel partenaire IDE |
| pi-mono | CSI / TUI différentiel | Faible priorité |

**Dernière mise à jour :** 2026-06-06 (wave 7 closure — voir [`ROADMAP_CLOSURE_STATUS.md`](./ROADMAP_CLOSURE_STATUS.md)).
