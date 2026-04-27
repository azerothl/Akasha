# Matrice de parité Hermes Agent ↔ Akasha

**Version:** 1.0.6  
**Date:** 2026-04-27  
**Références externes:** [Hermes Quickstart](https://hermes-agent.nousresearch.com/docs/getting-started/quickstart), [Hermes Tools](https://hermes-agent.nousresearch.com/docs/user-guide/features/tools/), [Hermes Features Overview](https://hermes-agent.nousresearch.com/docs/user-guide/features/overview), [README Hermes (GitHub)](https://github.com/NousResearch/hermes-agent/blob/main/README.md)

Légende: **Existe** = équivalent opérationnel dans Akasha · **Partiel** = incomplet ou UX différente · **Absent** = non livré · **Maturité** = faible / moyenne / élevée (adoption + fiabilité perçues).

| Domaine | Hermes (résumé) | Akasha | Statut | Maturité Akasha | Notes / implémentation |
|--------|------------------|--------|--------|-----------------|-------------------------|
| Install / setup | `curl … install.sh`, `hermes setup` | `akasha init`, `akasha doctor --fix`, `akasha services install` | Existe | Élevée | Voir `crates/akasha-cli`, `spec/onboarding.md` |
| Providers / modèles | `hermes model`, multi-provider | `llm_router.yaml`, `akasha config models/provider` | Existe | Élevée | Fallback, retries dans `crates/akasha-llm` |
| Contexte min 64K | Exigence doc Hermes | `AKASHA_MAX_CONTEXT_TOKENS`, compaction | Existe | Moyenne | Estimation tokens améliorée dans le code (voir tokenizer calibré) |
| CLI / TUI | `hermes`, `hermes --tui` | `akasha tui`, UI Tauri | Existe | Élevée | |
| Sessions / reprise | `--continue`, sessions | `session_id`, mémoire, `/api/session-state`, `/api/session/resume-brief`, `terminal_session start|list|read|write|resize|stop` | Partiel | Moyenne | + `memory_recall_metrics`, `tools_policy_brief` |
| Toolsets UX | `hermes tools`, presets par plateforme | `tool_profiles`, `akasha toolset profiles|effective`, `/api/tools/effective` | Partiel | Moyenne | CLI opérateur + API |
| Outils machine | 40+ outils, registry | `AVAILABLE_TOOLS`, exécution `akasha-tools` | Existe | Élevée | `crates/akasha-daemon/src/api.rs` |
| Terminal backends | local, docker, ssh, modal, … | local, `run_in_container`, docker services, `GET /api/terminal/capabilities`, **HTTP PTY** `/api/terminal/pty/sessions` (`portable-pty`), `akasha terminal capabilities` | Partiel | Moyenne | `spec/43_session_terminal.md`, `docs/terminal-backends-roadmap.md` |
| Sandbox | Docker, approbation | Policy deny-by-default, container `--network=none` | Existe | Élevée | `crates/akasha-tools/src/policy.rs` |
| Gateway messagerie | Telegram, Discord, … | Slack, Discord, Telegram, Teams | Existe | Moyenne | Hardening auth canaux |
| Webhooks externes | Adapter HMAC, routes, direct delivery | `POST /api/automation/webhook`, `/direct`, idempotence SQLite + chemin partageable `AKASHA_WEBHOOK_IDEM_SQLITE`, rate-limit distribué SQLite (`AKASHA_WEBHOOK_RATE_SQLITE`) | Partiel | Moyenne | Fallback mémoire possible par env |
| Cron / automation | `cronjob`, livraison plateforme | Scheduler persistant, `task_run` | Existe | Élevée | Ops pause/resume/run-now |
| Hooks | gateway / plugin / shell | `lifecycle_hooks.json` (`on_schedule_fire`, **`on_http_request_pre` / `on_http_request_post`** sur le trafic API), mode sandbox `strict` (allowlist commandes) | Partiel | Moyenne | `docs/gateway-shell-hooks.md`, `spec/56_lifecycle_hooks.example.json` |
| MCP | Serveurs MCP, OAuth | validation + probe + **`GET /api/mcp/runtime`**, **`POST /api/mcp/runtime/stdio/start|stop`** (stdio long-lived optionnel), `GET /api/mcp/status`, `GET/POST /api/mcp/runtime/oauth` | Partiel | Moyenne | OAuth complet provider-side reste roadmap |
| Skills | Hub, auto-amélioration | Install URL, `skills.lock.jsonl` (SHA256 + ref), `Akasha_skills` | Existe | Moyenne | Lockfile append côté daemon |
| Plugins WASM | Extensions | Host WASM, réputation, trust store | Existe | Élevée | `GET /api/plugins/metrics`, `akasha plugin metrics` |
| Mémoire | FTS5, profils, compaction | LT + épisodique + compaction | Existe | Élevée | `GET /api/memory/recall-metrics`, tokenizer calibré, spec `54_memory_hierarchical_compaction.md` |
| Doctor | `hermes doctor` | `akasha doctor`, `/api/doctor` | Existe | Moyenne | Auto-triage incidents |
| Perf / SLO | Dashboard local (Hermes récent) | Métriques routeur, `scripts/bench-e2e.ps1` | Partiel | Moyenne | Runbook `docs/runbooks/slo-akasha-internal.md` |
| Cache idempotent | — | LRU **GET** `GET /api/router/models`, `GET /api/router/routes`, `GET /api/mcp/status` derrière `AKASHA_HTTP_CACHE_TTL_SECS` + plafond 256 / 256 Ko | Partiel | Moyenne | `docs/cache-strategy.md`, module `http_get_cache` |
| Git worktree | — | `git_*` outils + `akasha worktree list|add|remove` | Partiel | Faible | Wrapper git local |
| Browser phase 2 | click/fill/… | Playwright `click|fill|wait|screenshot` + daemon | Partiel | Moyenne | `scripts/playwright-runner/run.mjs` |
| Web crawl | — | Cloudflare API `web_crawl` / `web_crawl_status` | Partiel | Moyenne | `akasha-tools` + policy |
| Migration OpenClaw-like | `hermes claw migrate` | Doc import settings/skills | Partiel | Faible | `Akasha_app` + spec |
| RL / trajectoires | Atropos, batch | Non cœur produit | Absent | — | Hors scope court terme |

## Applications connexes

| Application | Rôle parité Hermes |
|-------------|---------------------|
| Monorepo **Akasha** (daemon, CLI, tools) | Exécution, politiques, APIs, matrice source |
| **`apps/akasha-ui`** (Tauri) | UX desktop : reprise session, état outils, liens doc opérateur |
| **`Akasha_app`** (site statique) | Hub public : comparaison, guides MCP/webhooks/toolsets/migration |
| **`Akasha_skills`** | Catalogue vivant : semver, changelog, compat daemon, evals CI |
| **`Akasha_plugins`** | Trust : WASM hash dans l’index, permissions visibles |
| **`akasha-code-studio`** | Cockpit dev : jobs, `task_runs`, process watch, terminal, tools effective |
| **`Rbitnet`** | Backend local : OpenAI-compatible, `/metrics`, doc `llm_router.yaml` |

Légende pour le tableau ci-dessous : **C** = implémentation dans le core · **Doc** = documentation / site · **Cat** = catalogue skills/plugins · **Studio** = Code Studio · **Rbit** = Rbitnet · **UI** = app Tauri.

## Propriétaire par domaine (Hermes → écosystème)

Synthèse : où la parité Hermes devient **visible** ou **opérable** hors du seul daemon.

| Thème Hermes | C | Doc | Cat | Studio | Rbit | UI |
|--------------|---|-----|-----|--------|------|-----|
| Setup / doctor / services | ● | ● | — | — | ● | — |
| Providers / routing / fallback | ● | ● | — | — | ● | — |
| Sessions / reprise / mémoire | ● | ● | — | ○ | — | ● |
| Toolsets / tools policy | ● | ● | — | ● | — | ● |
| Terminal / PTY / background | ● | ● | — | ● | — | ● |
| Webhooks / automation externe | ● | ● | — | ● | — | — |
| MCP (validation, probe, statut HTTP, OAuth doc) | ● | ● | ● | ● | — | ● |
| Skills / lockfile | ● | ● | ● | — | — | — |
| Plugins WASM / métriques | ● | ● | ● | ○ | — | ● |
| Perf / SLO / bench | ● | ● | — | ○ | ○ | — |

**○** = surface partielle ou roadmap UI ; mettre à jour ce tableau quand une PR satellite ferme une case.

## Changelog matrice

- **1.0.6** (2026-04-27): `terminal_session` branché sur API PTY (start/list/read/write/resize/stop) + sessions listables ; MCP OAuth state routes (`GET/POST /api/mcp/runtime/oauth`) ; sandbox hooks mode `strict` (allowlist commandes) ; rate-limit distribué webhooks via SQLite (`AKASHA_WEBHOOK_RATE_SQLITE`) ; cache LRU étendu (`/api/router/routes`, `/api/mcp/status`) ; cockpit opérateur enrichi (health résumé) sur Studio/TUI/Tauri ; CI skills auto-découverte des `self_check.sh`.
- **1.0.5** (2026-04-27): **PTY HTTP** (`/api/terminal/pty/sessions*`, `portable-pty`) ; **MCP runtime** `GET /api/mcp/runtime`, stdio long-lived `POST /api/mcp/runtime/stdio/start|stop` ; **gateway hooks** `on_http_request_pre` / `on_http_request_post` (timeouts `AKASHA_GATEWAY_HOOK_TIMEOUT_SECS`) ; **cache LRU** GET `/api/router/models` (`AKASHA_HTTP_CACHE_TTL_SECS`) ; webhooks `AKASHA_WEBHOOK_IDEM_SQLITE` ; CLI `akasha terminal capabilities` ; cockpit **Code Studio** + **TUI** + **Tauri** (MCP runtime + terminal) ; **Akasha_app** docs Hermes (MCP + terminal) ; **Akasha_skills** pilotes `scripts/self_check.sh` + CI ; **Akasha_plugins** trust catalogue (hook events WASM) ; **Rbitnet** lien bench ↔ matrice.
- **1.0.4** (2026-04-26): webhooks idempotence persistante (`webhook_idempotency.sqlite3`) ; `GET /api/mcp/status` ; `GET /api/lifecycle/hooks` ; cockpit Code Studio (recall, MCP, lifecycle, actions scheduler) ; TUI onglet Routeur (bloc Hermes) ; Tauri réglages expert (recall, MCP, lifecycle) ; site `Akasha_app` (digest WASM plugins, min daemon skills) ; CI catalogue `Akasha_plugins` ; validation `akasha_daemon_min_version` dans `Akasha_skills` CI.
- **1.0.3** (2026-04-26): colonne écosystème (applications + tableau propriétaire Hermes → satellites) ; inclusion explicite de `apps/akasha-ui`.
- **1.0.2** (2026-04-26): webhooks signés + direct delivery, watch processus (`GET /api/process/watch/recent`), hooks schedule, MCP stdio probe (daemon + CLI), `akasha services logs|restart|doctor`, docs MCP OAuth / terminal backends / hooks.
- **1.0.1** (2026-04-26): browser phase 2, crawl Cloudflare MVP, métriques recall/plugins, CLI toolset/worktree/config validate, MCP validation + docs cache/SLO/compaction.
- **1.0.0** (2026-04-26): publication initiale alignée sur le plan d’intégration Hermes vs Akasha.
