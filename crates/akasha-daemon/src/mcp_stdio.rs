//! MCP stdio transport — probe handshake (`initialize` + optional `tools/list`) for compatibility tests.
//!
//! Uses the standard `Content-Length`-framed transport as specified by the MCP stdio protocol
//! (identical to Language Server Protocol framing), which is required by common IDE MCP servers.
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

/// Write one MCP stdio framed message: `Content-Length: <n>\r\n\r\n<json_body>`.
async fn mcp_write_framed<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    msg: &Value,
) -> std::io::Result<()> {
    let body = msg.to_string();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(body.as_bytes()).await?;
    writer.flush().await
}

/// Read one MCP stdio framed message by consuming `Content-Length` headers then the body.
async fn mcp_read_framed<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<Value, String> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| format!("read header: {}", e))?;
        if n == 0 {
            return Err("connection closed before response headers".to_string());
        }
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if trimmed.is_empty() {
            break;
        }
        if let Some(val) = trimmed.strip_prefix("Content-Length:") {
            content_length = val.trim().parse().ok();
        }
    }
    let len = content_length
        .ok_or_else(|| "no Content-Length header in response".to_string())?;
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|e| format!("read body ({} bytes): {}", len, e))?;
    let s = std::str::from_utf8(&body).map_err(|e| format!("non-UTF-8 body: {}", e))?;
    serde_json::from_str(s).map_err(|e| format!("invalid JSON in body: {}", e))
}

/// Spawn `program` with `args`, send JSON-RPC `initialize` using MCP stdio framing,
/// read one framed response, optionally send `tools/list` and read its response.
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
    timeout(deadline, mcp_write_framed(&mut stdin, &init))
        .await
        .map_err(|_| "timeout writing initialize".to_string())?
        .map_err(|e| format!("write initialize: {}", e))?;

    let mut reader = BufReader::new(stdout);
    let init_resp = match timeout(deadline, mcp_read_framed(&mut reader)).await {
        Err(_) => return Err("timeout reading initialize response".to_string()),
        Ok(Ok(v)) => Some(v),
        Ok(Err(e)) if e.starts_with("connection closed") => None,
        Ok(Err(e)) => return Err(format!("initialize response: {}", e)),
    };

    let mut tools_resp = None;
    if include_tools_list {
        let list = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        });
        let _ = timeout(deadline, mcp_write_framed(&mut stdin, &list)).await;
        if let Ok(Ok(v)) = timeout(deadline, mcp_read_framed(&mut reader)).await {
            tools_resp = Some(v);
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
