# MCP runtime (stdio) — Hermes parity

## Validation (offline)

- Rust: `akasha_daemon::mcp::validate_mcp_config_json` (unit tests in `crates/akasha-daemon/src/mcp.rs`).
- CLI: `akasha mcp validate path/to/mcp.json`

## Stdio probe (local)

Spawns the configured process, sends JSON-RPC `initialize`, reads one response line, optionally sends `tools/list`, then kills the child.

- CLI: `akasha mcp probe path/to/mcp.json [--name myserver] [--tools] [--timeout-secs 8]`
- Library: `akasha_daemon::mcp::probe_stdio_mcp(program, args, include_tools_list, deadline).await` (`crates/akasha-daemon/src/mcp_stdio.rs`)

**Security:** probing runs arbitrary commands from the JSON file — only run on trusted configs (same trust model as Cursor MCP).

## HTTP transport & long-lived sessions

- `GET /api/mcp/runtime` — runtime summary (stdio server attached, oauth state mirror).
- `GET /api/mcp/runtime/sse` — SSE-compatible runtime event payload (single-shot event for operator consumers).
- `POST /api/mcp/runtime/stdio/start` / `POST /api/mcp/runtime/stdio/stop` — attach/detach a long-lived stdio child from `mcp.json`.
- `GET /api/mcp/runtime/oauth` / `POST /api/mcp/runtime/oauth` — operator OAuth state (persisted in `mcp_oauth_state.json` under data dir).

See **`mcp-mvp.md`** for MVP scope. Full HTTP/SSE client pooling and provider OAuth exchange remain covered in **`mcp-oauth.md`** and the Hermes integration remainder doc.

## Operator status (HTTP)

- **`GET /api/mcp/status`** — reads `{data_dir}/mcp.json` if present, returns `config_present`, `valid` (schema validation), `server_count`, and a runtime descriptor (`stdio_probe_validate_and_optional_long_lived`).

## Tests (Windows)

Prefer `cargo test -p akasha-daemon --lib --no-default-features --features embedded mcp` if default features hit ONNX/CRT link issues (see `mcp-mvp.md`).
