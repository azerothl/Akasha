> **Archive:** Ce document est archivé. Source de vérité : [`ROADMAP_FINAL_REGISTRY.md`](./ROADMAP_FINAL_REGISTRY.md) v1.2.0. La matrice reste la référence produit ; les mises à jour roadmap passent par le registre.

# Matrice de parité — produits de référence ↔ Akasha

**Version:** 3.0.1  
**Date:** 2026-06-07  

Document **central** : une ligne = un **domaine fonctionnel** ; les colonnes *Hermes … Mercury* résument ce que chaque produit **expose typiquement** sur ce point (**indicatif** — vérifier chez l’éditeur). La colonne **jcode (RFC)** renvoie aux [concepts internes](./jcode_inspired_integration_rfc.md) (pas un produit concurrent). **État Akasha** reprend l’ancienne paire statut / maturité (synthèse).

**Ancien emplacement :** le fichier `hermes-akasha-parity-matrix.md` est désormais un **alias de redirection** ; toute évolution se fait ici.

---

## Liens documentation / dépôts (références)

| Produit | Entrée utile |
|---------|----------------|
| **KinBot** | [kinbot](https://github.com/MarlBurroW/kinbot) · [roadmap](./kinbot_inspired_roadmap.md) |
| **Hermes Agent** (Nous Research) | [Quickstart](https://hermes-agent.nousresearch.com/docs/getting-started/quickstart) · [Tools](https://hermes-agent.nousresearch.com/docs/user-guide/features/tools/) · [Features](https://hermes-agent.nousresearch.com/docs/user-guide/features/overview) · [GitHub](https://github.com/NousResearch/hermes-agent) |
| **OpenClaw** | [openclaws.io](https://openclaws.io) |
| **Odysseus** | [pewdiepie-archdaemon/odysseus](https://github.com/pewdiepie-archdaemon/odysseus) · [matrice inspiration](./odysseus-inspiration-matrix.md) |
| **Claude Code** | [Anthropic — Claude Code](https://docs.anthropic.com/en/docs/claude-code) |
| **Cursor** | [cursor.com](https://cursor.com) |
| **Mercury Agent** | [cosmicstack-labs/mercury-agent](https://github.com/cosmicstack-labs/mercury-agent) |
| **RFC jcode (Akasha)** | [jcode_inspired_integration_rfc.md](./jcode_inspired_integration_rfc.md) |

---

## Légende

- **État Akasha** : `Existe · haute` = livré et maturité perçue élevée · `Partiel · moyenne` = incomplet ou UX différente · `Absent` = hors scope court terme.
- **n/a** dans une colonne référence = le domaine n’est en général **pas** le positionnement principal du produit (pas une absence commerciale).
- **indic.** = dépend fortement de la version / du déploiement — relire la doc produit.

---

## 1. Synthèse exécutive (une lecture)

| Axe | Akasha | Hermes | OpenClaw | Claude Code | Cursor | Mercury |
|-----|--------|--------|----------|-------------|--------|---------|
| **Positionnement** | Daemon Rust 24/7, TUI, Tauri, API HTTP | Opérateur MCP, toolsets, webhooks | Assistant multi-canaux CLI | Agent code dans flux IDE/terminal Anthropic | IDE + agents intégrés | Agent CLI OSS (+ Telegram optionnel) |
| **LLM par défaut** | Modèle embarqué sans compte ; routeur multi-provider optionnel | Multi-provider | BYO / local (ex. Ollama) | Écosystème Claude | Modèles au choix + offre hébergée | BYO (README amont) |
| **Surface principale** | Binaires + web Code Studio | CLI + TUI produit | CLI + messagers | IDE / terminal | Desktop IDE | Terminal / chat |
| **MCP** | Validation, probe, runtime stdio, OAuth partiel | Très poussé | Selon intégration utilisateur | Fort dans l’écosystème | Fort (IDE) | Selon besoin perso |
| **Webhooks / HTTP automation** | HMAC, idempotence, rate limit, direct body | Oui (doc produit) | Variable | n/a focal | n/a focal | n/a focal |
| **Canaux conversationnels** | Slack, Discord, Telegram, Teams | Plusieurs gateways | Très large | n/a | n/a | Souvent Telegram (optionnel) |
| **Politique d’outils** | `tools_policy.yaml`, `tool_profiles` | Toolsets / presets | Configurable | Workflow éditeur | Rules + MCP | Permissions / budgets (README) |
| **Code source moteur** | Binaire propriétaire | Selon offre Hermes | OSS (typ.) | Produit fermé | Produit fermé | OSS |

---

## 2. Socle produit & interface

| Domaine | Akasha | Hermes | OpenClaw | Claude Code | Cursor | Mercury | jcode (RFC) | État Akasha |
|---------|--------|--------|----------|-------------|--------|---------|-------------|-------------|
| Install / setup | `akasha init`, `akasha doctor --fix`, wizard Tauri (`OnboardingWizard`), `akasha services install` | `curl … install.sh`, `hermes setup` | Scripts / npm selon doc amont | Via compte Anthropic + intégration IDE | Installeur IDE + compte | Clone / dépendances README | — | Existe · haute |
| Providers / modèles | `llm_router.yaml`, `akasha config models/provider` | `hermes model`, multi-provider | Clés API + local (ex. Ollama) | Claude API / abonnement | Choix modèle + cloud Cursor | BYO modèles | — | Existe · haute |
| Contexte long (≈64k+) | `AKASHA_MAX_CONTEXT_TOKENS`, compaction | Exigence doc Hermes | Selon routeur utilisateur | Fenêtre contexte IDE | Selon provider / plan | Selon modèle branché | — | **Existe · haute** |
| CLI / TUI / desktop | `akasha tui`, UI Tauri | `hermes`, `hermes --tui` | CLI principale | CLI / panneaux IDE | IDE natif | CLI + optional bot | — | Existe · haute |
| Sessions / reprise | `session_id`, mémoire, `/api/session-state`, `/api/session/resume-brief`, `terminal_session …`, recherche threads Tauri (`chatThreadSearch`) | `--continue`, sessions | Sessions bots / persistance selon stack | Threads liés au dépôt | Chats session IDE | Persistance locale (README) | Transcripts + résumés opérateur (RFC ph. A–B · wave 5) | **Existe · haute** |

---

## 3. Outils, exécution, canaux

| Domaine | Akasha | Hermes | OpenClaw | Claude Code | Cursor | Mercury | jcode (RFC) | État Akasha |
|---------|--------|--------|----------|-------------|--------|---------|-------------|-------------|
| Toolsets / UX outils | `tool_profiles`, `akasha toolset …`, `/api/tools/effective` | `hermes tools`, presets plateforme | Plugins / policy selon doc | Commandes + contexte repo | Rules, MCP, @ fichiers | Fichiers « soul » + budgets | File d’attente permissions (RFC ph. A–B · wave 5) | **Existe · haute** |
| Outils machine / registry | `AVAILABLE_TOOLS`, `akasha-tools` | 40+ outils registry | Selon skills/plugins | Outils Anthropic / MCP | Outils IDE + MCP | Outils README (git, shell, …) | — | Existe · haute |
| Terminal / backends | local, conteneur, `GET /api/terminal/capabilities`, **HTTP PTY** `/api/terminal/pty/sessions`, `akasha terminal capabilities` | local, docker, ssh, modal, … | Shell selon hébergement | Terminal intégré IDE | Terminal + agent | Shell local (README) | Cockpit swarm (RFC ph. D · wave 5) | **Existe · moyenne** |
| Sandbox | Policy deny-by-default, conteneur `--network=none` | Docker, approbation | Selon déploiement | Garde-fous éditeur | Sandboxing IDE | Permissions strictes README | — | Existe · haute |
| Gateway messagerie | Slack, Discord, Telegram, Teams | Telegram, Discord, … | Très large (messagers) | n/a | n/a | Souvent Telegram | — | **Existe · moyenne** |

---

## 4. Automation, intégrations, extensions

| Domaine | Akasha | Hermes | OpenClaw | Claude Code | Cursor | Mercury | jcode (RFC) | État Akasha |
|---------|--------|--------|----------|-------------|--------|---------|-------------|-------------|
| Webhooks externes | `POST /api/automation/webhook`, `/direct`, idempotence SQLite, `GET /api/automation/webhook/recent`, rate limit SQLite | Adapter HMAC, routes, direct | Selon automation | n/a focal | n/a focal | n/a focal | — | **Existe · haute** |
| Cron / automation planifiée | Scheduler persistant, `task_run`, pause/resume/run-now ; **P7** packs overnight/morning brief + `POST /api/schedules/from-nl` (Hermes NL) + `POST /api/channels/notify` | `cronjob`, NL scheduling, briefings gateway | Tâches / hooks selon stack | n/a | n/a | Tâches planifiées (README) | — | Existe · haute |
| Hooks (gateway / shell) | `lifecycle_hooks.json`, `on_http_request_pre/post`, `on_schedule_fire`, sandbox `strict` | gateway / plugin / shell | Webhooks / scripts | n/a | n/a | Hooks README | Gateway hooks (alignement conceptuel) | **Existe · moyenne** |
| MCP | validation + probe + `GET /api/mcp/runtime`, stdio long-lived, OAuth refresh `GET/POST …/oauth` | Serveurs MCP, OAuth | Config MCP utilisateur | MCP IDE | MCP IDE | n/a ou minimal | — | **Existe · haute** |
| Skills | Install URL, `skills.lock.jsonl`, hub `Akasha_skills`, UI galerie Tauri | Hub, auto-amélioration | Skills communautaires | MCP / instructions projet | Rules + packs | n/a focal | — | **Existe · haute** |
| Plugins WASM | Host WASM, réputation, `GET /api/plugins/metrics` | Extensions | Selon architecture | n/a | Extensions IDE | n/a | — | Existe · haute |

---

## 5. Données, observabilité & domaines avancés

| Domaine | Akasha | Hermes | OpenClaw | Claude Code | Cursor | Mercury | jcode (RFC) | État Akasha |
|---------|--------|--------|----------|-------------|--------|---------|-------------|-------------|
| Mémoire | LT 4 couches + RRF + graph expand + export/import, `memory_update`, janitor ; vs Mem0/Letta/Zep : local, pas SaaS | FTS5, profils, compaction | SaaS mémoire user_id | Contexte IDE | Indexation projet | « Second brain » SQLite (README) | Post-retrieval implémenté | **Différenciation** · haute |
| Doctor / diagnostics | `akasha doctor`, `/api/doctor` | `hermes doctor` | Logs / health selon stack | Diagnostics IDE | Diagnostics | n/a | — | Existe · moyenne |
| Perf / SLO | Métriques routeur, `scripts/bench-e2e.ps1`, runbook interne | Dashboard local récent | Selon déploiement | n/a | n/a | Budget tokens | Métriques qualité mémoire (RFC ph. C · livré) | **Existe · moyenne** |
| Cache idempotent HTTP | LRU `GET /api/router/models`, `/api/router/routes`, `/api/mcp/status`, `/api/doctor`, `/api/memory/recall-metrics`, `/api/lifecycle/hooks`, `/api/plugins/metrics`, `/api/process/watch/recent` (`http_get_cache.rs`) | — (non focal Hermes) | n/a | n/a | n/a | n/a | — | **Existe · haute** |
| Git worktree | `git_*`, `akasha worktree list|add|remove|doctor` | — | n/a | Outils git IDE | Outils git IDE | Git intégré README | — | **Existe · moyenne** |
| Browser (phase 2) | Playwright via daemon, hint santé Tauri | click/fill… | n/a | n/a | n/a | n/a | — | **Existe · moyenne** |
| Web crawl | Cloudflare `web_crawl` / status | — | n/a | n/a | n/a | n/a | — | **Existe · moyenne** |
| Migration type OpenClaw | `akasha migrate openclaw preview\|apply`, `openclaw_migration.rs`, `OpenClawMigrationPanel.tsx` (Tauri) | `hermes claw migrate` | Import configs communautaires | n/a | n/a | n/a | — | **Existe · moyenne** |
| RL / trajectoires | Hors cœur court terme | Atropos, batch | n/a | n/a | n/a | n/a | — | Absent |

**Implémentation & notes** (rappels utiles) : `crates/akasha-cli`, `crates/akasha-llm`, `crates/akasha-daemon/src/api.rs`, `../../43_session_terminal.md`, `../runtime/terminal-backends-roadmap.md`, `../runtime/gateway-shell-hooks.md`, `../../56_lifecycle_hooks.example.json`, `../runtime/cache-strategy.md`, `../integrations/automation-webhooks.md`, `../ops/slo-akasha-internal.md`, `../../54_memory_hierarchical_compaction.md`.

---

## 6. Applications satellites (rôle dans la matrice)

| Application | Rôle |
|-------------|------|
| Monorepo **Akasha** (daemon, CLI, tools) | Source de vérité code + API + cette matrice |
| **`apps/akasha-ui`** (Tauri) | UX desktop : session, outils, liens doc opérateur |
| **`Akasha_app`** | Site public : compare, guides MCP / webhooks / toolsets |
| **`Akasha_skills`** | Catalogue : semver, lockfile, evals CI |
| **`Akasha_plugins`** | Trust WASM, métadonnées permissions |
| **`akasha-code-studio`** | Cockpit opérateur : scheduler, `task_runs`, process watch, terminal, tools, MCP, lifecycle |
| **`Rbitnet`** | Inférence locale OpenAI-compatible, `/metrics`, benches |

---

## 7. Propriétaire par thème (où c’est visible hors daemon)

Légende : **C** = core · **Doc** = doc / site · **Cat** = catalogues skills/plugins · **Studio** = Code Studio · **Rbit** = Rbitnet · **UI** = Tauri.

| Thème | C | Doc | Cat | Studio | Rbit | UI |
|-------|---|-----|-----|--------|------|-----|
| Setup / doctor / services | ● | ● | — | — | ● | — |
| Providers / routing / fallback | ● | ● | — | — | ● | — |
| Sessions / reprise / mémoire | ● | ● | — | ● | — | ● |
| Toolsets / tools policy | ● | ● | — | ● | — | ● |
| Terminal / PTY / background | ● | ● | — | ● | — | ● |
| Webhooks / automation externe | ● | ● | — | ● | — | — |
| MCP (validation, probe, runtime, OAuth doc) | ● | ● | ● | ● | — | ● |
| Skills / lockfile | ● | ● | ● | — | — | — |
| Plugins WASM / métriques | ● | ● | ● | ○ | — | ● |
| Perf / SLO / bench | ● | ● | — | ● | ○ | — |

**○** = surface partielle ou roadmap UI — mettre à jour quand une PR satellite ferme une case.

---

## Changelog

- **3.0.2** (2026-07-17): P7 Life layer — overnight/morning brief packs, `POST /api/channels/notify`, NL schedule Hermes (`/api/schedules/from-nl`) ; signaux Vellum/Lindy/Joanium/Hermes dans canvas concurrence.
- **3.0.1** (2026-06-07): resync registre [`ROADMAP_FINAL_REGISTRY.md`](./ROADMAP_FINAL_REGISTRY.md) v1.2.0 — M-* post-clôture, preuves lifecycle/Matrix ; notes Odysseus en suivi post-clôture.
- **3.0.0** (2026-06-06): **roadmap-complete** — satellites §7 Studio + plugins ● (handoff, fork, recall-metrics, trust-catalog `hook_events`) ; clôture registre [`ROADMAP_FINAL_REGISTRY.md`](./ROADMAP_FINAL_REGISTRY.md) v1.1.0.
- **2.1.1** (2026-06-06): Phase 1 matrice closure — sessions Reprendre (Tauri + Studio), webhooks recent cockpit, MCP OAuth refresh, skills browse UI, worktree doc site, browser health hint, OpenClaw import memory, S-ORCH-01 runbook ; M-01…M-15 → Existe.
- **2.1.0** (2026-06-06): wave 7 resync — OpenClaw migration CLI+UI → Existe · moyenne ; cache HTTP généralisé (`http_get_cache.rs` 8 routes) → Existe · haute ; sessions/reprise + recherche threads Tauri → Existe · haute ; constitution recall filter (`constitution.rs`, S-RAG-02).
- **2.0.1** (2026-06-06): resync roadmaps — jcode wave 5, wizard Tauri onboarding, steering pi-mono livré ([`pi_mono_alignment_priorities.md`](./pi_mono_alignment_priorities.md)).
- **2.0.0** (2026-05-03): **Centralisation multi-produits** — un seul document avec synthèse + tableaux thématiques (Hermes, OpenClaw, Claude Code, Cursor, Mercury Agent, colonne RFC jcode) ; `hermes-akasha-parity-matrix.md` conservé comme stub de redirection ; liens mis à jour (`Akasha_app`, `Rbitnet`, `apps/akasha-ui`, specs internes).
- **1.0.8** (2026-04-27): cockpit `akasha-code-studio` restructuré en sections opérateur typées (task runs, process watch, terminal, tools, MCP, lifecycle) avec actions scheduler + auto-refresh léger + fallback raw JSON ; E2E smoke cockpit renforcés ; doc `OPERATOR_COCKPIT.md` synchronisée ; création projet Studio avec initialisation git `main` et reprise projet avec garde-fou de branche primaire (`main`/`master`).
- **1.0.7** (2026-04-27): OAuth MCP state persisté (`mcp_oauth_state.json`) via `GET/POST /api/mcp/runtime/oauth` ; docs runtime MCP alignées ; `akasha worktree doctor` ; roadmap explicite des domaines encore partiels (`hermes-partial-domains-roadmap.md`).
- **1.0.6** (2026-04-27): `terminal_session` branché sur API PTY (start/list/read/write/resize/stop) + sessions listables ; MCP OAuth state routes (`GET/POST /api/mcp/runtime/oauth`) ; sandbox hooks mode `strict` (allowlist commandes) ; rate-limit distribué webhooks via SQLite (`AKASHA_WEBHOOK_RATE_SQLITE`) ; cache LRU étendu (`/api/router/routes`, `/api/mcp/status`) ; cockpit opérateur enrichi (health résumé) sur Studio/TUI/Tauri ; CI skills auto-découverte des `self_check.sh`.
- **1.0.5** (2026-04-27): **PTY HTTP** (`/api/terminal/pty/sessions*`, `portable-pty`) ; **MCP runtime** `GET /api/mcp/runtime`, stdio long-lived `POST /api/mcp/runtime/stdio/start|stop` ; **gateway hooks** `on_http_request_pre` / `on_http_request_post` (timeouts `AKASHA_GATEWAY_HOOK_TIMEOUT_SECS`) ; **cache LRU** GET `/api/router/models` (`AKASHA_HTTP_CACHE_TTL_SECS`) ; webhooks `AKASHA_WEBHOOK_IDEM_SQLITE` ; CLI `akasha terminal capabilities` ; cockpit **Code Studio** + **TUI** + **Tauri** (MCP runtime + terminal) ; **Akasha_app** docs MCP + terminal ; **Akasha_skills** pilotes `scripts/self_check.sh` + CI ; **Akasha_plugins** trust catalogue (hook events WASM) ; **Rbitnet** lien bench ↔ matrice.
- **1.0.4** (2026-04-26): webhooks idempotence persistante (`webhook_idempotency.sqlite3`) ; `GET /api/mcp/status` ; `GET /api/lifecycle/hooks` ; cockpit Code Studio (recall, MCP, lifecycle, actions scheduler) ; TUI onglet Routeur (snapshot opérateur) ; Tauri réglages expert (recall, MCP, lifecycle) ; site `Akasha_app` (digest WASM plugins, min daemon skills) ; CI catalogue `Akasha_plugins` ; validation `akasha_daemon_min_version` dans `Akasha_skills` CI.
- **1.0.3** (2026-04-26): colonne écosystème (applications + tableau propriétaire → satellites) ; inclusion explicite de `apps/akasha-ui`.
- **1.0.2** (2026-04-26): webhooks signés + direct delivery, watch processus (`GET /api/process/watch/recent`), hooks schedule, MCP stdio probe (daemon + CLI), `akasha services logs|restart|doctor`, docs MCP OAuth / terminal backends / hooks.
- **1.0.1** (2026-04-26): browser phase 2, crawl Cloudflare MVP, métriques recall/plugins, CLI toolset/worktree/config validate, MCP validation + docs cache/SLO/compaction.
- **1.0.0** (2026-04-26): publication initiale alignée sur le plan d’intégration Hermes vs Akasha.
