# MCP runtime (stdio) — Hermes parity

**Statut produit (v0.11 P4 must)** : runtime stdio + policy + surface opérateur **Implemented** — voir registre `spec/feature_evolution_tracking.md` et doc user `docs/user/configuration.md` (§8bis).

## Validation (offline)

- Rust: `akasha_daemon::mcp::validate_mcp_config_json` (unit tests in `crates/akasha-daemon/src/mcp.rs`).
- CLI: `akasha mcp validate path/to/mcp.json`

## Stdio probe (local)

Spawns the configured process, sends JSON-RPC `initialize`, reads one response line, optionally sends `tools/list`, then kills the child.

- CLI: `akasha mcp probe path/to/mcp.json [--name myserver] [--tools] [--timeout-secs 8]`
- Library: `akasha_daemon::mcp::probe_stdio_mcp(program, args, include_tools_list, deadline).await` (`crates/akasha-daemon/src/mcp_stdio.rs`)

**Security:** probing runs arbitrary commands from the JSON file — only run on trusted configs (same trust model as Cursor MCP).

## Long-lived stdio + tools

- CLI: `akasha mcp start --server <key>` / `akasha mcp stop` / `akasha mcp status [--json]`
- `GET /api/mcp/runtime` — runtime summary (stdio server attached, oauth state mirror).
- `GET /api/mcp/runtime/sse` — SSE-compatible runtime event payload (single-shot event for operator consumers).
- `POST /api/mcp/runtime/stdio/start` / `POST /api/mcp/runtime/stdio/stop` — attach/detach a long-lived stdio child from `mcp.json`.
- `POST /api/mcp/runtime/tools/list` / `POST /api/mcp/runtime/tools/call` — JSON-RPC against the attached process.
- `GET /api/mcp/runtime/oauth` / `POST /api/mcp/runtime/oauth` — operator OAuth state (persisted in `mcp_oauth_state.json` under data dir).

Attach is gated by `tools_policy.yaml` → `mcp_servers` when that map is non-empty (`can_attach_mcp_server`). Agent tool names are namespaced `mcp_<server>_<tool>` with the same allow-list + optional `mcp_max_calls_per_task`.

Full HTTP/SSE client pooling and provider OAuth exchange remain stretch — see **`mcp-oauth.md`**.

## Operator status (HTTP)

- **`GET /api/mcp/status`** — reads `{data_dir}/mcp.json` if present, returns `config_present`, `valid`, `server_count`, `server_names`, `policy` (MCP allow-list snapshot), CLI hints, and runtime HTTP descriptors.

## UI

Desktop **System health** (expert): fetches `/api/mcp/status` + `/api/mcp/runtime`.

## Tests

Prefer `cargo test -p akasha-daemon --lib --no-default-features --features embedded mcp` if default features hit ONNX/CRT link issues (see `mcp-mvp.md`).

Policy unit tests: `akasha-tools` (`can_attach_mcp_server`, `can_use_mcp_tool`).
