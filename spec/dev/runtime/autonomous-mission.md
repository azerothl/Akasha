# Mission autonome — cycle, contrats, observabilité

**Statut** : Implemented (daemon + UI Mission). Complète `docs/user/mission.md` et `spec/autonomous_mission.example.yaml`.

## Cycle (heartbeat)

1. Le daemon charge `autonomous_mission.yaml` depuis le data_dir (`enabled`, `status`, `heartbeat_interval_minutes`, `objective`, …).
2. Tant que `enabled` et `status = active`, un **heartbeat** périodique crée une tâche orchestrée (agent de première étape configurable, souvent `project_manager`).
3. L’orchestrateur peut **déléguer** selon les rôles décrits dans l’objectif / le contexte.
4. Un rapport Markdown est écrit sous `report_dir` (relatif au data_dir) après un cycle réussi.
5. `POST /api/autonomous-mission/pause` | `/resume` bascule `status` sans effacer l’objectif.

Persistance : snapshot + **journal d’événements** SQLite (`AutonomousMissionStore` dans `akasha-store`).

## Contrats de réponse

Les agents production / QA peuvent clôturer par un bloc JSON (`status`, `summary`, `files_created`, `issues_found`, optionnel `blocked`) — voir `spec/agent_contracts/` et `parse_contract_from_response`.

- Validation « contrat significatif » avant strip pour le résumé UI (`strip_trailing_contract`).
- Violation soft : événement `contract_violation` sur la timeline de la tâche racine ; la réponse reste agrégée.

Alignement partiel avec les contrats client : `spec/dev/runtime/agent_client_event_contract.md`.

## Observabilité

| Surface | Contenu |
|---------|---------|
| `GET /api/autonomous-mission` | État courant (YAML fusionné) |
| `GET /api/autonomous-mission/events?limit=&since=` | Journal paginé (RFC3339) |
| Onglet **Mission** (desktop) | Config, pause/reprise, activité |
| Rapports Markdown | Fichiers sous `report_dir` |
| Timeline tâche | Progression / contrats / erreurs d’outils |

Le chat peut réutiliser le même `session_id` que la mission pour hériter du mode « sans questions inutiles ».

## Life layer (complément)

Packs planifiés (brief matinal Telegram, overnight, NL schedules) : panneau Calendrier → Récurrences ; pas un remplacement du heartbeat mission — voir `docs/user/mission.md`.

## Références code

- Routes : `crates/akasha-daemon/src/api_routes_mission.rs`
- Exemple config : `spec/autonomous_mission.example.yaml`
- Contrats agent : `spec/agent_contracts/README.md`
