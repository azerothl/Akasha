> **Archive:** Ce document est archivé. Source de vérité active : [`ROADMAP_FINAL_REGISTRY.md`](./ROADMAP_FINAL_REGISTRY.md).

# Roadmap — inspirations KinBot

Programme 12 mois (5 phases). Statut mis à jour à l'implémentation initiale (2026-06) ; **wave 5** (2026-06-06) : matrice parité + doc site.

| ID | Feature | Phase | Statut |
|----|---------|-------|--------|
| B5 | User RAG async + hybrid + outil | 1 | Partiel — API upload + index async ; retrieval hybride ; pas UI knowledge base dédiée |
| MEM | Multi-query / HyDE / consolidation / adaptive K / temporal decay | 1 | Partiel — voir notes ci-dessous |
| ARCH | Conversation archive + search | 1 | Implémenté |
| WAKE | Wakeups conversationnels | 2 | Implémenté |
| HITL | HITL multi-canal (Telegram) | 2 | Partiel — Telegram + API human-input ; pas WhatsApp/Signal |
| PROF | Profils agent YAML + API | 2 | Implémenté |
| DEL | delegation_reason + AgentDelegated | 2 | Implémenté |
| INTER | Contrat inter-agent | 3 | Implémenté (module) |
| CONT | Contacts CRUD | 3 | Implémenté |
| NOTIF | Notifications persistées | 3 | Implémenté |
| PLUG | Marketplace plugins install API | 4 | Partiel — `POST /api/plugins/install` + CLI catalog ; pas registry npm KinBot |
| DASH | Dashboards agent sandbox | 4 | Implémenté |
| SSE | SSE filtrage types | 4 | Partiel — `GET /api/events` + filtre `types`/`task_id` ; fallback poll sur certains écrans UI |
| MATRIX | Canal Matrix plugin | 5 | Stub catalogue |
| ONBOARD | Wizard Tauri premier lancement | 5 | Partiel — wizard Tauri 4 étapes + `akasha init` CLI ; pas wizard Docker KinBot |
| DOC | Matrice parité KinBot | 5 | Implémenté — [kinbot-akasha-parity-matrix.md](./kinbot-akasha-parity-matrix.md) |

### Notes MEM / B5

- **Multi-query / HyDE** : `memory_retrieval_enhance.rs`, env-gated (`AKASHA_MEMORY_MULTI_QUERY`, HyDE optionnel).
- **Consolidation** : `memory_consolidation.rs` (fusion doublons proches).
- **Adaptive K** : `memory_fusion::adaptive_k_cutoff` (`AKASHA_MEMORY_ADAPTIVE_K`).
- **Temporal decay** : `expires_at` sur entrées LT + hygiene purge ; pas graphe temporel Zep-complet.
- **B5 user RAG** : index sidecar async post-upload ; recherche hybride à la requête ; gap = UX catalogue documents.

Voir aussi [memory_landscape_roadmap_matrix.md](memory_landscape_roadmap_matrix.md) et [kinbot-akasha-parity-matrix.md](./kinbot-akasha-parity-matrix.md).
