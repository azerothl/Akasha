# Companion LAN (API daemon)

Cette page décrit ce que le **daemon Akasha** expose déjà sur le réseau local pour un **client Companion** (logiciel ou firmware futur). Elle ne couvre pas un appareil ESP32 livré : le matériel **Freenove FNK0104 / avatar** reste **prévu (Planned)** dans le registre des fonctionnalités.

## Ce que vous accomplissez

- Rendre l’API HTTP du daemon joignable depuis d’autres machines du LAN (pas seulement `127.0.0.1`).
- Permettre à un client de **découvrir** le port API via **UDP 3877**.
- **Appairer** un identifiant d’appareil et obtenir un jeton stocké dans `companion_devices.json`.
- Consulter un **snapshot** d’état (LLM, voix, tâches, mode présence) et, si besoin, configurer la **politique VAD** (fichier `companion_presence.json`).

Les routes voix (`/api/voice/*`) existent séparément pour STT/TTS ; le snapshot Companion indique seulement si la voix est configurée. **Spécification technique (protocole, schémas JSON, erreurs)** : [COMPANION_SPEC.md](../COMPANION_SPEC.md).

## Prérequis

| Élément | Détail |
|--------|--------|
| Daemon | `akasha start` (ou `akasha start --foreground`) en cours d’exécution |
| Réseau | PC et client sur le **même LAN** ; pare-feu autorisant le trafic entrant vers le daemon |
| Bind LAN | Par défaut le daemon écoute sur **`127.0.0.1`** : définir **`AKASHA_BIND=0.0.0.0`** pour accepter les connexions LAN |
| Port API | **`3876`** par défaut (`AKASHA_PORT`) — le même port doit être utilisé côté client |
| Data dir | Fichiers sous `~/akasha` (ou votre `AKASHA_DATA_DIR`) : `companion_devices.json`, `companion_presence.json` |
| Pairing (recommandé) | Variable **`AKASHA_COMPANION_PAIR_SECRET`** : secret partagé pour `POST /api/companion/pair` |

Découverte UDP : le daemon écoute **`0.0.0.0:3877`** dès le démarrage (indépendamment de `AKASHA_BIND` pour l’HTTP).

## Étapes

### 1. Activer l’écoute LAN

Dans `akasha.env` (data_dir) ou l’environnement du service :

```bash
export AKASHA_BIND=0.0.0.0
export AKASHA_PORT=3876
# Recommandé sur un réseau partagé :
export AKASHA_COMPANION_PAIR_SECRET="votre-secret-long-et-aleatoire"
```

Redémarrez le daemon (`akasha stop` puis `akasha start`). Vérifiez les logs : message du type « Daemon listening » avec `bind=0.0.0.0` et `port=3876`, et « Companion LAN discovery listening » sur le port **3877**.

### 2. Découvrir le daemon (UDP)

Le client envoie un datagramme UDP contenant la chaîne **`AKASHA_DISCOVER`** (par exemple en broadcast vers le port **3877**).

Réponse JSON unicast du daemon (exemple) :

```json
{"service":"akasha","port":3876,"version":"0.11.0"}
```

Le champ `port` est le port HTTP API actuel ; `version` est la version du package daemon.

Test manuel depuis une autre machine (remplacez `IP_DU_PC`) :

```bash
echo -n 'AKASHA_DISCOVER' | nc -u -w1 IP_DU_PC 3877
```

### 3. Appairer un appareil

`POST /api/companion/pair` — corps JSON :

| Champ | Obligatoire | Description |
|-------|-------------|-------------|
| `device_id` | oui | Identifiant stable (ex. adresse MAC, UUID matériel) |
| `name` | non | Libellé affiché (défaut : `companion`) |
| `secret` | si secret configuré | Doit égaler `AKASHA_COMPANION_PAIR_SECRET` |

Exemple avec `curl` (depuis le LAN, `DAEMON=http://IP_DU_PC:3876`) :

```bash
curl -sS -X POST "$DAEMON/api/companion/pair" \
  -H 'Content-Type: application/json' \
  -d '{"device_id":"aa:bb:cc:dd:ee:ff","name":"mon-banc-test","secret":"votre-secret-long-et-aleatoire"}'
```

Réponse attendue (`200 OK`) :

```json
{"token":"cmp_…","device_id":"aa:bb:cc:dd:ee:ff"}
```

Le jeton est enregistré dans **`~/akasha/companion_devices.json`** (avec `name`, `paired_at`). Un nouvel appairage pour le même `device_id` **remplace** le jeton.

Si `AKASHA_COMPANION_PAIR_SECRET` est **vide**, l’appairage est **ouvert** sur le LAN (le daemon journalise un avertissement). En production domestique ou bureau, définissez toujours un secret.

### 4. Vérifier le snapshot et l’auth

`GET /api/companion/snapshot` renvoie l’état agrégé (`schema_version`: 1), notamment :

- `daemon.ok`, `daemon.version`
- `llm.provider`, `llm.model` (route conversation)
- `voice.stt`, `voice.tts` (config `voice_router.yaml`)
- `tasks.active` (tâches pending / queued / running / waiting user input)
- `presence_mode` : `hands_free` si VAD effectif, sinon `ptt`
- `vad_enabled`, bloc `presence` (paramètres + `quiet_now`)
- `notify.unread` (toujours **0** dans l’implémentation actuelle), `avatar_hint` (`idle` ou `notify` si unread > 0)

```bash
curl -sS "$DAEMON/api/companion/snapshot"
# Optionnel : Authorization: Bearer <token> après appairage
curl -sS -H "Authorization: Bearer cmp_…" "$DAEMON/api/companion/snapshot"
```

**Authentification « souple »** sur toutes les routes `/api/companion/*` :

- Pas d’en-tête `Authorization` → **accepté** (accès LAN implicite).
- `Authorization: Bearer …` présent → le jeton doit correspondre à un appareil appairé, ou à **`AKASHA_API_TOKEN`** / **`AKASHA_TOKEN`** s’ils sont définis ; sinon **`401`** `{"error":"invalid_token"}`.

### 5. (Optionnel) Présence / VAD côté daemon

Politique stockée dans **`companion_presence.json`** (désactivée par défaut) :

- `GET /api/companion/presence/config` — lecture + `effective_enabled` (respecte les heures calmes).
- `POST /api/companion/presence/config` — mise à jour partielle des champs (`enabled`, `threshold_rms`, `min_speech_ms`, `max_speech_ms`, `silence_hang_ms`, `cooldown_ms`, `quiet_hours` au format `HH:MM-HH:MM` heure locale).
- `POST /api/companion/presence/event` — le client signale des événements (`vad_start`, `vad_end`, `speech_dropped`, `speech_sent`, `hands_free_on`, `hands_free_off`, `error`) ; le dernier événement est conservé dans `companion_devices.json`.

Override au démarrage : **`AKASHA_COMPANION_VAD_ENABLED=1`** ou `true` force `enabled` dans la politique chargée.

## Ce que vous devez observer

| Étape | Succès | Échec typique |
|-------|--------|----------------|
| Bind LAN | `curl http://IP:3876/api/status` depuis une autre machine | Connexion refusée → `AKASHA_BIND` encore sur loopback |
| UDP 3877 | Réponse JSON avec `service`, `port`, `version` | Silence → pare-feu UDP, daemon arrêté, ou mauvais réseau |
| Pair | `200` + `token` commençant par `cmp_` | `403 pair_secret_invalid`, `400 device_id_required` |
| Snapshot | `daemon.ok: true`, champs `voice` / `tasks` cohérents | `401 invalid_token` si Bearer incorrect |
| Fichiers data_dir | `companion_devices.json` mis à jour après pair | Erreur disque → `500` sur pair |

## Erreurs courantes et dépannage

- **`Connection refused` sur le port 3876** : daemon non démarré, mauvais `AKASHA_PORT`, ou bind `127.0.0.1` uniquement → `AKASHA_BIND=0.0.0.0`.
- **Découverte UDP sans réponse** : ouvrir le port **3877/udp** sur l’hôte ; vérifier que le probe contient bien `AKASHA_DISCOVER` (sous-chaîne suffisante).
- **`403 pair_secret_invalid`** : aligner le champ JSON `secret` avec `AKASHA_COMPANION_PAIR_SECRET`.
- **`401 invalid_token`** : jeton révoqué par ré-appairage, ou utilisation de `AKASHA_API_TOKEN` / `AKASHA_TOKEN` qui ne correspond pas.
- **`400 event_unknown` / `event_required`** sur `/presence/event` : respecter la liste d’événements autorisés.
- **Bind discovery échoue au démarrage** : un autre processus occupe peut-être le port 3877 (log « companion UDP discovery bind failed ») — l’API HTTP reste disponible, mais la découverte automatique ne fonctionnera pas.

Commandes utiles : `akasha doctor`, page **Dépannage** de cette documentation.

## Sécurité (exposition LAN)

- Avec **`AKASHA_BIND=0.0.0.0`**, toute machine du LAN peut appeler l’API Companion (et, sans Bearer, les routes `/api/companion/*` sont accessibles).
- Sans **`AKASHA_COMPANION_PAIR_SECRET`**, n’importe qui sur le LAN peut appairer un `device_id` et obtenir un jeton.
- Les jetons dans `companion_devices.json` sont des secrets : ne les commitez pas, ne les affichez pas dans des captures d’écran.
- Pour durcir l’ensemble du daemon (pas seulement Companion), utilisez aussi **`AKASHA_API_TOKEN`** / **`AKASHA_TOKEN`** et une politique `tools_policy.yaml` adaptée.
- Réseau Wi‑Fi invité ou coworking : évitez le bind `0.0.0.0` ou isolez le VLAN.

## Limites connues (état du code)

- **Aucun firmware ESP32 / avatar** n’est fourni avec Akasha ; seules les APIs daemon et la découverte UDP sont implémentées.
- **`notify.unread`** et **`notify.headline`** dans le snapshot sont des placeholders (non alimentés).
- La découverte UDP ne transmet pas d’adresse IP : le client utilise l’IP source du datagramme de réponse ou une IP configurée manuellement.
- Les routes **`/api/voice/*`** ne font pas partie du préfixe `/api/companion` ; un client voix doit les appeler séparément si besoin.
- Détail normatif daemon : [COMPANION_SPEC.md](../COMPANION_SPEC.md). Le registre `spec/feature_evolution_tracking.md` distingue **Companion LAN** (API) et **matériel ESP32** (Planned).
