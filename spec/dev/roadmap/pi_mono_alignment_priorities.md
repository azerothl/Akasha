# Priorités d'alignement (inspiration pi-mono vs Akasha)

Document de **décision produit / technique** suite à l'analyse [pi-mono](https://github.com/badlogic/pi-mono). Il ne modifie pas le code ; il cadrage les axes retenus pour des tickets ou plans d'implémentation ultérieurs.

## Axes livrés (2026-06)

### 1. File « steering » / « follow-up » pendant tâches longues — **Livré**

**Référence Pi** : `@mariozechner/pi-agent-core` — `steer()` vs `followUp()`.

**Livré Akasha** :

- **API daemon** : `message_delivery_mode` (`steering` | `follow_up`) sur `POST /api/message` ; `GET/DELETE /api/tasks/:id/queue` — `crates/akasha-daemon/src/steering_queue.rs`.
- **CLI** : `akasha task queue list|clear <task_id>`.
- **Tauri** : sélecteur de mode livraison + `SteeringQueueBell` / `SteeringQueuePanel` — `apps/akasha-ui/src/components/`.
- **Code Studio** : sélecteur steering / follow-up — `akasha-code-studio/src/App.tsx`.
- **TUI** : envoi avec `message_delivery_mode` steering ou follow_up.
- **Événements** : `user_steering_queued`, `user_follow_up_queued`, `user_steering_applied`, `user_follow_up_applied` — [agent_client_event_contract.md](../runtime/agent_client_event_contract.md).

**Hors périmètre v1 (reste ouvert)** : parité exacte des raccourcis clavier Pi (Enter vs Alt+Enter) ; modes de vidage `one-at-a-time` | `all` explicites côté client.

### 2. Fork de session dans Code Studio (branche depuis un message) — **Livré**

**Référence Pi** : `coding-agent` — `/fork`, `/tree`, sessions JSONL avec `parentId`.

**Statut (2026-06)** : v1 implémentée — action UI « Fork à partir d'ici », nouvelle tâche + `session_id` fille, événement `session_fork_created` côté daemon. Voir [`docs/SESSION_FORK_SPEC.md`](../../../../akasha-code-studio/docs/SESSION_FORK_SPEC.md) (cases cochées v1).

**Hors périmètre v1 (reste ouvert)** : arbre interactif complet type `/tree` Pi ; export HTML gist ; fusion de branches.

## Prochaine priorité produit

### Handoff modèle explicite

Reprendre le transcript d'une session avec un **autre modèle** (routeur + UI). Dépend d'une sérialisation de contexte stable — voir [wave6_veille_backlog.md](./wave6_veille_backlog.md).

## Axes reportés (justification courte)

| Axe | Report |
|-----|--------|
| **Streaming JSON partiel des tool calls** (`toolcall_delta`) | Voir [pi_mono_backend_parity_check.md](../integrations/pi_mono_backend_parity_check.md) : aujourd'hui les outils sont surtout dérivés du texte assistant final ; évolution **backend + contrat événements**. |
| **CSI 2026 / TUI différentiel** (`pi-tui`) | Gain UX terminal ; **faible priorité**. |
| **RPC stdio JSONL** (`pi --mode rpc`) | HTTP + tâches couvrent l'intégration IDE ; RPC **optionnel** si partenaire IDE l'exige. |

## Références croisées

- Vérification backend (tokens, coût, streaming outils) : [pi_mono_backend_parity_check.md](../integrations/pi_mono_backend_parity_check.md)
- Contrat d'événements client (cible SSE/WebSocket) : [agent_client_event_contract.md](../runtime/agent_client_event_contract.md)
- Veille consolidée : [wave6_veille_backlog.md](./wave6_veille_backlog.md)

**Dernière mise à jour :** 2026-06-06 (steering/follow-up marqué livré — aligné code + wave 6).
