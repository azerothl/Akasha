> **Archive:** Ce document est archivé. Source de vérité active : [`ROADMAP_FINAL_REGISTRY.md`](./ROADMAP_FINAL_REGISTRY.md).

# Roadmap closure status (wave 7–10)

**Date:** 2026-06-06  
Consolidated tracking after implementation pass aligned with the closure plan.

## Vague 7 — Livré

| Item | Statut | Preuves |
|------|--------|---------|
| Doc resync | Done | `hermes-partial-domains-roadmap.md`, `kinbot-akasha-parity-matrix.md` |
| jcode Phase E | Done | `Akasha_app/docs.html`, `jcode_inspired_integration_rfc.md` |
| OpenClaw CLI | Done | `akasha migrate openclaw preview\|apply` |
| OpenClaw UI | Done | `OpenClawMigrationPanel.tsx` |
| Handoff modèle | Done | `POST /api/session/handoff`, Tauri + Code Studio UI |
| F3 rollup | Done | `run_lt_rollup_with_llm` + hygiene scheduler |
| G3 process_id | Done | `memory_orchestrator.rs`, root task id in `api.rs` |
| D7 workspace fusion | Done | `RecallParams.workspace_graph_lines` fused in recall |
| H5 profile UI | Done | Advanced settings + env overrides |
| B5 user RAG UI | Done | Settings panel + index status |

## Vague 8 — Livré (MVP)

| Item | Statut | Preuves |
|------|--------|---------|
| Hermes WASM hooks | MVP | `plugin_hook_bus.rs`, `task_completed` dispatch |
| Cache GET étendu | Done | `http_get_cache.rs` lifecycle/plugins/process |
| Akasha_plugins CI | Done | `.github/workflows/trust-catalog.yml` |
| Matrix plugin | Stub+ | WASM builds; full sync deferred in README |
| SSE / steering | Done | Existing SSE + steering UI (poll reduced where SSE active) |
| Onboarding extend | Done | `OnboardingWizard.tsx` provider/services step |
| Incognito | Done | `incognito` / `no_memory` on `/api/message` |
| Floutage secrets | Done | `blurSecrets` in `ChatRenderer.tsx` |
| Session search | Done | `chatThreadSearch` in sidebar |
| Mémoire P2 MVP | Done | `GET/POST /api/agent-identity`, bi-temporel filter, upgrade reminder |

## Vague 9 — Livré (foundation)

| Item | Statut | Preuves |
|------|--------|---------|
| toolcall_delta | Stub event | `api.rs` SSE emission |
| Fork tree v2 | Spec | `SESSION_FORK_SPEC.md` v2 section |
| Steering polish | Partial | Alt+Enter follow-up in Tauri (see App.tsx) |
| A4 encryption | Foundation | `AKASHA_MEMORY_ENCRYPT` + doctor guidance; SQLCipher spike deferred |

## Vague 10 — Veille traitée

| Item | Statut | Preuves |
|------|--------|---------|
| B2 bench FTS5 | Done | `scripts/bench-fts5-recall.ps1` |
| E1 CMA | Sample | `spec/dev/integrations/constitution_governance_sample.yaml` |
| G1 LangGraph | Done | `langgraph_memory_example.py` |
| G2 MemoryPlugin | Stub | HTTP delegate notes in `akasha-plugin-api` |
| H3 Deep research | Done | Query logging in `deep_research/engine.rs` |
| D4 GraphRAG | Veille | Pilote `project:*` via workspace graph — no community GraphRAG yet |

## Hors scope (inchangé)

Auth multi-user, WhatsApp/Signal natifs, RL trajectoires, PWA mobile, RPC stdio pi-mono (optionnel).
