# Tâches déclenchées par événement

Spécification du type **event trigger** : création automatique de tâches agent en réaction à un événement externe ou interne.

## Modèle de données

Tables SQLite (`event_triggers`, `event_trigger_runs`) dans le store principal.

| Champ | Description |
|-------|-------------|
| `trigger_type` | `webhook`, `task_failed`, `filesystem`, `model_available`, `daemon_error` |
| `filter` | JSON — critères de correspondance (provider, path, pattern…) |
| `prompt_template` | Texte avec variables `{{field}}` et `{{payload.field}}` |
| `cooldown_seconds` | Anti-rebond entre deux déclenchements |
| `execution_mode` | `direct`, `guided`, `orchestrated` (optionnel) |

## API

| Méthode | Chemin | Action |
|---------|--------|--------|
| GET | `/api/event-triggers` | Liste |
| POST | `/api/event-triggers` | Créer |
| GET | `/api/event-triggers/:id` | Détail |
| PUT | `/api/event-triggers/:id` | Modifier |
| DELETE | `/api/event-triggers/:id` | Supprimer |
| POST | `/api/event-triggers/:id/test` | Dry-run |
| POST | `/api/event-triggers/:id/pause` | Désactiver |
| POST | `/api/event-triggers/:id/resume` | Réactiver |

## Déclencheurs

### Webhook (`webhook`)

`POST /api/automation/webhook` — après vérification HMAC, dispatch vers les triggers `webhook` actifs dont le filtre matche le payload.

### Tâche échouée (`task_failed`)

Subscriber EventBus sur `task_failed` → évalue les triggers (ex. analyse d'erreur).

### Filesystem (`filesystem`)

Poller 5 s sur `filter.path` — événements `created` / `modified`.

### Modèle disponible (`model_available`)

Poller 5 min — diff catalogue `LLMRouter::list_models_from_config()`.

### Erreur daemon (`daemon_error`)

Subscriber EventBus sur `progress_update` dont le message contient « error » ou « panic ».

## UI

- Task Center : bouton **Créer** → onglet **Événement**
- Sidebar droite : liste des déclencheurs actifs

## Références

- [automation-webhooks.md](dev/integrations/automation-webhooks.md)
- [37_scheduler_design.md](37_scheduler_design.md)
- Code : `crates/akasha-store/src/event_triggers.rs`, `crates/akasha-daemon/src/event_trigger_engine.rs`
