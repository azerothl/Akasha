//! Simple HTTP API: POST /api/message, GET /api/tasks/:id, GET / (health)

use akasha_core::{EventEnvelope, EventType};
use akasha_vault::Vault;
use akasha_llm::CompletionRequest;
use akasha_store::{Schedule, ScheduleStore, TaskRunStatus, TaskStatus, TaskStore};
pub use akasha_store::tasks::MAX_PROGRESS_PER_TASK;
use crate::agents::EventBus;
use crate::memory::ShortTermStore;
use crate::memory_actor::LongTermMemoryClient;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Limits concurrent background LLM fact-extraction tasks to prevent unbounded queue growth under load.
static EXTRACT_SEMAPHORE: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();

fn extract_semaphore() -> Arc<tokio::sync::Semaphore> {
    EXTRACT_SEMAPHORE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2))).clone()
}

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

/// Per-session result cell for background commands. The spawned task writes the result when done.
type BackgroundResultCell = Arc<RwLock<Option<anyhow::Result<(std::process::Output, akasha_tools::ToolResult)>>>>;

/// Registry of background command sessions: session_id -> (task handle to abort, result cell).
pub type ProcessRegistry = Arc<RwLock<std::collections::HashMap<Uuid, (tokio::task::JoinHandle<()>, BackgroundResultCell)>>>;

pub fn new_process_registry() -> ProcessRegistry {
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
/// Note: "Session terminal" (spec 33) est optionnel et prévu pour une version ultérieure.
pub const AVAILABLE_TOOLS: &[(&str, &str)] = &[
    ("read_file", "read_file <path> — lire le contenu d'un fichier texte"),
    ("write_file", "write_file <path> <content> — écrire du texte dans un fichier (path puis contenu)"),
    ("search_files", "search_files <dir> <pattern> — chercher des fichiers (glob) sous un répertoire"),
    ("grep_content", "grep_content <dir> <pattern> [file_glob] — chercher le motif dans le contenu des fichiers (ex. grep_content . \"fn \" \"*.rs\")"),
    ("run_command", "run_command <cmd> [arg1 arg2 ...] — exécuter une commande (autorisée par la politique)"),
    ("run_terminal", "run_terminal <cmd> [args...] — exécuter une commande (même que run_command)"),
    ("run_command_background", "run_command_background <cmd> [args...] — lancer en arrière-plan, retourne session_id pour process poll/kill"),
    ("process", "process list | process poll <session_id> | process kill <session_id> — lister, consulter ou arrêter des commandes en arrière-plan"),
    ("file_diff", "file_diff <path_a> <path_b> — diff texte entre deux fichiers"),
    ("edit_file", "edit_file <path> <start_line> <end_line> <new_content> — remplacer les lignes start..end par new_content (lignes 1-based)"),
    ("apply_patch", "apply_patch <path> <patch_content> — appliquer un patch unifié (contenu du patch après le path)"),
    ("search_replace", "search_replace <path> <search> | <replace> — remplacer toutes les occurrences de search par replace dans le fichier (séparateur \" | \")"),
    ("web_fetch", "web_fetch <url> — récupérer le contenu d'une URL (domaine autorisé dans tools_policy allowed_web_domains)"),
    ("web_search", "web_search <query> [max_results] — rechercher sur le web (Brave API; BRAVE_API_KEY, web_search_enabled)"),
    ("run_in_container", "run_in_container <work_dir> <image> <command> [args...] — exécuter une commande dans un conteneur (work_dir autorisé en lecture, ex. node:20 node index.js)"),
];

fn available_tools_instruction() -> String {
    AVAILABLE_TOOLS
        .iter()
        .map(|(_, desc)| *desc)
        .collect::<Vec<_>>()
        .join(" ; ")
}

/// Contexte applicatif injecté dans le prompt : l'agent sait qu'il tourne dans Akasha et peut en parler.
const APP_CONTEXT: &str = "[Contexte Akasha] Tu es l'assistant intégré à Akasha. Akasha est l'application dans laquelle tu tournes actuellement. \
Si l'utilisateur te parle d'Akasha, du programme, de l'appli ou de comment ça marche, tu peux expliquer : \
commandes (akasha start, akasha init, akasha doctor), interfaces (TUI avec onglets Chat/Routeur/Mémoire/Doc/Activité), \
commandes slash dans le Chat (/help, /status, /doctor, /advice, /config, /models, /routes, /newsession, etc.). \
La documentation complète est disponible dans l'onglet Doc de l'interface. \
Réponds en français sauf si l'utilisateur utilise une autre langue.\n\n";

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
    process_registry: Option<&ProcessRegistry>,
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
        "run_terminal" => {
            let cmd = args.get(0).map(String::as_str).unwrap_or("");
            let cmd_args: Vec<String> = args.iter().skip(1).cloned().collect();
            match executor.run_command(cmd, &cmd_args, None).await {
                Ok((out, res)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    if res.success {
                        format!("[run_terminal {}] stdout: {} stderr: {}", cmd, stdout.trim(), stderr.trim())
                    } else {
                        format!("[run_terminal] {} stderr: {}", res.summary, stderr.trim())
                    }
                }
                Err(e) => format!("[run_terminal] error: {}", e),
            }
        }
        "run_command_background" => {
            let cmd = args.get(0).cloned().unwrap_or_default();
            let cmd_args: Vec<String> = args.iter().skip(1).cloned().collect();
            let cmd_display = format!("{} {}", cmd, cmd_args.join(" "));
            match process_registry {
                Some(reg) => {
                    let session_id = Uuid::new_v4();
                    let exec = executor.clone();
                    let cell: BackgroundResultCell = Arc::new(RwLock::new(None));
                    let cell_clone = cell.clone();
                    let task = tokio::spawn(async move {
                        let result = exec.run_command(&cmd, &cmd_args, None).await;
                        *cell_clone.write().await = Some(result);
                    });
                    reg.write().await.insert(session_id, (task, cell));
                    format!("[run_command_background] session_id: {} (cmd: {})", session_id, cmd_display)
                }
                None => "[run_command_background] process registry not available".to_string(),
            }
        }
        "process" => {
            let sub = args.get(0).map(String::as_str).unwrap_or("");
            match (process_registry, sub) {
                (Some(reg), "list") => {
                    let ids: Vec<String> = reg.read().await.keys().map(|u| u.to_string()).collect();
                    format!("[process list] {} session(s): {:?}", ids.len(), ids)
                }
                (Some(reg), "poll") => {
                    let session_id = args.get(1).and_then(|s| Uuid::parse_str(s).ok());
                    match session_id {
                        Some(id) => {
                            let (has_entry, result_opt) = {
                                let g = reg.write().await;
                                if let Some((_task, cell)) = g.get(&id) {
                                    let taken = cell.write().await.take();
                                    (true, taken)
                                } else {
                                    (false, None)
                                }
                            };
                            if !has_entry {
                                return format!("[process poll] unknown session_id: {}", id);
                            }
                            match result_opt {
                                Some(Ok((out, _res))) => {
                                    reg.write().await.remove(&id);
                                    let stdout = String::from_utf8_lossy(&out.stdout);
                                    let stderr = String::from_utf8_lossy(&out.stderr);
                                    format!("[process poll {}] done — exit {} stdout: {} stderr: {}", id, out.status.code().unwrap_or(-1), stdout.trim(), stderr.trim())
                                }
                                Some(Err(e)) => {
                                    reg.write().await.remove(&id);
                                    format!("[process poll {}] error: {}", id, e)
                                }
                                None => format!("[process poll {}] still running", id),
                            }
                        }
                        None => "[process poll] usage: process poll <session_id>".to_string(),
                    }
                }
                (Some(reg), "kill") => {
                    let session_id = args.get(1).and_then(|s| Uuid::parse_str(s).ok());
                    match session_id {
                        Some(id) => {
                            let mut g = reg.write().await;
                            if let Some((task, _cell)) = g.remove(&id) {
                                task.abort();
                                format!("[process kill {}] aborted", id)
                            } else {
                                format!("[process kill] unknown session_id: {}", id)
                            }
                        }
                        None => "[process kill] usage: process kill <session_id>".to_string(),
                    }
                }
                (_, _) => "[process] usage: process list | process poll <session_id> | process kill <session_id>".to_string(),
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
        "grep_content" => {
            let dir = path_arg(0).unwrap_or(Path::new("."));
            let pattern = args.get(1).map(String::as_str).unwrap_or("");
            let file_glob = args.get(2).map(String::as_str).filter(|s| !s.is_empty());
            if pattern.is_empty() {
                return "[grep_content] usage: grep_content <dir> <pattern> [file_glob]".to_string();
            }
            match executor.grep_content(dir, pattern, file_glob, 50).await {
                Ok((matches, res)) => {
                    if res.success {
                        let lines: Vec<String> = matches
                            .iter()
                            .take(30)
                            .map(|(p, n, line)| format!("{}:{}: {}", p.display(), n, line.trim()))
                            .collect();
                        format!("[grep_content] {} — {}", res.summary, lines.join(" ; "))
                    } else {
                        format!("[grep_content] {}", res.summary)
                    }
                }
                Err(e) => format!("[grep_content] error: {}", e),
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
        "search_replace" => {
            let path = path_arg(0);
            let rest = args.get(1..).map(|a| a.join(" ")).unwrap_or_default();
            let (search, replace) = rest.split_once(" | ").unwrap_or((rest.as_str(), ""));
            match path {
                Some(p) => match executor.search_replace(p, search.trim(), replace.trim()).await {
                    Ok(res) => {
                        if res.success {
                            format!("[search_replace {}] {}", p.display(), res.summary)
                        } else {
                            format!("[search_replace] {}", res.summary)
                        }
                    }
                    Err(e) => format!("[search_replace] error: {}", e),
                },
                None => "[search_replace] usage: search_replace <path> <search> | <replace>".to_string(),
            }
        }
        "edit_file" => {
            let path = path_arg(0);
            let start_line = args.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let end_line = args.get(2).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let new_content = args.get(3..).map(|a| a.join("\n")).unwrap_or_default();
            match path {
                Some(p) => match executor.edit_file(p, start_line, end_line, &new_content).await {
                    Ok(res) => {
                        if res.success {
                            format!("[edit_file {}] {}", p.display(), res.summary)
                        } else {
                            format!("[edit_file] {}", res.summary)
                        }
                    }
                    Err(e) => format!("[edit_file] error: {}", e),
                },
                None => "[edit_file] usage: edit_file <path> <start_line> <end_line> <new_content>".to_string(),
            }
        }
        "apply_patch" => {
            let path = path_arg(0);
            let patch_content = args.get(1..).map(|a| a.join("\n")).unwrap_or_default();
            match path {
                Some(p) => match executor.apply_patch(p, &patch_content).await {
                    Ok(res) => {
                        if res.success {
                            format!("[apply_patch {}] {}", p.display(), res.summary)
                        } else {
                            format!("[apply_patch] {}", res.summary)
                        }
                    }
                    Err(e) => format!("[apply_patch] error: {}", e),
                },
                None => "[apply_patch] usage: apply_patch <path> <patch_content>".to_string(),
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
        "web_fetch" => {
            let url = args.get(0).map(String::as_str).unwrap_or("");
            if url.is_empty() {
                return "[web_fetch] usage: web_fetch <url>".to_string();
            }
            match executor.web_fetch(url).await {
                Ok((body, res)) => {
                    if res.success {
                        let preview = if body.len() <= 500 { body.as_str() } else { &body[..500] };
                        format!("[web_fetch] {} — {}", res.summary, preview)
                    } else {
                        format!("[web_fetch] {}", res.summary)
                    }
                }
                Err(e) => format!("[web_fetch] error: {}", e),
            }
        }
        "web_search" => {
            let (query, max_results) = if args.len() >= 2 {
                let last = args.last().unwrap();
                if last.parse::<u32>().is_ok() {
                    (args[..args.len() - 1].join(" "), last.parse().unwrap_or(5))
                } else {
                    (args.join(" "), 5u32)
                }
            } else {
                (args.join(" "), 5u32)
            };
            let query = query.trim();
            if query.is_empty() {
                return "[web_search] usage: web_search <query> [max_results]".to_string();
            }
            match executor.web_search(query.trim(), max_results).await {
                Ok((body, res)) => {
                    if res.success {
                        let preview = if body.len() <= 600 { body.as_str() } else { &body[..600] };
                        format!("[web_search] {} — {}", res.summary, preview)
                    } else {
                        format!("[web_search] {}", res.summary)
                    }
                }
                Err(e) => format!("[web_search] error: {}", e),
            }
        }
        "run_in_container" => {
            let work_dir = path_arg(0);
            let image = args.get(1).map(String::as_str).unwrap_or("");
            let command = args.get(2).map(String::as_str).unwrap_or("");
            if work_dir.is_none() || image.is_empty() || command.is_empty() {
                return "[run_in_container] usage: run_in_container <work_dir> <image> <command> [args...]".to_string();
            }
            let work_dir = work_dir.unwrap();
            let cmd_args: Vec<String> = args.iter().skip(3).cloned().collect();
            match executor.run_in_container(work_dir, image, command, &cmd_args, 300, 512).await {
                Ok((stdout, stderr, exit_code, _res)) => {
                    let out = String::from_utf8_lossy(&stdout);
                    let err = String::from_utf8_lossy(&stderr);
                    format!(
                        "[run_in_container] exit {} — stdout: {} stderr: {}",
                        exit_code,
                        if out.len() > 400 { format!("{}...", &out[..400]) } else { out.to_string() },
                        if err.len() > 200 { format!("{}...", &err[..200]) } else { err.to_string() }
                    )
                }
                Err(e) => format!("[run_in_container] error: {}", e),
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
    let summary_max_tokens = std::env::var("AKASHA_SYSTEM_TASK_MAX_TOKENS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(2048);
    let req = CompletionRequest {
        prompt: summary_prompt,
        max_tokens: Some(summary_max_tokens),
        temperature: Some(0.2),
        preferred_task_type: None,
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
    skill_registry: Option<std::sync::Arc<crate::skills::SkillRegistry>>,
    process_registry: Option<ProcessRegistry>,
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
        let base = available_tools_instruction();
        let skills_part = match &skill_registry {
            Some(reg) => {
                let list = reg.list().await;
                if list.is_empty() {
                    String::new()
                } else {
                    let skills_desc: Vec<String> = list
                        .iter()
                        .map(|s| format!("{} ({})", s.name, s.description))
                        .collect();
                    format!(" ; Skills (use skill name as tool): {}", skills_desc.join(", "))
                }
            }
            None => String::new(),
        };
        format!(
            "\n\nYou may request tools by writing a line: TOOL: tool_name arg1 arg2 ...\nAvailable: {}{}.\nIf you need no tool, reply normally with your answer.\n\
             If write_file or read_file returns \"path not allowed by policy\" or \"denied\", tell the user that they CAN configure this: edit the file tools_policy.yaml \
             (in the Akasha data directory) and add path prefixes under allowed_write_paths or allowed_read_paths. It is not impossible — the user controls this YAML file.",
            base, skills_part
        )
    } else {
        String::new()
    };

    // Build prompt with short-term + long-term memory (spec 06)
    let mut context_prefix = String::new();
    context_prefix.push_str(APP_CONTEXT);

    // Long-term: retrieve top-k relevant memories by embedding similarity (current message + optional user-identity for first message)
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
        // First message of session: search long-term for user identity (name, etc.) so the agent can greet the user.
        let is_first_message = turns.is_empty();
        if is_first_message {
            if let Some(ref client) = long_term_client {
                let query = "nom prénom utilisateur user name identité".to_string();
                let client = client.clone();
                let user_memories = tokio::task::spawn_blocking(move || client.search(query, 3))
                    .await
                    .ok()
                    .unwrap_or_default();
                if !user_memories.is_empty() {
                    context_prefix.push_str("[Contexte utilisateur — utilise pour saluer si pertinent]\n");
                    for content in &user_memories {
                        context_prefix.push_str("- ");
                        context_prefix.push_str(&content.replace('\n', " "));
                        context_prefix.push_str("\n");
                    }
                    context_prefix.push_str("Si c'est le premier échange de la session, salue l'utilisateur avec son prénom si tu le connais.\n\n");
                }
            }
        }
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
    let idle_timeout_secs = std::env::var("AKASHA_LLM_STREAM_IDLE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);
    // First chunk can take long (model load, first token on CPU). Use longer wait so we don't hit idle before any data.
    let first_chunk_timeout_secs = std::env::var("AKASHA_LLM_FIRST_CHUNK_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or_else(|| llm_timeout_secs.min(300));

    'tool_rounds: loop {
        let request = CompletionRequest {
            prompt: format!("{}{}", current_prompt, tool_instruction),
            max_tokens: Some(max_tokens),
            temperature: Some(0.7),
            preferred_task_type: None,
        };
        // Streaming path: single forwarder thread → tokio channel (avoids spawn_blocking per chunk).
        // Overall deadline bounds the full generation; idle timeout bounds inter-chunk wait.
        let (stream_tx, std_rx) = std::sync::mpsc::channel::<String>();
        let (tok_tx, mut tok_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        std::thread::Builder::new()
            .name("akasha-stream-fwd".to_string())
            .spawn(move || {
                for chunk in std_rx {
                    if tok_tx.send(chunk).is_err() {
                        break;
                    }
                }
            })
            .ok();
        let router = llm_router.clone();
        let stream_join = tokio::spawn(async move { router.complete_stream(&request, stream_tx).await });
        let mut accumulated = String::new();
        let mut first_wait = true;
        let overall_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(llm_timeout_secs);
        loop {
            // Check the overall deadline before waiting for a chunk to avoid spurious zero-duration timeouts.
            if tokio::time::Instant::now() >= overall_deadline {
                tracing::warn!(timeout_secs = llm_timeout_secs, "Overall LLM timeout exceeded; aborting task");
                stream_join.abort();
                reply_text = if accumulated.is_empty() {
                    format!("LLM response timed out after {} seconds.", llm_timeout_secs)
                } else {
                    accumulated
                };
                break 'tool_rounds;
            }
            let idle = if first_wait {
                first_wait = false;
                std::time::Duration::from_secs(first_chunk_timeout_secs)
            } else {
                std::time::Duration::from_secs(idle_timeout_secs)
            };
            match tokio::time::timeout(idle, tok_rx.recv()).await {
                Ok(Some(chunk)) => {
                    accumulated.push_str(&chunk);
                    let _ = bus.send(
                        EventEnvelope::new(
                            EventType::ProgressUpdate,
                            Some(serde_json::json!({
                                "task_id": task_id.to_string(),
                                "progress_pct": 50,
                                "message": accumulated
                            })),
                        )
                        .with_correlation(task_id),
                    );
                }
                Ok(None) => break, // channel closed (sender dropped)
                Err(_) => {
                    tracing::debug!(idle_secs = idle_timeout_secs, "Stream idle timeout, waiting for final response");
                    break;
                }
            }
        }
        // Wrap stream_join.await with remaining overall budget; guard against zero remaining.
        let remaining = overall_deadline.saturating_duration_since(tokio::time::Instant::now());
        let response = if remaining.is_zero() {
            tracing::warn!(timeout_secs = llm_timeout_secs, "Overall LLM timeout on stream completion");
            reply_text = if accumulated.is_empty() {
                format!("LLM response timed out after {} seconds.", llm_timeout_secs)
            } else {
                accumulated
            };
            break;
        } else {
            match tokio::time::timeout(remaining, stream_join).await {
                Ok(Ok(Ok(resp))) => resp.text.trim().to_string(),
                Ok(Ok(Err(e))) => {
                    tracing::warn!(error = %e, "LLM completion failed");
                    reply_text = format!("Sorry, I couldn't get a response (error: {}).", e);
                    break;
                }
                Ok(Err(join_err)) => {
                    tracing::warn!(error = %join_err, "Stream task join failed");
                    reply_text = if accumulated.is_empty() {
                        format!("LLM task error: {}", join_err)
                    } else {
                        accumulated
                    };
                    break;
                }
                Err(_timeout) => {
                    tracing::warn!(timeout_secs = llm_timeout_secs, "Overall LLM timeout on stream completion");
                    reply_text = if accumulated.is_empty() {
                        format!("LLM response timed out after {} seconds.", llm_timeout_secs)
                    } else {
                        accumulated
                    };
                    break;
                }
            }
        };
        if !accumulated.is_empty() && response.is_empty() {
            // Stream sent chunks but final response empty; use accumulated
            reply_text = accumulated.trim().to_string();
            break;
        }

        let tool_calls = tools_executor.as_ref().and_then(|_| {
            let calls = parse_tool_calls(&response);
            if calls.is_empty() { None } else { Some(calls) }
        });

        if let (Some(exec), Some(calls)) = (tools_executor.as_ref(), tool_calls) {
            round += 1;
            let mut tool_results = Vec::new();
            for (name, args) in &calls {
                // Phase D: resolve skill name to tool_ref (spec 33)
                let actual_tool = match &skill_registry {
                    Some(reg) => reg.get(name).await.map(|s| s.tool_ref).unwrap_or_else(|| name.clone()),
                    None => name.clone(),
                };
                let res = execute_tool_call(exec, &actual_tool, args, process_registry.as_ref()).await;
                // Phase F: emit ToolInvoked for Actions tab (spec 33)
                let success = !res.contains("denied") && !res.contains(" error:");
                let payload = serde_json::json!({
                    "tool": actual_tool,
                    "skill": if &actual_tool != name { Some(name.as_str()) } else { None::<&str> },
                    "args": args,
                    "result_preview": if res.len() > 300 { format!("{}...", &res[..300]) } else { res.clone() },
                    "success": success
                });
                let _ = bus.send(
                    EventEnvelope::new(EventType::ToolInvoked, Some(payload)).with_correlation(task_id),
                );
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
        st.append(&session_id, "user", message.clone()).await;
        st.append(&session_id, "assistant", reply_text.clone()).await;
    }

    // Extract and promote personal facts to long-term memory (spec 06: nom, préférences, décisions).
    if let Some(ref long_term) = long_term_client {
        // Heuristic: capture obvious name/intro from user message. Promote these *immediately* so they appear in Memory tab right away.
        // Case-insensitive matching on lowercased text, but extract from original message to preserve casing.
        let msg_lower = message.to_lowercase();
        let mut heuristic_facts = Vec::new();
        for (pattern, prefix) in [
            ("je m'appelle ", "L'utilisateur s'appelle "),
            ("mon nom est ", "L'utilisateur s'appelle "),
            ("mon prénom est ", "L'utilisateur s'appelle "),
            ("mon prénom c'est ", "L'utilisateur s'appelle "),
            ("je suis ", "L'utilisateur est "),
            ("tu peux m'appeler ", "L'utilisateur veut être appelé "),
            ("appelle-moi ", "L'utilisateur veut être appelé "),
            ("i'm ", "The user is "),
            ("my name is ", "The user's name is "),
            ("call me ", "The user wants to be called "),
        ] {
            if let Some(start_idx) = msg_lower.find(pattern) {
                let value_start = start_idx + pattern.len();
                // Extract value from the *original* message at the same position to preserve casing.
                let original_rest = &message[value_start..];
                let name = original_rest
                    .trim()
                    .split(|c: char| c == ',' || c == '.' || c == '\n' || c == '!')
                    .next()
                    .unwrap_or(original_rest)
                    .trim();
                let name = name.chars().take(80).collect::<String>();
                if !name.is_empty() {
                    heuristic_facts.push(format!("{}{}", prefix, name));
                    break;
                }
            }
        }
        // Promote heuristic facts synchronously so they are stored before the user opens the Memory tab.
        for fact in &heuristic_facts {
            let client = long_term.clone();
            let fact = fact.clone();
            match tokio::task::spawn_blocking(move || client.promote(fact, "user_fact".to_string())).await {
                Ok(Ok(())) => tracing::info!("Personal fact stored in long-term memory (heuristic)"),
                Ok(Err(e)) => tracing::warn!(error = %e, "Long-term promote failed — check that embeddings/tract model loads (see daemon logs)"),
                Err(e) => tracing::debug!(error = %e, "Promote task join error"),
            }
        }

        // Then spawn LLM extraction for additional facts (async, bounded concurrency, no wait).
        let msg = message.clone();
        let reply = reply_text.clone();
        let client = long_term.clone();
        let router = llm_router.clone();
        let n_heuristic = heuristic_facts.len();
        let sem = extract_semaphore();
        tokio::spawn(async move {
            // Acquire a permit; if all slots are busy, drop this extraction cycle rather than queuing unbounded work.
            let _permit = match sem.try_acquire() {
                Ok(p) => p,
                Err(_) => {
                    tracing::debug!("Background fact extraction skipped: semaphore full (too many concurrent extractions)");
                    return;
                }
            };
            let mut facts = heuristic_facts;
            let extract_prompt = format!(
                "Tu dois extraire UNIQUEMENT les faits personnels à retenir sur l'utilisateur (nom, prénom, préférences, décisions). \
Une ligne par fait, chaque ligne commence par FACT: (ex: FACT: L'utilisateur s'appelle Jean. FACT: L'utilisateur préfère le café.). \
N'écris que des lignes FACT: ou NOTHING si aucun fait. Pas d'autre texte.\n\nUtilisateur: {}\n\nAssistant: {}",
                msg.trim(),
                reply.trim()
            );
            // Allow enough tokens for models that output "thinking" before the FACT: lines (done_reason: length otherwise).
            let extract_max_tokens = std::env::var("AKASHA_SYSTEM_TASK_MAX_TOKENS")
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(2048);
            let req = CompletionRequest {
                prompt: extract_prompt,
                max_tokens: Some(extract_max_tokens),
                temperature: Some(0.1),
                preferred_task_type: Some("system".to_string()),
            };
            if let Ok(Ok(resp)) = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                router.complete(&req),
            ).await {
                for line in resp.text.lines() {
                    let line = line.trim();
                    if let Some(fact) = line.strip_prefix("FACT:") {
                        let fact = fact.trim().to_string();
                        if !fact.is_empty() && !facts.contains(&fact) {
                            facts.push(fact);
                        }
                    }
                }
            }
            if facts.is_empty() {
                tracing::debug!(user_msg = %msg.trim().chars().take(100).collect::<String>(), "No personal facts extracted for long-term memory");
            } else {
                tracing::debug!(count = facts.len(), "Promoting extra facts to long-term memory");
            }
            // Promote only facts that weren't already promoted (heuristic ones were done above).
            for fact in facts.into_iter().skip(n_heuristic) {
                let client = client.clone();
                match tokio::task::spawn_blocking(move || client.promote(fact, "user_fact".to_string())).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!(error = %e, "Long-term promote user_fact failed"),
                    Err(e) => tracing::debug!(error = %e, "Promote task join error"),
                }
            }
        });
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
    short_term: Option<std::sync::Arc<ShortTermStore>>,
    long_term_client: Option<LongTermMemoryClient>,
) -> String {
    let data_dir = store_path.parent().unwrap_or_else(|| store_path.as_ref());

    if method == "GET" && (path == "/" || path.is_empty()) {
        return json_response("200 OK", r#"{"status":"ok"}"#);
    }

    // GET /api/memory/short-term?session_id=... — turns for session (default: day-YYYY-MM-DD)
    if method == "GET" && path.starts_with("/api/memory/short-term") {
        let session_id = path
            .split('?')
            .nth(1)
            .and_then(|q| {
                q.split('&')
                    .find(|p| p.starts_with("session_id="))
                    .map(|p| urlencoding::decode(p.trim_start_matches("session_id=")).unwrap_or_default().into_owned())
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("day-{}", chrono::Utc::now().format("%Y-%m-%d")));
        let turns = if let Some(ref st) = short_term {
            st.get_turns(&session_id).await
        } else {
            vec![]
        };
        let list: Vec<serde_json::Value> = turns
            .iter()
            .map(|t| serde_json::json!({ "role": t.role, "content": t.content }))
            .collect();
        let body_json = serde_json::json!({ "session_id": session_id, "turns": list });
        return json_response("200 OK", &body_json.to_string());
    }

    // GET /api/memory/long-term?limit=50 — recent long-term entries (content, created_at, source)
    if method == "GET" && path.starts_with("/api/memory/long-term") {
        let limit = path
            .split('?')
            .nth(1)
            .and_then(|q| {
                q.split('&')
                    .find(|p| p.starts_with("limit="))
                    .and_then(|p| p.trim_start_matches("limit=").parse::<usize>().ok())
            })
            .unwrap_or(50)
            .min(200);
        let entries = if let Some(ref client) = long_term_client {
            let client = client.clone();
            tokio::task::spawn_blocking(move || client.list(limit))
                .await
                .unwrap_or_default()
        } else {
            vec![]
        };
        let list: Vec<serde_json::Value> = entries
            .iter()
            .map(|(content, created_at, source)| {
                serde_json::json!({ "content": content, "created_at": created_at, "source": source })
            })
            .collect();
        let body_json = serde_json::json!({
            "entries": list,
            "long_term_available": long_term_client.is_some()
        });
        return json_response("200 OK", &body_json.to_string());
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
        // Session: "new_session" => new UUID; else provided non-empty session_id; else new UUID (isolated context).
        let session_id = {
            let new_session = body_json.as_ref().and_then(|v| v.get("new_session")).and_then(|v| v.as_bool()).unwrap_or(false);
            let provided = body_json.as_ref().and_then(|v| v.get("session_id").and_then(|v| v.as_str().map(String::from)));
            if new_session {
                uuid::Uuid::new_v4().to_string()
            } else if let Some(s) = provided {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    uuid::Uuid::new_v4().to_string()
                } else {
                    trimmed.to_string()
                }
            } else {
                uuid::Uuid::new_v4().to_string()
            }
        };
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
    if path.starts_with("/api/tasks/") {
        let rest = path.trim_start_matches("/api/tasks/");
        let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if let Some(&id_str) = parts.first() {
            if let Ok(id) = Uuid::parse_str(id_str) {
                if method == "POST" && parts.get(1) == Some(&"cancel") {
                    return cancel_task(store_path, id, main_agent).await;
                }
                if method == "GET" && parts.get(1) == Some(&"events") {
                    return get_task_events(events, id).await;
                }
                if method == "GET" {
                    return get_task_status(store_path, progress, id).await;
                }
            }
        }
    }

    // Schedules and task_runs (FR-028, FR-029)
    if method == "GET" && path == "/api/schedules" {
        return get_schedules_list(store_path).await;
    }
    if method == "GET" && path.starts_with("/api/schedules/") {
        let rest = path.trim_start_matches("/api/schedules/");
        let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if let Some(&id_str) = parts.first() {
            if let Ok(id) = Uuid::parse_str(id_str) {
                return get_schedule_by_id(store_path, id).await;
            }
        }
    }
    if method == "POST" && path == "/api/schedules" {
        return post_schedule(store_path, body).await;
    }
    if method == "PUT" && path.starts_with("/api/schedules/") {
        let rest = path.trim_start_matches("/api/schedules/");
        if let Some(id_str) = rest.split('/').next() {
            if let Ok(id) = Uuid::parse_str(id_str) {
                return put_schedule(store_path, id, body).await;
            }
        }
    }
    if method == "DELETE" && path.starts_with("/api/schedules/") {
        let rest = path.trim_start_matches("/api/schedules/");
        if let Some(id_str) = rest.split('/').next() {
            if let Ok(id) = Uuid::parse_str(id_str) {
                return delete_schedule(store_path, id).await;
            }
        }
    }
    if method == "GET" && path == "/api/task_runs" {
        return get_task_runs_list(store_path, path).await;
    }
    if method == "GET" && path.starts_with("/api/task_runs/") {
        let rest = path.trim_start_matches("/api/task_runs/");
        if let Some(id_str) = rest.split('/').next() {
            if let Ok(id) = Uuid::parse_str(id_str) {
                return get_task_run_by_id(store_path, id).await;
            }
        }
    }
    if method == "GET" && path == "/api/schedule_run_reports" {
        return get_schedule_run_reports(store_path, progress).await;
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
            preferred_task_type: None,
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
            preferred_task_type: None,
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

async fn cancel_task(
    store_path: &Path,
    id: Uuid,
    main_agent: &crate::agents::MainAgent,
) -> String {
    let store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let task = match store.get(id) {
        Ok(Some(t)) => t,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"task_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let cancellable = matches!(
        task.status,
        TaskStatus::Pending | TaskStatus::Queued | TaskStatus::Running
    );
    if !cancellable {
        let body = serde_json::json!({
            "error": "task_not_cancellable",
            "detail": "La tâche est déjà terminée, annulée ou en pause.",
            "status": task.status.as_str()
        });
        return json_response("400 Bad Request", &body.to_string());
    }
    if store.update_status(id, TaskStatus::Cancelled).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    let _ = main_agent.bus().send(
        EventEnvelope::new(
            EventType::TaskCancelled,
            Some(serde_json::json!({ "task_id": id.to_string() })),
        )
        .with_correlation(id),
    );
    let body = serde_json::json!({ "cancelled": true, "task_id": id.to_string() });
    json_response("200 OK", &body.to_string())
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
    let mut progress_list: Vec<ProgressEntry> = store
        .get_progress(id)
        .map(|v| v.into_iter().map(|(pct, msg)| ProgressEntry { progress_pct: pct, message: msg }).collect())
        .unwrap_or_default();
    // Prefer persisted progress when available; fall back to in-memory progress if none is stored.
    if progress_list.is_empty() {
        let mem_entries: Vec<ProgressEntry> = {
            let g = progress.read().await;
            g.get(&id)
                .map(|q| q.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        };
        progress_list = mem_entries;
    }
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

async fn get_schedules_list(store_path: &Path) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let list = match store.list_schedules() {
        Ok(l) => l,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let arr: Vec<serde_json::Value> = list
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id.to_string(),
                "name": s.name,
                "description": s.description,
                "enabled": s.enabled,
                "timezone": s.timezone,
                "rrule": s.rrule,
                "interval_seconds": s.interval_seconds,
                "start_at": s.start_at.to_rfc3339(),
                "end_at": s.end_at.map(|t| t.to_rfc3339()),
                "created_at": s.created_at.to_rfc3339(),
                "updated_at": s.updated_at.to_rfc3339()
            })
        })
        .collect();
    let body = serde_json::json!({ "schedules": arr });
    json_response("200 OK", &body.to_string())
}

async fn get_schedule_by_id(store_path: &Path, id: Uuid) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let s = match store.get_schedule(id) {
        Ok(Some(x)) => x,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"schedule_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let body = serde_json::json!({
        "id": s.id.to_string(),
        "name": s.name,
        "description": s.description,
        "enabled": s.enabled,
        "timezone": s.timezone,
        "rrule": s.rrule,
        "interval_seconds": s.interval_seconds,
        "start_at": s.start_at.to_rfc3339(),
        "end_at": s.end_at.map(|t| t.to_rfc3339()),
        "channel_context": s.channel_context,
        "created_at": s.created_at.to_rfc3339(),
        "updated_at": s.updated_at.to_rfc3339()
    });
    json_response("200 OK", &body.to_string())
}

async fn post_schedule(store_path: &Path, body: Option<Vec<u8>>) -> String {
    let json: serde_json::Value = match body.as_deref().and_then(|b| serde_json::from_slice(b).ok()) {
        Some(j) => j,
        None => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
    };
    let now = chrono::Utc::now();
    let id = Uuid::new_v4();
    let schedule = Schedule {
        id,
        name: json.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        description: json.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        enabled: json.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
        timezone: json.get("timezone").and_then(|v| v.as_str()).unwrap_or("UTC").to_string(),
        rrule: json.get("rrule").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        interval_seconds: json.get("interval_seconds").and_then(|v| v.as_u64()),
        start_at: json
            .get("start_at")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or(now),
        end_at: json
            .get("end_at")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&chrono::Utc)),
        channel_context: json.get("channel_context").and_then(|v| v.as_str()).map(String::from),
        created_at: now,
        updated_at: now,
    };
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    if store.insert_schedule(&schedule).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    let body = serde_json::json!({
        "id": id.to_string(),
        "name": schedule.name,
        "description": schedule.description,
        "enabled": schedule.enabled,
        "timezone": schedule.timezone,
        "rrule": schedule.rrule,
        "interval_seconds": schedule.interval_seconds,
        "start_at": schedule.start_at.to_rfc3339(),
        "end_at": schedule.end_at.map(|t| t.to_rfc3339()),
        "created_at": schedule.created_at.to_rfc3339(),
        "updated_at": schedule.updated_at.to_rfc3339()
    });
    json_response("201 Created", &body.to_string())
}

async fn put_schedule(store_path: &Path, id: Uuid, body: Option<Vec<u8>>) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let mut s = match store.get_schedule(id) {
        Ok(Some(x)) => x,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"schedule_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let json: serde_json::Value = match body.as_deref().and_then(|b| serde_json::from_slice(b).ok()) {
        Some(j) => j,
        None => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
    };
    if let Some(v) = json.get("name").and_then(|v| v.as_str()) {
        s.name = v.to_string();
    }
    if let Some(v) = json.get("description").and_then(|v| v.as_str()) {
        s.description = v.to_string();
    }
    if let Some(v) = json.get("enabled").and_then(|v| v.as_bool()) {
        s.enabled = v;
    }
    if let Some(v) = json.get("timezone").and_then(|v| v.as_str()) {
        s.timezone = v.to_string();
    }
    if let Some(v) = json.get("rrule").and_then(|v| v.as_str()) {
        s.rrule = v.to_string();
    }
    if let Some(v) = json.get("interval_seconds").and_then(|v| v.as_u64()) {
        s.interval_seconds = Some(v);
    }
    if store.update_schedule(&s).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    json_response("200 OK", &serde_json::json!({ "id": id.to_string() }).to_string())
}

async fn delete_schedule(store_path: &Path, id: Uuid) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    if store.delete_schedule(id).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    json_response("200 OK", &serde_json::json!({ "deleted": id.to_string() }).to_string())
}

async fn get_task_runs_list(store_path: &Path, path: &str) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let schedule_id = path
        .split('?')
        .nth(1)
        .and_then(|q| q.split('&').find(|p| p.starts_with("schedule_id=")))
        .and_then(|p| p.strip_prefix("schedule_id="))
        .and_then(|s| Uuid::parse_str(s).ok());
    let list = match store.list_task_runs(schedule_id, 100) {
        Ok(l) => l,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let arr: Vec<serde_json::Value> = list
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id.to_string(),
                "schedule_id": r.schedule_id.map(|u| u.to_string()),
                "task_id": r.task_id.to_string(),
                "status": r.status.as_str(),
                "planned_for": r.planned_for.to_rfc3339(),
                "started_at": r.started_at.map(|t| t.to_rfc3339()),
                "ended_at": r.ended_at.map(|t| t.to_rfc3339()),
                "dedup_key": r.dedup_key
            })
        })
        .collect();
    let body = serde_json::json!({ "task_runs": arr });
    json_response("200 OK", &body.to_string())
}

async fn get_task_run_by_id(store_path: &Path, id: Uuid) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let r = match store.get_task_run(id) {
        Ok(Some(x)) => x,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"task_run_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let body = serde_json::json!({
        "id": r.id.to_string(),
        "schedule_id": r.schedule_id.map(|u| u.to_string()),
        "task_id": r.task_id.to_string(),
        "status": r.status.as_str(),
        "planned_for": r.planned_for.to_rfc3339(),
        "started_at": r.started_at.map(|t| t.to_rfc3339()),
        "ended_at": r.ended_at.map(|t| t.to_rfc3339()),
        "dedup_key": r.dedup_key
    });
    json_response("200 OK", &body.to_string())
}

/// GET /api/schedule_run_reports — recent completed schedule runs with schedule name and task result message (for chat).
async fn get_schedule_run_reports(store_path: &Path, progress: &ProgressCache) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let list = match store.list_task_runs(None, 50) {
        Ok(l) => l,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let completed: Vec<_> = list
        .into_iter()
        .filter(|r| r.status == TaskRunStatus::Completed && r.schedule_id.is_some())
        .collect();
    // Prefetch all needed schedules into a local cache before acquiring the progress lock.
    let mut schedule_names: std::collections::HashMap<Uuid, String> =
        std::collections::HashMap::new();
    for run in &completed {
        if let Some(sid) = run.schedule_id {
            if !schedule_names.contains_key(&sid) {
                if let Ok(Some(schedule)) = store.get_schedule(sid) {
                    schedule_names.insert(sid, schedule.name);
                }
            }
        }
    }
    let progress_guard = progress.read().await;
    let reports: Vec<serde_json::Value> = completed
        .into_iter()
        .filter_map(|r| {
            let schedule_id = r.schedule_id?;
            let schedule_name = schedule_names.get(&schedule_id)?.clone();
            let message = progress_guard
                .get(&r.task_id)
                .and_then(|q| q.back())
                .map(|e| e.message.clone())
                .unwrap_or_else(|| "Exécuté.".to_string());
            Some(serde_json::json!({
                "schedule_id": schedule_id.to_string(),
                "schedule_name": schedule_name,
                "task_id": r.task_id.to_string(),
                "task_run_id": r.id.to_string(),
                "message": message,
                "ended_at": r.ended_at.map(|t| t.to_rfc3339())
            }))
        })
        .collect();
    let body = serde_json::json!({ "reports": reports });
    json_response("200 OK", &body.to_string())
}
