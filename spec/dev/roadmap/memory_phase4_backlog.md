> **Archive:** Ce document est archivé. Source de vérité active : [`ROADMAP_FINAL_REGISTRY.md`](./ROADMAP_FINAL_REGISTRY.md) v1.2.0.

# Phase 4 — Backlog mémoire (veille)

**Resync 2026-06-07 :** statuts normalisés selon le registre terminal (plus de « Veille » / « Stub » ouverts).

| ID | Item | Statut terminal | Registre |
|----|------|-----------------|----------|
| E1 | Gouvernance CMA simplifiée (constitution YAML + DB) | **Production** | S-RAG-02 — [`constitution_governance_sample.yaml`](../integrations/constitution_governance_sample.yaml) |
| D4/D6 | GraphRAG communautés / hypergraphes | **Reporter R&D** | S-RAG-04 ; workspace graph fusion livré (D7, wave 7) |
| G2 | `MemoryPlugin` branché sur HTTP interne | **Production** | S-PLG-01 — délégué HTTP `MemoryDelegateRequest` |
| B2 | BM25 explicite | **Reporter** | Bench FTS5 — [`scripts/bench-fts5-recall.ps1`](../../../scripts/bench-fts5-recall.ps1) |
| A4 | Chiffrement SQLCipher `memory.db` | **Reporter** | S-MEM-05 — `AKASHA_MEMORY_ENCRYPT` + doctor ; spike SQLCipher reporté |

Phases 1–3 + items wave 7 (F3, G3, H5, B5 UI) : journal historique [`ROADMAP_CLOSURE_STATUS.md`](./ROADMAP_CLOSURE_STATUS.md).
