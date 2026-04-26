# Automation webhooks (signed, idempotent)

Hermes-style **external** automation: verify caller, dedupe, rate-limit, accept payload **without** invoking the LLM.

## Endpoints

| Method | Path | Behaviour |
|--------|------|-----------|
| `POST` | `/api/automation/webhook` | HMAC body → `202` + JSON summary of top-level keys |
| `POST` | `/api/automation/webhook/direct` | Same HMAC; response body = **`AKASHA_WEBHOOK_DIRECT_BODY_JSON`** (must be valid JSON text) |

## Environment

| Variable | Role |
|----------|------|
| `AKASHA_AUTOMATION_WEBHOOK_SECRET` | Required. Shared secret for HMAC-SHA256 over raw POST body. |
| `AKASHA_WEBHOOK_DIRECT_BODY_JSON` | Required for `/direct`. Static JSON string returned as HTTP body. |

## Headers

- **`X-Signature`** or **`X-Hub-Signature-256`**: hex digest of HMAC-SHA256(body), or `sha256=<hex>` (GitHub style).
- **`Idempotency-Key`**: optional; duplicates within **24h** → `409`.
- Rate limit: **120 requests / minute** per route (in-memory; process restart resets counters).

## CSRF

Browser `Origin` rules apply to `POST` (see daemon `handle_api`). Server-to-server callers typically omit `Origin` and are unaffected.

## Schedule lifecycle hooks

For **sandboxed** shell hooks on schedule fire (separate from HTTP webhooks), see `lifecycle_hooks.json` in **`docs/gateway-shell-hooks.md`**.
