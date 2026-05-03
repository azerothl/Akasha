# Contrat d’événements agent (cible client) — alignement Pi / Akasha

Document **interne** : cible pour une exposition unifiée des événements (polling enrichi, **SSE**, ou **WebSocket**). Il ne définit pas un transport déjà livré ; il aligne la nomenclature sur [@mariozechner/pi-agent-core](https://github.com/badlogic/pi-mono/tree/main/packages/agent) tout en réutilisant le modèle existant `EventType` dans `akasha-core`.

## Contexte aujourd’hui

- Bus interne : `akasha_core::EventType` (`crates/akasha-core/src/events.rs`) — inclut `ProgressUpdate`, `ToolCallStarted`, `ToolCallFinished`, cycle de vie tâche, etc.
- API HTTP : `GET /api/tasks/:id/events` agrège persistance + cache (`get_task_events` dans `api.rs`).
- Le client **Code Studio** / **Tauri** consomme surtout `GET /api/tasks/:id` (progress + champs agrégés) et les events au besoin.

## Cartographie Pi → Akasha (sémantique)

| Événement Pi (`Agent.subscribe`) | Équivalent Akasha (cible / existant) | Notes |
|----------------------------------|--------------------------------------|--------|
| `agent_start` | Agrégat client OU `task_started` | Pi : début de `prompt()`. Akasha : tâche conversation démarrée. |
| `agent_end` | `task_completed` / `task_failed` / `task_cancelled` | Fin de boucle ; payload déjà standardisé côté bus. |
| `turn_start` | *(nouveau alias client)* `llm_turn_start` | Optionnel : émis au début de chaque appel LLM dans la boucle outils. |
| `turn_end` | `llm_turn_end` | Après réponse assistant + exécution outils du tour. |
| `message_start` / `message_end` | `session_state_snapshot` ou messages via progress | Aujourd’hui le texte assistant arrive souvent via **progress** ; harmoniser si SSE. |
| `message_update` (text delta) | `progress_update` avec `message` incrémental OU type dédié `assistant_text_delta` | **Gap** : deltas fins non garantis sur tous les chemins. |
| `tool_execution_start` | `tool_call_started` | Déjà dans `EventType` ; garantir émission systématique depuis `run_message_via_llm` / dispatch. |
| `tool_execution_update` | *(optionnel)* `tool_call_progress` | Si un outil stream (ex. terminal read) ; payload `partial`. |
| `tool_execution_end` | `tool_call_finished` | Après résultat outil. |
| `toolResult` (message) | Contenu dans progress ou événement structuré | Aujourd’hui souvent résumé dans la trace textuelle. |

## Enveloppe JSON recommandée (future SSE/WebSocket)

Chaque ligne ou frame :

```json
{
  "schema_version": 1,
  "task_id": "uuid",
  "session_id": "string",
  "at": "2026-04-29T12:00:00Z",
  "kind": "tool_call_started",
  "payload": { }
}
```

- `kind` : reprendre les chaînes **`EventType::as_str()`** (`snake_case`) pour éviter la duplication de vocabulaire.
- `payload` : identique aux payloads déjà publiés sur le bus (où applicable).

## Profil live retenu (SSE + fallback polling)

Pour les clients UI/CLI, le mode live recommandé est:

1. **SSE prioritaire** sur `GET /api/events` (bus global).
2. Filtrage client par `correlation_id == <task_id>`.
3. **Fallback polling** sur `GET /api/tasks/:id/events` quand SSE n'est pas disponible (proxy, navigateur ancien, coupure réseau, etc.).

Contraintes client:

- L'ordre d'affichage est trié sur `at`/`timestamp` croissant.
- Les `kind` inconnus sont ignorés sans erreur.
- La reconnexion SSE ne doit pas supprimer l'historique déjà reçu pour la tâche.
- Le polling fallback doit être borné (intervalle >= 800 ms recommandé) et doit reprendre le flux sans doublons visuels.

Ce profil garantit une UX live proche d'un `run.stream()` tout en restant compatible avec l'API HTTP existante.

## Files « steering » / « follow-up » (extension)

Lorsque l’API file d’attente (voir [pi_mono_alignment_priorities.md](../roadmap/pi_mono_alignment_priorities.md)) sera en place, ajouter des kinds **client-visibles** :

| `kind` | Description |
|--------|-------------|
| `user_steering_queued` | Message utilisateur mis en file *steering*. |
| `user_follow_up_queued` | Message utilisateur mis en file *follow-up*. |
| `user_queue_flushed` | Files vidées (annulation). |

Ces kinds peuvent rester **sans** équivalent Pi strict ; ils documentent la politique Akasha.

## Compatibilité et versioning

- Incrémenter `schema_version` lors d’un breaking change sur `payload`.
- Les clients doivent **ignorer** les `kind` inconnus (forward-compatible).

## Références code

- `crates/akasha-core/src/events.rs` — liste `EventType`.
- `crates/akasha-daemon/src/agents/progress_subscriber.rs` — bus → caches API.
- `crates/akasha-daemon/src/api.rs` — `get_task_events`, `get_task_status`.
