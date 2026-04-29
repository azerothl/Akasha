# MCP MVP (Akasha)

## Goal

Hermes-style **plug-and-play MCP**: declare servers in JSON, validate before enablement, then attach a transport (stdio first, HTTP later) with clear auth guidance.

## What exists today

- **Config validation** (library): `akasha_daemon::mcp::validate_mcp_config_json` checks a `mcpServers` object with per-server `command` + optional `args` array. Used by tests in `crates/akasha-daemon/src/mcp.rs`.
- **Stdio probe** (library + CLI): `akasha_daemon::mcp::probe_stdio_mcp`, `akasha mcp validate|probe` — see **`mcp-runtime.md`**.
- **Plugin surface**: `PluginKind::Mcp` in `akasha-plugin-api` (bridge staged).

## Recommended config shape (stdio)

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed/root"]
    }
  }
}
```

## Auth / secrets

- Prefer **vault keys** (`akasha vault set …`) for tokens used by MCP HTTP transports; document env fallbacks only for local dev.
- Do not commit MCP JSON with live secrets into git.

## Tests (Windows)

`cargo test -p akasha-daemon` avec les **features par défaut** (`embeddings` + ONNX) peut échouer au **link** (CRT / `ort_sys`). Pour lancer uniquement les tests du module `mcp` :

`cargo test -p akasha-daemon --lib --no-default-features --features embedded mcp`

## Next steps (runtime)

1. Spawn process, newline-delimited JSON-RPC `initialize` + `tools/list`.
2. Map MCP tools into Akasha tool namespace with policy gates (`tools_policy.yaml`).
3. Smoke tests against a reference stdio server (filesystem or echo).

See also `../roadmap/hermes-akasha-parity-matrix.md` and `../../16_plugin_architecture.md`.
