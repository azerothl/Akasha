# CalDAV integration — Akasha

**Statut:** Phase 1–3 livrées (ICS, sync pull sidecar, outbox bidirectionnelle)  
**Architecture:** Hybride — daemon (cache + API + outils agent) + sidecar `Akasha_plugins/caldav-channel`

---

## Modèle de données

Table SQLite `external_calendar_events` (via `ExternalCalendarStore`) — **séparée** des `schedules` opérateur.

| Champ | Description |
|-------|-------------|
| `account_id` | Compte CalDAV ou compte ICS par défaut |
| `uid` | UID iCalendar (unique par compte) |
| `href` / `etag` | Identité CalDAV (sync) |
| `source` | `ics_import` \| `caldav` \| `local` |

Comptes : table `caldav_accounts`. Outbox push : `caldav_outbox`.

---

## OAuth (Google, Microsoft)

En plus du mot de passe d'application (CalDAV), **Google Calendar** et **Outlook / Microsoft 365** supportent une connexion **OAuth** lorsque l'administrateur a enregistré des identifiants client dans le vault :

| Clé vault | Usage |
|-----------|--------|
| `google_calendar_oauth_client_id` | Client OAuth Google Cloud |
| `google_calendar_oauth_client_secret` | Secret client Google |
| `microsoft_calendar_oauth_client_id` | App registration Azure |
| `microsoft_calendar_oauth_client_secret` | Secret client Microsoft |

Redirect URI à déclarer chez le fournisseur : `http://127.0.0.1:3876/api/calendar/oauth/callback` (ou port `AKASHA_PORT`).

### API OAuth

| Méthode | Route | Description |
|---------|-------|-------------|
| GET | `/api/calendar/oauth/config` | Fournisseurs OAuth configurés |
| POST | `/api/calendar/oauth/start` | `{ provider_id, label? }` → `{ auth_url, state }` |
| GET | `/api/calendar/oauth/callback` | Callback navigateur (code + state) |
| GET | `/api/calendar/oauth/status?state=` | Polling UI après ouverture du navigateur |

Tokens stockés dans le vault : `caldav_<account_id>_oauth`. Compte créé avec `auth_method: oauth`.

**Limite actuelle :** la sync sidecar utilise encore l'auth CalDAV (mot de passe) ; le support OAuth côté sidecar (Google Calendar API / Microsoft Graph) est le prochain jalon.

---

## API HTTP

| Méthode | Route | Description |
|---------|-------|-------------|
| GET | `/api/calendar/events?from=&to=` | Runs scheduler + tâches + **`type: external`** |
| GET | `/api/calendar/ics?from=&to=&account_id=` | Export VEVENT |
| POST | `/api/calendar/ics` | Import body `.ics` ou JSON `{ "ics": "..." }` |
| GET | `/api/calendar/providers` | Connecteurs prédéfinis (Google, Outlook, iCloud, …) |
| GET | `/api/calendar/accounts` | Liste comptes |
| POST | `/api/calendar/accounts` | `{ label?, url, username, provider_id?, calendar_path? }` + mot de passe via `POST /api/vault` |
| DELETE | `/api/calendar/accounts/:id` | Supprimer compte + événements |
| GET | `/api/calendar/sync-status` | Statut sidecar (`calendar_sync_status.json`) |
| POST | `/api/calendar/external/sync` | Sidecar — header `X-Akasha-Calendar-Token` si `AKASHA_CALENDAR_SYNC_TOKEN` |
| POST | `/api/calendar/events` | Créer événement local + outbox |
| PUT | `/api/calendar/events/:id` | Mettre à jour + outbox |
| DELETE | `/api/calendar/events/:id` | Soft-delete + outbox |
| GET | `/api/calendar/external/outbox` | Debug opérateur |
| GET | `/api/calendar/external/outbox/pending` | Sidecar — mutations en attente |
| POST | `/api/calendar/external/outbox/ack` | Sidecar — acquitter outbox |

---

## Outils agent

| Outil | Policy |
|-------|--------|
| `calendar_query` | `calendar_read_enabled` (défaut **true**) |
| `calendar_create` / `update` / `delete` | `calendar_write_enabled` (défaut **false**) + `require_approval` recommandé |

---

## Sidecar

Voir [`Akasha_plugins/caldav-channel/README.md`](../../../../Akasha_plugins/caldav-channel/README.md).

1. `POST /api/calendar/accounts` → noter `id`
2. Vault : `caldav_<id>_password`
3. Lancer `akasha-caldav-sidecar` avec `CALDAV_*` et `CALDAV_ACCOUNT_ID`

---

## Limites ICS MVP

- VEVENT simple, RRULE texte brut
- Pas d'alarmes VALARM
- Timezones : UTC / format `Z` privilégié

---

## Sécurité

- Credentials **vault only** — pas dans `connectors.env` en clair
- `AKASHA_CALENDAR_SYNC_TOKEN` pour `/api/calendar/external/*`
- `AKASHA_CALENDAR_REDACT_LOGS=1` — réduire logs titres (future)

---

## Diagnostic

`akasha doctor` (daemon actif) expose :

| Check | Description |
|-------|-------------|
| `calendar_accounts` | Comptes CalDAV, dernière erreur sync |
| `calendar_external_events` | Nombre d'événements externes en cache |
| `calendar_vault_passwords` | Présence `caldav_<id>_password` par compte |

Sidecar : `CALDAV_SIDECAR_ENABLED=1` dans `connectors.env` (activation opérateur, sans mot de passe en clair).
