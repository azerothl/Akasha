# Spécification technique — protocole Companion LAN (daemon)

Document **normatif pour l’implémentation actuelle** du daemon Akasha (`akasha-daemon`). Tout ce qui suit est dérivé du code source ; les écarts connus sont listés en fin de document.

| Public | Document |
|--------|----------|
| Utilisateur / opération | [user/companion-lan.md](user/companion-lan.md) |
| Développeur client Companion | Ce fichier |
| Registre produit | [`spec/feature_evolution_tracking.md`](../spec/feature_evolution_tracking.md) |

**Hors périmètre livré :** firmware **ESP32** (Freenove FNK0104, avatar) — statut **Planned** ; dépôt `Akasha_companion` non livré avec ce monorepo. Les routes **`/api/voice/*`** (STT/TTS) existent à part ; le snapshot Companion ne fait qu’indiquer si la voix est configurée.

**Sources de vérité (Rust) :**

- `crates/akasha-daemon/src/companion_discovery.rs` — découverte UDP
- `crates/akasha-daemon/src/api_routes_companion.rs` — routes HTTP `/api/companion/*`
- Démarrage discovery : `crates/akasha-daemon/src/daemon.rs` (appel à `spawn_companion_lan_discovery` avec le port API et `CARGO_PKG_VERSION`)

---

## Versionnage

| Niveau | Champ / mécanisme | Valeur actuelle |
|--------|-------------------|-----------------|
| Snapshot JSON | `schema_version` (entier) | **1** — incrémenter si la forme du snapshot change de façon incompatible |
| Daemon | `daemon.version` dans le snapshot | Version du crate `akasha-daemon` (`env!("CARGO_PKG_VERSION")`) |
| Découverte UDP | champ `version` dans la réponse | Même version crate daemon |
| API HTTP | pas de préfixe `/v1` dédié Companion | Routes fixes sous `/api/companion/…` |

Les clients doivent ignorer les champs JSON inconnus et traiter `schema_version` pour les évolutions futures.

---

## Transport et prérequis réseau

| Service | Port | Bind | Variable |
|---------|------|------|----------|
| API HTTP | **3876** (défaut) | `AKASHA_BIND` (défaut `127.0.0.1`) | `AKASHA_PORT` |
| Découverte UDP | **3877** | `0.0.0.0:3877` (indépendant de `AKASHA_BIND`) | — |

Pour joindre le daemon depuis le LAN, `AKASHA_BIND=0.0.0.0` est requis. La découverte UDP démarre au lancement du daemon ; en cas d’échec de bind UDP, l’API HTTP reste disponible (log : `companion UDP discovery bind failed`).

Répertoire de données : parent du fichier base SQLite du daemon (souvent `~/akasha` via `AKASHA_DATA_DIR`). Fichiers Companion :

- `companion_devices.json`
- `companion_presence.json`

---

## Découverte UDP (port 3877)

### Probe (client → daemon)

- Datagramme UDP vers le port **3877** (unicast ou broadcast).
- Corps interprété en UTF-8 (octets invalides → chaîne vide).
- Le message est **accepté** si la chaîne trimée **contient** la sous-chaîne `AKASHA_DISCOVER` (pas besoin d’égalité stricte sur tout le paquet).
- Taille de lecture côté daemon : buffer **256** octets (troncature possible si probe plus long).

Constantes code : `DISCOVER_PORT = 3877`, `DISCOVER_MAGIC = "AKASHA_DISCOVER"`.

### Réponse (daemon → client)

- Réponse **unicast** vers l’adresse source du probe (`send_to` vers `peer`).
- Corps : JSON UTF-8, objet à **trois** clés :

```json
{
  "service": "akasha",
  "port": 3876,
  "version": "0.11.1"
}
```

| Champ | Type | Sémantique |
|-------|------|------------|
| `service` | string | Toujours `"akasha"` |
| `port` | number | Port HTTP API au moment du démarrage (`api_port` passé au spawn) |
| `version` | string | Version du package daemon |

**Non fourni par le protocole :** adresse IP de l’hôte — le client utilise l’IP source du datagramme de réponse ou une IP configurée manuellement.

---

## Authentification HTTP (état actuel)

Toutes les routes dont le chemin commence par `/api/companion` passent par `companion_auth_ok` **avant** le routage.

| En-tête `Authorization` | Comportement |
|-------------------------|--------------|
| Absent | Routes de données refusées (**401** `{"error":"invalid_token"}`) ; `POST /pair` vérifie son secret d’appairage |
| `Bearer <token>` ou `bearer <token>` | Les routes de données acceptent uniquement un token enregistré dans `companion_devices.json` |
| Bearer présent mais token absent du fichier | **401** `{"error":"invalid_token"}` |

Les valeurs `AKASHA_API_TOKEN` / `AKASHA_TOKEN` ne sont pas reconnues comme tokens Companion. Le secret de pairage autorise uniquement l’amorçage de `POST /pair` ; les autres routes nécessitent le token de l’appareil obtenu après un appairage valide.

---

## Routes HTTP

Préfixe commun : `/api/companion`. Méthodes non implémentées pour un chemin donné → **404** `{"error":"companion_route_not_found"}`.

Réponses JSON : format HTTP construit par `json_response` (statut + corps JSON).

### `POST /api/companion/pair`

Appairage (ou ré-appairage) d’un `device_id`.

**Corps JSON (tous champs optionnels sauf contraintes ci-dessous) :**

| Champ | Type | Obligatoire | Défaut / règle |
|-------|------|-------------|----------------|
| `device_id` | string | oui | Trim ; vide → **400** `device_id_required` |
| `name` | string | non | Défaut `"companion"` |
| `secret` | string | oui | Doit correspondre à `AKASHA_COMPANION_PAIR_SECRET` |

**Secret d’appairage :**

- Si `AKASHA_COMPANION_PAIR_SECRET` est absent ou vide → **503** `{"error":"pairing_disabled"}` ; aucun appareil n’est enregistré.
- Si le secret fourni est incorrect → **403** `{"error":"pair_secret_invalid"}`.

**Succès 200 :**

```json
{
  "token": "cmp_<uuid32_hex>",
  "device_id": "<device_id>"
}
```

- `token` : préfixe `cmp_` + UUID v4 sans tirets (`uuid::Uuid::new_v4().simple()`).
- Persistance : voir `companion_devices.json` ci-dessous. Même `device_id` → remplace `token`, `name`, `paired_at`.

**Erreur disque :** **500** `{"error":"<message>"}`.

### `GET /api/companion/snapshot`

État agrégé pour l’UI / le client Companion.

**Succès 200 :** objet JSON (forme imposée par `build_snapshot`, testée par `snapshot_shape_has_required_keys`).

```json
{
  "schema_version": 1,
  "daemon": {
    "ok": true,
    "version": "<CARGO_PKG_VERSION>"
  },
  "llm": {
    "provider": "<string|null>",
    "model": "<string|null>"
  },
  "voice": {
    "stt": false,
    "tts": false
  },
  "tasks": {
    "active": 0
  },
  "notify": {
    "unread": 0,
    "headline": null
  },
  "avatar_hint": "idle",
  "presence_mode": "ptt",
  "vad_enabled": false,
  "last_event_ts": null,
  "presence": {
    "enabled": false,
    "threshold_rms": 0.035,
    "min_speech_ms": 400,
    "max_speech_ms": 10000,
    "silence_hang_ms": 700,
    "cooldown_ms": 2500,
    "quiet_hours": "",
    "quiet_now": false
  }
}
```

**Sémantique (code) :**

| Champ | Calcul |
|-------|--------|
| `llm.provider` / `llm.model` | Route primaire tâche `"conversation"` ; chaînes vides → `null` |
| `voice.stt` / `voice.tts` | Config voix (`voice_router.yaml`) via `load_voice_config` |
| `tasks.active` | Compte des tâches `Pending`, `Queued`, `Running`, `WaitingUserInput` |
| `notify.unread` / `notify.headline` | **Constantes** `0` et `null` (voir écart #173) |
| `avatar_hint` | `"notify"` si `unread > 0`, sinon `"idle"` (donc toujours `"idle"` aujourd’hui) |
| `presence.quiet_now` | Heures calmes `quiet_hours` (heure locale daemon) |
| `vad_enabled` | `policy.enabled && !quiet_now && voice.stt` |
| `presence_mode` | `"hands_free"` si `vad_enabled`, sinon `"ptt"` |
| `last_event_ts` | `ts` du dernier `last_presence_event` dans `companion_devices.json`, ou absent → `null` |
| `presence.*` | Copie de la politique chargée (fichier + override env VAD) |

**Écart connu ([#173](https://github.com/azerothl/Akasha/issues/173)) :** `notify.unread` et `notify.headline` ne sont alimentés par aucune source métier.

### `GET /api/companion/presence/config`

**Succès 200 :**

```json
{
  "enabled": false,
  "threshold_rms": 0.035,
  "min_speech_ms": 400,
  "max_speech_ms": 10000,
  "silence_hang_ms": 700,
  "cooldown_ms": 2500,
  "quiet_hours": "",
  "quiet_now": false,
  "effective_enabled": false
}
```

`effective_enabled` = `enabled && !quiet_now` (sans exiger STT ; contrairement à `vad_enabled` du snapshot).

### `POST /api/companion/presence/config`

Mise à jour **partielle** : seuls les champs présents dans le corps JSON sont modifiés.

| Champ | Type | Clamp (si présent) |
|-------|------|-------------------|
| `enabled` | bool | — |
| `threshold_rms` | number | `[0.001, 0.5]` |
| `min_speech_ms` | number | `[100, 5000]` |
| `max_speech_ms` | number | `[1000, 15000]` |
| `silence_hang_ms` | number | `[100, 3000]` |
| `cooldown_ms` | number | `[500, 30000]` |
| `quiet_hours` | string | Trim ; format `"HH:MM-HH:MM"` heure locale ; vide = désactivé |

**Succès 200 :** corps = objet `PresencePolicy` sérialisé (sans `quiet_now` ni `effective_enabled`).

**Erreur écriture fichier :** **500** `{"error":"<message>"}`.

**Override au chargement :** `AKASHA_COMPANION_VAD_ENABLED` = `1` / `true` force `enabled=true` ; `0` / `false` force `enabled=false`.

### `POST /api/companion/presence/event`

Le client signale un événement de présence / VAD.

**Corps JSON :**

| Champ | Type | Obligatoire |
|-------|------|-------------|
| `event` | string | oui |
| `device_id` | string | non (défaut `""`) |
| `detail` | string | non |

| `event` (valeurs autorisées) |
|------------------------------|
| `vad_start`, `vad_end`, `speech_dropped`, `speech_sent`, `hands_free_on`, `hands_free_off`, `error` |

- `event` vide → **400** `event_required`
- `event` hors liste → **400** `event_unknown`

**Succès 200 :**

```json
{
  "ok": true,
  "ts": "<RFC3339 UTC>"
}
```

Persiste `last_presence_event` dans `companion_devices.json` :

```json
{
  "event": "<event>",
  "device_id": "<device_id>",
  "ts": "<RFC3339 UTC>",
  "detail": "<string|null>"
}
```

---

## Persistance `companion_devices.json`

Schéma Rust `CompanionDevicesFile` :

```json
{
  "devices": [
    {
      "device_id": "aa:bb:cc:dd:ee:ff",
      "token": "cmp_…",
      "name": "mon-appareil",
      "paired_at": "2026-10-18T12:00:00+00:00"
    }
  ],
  "last_presence_event": {
    "event": "vad_start",
    "device_id": "aa:bb:cc:dd:ee:ff",
    "ts": "2026-10-18T12:01:00+00:00",
    "detail": "rms=0.1"
  }
}
```

- `devices` : liste ; absence ou JSON invalide → fichier traité comme vide.
- `last_presence_event` : optionnel ; mis à jour par `POST …/presence/event` (écrasement du dernier événement uniquement).

---

## Persistance `companion_presence.json`

Schéma `PresencePolicy` (valeurs par défaut si fichier absent) :

```json
{
  "enabled": false,
  "threshold_rms": 0.035,
  "min_speech_ms": 400,
  "max_speech_ms": 10000,
  "silence_hang_ms": 700,
  "cooldown_ms": 2500,
  "quiet_hours": ""
}
```

**Heures calmes :** `quiet_hours` = `"HH:MM-HH:MM"` en heure locale du daemon. Plage même jour : `start <= end` → actif si `cur >= start && cur < end`. Plage sur minuit : `start > end` → actif si `cur >= start || cur < end`. Format invalide → `quiet_now` faux.

---

## Table des erreurs JSON (`error`)

| Code HTTP | `error` | Route / condition |
|-----------|---------|-------------------|
| 401 | `invalid_token` | Bearer fourni et non reconnu |
| 403 | `pair_secret_invalid` | `POST /pair` secret incorrect |
| 503 | `pairing_disabled` | `POST /pair` sans secret configuré sur le daemon |
| 401 | `invalid_token` | Route de données sans token d’appareil appairé valide |
| 400 | `device_id_required` | `POST /pair` sans `device_id` |
| 400 | `event_required` | `POST /presence/event` sans `event` |
| 400 | `event_unknown` | `POST /presence/event` event hors liste |
| 404 | `companion_route_not_found` | Chemin ou méthode non gérée sous `/api/companion` |
| 500 | *(chaîne message)* | Échec `write` JSON disque (`pair`, `presence/config`) |

---

## Variables d’environnement (Companion)

| Variable | Effet |
|----------|--------|
| `AKASHA_BIND` / `AKASHA_PORT` | Écoute HTTP |
| `AKASHA_COMPANION_PAIR_SECRET` | Secret obligatoire de `POST /pair` ; vide ou absent désactive l’appairage |
| `AKASHA_API_TOKEN` / `AKASHA_TOKEN` | Non acceptés comme tokens Companion |
| `AKASHA_COMPANION_VAD_ENABLED` | Override `enabled` au chargement de `companion_presence.json` |

---

## Écarts connus et évolutions

| ID | Sujet | État code |
|----|--------|-----------|
| [#172](https://github.com/azerothl/Akasha/issues/172) | Routes Companion exigent un token d’appareil ; appairage fermé sans secret | Corrigé : les routes de données refusent l’absence de token ; `POST /pair` exige le secret configuré |
| [#173](https://github.com/azerothl/Akasha/issues/173) | `notify.unread` / `notify.headline` non alimentés | Toujours `0` / `null` dans `build_snapshot` |
| — | Matériel ESP32 / avatar | **Planned** (registre features) |

---

## Checklist client minimal

1. (Optionnel) Probe UDP `AKASHA_DISCOVER` → port 3876 + version.
2. `POST /api/companion/pair` avec `device_id` et le secret configuré ; l’appairage échoue avec `503 pairing_disabled` si aucun secret n’est défini.
3. Conserver le token retourné puis l’envoyer comme Bearer sur chaque route de données Companion.
4. Si VAD côté client : lire `GET /api/companion/presence/config`, ajuster via `POST …/config`, remonter les événements via `POST …/event`.

Pour l’exploitation et le dépannage réseau : [user/companion-lan.md](user/companion-lan.md).
