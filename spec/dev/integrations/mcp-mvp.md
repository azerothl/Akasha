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

### Parité « Agent TARS » (outillage MCP)

Pour rapprocher l’extensibilité à base MCP d’écosystèmes comme [Agent TARS](https://agent-tars.com) sans dépendre de leur UI :

| Étape | Livrable | Notes |
|-------|-----------|--------|
| **Namespacing** | Préfixer les outils MCP (`mcp_<server>_<tool>` ou équivalent) pour éviter les collisions avec les outils natifs Akasha. | Documenter la convention dans `tools_policy.yaml` (clés `allowed_tools` / refus par défaut). |
| **Politique** | Étendre `tools_policy.yaml` avec blocs optionnels `mcp_servers:` ou liste blanche par serveur / par nom d’outil MCP. | Alignement avec le centre de permissions pour les appels sensibles. |
| **Budget** | Compter tokens / coût des tours qui invoquent des outils MCP comme pour les outils natifs. | Réutiliser les métriques session existantes. |
| **Échec isolé** | Erreur MCP → message d’outil compact ; pas d’arrêt du daemon. | Même style que `execute_tool_call` aujourd’hui. |

Ordre d’implémentation recommandé : **stdio attaché** (déjà partiellement exposé via `/api/mcp/runtime` — voir **`mcp-runtime.md`**) → **tools/list + invoke** → **mapping + policy** → tests de régression sur un serveur filesystem echo.

See also `../roadmap/reference-products-parity-matrix.md` and `../../16_plugin_architecture.md`.
