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

See **`docs/mcp-mvp.md`** for MVP scope. HTTP/SSE client, connection pooling, and OAuth are covered in **`docs/mcp-oauth.md`** and the Hermes integration remainder doc.

## Tests (Windows)

Prefer `cargo test -p akasha-daemon --lib --no-default-features --features embedded mcp` if default features hit ONNX/CRT link issues (see `docs/mcp-mvp.md`).
