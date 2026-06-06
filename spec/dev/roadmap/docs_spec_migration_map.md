> **Archive:** Ce document est archivé. Source de vérité active : [`ROADMAP_FINAL_REGISTRY.md`](./ROADMAP_FINAL_REGISTRY.md).

# Matrice de migration `docs/` -> `spec/` (avril 2026)

Ce document trace le refactor de séparation documentaire:
- `docs/` = documentation utilisateur finale
- `spec/` = documentation développeur, architecture, exploitation et suivi d'implémentation

## Migrations effectuées

| Ancien chemin (`docs/`) | Nouveau chemin (`spec/`) | Bloc fonctionnel |
|---|---|---|
| `docs/automation-webhooks.md` | `spec/dev/integrations/automation-webhooks.md` | Intégrations |
| `docs/mcp-mvp.md` | `spec/dev/integrations/mcp-mvp.md` | Intégrations |
| `docs/mcp-oauth.md` | `spec/dev/integrations/mcp-oauth.md` | Intégrations |
| `docs/mcp-runtime.md` | `spec/dev/integrations/mcp-runtime.md` | Intégrations |
| `docs/integrations/claude-src-akasha.md` | `spec/dev/integrations/claude/claude-src-akasha.md` | Intégrations |
| `docs/plugin-host-network.md` | `spec/dev/plugins/plugin-host-network.md` | Plugins |
| `docs/plugin-map-view-schema.md` | `spec/dev/plugins/plugin-map-view-schema.md` | Plugins |
| `docs/cache-strategy.md` | `spec/dev/runtime/cache-strategy.md` | Runtime |
| `docs/gateway-shell-hooks.md` | `spec/dev/runtime/gateway-shell-hooks.md` | Runtime |
| `docs/terminal-backends-roadmap.md` | `spec/dev/runtime/terminal-backends-roadmap.md` | Runtime |
| `docs/bench_prompt_results.md` | `spec/dev/quality/bench_prompt_results.md` | Qualite |
| `docs/tests_and_benchmarks.md` | `spec/dev/quality/tests_and_benchmarks.md` | Qualite |
| `docs/hermes-akasha-parity-matrix.md` | `spec/dev/roadmap/hermes-akasha-parity-matrix.md` (stub → `reference-products-parity-matrix.md`) | Roadmap |
| — | `spec/dev/roadmap/reference-products-parity-matrix.md` | Matrice multi-produits (source) |
| `docs/hermes-integration-remainder.md` | `spec/dev/roadmap/hermes-integration-remainder.md` | Roadmap |
| `docs/hermes-partial-domains-roadmap.md` | `spec/dev/roadmap/hermes-partial-domains-roadmap.md` | Roadmap |
| `docs/internal_release_0.8.md` | `spec/dev/releases/internal_release_0.8.md` | Releases |
| `docs/runbooks/slo-akasha-internal.md` | `spec/dev/ops/slo-akasha-internal.md` | Ops |

## Fichiers conserves dans `docs/`

- `docs/user_guide_final.md`
- `docs/user_guide.md`
- `docs/README.md`
- `docs/screenshots/README.md`
