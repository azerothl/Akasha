> **Archive:** Ce document est archivé. Source de vérité active : [`ROADMAP_FINAL_REGISTRY.md`](./ROADMAP_FINAL_REGISTRY.md).

# Hermes parity — remainder (roadmap)

**Delivered in core (v0.8.x + wave 7):** signed inbound webhooks, idempotency + rate limit, SQLite-backed idempotency, process watch, schedule/gateway hooks, MCP runtime + OAuth routes, PTY HTTP, cache LRU (extended: lifecycle hooks, plugins metrics, process watch), worktree doctor, OpenClaw migration API+CLI+UI, **plugin hook bus** (`plugin_hook_bus.rs` on `task_completed`), steering/follow-up queues, permission review queue, task operator reports, Code Studio swarm MVP.

**Still incremental / deeper work:** full WASM hook coverage for all catalog events (beyond `task_completed`), MCP agent CRUD, migration OpenClaw memory import automation, industrialisation trust automation bout-en-bout côté `Akasha_plugins` (CI trust-catalog added).

Track in [`reference-products-parity-matrix.md`](./reference-products-parity-matrix.md) and [`ROADMAP_CLOSURE_STATUS.md`](./ROADMAP_CLOSURE_STATUS.md).

For remaining “Partiel” domains (worktree docs, browser policy UX), see **`hermes-partial-domains-roadmap.md`**.

## Definition of Done — PR « analyse Hermes → écosystème »

Pour qu’une évolution de parité Hermes soit **consommable** au-delà du monorepo :

1. **Matrice** : mettre à jour la version et le changelog dans [`reference-products-parity-matrix.md`](./reference-products-parity-matrix.md) si le statut d’une ligne change (ou ajouter une note en « ○ » dans le tableau propriétaire).
2. **Au moins un satellite** parmi :
   - **`Akasha_app`** : section doc ou page compare (guides opérateur, liens vers les `.md` du repo Akasha sur GitHub) ;
   - **`Akasha_skills`** / **`Akasha_plugins`** : métadonnées catalogue (version, compat, hash) ou CI de validation ;
   - **`akasha-code-studio`** / **`apps/akasha-ui`** : appel ou affichage d’un endpoint documenté (cockpit / réglages opérateur) ;
   - **`Rbitnet`** : doc d’exploitation ou métriques alignées self-hosted.

Les PR **core-only** restent possibles pour correctifs internes ; dans ce cas, créer une issue satellite référencée depuis la matrice pour ne pas perdre la visibilité produit.

## Vérification locale (CI / Windows)

- **Daemon** : `cargo check -p akasha-daemon --lib`
- **Code Studio** : `npm run build` à la racine de `akasha-code-studio`.
- **TUI** : `cargo check -p akasha-tui`.
- **UI** : `cd apps/akasha-ui && npm run build`
