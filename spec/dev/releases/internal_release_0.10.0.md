# Notes internes — release 0.10.0 (depuis v0.9.0)

Document contributeurs / équipe release. Complète [roadmap_v0.10.0.md](roadmap_v0.10.0.md), [tests_and_benchmarks.md](../quality/tests_and_benchmarks.md), [REFACTOR_MONOREPO_TRACKING.md](../quality/REFACTOR_MONOREPO_TRACKING.md).

## Périmètre

Référence git : **v0.9.0** → **0.10.0**. Thèmes : **parcours first-use embarqué**, packaging CUDA complet, discovery + Home Assistant, refactor API daemon (L3–L6), **cockpit agentic (P6)**, **Life layer (P7)**.

## Life layer (P7)

- **Overnight pack** : schedule nocturne template + UI Calendrier / Mission (`LifeLayerPanel`).
- **Morning brief** : `POST /api/channels/notify` (Telegram) + hook notify sur schedules tagués + UI heure/canal.
- **OAuth Connectors** : CalDAV Google/Microsoft surface dans Settings → Connectors (même flux que Calendrier).
- **NL schedule (Hermes)** : `POST /api/schedules/from-nl` + `akasha schedule from-nl "…"`.
- Stretch livré : B3 process-watch → wakeup, B4 subagent threads UI (Active work), H5 `POST /api/skills/draft-from-task`.

## Cockpit agentic (P6)

- **Active work** : bandeau/drawer chat des tâches running/queued, deep-link session, cancel/pause, indicateur sidebar (`ActiveWorkDrawer.tsx`).
- **Modes composer** : presets `architect` / `code` / `ask` / `agent` (`agent_profiles` + `composer_mode` sur `POST /api/message`).
- **Usage 7/30j** : Paramètres → Système → Usage (`UsageDashboardPanel.tsx`, `get_router_metrics?period=`).
- **Sessions** : pin, fork transcript, dual-pane MVP.
- Stretch B3/B4 reportés post-tag ; hors tag : attach externe, recipes, PWA, Chrome ext.

## First-use / embarqué

- Wizard Tauri : statut modèle, download GGUF + progression, multi-modèle, premier message test (`OnboardingWizard.tsx`, `EmbeddedLocalModelSettings.tsx`).
- API `POST/GET /api/router/embedded/download`, calibration `POST /api/router/embedded/calibrate`.
- `doctor --json` : check `embedded_llm` evidence-gated avec `action: embedded-download`.
- Artefact **`akasha-full-windows-x86_64-cuda.zip`** (CLI + UI + scripts).
- Bench : `spec/dev/quality/bench_embedded.ps1`, `bench_embedded_models.ps1`, baselines dans `bench_embedded_results.md`.

## Intégrations post-v0.9.0

- **Service discovery** (`akasha-core`) : profils `ollama`, `homeassistant` ; `GET /api/discovery`, `akasha discover`.
- **Home Assistant** : connecteur (`connectors.env`, vault token), UI Connecteurs, plugin `homeassistant`, outils `ha_*`.
- **CUDA install** : `install.ps1` copie DLL runtime (`cudart`, `cublas`) vers `InstallDir`.

## Refactor daemon (v0.10.0)

Extraction de `api.rs` vers :

| Module | Rôle |
|--------|------|
| `api_routes_docs.rs` | `/`, `/api/status`, `/api/docs`, update status |
| `api_routes_config.rs` | session, connectors, discovery, config |
| `api_routes_router.rs` | router, embedded, budget, voice |
| `api_routes_plugins.rs` | device, plugins, skills, tools |
| `api_routes_memory.rs` | `/api/memory/*`, agent-identity |
| `api_routes_tasks.rs` | message, tasks, schedules, timeline |

`api.rs` : ~16k lignes (cible <8k reportée v0.11 pour boucle outils / `execute_tool_call`).

## CI / release

- Workflow **GPU CI** : `.github/workflows/gpu-ci.yml` (job `gpu-smoke` sur runner `self-hosted,gpu,nvidia`).
- Smoke staging : `scripts/smoke-release-staging.ps1` (CPU HTTP + CUDA layout).
- E2E UI : `apps/akasha-ui/e2e/wizard-embedded.spec.ts`, `smoke.spec.ts`.

## Satellites

- **Akasha_app** : `api/latest.json`, `data/releases.json`, pills v0.10.0, section What's new.
- **akasha-code-studio** : npm `0.10.0`.
- **Akasha_skills** : skill `home-assistant` v1.0.0.
- **Akasha_plugins** : 6 plugins (dont `homeassistant`).

## Validation release

Voir checklist Phase 6 du plan release v0.10.0 (init zip, TUI 7 onglets, Tauri 13 onglets, plugins/skills).

### Automatisé (2026-07-14)

| Check | Résultat |
|-------|----------|
| `cargo test --workspace` | ✅ vert (fix CRLF `studio_worktree` Windows) |
| `cargo test -p akasha-daemon --lib` | ✅ 343 passed |
| `apps/akasha-ui` unit tests (vitest) | ✅ 43 passed (3 erreurs teardown module loader non bloquantes) |
| `smoke-release-staging.ps1` | ⏭ staging/ absent localement — à exécuter post-build Release |
| E2E Playwright | ⏭ nécessite daemon + `npm run test:e2e:install` |
| GPU CI `gpu-smoke` | ⏭ runner self-hosted non provisionné |

### Manuel (opérateur, avant tag)

- [ ] Install CPU full zip → wizard → premier message
- [ ] Install CUDA full zip → `embedded-download` → premier message GPU
- [ ] `akasha discover` / `homeassistant`
- [ ] Connecteur HA + plugin `homeassistant`
- [ ] TUI 7 onglets + scroll
- [ ] Tauri 13 onglets + Paramètres embedded/connectors
- [ ] 2 plugins + 2 skills installés depuis catalogue

### Tag / publish (après GPU CI vert)

Prérequis : runner `self-hosted,gpu,nvidia` provisionné + job `gpu-smoke` vert sur `main`.

```bash
# Akasha monorepo — commit puis :
git tag v0.10.0 -m "Release v0.10.0"
git push origin v0.10.0

# Satellites (commits séparés si besoin) :
# Akasha_app → push main (JSON + HTML)
# Akasha_skills → push skill home-assistant v1.0.0
# akasha-code-studio :
cd akasha-code-studio && npm publish
```

- [ ] GitHub Release Akasha : artefacts CPU, CUDA, full zip présents
- [ ] `api/latest.json` sur GitHub Pages → bannière update en 0.9.x → 0.10.0
- [ ] Install fresh `setup.ps1` depuis release
- [ ] Job `gpu-ci.yml` vert post-tag sur runner provisionné
- [ ] `npx akasha-code-studio@0.10.0` → proxy `/api` OK
