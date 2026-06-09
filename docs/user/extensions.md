# Skills, plugins et canaux

## ## 9. Skills (capacités supplémentaires)


Vous pouvez **demander à l'agent d'installer un skill** depuis une URL. Par exemple, dans le chat : « Installe le skill bankr depuis https://github.com/BankrBot/skills/tree/main/bankr ». L'agent utilisera l'outil d'installation, téléchargera le skill, puis le rechargera.

- Par défaut, seuls les hôtes **GitHub** sont autorisés. Pour autoriser d'autres sites (GitLab, votre propre serveur, etc.), éditez le fichier **tools_policy.yaml** dans le data_dir et ajoutez la clé **allowed_skill_install_hosts** avec la liste des hôtes (ex. `["github.com", "raw.githubusercontent.com", "gitlab.com"]`). Utilisez `["*"]` pour autoriser tout hôte HTTPS.
- Après avoir ajouté ou modifié des skills manuellement (fichiers dans le data_dir), tapez **/skills reload** dans le chat pour les recharger sans redémarrer le daemon.

---



---

## ## 10. Canaux (Telegram, Slack, Discord)


- **Telegram** : enregistrez le token du bot avec `akasha vault set telegram_bot_token VOTRE_TOKEN`, puis définissez la variable d'environnement `AKASHA_TELEGRAM_ENABLED=1`. Le bot répond aux commandes `/akasha <message>` ou `/start`.
- **Slack** : vault `slack_signing_secret`, puis `AKASHA_SLACK_ENABLED=1`. Configurez la slash command vers l'URL fournie par votre déploiement.
- **Discord** : vault `discord_bot_token`, puis `AKASHA_DISCORD_ENABLED=1`. Le bot répond au préfixe `!akasha <message>`.

Les variables d'activation sont chargées depuis le fichier **connectors.env** du data_dir (créé ou complété par `akasha init`).

---


## Plugins WASM

Installez depuis un dossier cloné : `akasha plugin install CHEMIN`.

Catalogue public : page **Plugins** sur https://azerothl.github.io/Akasha_app/plugins.html

Plugins **sidecar** (canaux Matrix, CalDAV) : processus compagnon ; voir le README de chaque plugin.

## Code Studio

Interface opérateur pour projets code (cockpit, éditeur, build, preview) :

```bash
npx akasha-code-studio@0.9.0
```

Nécessite le daemon Akasha sur le port 3876.
