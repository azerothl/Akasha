//! Simple HTTP API: POST /api/message, GET /api/tasks/:id, GET / (health)

use akasha_core::{EventEnvelope, EventType};
use akasha_vault::Vault;
use akasha_llm::CompletionRequest;
use akasha_store::{Schedule, ScheduleStore, Task, TaskRunStatus, TaskStatus, TaskStore};
pub use akasha_store::tasks::MAX_PROGRESS_PER_TASK;
use crate::agent_profile::AgentProfile;
use crate::agents::{EventBus, OrchestratorTask};
use crate::memory::ShortTermStore;
use crate::memory_actor::LongTermMemoryClient;
use std::path::{Path, PathBuf};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::mpsc;
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

async fn get_task_events(events: &EventsCache, store_path: &Path, id: Uuid) -> String {
    let mut list: Vec<TaskEventEntry> = {
        let g = events.read().await;
        g.get(&id)
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    };
    // Include child task events so the UI shows sub-agent activity (children emit with their own correlation_id).
    if let Ok(store) = TaskStore::open(store_path) {
        if let Ok(children) = store.get_children(id) {
            let g = events.read().await;
            for child in &children {
                if let Some(q) = g.get(&child.id) {
                    for e in q.iter().cloned() {
                        list.push(e);
                    }
                }
            }
        }
    }
    list.sort_by(|a, b| a.at.cmp(&b.at));
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
    /// When present, indicates which task this event belongs to (for merged root+child responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
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

/// Pending "human in the loop" request: agent is waiting for the user to answer.
pub struct PendingHumanInput {
    pub question: String,
    pub context: String,
    pub choices: Option<Vec<String>>,
    pub response_tx: tokio::sync::oneshot::Sender<String>,
}

/// Store of pending human-input requests by task_id. Used by the conversation worker (to register and wait) and by the API (to return question/context/choices and to submit the reply).
pub type HumanInputStore = Arc<RwLock<std::collections::HashMap<Uuid, PendingHumanInput>>>;

pub fn new_human_input_store() -> HumanInputStore {
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
    ("memory_search", "memory_search <query> [top_k] — rechercher dans la mémoire long terme (si activée)"),
    ("memory_store", "memory_store <content> <source> — stocker/promouvoir un contenu en mémoire long terme"),
    ("memory_delete", "memory_delete <id> — supprimer une entrée de la mémoire long terme par son id (UUID)"),
    ("sessions_list", "sessions_list [limit] — lister les tâches/sessions récentes"),
    ("sessions_spawn", "sessions_spawn <message> [session_id] — créer une sous-tâche et la lancer"),
    ("session_status", "session_status <task_id> — statut d'une tâche donnée"),
    ("message", "message send <channel> <text> — envoyer un message vers un canal (webhook configuré via AKASHA_MESSAGE_WEBHOOK_URL)"),
    ("browser", "browser navigate <url> | browser screenshot | browser snapshot — automation navigateur (non implémenté, prévu phase 3)"),
    ("image", "image <path|url> [prompt] — analyse d'image par modèle vision (non implémenté, prévu phase 3)"),
    ("pdf", "pdf <path|url> — extraire le texte d'un PDF (non implémenté, prévu phase 3)"),
    ("ask_user", "ask_user — demande une information à l'utilisateur (human in the loop). Ligne suivante : JSON avec question (requis), context (optionnel), choices (optionnel, tableau de chaînes pour choix multiples). Exemple : {\"question\":\"Quel fichier ?\",\"context\":\"...\",\"choices\":[\"a.txt\",\"b.txt\"]}"),
];

fn available_tools_instruction(allowed_tools: Option<&[String]>) -> String {
    let iter: Box<dyn Iterator<Item = &(&str, &str)>> = if let Some(allowed) = allowed_tools {
        Box::new(
            AVAILABLE_TOOLS
                .iter()
                .filter(move |(name, _)| allowed.iter().any(|a| a == *name)),
        )
    } else {
        Box::new(AVAILABLE_TOOLS.iter())
    };
    iter.map(|(_, desc)| *desc)
        .collect::<Vec<_>>()
        .join(" ; ")
}

/// Contexte applicatif injecté dans le prompt : l'agent sait qu'il tourne dans Akasha et peut en parler.
const APP_CONTEXT: &str = "[Contexte Akasha] Tu es l'assistant intégré à Akasha. Akasha est l'application dans laquelle tu tournes actuellement. \
Si l'utilisateur te parle d'Akasha, du programme, de l'appli ou de comment ça marche, tu peux expliquer : \
commandes (akasha start, akasha init, akasha doctor), interfaces (TUI avec onglets Chat/Routeur/Mémoire/Doc/Activité), \
commandes slash dans le Chat (/help, /status, /doctor, /advice, /config, /models, /routes, /newsession, etc.). \
La documentation complète est disponible dans l'onglet Doc de l'interface. \
Réponds en français sauf si l'utilisateur utilise une autre langue. \
Ne jamais inventer de données. Si tu n'as pas l'information pour répondre, dis-le clairement (ex. « Je n'ai pas trouvé d'information »). \
Si une tâche nécessite de te connecter à un service externe (compte, clé API, identifiants), demande à l'utilisateur les informations de connexion ou indique-lui comment les configurer (ex. variable d'environnement, vault Akasha), puis reprends la tâche une fois qu'il t'a répondu.\n\n";

/// If AKASHA_TOOLS_JOURNAL_PATH is set, append a line for write tool invocations (Phase 4 modification journal).
async fn log_tool_journal_if_write(tool: &str, args: &[String], result_preview: &str) {
    const WRITE_TOOLS: &[&str] = &["write_file", "search_replace", "edit_file", "apply_patch"];
    if !WRITE_TOOLS.contains(&tool) {
        return;
    }
    if let Ok(path) = std::env::var("AKASHA_TOOLS_JOURNAL_PATH") {
        let line = format!(
            "{} {} {} {}\n",
            chrono::Utc::now().to_rfc3339(),
            tool,
            args.join(" ").replace('\n', " "),
            result_preview.replace('\n', " ").chars().take(200).collect::<String>()
        );
        if let Ok(mut f) = tokio::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .await
        {
            let _ = tokio::io::AsyncWriteExt::write_all(&mut f, line.as_bytes()).await;
        }
    }
}

/// Parse tool calls from LLM response: lines "TOOL: tool_name arg1 arg2 ...".
fn parse_tool_calls(response: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let lines: Vec<&str> = response.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if let Some(rest) = line.strip_prefix("TOOL:") {
            let rest = rest.trim();
            // Split the header line by whitespace for tool name + fixed positional args
            let parts: Vec<String> = rest.split_whitespace().map(String::from).collect();
            if let Some((name, fixed_args)) = parts.split_first() {
                let mut args = fixed_args.to_vec();
                // Only certain tools support a multi-line body argument.
                let tool_name_lc = name.to_lowercase();
                let supports_body = matches!(
                    tool_name_lc.as_str(),
                    "apply_patch" | "edit_file" | "write_file" | "ask_user"
                );

                if supports_body {
                    // Collect subsequent non-TOOL: lines as a raw multi-line body (for
                    // tools like apply_patch / edit_file that need preserved whitespace).
                    i += 1;
                    let body_start = i;
                    while i < lines.len() && !lines[i].trim_start().starts_with("TOOL:") {
                        i += 1;
                    }
                    // Trim trailing blank lines from the body
                    let mut body_end = i;
                    while body_end > body_start && lines[body_end - 1].trim().is_empty() {
                        body_end -= 1;
                    }
                    if body_end > body_start {
                        args.push(lines[body_start..body_end].join("\n"));
                    }
                    out.push((name.clone(), args));
                    continue;
                } else {
                    // For other tools, only use the header line arguments and
                    // do not consume following lines as a body.
                    out.push((name.clone(), args));
                    i += 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// Execute one tool call via ToolExecutor. Returns `(success, display_string)` for structured events.
async fn execute_tool_call(
    executor: &std::sync::Arc<akasha_tools::ToolExecutor>,
    tool_name: &str,
    args: &[String],
    process_registry: Option<&ProcessRegistry>,
    long_term_client: Option<&LongTermMemoryClient>,
    task_id: Uuid,
    store_path: Option<&std::path::Path>,
    conv_tx: Option<mpsc::Sender<OrchestratorTask>>,
    message_webhook_url: Option<&str>,
) -> (bool, String) {
    use std::path::Path;
    if !executor.policy.can_use_tool(tool_name) {
        return (false, format!("[{}] tool not allowed by current profile", tool_name));
    }
    let path_arg = |i: usize| args.get(i).map(|s| Path::new(s.as_str()));
    let result = match tool_name {
        "read_file" => {
            if let Some(p) = path_arg(0) {
                match executor.read_file(p).await {
                    Ok((content, res)) => {
                        let msg = if res.success {
                            format!("[read_file {}] {} chars: {}", p.display(), content.len(), if content.len() <= 500 { content.as_str() } else { &content[..500] })
                        } else {
                            format!("[read_file] denied or error: {}", res.summary)
                        };
                        (res.success, msg)
                    }
                    Err(e) => (false, format!("[read_file] error: {}", e)),
                }
            } else {
                (false, "[read_file] usage: read_file <path>".to_string())
            }
        }
        "run_command" => {
            let cmd = args.get(0).map(String::as_str).unwrap_or("");
            let cmd_args: Vec<String> = args.iter().skip(1).cloned().collect();
            match executor.run_command(cmd, &cmd_args, None).await {
                Ok((out, res)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let msg = if res.success {
                        format!("[run_command {}] stdout: {} stderr: {}", cmd, stdout.trim(), stderr.trim())
                    } else {
                        format!("[run_command] {} stderr: {}", res.summary, stderr.trim())
                    };
                    (res.success, msg)
                }
                Err(e) => (false, format!("[run_command] error: {}", e)),
            }
        }
        "run_terminal" => {
            let cmd = args.get(0).map(String::as_str).unwrap_or("");
            let cmd_args: Vec<String> = args.iter().skip(1).cloned().collect();
            match executor.run_command(cmd, &cmd_args, None).await {
                Ok((out, res)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let msg = if res.success {
                        format!("[run_terminal {}] stdout: {} stderr: {}", cmd, stdout.trim(), stderr.trim())
                    } else {
                        format!("[run_terminal] {} stderr: {}", res.summary, stderr.trim())
                    };
                    (res.success, msg)
                }
                Err(e) => (false, format!("[run_terminal] error: {}", e)),
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
                    let reg_clone = reg.clone();
                    let task = tokio::spawn(async move {
                        let result = exec.run_command(&cmd, &cmd_args, None).await;
                        *cell_clone.write().await = Some(result);
                        // Auto-cleanup after a TTL to prevent leaking sessions the client never polls.
                        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                        reg_clone.write().await.remove(&session_id);
                    });
                    reg.write().await.insert(session_id, (task, cell));
                    (true, format!("[run_command_background] session_id: {} (cmd: {})", session_id, cmd_display))
                }
                None => (false, "[run_command_background] process registry not available".to_string()),
            }
        }
        "process" => {
            let sub = args.get(0).map(String::as_str).unwrap_or("");
            match (process_registry, sub) {
                (Some(reg), "list") => {
                    let ids: Vec<String> = reg.read().await.keys().map(|u| u.to_string()).collect();
                    (true, format!("[process list] {} session(s): {:?}", ids.len(), ids))
                }
                (Some(reg), "poll") => {
                    let session_id = args.get(1).and_then(|s| Uuid::parse_str(s).ok());
                    match session_id {
                        Some(id) => {
                            let cell_opt = {
                                let g = reg.read().await;
                                g.get(&id).map(|(_task, cell)| cell.clone())
                            };
                            let Some(cell) = cell_opt else {
                                return (false, format!("[process poll] unknown session_id: {}", id));
                            };
                            let result_opt = cell.write().await.take();
                            match result_opt {
                                Some(Ok((out, res))) => {
                                    reg.write().await.remove(&id);
                                    let stdout = String::from_utf8_lossy(&out.stdout);
                                    let stderr = String::from_utf8_lossy(&out.stderr);
                                    let base_msg = format!(
                                        "[process poll {}] done — exit {} stdout: {} stderr: {}",
                                        id,
                                        out.status.code().unwrap_or(-1),
                                        stdout.trim(),
                                        stderr.trim()
                                    );
                                    if res.success {
                                        (true, base_msg)
                                    } else {
                                        (false, format!("{} | summary: {}", base_msg, res.summary))
                                    }
                                }
                                Some(Err(e)) => {
                                    reg.write().await.remove(&id);
                                    (false, format!("[process poll {}] error: {}", id, e))
                                }
                                None => (true, format!("[process poll {}] still running", id)),
                            }
                        }
                        None => (false, "[process poll] usage: process poll <session_id>".to_string()),
                    }
                }
                (Some(reg), "kill") => {
                    let session_id = args.get(1).and_then(|s| Uuid::parse_str(s).ok());
                    match session_id {
                        Some(id) => {
                            let mut g = reg.write().await;
                            if let Some((task, _cell)) = g.remove(&id) {
                                task.abort();
                                (true, format!("[process kill {}] aborted", id))
                            } else {
                                (false, format!("[process kill] unknown session_id: {}", id))
                            }
                        }
                        None => (false, "[process kill] usage: process kill <session_id>".to_string()),
                    }
                }
                (_, _) => (false, "[process] usage: process list | process poll <session_id> | process kill <session_id>".to_string()),
            }
        }
        "memory_search" => {
            let query_str = args.get(0).map(|a| a.as_str()).unwrap_or("").trim();
            let top_k = args.get(1).and_then(|s| s.parse::<usize>().ok()).unwrap_or(5).min(20);
            if query_str.is_empty() {
                return (false, "[memory_search] usage: memory_search <query> [top_k]".to_string());
            }
            match long_term_client {
                Some(client) => {
                    let client = client.clone();
                    let query = query_str.to_string();
                    let results = tokio::task::spawn_blocking(move || client.search(query, top_k))
                        .await
                        .ok()
                        .unwrap_or_default();
                    if results.is_empty() {
                        (true, format!("[memory_search] no results for \"{}\"", query_str))
                    } else {
                        let preview: Vec<String> = results.iter().take(5).map(|(id, content)| format!("id: {} — {}", id, content.replace('\n', " "))).collect();
                        (true, format!("[memory_search] {} result(s): {}", results.len(), preview.join(" | ")))
                    }
                }
                None => (false, "[memory_search] long-term memory not available".to_string()),
            }
        }
        "memory_store" => {
            let content = args.get(0).map(|a| a.as_str()).unwrap_or("");
            let source = args.get(1).map(|a| a.as_str()).unwrap_or("agent");
            if content.is_empty() {
                return (false, "[memory_store] usage: memory_store <content> <source>".to_string());
            }
            match long_term_client {
                Some(client) => {
                    let client = client.clone();
                    let content = content.to_string();
                    let source = source.to_string();
                    let out = tokio::task::spawn_blocking(move || client.promote(content, source))
                        .await
                        .ok()
                        .and_then(|r| r.ok());
                    match out {
                        Some(()) => (true, "[memory_store] stored".to_string()),
                        None => (false, "[memory_store] failed or memory not available".to_string()),
                    }
                }
                None => (false, "[memory_store] long-term memory not available".to_string()),
            }
        }
        "memory_delete" => {
            let id = args.get(0).map(|a| a.as_str()).unwrap_or("").trim();
            if id.is_empty() {
                return (false, "[memory_delete] usage: memory_delete <id> (UUID de l'entrée)".to_string());
            }
            match long_term_client {
                Some(client) => {
                    let client = client.clone();
                    let id = id.to_string();
                    let out = tokio::task::spawn_blocking(move || client.delete(id))
                        .await
                        .ok()
                        .and_then(|r| r.ok());
                    match out {
                        Some(()) => (true, "[memory_delete] deleted".to_string()),
                        None => (false, "[memory_delete] failed or not found (vérifiez l'id)".to_string()),
                    }
                }
                None => (false, "[memory_delete] long-term memory not available".to_string()),
            }
        }
        "sessions_list" => {
            let limit = args.get(0).and_then(|s| s.parse::<usize>().ok()).unwrap_or(20).min(50);
            match store_path {
                Some(path) => match TaskStore::open(path) {
                    Ok(store) => match store.get_all() {
                        Ok(tasks) => {
                            let list: Vec<String> = tasks
                                .into_iter()
                                .rev()
                                .take(limit)
                                .map(|t| format!("{} {} {}", t.id, t.status.as_str(), t.assigned_agent))
                                .collect();
                            (true, format!("[sessions_list] {} task(s): {}", list.len(), list.join(" ; ")))
                        }
                        Err(e) => (false, format!("[sessions_list] error: {}", e)),
                    },
                    Err(e) => (false, format!("[sessions_list] store error: {}", e)),
                },
                None => (false, "[sessions_list] store not available".to_string()),
            }
        }
        "session_status" => {
            let task_id_str = args.get(0).map(String::as_str).unwrap_or("");
            let id = task_id_str.parse::<Uuid>().ok();
            match (store_path, id) {
                (Some(path), Some(id)) => match TaskStore::open(path) {
                    Ok(store) => match store.get(id) {
                        Ok(Some(t)) => (true, format!(
                            "[session_status] {} status={} agent={}",
                            t.id,
                            t.status.as_str(),
                            t.assigned_agent
                        )),
                        Ok(None) => (false, format!("[session_status] task {} not found", id)),
                        Err(e) => (false, format!("[session_status] error: {}", e)),
                    },
                    Err(e) => (false, format!("[session_status] store error: {}", e)),
                },
                (_, _) => (false, "[session_status] usage: session_status <task_id>".to_string()),
            }
        }
        "sessions_spawn" => {
            let message = args.get(0).map(|a| a.as_str()).unwrap_or("").to_string();
            let child_session_id = args.get(1).map(|a| a.as_str()).unwrap_or("").to_string();
            if message.is_empty() {
                return (false, "[sessions_spawn] usage: sessions_spawn <message> [session_id]".to_string());
            }
            match (store_path, conv_tx) {
                (Some(path), Some(tx)) => {
                    let new_id = Uuid::new_v4();
                    let now = chrono::Utc::now();
                    let task = Task {
                        id: new_id,
                        parent_task_id: Some(task_id),
                        status: TaskStatus::Pending,
                        assigned_agent: "conversation".to_string(),
                        created_at: now,
                        updated_at: now,
                    };
                    match TaskStore::open(path) {
                        Ok(store) => {
                            if store.insert(&task).is_err() {
                                return (false, "[sessions_spawn] failed to insert task".to_string());
                            }
                            let sid = if child_session_id.is_empty() {
                                new_id.to_string()
                            } else {
                                child_session_id
                            };
                            if tx.send((new_id, message, sid)).await.is_err() {
                                return (false, "[sessions_spawn] failed to send to conversation queue".to_string());
                            }
                            (true, format!("[sessions_spawn] task_id: {} (queued)", new_id))
                        }
                        Err(e) => (false, format!("[sessions_spawn] store error: {}", e)),
                    }
                }
                (_, _) => (false, "[sessions_spawn] store or conversation channel not available".to_string()),
            }
        }
        "message" => {
            let sub = args.get(0).map(String::as_str).unwrap_or("");
            if sub != "send" || args.len() < 3 {
                return (false, "[message] usage: message send <channel> <text>".to_string());
            }
            let channel = args.get(1).map(String::as_str).unwrap_or("");
            let text = args.get(2..).map(|a| a.join(" ")).unwrap_or_default();
            match message_webhook_url {
                Some(url) => {
                    let body = serde_json::json!({ "channel": channel, "text": text });
                    let client = match reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(10))
                        .build()
                    {
                        Ok(c) => c,
                        Err(e) => return (false, format!("[message] client error: {}", e)),
                    };
                    match client.post(url).json(&body).send().await {
                        Ok(res) if res.status().is_success() => (true, "[message] sent".to_string()),
                        Ok(res) => (false, format!("[message] send failed: {}", res.status())),
                        Err(e) => (false, format!("[message] error: {}", e)),
                    }
                }
                None => (false, "[message] AKASHA_MESSAGE_WEBHOOK_URL not set".to_string()),
            }
        }
        "browser" => (false, "[browser] browser automation not implemented (planned Phase 3)".to_string()),
        "image" => (false, "[image] image analysis (vision) not implemented (planned Phase 3)".to_string()),
        "pdf" => (false, "[pdf] PDF extraction not implemented (planned Phase 3)".to_string()),
        "search_files" => {
            let dir = path_arg(0).unwrap_or(Path::new("."));
            let pattern = args.get(1).map(String::as_str).unwrap_or("*");
            match executor.search_files(dir, pattern).await {
                Ok((paths, res)) => {
                    let msg = if res.success {
                        let list: Vec<String> = paths.iter().take(20).map(|p| p.display().to_string()).collect();
                        format!("[search_files] found {}: {:?}", paths.len(), list)
                    } else {
                        format!("[search_files] {}", res.summary)
                    };
                    (res.success, msg)
                }
                Err(e) => (false, format!("[search_files] error: {}", e)),
            }
        }
        "grep_content" => {
            let dir = path_arg(0).unwrap_or(Path::new("."));
            let pattern = args.get(1).map(String::as_str).unwrap_or("");
            let file_glob = args.get(2).map(String::as_str).filter(|s| !s.is_empty());
            if pattern.is_empty() {
                return (false, "[grep_content] usage: grep_content <dir> <pattern> [file_glob]".to_string());
            }
            match executor.grep_content(dir, pattern, file_glob, 50).await {
                Ok((matches, res)) => {
                    let msg = if res.success {
                        let lines: Vec<String> = matches
                            .iter()
                            .take(30)
                            .map(|(p, n, line)| format!("{}:{}: {}", p.display(), n, line.trim()))
                            .collect();
                        format!("[grep_content] {} — {}", res.summary, lines.join(" ; "))
                    } else {
                        format!("[grep_content] {}", res.summary)
                    };
                    (res.success, msg)
                }
                Err(e) => (false, format!("[grep_content] error: {}", e)),
            }
        }
        "write_file" => {
            let path = match path_arg(0) {
                Some(p) => p,
                None => return (false, "[write_file] usage: write_file <path> <content>".to_string()),
            };
            let content = args.get(1..).map(|a| a.join(" ")).unwrap_or_default();
            match executor.write_file(path, &content).await {
                Ok(res) => {
                    let msg = if res.success {
                        format!("[write_file {}] {}", path.display(), res.summary)
                    } else {
                        format!("[write_file] {}", res.summary)
                    };
                    (res.success, msg)
                }
                Err(e) => (false, format!("[write_file] error: {}", e)),
            }
        }
        "search_replace" => {
            let path = path_arg(0);
            let rest = args.get(1..).map(|a| a.join(" ")).unwrap_or_default();
            let Some((search, replace)) = rest
                .split_once('|')
                .map(|(s, r)| (s.trim().to_string(), r.trim().to_string()))
            else {
                return (false, "[search_replace] usage: search_replace <path> <search> | <replace>".to_string());
            };
            match path {
                Some(p) => match executor.search_replace(p, &search, &replace).await {
                    Ok(res) => {
                        let msg = if res.success {
                            format!("[search_replace {}] {}", p.display(), res.summary)
                        } else {
                            format!("[search_replace] {}", res.summary)
                        };
                        (res.success, msg)
                    }
                    Err(e) => (false, format!("[search_replace] error: {}", e)),
                },
                None => (false, "[search_replace] usage: search_replace <path> <search> | <replace>".to_string()),
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
                        let msg = if res.success {
                            format!("[edit_file {}] {}", p.display(), res.summary)
                        } else {
                            format!("[edit_file] {}", res.summary)
                        };
                        (res.success, msg)
                    }
                    Err(e) => (false, format!("[edit_file] error: {}", e)),
                },
                None => (false, "[edit_file] usage: edit_file <path> <start_line> <end_line> <new_content>".to_string()),
            }
        }
        "apply_patch" => {
            let path = path_arg(0);
            let patch_content = args.get(1..).map(|a| a.join("\n")).unwrap_or_default();
            match path {
                Some(p) => match executor.apply_patch(p, &patch_content).await {
                    Ok(res) => {
                        let msg = if res.success {
                            format!("[apply_patch {}] {}", p.display(), res.summary)
                        } else {
                            format!("[apply_patch] {}", res.summary)
                        };
                        (res.success, msg)
                    }
                    Err(e) => (false, format!("[apply_patch] error: {}", e)),
                },
                None => (false, "[apply_patch] usage: apply_patch <path> <patch_content>".to_string()),
            }
        }
        "file_diff" => {
            let path_a = path_arg(0);
            let path_b = path_arg(1);
            match (path_a, path_b) {
                (Some(a), Some(b)) => match executor.file_diff(a, b).await {
                    Ok((diff, res)) => {
                        let msg = if res.success {
                            let preview = if diff.len() <= 400 { diff.as_str() } else { &diff[..400] };
                            format!("[file_diff] {} — {}", res.summary, preview)
                        } else {
                            format!("[file_diff] {}", res.summary)
                        };
                        (res.success, msg)
                    }
                    Err(e) => (false, format!("[file_diff] error: {}", e)),
                },
                _ => (false, "[file_diff] usage: file_diff <path_a> <path_b>".to_string()),
            }
        }
        "web_fetch" => {
            let url = args.get(0).map(String::as_str).unwrap_or("");
            if url.is_empty() {
                return (false, "[web_fetch] usage: web_fetch <url>".to_string());
            }
            match executor.web_fetch(url).await {
                Ok((body, res)) => {
                    let msg = if res.success {
                        let preview = if body.len() <= 500 { body.as_str() } else { &body[..500] };
                        format!("[web_fetch] {} — {}", res.summary, preview)
                    } else {
                        format!("[web_fetch] {}", res.summary)
                    };
                    (res.success, msg)
                }
                Err(e) => (false, format!("[web_fetch] error: {}", e)),
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
                return (false, "[web_search] usage: web_search <query> [max_results]".to_string());
            }
            match executor.web_search(query.trim(), max_results).await {
                Ok((body, res)) => {
                    let msg = if res.success {
                        let preview = if body.len() <= 600 { body.as_str() } else { &body[..600] };
                        format!("[web_search] {} — {}", res.summary, preview)
                    } else {
                        format!("[web_search] {}", res.summary)
                    };
                    (res.success, msg)
                }
                Err(e) => (false, format!("[web_search] error: {}", e)),
            }
        }
        "run_in_container" => {
            let work_dir = path_arg(0);
            let image = args.get(1).map(String::as_str).unwrap_or("");
            let command = args.get(2).map(String::as_str).unwrap_or("");
            if work_dir.is_none() || image.is_empty() || command.is_empty() {
                return (false, "[run_in_container] usage: run_in_container <work_dir> <image> <command> [args...]".to_string());
            }
            let work_dir = work_dir.unwrap();
            let cmd_args: Vec<String> = args.iter().skip(3).cloned().collect();
            match executor.run_in_container(work_dir, image, command, &cmd_args, 300, 512).await {
                Ok((stdout, stderr, exit_code, _res)) => {
                    let out = String::from_utf8_lossy(&stdout);
                    let err = String::from_utf8_lossy(&stderr);
                    let success = exit_code == 0;
                    (success, format!(
                        "[run_in_container] exit {} — stdout: {} stderr: {}",
                        exit_code,
                        if out.len() > 400 { format!("{}...", &out[..400]) } else { out.to_string() },
                        if err.len() > 200 { format!("{}...", &err[..200]) } else { err.to_string() }
                    ))
                }
                Err(e) => (false, format!("[run_in_container] error: {}", e)),
            }
        }
        _ => {
            let names: Vec<&str> = AVAILABLE_TOOLS.iter().map(|(n, _)| *n).collect();
            (false, format!("[{}] unknown tool. Available: {}.", tool_name, names.join(", ")))
        }
    };
    result
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

/// At daemon startup: if yesterday's short-term file exists, summarize it via LLM and promote to long-term (source "daily_summary").
pub async fn summarize_yesterday_and_promote(
    short_term_dir: PathBuf,
    llm_router: Arc<akasha_llm::LLMRouter>,
    long_term_client: Option<LongTermMemoryClient>,
) {
    let Some(client) = long_term_client else { return };
    let yesterday = chrono::Utc::now() - chrono::Duration::days(1);
    let session_id = format!("day-{}", yesterday.format("%Y-%m-%d"));
    let turns = match ShortTermStore::read_day_from_disk(&session_id, &short_term_dir) {
        Some(t) if !t.is_empty() => t,
        _ => return,
    };
    let blob = ShortTermStore::turns_to_context(&turns);
    let summary_prompt = format!(
        "Résume en un court paragraphe synthétique (5 à 10 lignes) la journée du {} : sujets abordés, décisions, projets ou informations importantes. \
Réponse en français, factuelle.\n\n{}",
        session_id.trim_start_matches("day-"),
        blob
    );
    let summary_max_tokens = std::env::var("AKASHA_SYSTEM_TASK_MAX_TOKENS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1024);
    let req = CompletionRequest {
        prompt: summary_prompt,
        max_tokens: Some(summary_max_tokens),
        temperature: Some(0.2),
        preferred_task_type: Some("system".to_string()),
    };
    match llm_router.complete(&req).await {
        Ok(resp) => {
            let summary = resp.text.trim();
            if !summary.is_empty() {
                let content = format!("Résumé du {} : {}", session_id.trim_start_matches("day-"), summary);
                let client = client.clone();
                match tokio::task::spawn_blocking(move || client.promote(content, "daily_summary".to_string())).await {
                    Ok(Ok(())) => tracing::info!(session_id = %session_id, "Yesterday summarized and stored in long-term memory"),
                    Ok(Err(e)) => tracing::warn!(error = %e, "Daily summary promote to long-term failed"),
                    Err(e) => tracing::warn!(error = %e, "Daily summary task join failed"),
                }
            }
        }
        Err(e) => tracing::debug!(error = %e, "Daily summary LLM call failed"),
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
    conv_tx: Option<mpsc::Sender<OrchestratorTask>>,
    human_input_store: Option<HumanInputStore>,
) {
    let store = match TaskStore::open(&store_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "LLM task: store open failed");
            return;
        }
    };
    let _ = store.update_status(task_id, TaskStatus::Running);

    let message_webhook_url = std::env::var("AKASHA_MESSAGE_WEBHOOK_URL").ok();

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
        let allowed_tools = tools_executor.as_ref().and_then(|e| e.policy.allowed_tool_list());
        let base = available_tools_instruction(allowed_tools.as_deref());
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
            "\n\nYou may request tools by writing a line: TOOL: tool_name arg1 arg2 ...\nAvailable: {}{}.\n\
             When you need the user to provide information (choice, confirmation, or free text), use TOOL: ask_user then on the next line a single JSON: {{\"question\":\"...\", \"context\":\"...\", \"choices\":[\"a\",\"b\"]}} (context and choices optional).\n\
             When a task requires connecting to an external service (account, API key, credentials), use ask_user to ask for the connection details or to explain how the user can provide or configure them (e.g. env var, Akasha vault); then continue the task once the user has replied.\n\
             If you need no tool, reply normally with your answer.\n\
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

    // Agent profile: name, personality, rules, can/cannot (persisted in data_dir/agent_profile.json)
    let data_dir = store_path.parent().unwrap_or_else(|| store_path.as_ref());
    let agent_profile = AgentProfile::load(data_dir);
    let profile_block = agent_profile.format_for_prompt();
    if !profile_block.is_empty() {
        context_prefix.push_str(&profile_block);
    }

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
            for (_, content) in &results {
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
                    for (_, content) in &user_memories {
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
    let mut tool_loop_history: Vec<(String, String)> = Vec::new();

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
                let args_str = args.join(" ");
                tool_loop_history.push((actual_tool.clone(), args_str.clone()));
                // Phase 4: loop detection — same tool+args repeated 3 times
                if tool_loop_history.len() >= 3 {
                    let last = tool_loop_history.last().unwrap();
                    if tool_loop_history.iter().rev().take(3).all(|e| e.0 == last.0 && e.1 == last.1) {
                        reply_text = "Loop detected: same tool and arguments repeated. Stopping.".to_string();
                        break 'tool_rounds;
                    }
                }
                let (success, res) = if actual_tool == "ask_user" {
                    // Human in the loop: register pending request, emit event, wait for user reply.
                    match &human_input_store {
                        Some(store) => {
                            let body = args.first().map(String::as_str).unwrap_or("{}");
                            let v = serde_json::from_str::<serde_json::Value>(body).ok();
                            let (question, context, choices) = match &v {
                                Some(v) => (
                                    v.get("question").and_then(|q| q.as_str()).unwrap_or("").to_string(),
                                    v.get("context").and_then(|c| c.as_str()).unwrap_or("").to_string(),
                                    v.get("choices").and_then(|c| c.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>()),
                                ),
                                None => (body.to_string(), String::new(), None),
                            };
                            if question.is_empty() {
                                (false, format!("[ask_user] invalid JSON: question required. Got: {}", body.chars().take(100).collect::<String>()))
                            } else {
                                let (tx, rx) = tokio::sync::oneshot::channel();
                                let pending = PendingHumanInput {
                                    question: question.clone(),
                                    context: context.clone(),
                                    choices: choices.clone(),
                                    response_tx: tx,
                                };
                                {
                                    let mut g = store.write().await;
                                    g.insert(task_id, pending);
                                }
                                let payload = serde_json::json!({
                                    "task_id": task_id.to_string(),
                                    "question": question,
                                    "context": context,
                                    "choices": choices
                                });
                                let _ = bus.send(
                                    EventEnvelope::new(EventType::TaskWaitingUserInput, Some(payload)).with_correlation(task_id),
                                );
                                const HUMAN_INPUT_TIMEOUT_SECS: u64 = 3600;
                                match tokio::time::timeout(
                                    std::time::Duration::from_secs(HUMAN_INPUT_TIMEOUT_SECS),
                                    rx,
                                ).await {
                                    Ok(Ok(reply)) => (true, format!("[ask_user] User replied: {}", reply)),
                                    Ok(Err(_)) => (false, "[ask_user] Channel closed.".to_string()),
                                    Err(_) => (false, format!("[ask_user] Timeout after {}s; no user reply.", HUMAN_INPUT_TIMEOUT_SECS)),
                                }
                            }
                        }
                        None => (false, "[ask_user] Human-in-the-loop not available.".to_string()),
                    }
                } else {
                    execute_tool_call(
                        exec,
                        &actual_tool,
                        args,
                        process_registry.as_ref(),
                        long_term_client.as_ref(),
                        task_id,
                        Some(store_path.as_path()),
                        conv_tx.clone(),
                        message_webhook_url.as_deref(),
                    )
                    .await
                };
                // Phase F: emit ToolInvoked for Actions tab (spec 33)
                // Redact or truncate args in the event to avoid leaking large blobs or secrets.
                let redacted_args: Vec<String> = if matches!(actual_tool.as_str(), "apply_patch" | "edit_file" | "write_file") {
                    vec!["[redacted for write-like tool]".to_string()]
                } else {
                    const MAX_ARG_PREVIEW_LEN: usize = 512;
                    args.iter()
                        .map(|arg| {
                            if arg.len() > MAX_ARG_PREVIEW_LEN {
                                format!("{}...[truncated {} chars]", &arg[..MAX_ARG_PREVIEW_LEN], arg.len().saturating_sub(MAX_ARG_PREVIEW_LEN))
                            } else {
                                arg.clone()
                            }
                        })
                        .collect()
                };
                let payload = serde_json::json!({
                    "tool": actual_tool,
                    "skill": if &actual_tool != name { Some(name.as_str()) } else { None::<&str> },
                    "args": redacted_args,
                    "result_preview": if res.len() > 300 { format!("{}...", &res[..300]) } else { res.clone() },
                    "success": success
                });
                let _ = bus.send(
                    EventEnvelope::new(EventType::ToolInvoked, Some(payload)).with_correlation(task_id),
                );
                if success {
                    log_tool_journal_if_write(&actual_tool, args, &res).await;
                }
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

        // Then spawn LLM extraction for projects, interests, important info, personal facts, and agent profile (async).
        let msg = message.clone();
        let reply = reply_text.clone();
        let client = long_term.clone();
        let router = llm_router.clone();
        let heuristic_set: std::collections::HashSet<String> = heuristic_facts.iter().cloned().collect();
        let sem = extract_semaphore();
        let data_dir_for_extract = data_dir.to_path_buf();
        tokio::spawn(async move {
            let _permit = match sem.try_acquire() {
                Ok(p) => p,
                Err(_) => {
                    tracing::debug!("Background fact extraction skipped: semaphore full");
                    return;
                }
            };
            let extract_prompt = format!(
                "Extrais les éléments à retenir. Une ligne par élément, chaque ligne commence par exactement un des préfixes suivants :\n\
FACT: faits personnels (nom, prénom, préférences, décisions)\n\
PROJECT: projets créés ou mentionnés\n\
INTEREST: centres d'intérêt\n\
IMPORTANT: informations importantes à retenir\n\
AGENT_NAME: le nom que l'utilisateur donne à l'agent (ex: Tu t'appelles X)\n\
AGENT_PERSONALITY: personnalité ou ton demandé pour l'agent\n\
AGENT_RULE: une règle que l'agent doit respecter\n\
AGENT_CAN: ce que l'agent peut faire (autorisé)\n\
AGENT_CANNOT: ce que l'agent ne doit pas faire (interdit)\n\
N'écris que des lignes avec ces préfixes, ou NOTHING si rien. Pas d'autre texte.\n\
N'extrais que des faits explicitement mentionnés (par l'utilisateur ou l'assistant). N'invente rien.\n\nUtilisateur: {}\n\nAssistant: {}",
                msg.trim(),
                reply.trim()
            );
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
            let mut to_promote: Vec<(String, String)> = Vec::new();
            let mut agent_updates: Vec<(String, String)> = Vec::new();
            if let Ok(Ok(resp)) = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                router.complete(&req),
            ).await {
                for line in resp.text.lines() {
                    let line = line.trim();
                    if let Some(rest) = line.strip_prefix("AGENT_NAME:") {
                        agent_updates.push(("AGENT_NAME".to_string(), rest.trim().to_string()));
                    } else if let Some(rest) = line.strip_prefix("AGENT_PERSONALITY:") {
                        agent_updates.push(("AGENT_PERSONALITY".to_string(), rest.trim().to_string()));
                    } else if let Some(rest) = line.strip_prefix("AGENT_RULE:") {
                        agent_updates.push(("AGENT_RULE".to_string(), rest.trim().to_string()));
                    } else if let Some(rest) = line.strip_prefix("AGENT_CAN:") {
                        agent_updates.push(("AGENT_CAN".to_string(), rest.trim().to_string()));
                    } else if let Some(rest) = line.strip_prefix("AGENT_CANNOT:") {
                        agent_updates.push(("AGENT_CANNOT".to_string(), rest.trim().to_string()));
                    } else {
                        let (content, source) = if let Some(rest) = line.strip_prefix("FACT:") {
                            (rest.trim().to_string(), "user_fact".to_string())
                        } else if let Some(rest) = line.strip_prefix("PROJECT:") {
                            (rest.trim().to_string(), "project".to_string())
                        } else if let Some(rest) = line.strip_prefix("INTEREST:") {
                            (rest.trim().to_string(), "interest".to_string())
                        } else if let Some(rest) = line.strip_prefix("IMPORTANT:") {
                            (rest.trim().to_string(), "important".to_string())
                        } else {
                            continue;
                        };
                        if !content.is_empty() && !heuristic_set.contains(&content) {
                            to_promote.push((content, source));
                        }
                    }
                }
            }
            if !agent_updates.is_empty() {
                let mut profile = AgentProfile::load(&data_dir_for_extract);
                for (kind, value) in agent_updates {
                    profile.apply_extracted(&kind, value);
                }
                if let Err(e) = profile.save(&data_dir_for_extract) {
                    tracing::warn!(error = %e, "Failed to save agent profile");
                } else {
                    tracing::info!("Agent profile updated from conversation");
                }
            }
            if !to_promote.is_empty() {
                tracing::debug!(count = to_promote.len(), "Promoting extracted items to long-term memory");
            }
            for (content, source) in to_promote {
                let client = client.clone();
                let c = content.clone();
                let s = source.clone();
                match tokio::task::spawn_blocking(move || client.promote(c, s)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!(error = %e, "Long-term promote failed"),
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
    human_input_store: Option<HumanInputStore>,
) -> String {
    let data_dir = store_path.parent().unwrap_or_else(|| store_path.as_ref());

    if method == "GET" && (path == "/" || path.is_empty()) {
        return json_response("200 OK", r#"{"status":"ok"}"#);
    }

    // GET /api/agent-profile — read agent profile (name, personality, rules, can_do, cannot_do)
    if method == "GET" && path == "/api/agent-profile" {
        let profile = AgentProfile::load(data_dir);
        let body_json = serde_json::json!({
            "name": profile.name,
            "personality": profile.personality,
            "rules": profile.rules,
            "can_do": profile.can_do,
            "cannot_do": profile.cannot_do
        });
        return json_response("200 OK", &body_json.to_string());
    }

    // POST /api/agent-profile — update agent profile (merge with existing). Body: { name?, personality?, rules?, can_do?, cannot_do? }
    if method == "POST" && path == "/api/agent-profile" {
        let mut profile = AgentProfile::load(data_dir);
        if let Some(body) = body.as_deref() {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) {
                if let Some(s) = v.get("name").and_then(|x| x.as_str()) {
                    profile.name = Some(s.to_string());
                }
                if let Some(s) = v.get("personality").and_then(|x| x.as_str()) {
                    profile.personality = Some(s.to_string());
                }
                if let Some(arr) = v.get("rules").and_then(|x| x.as_array()) {
                    profile.rules = arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
                }
                if let Some(arr) = v.get("can_do").and_then(|x| x.as_array()) {
                    profile.can_do = arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
                }
                if let Some(arr) = v.get("cannot_do").and_then(|x| x.as_array()) {
                    profile.cannot_do = arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
                }
            }
        }
        match profile.save(data_dir) {
            Ok(()) => return json_response("200 OK", r#"{"ok":true,"message":"Profil agent mis à jour"}"#),
            Err(e) => return json_response("500 Internal Server Error", &serde_json::json!({ "error": e.to_string() }).to_string()),
        }
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
            .map(|(id, content, created_at, source)| {
                serde_json::json!({ "id": id, "content": content, "created_at": created_at, "source": source })
            })
            .collect();
        let body_json = serde_json::json!({
            "entries": list,
            "long_term_available": long_term_client.is_some()
        });
        return json_response("200 OK", &body_json.to_string());
    }

    // DELETE /api/memory/long-term/:id — delete one long-term memory entry by id
    if method == "DELETE" && path.starts_with("/api/memory/long-term/") {
        let id = path.trim_start_matches("/api/memory/long-term/").split('?').next().unwrap_or("").trim();
        if id.is_empty() {
            return json_response("400 Bad Request", r#"{"error":"missing id"}"#);
        }
        let result = match long_term_client {
            Some(ref client) => {
                let client = client.clone();
                let id = id.to_string();
                tokio::task::spawn_blocking(move || client.delete(id))
                    .await
                    .unwrap_or_else(|e| Err(format!("task join error: {}", e)))
            }
            None => Err("long-term memory not available".to_string()),
        };
        match result {
            Ok(()) => return json_response("200 OK", r#"{"deleted":true}"#),
            Err(e) if e == "not found" || e == "invalid uuid" => {
                return json_response("404 Not Found", &format!(r#"{{"error":"{}"}}"#, e));
            }
            Err(e) => return json_response("500 Internal Server Error", &format!(r#"{{"error":"{}"}}"#, e)),
        }
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

    // DELETE /api/vault — remove one key (body: {"key": "KEY_NAME"})
    if method == "DELETE" && path == "/api/vault" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let key = body_json.as_ref().and_then(|j| j.get("key")).and_then(|v| v.as_str()).map(String::from);
        match key {
            Some(k) if !k.is_empty() => {
                match akasha_vault::open_vault(data_dir) {
                    Ok(v) => match v.delete(&k) {
                        Ok(()) => return json_response("200 OK", &serde_json::json!({ "ok": true, "key": k }).to_string()),
                        Err(akasha_vault::VaultError::NotFound(_)) => return json_response("404 Not Found", &serde_json::json!({ "error": "not_found", "key": k }).to_string()),
                        Err(e) => return json_response("500 Internal Server Error", &serde_json::json!({ "error": e.to_string() }).to_string()),
                    },
                    Err(e) => return json_response("503 Service Unavailable", &serde_json::json!({ "error": e.to_string() }).to_string()),
                }
            }
            _ => return json_response("400 Bad Request", r#"{"error":"missing or empty key"}"#),
        }
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
        // Session: "new_session" => new UUID; else provided non-empty session_id; else day-YYYY-MM-DD (short-term = current day, survives UI restart).
        let session_id = {
            let new_session = body_json.as_ref().and_then(|v| v.get("new_session")).and_then(|v| v.as_bool()).unwrap_or(false);
            let provided = body_json.as_ref().and_then(|v| v.get("session_id").and_then(|v| v.as_str().map(String::from)));
            if new_session {
                uuid::Uuid::new_v4().to_string()
            } else if let Some(s) = provided {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    format!("day-{}", chrono::Utc::now().format("%Y-%m-%d"))
                } else {
                    trimmed.to_string()
                }
            } else {
                format!("day-{}", chrono::Utc::now().format("%Y-%m-%d"))
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
                    return get_task_events(events, store_path, id).await;
                }
                // Human in the loop: GET pending question/context/choices for the task
                if method == "GET" && parts.get(1) == Some(&"human-input") {
                    if let Some(ref store) = human_input_store {
                        let g = store.read().await;
                        if let Some(pending) = g.get(&id) {
                            let body = serde_json::json!({
                                "task_id": id.to_string(),
                                "question": pending.question,
                                "context": pending.context,
                                "choices": pending.choices
                            });
                            return json_response("200 OK", &body.to_string());
                        }
                    }
                    return json_response("404 Not Found", &serde_json::json!({ "error": "no_pending_human_input", "task_id": id.to_string() }).to_string());
                }
                // Human in the loop: POST user reply to unblock the agent
                if method == "POST" && parts.get(1) == Some(&"human-reply") {
                    let response_text = body.as_deref()
                        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
                        .and_then(|v| v.get("response").and_then(|r| r.as_str().map(String::from)))
                        .unwrap_or_else(|| String::new());
                    if let Some(ref store) = human_input_store {
                        let pending = {
                            let mut g = store.write().await;
                            g.remove(&id)
                        };
                        if let Some(pending) = pending {
                            let _ = pending.response_tx.send(response_text);
                            return json_response("200 OK", &serde_json::json!({ "ok": true, "message": "Réponse transmise à l'agent." }).to_string());
                        }
                    }
                    return json_response("404 Not Found", &serde_json::json!({ "error": "no_pending_human_input", "task_id": id.to_string() }).to_string());
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
    // For a root task with children, aggregate child progress so the UI shows intermediate percentages.
    if let Ok(children) = store.get_children(id) {
        if !children.is_empty() {
            let mut sum: u32 = 0;
            let g = progress.read().await;
            for child in &children {
                let child_pct = store
                    .get_progress(child.id)
                    .ok()
                    .and_then(|v| v.last().map(|(pct, _)| *pct as u32))
                    .or_else(|| {
                        g.get(&child.id)
                            .and_then(|q| q.back().map(|e| e.progress_pct as u32))
                    })
                    .unwrap_or(0);
                sum += child_pct;
            }
            let aggregated_pct = (sum / children.len() as u32).min(100) as u8;
            let root_last_pct = progress_list.last().map(|e| e.progress_pct).unwrap_or(0);
            let display_pct = aggregated_pct.max(root_last_pct);
            if progress_list.is_empty() {
                progress_list.push(ProgressEntry {
                    progress_pct: display_pct,
                    message: "Sous-tâches en cours.".to_string(),
                });
            } else if let Some(last) = progress_list.last_mut() {
                last.progress_pct = display_pct;
            }
        }
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
