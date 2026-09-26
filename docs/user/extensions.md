# Skills, plugins et canaux

> **akasha-os** : les skills/plugins du daemon ne sont pas les modules Preview — voir [extensions vs akasha-os](extensions-vs-akasha-os.md).

## ## 9. Skills (capacités supplémentaires)


Vous pouvez **demander à l'agent d'installer un skill** depuis une URL. Par exemple, dans le chat : « Installe le skill bankr depuis https://github.com/BankrBot/skills/tree/main/bankr ». L'agent utilisera l'outil d'installation, téléchargera le skill, puis le rechargera.

- Par défaut, seuls les hôtes **GitHub** sont autorisés. Pour autoriser d'autres sites (GitLab, votre propre serveur, etc.), éditez le fichier **tools_policy.yaml** dans le data_dir et ajoutez la clé **allowed_skill_install_hosts** avec la liste des hôtes (ex. `["github.com", "raw.githubusercontent.com", "gitlab.com"]`). Utilisez `["*"]` pour autoriser tout hôte HTTPS.
- Après avoir ajouté ou modifié des skills manuellement (fichiers dans le data_dir), tapez **/skills reload** dans le chat pour les recharger sans redémarrer le daemon.

---



---

## ## 10. Canaux (Telegram, Slack, Discord)


- **Telegram** : enregistrez le token du bot avec `akasha vault set telegram_bot_token VOTRE_TOKEN`, puis définissez la variable d'environnement `AKASHA_TELEGRAM_ENABLED=1`. Le bot répond aux commandes `/akasha <message>` ou `/start`. Pour les briefs / notifies (Life layer), renseignez aussi `AKASHA_TELEGRAM_NOTIFY_CHAT_ID` (UI Connecteurs) — le daemon peut alors appeler `POST /api/channels/notify`.
- **Slack** : vault `slack_signing_secret`, puis `AKASHA_SLACK_ENABLED=1`. Configurez la slash command vers l'URL fournie par votre déploiement.
- **Discord** : vault `discord_bot_token`, puis `AKASHA_DISCORD_ENABLED=1`. Le bot répond au préfixe `!akasha <message>`.

Les variables d'activation sont chargées depuis le fichier **connectors.env** du data_dir (créé ou complété par `akasha init`).

---


## Plugins WASM

Installez depuis un dossier cloné : `akasha plugin install CHEMIN`.

Catalogue public : page **Plugins** sur https://azerothl.github.io/Akasha_app/plugins.html

Plugins **sidecar** (canaux Matrix, CalDAV, **Home Assistant événements**) : processus compagnon ; voir le README de chaque plugin.

### Réseau host (sandbox HTTP)

Les plugins WASM n’ont pas d’accès réseau libre. Pour autoriser des appels HTTP :

1. Dans le `manifest.toml` du plugin : `permissions = ["network"]`.
2. Section `[network]` avec `allowed_url_prefixes`, `https_only`, `max_response_bytes`, `timeout_ms`, `max_requests_per_run`.

L’host expose uniquement `akasha::http_fetch` (JSON in/out). Sans permission `network` ou sans préfixe autorisé, les appels sont refusés. Détail : `spec/dev/plugins/plugin-host-network.md`.

### Vue carte (`view: "map"`)

Un outil (plugin ou natif) peut renvoyer un JSON avec `"view": "map"` pour afficher une carte dans l’UI (géométrie GeoJSON-like `[lon, lat]`, routes, étapes, bbox, liens OSM optionnels). Contrat : `spec/dev/plugins/plugin-map-view-schema.md`.

### Home Assistant (domotique)

Akasha pilote **Home Assistant** (Zigbee, Z-Wave, Matter via HA) — pas un hub radio natif.

**Prérequis HA**

1. Installer [Home Assistant](https://www.home-assistant.io/installation/).
2. Créer un jeton d'accès long-lived (Profil → Sécurité).
3. Détecter l'URL : `akasha discover homeassistant` ou Réglages → Connecteurs → **Détecter**.

**Configuration Akasha**

- `connectors.env` : `AKASHA_HOMEASSISTANT_ENABLED=1`, `HA_BASE_URL=http://…`
- Vault : `akasha vault set ha_access_token VOTRE_TOKEN`
- Plugin : `akasha plugin install CHEMIN/vers/Akasha_plugins/plugins/homeassistant`
- Sidecar (événements → webhooks) : voir `plugins/homeassistant/README.md`

**Outils agent** : `ha_get_state`, `ha_list_entities`, `ha_call_service`, `ha_run_script` (plugin `homeassistant`).

## Code Studio

Interface opérateur pour projets code (cockpit, éditeur, build, preview) :

```bash
npx akasha-code-studio@0.9.0
```

Nécessite le daemon Akasha sur le port 3876.
