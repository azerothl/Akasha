//! MCP stdio transport — probe handshake (`initialize` + optional `tools/list`) for Hermes parity / compatibility tests.
//!
//! See `docs/mcp-runtime.md` and `docs/mcp-mvp.md`.

use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

/// Result of a short stdio probe against an MCP server process.
#[derive(Debug, Clone, serde::Serialize)]
pub struct McpProbeResult {
    pub initialize: Option<Value>,
    pub tools_list: Option<Value>,
    pub stderr_tail: String,
}

/// Spawn `program` with `args`, send JSON-RPC `initialize`, read one line, optionally send `tools/list`.
/// The child is killed when the probe finishes or on timeout.
pub async fn probe_stdio_mcp(
    program: &str,
    args: &[String],
    include_tools_list: bool,
    deadline: Duration,
) -> Result<McpProbeResult, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawn {:?}: {}", program, e))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "stdin not available".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "stdout not available".to_string())?;
    let mut stderr = child.stderr.take();

    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "akasha-mcp-probe", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    let init_line = init.to_string() + "\n";
    timeout(deadline, stdin.write_all(init_line.as_bytes()))
        .await
        .map_err(|_| "timeout writing initialize".to_string())?
        .map_err(|e| format!("write initialize: {}", e))?;
    timeout(deadline, stdin.flush())
        .await
        .map_err(|_| "timeout flush initialize".to_string())?
        .map_err(|e| format!("flush: {}", e))?;

    let mut reader = BufReader::new(stdout);
    let mut line1 = String::new();
    timeout(deadline, reader.read_line(&mut line1))
        .await
        .map_err(|_| "timeout reading initialize response".to_string())?
        .map_err(|e| format!("read initialize: {}", e))?;
    let init_resp: Option<Value> = if line1.trim().is_empty() {
        None
    } else {
        Some(serde_json::from_str(line1.trim()).map_err(|e| format!("initialize JSON: {}", e))?)
    };

    let mut tools_resp = None;
    if include_tools_list {
        let list = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        });
        let list_line = list.to_string() + "\n";
        let _ = timeout(deadline, stdin.write_all(list_line.as_bytes())).await;
        let _ = timeout(deadline, stdin.flush()).await;
        let mut line2 = String::new();
        if timeout(deadline, reader.read_line(&mut line2))
            .await
            .ok()
            .and_then(|r| r.ok())
            .map(|n| n > 0)
            == Some(true)
            && !line2.trim().is_empty()
        {
            tools_resp = serde_json::from_str(line2.trim()).ok();
        }
    }

    let mut err_tail = String::new();
    if let Some(mut err) = stderr.take() {
        let mut buf = Vec::new();
        let _ = timeout(Duration::from_millis(400), err.read_to_end(&mut buf)).await;
        err_tail = String::from_utf8_lossy(&buf).chars().take(2000).collect();
    }

    let _ = child.kill().await;

    Ok(McpProbeResult {
        initialize: init_resp,
        tools_list: tools_resp,
        stderr_tail: err_tail,
    })
}
