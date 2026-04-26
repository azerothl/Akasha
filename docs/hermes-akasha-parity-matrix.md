# Matrice de parité Hermes Agent ↔ Akasha

**Version:** 1.0.1  
**Date:** 2026-04-26  
**Références externes:** [Hermes Quickstart](https://hermes-agent.nousresearch.com/docs/getting-started/quickstart), [Hermes Tools](https://hermes-agent.nousresearch.com/docs/user-guide/features/tools/), [Hermes Features Overview](https://hermes-agent.nousresearch.com/docs/user-guide/features/overview), [README Hermes (GitHub)](https://github.com/NousResearch/hermes-agent/blob/main/README.md)

Légende: **Existe** = équivalent opérationnel dans Akasha · **Partiel** = incomplet ou UX différente · **Absent** = non livré · **Maturité** = faible / moyenne / élevée (adoption + fiabilité perçues).

| Domaine | Hermes (résumé) | Akasha | Statut | Maturité Akasha | Notes / implémentation |
|--------|------------------|--------|--------|-----------------|-------------------------|
| Install / setup | `curl … install.sh`, `hermes setup` | `akasha init`, `akasha doctor --fix`, `akasha services install` | Existe | Élevée | Voir `crates/akasha-cli`, `spec/onboarding.md` |
| Providers / modèles | `hermes model`, multi-provider | `llm_router.yaml`, `akasha config models/provider` | Existe | Élevée | Fallback, retries dans `crates/akasha-llm` |
| Contexte min 64K | Exigence doc Hermes | `AKASHA_MAX_CONTEXT_TOKENS`, compaction | Existe | Moyenne | Estimation tokens améliorée dans le code (voir tokenizer calibré) |
| CLI / TUI | `hermes`, `hermes --tui` | `akasha tui`, UI Tauri | Existe | Élevée | |
| Sessions / reprise | `--continue`, sessions | `session_id`, mémoire, `/api/session-state`, `/api/session/resume-brief` | Partiel | Moyenne | + `memory_recall_metrics`, `tools_policy_brief` |
| Toolsets UX | `hermes tools`, presets par plateforme | `tool_profiles`, `akasha toolset profiles|effective`, `/api/tools/effective` | Partiel | Moyenne | CLI opérateur + API |
| Outils machine | 40+ outils, registry | `AVAILABLE_TOOLS`, exécution `akasha-tools` | Existe | Élevée | `crates/akasha-daemon/src/api.rs` |
| Terminal backends | local, docker, ssh, modal, … | local, `run_in_container`, docker services | Partiel | Moyenne | Spec backends dans `docs/` |
| Sandbox | Docker, approbation | Policy deny-by-default, container `--network=none` | Existe | Élevée | `crates/akasha-tools/src/policy.rs` |
| Gateway messagerie | Telegram, Discord, … | Slack, Discord, Telegram, Teams | Existe | Moyenne | Hardening auth canaux |
| Webhooks externes | Adapter HMAC, routes, direct delivery | Plateforme + mode direct (impl progressive) | Partiel | Faible → moyenne | Voir `spec/` + daemon |
| Cron / automation | `cronjob`, livraison plateforme | Scheduler persistant, `task_run` | Existe | Élevée | Ops pause/resume/run-now |
| Hooks | gateway / plugin / shell | Hooks lifecycle (impl progressive) | Partiel | Faible → moyenne | Aligné README Hermes |
| MCP | Serveurs MCP, OAuth | `PluginKind::Mcp`, `validate_mcp_config_json`, `docs/mcp-mvp.md` | Partiel | Faible → moyenne | Transport runtime à finaliser |
| Skills | Hub, auto-amélioration | Install URL, `skills.lock.jsonl` (SHA256 + ref), `Akasha_skills` | Existe | Moyenne | Lockfile append côté daemon |
| Plugins WASM | Extensions | Host WASM, réputation, trust store | Existe | Élevée | `GET /api/plugins/metrics`, `akasha plugin metrics` |
| Mémoire | FTS5, profils, compaction | LT + épisodique + compaction | Existe | Élevée | `GET /api/memory/recall-metrics`, tokenizer calibré, spec `54_memory_hierarchical_compaction.md` |
| Doctor | `hermes doctor` | `akasha doctor`, `/api/doctor` | Existe | Moyenne | Auto-triage incidents |
| Perf / SLO | Dashboard local (Hermes récent) | Métriques routeur, `scripts/bench-e2e.ps1` | Partiel | Moyenne | Runbook `docs/runbooks/slo-akasha-internal.md` |
| Cache idempotent | — | Stratégie doc + flag env (impl. LRU planifiée) | Partiel | Faible | `docs/cache-strategy.md` |
| Git worktree | — | `git_*` outils + `akasha worktree list|add|remove` | Partiel | Faible | Wrapper git local |
| Browser phase 2 | click/fill/… | Playwright `click|fill|wait|screenshot` + daemon | Partiel | Moyenne | `scripts/playwright-runner/run.mjs` |
| Web crawl | — | Cloudflare API `web_crawl` / `web_crawl_status` | Partiel | Moyenne | `akasha-tools` + policy |
| Migration OpenClaw-like | `hermes claw migrate` | Doc import settings/skills | Partiel | Faible | `Akasha_app` + spec |
| RL / trajectoires | Atropos, batch | Non cœur produit | Absent | — | Hors scope court terme |

## Applications connexes

| Repo | Rôle parité Hermes |
|------|---------------------|
| `Akasha_app` | Hub doc public, comparaison, guides webhooks/migration |
| `Akasha_skills` | Versioning, evals CI, bundles |
| `Akasha_plugins` | Trust catalogue (hash, permissions) |
| `akasha-code-studio` | Cockpit jobs, logs, RAG, worktree UI |
| `Rbitnet` | Inférence locale OpenAI-compatible, métriques, doc router |

## Changelog matrice

- **1.0.1** (2026-04-26): browser phase 2, crawl Cloudflare MVP, métriques recall/plugins, CLI toolset/worktree/config validate, MCP validation + docs cache/SLO/compaction.
- **1.0.0** (2026-04-26): publication initiale alignée sur le plan d’intégration Hermes vs Akasha.
