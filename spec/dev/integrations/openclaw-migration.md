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

## CLI

```bash
akasha migrate openclaw preview --source-dir /path/to/openclaw/data
akasha migrate openclaw apply --source-dir /path/to/openclaw/data [--dry-run]
```

Requires a running daemon on `AKASHA_PORT` (default 3876). Equivalent HTTP:

- `POST /api/migrate/openclaw/preview` — body `{ "source_dir": "..." }`
- `POST /api/migrate/openclaw/apply` — body `{ "source_dir": "...", "dry_run": false }`
