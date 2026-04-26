# Multi-level hooks (gateway, shell) — status

Hermes describes **gateway**, **plugin**, and **shell** hooks. Akasha implements a **first shell slice** tied to the scheduler; gateway/plugin fan-out remains incremental.

## 1) Schedule fire — `lifecycle_hooks.json`

Place in the **data directory** (same parent directory as the task store file — see `akasha paths`).

```json
{
  "on_schedule_fire": [
    ["powershell", "-NoProfile", "-Command", "Write-Host schedule=$env:AKASHA_SCHEDULE_ID"]
  ]
}
```

Each array under `on_schedule_fire` is a full **argv** (no shell parsing). Environment variables set by Akasha:

| Variable | Meaning |
|----------|---------|
| `AKASHA_DATA_DIR` | Data directory path |
| `AKASHA_SCHEDULE_ID` | UUID of the schedule |
| `AKASHA_TASK_ID` | UUID of the created task |

Execution is **fire-and-forget** with a **30s** timeout per argv list; failures are logged only.

## 2) Gateway / plugin hooks

- **Gateway:** HTTP middleware hooks (pre/post route) — roadmap; use signed **`/api/automation/webhook`** for external triggers today.
- **Plugin:** WASM plugin host events — extend `Akasha_plugins` + daemon registry (see plugins trust doc).
- **Shell (general):** unify with `lifecycle_hooks.json` schema extensions (`on_task_start`, …) in a future release.

## Security

Untrusted hook scripts run with the **same privileges as the daemon**. Treat `lifecycle_hooks.json` like a root-owned cron: only operators should edit it; prefer allow-listed commands and dedicated service accounts where possible.
