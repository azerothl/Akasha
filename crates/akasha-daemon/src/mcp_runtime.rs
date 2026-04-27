//! MCP stdio long-lived attach (Hermes tranche) + runtime summary for operators.
//! HTTP/SSE and in-product OAuth remain roadmap; see `docs/mcp-runtime.md`, `docs/mcp-oauth.md`.

use serde_json::{json, Value};
use std::path::Path;
use std::sync::OnceLock;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

struct StdioState {
    server: String,
    child: tokio::process::Child,
    /// Keep stdin open after `initialize` so the MCP server process stays healthy.
    _stdin: tokio::process::ChildStdin,
}

static STDIO: OnceLock<Mutex<Option<StdioState>>> = OnceLock::new();
static OAUTH_STATE: OnceLock<Mutex<Value>> = OnceLock::new();

fn cell() -> &'static Mutex<Option<StdioState>> {
    STDIO.get_or_init(|| Mutex::new(None))
}

fn oauth_cell() -> &'static Mutex<Value> {
    OAUTH_STATE.get_or_init(|| {
        Mutex::new(json!({
            "status": "not_configured",
            "provider": null,
            "updated_at": null
        }))
    })
}

fn oauth_state_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("mcp_oauth_state.json")
}

async fn oauth_load_from_disk(data_dir: &Path) -> Value {
    let p = oauth_state_path(data_dir);
    match tokio::fs::read_to_string(&p).await {
        Ok(raw) => serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| {
            json!({
                "status": "invalid_state_file",
                "provider": null,
                "updated_at": null
            })
        }),
        Err(_) => json!({
            "status": "not_configured",
            "provider": null,
            "updated_at": null
        }),
    }
}

async fn write_framed(stdin: &mut (impl AsyncWriteExt + Unpin), msg: &Value) -> std::io::Result<()> {
    let body = msg.to_string();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin.write_all(header.as_bytes()).await?;
    stdin.write_all(body.as_bytes()).await?;
    stdin.flush().await
}

pub async fn summary() -> Value {
    let g = cell().lock().await;
    json!({
        "stdio_server": g.as_ref().map(|s| s.server.as_str()),
        "oauth": {
            "mode": "documented_vault_reserved",
            "see": "docs/mcp-oauth.md"
        },
        "transports": {
            "stdio_long_lived": "POST /api/mcp/runtime/stdio/start { \"server\": \"name\" }",
            "http_sse": "GET /api/mcp/runtime/sse (phase next: heartbeat stream)"
        },
        "oauth_state": oauth_cell().lock().await.clone(),
    })
}

pub async fn oauth_get(data_dir: &Path) -> Value {
    let disk = oauth_load_from_disk(data_dir).await;
    let mut g = oauth_cell().lock().await;
    if g.get("updated_at").and_then(|x| x.as_str()).is_none()
        || g.get("updated_at").and_then(|x| x.as_str()).unwrap_or("")
            < disk.get("updated_at").and_then(|x| x.as_str()).unwrap_or("")
    {
        *g = disk;
    }
    g.clone()
}

pub async fn oauth_put(data_dir: &Path, provider: String, status: String) -> Result<Value, String> {
    let mut g = oauth_cell().lock().await;
    *g = json!({
        "status": status,
        "provider": provider,
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });
    let out = g.clone();
    let p = oauth_state_path(data_dir);
    let raw = serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?;
    // Write atomically: write to a temp file then rename so a crash can never leave a
    // partially-written state file (same pattern as session_state.rs / autonomous_mission_config.rs).
    let tmp = p.with_extension("json.tmp");
    tokio::fs::write(&tmp, raw)
        .await
        .map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    // std::fs::rename on Windows fails when the destination already exists — remove it first.
    let _ = tokio::fs::remove_file(&p).await;
    tokio::fs::rename(&tmp, &p)
        .await
        .map_err(|e| format!("rename {} -> {}: {}", tmp.display(), p.display(), e))?;
    Ok(out)
}

/// Spawn one MCP server from `mcp.json`, send `initialize`, keep the process alive for operator tooling.
pub async fn start_stdio_server(data_dir: &Path, server: &str) -> Result<Value, String> {
    let path = data_dir.join("mcp.json");
    let raw = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("read mcp.json: {}", e))?;
    let root: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let entry = root
        .get("mcpServers")
        .and_then(|m| m.as_object())
        .and_then(|o| o.get(server))
        .ok_or_else(|| format!("unknown mcp server {:?}", server))?;
    let cmd = entry
        .get("command")
        .and_then(|c| c.as_str())
        .ok_or_else(|| "missing command".to_string())?;
    let args: Vec<String> = entry
        .get("args")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut child = Command::new(cmd)
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(false)
        .spawn()
        .map_err(|e| format!("spawn: {}", e))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "stdin missing".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "stdout missing".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "stderr missing".to_string())?;
    tokio::spawn(async move {
        let mut e = stderr;
        let mut b = [0u8; 2048];
        loop {
            match e.read(&mut b).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });

    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "akasha-mcp-runtime", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    write_framed(&mut stdin, &init)
        .await
        .map_err(|e| format!("initialize write: {}", e))?;

    let mut reader = BufReader::new(stdout);
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });

    let mut slot = cell().lock().await;
    if let Some(prev) = slot.take() {
        let mut c = prev.child;
        let _ = c.kill().await;
        let _ = c.wait().await;
    }
    *slot = Some(StdioState {
        server: server.to_string(),
        child,
        _stdin: stdin,
    });
    Ok(json!({ "ok": true, "server": server }))
}

pub async fn stop_stdio_server() -> Value {
    let mut slot = cell().lock().await;
    if let Some(prev) = slot.take() {
        let mut c = prev.child;
        let _ = c.kill().await;
        let _ = c.wait().await;
    }
    json!({ "ok": true })
}
