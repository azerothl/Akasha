# Captures d’interface (UI)

Les fichiers `ui-chat.png` et `ui-docs.png` sont générés par la suite Playwright du dossier `apps/akasha-ui` :

```bash
# À la racine du dépôt Akasha : binaire daemon + build UI mode e2e
cargo build -p akasha-daemon
cd apps/akasha-ui && npm ci && npm run test:e2e:install && npm run test:e2e
```

Copier ensuite les PNG vers le site marketing (`Akasha_app/assets/screenshots/`) si vous publiez une nouvelle version des visuels.
