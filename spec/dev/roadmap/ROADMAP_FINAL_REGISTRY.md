# ROADMAP_FINAL_REGISTRY — registre unique de clôture

**Version:** 1.2.0  
**Date:** 2026-06-07  
**Clôture roadmap :** 2026-06-06 (M-* atteints — resync matrice parité v3.0.0)  
**Remplace:** [`ROADMAP_CLOSURE_STATUS.md`](./ROADMAP_CLOSURE_STATUS.md) (archivé)

Source de vérité pour le statut terminal de chaque item roadmap et stub code.  
**Statuts terminaux :** `Production` · `Reporter` · `Degraded path` (intentionnel, documenté)

Aligné sur [`reference-products-parity-matrix.md`](./reference-products-parity-matrix.md) v3.0.0 (changelog 2.1.1).

---

## Matrice centrale — domaines M-* (post-clôture)

Colonne **Cible** : objectif Phase 1 ; **Atteinte** = livré au 2026-06-06 (preuve ci-dessous).

| ID | Domaine | Statut actuel | Phase | Cible | Preuve / owner |
|----|---------|---------------|-------|-------|----------------|
| M-01 | Sessions / reprise | **Existe · haute** | — | Existe · haute | `GET /api/session/resume-brief`, handoff `schema_version: 2`, UI Reprendre Tauri + Studio |
| M-02 | Toolsets UX | **Existe · haute** | — | Existe · haute | `/api/tools/effective`, presets Tauri |
| M-03 | Terminal / backends | **Existe · moyenne** | — | Existe · moyenne | PTY HTTP ; SSH **Reporter** (S-TOOL-04) |
| M-04 | Gateway messagerie | **Existe · moyenne** | — | Existe · moyenne | Dashboard health canaux Tauri |
| M-05 | Webhooks | **Existe · haute** | — | Existe · haute | Cockpit Studio idempotence + `GET /api/automation/webhook/recent` |
| M-06 | Hooks gateway | **Existe · moyenne** | — | Existe · moyenne | `lifecycle_hooks.json` + `GET /api/lifecycle/hooks` ; résumé Tauri (`SystemHealthPanel`) — éditeur GUI **Reporter** |
| M-07 | MCP | **Existe · haute** | — | Existe · haute | OAuth refresh ; outils agent `mcp_server_add` / `mcp_server_remove` |
| M-08 | Skills browse | **Existe · haute** | — | Existe · haute | UI install catalogue `Akasha_skills` (Tauri Settings) |
| M-09 | Perf / SLO | **Existe · moyenne** | — | Existe · moyenne | Dashboard recall Studio + runbook interne |
| M-10 | Cache HTTP | **Existe · haute** | — | Existe · haute | `http_get_cache.rs` généralisé (S-CACHE-01) |
| M-11 | Git worktree | **Existe · moyenne** | — | Existe · moyenne | `akasha worktree doctor` ; doc happy path `Akasha_app` |
| M-12 | Browser phase 2 | **Existe · moyenne** | — | Existe · moyenne | Playwright runner + diagnostics timeout ; panneau erreurs domain policy TUI **Reporter** |
| M-13 | Web crawl | **Existe · moyenne** | — | Existe · moyenne | Retries `AKASHA_WEB_CRAWL_RETRIES` ; timeline Task Center |
| M-14 | Migration OpenClaw | **Existe · moyenne** | — | Existe · moyenne | Import mémoire semi-auto S-MIG-01 ; CLI + `OpenClawMigrationPanel` |
| M-15 | Contexte long | **Existe · haute** | — | Existe · haute | `AKASHA_MAX_CONTEXT_TOKENS`, bench compaction 64k+ |

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
| S-TOOL-02 | `device_discover` (network/usb) | Reporter | 5 | Refus explicite ; `local_media`/`system`/`synthetic_input` livrés | — |
| S-TOOL-03 | `device_invoke` (print OS) | Reporter | 5 | Impression OS hors scope ; autres interfaces selon policy | — |
| S-TOOL-04 | Session terminal spec 33 | Reporter | 5 | PTY HTTP suffit | — |
| S-TOOL-05 | `mcp.rs` MVP | Production | 1 | MCP CRUD agent tools | — |
| S-EVT-01 | `toolcall_delta` stub flag | Production | 6 | Partial JSON structuré | — |
| S-EVT-02 | Studio swarm synthétique | Production | 6 | Events worker natifs | — |
| S-EVT-03 | `task_progress_is_chat_stub` | Production | 6 | Progression substantive | — |
| S-EVT-04 | `plugin_hook_bus` | Production | 7 | 4+ events WASM (`task_*`, `on_schedule_fire`, `on_channel_message`) | — |
| S-MIG-01 | `openclaw_migration.rs` | Production | 1 | Import mémoire apply | — |
| S-PLG-01 | MemoryPlugin HTTP | Production | 2C | Délégué plugin-host (`MemoryDelegateRequest`) | — |
| S-PLG-02 | ModelPlugin | Reporter | 5 | HTTP router suffit | — |
| S-PLG-03 | SecurityPlugin | Reporter | 5 | Vault core suffit | — |
| S-PLG-04 | matrix-channel WASM | Production | 3 | Sidecar HTTP bridge (`Akasha_plugins/matrix-channel`) | — |
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
| device_discover network/usb | **Reporter** | Interfaces refusées ; `local_media` livré (S-TOOL-02) |
| device_invoke print OS | **Reporter** | Impression système hors scope (S-TOOL-03) |
| SQLCipher memory.db | **Reporter** | RFC spike ; chiffrement disque OS (S-MEM-05) |
| Fork tree UI v2 (`/tree`) | **Reporter** | v1 + trace événementielle suffisent ; voir SESSION_FORK_SPEC |
| CSI / TUI différentiel pi-mono | **Reporter** | Faible priorité UX terminal |
| Éditeur GUI lifecycle hooks | **Reporter** | Fichier JSON + `GET /api/lifecycle/hooks` + cockpit Studio |
| Panneau erreurs Playwright TUI | **Reporter** | Diagnostics runner + hint santé Tauri suffisent |
| Couverture WASM hooks catalogue complète | **Reporter** | Bus livré ; extension événement par événement si demande |

---

## KinBot / Odysseus / pi-mono — jalons

| Matrice | Jalon | Condition | Statut |
|---------|-------|-----------|--------|
| KinBot | M3 | MATRIX **Production** (S-PLG-04 sidecar) ; zéro Stub ouvert | **Atteint** |
| Odysseus | M4 | Incognito/floutage Existe · haute ; wizard 6 étapes | **Atteint** |
| pi-mono | M5 | toolcall_delta Production ; fork tree v2 trace | **Atteint** |
| Écosystème | M7 | Hooks WASM complets ; matrice satellites ● | **Atteint** |

Parité détaillée KinBot (ex. sync Matrix E2E ops) : [`kinbot-akasha-parity-matrix.md`](./kinbot-akasha-parity-matrix.md) — peut rester **Partiel** sans rouvrir le registre terminal.

---

## Évolutions post-clôture (suivi produit, hors M-*)

Items livrés après la clôture du 2026-06-06 — à refléter dans les matrices inspiration, pas dans STUB_CODE_REGISTRY.

| Date | Item | Statut | Preuve |
|------|------|--------|--------|
| 2026-06-07 | Notes Odysseus (Tauri) | Existe · moyenne | `notes.rs`, `/api/notes`, `NotesPanel.tsx` — voir [`odysseus-inspiration-matrix.md`](./odysseus-inspiration-matrix.md) |

---

## Tag Git — instruction de release (ne pas exécuter automatiquement)

**État au 2026-06-07 :** tag `v0.8.0` présent ; versions workspace et Tauri alignées sur `0.8.0`.

Pour une future release documentaire ou produit :

1. Vérifier l'alignement version workspace (`Cargo.toml` `[workspace.package].version`) et Tauri (`apps/akasha-ui`).
2. Synchroniser si besoin : `python3 scripts/sync-release-version.py X.Y.Z`.
3. Créer et pousser le tag :

```bash
git tag vX.Y.Z -m "roadmap-complete: ROADMAP_FINAL_REGISTRY 1.2.0"
git push origin vX.Y.Z
```

4. Le workflow **Release** publie les artefacts ; `verify-release-version` valide la cohérence tag ↔ versions embarquées.

---

## Changelog

- **1.2.0** (2026-06-07): resync M-* avec matrice parité v3.0.0 ; preuves corrigées (M-06 lecture lifecycle, M-12 TUI Reporter) ; S-TOOL-02/03 et hors scope device clarifiés ; KinBot M3 ↔ S-PLG-04 ; section post-clôture (notes Odysseus) ; tag `v0.8.0` noté ; archives resynchronisées.
- **1.1.0** (2026-06-06): clôture roadmap — stubs tous Production/Reporter/Degraded path ; décisions Phase 5 finalisées ; handoff `schema_version: 2` ; satellites matrice ● ; instruction tag Git.
- **1.0.0** (2026-06-06): création registre unique ; fusion wave 7–10 + STUB_CODE_REGISTRY 30 IDs.
