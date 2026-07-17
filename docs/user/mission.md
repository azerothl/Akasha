# Mission autonome

La **mission autonome** permet à Akasha de poursuivre un objectif de fond via des **heartbeats** périodiques.

## Interface

Onglet **Mission** (application desktop) ou fichier **`autonomous_mission.yaml`** dans le data_dir.

## Champs principaux (`autonomous_mission.yaml`)

| Champ | Description |
|-------|-------------|
| `enabled` | Active la mission |
| `objective` | Objectif en langage naturel |
| `global_context` | Contexte injecté à chaque cycle |
| `horizon` | `short`, `medium`, `long` |
| `heartbeat_interval_minutes` | Fréquence des cycles |
| `report_dir` | Dossier des rapports (relatif au data_dir) |
| `session_id` | Session chat liée (mode sans questions) |
| `status` | `active`, `paused`, etc. |

## API (daemon local)

- `GET` / `PUT /api/autonomous-mission` — lire / mettre à jour
- `POST /api/autonomous-mission/pause` | `/resume`
- `GET /api/autonomous-mission/events` — journal (`limit`, `since`)

Les rapports Markdown sont écrits sous le répertoire configuré après chaque heartbeat réussi.

## Life layer (packs planifiés)

En complément de la mission autonome, l’onglet **Calendrier → Récurrences** propose un panneau **Life layer** :

| Pack | Rôle |
|------|------|
| **Brief matinal** | Schedule quotidien ; le résultat est poussé sur Telegram (`AKASHA_TELEGRAM_NOTIFY_CHAT_ID`) via `POST /api/channels/notify` |
| **Pack nuit** | Passage nocturne avec rapport (skills / agenda / follow-ups) |
| **Langage naturel** | Phrase du type « chaque matin à 7h30, brief Telegram » → aperçu puis création (`POST /api/schedules/from-nl`, CLI `akasha schedule from-nl`) |

Prérequis brief canal : connecteur Telegram activé + chat id notify (Paramètres → Connecteurs).
