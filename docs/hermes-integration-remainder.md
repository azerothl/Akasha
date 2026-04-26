# Hermes parity — remainder (roadmap)

**Delivered in core (v0.8.x iteration):** signed inbound webhooks (`/api/automation/webhook`, `/direct`), idempotency + rate limit, **SQLite-backed idempotency keys** (see `docs/automation-webhooks.md`), process completion ring buffer (`GET /api/process/watch/recent`), schedule shell hooks (`lifecycle_hooks.json`), operator **`GET /api/lifecycle/hooks`** (summary of hook phases), operator **`GET /api/mcp/status`** (validates `mcp.json` in data dir if present), MCP stdio probe (`mcp_stdio`, `akasha mcp`), terminal capabilities JSON, Docker compose `logs|restart|doctor` via CLI, and companion docs (`docs/mcp-runtime.md`, `docs/automation-webhooks.md`, `docs/mcp-oauth.md`, `docs/terminal-backends-roadmap.md`, `docs/gateway-shell-hooks.md`).

**Still roadmap / deeper work:** full interactive PTY sessions + resize + resume, HTTP/SSE MCP client + OAuth flows in-product, gateway middleware hooks (pre/post every HTTP route), plugin-level WASM hook events, distributed webhook store (multi-instance), advanced sandbox for hook scripts, richer Code Studio / Tauri wiring for all operator endpoints, full catalog trust automation in `Akasha_plugins`.

Track in the issue tracker and in **`docs/hermes-akasha-parity-matrix.md`**. Satellite repos: incremental PRs per repo as priorities allow (see README / `docs/HERMES_COCKPIT.md` / `EVALS_AND_VERSIONING.md` stubs).

## Definition of Done — PR « analyse Hermes → écosystème »

Pour qu’une évolution de parité Hermes soit **consommable** au-delà du monorepo :

1. **Matrice** : mettre à jour la version et le changelog dans [`docs/hermes-akasha-parity-matrix.md`](./hermes-akasha-parity-matrix.md) si le statut d’une ligne change (ou ajouter une note en « ○ » dans le tableau propriétaire).
2. **Au moins un satellite** parmi :
   - **`Akasha_app`** : section doc ou page compare (guides opérateur, liens vers les `.md` du repo Akasha sur GitHub) ;
   - **`Akasha_skills`** / **`Akasha_plugins`** : métadonnées catalogue (version, compat, hash) ou CI de validation ;
   - **`akasha-code-studio`** / **`apps/akasha-ui`** : appel ou affichage d’un endpoint documenté (cockpit / réglages opérateur) ;
   - **`Rbitnet`** : doc d’exploitation ou métriques alignées self-hosted.

Les PR **core-only** restent possibles pour correctifs internes ; dans ce cas, créer une issue satellite référencée depuis la matrice pour ne pas perdre la visibilité produit.

## Vérification locale (CI / Windows)

- **Daemon** : `cargo check -p akasha-daemon --lib` valide les changements Rust sans lier le binaire `akasha-daemon` (sur certains postes Windows le link ONNX peut échouer ; utiliser alors les flags documentés dans `docs/mcp-runtime.md` pour `cargo test --lib` ou une toolchain MSVC à jour).
- **Code Studio** : `npm run build` à la racine de `akasha-code-studio`.
- **TUI** : `cargo check -p akasha-tui`.
