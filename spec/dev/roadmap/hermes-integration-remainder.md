> **Archive:** Ce document est archivé. Source de vérité active : [`ROADMAP_FINAL_REGISTRY.md`](./ROADMAP_FINAL_REGISTRY.md) v1.2.0.

# Hermes parity — remainder (roadmap)

**Clôturé 2026-06-06** — le registre final porte les statuts terminaux M-* et STUB. Ce fichier reste une **checklist opérateur / satellite** pour les PR de parité Hermes.

## Livré en core (v0.8.x + waves 7–8)

Signed inbound webhooks, idempotency + rate limit SQLite, process watch, schedule/gateway hooks, MCP runtime + OAuth + `mcp_server_add`/`remove`, PTY HTTP, cache LRU étendu (`http_get_cache.rs`), worktree doctor, OpenClaw migration API+CLI+UI, **plugin hook bus** (`task_completed`, `task_failed`, `task_cancelled`, `on_schedule_fire`, `on_channel_message`), steering/follow-up queues, permission review queue, task operator reports, Code Studio swarm MVP.

## Items anciennement « incrementaux » — décision registre

| Sujet | Statut terminal | ID |
|-------|-----------------|-----|
| MCP agent CRUD (`mcp_server_add` / `remove`) | **Production** | S-TOOL-05, M-07 |
| Migration OpenClaw import mémoire | **Production** (semi-auto) | S-MIG-01, M-14 |
| Trust automation `Akasha_plugins` | **Production** (CI trust-catalog) | M-07 / satellites ● |
| Couverture WASM hooks tous événements catalogue | **Reporter** | Hors scope § Phase 5 |
| Worktree / browser policy UX résiduels | **Existe · moyenne** | M-11, M-12 — voir [`hermes-partial-domains-roadmap.md`](./hermes-partial-domains-roadmap.md) |

Suivi statut produit : [`reference-products-parity-matrix.md`](./reference-products-parity-matrix.md).

## Definition of Done — PR « analyse Hermes → écosystème »

Pour qu’une évolution de parité Hermes soit **consommable** au-delà du monorepo :

1. **Registre** : si statut terminal change, mettre à jour [`ROADMAP_FINAL_REGISTRY.md`](./ROADMAP_FINAL_REGISTRY.md) (ou section post-clôture si hors M-*).
2. **Matrice** : mettre à jour la version et le changelog dans [`reference-products-parity-matrix.md`](./reference-products-parity-matrix.md) si le statut d’une ligne change (ou ajouter une note en « ○ » dans le tableau propriétaire).
3. **Au moins un satellite** parmi :
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
