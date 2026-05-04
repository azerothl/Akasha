//! MCP (Model Context Protocol) — MVP helpers: validate local server definitions before wiring a transport.
//!
//! Stdio probe: [`crate::mcp_stdio`]. OAuth hardening: **`spec/dev/integrations/mcp-oauth.md`**. See also **`spec/dev/integrations/mcp-mvp.md`**, **`spec/dev/integrations/mcp-runtime.md`**.

pub use crate::mcp_stdio::{probe_stdio_mcp, McpProbeResult};

use serde_json::{json, Value};
use std::path::Path;

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

/// Validate a top-level config object with `mcpServers` map (IDE-style MCP JSON).
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

/// JSON for `GET /api/mcp/status` — validates `mcp.json` under the daemon data dir if present.
pub fn mcp_operator_status(data_dir: &Path) -> Value {
    let path = data_dir.join("mcp.json");
    let present = path.is_file();
    let mut out = json!({
        "config_path": path.display().to_string(),
        "config_present": present,
        "valid": Value::Null,
        "server_count": 0_i32,
        "runtime": "stdio_probe_validate_and_optional_long_lived",
        "oauth": {
            "mode": "documented_vault_reserved",
            "see": "spec/dev/integrations/mcp-oauth.md"
        },
        "mcp_runtime_http": {
            "GET /api/mcp/runtime": "attached stdio server + transport roadmap",
            "POST /api/mcp/runtime/stdio/start": { "body": { "server": "mcpServers key" } },
            "POST /api/mcp/runtime/stdio/stop": "kill attached stdio child"
        },
    });
    if !present {
        return out;
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(e) => {
            out["read_error"] = Value::String(e.to_string());
            return out;
        }
    };
    let root: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            out["valid"] = json!(false);
            out["parse_error"] = Value::String(e.to_string());
            return out;
        }
    };
    let valid = validate_mcp_config_json(&root).is_ok();
    out["valid"] = json!(valid);
    let count = root
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .map(|o| o.len() as i32)
        .unwrap_or(0);
    out["server_count"] = json!(count);
    out
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

    #[test]
    fn mcp_operator_status_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = super::mcp_operator_status(dir.path());
        assert_eq!(s["config_present"], false);
    }
}
