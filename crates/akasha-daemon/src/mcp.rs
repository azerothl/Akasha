//! MCP (Model Context Protocol) — MVP helpers: validate local server definitions before wiring a transport.
//!
//! Stdio probe: [`crate::mcp_stdio`]. OAuth hardening: **`docs/mcp-oauth.md`**. See also **`docs/mcp-mvp.md`**, **`docs/mcp-runtime.md`**.

pub use crate::mcp_stdio::{probe_stdio_mcp, McpProbeResult};

use serde_json::Value;

/// Validate one server entry from a JSON config (e.g. `{ "command": "npx", "args": ["-y", "@pkg/mcp"] }`).
pub fn validate_mcp_server_entry(entry: &Value) -> Result<(), String> {
    let cmd = entry
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if cmd.is_none() {
        return Err("missing non-empty string field \"command\"".to_string());
    }
    if let Some(args) = entry.get("args") {
        if !args.is_null() && !args.is_array() {
            return Err("\"args\" must be a JSON array or omitted".to_string());
        }
    }
    Ok(())
}

/// Validate a top-level config object with `mcpServers` map (Cursor / VS Code style).
pub fn validate_mcp_config_json(root: &Value) -> Result<(), String> {
    let servers = root
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .ok_or_else(|| "missing object \"mcpServers\"".to_string())?;
    if servers.is_empty() {
        return Err("mcpServers is empty".to_string());
    }
    for (name, entry) in servers {
        validate_mcp_server_entry(entry)
            .map_err(|e| format!("server {:?}: {}", name, e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_minimal_stdio() {
        let v = serde_json::json!({
            "mcpServers": {
                "demo": { "command": "node", "args": ["server.js"] }
            }
        });
        validate_mcp_config_json(&v).unwrap();
    }

    #[test]
    fn validate_rejects_bad_args_type() {
        let v = serde_json::json!({
            "mcpServers": {
                "bad": { "command": "x", "args": "not-array" }
            }
        });
        assert!(validate_mcp_config_json(&v).is_err());
    }
}
