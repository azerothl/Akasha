> **Archive:** Ce document est archivé. Statut : **Livré / Archivé** (2026-06-06). Source de vérité active : [`ROADMAP_FINAL_REGISTRY.md`](./ROADMAP_FINAL_REGISTRY.md).

# Veille long terme — roadmap consolidée (vague 6)

**Statut : Livré / Archivé** — items actifs promus Production ; items ouverts reclassés **Reporter** dans le registre final.

Références : [`memory_phase4_backlog.md`](memory_phase4_backlog.md), [`memory_encryption_rfc.md`](memory_encryption_rfc.md), [`pi_mono_alignment_priorities.md`](pi_mono_alignment_priorities.md).

| ID | Item | Décision finale |
|----|------|-----------------|
| A4 | Chiffrement `memory.db` | **Reporter** — S-MEM-05 ; RFC [`memory_encryption_rfc.md`](memory_encryption_rfc.md) |
| B2 | BM25 explicite | **Reporter** — bench FTS5 avant crate BM25 |
| D4/D6 | GraphRAG communautés / hypergraphes | **Reporter R&D** — S-RAG-04 |
| E1 | Gouvernance CMA 2 niveaux | **Production** — constitution YAML (S-RAG-02) |
| E2/E3 | Héritage mémoire / fork identité | **Reporter** — export/import branché suffit |
| G2 | `MemoryPlugin` trait | **Production** — HTTP interne (S-PLG-01) |
| G1 | Interop LangGraph | **Production** — S-RAG-03 CI validate |
| pi-mono | Handoff modèle explicite | **Production** — `POST /api/session/handoff` schema v2 |
| pi-mono | `toolcall_delta` streaming | **Production** — S-EVT-01 |
| pi-mono | RPC stdio JSONL | **Reporter** |
| pi-mono | CSI / TUI différentiel | **Reporter** |

**Dernière mise à jour :** 2026-06-07 (registre v1.2.0 — voir [`ROADMAP_FINAL_REGISTRY.md`](./ROADMAP_FINAL_REGISTRY.md)).
