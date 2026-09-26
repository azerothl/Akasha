# MCP OAuth & supply-chain hardening

## Current state (Akasha — P4 must Implemented)

- **Config validation**, **stdio probe**, **long-lived attach**, **tools/list|call**, and **`tools_policy.yaml` → `mcp_servers` allow-list** are production operator surfaces (`akasha mcp validate|probe|status|start|stop`, `GET /api/mcp/status`).
- Operator OAuth **state mirror** exists (`GET/POST /api/mcp/runtime/oauth` → `mcp_oauth_state.json`) with optional refresh when `token_endpoint` + `refresh_token` are set.
- Full **OAuth 2.1 dance** / dynamic client registration for third-party MCP servers is still **stretch**.

## Target hardening (Hermes / enterprise) — stretch

1. **OAuth 2.1 / dynamic client registration** where the upstream MCP spec requires it; store refresh tokens in **vault** (`akasha-vault`), never in world-readable files.
2. ~~**Allow-list** of MCP server packages / commands in `tools_policy`~~ — **done** via `mcp_servers` (must P4).
3. **TLS pinning** optional for known SaaS MCP endpoints.
4. **Attestation**: prefer servers distributed as pinned npm/pip crates with checksum verification (align with `skills.lock.jsonl` patterns).

## Operator checklist before enabling a server

- [ ] Validate JSON (`akasha mcp validate`).
- [ ] Probe on a non-production machine (`akasha mcp probe --tools`).
- [ ] Confirm command + args are not `curl | sh` or other opaque installers.
- [ ] Add the server under `mcp_servers` in `tools_policy.yaml` (recommended for production).
- [ ] Scope filesystem / network via OS sandbox or dedicated VM if the server is not fully trusted.

See also **`mcp-runtime.md`**, user doc §8bis, and **`../roadmap/hermes-integration-remainder.md`**.
