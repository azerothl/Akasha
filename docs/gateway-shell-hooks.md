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

- **Operator summary:** `GET /api/lifecycle/hooks` returns whether `lifecycle_hooks.json` exists and counts hook arrays (`on_schedule_fire`, `on_http_request_pre`, `on_http_request_post`).
- **Gateway (MVP):** `on_http_request_pre` runs **before** the main HTTP handler (after path parsing); each argv list is executed with **`AKASHA_GATEWAY_HOOK_TIMEOUT_SECS`** (default **3s**) per command. Environment: `AKASHA_DATA_DIR`, `AKASHA_HTTP_METHOD`, `AKASHA_HTTP_PATH`, `AKASHA_GATEWAY_HOOK_SANDBOX` (metadata flag for operators; advanced isolation remains roadmap). `on_http_request_post` is **spawned when the API handler returns** (response body already built); same timeout and env vars. Failures are logged only.
- **External automation:** use signed **`/api/automation/webhook`** (or `/direct`) for triggers that bypass the HTTP hook path.
- **Plugin:** WASM plugin host events — extend `Akasha_plugins` + daemon registry (see plugins trust doc).
- **Shell (general):** unify with `lifecycle_hooks.json` schema extensions (`on_task_start`, …) in a future release.

## Security

Untrusted hook scripts run with the **same privileges as the daemon**. Treat `lifecycle_hooks.json` like a root-owned cron: only operators should edit it; prefer allow-listed commands and dedicated service accounts where possible. The env var **`AKASHA_GATEWAY_HOOK_SANDBOX`** is passed through for future policy / documentation (default `none`); it does **not** by itself isolate the child process.
