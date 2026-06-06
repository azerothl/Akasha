# Partial domains roadmap — operator parity (Phase 4)

This file tracks domains that remain **Partiel** in [`reference-products-parity-matrix.md`](./reference-products-parity-matrix.md) and gives an operator-focused closure path.

**Wave 7 status:** see [`ROADMAP_CLOSURE_STATUS.md`](./ROADMAP_CLOSURE_STATUS.md).

## 1) Git worktree — **Doc closure remaining**

- CLI: `akasha worktree list|add|remove|doctor` (diagnostics: branch, cleanliness, worktree count).
- **Remaining:** operator doc happy path + failure hints in user guide / `Akasha_app`.

## 2) Browser phase 2 — **Hardened (wave 7)**

- Playwright runner: explicit timeout diagnostics + `install_playwright` guidance (`scripts/playwright-runner/run.mjs`).
- **Remaining:** operator surface for domain policy errors in TUI health panel (optional).

## 3) Web crawl (Cloudflare) — **Hardened (wave 7)**

- Retries via `AKASHA_WEB_CRAWL_RETRIES`; job-state in tool status output.
- Policy-first unchanged.

## 4) Migration OpenClaw-like — **MVP livré (wave 7)**

- API: `POST /api/migrate/openclaw/preview|apply`
- CLI: `akasha migrate openclaw preview|apply --source-dir …`
- UI: `OpenClawMigrationPanel` (Tauri Settings → System)
- Docs: [`openclaw-migration.md`](../integrations/openclaw-migration.md), `Akasha_app/docs.html`

## Validation checklist

- Core checks: `cargo check -p akasha-daemon --lib`, `cargo check -p akasha-cli`.
- Docs links present in parity matrix and site docs.
- At least one operator-visible satellite proof for each shipped sub-domain.
