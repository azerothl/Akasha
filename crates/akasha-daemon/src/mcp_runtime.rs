//! MCP stdio long-lived attach + runtime summary for operators.
//! HTTP/SSE and in-product OAuth remain roadmap; see `spec/dev/integrations/mcp-runtime.md`, `spec/dev/integrations/mcp-oauth.md`.

use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

struct StdioState {
    server: String,
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    next_id: AtomicU64,
}

pub fn mcp_namespaced_tool(server: &str, tool: &str) -> String {
    let safe_server = server.replace('.', "_").replace('-', "_");
    let safe_tool = tool.replace('.', "_").replace('-', "_");
    format!("mcp_{safe_server}_{safe_tool}")
}

pub fn parse_mcp_tool_name(tool_name: &str) -> Option<(String, String)> {
    let rest = tool_name.strip_prefix("mcp_")?;
    let (server, tool) = rest.split_once('_')?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_string(), tool.to_string()))
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
            "see": "spec/dev/integrations/mcp-oauth.md"
        },
        "transports": {
            "stdio_long_lived": "POST /api/mcp/runtime/stdio/start { \"server\": \"name\" }",
            "http_sse": "GET /api/mcp/runtime/sse (phase next: heartbeat stream)"
        },
        "oauth_state": oauth_cell().lock().await.clone(),
    })
}

fn oauth_expires_at_past(state: &Value) -> bool {
    let Some(raw) = state.get("expires_at") else {
        return false;
    };
    if let Some(secs) = raw.as_i64() {
        let now = chrono::Utc::now().timestamp();
        return secs <= now;
    }
    if let Some(s) = raw.as_str() {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            return dt <= chrono::Utc::now();
        }
    }
    false
}

async fn oauth_refresh_if_needed(data_dir: &Path, state: &mut Value) {
    let refresh_token = state
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let Some(refresh_token) = refresh_token else {
        return;
    };
    if !oauth_expires_at_past(state) {
        return;
    }
    let token_endpoint = state
        .get("token_endpoint")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let Some(token_endpoint) = token_endpoint else {
        state["refresh_error"] = json!("missing token_endpoint");
        return;
    };
    let client_id = state
        .get("client_id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let mut form: Vec<(String, String)> = vec![
        ("grant_type".into(), "refresh_token".into()),
        ("refresh_token".into(), refresh_token),
    ];
    if !client_id.is_empty() {
        form.push(("client_id".into(), client_id));
    }
    if let Some(secret) = state.get("client_secret").and_then(|x| x.as_str()).filter(|s| !s.is_empty()) {
        form.push(("client_secret".into(), secret.to_string()));
    }
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            state["refresh_error"] = json!(format!("http client: {e}"));
            return;
        }
    };
    let resp = match client.post(&token_endpoint).form(&form).send().await {
        Ok(r) => r,
        Err(e) => {
            state["refresh_error"] = json!(format!("token request failed: {e}"));
            return;
        }
    };
    let status = resp.status();
    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            state["refresh_error"] = json!(format!("token response parse failed: {e}"));
            return;
        }
    };
    if !status.is_success() {
        state["refresh_error"] = json!({
            "http_status": status.as_u16(),
            "body": body,
        });
        return;
    }
    if let Some(at) = body.get("access_token").and_then(|x| x.as_str()) {
        state["access_token"] = json!(at);
    }
    if let Some(rt) = body.get("refresh_token").and_then(|x| x.as_str()) {
        state["refresh_token"] = json!(rt);
    }
    if let Some(exp) = body.get("expires_in").and_then(|x| x.as_i64()) {
        let at = chrono::Utc::now().timestamp() + exp;
        state["expires_at"] = json!(at);
    }
    state["status"] = json!("refreshed");
    state["updated_at"] = json!(chrono::Utc::now().to_rfc3339());
    if let Some(o) = state.as_object_mut() {
        o.remove("refresh_error");
    }
    let out = state.clone();
    let _ = oauth_persist(data_dir, &out).await;
}

async fn oauth_persist(data_dir: &Path, out: &Value) -> Result<(), String> {
    let p = oauth_state_path(data_dir);
    let raw = serde_json::to_string_pretty(out).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("json.tmp");
    tokio::fs::write(&tmp, raw)
        .await
        .map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    #[cfg(windows)]
    let _ = tokio::fs::remove_file(&p).await;
    tokio::fs::rename(&tmp, &p)
        .await
        .map_err(|e| format!("rename {} -> {}: {}", tmp.display(), p.display(), e))?;
    Ok(())
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
    oauth_refresh_if_needed(data_dir, &mut g).await;
    g.clone()
}

pub async fn oauth_put(data_dir: &Path, body: &Value) -> Result<Value, String> {
    let provider = body
        .get("provider")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string();
    let status = body
        .get("status")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("configured")
        .to_string();
    let mut g = oauth_cell().lock().await;
    let mut next = json!({
        "status": status,
        "provider": provider,
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });
    for key in [
        "access_token",
        "refresh_token",
        "expires_at",
        "token_endpoint",
        "client_id",
        "client_secret",
    ] {
        if let Some(v) = body.get(key) {
            next[key] = v.clone();
        }
    }
    *g = next;
    let out = g.clone();
    oauth_persist(data_dir, &out).await?;
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
    let _ = read_framed(&mut reader).await;

    let mut slot = cell().lock().await;
    if let Some(prev) = slot.take() {
        let mut c = prev.child;
        let _ = c.kill().await;
        let _ = c.wait().await;
    }
    *slot = Some(StdioState {
        server: server.to_string(),
        child,
        stdin,
        reader,
        next_id: AtomicU64::new(2),
    });
    Ok(json!({ "ok": true, "server": server }))
}

async fn read_framed<R: tokio::io::AsyncRead + Unpin>(
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
            return Err("connection closed".to_string());
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((_, v)) = trimmed.split_once(':') {
            if trimmed.to_ascii_lowercase().starts_with("content-length") {
                content_length = v.trim().parse().ok();
            }
        }
    }
    let len = content_length.ok_or_else(|| "no Content-Length".to_string())?;
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|e| format!("read body: {}", e))?;
    serde_json::from_slice(&body).map_err(|e| format!("json: {}", e))
}

async fn rpc_request(state: &mut StdioState, method: &str, params: Value) -> Result<Value, String> {
    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    let req = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    });
    write_framed(&mut state.stdin, &req).await.map_err(|e| e.to_string())?;
    read_framed(&mut state.reader).await
}

pub async fn tools_list() -> Result<Value, String> {
    let mut slot = cell().lock().await;
    let Some(state) = slot.as_mut() else {
        return Err("no stdio MCP server attached (POST /api/mcp/runtime/stdio/start)".to_string());
    };
    rpc_request(state, "tools/list", json!({})).await
}

pub async fn tools_call(tool_name: &str, arguments: Value) -> Result<Value, String> {
    let mut slot = cell().lock().await;
    let Some(state) = slot.as_mut() else {
        return Err("no stdio MCP server attached".to_string());
    };
    rpc_request(
        state,
        "tools/call",
        json!({ "name": tool_name, "arguments": arguments }),
    )
    .await
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
