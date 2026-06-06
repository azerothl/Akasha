# Priorités d'alignement (inspiration pi-mono vs Akasha)

Document de **décision produit / technique** suite à l'analyse [pi-mono](https://github.com/badlogic/pi-mono). Il ne modifie pas le code ; il cadrage les **1–2 axes** retenus pour des tickets ou plans d'implémentation ultérieurs.

## Axes retenus pour la prochaine vague

### 1. File « steering » / « follow-up » pendant tâches longues

**Référence Pi** : `@mariozechner/pi-agent-core` — `steer()` vs `followUp()`, modes `one-at-a-time` | `all` ; dans le CLI, file d'attente (Enter vs Alt+Enter).

**Problème utilisateur Akasha** : pendant une tâche longue (outils, orchestration), un second message est soit bloquant, soit traité de façon ambiguë selon le canal (Tauri, TUI, Code Studio).

**Décision** : traiter ce sujet comme **priorité 1** côté produit.

**Livrables cibles (à découper en tickets)** :

- Contrat API : champs ou endpoint pour **injecter** un message « après le tour assistant courant » vs « après fin complète du travail » (aligné conceptuellement sur steer / follow-up, sans imposer les noms Pi).
- UI : file visible + annulation (équivalent `clearSteeringQueue` / `clearFollowUpQueue`).
- Voir aussi [agent_client_event_contract.md](../runtime/agent_client_event_contract.md) pour exposer ces transitions côté client.

**Hors périmètre immédiat** : parité exacte des raccourcis clavier avec `pi` ; support `transport` sse/ws côté provider (déjà géré différemment par Akasha).

### 2. Fork de session dans Code Studio (branche depuis un message) — **Livré**

**Référence Pi** : `coding-agent` — `/fork`, `/tree`, sessions JSONL avec `parentId`.

**Statut (2026-06)** : v1 implémentée — action UI « Fork à partir d'ici », nouvelle tâche + `session_id` fille, événement `session_fork_created` côté daemon. Voir [`docs/SESSION_FORK_SPEC.md`](../../../../akasha-code-studio/docs/SESSION_FORK_SPEC.md) (cases cochées v1).

**Hors périmètre v1 (reste ouvert)** : arbre interactif complet type `/tree` Pi ; export HTML gist ; fusion de branches.

## Axes reportés (justification courte)

| Axe | Report |
|-----|--------|
| **Handoff modèle explicite** (reprendre le transcript avec un autre modèle) | Utile ; dépend d'une sérialisation de contexte stable et de l'UI routeur — **phase suivante** après file steering (fork livré). |
| **Streaming JSON partiel des tool calls** (`toolcall_delta`) | Voir [pi_mono_backend_parity_check.md](../integrations/pi_mono_backend_parity_check.md) : aujourd'hui les outils sont surtout dérivés du texte assistant final ; évolution **backend + contrat événements**. |
| **CSI 2026 / TUI différentiel** (`pi-tui`) | Gain UX terminal ; **faible priorité** vs file steering. |
| **RPC stdio JSONL** (`pi --mode rpc`) | HTTP + tâches couvrent l'intégration IDE ; RPC **optionnel** si partenaire IDE l'exige. |

## Références croisées

- Vérification backend (tokens, coût, streaming outils) : [pi_mono_backend_parity_check.md](../integrations/pi_mono_backend_parity_check.md)
- Contrat d'événements client (cible SSE/WebSocket) : [agent_client_event_contract.md](../runtime/agent_client_event_contract.md)
