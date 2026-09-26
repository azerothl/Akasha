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

/// Path to `mcp.json` under the daemon data directory.
pub fn mcp_config_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("mcp.json")
}

/// Load `mcp.json` or return an empty `mcpServers` object.
pub fn load_mcp_config(data_dir: &Path) -> Result<Value, String> {
    let path = mcp_config_path(data_dir);
    if !path.is_file() {
        return Ok(json!({ "mcpServers": {} }));
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read mcp.json: {}", e))?;
    let mut root: Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse mcp.json: {}", e))?;
    if root.get("mcpServers").and_then(|v| v.as_object()).is_none() {
        root["mcpServers"] = json!({});
    }
    Ok(root)
}

/// Persist validated MCP config to `mcp.json`.
pub fn save_mcp_config(data_dir: &Path, root: &Value) -> Result<(), String> {
    let path = mcp_config_path(data_dir);
    let empty = root
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .is_none_or(|o| o.is_empty());
    if empty {
        if path.is_file() {
            std::fs::remove_file(&path).map_err(|e| format!("remove mcp.json: {}", e))?;
        }
        return Ok(());
    }
    validate_mcp_config_json(root)?;
    let pretty = serde_json::to_string_pretty(root)
        .map_err(|e| format!("serialize mcp.json: {}", e))?;
    std::fs::write(&path, pretty).map_err(|e| format!("write mcp.json: {}", e))?;
    Ok(())
}

/// Add or replace one MCP server entry in `mcp.json`.
pub fn add_mcp_server(data_dir: &Path, name: &str, entry: &Value) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("server name required".to_string());
    }
    validate_mcp_server_entry(entry)?;
    let mut root = load_mcp_config(data_dir)?;
    let servers = root
        .get_mut("mcpServers")
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| "missing mcpServers object".to_string())?;
    servers.insert(name.to_string(), entry.clone());
    let count = servers.len();
    save_mcp_config(data_dir, &root)?;
    Ok(format!("mcp server {:?} added ({} total)", name, count))
}

/// Remove one MCP server from `mcp.json`.
pub fn remove_mcp_server(data_dir: &Path, name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("server name required".to_string());
    }
    let mut root = load_mcp_config(data_dir)?;
    let servers = root
        .get_mut("mcpServers")
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| "missing mcpServers object".to_string())?;
    if servers.remove(name).is_none() {
        return Err(format!("server {:?} not found", name));
    }
    let remaining = servers.len();
    save_mcp_config(data_dir, &root)?;
    Ok(format!("mcp server {:?} removed ({} remaining)", name, remaining))
}

/// Snapshot of `tools_policy.yaml` MCP allow-list for operators.
fn mcp_policy_status(data_dir: &Path) -> Value {
    let path = data_dir.join("tools_policy.yaml");
    if !path.is_file() {
        return json!({
            "path": path.display().to_string(),
            "present": false,
            "mcp_allowlist_active": false,
            "mcp_servers": [],
            "mcp_max_calls_per_task": Value::Null,
            "note": "absent tools_policy → no MCP allow-list (all mcp.json servers may attach)"
        });
    }
    match akasha_tools::ToolsPolicy::load_from_path(&path) {
        Ok(policy) => {
            let keys: Vec<String> = policy.mcp_servers.keys().cloned().collect();
            let active = !policy.mcp_servers.is_empty();
            json!({
                "path": path.display().to_string(),
                "present": true,
                "mcp_allowlist_active": active,
                "mcp_servers": keys,
                "mcp_max_calls_per_task": policy.mcp_max_calls_per_task,
                "note": if active {
                    "only listed mcp_servers may attach / invoke (enabled != false)"
                } else {
                    "mcp_servers empty → open (document allow-list before production)"
                }
            })
        }
        Err(e) => json!({
            "path": path.display().to_string(),
            "present": true,
            "error": e.to_string(),
            "mcp_allowlist_active": Value::Null,
            "mcp_servers": [],
        }),
    }
}

/// JSON for `GET /api/mcp/status` — validates `mcp.json` under the daemon data dir if present.
pub fn mcp_operator_status(data_dir: &Path) -> Value {
    let path = mcp_config_path(data_dir);
    let present = path.is_file();
    let mut out = json!({
        "config_path": path.display().to_string(),
        "config_present": present,
        "valid": Value::Null,
        "server_count": 0_i32,
        "server_names": [],
        "runtime": "stdio_probe_validate_and_optional_long_lived",
        "policy": mcp_policy_status(data_dir),
        "oauth": {
            "mode": "documented_vault_reserved",
            "see": "spec/dev/integrations/mcp-oauth.md"
        },
        "operator_cli": {
            "validate": "akasha mcp validate <mcp.json>",
            "probe": "akasha mcp probe <mcp.json> [--name KEY] [--tools]",
            "status": "akasha mcp status",
            "start": "akasha mcp start --server KEY",
            "stop": "akasha mcp stop"
        },
        "mcp_runtime_http": {
            "GET /api/mcp/runtime": "attached stdio server + transport roadmap",
            "POST /api/mcp/runtime/stdio/start": { "body": { "server": "mcpServers key" } },
            "POST /api/mcp/runtime/stdio/stop": "kill attached stdio child",
            "POST /api/mcp/runtime/tools/list": "tools/list on attached stdio server",
            "POST /api/mcp/runtime/tools/call": { "body": { "name": "tool", "arguments": {} } }
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
    let names: Vec<String> = root
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    out["server_count"] = json!(names.len() as i32);
    out["server_names"] = json!(names);
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

    #[test]
    fn add_remove_mcp_server_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let entry = serde_json::json!({ "command": "node", "args": ["server.js"] });
        super::add_mcp_server(dir.path(), "demo", &entry).unwrap();
        let root = super::load_mcp_config(dir.path()).unwrap();
        assert!(root["mcpServers"]["demo"].is_object());
        super::remove_mcp_server(dir.path(), "demo").unwrap();
        let root2 = super::load_mcp_config(dir.path()).unwrap();
        assert_eq!(root2["mcpServers"].as_object().unwrap().len(), 0);
    }

    #[test]
    fn mcp_operator_status_includes_policy_and_cli_hints() {
        let dir = tempfile::tempdir().unwrap();
        let s = super::mcp_operator_status(dir.path());
        assert_eq!(s["config_present"], false);
        assert_eq!(s["policy"]["present"], false);
        assert_eq!(s["policy"]["mcp_allowlist_active"], false);
        assert!(s["operator_cli"]["status"].as_str().unwrap().contains("mcp status"));
    }

    #[tokio::test]
    async fn start_stdio_denied_by_mcp_allowlist() {
        let dir = tempfile::tempdir().unwrap();
        let entry = serde_json::json!({ "command": "false", "args": [] });
        super::add_mcp_server(dir.path(), "blocked", &entry).unwrap();
        std::fs::write(
            dir.path().join("tools_policy.yaml"),
            "mcp_servers:\n  other:\n    enabled: true\n    allowed_tools: [\"*\"]\n",
        )
        .unwrap();
        let err = crate::mcp_runtime::start_stdio_server(dir.path(), "blocked")
            .await
            .expect_err("allow-list must deny");
        assert!(
            err.contains("denied") || err.contains("allow-list"),
            "unexpected err: {err}"
        );
    }
}
