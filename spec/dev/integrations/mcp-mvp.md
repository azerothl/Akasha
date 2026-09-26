# MCP MVP (Akasha)

## Goal

Hermes-style **plug-and-play MCP**: declare servers in JSON, validate before enablement, then attach a transport (stdio first, HTTP later) with clear auth guidance.

## What exists today (Implemented — P4 must)

- **Config validation** (library): `akasha_daemon::mcp::validate_mcp_config_json` checks a `mcpServers` object with per-server `command` + optional `args` array. Used by tests in `crates/akasha-daemon/src/mcp.rs`.
- **Stdio probe** (library + CLI): `akasha_daemon::mcp::probe_stdio_mcp`, `akasha mcp validate|probe` — see **`mcp-runtime.md`**.
- **Long-lived stdio** + `tools/list` / `tools/call` via `/api/mcp/runtime/*` and `akasha mcp start|stop|status`.
- **Policy gates**: `tools_policy.yaml` → `mcp_servers` / `mcp_max_calls_per_task` ; attach denied when allow-list active and server missing/`enabled: false`.
- **Agent namespace**: `mcp_<server>_<tool>` + tools `mcp_server_add` / `mcp_server_remove`.
- **Plugin surface**: `PluginKind::Mcp` in `akasha-plugin-api` (bridge staged).
- **User doc**: `docs/user/configuration.md` §8bis + CLI rows in `docs/user/commandes.md`.

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
- Full OAuth 2.1 / vault-only refresh remains **stretch** — see **`mcp-oauth.md`**.

## Tests (Windows)

`cargo test -p akasha-daemon` avec les **features par défaut** (`embeddings` + ONNX) peut échouer au **link** (CRT / `ort_sys`). Pour lancer uniquement les tests du module `mcp` :

`cargo test -p akasha-daemon --lib --no-default-features --features embedded mcp`

## Stretch / next (hors must P4)

1. OAuth 2.1 + tokens vault-only (pas `mcp_oauth_state.json` world-readable).
2. Attestation checksum packages MCP (pattern `skills.lock.jsonl`).
3. TLS pinning optionnel endpoints SaaS MCP.
4. HTTP/SSE client pooling multi-serveurs.

### Parité « Agent TARS » (outillage MCP)

| Étape | Livrable | Statut |
|-------|-----------|--------|
| **Namespacing** | Préfixer les outils MCP (`mcp_<server>_<tool>`) | Done |
| **Politique** | `tools_policy.yaml` → `mcp_servers` | Done |
| **Budget** | `mcp_max_calls_per_task` / per-server | Done |
| **Échec isolé** | Erreur MCP → message d'outil compact | Done |

See also `../roadmap/reference-products-parity-matrix.md` and `../../16_plugin_architecture.md`.
