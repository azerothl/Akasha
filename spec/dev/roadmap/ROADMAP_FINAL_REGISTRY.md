# ROADMAP_FINAL_REGISTRY — registre unique de clôture

**Version:** 1.1.0  
**Date:** 2026-06-06  
**Clôture roadmap :** 2026-06-06  
**Remplace:** [`ROADMAP_CLOSURE_STATUS.md`](./ROADMAP_CLOSURE_STATUS.md) (archivé)

Source de vérité pour le statut terminal de chaque item roadmap et stub code.  
**Statuts terminaux :** `Production` · `Reporter` · `Degraded path` (intentionnel, documenté)

---

## Matrice centrale — domaines Partiel → cible

| ID | Domaine | Statut actuel | Phase | Cible | Preuve / owner |
|----|---------|---------------|-------|-------|----------------|
| M-01 | Sessions / reprise | **Existe · haute** | 1 | Existe · haute | `GET /api/session/resume-brief`, handoff `schema_version: 2`, UI Reprendre Tauri + Studio |
| M-02 | Toolsets UX | Partiel | 1 | Existe · haute | `/api/tools/effective`, presets Tauri |
| M-03 | Terminal / backends | Partiel | 1 | Existe · moyenne | PTY HTTP ; SSH **Reporter** |
| M-04 | Gateway messagerie | Partiel | 1 | Existe · moyenne | Dashboard health canaux Tauri |
| M-05 | Webhooks | Partiel | 1 | Existe · haute | Cockpit Studio idempotence |
| M-06 | Hooks gateway | Partiel | 1 | Existe · moyenne | Éditeur `lifecycle_hooks.json` Tauri |
| M-07 | MCP | Partiel | 1 | Existe · haute | OAuth refresh + outils agent MCP CRUD |
| M-08 | Skills browse | Existe · moyenne | 1 | Existe · haute | UI install `Akasha_skills` |
| M-09 | Perf / SLO | Partiel | 1 | Existe · moyenne | Dashboard recall Studio |
| M-10 | Cache HTTP | **Existe · haute** | 1 | Existe · haute | `http_get_cache.rs` généralisé |
| M-11 | Git worktree | Partiel | 1 | Existe · moyenne | Doc happy path `Akasha_app` |
| M-12 | Browser phase 2 | Partiel | 1 | Existe · moyenne | Panneau erreurs Playwright TUI |
| M-13 | Web crawl | Partiel | 1 | Existe · moyenne | Timeline Task Center |
| M-14 | Migration OpenClaw | **Existe · moyenne** | 1 | Existe · moyenne | Import mémoire semi-auto S-MIG-01 |
| M-15 | Contexte long | Existe · moyenne | 1 | Existe · haute | Bench compaction 64k+ |

---

## STUB_CODE_REGISTRY

Tous les stubs sont en statut terminal **Production**, **Reporter** ou **Degraded path** (aucun « Stub » ni « Veille » ouvert).

| ID | Fichier | Statut | Phase | Cible | Issue |
|----|---------|--------|-------|-------|-------|
| S-MEM-01 | `memory_actor.rs` LtRollup | Production | 2A | Promote + delete rollup | — |
| S-MEM-02 | `memory_hierarchical.rs` fallback | Production | 2A | Métrique `rollup_deferred` | — |
| S-MEM-03 | `memory_export.rs` import | Production | 2A | Re-embedding à l'import | — |
| S-MEM-04 | `memory_relation_semantic.rs` | Production | 2C | LLM edge labeling | — |
| S-MEM-05 | SQLCipher / doctor | Reporter | 2B | Spike `--encrypt-memory` reporté | — |
| S-MEM-06 | `akasha init` memory | Production | 4 | Pointer wizard + doctor | — |
| S-LLM-01 | `provider.rs` placeholder | Production | 1 | Erreur explicite sans embedded | — |
| S-TOOL-01 | `api.rs` image local path | Production | 2C | data URL vision | — |
| S-TOOL-02 | `device_discover` stub | Reporter | 5 | Deny explicite network/usb | — |
| S-TOOL-03 | `device_invoke` system | Reporter | 5 | Print OS hors scope | — |
| S-TOOL-04 | Session terminal spec 33 | Reporter | 5 | PTY HTTP suffit | — |
| S-TOOL-05 | `mcp.rs` MVP | Production | 1 | MCP CRUD agent tools | — |
| S-EVT-01 | `toolcall_delta` stub flag | Production | 6 | Partial JSON structuré | — |
| S-EVT-02 | Studio swarm synthétique | Production | 6 | Events worker natifs | — |
| S-EVT-03 | `task_progress_is_chat_stub` | Production | 6 | Progression substantive | — |
| S-EVT-04 | `plugin_hook_bus` | Production | 7 | 4+ lifecycle hooks | — |
| S-MIG-01 | `openclaw_migration.rs` | Production | 1 | Import mémoire apply | — |
| S-PLG-01 | MemoryPlugin HTTP | Production | 2C | Délégué plugin-host | — |
| S-PLG-02 | ModelPlugin | Reporter | 5 | HTTP router suffit | — |
| S-PLG-03 | SecurityPlugin | Reporter | 5 | Vault core suffit | — |
| S-PLG-04 | matrix-channel WASM | Production | 3 | Sidecar HTTP bridge | — |
| S-RAG-01 | `akasha-rag/index.rs` | Production | 2A | Récursif + chunking | — |
| S-RAG-02 | constitution sample | Production | 2C | `constitution.yaml` actif | — |
| S-RAG-03 | langgraph example | Production | 7 | CI validate example | — |
| S-RAG-04 | GraphRAG D4 | Reporter | 2C | Pilote project communities reporté R&D | — |
| S-ORCH-01 | orchestrator stub_body | Degraded path | 1 | Doc runbook remediation | — |
| S-ORCH-02 | placeholder_after_tools | Production | 6 | Prévention prompt | — |
| S-CACHE-01 | `http_get_cache.rs` | Production | 1 | Routes lifecycle/plugins | — |

---

## Hors scope — décisions Phase 5 (finalisées)

| Item | Décision | Alternative / justification |
|------|----------|----------------------------|
| Auth multi-user | **Reporter P4** | HTTP Basic LAN optionnel |
| WhatsApp / Signal | **Reporter** | Telegram/Matrix/Slack couvrent messagerie |
| RL trajectoires | **Reporter** | MVP export JSONL ; pas training core |
| PWA mobile | **Reporter** | Desktop-first (Tauri + Studio) |
| RPC stdio pi-mono | **Reporter** | HTTP API + tâches suffisent |
| Email IMAP | **Reporter** | Hors scope court terme |
| CalDAV | **Reporter** | MVP ICS import/export uniquement |
| @mentions multi-Kin | **Reporter** | `agent-profiles` suffit |
| GraphRAG hypergraphes D6 | **Reporter R&D** | Pilote `project:*` si demande |
| ModelPlugin trait | **Reporter** | `llm_router.yaml` + providers HTTP (S-PLG-02) |
| SecurityPlugin trait | **Reporter** | Vault core + `tools_policy.yaml` (S-PLG-03) |
| SSH terminal backend | **Reporter** | PTY HTTP local suffit (S-TOOL-04) |
| device_discover / device_invoke | **Reporter** | Deny explicite ; hors scope OS (S-TOOL-02/03) |
| SQLCipher memory.db | **Reporter** | RFC spike ; chiffrement disque OS (S-MEM-05) |
| Fork tree UI v2 (`/tree`) | **Reporter** | v1 + trace événementielle suffisent ; voir SESSION_FORK_SPEC |
| CSI / TUI différentiel pi-mono | **Reporter** | Faible priorité UX terminal |

---

## KinBot / Odysseus / pi-mono — jalons

| Matrice | Jalon | Condition | Statut |
|---------|-------|-----------|--------|
| KinBot | M3 | MATRIX Production ou Reporter ; zéro Stub | **Atteint** |
| Odysseus | M4 | Incognito/floutage Existe · haute ; wizard 6 étapes | **Atteint** |
| pi-mono | M5 | toolcall_delta Production ; fork tree v2 trace | **Atteint** |
| Écosystème | M7 | Hooks WASM complets ; matrice satellites ● | **Atteint** |

---

## Tag Git — instruction de release (ne pas exécuter automatiquement)

Après merge de la clôture roadmap sur `main` :

1. Vérifier l'alignement version workspace (`Cargo.toml` `[workspace.package].version`) et Tauri (`apps/akasha-ui`).
2. Synchroniser si besoin : `python3 scripts/sync-release-version.py X.Y.Z`.
3. Créer et pousser le tag (exemple roadmap-complete `0.8.x`) :

```bash
git tag vX.Y.Z -m "roadmap-complete: ROADMAP_FINAL_REGISTRY 1.1.0"
git push origin vX.Y.Z
```

4. Le workflow **Release** publie les artefacts ; `verify-release-version` valide la cohérence tag ↔ versions embarquées.

---

## Changelog

- **1.1.0** (2026-06-06): clôture roadmap — stubs tous Production/Reporter/Degraded path ; décisions Phase 5 finalisées ; handoff `schema_version: 2` ; satellites matrice ● ; instruction tag Git.
- **1.0.0** (2026-06-06): création registre unique ; fusion wave 7–10 + STUB_CODE_REGISTRY 30 IDs.
