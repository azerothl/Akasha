# Matrice de parité KinBot ↔ Akasha

**Date :** 2026-06-06  
**Source KinBot :** [MarlBurroW/kinbot](https://github.com/MarlBurroW/kinbot) · [roadmap interne](./kinbot_inspired_roadmap.md)

Une ligne = une capacité **KinBot** (README / doc amont) ; la colonne **Akasha** indique l’état dans le daemon, l’UI ou les plugins. Ce document clôt l’item **DOC** (phase 5) de la roadmap inspirations KinBot.

## Légende statut

| Statut | Signification |
|--------|----------------|
| **Implémenté** | Parité fonctionnelle ou équivalent Akasha livré |
| **Partiel** | Sous-ensemble, UX différente, ou dépend d’un flag / plugin |
| **Stub** | Contrat ou catalogue ; pas de runtime complet |
| **À faire** | Hors scope livré ; ticket ou phase future |
| **Hors scope** | Choix produit Akasha (non prévu court terme) |

---

## Matrice détaillée

| ID | Feature KinBot | Akasha | Notes / preuves |
|----|----------------|--------|-----------------|
| B5 | User RAG — upload docs, index hybride, recherche à la requête | **Partiel** | API + index async ; UI Réglages Tauri (`App.tsx` user RAG list/upload) ; gap = panel « knowledge base » dédié (recherche test, statut index enrichi) — vague 7 |
| MEM | Mémoire LT — extraction auto, recherche hybride, intent, consolidation | **Partiel** | 4 couches + RRF (`memory_fusion`) ; multi-query / HyDE (`memory_retrieval_enhance`, env-gated) ; consolidation (`memory_consolidation.rs`) ; adaptive K ; decay via `expires_at` + hygiene — voir notes MEM/B5 roadmap |
| ARCH | Archive conversation + recherche full-text | **Implémenté** | `ConversationArchiveStore`, `GET /api/sessions/:id/archive/search` |
| WAKE | Wakeups conversationnels (`wake_me_in`, cron-like) | **Implémenté** | `GET/POST/DELETE /api/wakeups`, scheduler |
| HITL | Human-in-the-loop multi-canal | **Partiel** | Telegram + `task_waiting_user_input`, `GET/POST /api/tasks/:id/human-*` ; pas WhatsApp/Signal |
| PROF | Profils agent (identité YAML, rôles) | **Implémenté** | `GET/POST/DELETE /api/agent-profiles`, `agent_profile.json` |
| DEL | Délégation sub-agents avec raison | **Implémenté** | `delegation_reason` sur `sub_agent_spawned` / `AgentDelegated` — `spec/api_integration.md` |
| INTER | Communication inter-agents (request/reply, rate limit) | **Implémenté** | Module `inter_agent.rs` (depth + rate limit) |
| CONT | Contacts CRUD + recherche | **Implémenté** | `GET/POST/PUT/DELETE /api/contacts` |
| NOTIF | Notifications persistées | **Implémenté** | `GET /api/notifications`, mark read / read-all |
| PLUG | Marketplace plugins (npm, Git URL, hot reload UI) | **Partiel** | `POST /api/plugins/install` (catalog id ou URL), `akasha plugin catalog|install` ; pas registry npm ni circuit-breaker plugins |
| DASH | Mini Apps / dashboards agent (HTML sandbox) | **Implémenté** | `GET/POST/DELETE /api/dashboards`, `GET …/html` — sandbox HTML agent |
| SSE | Streaming SSE multiplexé + filtrage types | **Partiel** | `GET /api/events` + `parse_sse_event_filter` (task_id, types) ; UI poll fallback sur certains écrans |
| MATRIX | Canal Matrix | **Stub** | Plugin catalogue `Akasha_plugins/matrix-channel` — WASM à compléter |
| ONBOARD | Wizard premier lancement (Docker / Tauri) | **Partiel** | Wizard Tauri 4 étapes (`apps/akasha-ui/src/components/OnboardingWizard.tsx` : bienvenue, doctor --fix, profil, fin) + `akasha init` CLI ; pas de wizard Docker intégré type KinBot |
| — | Mémoire chiffrée AES-256 (vault + DB) | **Partiel** | Vault chiffré ; `memory.db` en clair — [memory_encryption_rfc.md](./memory_encryption_rfc.md) (wave 6) |
| — | Auth multi-utilisateur + rôles + invitations | **Hors scope** | Instance mono-opérateur locale ; gouvernance Telegram v0.8+ |
| — | 6 canaux (WhatsApp, Signal, …) | **Partiel** | Slack, Discord, Telegram, Teams ; pas WhatsApp/Signal natifs |
| — | Mini Apps SDK (sidebar, gallery, storage KV) | **Partiel** | Dashboards HTML ; pas SDK mini-apps complet |
| — | Continuous session (une session par Kin, jamais reset) | **Partiel** | `session_id` persistant côté client ; compaction + nouvelle session possible |
| — | Sub-Kins await/async + file parent | **Partiel** | Orchestrateur + délégation ; modèle file parent KinBot non reproduit à l’identique |
| — | MCP gestion par l’agent | **Partiel** | Probe, runtime stdio, OAuth partiel — pas CRUD MCP par l’agent |
| — | Cron créé par l’agent (avec approbation) | **Partiel** | Scheduler + outils agent schedule_* ; approbation via queue permissions |
| — | Webhooks inbound par agent | **Implémenté** | `POST /api/automation/webhook`, HMAC, idempotence |
| — | @mentions + autocomplete multi-Kin | **Hors scope** | Mono-agent principal ; profils multiples via agent-profiles |
| — | 120+ outils natifs | **Partiel** | `AVAILABLE_TOOLS` + skills + plugins WASM ; surface différente |
| — | i18n EN/FR UI | **Partiel** | UI Tauri FR/EN ; doc site EN |
| — | File review permissions (queue) | **Implémenté** | Wave 5 — `GET/POST /api/permissions/queue/*` |
| — | Rapport opérateur post-tâche | **Implémenté** | Wave 5 — `GET /api/tasks/:id/report` |
| — | Code Studio swarm MVP | **Implémenté** | Wave 5 — coordinateur + workers bornés, événements synthétiques cockpit |

---

## Synthèse par phase (roadmap KinBot)

| Phase | Thème | Implémenté | Partiel | Stub / À faire |
|-------|-------|------------|---------|----------------|
| 1 | Mémoire + RAG + archive | ARCH | B5, MEM | — |
| 2 | Wakeups, HITL, profils, délégation | WAKE, PROF, DEL | HITL | — |
| 3 | Inter-agent, contacts, notifications | INTER, CONT, NOTIF | — | — |
| 4 | Plugins, dashboards, SSE | DASH | PLUG, SSE | — |
| 5 | Matrix, onboarding, doc parité | DOC (ce doc) | ONBOARD | MATRIX |

---

## Références

- [kinbot_inspired_roadmap.md](./kinbot_inspired_roadmap.md)
- [memory_landscape_roadmap_matrix.md](./memory_landscape_roadmap_matrix.md)
- [reference-products-parity-matrix.md](./reference-products-parity-matrix.md) (colonne KinBot)
- Code : `crates/akasha-daemon/src/api_routes_kinbot.rs`, `inter_agent.rs`, `memory_orchestrator.rs`

**Dernière mise à jour :** 2026-06-06 (resync ONBOARD ↔ wizard Tauri ; aligné avec [kinbot_inspired_roadmap.md](./kinbot_inspired_roadmap.md)).
