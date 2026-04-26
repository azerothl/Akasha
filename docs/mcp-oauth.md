# MCP OAuth & supply-chain hardening

## Current state (Akasha 0.8.x)

- **Config validation** and **stdio probe** reduce “it starts but nothing works” failures (`akasha mcp validate` / `akasha mcp probe`, `akasha_daemon::mcp`).
- No built-in OAuth dance for third-party MCP servers yet.

## Target hardening (Hermes / enterprise)

1. **OAuth 2.1 / dynamic client registration** where the upstream MCP spec requires it; store refresh tokens in **vault** (`akasha-vault`), never in world-readable files.
2. **Allow-list** of MCP server packages / commands in `tools_policy` or dedicated `mcp_policy.yaml`.
3. **TLS pinning** optional for known SaaS MCP endpoints.
4. **Attestation**: prefer servers distributed as pinned npm/pip crates with checksum verification (align with `skills.lock.jsonl` patterns).

## Operator checklist before enabling a server

- [ ] Validate JSON (`akasha mcp validate`).
- [ ] Probe on a non-production machine (`akasha mcp probe --tools`).
- [ ] Confirm command + args are not `curl | sh` or other opaque installers.
- [ ] Scope filesystem / network via OS sandbox or dedicated VM if the server is not fully trusted.

See also **`docs/mcp-runtime.md`** and **`docs/hermes-integration-remainder.md`**.
