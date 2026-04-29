# Hermes partial domains roadmap (Phase 4)

This file tracks domains that remain **Partiel** in the parity matrix and gives an operator-focused closure path.

## 1) Git worktree

- CLI baseline exists: `akasha worktree list|add|remove`.
- Add diagnostics path (`akasha worktree doctor`) so operators can validate branch, cleanliness, and worktree topology before action.
- Exit target for matrix: clear “happy path” + failure hints in CLI/docs.

## 2) Browser phase 2

- Keep Playwright runner as primary backend (`scripts/playwright-runner/run.mjs`).
- Harden with:
  - explicit timeout diagnostics,
  - actionable install guidance (`install_playwright`),
  - safer domain policy messaging.

## 3) Web crawl (Cloudflare)

- Stabilize by standardizing retries/timeouts and surfacing job-state diagnostics in operator surfaces.
- Keep `web_crawl` / `web_crawl_status` policy-first (deny by default unless configured).

## 4) Migration OpenClaw-like

- Move from documentation-only to a guided operator flow:
  - config validation,
  - mapping preview,
  - safe apply.
- Keep secrets migration vault-only.

## Validation checklist

- Core checks: `cargo check -p akasha-daemon --lib`, `cargo check -p akasha-cli`.
- Docs links present in parity matrix and site docs.
- At least one operator-visible satellite proof for each shipped sub-domain.
