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
    data_dir: std::path::PathBuf,
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    next_id: AtomicU64,
    healthy: bool,
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

#[derive(Clone, Debug, serde::Serialize)]
pub struct DiscoveredTool {
    pub name: String,
    pub server: String,
    pub tool: String,
    pub description: String,
    pub input_schema: Value,
}

static DISCOVERED: OnceLock<std::sync::RwLock<Vec<DiscoveredTool>>> = OnceLock::new();
fn tool_cache() -> &'static std::sync::RwLock<Vec<DiscoveredTool>> {
    DISCOVERED.get_or_init(|| std::sync::RwLock::new(Vec::new()))
}
pub fn discovered_tools() -> Vec<DiscoveredTool> {
    tool_cache()
        .read()
        .map(|tools| tools.clone())
        .unwrap_or_default()
}
pub fn discovered_tool(name: &str) -> Option<DiscoveredTool> {
    discovered_tools()
        .into_iter()
        .find(|tool| tool.name == name)
}

fn has_external_ref(value: &Value, depth: usize) -> bool {
    if depth > 64 {
        return true;
    }
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            (matches!(key.as_str(), "$dynamicRef" | "$recursiveRef")
                || (key == "$ref" && value.as_str().is_some_and(|r| !r.starts_with('#'))))
                || has_external_ref(value, depth + 1)
        }),
        Value::Array(values) => values.iter().any(|v| has_external_ref(v, depth + 1)),
        _ => false,
    }
}

struct NoExternalSchema;
impl jsonschema::SchemaResolver for NoExternalSchema {
    fn resolve(
        &self,
        _root: &Value,
        _url: &url::Url,
        _reference: &str,
    ) -> Result<std::sync::Arc<Value>, jsonschema::SchemaResolverError> {
        Err(anyhow::anyhow!("External MCP schemas are disabled"))
    }
}
fn compile_schema(schema: &Value) -> Result<jsonschema::JSONSchema, String> {
    jsonschema::JSONSchema::options()
        .with_resolver(NoExternalSchema)
        .compile(schema)
        .map_err(|_| "unsupported MCP input schema".into())
}

pub fn validate_arguments(tool: &DiscoveredTool, arguments: &Value) -> Result<(), String> {
    if !arguments.is_object() {
        return Err("MCP arguments must be a JSON object".into());
    }
    let compiled = compile_schema(&tool.input_schema)
        .map_err(|_| "unsupported MCP argument schema".to_string())?;
    let result = compiled.validate(arguments);
    match result {
        Ok(()) => Ok(()),
        Err(errors) => Err(format!(
            "MCP argument validation failed at: {}",
            errors
                .take(4)
                .map(|error| error.instance_path.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn cache_tools(server: &str, response: &Value) -> Result<usize, String> {
    let list = response
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .ok_or("invalid tools/list result")?;
    if list.len() > 256 {
        return Err("MCP catalogue exceeds 256 tools".into());
    }
    let mut tools = Vec::new();
    let mut names = std::collections::HashSet::new();
    for item in list {
        let bare = item["name"]
            .as_str()
            .filter(|name| !name.is_empty() && name.len() <= 128)
            .ok_or("invalid MCP tool name")?;
        let name = mcp_namespaced_tool(server, bare);
        if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || !names.insert(name.clone())
        {
            return Err(
                "MCP namespace contains unsupported characters or colliding tool names".into(),
            );
        }
        let schema = item
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({"type":"object"}));
        if !schema.is_object()
            || schema.to_string().len() > 64 * 1024
            || has_external_ref(&schema, 0)
        {
            return Err("MCP schema too large, too deep or uses external references".into());
        }
        compile_schema(&schema).map_err(|_| "unsupported MCP input schema")?;
        tools.push(DiscoveredTool {
            name,
            server: server.into(),
            tool: bare.into(),
            description: item["description"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(2000)
                .collect(),
            input_schema: schema,
        });
    }
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    let count = tools.len();
    *tool_cache()
        .write()
        .map_err(|_| "MCP catalogue lock unavailable")? = tools;
    Ok(count)
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

// MCP stdio uses newline-delimited JSON-RPC, not LSP Content-Length frames.
async fn write_framed(
    stdin: &mut (impl AsyncWriteExt + Unpin),
    msg: &Value,
) -> std::io::Result<()> {
    let body = msg.to_string();
    if body.len() > 8 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "MCP message too large",
        ));
    }
    stdin.write_all(body.as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await
}

pub async fn summary() -> Value {
    let g = cell().lock().await;
    json!({
        "stdio_server": g.as_ref().map(|s| s.server.as_str()),
        "stdio_healthy": g.as_ref().is_some_and(|state| state.healthy),
        "discovered_tools": discovered_tools(),
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
    if let Some(secret) = state
        .get("client_secret")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
    {
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
            < disk
                .get("updated_at")
                .and_then(|x| x.as_str())
                .unwrap_or("")
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
    let policy_path = data_dir.join("tools_policy.yaml");
    if policy_path.is_file() {
        match akasha_tools::ToolsPolicy::load_from_path(&policy_path) {
            Ok(policy) if !policy.can_attach_mcp_server(server) => {
                return Err(format!(
                    "MCP server {:?} denied by tools_policy.yaml (mcp_servers allow-list)",
                    server
                ));
            }
            Ok(_) => {}
            Err(e) => {
                return Err(format!("tools_policy.yaml: {}", e));
            }
        }
    }

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

    let mut command = Command::new(cmd);
    command.args(&args);
    if let Some(env) = entry.get("env").and_then(Value::as_object) {
        for (key, value) in env {
            let value = value.as_str().ok_or("MCP env values must be strings")?;
            command.env(key, value);
        }
    }
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
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
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "akasha-mcp-runtime", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    write_framed(&mut stdin, &init)
        .await
        .map_err(|e| format!("initialize write: {}", e))?;

    let mut reader = BufReader::new(stdout);
    let initialized =
        tokio::time::timeout(std::time::Duration::from_secs(20), read_framed(&mut reader))
            .await
            .map_err(|_| "MCP initialize timeout")??;
    if initialized.get("error").is_some()
        || initialized.get("result").is_none()
        || initialized["id"] != 1
    {
        return Err("MCP initialize rejected or malformed".into());
    }

    if !matches!(
        initialized
            .pointer("/result/protocolVersion")
            .and_then(Value::as_str),
        Some("2024-11-05" | "2025-03-26" | "2025-06-18")
    ) {
        return Err("MCP protocol version not supported".into());
    }

    // MCP spec requires the client to send notifications/initialized after the
    // initialize response and before any tool requests (tools/list, tools/call, …).
    let initialized_notif = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    write_framed(&mut stdin, &initialized_notif)
        .await
        .map_err(|e| format!("notifications/initialized write: {}", e))?;

    let mut slot = cell().lock().await;
    if let Some(prev) = slot.take() {
        let mut c = prev.child;
        let _ = c.kill().await;
        let _ = c.wait().await;
    }
    *slot = Some(StdioState {
        server: server.to_string(),
        data_dir: data_dir.to_path_buf(),
        child,
        stdin,
        reader,
        next_id: AtomicU64::new(2),
        healthy: true,
    });
    if let Ok(mut cache) = tool_cache().write() {
        cache.clear();
    }
    let discovery = if let Some(state) = slot.as_mut() {
        match discover_all(state).await {
            Ok(response) => cache_tools(server, &response),
            Err(error) => Err(error),
        }
    } else {
        Err("MCP state unavailable".into())
    };
    Ok(
        json!({ "ok": true, "server": server, "tool_count":discovery.as_ref().ok(), "discovery_error":discovery.err() }),
    )
}

async fn read_framed<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<Value, String> {
    let mut body = Vec::new();
    let n = (&mut *reader)
        .take(8 * 1024 * 1024 + 1)
        .read_until(b'\n', &mut body)
        .await
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("MCP connection closed".into());
    }
    if n > 8 * 1024 * 1024 || !body.ends_with(b"\n") {
        return Err("MCP response exceeds limit or is not newline delimited".into());
    }
    serde_json::from_slice(&body).map_err(|_| "invalid MCP JSON response".into())
}

async fn rpc_request(state: &mut StdioState, method: &str, params: Value) -> Result<Value, String> {
    if !state.healthy {
        return Err("MCP stream interrupted; reconnect the server".into());
    }
    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    let result = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        write_framed(&mut state.stdin, &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})).await.map_err(|e| e.to_string())?;
        for _ in 0..256 {
            let message = read_framed(&mut state.reader).await?;
            if message["id"] == id && message.get("method").is_none() {
                return Ok(message);
            }
            if let Some(request_id) = message.get("id") {
                let answer = if message["method"] == "ping" {json!({"jsonrpc":"2.0","id":request_id,"result":{}})}
                    else {json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":"Client method not supported"}})};
                write_framed(&mut state.stdin, &answer).await.map_err(|e| e.to_string())?;
            }
            // Notifications do not satisfy the pending response id.
        }
        Err("MCP notification limit exceeded".into())
    }).await.map_err(|_| "MCP request timeout; reconnect required".to_string()).and_then(|r| r);
    if result.is_err() {
        state.healthy = false;
        if let Ok(mut cache) = tool_cache().write() {
            cache.clear();
        }
    }
    result
}

async fn discover_all(state: &mut StdioState) -> Result<Value, String> {
    let mut cursor: Option<String> = None;
    let mut seen = std::collections::HashSet::new();
    let mut tools = Vec::new();
    for _ in 0..16 {
        let params = cursor
            .as_ref()
            .map(|value| json!({"cursor":value}))
            .unwrap_or_else(|| json!({}));
        let response = rpc_request(state, "tools/list", params).await?;
        let page = response
            .pointer("/result/tools")
            .and_then(Value::as_array)
            .ok_or("invalid MCP tools/list response")?;
        tools.extend(page.iter().cloned());
        if tools.len() > 256 {
            return Err("MCP catalogue exceeds 256 tools".into());
        }
        cursor = response
            .pointer("/result/nextCursor")
            .and_then(Value::as_str)
            .map(String::from);
        if let Some(ref value) = cursor {
            if !seen.insert(value.clone()) {
                return Err("MCP repeated pagination cursor".into());
            }
        } else {
            return Ok(json!({"jsonrpc":"2.0","result":{"tools":tools}}));
        }
    }
    Err("MCP tools/list exceeds 16 pages".into())
}

pub async fn tools_list() -> Result<Value, String> {
    let mut slot = cell().lock().await;
    let state = slot.as_mut().ok_or("no stdio MCP server attached")?;
    let discovered = discover_all(state).await.and_then(|response| {
        cache_tools(&state.server, &response)?;
        Ok(response)
    });
    if discovered.is_err() {
        if let Ok(mut cache) = tool_cache().write() {
            cache.clear();
        }
    }
    discovered
}

pub async fn tools_call(tool_name: &str, arguments: Value) -> Result<Value, String> {
    let mut slot = cell().lock().await;
    let Some(state) = slot.as_mut() else {
        return Err("no stdio MCP server attached".to_string());
    };
    let descriptor = discovered_tools()
        .into_iter()
        .find(|tool| tool.server == state.server && tool.tool == tool_name)
        .ok_or("tool not discovered on attached MCP server")?;
    validate_arguments(&descriptor, &arguments)?;
    check_current_policy(state, tool_name)?;
    rpc_request(
        state,
        "tools/call",
        json!({ "name": tool_name, "arguments": arguments }),
    )
    .await
}

pub async fn call_discovered(name: &str, arguments: Value) -> Result<Value, String> {
    let descriptor =
        discovered_tool(name).ok_or("MCP tool not discovered on the attached server")?;
    validate_arguments(&descriptor, &arguments)?;
    let mut slot = cell().lock().await;
    let state = slot.as_mut().ok_or("no stdio MCP server attached")?;
    if state.server != descriptor.server {
        return Err("MCP server changed; refresh capabilities".into());
    }
    check_current_policy(state, &descriptor.tool)?;
    rpc_request(
        state,
        "tools/call",
        json!({"name":descriptor.tool,"arguments":arguments}),
    )
    .await
}

fn check_current_policy(state: &StdioState, tool: &str) -> Result<(), String> {
    let path = state.data_dir.join("tools_policy.yaml");
    if path.is_file() {
        let policy = akasha_tools::ToolsPolicy::load_from_path(&path)
            .map_err(|_| "cannot read current MCP policy")?;
        if !policy.can_use_mcp_tool(&state.server, tool) {
            return Err("MCP tool denied by current server/tool policy".into());
        }
    }
    Ok(())
}

pub async fn stop_stdio_server() -> Value {
    if let Ok(mut cache) = tool_cache().write() {
        cache.clear();
    }
    let mut slot = cell().lock().await;
    if let Some(prev) = slot.take() {
        let mut c = prev.child;
        let _ = c.kill().await;
        let _ = c.wait().await;
    }
    json!({ "ok": true })
}
