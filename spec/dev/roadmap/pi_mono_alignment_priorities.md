> **Archive:** Ce document est archivé. Statut : **Livré / Archivé** (2026-06-06). Source de vérité active : [`ROADMAP_FINAL_REGISTRY.md`](./ROADMAP_FINAL_REGISTRY.md).

# Priorités d'alignement (inspiration pi-mono vs Akasha)

**Statut : Livré / Archivé**

Document de **décision produit / technique** suite à l'analyse [pi-mono](https://github.com/badlogic/pi-mono). Tous les axes prioritaires sont livrés ou reclassés Reporter.

## Axes livrés (2026-06)

### 1. File « steering » / « follow-up » pendant tâches longues — **Livré**

**Référence Pi** : `@mariozechner/pi-agent-core` — `steer()` vs `followUp()`.

**Livré Akasha** :

- **API daemon** : `message_delivery_mode` (`steering` | `follow_up`) sur `POST /api/message` ; `GET/DELETE /api/tasks/:id/queue` — `crates/akasha-daemon/src/steering_queue.rs`.
- **CLI** : `akasha task queue list|clear <task_id>`.
- **Tauri** : sélecteur de mode livraison + raccourcis Entrée=steering (tâche active) / Alt+Entrée=follow-up — `apps/akasha-ui/src/App.tsx`.
- **Code Studio** : sélecteur steering / follow-up — `akasha-code-studio/src/App.tsx`.
- **TUI** : envoi avec `message_delivery_mode` steering ou follow_up.
- **Événements** : `user_steering_queued`, `user_follow_up_queued`, `user_steering_applied`, `user_follow_up_applied` — [agent_client_event_contract.md](../runtime/agent_client_event_contract.md).

### 2. Fork de session dans Code Studio (branche depuis un message) — **Livré**

**Référence Pi** : `coding-agent` — `/fork`, `/tree`, sessions JSONL avec `parentId`.

**Statut (2026-06)** : v1 implémentée — action UI « Fork à partir d'ici », nouvelle tâche + `session_id` fille, événement `session_fork_created` côté daemon. Voir [`docs/SESSION_FORK_SPEC.md`](../../../../akasha-code-studio/docs/SESSION_FORK_SPEC.md).

**Reporter v2** : arbre interactif `/tree` ; export HTML gist ; fusion de branches.

### 3. Handoff modèle explicite — **Livré**

`POST /api/session/handoff` avec `target_model` / `target_provider` ; réponse `schema_version: 2`. UI handoff Code Studio + routeur daemon.

## Axes reportés (justification courte)

| Axe | Report |
|-----|--------|
| **Streaming JSON partiel des tool calls** (`toolcall_delta`) | **Production** (S-EVT-01) — voir [pi_mono_backend_parity_check.md](../integrations/pi_mono_backend_parity_check.md). |
| **CSI 2026 / TUI différentiel** (`pi-tui`) | **Reporter** — faible priorité. |
| **RPC stdio JSONL** (`pi --mode rpc`) | **Reporter** — HTTP + tâches suffisent. |
| **Fork tree UI v2** | **Reporter** — trace événementielle v1 suffit. |

## Références croisées

- Vérification backend (tokens, coût, streaming outils) : [pi_mono_backend_parity_check.md](../integrations/pi_mono_backend_parity_check.md)
- Contrat d'événements client (cible SSE/WebSocket) : [agent_client_event_contract.md](../runtime/agent_client_event_contract.md)
- Veille consolidée : [wave6_veille_backlog.md](./wave6_veille_backlog.md)

**Dernière mise à jour :** 2026-06-06 (clôture roadmap — registre v1.1.0).
