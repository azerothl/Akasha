# Migration pack OpenClaw-like

## API

- `POST /api/migrate/openclaw/preview` — body `{ "source_dir": "/path/to/openclaw/data" }`
- `POST /api/migrate/openclaw/apply` — body `{ "source_dir": "...", "dry_run": false }`

## Layout attendu sous `source_dir`

| Chemin | Import |
|--------|--------|
| `skills/` ou `data/skills/` | Copie vers `data_dir/skills/` |
| `tools_policy.yaml` | Copie ou sidecar `tools_policy.openclaw.import.yaml` si fichier existant |
| `memory_export.json` | Détecté, signalé ; import manuel recommandé |

## Secrets

Ne jamais copier de tokens depuis le pack : utiliser `akasha vault set` après migration.

## CLI (optionnel)

Les mêmes endpoints peuvent être appelés via `curl` contre `http://127.0.0.1:3876` pendant la phase opérateur.
