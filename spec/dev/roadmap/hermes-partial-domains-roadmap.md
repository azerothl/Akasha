> **Archive:** Ce document est archivé. Statut : **Livré / Archivé** (2026-06-06). Source de vérité active : [`ROADMAP_FINAL_REGISTRY.md`](./ROADMAP_FINAL_REGISTRY.md).

# Partial domains roadmap — operator parity (Phase 4)

**Statut : Livré / Archivé** — tous les sous-domaines ci-dessous ont une preuve opérateur ou une décision Reporter documentée dans le registre final.

This file tracks domains that remain **Partiel** in [`reference-products-parity-matrix.md`](./reference-products-parity-matrix.md) and gives an operator-focused closure path.

## 1) Git worktree — **Livré**

- CLI: `akasha worktree list|add|remove|doctor` (diagnostics: branch, cleanliness, worktree count).
- Doc happy path : `Akasha_app` + guide opérateur.

## 2) Browser phase 2 — **Livré**

- Playwright runner: explicit timeout diagnostics + `install_playwright` guidance (`scripts/playwright-runner/run.mjs`).
- Panneau erreurs domain policy TUI (optionnel) : **Reporter**.

## 3) Web crawl (Cloudflare) — **Livré**

- Retries via `AKASHA_WEB_CRAWL_RETRIES`; job-state in tool status output.
- Policy-first unchanged.

## 4) Migration OpenClaw-like — **Livré**

- API: `POST /api/migrate/openclaw/preview|apply`
- CLI: `akasha migrate openclaw preview|apply --source-dir …`
- UI: `OpenClawMigrationPanel` (Tauri Settings → System)
- Docs: [`openclaw-migration.md`](../integrations/openclaw-migration.md), `Akasha_app/docs.html`

## Validation checklist

- Core checks: `cargo check -p akasha-daemon --lib`, `cargo check -p akasha-cli`.
- Docs links present in parity matrix and site docs.
- At least one operator-visible satellite proof for each shipped sub-domain.
