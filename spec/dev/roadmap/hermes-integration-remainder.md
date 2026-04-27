# Hermes parity — remainder (roadmap)

**Delivered in core (v0.8.x iteration — passe suivante):** signed inbound webhooks (`/api/automation/webhook`, `/direct`), idempotency + rate limit, **SQLite-backed idempotency keys** + override **`AKASHA_WEBHOOK_IDEM_SQLITE`** (fichier partagé multi-instance), process completion ring buffer (`GET /api/process/watch/recent`), schedule shell hooks (`lifecycle_hooks.json`), **HTTP gateway hooks** `on_http_request_pre` / `on_http_request_post` (timeouts `AKASHA_GATEWAY_HOOK_TIMEOUT_SECS`, métadonnée `AKASHA_GATEWAY_HOOK_SANDBOX`), operator **`GET /api/lifecycle/hooks`**, operator **`GET /api/mcp/status`** + **`GET /api/mcp/runtime`** + **stdio long-lived** `POST /api/mcp/runtime/stdio/start|stop`, MCP stdio probe (`mcp_stdio`, `akasha mcp`), **PTY HTTP** `/api/terminal/pty/sessions*` + `akasha terminal capabilities`, **cache LRU** GET `/api/router/models` (`AKASHA_HTTP_CACHE_TTL_SECS`), terminal capabilities JSON, Docker compose `logs|restart|doctor` via CLI, **Studio Git guardrails** (init auto de repo sur `main` à la création et création/checkout `main|master` à la reprise), **Cockpit Code Studio structuré** (sections opérateur actionnables + auto-refresh runs/process + fallback raw JSON), and companion docs (`../integrations/mcp-runtime.md`, `../integrations/automation-webhooks.md`, `../integrations/mcp-oauth.md`, `../runtime/terminal-backends-roadmap.md`, `../runtime/gateway-shell-hooks.md`).

**Still roadmap / deeper work:** plugin-level WASM hook **delivery** complète (événements côté host sur tout le catalogue), cache GET élargi à plus d’endpoints métiers, et industrialisation trust automation bout-en-bout côté `Akasha_plugins`.

Track in the issue tracker and in **`hermes-akasha-parity-matrix.md`**. For remaining “Partiel” domains (worktree/browser/crawl/migration), see **`hermes-partial-domains-roadmap.md`**. Satellite repos: incremental PRs per repo as priorities allow (see README / `docs/HERMES_COCKPIT.md` / `EVALS_AND_VERSIONING.md` stubs).

## Definition of Done — PR « analyse Hermes → écosystème »

Pour qu’une évolution de parité Hermes soit **consommable** au-delà du monorepo :

1. **Matrice** : mettre à jour la version et le changelog dans [`hermes-akasha-parity-matrix.md`](./hermes-akasha-parity-matrix.md) si le statut d’une ligne change (ou ajouter une note en « ○ » dans le tableau propriétaire).
2. **Au moins un satellite** parmi :
   - **`Akasha_app`** : section doc ou page compare (guides opérateur, liens vers les `.md` du repo Akasha sur GitHub) ;
   - **`Akasha_skills`** / **`Akasha_plugins`** : métadonnées catalogue (version, compat, hash) ou CI de validation ;
   - **`akasha-code-studio`** / **`apps/akasha-ui`** : appel ou affichage d’un endpoint documenté (cockpit / réglages opérateur) ;
   - **`Rbitnet`** : doc d’exploitation ou métriques alignées self-hosted.

Les PR **core-only** restent possibles pour correctifs internes ; dans ce cas, créer une issue satellite référencée depuis la matrice pour ne pas perdre la visibilité produit.

## Vérification locale (CI / Windows)

- **Daemon** : `cargo check -p akasha-daemon --lib` valide les changements Rust sans lier le binaire `akasha-daemon` (sur certains postes Windows le link ONNX peut échouer ; utiliser alors les flags documentés dans `../integrations/mcp-runtime.md` pour `cargo test --lib` ou une toolchain MSVC à jour).
- **Code Studio** : `npm run build` à la racine de `akasha-code-studio`.
- **TUI** : `cargo check -p akasha-tui`.
