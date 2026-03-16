# Gateway Layer — normalisation des entrées (spec 48)

**Statut** : Implémenté (Phase 1 AI OS).

## 1. Objectif

Une seule couche qui reçoit tous les canaux (Slack, Teams, Discord, Telegram, API) et produit un **message envelope** standard. Toute la logique « créer tâche + envoyer à l’agent » est centralisée dans `handle_envelope`, appelée par chaque adapter. Les routes HTTP restent inchangées (rétrocompatibilité).

## 2. Flux

```
Canaux (Slack / Teams / Discord / Telegram / POST /api/message)
        │
        ▼
  Adapter par canal (parse payload, vérif auth si besoin)
        │
        ▼
  Construction MessageEnvelope (channel_type, session_id, raw_message, …)
        │
        ▼
  handle_envelope(main_agent, store_path, envelope)
        │
        ▼
  Main Agent (création tâche, routage Direct / Guidé / Orchestré)
        │
        ▼
  Event Bus + TaskStore + Orchestrator / Conversation worker
```

## 3. Types

### MessageEnvelope

Structure normalisée produite par les adapters :

| Champ              | Type                    | Description                                                                 |
|--------------------|-------------------------|-----------------------------------------------------------------------------|
| `channel_type`     | `ChannelType`           | `api` \| `slack` \| `teams` \| `discord` \| `telegram`                     |
| `channel_id`       | `Option<String>`        | Identifiant canal (ex. conversation Teams, channel Slack)                   |
| `session_id`       | `String`                | Session pour mémoire court terme (ex. `day-YYYY-MM-DD`, `slack`, `teams`)   |
| `user_id`          | `Option<String>`        | Identifiant utilisateur si disponible                                      |
| `workspace_id`     | `Option<String>`        | Workspace si pertinent                                                     |
| `raw_message`      | `String`                | Message utilisateur (éventuellement enrichi par l’adapter)                  |
| `image_data_urls`  | `Option<Vec<String>>`   | Pièces jointes images (data URL) pour modèles vision                        |
| `priority`         | `TaskPriority`         | Priorité tâche (UserNormal / UserHigh)                                      |

### handle_envelope

- **Signature** : `handle_envelope(main_agent, store_path, envelope) -> Result<Uuid>`.
- **Effet** : crée la tâche (TaskStore), émet les événements (UserRequestReceived, TaskCreated, etc.), envoie la tâche à l’orchestrateur ou au conversation worker selon le mode (Direct / Guidé / Orchestré). Retourne le `task_id`.

## 4. Adapters

- **Slack** (`channels::slack`) : vérification signature, parse form, `MessageEnvelope::slack("slack", text)`, `handle_envelope`, puis polling + post réponse Slack.
- **Teams** (`channels::teams`) : validation JWT, parse activity, `MessageEnvelope::teams("teams", text, Some(conversation_id))`, `handle_envelope`, puis polling + reply Bot Framework.
- **API** (`POST /api/message`) : parse body (message, session_id, attachments, priority), logique onboarding/reconnect, puis `MessageEnvelope::api(session_id, message, image_data_urls, priority)` et `handle_envelope`.

Discord et Telegram appellent aujourd’hui directement `POST /api/message` côté client ; ils passent donc déjà par le même chemin normalisé (envelope API).

## 5. Fichiers

| Composant   | Fichier |
|------------|---------|
| Gateway    | `akasha-daemon/src/gateway.rs` |
| Adapters   | `akasha-daemon/src/channels/slack.rs`, `channels/teams.rs` |
| API message| `akasha-daemon/src/api.rs` (bloc `POST /api/message`) |

## 6. Références

- [05_agent_architecture.md](05_agent_architecture.md) — Main Agent, Orchestrator.
- [36_ui_architecture.md](36_ui_architecture.md) — Transport et onglets.
