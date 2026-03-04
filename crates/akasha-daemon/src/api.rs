//! Simple HTTP API: POST /api/message, GET /api/tasks/:id, GET / (health)

use akasha_core::{EventEnvelope, EventType};
use akasha_vault::Vault;
use akasha_llm::CompletionRequest;
use akasha_store::{TaskStatus, TaskStore};
use crate::agents::EventBus;
use crate::memory::ShortTermStore;
use crate::memory_actor::LongTermMemoryClient;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

async fn get_task_list(store_path: &Path) -> String {
    let store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let tasks = match store.get_all() {
        Ok(t) => t,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let list: Vec<serde_json::Value> = tasks
        .into_iter()
        .rev()
        .take(50)
        .map(|t| {
            serde_json::json!({
                "id": t.id.to_string(),
                "parent_task_id": t.parent_task_id.map(|u| u.to_string()),
                "status": t.status.as_str(),
                "assigned_agent": t.assigned_agent,
                "created_at": t.created_at.to_rfc3339(),
                "updated_at": t.updated_at.to_rfc3339()
            })
        })
        .collect();
    let body = serde_json::json!({ "tasks": list });
    json_response("200 OK", &body.to_string())
}

async fn get_task_events(events: &EventsCache, id: Uuid) -> String {
    let list: Vec<TaskEventEntry> = {
        let g = events.read().await;
        g.get(&id)
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    };
    let body = serde_json::json!({ "task_id": id.to_string(), "events": list });
    json_response("200 OK", &body.to_string())
}

pub const MAX_PROGRESS_PER_TASK: usize = 32;
pub const MAX_EVENTS_PER_TASK: usize = 64;

#[derive(Clone, serde::Serialize)]
pub struct ProgressEntry {
    pub progress_pct: u8,
    pub message: String,
}

pub type ProgressCache = Arc<RwLock<std::collections::HashMap<Uuid, VecDeque<ProgressEntry>>>>;

#[derive(Clone, serde::Serialize)]
pub struct TaskEventEntry {
    pub event_type: String,
    pub payload: Option<serde_json::Value>,
    pub at: String,
}

pub type EventsCache =
    Arc<RwLock<std::collections::HashMap<Uuid, VecDeque<TaskEventEntry>>>>;

pub fn new_progress_cache() -> ProgressCache {
    Arc::new(RwLock::new(std::collections::HashMap::new()))
}

pub fn new_events_cache() -> EventsCache {
    Arc::new(RwLock::new(std::collections::HashMap::new()))
}

/// Parsed HTTP request: method, path, body, and lowercase header map.
pub fn parse_request(buf: &[u8]) -> (String, String, Option<Vec<u8>>, std::collections::HashMap<String, String>) {
    let mut method = String::new();
    let mut path = String::new();
    let mut content_length = 0usize;
    let mut headers = std::collections::HashMap::new();
    let sep = b"\r\n\r\n";
    let header_end = buf.windows(sep.len()).position(|w| w == sep);
    let (header_slice, rest) = if let Some(i) = header_end {
        (&buf[..i], &buf[i + sep.len()..])
    } else {
        (buf as &[u8], &[][..])
    };
    let lines: Vec<&[u8]> = header_slice.split(|&b| b == b'\n').collect();
    for (i, line) in lines.iter().enumerate() {
        let line_str = String::from_utf8_lossy(line).trim_end_matches('\r').to_string();
        if i == 0 {
            let parts: Vec<&str> = line_str.splitn(3, ' ').collect();
            if parts.len() >= 2 {
                method = parts[0].to_string();
                path = parts[1].to_string();
            }
        } else if let Some((name, value)) = line_str.split_once(':') {
            let name = name.trim().to_lowercase();
            let value = value.trim().to_string();
            if name == "content-length" {
                if let Ok(n) = value.parse::<usize>() {
                    content_length = n;
                }
            }
            headers.insert(name, value);
        }
    }
    let body = if content_length > 0 && rest.len() >= content_length {
        Some(rest[..content_length].to_vec())
    } else {
        None
    };
    (method, path, body, headers)
}

pub fn json_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    )
}

/// Liste des outils disponibles (source unique pour le prompt et la doc).
/// Format: une ligne par outil "nom — usage".
pub const AVAILABLE_TOOLS: &[(&str, &str)] = &[
    ("read_file", "read_file <path> — lire le contenu d'un fichier texte"),
    ("write_file", "write_file <path> <content> — écrire du texte dans un fichier (path puis contenu)"),
    ("search_files", "search_files <dir> <pattern> — chercher des fichiers (glob) sous un répertoire"),
    ("run_command", "run_command <cmd> [arg1 arg2 ...] — exécuter une commande (autorisée par la politique)"),
    ("file_diff", "file_diff <path_a> <path_b> — diff texte entre deux fichiers"),
];

fn available_tools_instruction() -> String {
    AVAILABLE_TOOLS
        .iter()
        .map(|(_, desc)| *desc)
        .collect::<Vec<_>>()
        .join(" ; ")
}

/// Parse tool calls from LLM response: lines "TOOL: tool_name arg1 arg2 ...".
fn parse_tool_calls(response: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for line in response.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("TOOL:") {
            let rest = rest.trim();
            let parts: Vec<String> = rest.split_whitespace().map(String::from).collect();
            if let Some((name, args)) = parts.split_first() {
                out.push((name.clone(), args.to_vec()));
            }
        }
    }
    out
}

/// Execute one tool call via ToolExecutor. Returns a short result string for the LLM context.
async fn execute_tool_call(
    executor: &std::sync::Arc<akasha_tools::ToolExecutor>,
    tool_name: &str,
    args: &[String],
) -> String {
    use std::path::Path;
    let path_arg = |i: usize| args.get(i).map(|s| Path::new(s.as_str()));
    match tool_name {
        "read_file" => {
            if let Some(p) = path_arg(0) {
                match executor.read_file(p).await {
                    Ok((content, res)) => {
                        if res.success {
                            format!("[read_file {}] {} chars: {}", p.display(), content.len(), if content.len() <= 500 { content.as_str() } else { &content[..500] })
                        } else {
                            format!("[read_file] denied or error: {}", res.summary)
                        }
                    }
                    Err(e) => format!("[read_file] error: {}", e),
                }
            } else {
                "[read_file] usage: read_file <path>".to_string()
            }
        }
        "run_command" => {
            let cmd = args.get(0).map(String::as_str).unwrap_or("");
            let cmd_args: Vec<String> = args.iter().skip(1).cloned().collect();
            match executor.run_command(cmd, &cmd_args, None).await {
                Ok((out, res)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    if res.success {
                        format!("[run_command {}] stdout: {} stderr: {}", cmd, stdout.trim(), stderr.trim())
                    } else {
                        format!("[run_command] {} stderr: {}", res.summary, stderr.trim())
                    }
                }
                Err(e) => format!("[run_command] error: {}", e),
            }
        }
        "search_files" => {
            let dir = path_arg(0).unwrap_or(Path::new("."));
            let pattern = args.get(1).map(String::as_str).unwrap_or("*");
            match executor.search_files(dir, pattern).await {
                Ok((paths, res)) => {
                    if res.success {
                        let list: Vec<String> = paths.iter().take(20).map(|p| p.display().to_string()).collect();
                        format!("[search_files] found {}: {:?}", paths.len(), list)
                    } else {
                        format!("[search_files] {}", res.summary)
                    }
                }
                Err(e) => format!("[search_files] error: {}", e),
            }
        }
        "write_file" => {
            let path = match path_arg(0) {
                Some(p) => p,
                None => return "[write_file] usage: write_file <path> <content>".to_string(),
            };
            let content = args.get(1..).map(|a| a.join(" ")).unwrap_or_default();
            match executor.write_file(path, &content).await {
                Ok(res) => {
                    if res.success {
                        format!("[write_file {}] {}", path.display(), res.summary)
                    } else {
                        format!("[write_file] {}", res.summary)
                    }
                }
                Err(e) => format!("[write_file] error: {}", e),
            }
        }
        "file_diff" => {
            let path_a = path_arg(0);
            let path_b = path_arg(1);
            match (path_a, path_b) {
                (Some(a), Some(b)) => match executor.file_diff(a, b).await {
                    Ok((diff, res)) => {
                        if res.success {
                            let preview = if diff.len() <= 400 { diff.as_str() } else { &diff[..400] };
                            format!("[file_diff] {} — {}", res.summary, preview)
                        } else {
                            format!("[file_diff] {}", res.summary)
                        }
                    }
                    Err(e) => format!("[file_diff] error: {}", e),
                },
                _ => "[file_diff] usage: file_diff <path_a> <path_b>".to_string(),
            }
        }
        _ => {
            let names: Vec<&str> = AVAILABLE_TOOLS.iter().map(|(n, _)| *n).collect();
            format!("[{}] unknown tool. Available: {}.", tool_name, names.join(", "))
        }
    }
}

/// Compact short-term memory when it would exceed context: summarize oldest turns via LLM and replace in store.
/// Optionally promote the summary to long-term memory (embed + store).
async fn compact_short_term_if_needed(
    short_term: &Arc<ShortTermStore>,
    session_id: &str,
    llm_router: &akasha_llm::LLMRouter,
    new_message_tokens: usize,
    long_term_client: Option<&LongTermMemoryClient>,
) {
    const MAX_CONTEXT_DEFAULT: usize = 8192;
    let max_context_tokens = std::env::var("AKASHA_MAX_CONTEXT_TOKENS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(MAX_CONTEXT_DEFAULT);
    let trigger = (max_context_tokens as f64 * short_term.compaction_trigger_ratio) as usize;

    let turns = short_term.get_turns(session_id).await;
    let history_tokens = ShortTermStore::turns_tokens(&turns);
    if history_tokens + new_message_tokens <= trigger || turns.len() <= 2 {
        return;
    }

    let to_summarize = turns.len() / 2;
    let old_turns: Vec<_> = turns.into_iter().take(to_summarize).collect();
    let blob = ShortTermStore::turns_to_context(&old_turns);
    let summary_prompt = format!(
        "Résume en un court paragraphe en français, en gardant les faits importants et décisions:\n\n{}",
        blob
    );
    let req = CompletionRequest {
        prompt: summary_prompt,
        max_tokens: Some(512),
        temperature: Some(0.2),
    };
    match llm_router.complete(&req).await {
        Ok(resp) => {
            let summary = resp.text.trim();
            if !summary.is_empty() {
                short_term.replace_oldest_with_summary(session_id, summary.to_string(), to_summarize).await;
                tracing::debug!(session_id, to_summarize, "Short-term memory compacted");
                // Promote summary to long-term memory (spec 06)
                if let Some(client) = long_term_client {
                    let summary = summary.to_string();
                    let client = client.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = client.promote(summary, "compaction".to_string()) {
                            tracing::warn!(error = %e, "Long-term promote after compaction failed");
                        }
                    })
                    .await
                    .ok();
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "Compaction LLM failed, keeping full history"),
    }
}

/// Run LLM completion for a user message, with short-term + long-term memory (and compaction), optional tool-use loop. Push reply as progress, mark task completed.
pub(crate) async fn run_message_via_llm(
    bus: EventBus,
    llm_router: Arc<akasha_llm::LLMRouter>,
    store_path: std::path::PathBuf,
    task_id: Uuid,
    message: String,
    session_id: String,
    short_term: Option<std::sync::Arc<ShortTermStore>>,
    long_term_client: Option<LongTermMemoryClient>,
    tools_executor: Option<std::sync::Arc<akasha_tools::ToolExecutor>>,
) {
    let store = match TaskStore::open(&store_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "LLM task: store open failed");
            return;
        }
    };
    let _ = store.update_status(task_id, TaskStatus::Running);

    // Emit user message so TUI/API can show "what this task is about"
    let _ = bus.send(
        EventEnvelope::new(
            EventType::UserRequestReceived,
            Some(serde_json::json!({ "message": message })),
        )
        .with_correlation(task_id),
    );

    // Progress pour indiquer que la génération a démarré (modèle local peut charger au premier appel).
    let _ = bus.send(
        EventEnvelope::new(
            EventType::ProgressUpdate,
            Some(serde_json::json!({
                "task_id": task_id.to_string(),
                "progress_pct": 10,
                "message": "Génération de la réponse…"
            })),
        )
        .with_correlation(task_id),
    );

    let max_tokens = std::env::var("AKASHA_MAX_RESPONSE_TOKENS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(4096);

    let tool_instruction = if tools_executor.is_some() {
        format!(
            "\n\nYou may request tools by writing a line: TOOL: tool_name arg1 arg2 ...\nAvailable: {}.\nIf you need no tool, reply normally with your answer.\n\
             If write_file or read_file returns \"path not allowed by policy\" or \"denied\", tell the user that they CAN configure this: edit the file tools_policy.yaml \
             (in the Akasha data directory) and add path prefixes under allowed_write_paths or allowed_read_paths. It is not impossible — the user controls this YAML file.",
            available_tools_instruction()
        )
    } else {
        String::new()
    };

    // Build prompt with short-term + long-term memory (spec 06)
    let mut context_prefix = String::new();
    // Long-term: retrieve top-k relevant memories by embedding similarity
    if let Some(ref client) = long_term_client {
        let msg = message.clone();
        let client = client.clone();
        let results = tokio::task::spawn_blocking(move || client.search(msg, 5))
            .await
            .ok()
            .unwrap_or_default();
        if !results.is_empty() {
            context_prefix.push_str("[Mémoire à long terme]\n");
            for content in &results {
                context_prefix.push_str("- ");
                context_prefix.push_str(&content.replace('\n', " "));
                context_prefix.push_str("\n");
            }
            context_prefix.push_str("\n");
        }
    }
    if let Some(ref st) = short_term {
        let new_msg_tokens = ShortTermStore::estimate_tokens(&message);
        compact_short_term_if_needed(
            st,
            &session_id,
            &llm_router,
            new_msg_tokens,
            long_term_client.as_ref(),
        )
        .await;
        let turns = st.get_turns(&session_id).await;
        let short_ctx = ShortTermStore::turns_to_context(&turns);
        if !short_ctx.is_empty() {
            context_prefix.push_str(short_ctx.trim_end());
            context_prefix.push_str("\n\n");
        }
    }
    let mut current_prompt = if context_prefix.is_empty() {
        message.clone()
    } else {
        format!("{}\nUtilisateur:\n{}", context_prefix.trim_end(), message)
    };
    let reply_text;
    const MAX_TOOL_ROUNDS: u32 = 3;
    let mut round = 0u32;

    let llm_timeout_secs = std::env::var("AKASHA_LLM_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(300);
    let llm_timeout = std::time::Duration::from_secs(llm_timeout_secs);

    loop {
        let request = CompletionRequest {
            prompt: format!("{}{}", current_prompt, tool_instruction),
            max_tokens: Some(max_tokens),
            temperature: Some(0.7),
        };
        let response = match tokio::time::timeout(llm_timeout, llm_router.complete(&request)).await {
            Ok(Ok(resp)) => resp.text.trim().to_string(),
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "LLM completion failed");
                reply_text = format!("Sorry, I couldn't get a response (error: {}).", e);
                break;
            }
            Err(_) => {
                tracing::warn!(timeout_secs = llm_timeout_secs, "LLM completion timed out (model loading or slow reply)");
                reply_text = format!(
                    "Délai dépassé ({} s). Modèle local en cours de chargement ou requête trop longue. Réessayez ou configurez Ollama.",
                    llm_timeout_secs
                );
                break;
            }
        };

        let tool_calls = tools_executor.as_ref().and_then(|_| {
            let calls = parse_tool_calls(&response);
            if calls.is_empty() { None } else { Some(calls) }
        });

        if let (Some(exec), Some(calls)) = (tools_executor.as_ref(), tool_calls) {
            round += 1;
            let mut tool_results = Vec::new();
            for (name, args) in &calls {
                let res = execute_tool_call(exec, name, args).await;
                tool_results.push(res);
            }
            let results_blob = tool_results.join("\n");
            current_prompt = format!("{}\n\nTool results:\n{}\n\nProvide your final answer to the user (no more TOOL: lines).", response, results_blob);
            if round >= MAX_TOOL_ROUNDS {
                reply_text = format!("{}\n\n[Tool round limit reached; final answer above.]", response);
                break;
            }
            continue;
        }

        reply_text = response;
        break;
    }

    let reply_text = if reply_text.is_empty() {
        tracing::warn!("LLM returned empty text");
        "No response from the model. Check Ollama or your LLM provider.".to_string()
    } else {
        if std::env::var("AKASHA_LOG_LLM_RESPONSE").as_deref() == Ok("1") {
            tracing::info!(response = %reply_text, "LLM full response");
        } else {
            tracing::debug!(response = %reply_text, "LLM full response (set AKASHA_LOG_LLM_RESPONSE=1 to log at info)");
        }
        reply_text
    };

    // Persist this exchange in short-term memory (spec 06)
    if let Some(ref st) = short_term {
        st.append(&session_id, "user", message).await;
        st.append(&session_id, "assistant", reply_text.clone()).await;
    }

    let _ = bus.send(
        EventEnvelope::new(
            EventType::ProgressUpdate,
            Some(serde_json::json!({
                "task_id": task_id.to_string(),
                "progress_pct": 100,
                "message": reply_text
            })),
        )
        .with_correlation(task_id),
    );
    let _ = bus.send(
        EventEnvelope::new(
            EventType::TaskCompleted,
            Some(serde_json::json!({
                "task_id": task_id.to_string(),
                "status": "completed"
            })),
        )
        .with_correlation(task_id),
    );
    let _ = store.update_status(task_id, TaskStatus::Completed);
}

/// Optional channel to trigger daemon shutdown (for POST /api/restart).
pub type RestartTx = Option<tokio::sync::mpsc::Sender<()>>;

pub async fn handle_api(
    method: &str,
    path: &str,
    body: Option<Vec<u8>>,
    headers: &std::collections::HashMap<String, String>,
    store_path: &Path,
    progress: &ProgressCache,
    events: &EventsCache,
    main_agent: &crate::agents::MainAgent,
    channel_config: &crate::channels::ChannelConfig,
    plugin_registry: &std::sync::Arc<crate::plugins::PluginRegistry>,
    llm_router: &std::sync::Arc<akasha_llm::LLMRouter>,
    rag_pack: &std::sync::Arc<akasha_rag::RagPack>,
    ollama_base_url: Option<&str>,
    spec_dir: &Path,
    restart_tx: RestartTx,
    _tools_executor: Option<&std::sync::Arc<akasha_tools::ToolExecutor>>,
    skill_registry: &std::sync::Arc<crate::skills::SkillRegistry>,
) -> String {
    let data_dir = store_path.parent().unwrap_or_else(|| store_path.as_ref());

    if method == "GET" && (path == "/" || path.is_empty()) {
        return json_response("200 OK", r#"{"status":"ok"}"#);
    }

    // GET /api/status — same as / but explicit for slash commands
    if method == "GET" && path == "/api/status" {
        return json_response("200 OK", r#"{"status":"ok"}"#);
    }

    // GET /api/doctor — health checks from daemon (for slash /doctor)
    if method == "GET" && path == "/api/doctor" {
        let mut checks: Vec<serde_json::Value> = Vec::new();
        checks.push(serde_json::json!({ "id": "daemon", "ok": true, "description": "Daemon running" }));

        let ollama_ok = if let Some(u) = ollama_base_url {
            let test_url = format!("{}/api/tags", u.trim_end_matches('/'));
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new());
            client.get(&test_url).send().await.map(|r| r.status().is_success()).unwrap_or(false)
        } else {
            false
        };
        checks.push(serde_json::json!({
            "id": "ollama",
            "ok": ollama_ok,
            "description": if ollama_ok { "Ollama reachable" } else { "Ollama unreachable" }
        }));

        let vault_ok = akasha_vault::open_vault(data_dir).is_ok();
        checks.push(serde_json::json!({
            "id": "vault",
            "ok": vault_ok,
            "description": if vault_ok { "Vault open" } else { "Vault error" }
        }));

        let spec_ok = spec_dir.exists();
        checks.push(serde_json::json!({
            "id": "spec_dir",
            "ok": spec_ok,
            "description": if spec_ok { "Spec directory present" } else { "Spec directory missing" }
        }));

        let embedded_available = llm_router.embedded_available();
        let embedded_loaded = llm_router.embedded_loaded();
        let embedded_desc = if !embedded_available {
            "Embedded model not available (compile with embedded feature, run on Linux/WSL2)"
        } else if embedded_loaded {
            "Embedded model loaded and ready"
        } else {
            "Embedded model available; loads on first use (first request may take 5–15 min)"
        };
        checks.push(serde_json::json!({
            "id": "embedded_llm",
            "ok": embedded_available,
            "description": embedded_desc
        }));

        let all_ok = checks.iter().all(|c| c.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));
        let body_json = serde_json::json!({ "ok": all_ok, "checks": checks }).to_string();
        return format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body_json.len(),
            body_json
        );
    }

    // GET /api/config — list vars from data_dir/akasha.env
    if method == "GET" && path == "/api/config" {
        let env_path = data_dir.join("akasha.env");
        let mut vars = std::collections::HashMap::new();
        if let Ok(s) = std::fs::read_to_string(&env_path) {
            for line in s.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
                    vars.insert(k.trim().to_string(), v);
                }
            }
        }
        let body_json = serde_json::to_string(&serde_json::json!({ "vars": vars })).unwrap_or_else(|_| "{}".into());
        return format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body_json.len(),
            body_json
        );
    }

    // POST /api/config — set one var in akasha.env (body: {"key": "K", "value": "V"})
    if method == "POST" && path == "/api/config" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let key = body_json.as_ref().and_then(|j| j.get("key")).and_then(|v| v.as_str()).map(String::from);
        let value = body_json.as_ref().and_then(|j| j.get("value")).and_then(|v| v.as_str()).map(String::from);
        match (key, value) {
            (Some(k), Some(v)) if !k.is_empty() => {
                let env_path = data_dir.join("akasha.env");
                let mut lines: Vec<String> = if env_path.exists() {
                    std::fs::read_to_string(&env_path).unwrap_or_default().lines().map(String::from).collect()
                } else {
                    vec!["# akasha.env".to_string(), "".to_string()]
                };
                let new_line = format!("{}={}", k, v);
                let mut found = false;
                for line in lines.iter_mut() {
                    if line.trim_start().starts_with(&format!("{}=", k)) {
                        *line = new_line.clone();
                        found = true;
                        break;
                    }
                }
                if !found {
                    lines.push(new_line);
                }
                if std::fs::write(&env_path, lines.join("\n")).is_ok() {
                    return json_response("200 OK", &serde_json::json!({ "ok": true, "key": k }).to_string());
                }
            }
            _ => {}
        }
        return json_response("400 Bad Request", r#"{"error":"missing key or value"}"#);
    }

    // GET /api/vault/keys — list vault key names (no values)
    if method == "GET" && path == "/api/vault/keys" {
        let keys = akasha_vault::open_vault(data_dir).ok().and_then(|v| v.list_keys().ok()).unwrap_or_default();
        let body_json = serde_json::json!({ "keys": keys }).to_string();
        return format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body_json.len(),
            body_json
        );
    }

    // POST /api/restart — signal daemon to exit (supervisor restarts it)
    if method == "POST" && path == "/api/restart" {
        if let Some(ref tx) = restart_tx {
            let _ = tx.send(()).await;
            return json_response("200 OK", r#"{"ok":true,"message":"Redémarrage demandé"}"#);
        }
        return json_response("503 Service Unavailable", r#"{"error":"restart_not_available"}"#);
    }

    // GET /api/docs — user documentation (markdown), for TUI and web UI
    if method == "GET" && path == "/api/docs" {
        let content = std::fs::read_to_string(spec_dir.join("user_guide.md"))
            .ok()
            .or_else(|| {
                spec_dir
                    .parent()
                    .and_then(|p| std::fs::read_to_string(p.join("docs").join("user_guide.md")).ok())
            })
            .unwrap_or_else(|| {
                "# Documentation\n\nDocumentation non disponible. Voir README et spec/onboarding.md dans le dépôt.\n"
                    .to_string()
            });
        let body_json = serde_json::json!({ "content": content }).to_string();
        return format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body_json.len(),
            body_json
        );
    }

    // Phase 4: Slack slash command
    if method == "POST" && path == "/channels/slack/command" {
        if let Some(ref secret) = channel_config.slack_signing_secret {
            let sig = headers.get("x-slack-signature").map(String::as_str);
            return crate::channels::slack::handle_slack_command(
                body,
                sig,
                secret,
                channel_config.port,
                main_agent,
                store_path,
            );
        }
        return json_response("404 Not Found", r#"{"error":"slack_not_configured"}"#);
    }

    if method == "POST" && path == "/api/message" {
        let body_json = body.as_deref().and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let message = body_json
            .as_ref()
            .and_then(|v| v.get("message").and_then(|v| v.as_str().map(String::from)))
            .unwrap_or_default();
        let session_id = body_json
            .as_ref()
            .and_then(|v| v.get("session_id").and_then(|v| v.as_str().map(String::from)))
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if let Err(e) = akasha_core::check_prompt_injection(&message) {
            let body = serde_json::json!({ "error": "prompt_injection_rejected", "detail": e.to_string() });
            return json_response("400 Bad Request", &body.to_string());
        }
        let correlation_id = uuid::Uuid::new_v4();
        // User talks only to orchestrator: ack immediately, delegate to conversation worker in background (non-blocking). session_id used for short-term memory.
        match main_agent.handle_message(store_path, &message, correlation_id, true, &session_id) {
            Ok(task_id) => {
                let body = serde_json::json!({
                    "ack": true,
                    "task_id": task_id.to_string(),
                    "session_id": session_id,
                    "message": "Je prends en compte votre demande."
                });
                return json_response("200 OK", &body.to_string());
            }
            Err(_) => return json_response("500 Internal Server Error", r#"{"error":"handle_failed"}"#),
        }
    }

    if method == "GET" && path == "/api/tasks" {
        return get_task_list(store_path).await;
    }
    if method == "GET" && path.starts_with("/api/tasks/") {
        let rest = path.trim_start_matches("/api/tasks/");
        let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if let Some(&id_str) = parts.first() {
            if let Ok(id) = Uuid::parse_str(id_str) {
                if parts.get(1) == Some(&"events") {
                    return get_task_events(events, id).await;
                }
                return get_task_status(store_path, progress, id).await;
            }
        }
    }

    // Phase 5: Plugins
    if method == "GET" && path == "/api/plugins" {
        let list = plugin_registry.list();
        let body = serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string());
        return json_response("200 OK", &body);
    }
    if method == "POST" && path == "/api/plugins/reload" {
        plugin_registry.reload();
        return json_response("200 OK", r#"{"reloaded":true}"#);
    }

    // Phase D: Skills (loadable skills for agents)
    if method == "GET" && path == "/api/skills" {
        let list = skill_registry.list().await;
        let body = serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string());
        return json_response("200 OK", &body);
    }

    // Liste des outils machine disponibles (Phase A)
    if method == "GET" && path == "/api/tools" {
        let list: Vec<serde_json::Value> = AVAILABLE_TOOLS
            .iter()
            .map(|(name, desc)| serde_json::json!({ "name": name, "description": desc }))
            .collect();
        let body = serde_json::to_string(&serde_json::json!({ "tools": list })).unwrap_or_else(|_| "{}".to_string());
        return json_response("200 OK", &body);
    }

    // Phase 6: LLM completion via router
    if method == "POST" && path == "/api/complete" {
        let body = match body.as_deref().and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok()) {
            Some(b) => b,
            None => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
        };
        let prompt = body
            .get("prompt")
            .or(body.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let req = akasha_llm::CompletionRequest {
            prompt,
            max_tokens: body.get("max_tokens").and_then(|v| v.as_u64()).map(|n| n as u32),
            temperature: body.get("temperature").and_then(|v| v.as_f64()).map(|f| f as f32),
        };
        match llm_router.complete(&req).await {
            Ok(resp) => {
                let body = serde_json::json!({
                    "text": resp.text,
                    "model_used": resp.model_used,
                    "usage": resp.usage
                });
                return json_response("200 OK", &body.to_string());
            }
            Err(e) => {
                let body = serde_json::json!({ "error": "completion_failed", "detail": e });
                return json_response("502 Bad Gateway", &body.to_string());
            }
        }
    }

    if method == "GET" && path == "/api/router/metrics" {
        let list = llm_router.metrics().list();
        let body = serde_json::to_string(&list).unwrap_or_else(|_| "{}".to_string());
        return json_response("200 OK", &body);
    }

    // GET /api/router/embedded-status — whether embedded LLM is compiled, and if already loaded (for diagnostics)
    if method == "GET" && path == "/api/router/embedded-status" {
        let available = llm_router.embedded_available();
        let loaded = llm_router.embedded_loaded();
        let hint = if !available {
            "Recompile daemon with feature 'embedded', run on Linux/WSL2; or use Ollama/cloud"
        } else if loaded {
            "Embedded model loaded and ready for /advice and conversation"
        } else {
            "Embedded model will load on first use (first request may take 5–15 min: download + load). Wait or increase AKASHA_LLM_TIMEOUT_SECS."
        };
        let body = serde_json::json!({
            "embedded_registered": true,
            "embedded_available": available,
            "embedded_loaded": loaded,
            "hint": hint
        });
        return json_response("200 OK", &body.to_string());
    }

    // POST /api/router/embedded/reload — unload embedded model; next request will load it again
    if method == "POST" && path == "/api/router/embedded/reload" {
        llm_router.embedded_unload();
        let body = serde_json::json!({
            "ok": true,
            "message": "Embedded model unloaded. Next request will load it again."
        });
        return json_response("200 OK", &body.to_string());
    }

    // POST /api/router/route — set primary provider/model for a task type (body: { "category", "provider", "model" })
    if method == "POST" && path == "/api/router/route" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let category = body_json.as_ref().and_then(|j| j.get("category")).and_then(|v| v.as_str()).map(String::from);
        let provider = body_json.as_ref().and_then(|j| j.get("provider")).and_then(|v| v.as_str()).map(String::from);
        let model = body_json.as_ref().and_then(|j| j.get("model")).and_then(|v| v.as_str()).map(String::from);
        match (category, provider, model) {
            (Some(cat), Some(prov), Some(modl)) if !cat.is_empty() && !prov.is_empty() && !modl.is_empty() => {
                if !llm_router.is_provider_registered(&prov) {
                    let body_err = serde_json::json!({ "ok": false, "error": format!("unknown provider '{}'", prov) });
                    return json_response("400 Bad Request", &body_err.to_string());
                }
                let entry = akasha_llm::config::RouteEntry {
                    provider: prov.clone(),
                    model: modl.clone(),
                    config: None,
                };
                llm_router.set_primary_route(&cat, entry.clone());
                let router_path = data_dir.join("llm_router.yaml");
                let mut config = akasha_llm::config::RoutingConfig::load_from_path(&router_path)
                    .unwrap_or_else(|_| akasha_llm::config::RoutingConfig::default_config());
                config.set_primary_route(&cat, entry);
                if let Err(e) = config.save_to_path(&router_path) {
                    let body_err = serde_json::json!({ "ok": false, "error": format!("save failed: {}", e) });
                    return json_response("500 Internal Server Error", &body_err.to_string());
                }
                let body_ok = serde_json::json!({
                    "ok": true,
                    "category": cat,
                    "provider": prov,
                    "model": modl,
                    "message": "Route updated (in memory and saved to llm_router.yaml)."
                });
                return json_response("200 OK", &body_ok.to_string());
            }
            _ => {
                let body_err = serde_json::json!({ "error": "missing or empty category, provider, or model" });
                return json_response("400 Bad Request", &body_err.to_string());
            }
        }
    }

    // GET /api/router/routes — list primary + fallback per category (for CLI and TUI "models by category")
    if method == "GET" && path == "/api/router/routes" {
        let routes = llm_router.routes_by_category();
        let body = serde_json::to_string(&routes).unwrap_or_else(|_| "{}".to_string());
        return json_response("200 OK", &body);
    }

    // GET /api/router/models — list models from all providers (config + Ollama live when available)
    if method == "GET" && path == "/api/router/models" {
        let mut providers: std::collections::HashMap<String, Vec<String>> =
            llm_router.list_models_from_config();
        if let Some(base_url) = ollama_base_url
            .map(String::from)
            .or_else(|| llm_router.ollama_base_url())
        {
            let base_url = base_url.trim_end_matches('/').to_string();
            let url = format!("{}/api/tags", base_url);
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new());
            if let Ok(resp) = client.get(&url).send().await {
                if resp.status().is_success() {
                    if let Ok(json) = resp.json::<serde_json::Value>().await {
                        let list = json.get("models").and_then(|m| m.as_array());
                        if let Some(arr) = list {
                            let models: Vec<String> = arr
                                .iter()
                                .filter_map(|m| {
                                    m.as_str()
                                        .map(String::from)
                                        .or_else(|| m.get("name").and_then(|n| n.as_str()).map(String::from))
                                        .or_else(|| m.get("model").and_then(|n| n.as_str()).map(String::from))
                                })
                                .collect();
                            if !models.is_empty() {
                                providers.insert("ollama".to_string(), models);
                            }
                        }
                    }
                }
            }
        }
        let body = serde_json::json!({ "providers": providers });
        return json_response("200 OK", &body.to_string());
    }

    // GET /api/router/ollama/models — list models from configured Ollama (kept for backward compat)
    if method == "GET" && path == "/api/router/ollama/models" {
        let base_url = ollama_base_url
            .map(String::from)
            .or_else(|| llm_router.ollama_base_url());
        let base_url = match base_url {
            Some(u) => u.trim_end_matches('/').to_string(),
            None => {
                return json_response("404 Not Found", r#"{"error":"ollama_not_configured"}"#);
            }
        };
        let url = format!("{}/api/tags", base_url);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    let models: Vec<String> = json
                        .get("models")
                        .and_then(|m| m.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|m| {
                                    m.get("name")
                                        .or_else(|| m.get("model"))
                                        .and_then(|n| n.as_str().map(String::from))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let body = serde_json::json!({ "base_url": base_url, "models": models });
                    return json_response("200 OK", &body.to_string());
                }
            }
            _ => {}
        }
        return json_response(
            "502 Bad Gateway",
            &serde_json::json!({ "error": "ollama_unreachable", "base_url": base_url }).to_string(),
        );
    }

    // GET /api/router/ollama/show?model=xxx — Ollama model details (context length, num_ctx)
    if method == "GET" && path.starts_with("/api/router/ollama/show") {
        let base_url = ollama_base_url
            .map(String::from)
            .or_else(|| llm_router.ollama_base_url());
        let base_url = match base_url {
            Some(u) => u.trim_end_matches('/').to_string(),
            None => {
                return json_response("404 Not Found", r#"{"error":"ollama_not_configured"}"#);
            }
        };
        let model = path
            .split('?')
            .nth(1)
            .and_then(|q| {
                q.split('&')
                    .find(|p| p.starts_with("model="))
                    .map(|p| p.trim_start_matches("model=").to_string())
            })
            .unwrap_or_else(|| "".to_string());
        if model.is_empty() {
            return json_response("400 Bad Request", r#"{"error":"missing query: model=<name>"}"#);
        }
        let url = format!("{}/api/show", base_url);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        match client.post(&url).json(&serde_json::json!({ "model": model })).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    // Extract context_length from model_info (e.g. "gemma3.context_length": 131072)
                    let model_info = json.get("model_info").and_then(|m| m.as_object());
                    let context_length = model_info.and_then(|m| {
                        m.iter()
                            .find(|(k, _)| k.ends_with("context_length"))
                            .and_then(|(_, v)| v.as_u64())
                    });
                    // num_ctx from parameters string (e.g. "num_ctx 2048")
                    let parameters = json.get("parameters").and_then(|p| p.as_str()).unwrap_or("");
                    let num_ctx = parameters
                        .lines()
                        .find(|l| l.trim().starts_with("num_ctx"))
                        .and_then(|l| l.trim().trim_start_matches("num_ctx").trim().split_whitespace().next())
                        .and_then(|s| s.parse::<u64>().ok());
                    let body = serde_json::json!({
                        "model": model,
                        "base_url": base_url,
                        "context_length_max": context_length,
                        "num_ctx": num_ctx,
                        "parameters_preview": if parameters.len() > 200 { format!("{}...", &parameters[..200]) } else { parameters.to_string() }
                    });
                    return json_response("200 OK", &body.to_string());
                }
            }
            _ => {}
        }
        return json_response(
            "502 Bad Gateway",
            &serde_json::json!({ "error": "ollama_show_failed", "model": model, "base_url": base_url }).to_string(),
        );
    }

    // Phase 8: Diagnostic advice (RAG + core model, guardrails)
    if (method == "POST" || method == "GET") && path == "/api/diagnostic/advice" {
        let health_json: serde_json::Value = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
            .and_then(|v: serde_json::Value| v.get("health").cloned())
            .unwrap_or_else(|| serde_json::json!({ "checks": [] }));
        let health_str = serde_json::to_string_pretty(&health_json).unwrap_or_default();
        let all_ok = health_json
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or_else(|| {
                health_json
                    .get("checks")
                    .and_then(|c| c.as_array())
                    .map(|a| a.iter().all(|c| c.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)))
                    .unwrap_or(false)
            });
        let summary = if all_ok {
            "→ All checks PASSED; no action required."
        } else {
            "→ Some checks FAILED; suggest fixes only for those."
        };
        let chunks = akasha_rag::retrieve(rag_pack, "diagnostic health runbook error daemon", 5);
        let doc_context: String = chunks
            .iter()
            .map(|c| format!("--- {} ---\n{}", c.path, c.content))
            .fold(String::new(), |a, b| a + "\n" + &b);
        let prompt = format!(
            r#"You are the Akasha diagnostic assistant. The JSON below is the ACTUAL current health state of the system (just ran). Each check has "ok": true (working) or "ok": false (missing/failed).

RULES:
- Base your answer ONLY on this state. Do NOT suggest fixing or starting something that already has "ok": true (e.g. if "daemon_health" is ok: true, the daemon IS running — do not suggest starting it).
- If ALL checks have "ok": true, say clearly that everything is OK and that no action is required; you may add one optional tip (e.g. run "akasha start --foreground" to see logs).
- If some checks have "ok": false, list only those and suggest 1–3 concrete, safe steps from the runbooks.
- Never recommend destructive actions. Do not expose secrets.

Documentation (runbooks/specs):
{}
---
Current health state (actual, JSON):
{}
{}
Reply in the same language as the user (or French if ambiguous). Be concise."#,
            doc_context.trim(),
            health_str,
            summary
        );
        let req = akasha_llm::CompletionRequest {
            prompt,
            max_tokens: Some(512),
            temperature: Some(0.3),
        };
        let advice_timeout = std::time::Duration::from_secs(120);
        match tokio::time::timeout(advice_timeout, llm_router.complete(&req)).await {
            Ok(Ok(resp)) => {
                let body = serde_json::json!({ "advice": resp.text, "model_used": resp.model_used });
                return json_response("200 OK", &body.to_string());
            }
            Ok(Err(e)) => {
                let body = serde_json::json!({ "error": "advice_failed", "detail": e.to_string() });
                return json_response("502 Bad Gateway", &body.to_string());
            }
            Err(_) => {
                let body = serde_json::json!({
                    "error": "advice_timeout",
                    "detail": "LLM timed out (120s). Embedded model may still be loading; try /embedded to check."
                });
                return json_response("504 Gateway Timeout", &body.to_string());
            }
        }
    }

    json_response("404 Not Found", r#"{"error":"not_found"}"#)
}

async fn get_task_status(store_path: &Path, progress: &ProgressCache, id: Uuid) -> String {
    let store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let task = match store.get(id) {
        Ok(Some(t)) => t,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"task_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let progress_list: Vec<ProgressEntry> = {
        let g = progress.read().await;
        g.get(&id)
            .map(|q| q.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    let body = serde_json::json!({
        "task_id": task.id.to_string(),
        "status": task.status.as_str(),
        "assigned_agent": task.assigned_agent,
        "created_at": task.created_at.to_rfc3339(),
        "updated_at": task.updated_at.to_rfc3339(),
        "progress": progress_list
    });
    json_response("200 OK", &body.to_string())
}
