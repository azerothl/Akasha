//! Simple HTTP API: POST /api/message, GET /api/tasks/:id, GET / (health)

use akasha_core::{EventEnvelope, EventType};
use akasha_vault::Vault;
use akasha_llm::CompletionRequest;
use akasha_store::{parse_todos_from_payload, Schedule, ScheduleStore, Task, TaskRunStatus, TaskStatus, TaskStore, TodoStatus};
pub use akasha_store::tasks::MAX_PROGRESS_PER_TASK;
use crate::agent_profile::AgentProfile;
use crate::user_profile::UserProfile;
use crate::agents::{interpret_message, EventBus, OrchestratorTask, TaskPriority};
use crate::memory::ShortTermStore;
use crate::memory_actor::LongTermMemoryClient;
use std::path::{Path, PathBuf};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::RwLock;
use uuid::Uuid;
use tracing::Instrument;

/// In-memory cache for AgentProfile to avoid repeated disk reads (invalidated on POST /api/agent-profile and after profile save in run_message_via_llm).
pub type AgentProfileCache = Arc<RwLock<Option<AgentProfile>>>;

/// Virtual workspace per task (Deep Agents-style). Paths prefixed with "workspace:/" or "workspace:" are read/written here instead of disk.
pub type TaskWorkspaceStore = Arc<RwLock<std::collections::HashMap<Uuid, std::collections::HashMap<String, String>>>>;

pub fn new_task_workspace_store() -> TaskWorkspaceStore {
    Arc::new(RwLock::new(std::collections::HashMap::new()))
}

pub fn new_agent_profile_cache() -> AgentProfileCache {
    Arc::new(RwLock::new(None))
}

/// Load profile from cache or disk and update cache.
pub async fn get_or_load_agent_profile(data_dir: &Path, cache: &AgentProfileCache) -> AgentProfile {
    {
        let g = cache.read().await;
        if let Some(ref p) = *g {
            return p.clone();
        }
    }
    let profile = AgentProfile::load(data_dir);
    {
        let mut g = cache.write().await;
        *g = Some(profile.clone());
    }
    profile
}

/// Update cache after profile save (call after writing to disk).
pub async fn set_agent_profile_cache(cache: &AgentProfileCache, profile: AgentProfile) {
    let mut g = cache.write().await;
    *g = Some(profile);
}

/// Cached result of fetching api/latest.json from the Akasha_app site (version, download_url, etc.).
#[derive(Default, Clone)]
pub struct UpdateStatus {
    pub remote_version: String,
    pub download_url: String,
    pub release_notes_url: Option<String>,
    pub last_checked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub error: Option<String>,
}

pub type UpdateCheckCache = Arc<RwLock<UpdateStatus>>;

pub fn new_update_check_cache() -> UpdateCheckCache {
    Arc::new(RwLock::new(UpdateStatus::default()))
}

/// Fetch {base}/api/latest.json and update the cache. Does not crash on network/parse errors.
pub async fn run_update_check_once(cache: &UpdateCheckCache, base_url: &str) {
    let base = base_url.trim_end_matches('/');
    let url = format!("{}/api/latest.json", base);
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let mut g = cache.write().await;
            g.error = Some(e.to_string());
            g.last_checked_at = Some(chrono::Utc::now());
            return;
        }
    };
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            let mut g = cache.write().await;
            g.error = Some(e.to_string());
            g.last_checked_at = Some(chrono::Utc::now());
            return;
        }
    };
    if !resp.status().is_success() {
        let mut g = cache.write().await;
        g.error = Some(format!("HTTP {}", resp.status()));
        g.last_checked_at = Some(chrono::Utc::now());
        return;
    }
    let data: serde_json::Value = match resp.json().await {
        Ok(d) => d,
        Err(e) => {
            let mut g = cache.write().await;
            g.error = Some(e.to_string());
            g.last_checked_at = Some(chrono::Utc::now());
            return;
        }
    };
    let mut g = cache.write().await;
    g.remote_version = data
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0")
        .to_string();
    g.download_url = data
        .get("download_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    g.release_notes_url = data.get("release_notes_url").and_then(|v| v.as_str()).map(String::from);
    g.last_checked_at = Some(chrono::Utc::now());
    g.error = None;
}

/// Request for a sub-agent delegation (from a worker to the orchestrator). Reply is sent on reply_tx.
pub struct DelegationRequest {
    pub requesting_task_id: Uuid,
    pub agent_type: String,
    pub message: String,
    pub reply_tx: oneshot::Sender<Result<String, String>>,
}

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
            let label = task_label(t.initial_message.as_ref(), &t.id);
            serde_json::json!({
                "id": t.id.to_string(),
                "parent_task_id": t.parent_task_id.map(|u| u.to_string()),
                "status": t.status.as_str(),
                "assigned_agent": t.assigned_agent,
                "created_at": t.created_at.to_rfc3339(),
                "updated_at": t.updated_at.to_rfc3339(),
                "label": label,
            })
        })
        .collect();
    let body = serde_json::json!({ "tasks": list });
    json_response("200 OK", &body.to_string())
}

async fn get_task_events(events: &EventsCache, id: Uuid) -> String {
    let mut list: Vec<TaskEventEntry> = {
        let g = events.read().await;
        g.get(&id)
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    };
    // Derive child task IDs from SubAgentSpawned events already in the in-memory cache —
    // avoids reopening SQLite (TaskStore) on every poll cycle (the endpoint is polled ~1.5s).
    let child_ids: Vec<Uuid> = list
        .iter()
        .filter(|e| e.event_type == "sub_agent_spawned")
        .filter_map(|e| {
            e.payload
                .as_ref()
                .and_then(|p| p.get("task_id"))
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
        })
        .collect();
    if !child_ids.is_empty() {
        let g = events.read().await;
        for child_id in child_ids {
            if let Some(q) = g.get(&child_id) {
                for e in q.iter().cloned() {
                    list.push(e);
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

/// Registry of task-completion notifiers: task_id → Notify. Written by the conversation worker
/// on task completion; awaited by run_delegation_handler and the orchestrator aggregator so they
/// can react immediately instead of polling TaskStore every 500 ms.
pub type TaskCompletionRegistry = Arc<RwLock<std::collections::HashMap<Uuid, Arc<tokio::sync::Notify>>>>;

/// Per-task and per-session LLM usage (tokens, cost USD) for GET /api/tasks/:id and cost visibility.
#[derive(Default)]
pub struct TaskUsageStore {
    by_task: RwLock<std::collections::HashMap<Uuid, (u64, f64)>>,
    by_session: RwLock<std::collections::HashMap<String, (u64, f64)>>,
}

impl TaskUsageStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn add(&self, task_id: Uuid, session_id: &str, tokens: u64, cost_usd: f64) {
        {
            let mut g = self.by_task.write().await;
            let e = g.entry(task_id).or_insert((0, 0.0));
            e.0 += tokens;
            e.1 += cost_usd;
        }
        if !session_id.is_empty() {
            let mut g = self.by_session.write().await;
            let e = g.entry(session_id.to_string()).or_insert((0, 0.0));
            e.0 += tokens;
            e.1 += cost_usd;
        }
    }
    pub async fn get_task(&self, task_id: Uuid) -> Option<(u64, f64)> {
        self.by_task.read().await.get(&task_id).copied()
    }
    pub async fn get_session(&self, session_id: &str) -> Option<(u64, f64)> {
        self.by_session.read().await.get(session_id).copied()
    }
}

pub fn new_task_completion_registry() -> TaskCompletionRegistry {
    Arc::new(RwLock::new(std::collections::HashMap::new()))
}

/// Runs in a loop: receives DelegationRequest, checks depth (root or direct child only), creates child task, sends to conv_tx, waits for child completion, sends reply on oneshot.
/// `delegation_sem`: semaphore for backpressure; when full, replies with "système surchargé".
pub async fn run_delegation_handler(
    mut delegation_rx: mpsc::Receiver<DelegationRequest>,
    conv_tx: mpsc::Sender<OrchestratorTask>,
    store_path: PathBuf,
    progress: ProgressCache,
    task_completion: TaskCompletionRegistry,
    delegation_sem: std::sync::Arc<tokio::sync::Semaphore>,
) {
    while let Some(req) = delegation_rx.recv().await {
        let permit = match delegation_sem.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("Delegation backpressure: max concurrent delegations reached");
                let _ = req.reply_tx.send(Err("Système surchargé, réessayez plus tard.".to_string()));
                continue;
            }
        };
        let span_guard = tracing::info_span!(
            "delegation",
            requesting_task_id = %req.requesting_task_id,
            agent_type = %req.agent_type
        )
        .entered();
        let store = match TaskStore::open(&store_path) {
            Ok(s) => s,
            Err(e) => {
                let _ = req.reply_tx.send(Err(format!("store open: {}", e)));
                continue;
            }
        };
        let requesting = match store.get(req.requesting_task_id) {
            Ok(Some(t)) => t,
            Ok(None) => {
                let _ = req.reply_tx.send(Err("requesting task not found".to_string()));
                continue;
            }
            Err(e) => {
                let _ = req.reply_tx.send(Err(format!("store: {}", e)));
                continue;
            }
        };
        if let Some(parent_id) = requesting.parent_task_id {
            if let Ok(Some(parent)) = store.get(parent_id) {
                if parent.parent_task_id.is_some() {
                    let _ = req.reply_tx.send(Err("max delegation depth (sous-sous-agent non autorisé)".to_string()));
                    continue;
                }
            }
        }
        let child_id = Uuid::new_v4();
        const WORKER_AGENT_TYPES: &[&str] = &[
            "search", "code", "conversation", "financial", "documentalist", "project_manager",
            "technical_writer", "research", "security_audit", "creative",
        ];
        let agent_type = if WORKER_AGENT_TYPES.contains(&req.agent_type.as_str()) {
            req.agent_type.clone()
        } else {
            "conversation".to_string()
        };
        const MAX_INITIAL_MSG: usize = 500;
        let initial_message = if req.message.chars().count() > MAX_INITIAL_MSG {
            Some(req.message.chars().take(MAX_INITIAL_MSG).chain(std::iter::once('…')).collect::<String>())
        } else if req.message.is_empty() {
            None
        } else {
            Some(req.message.clone())
        };
        let child_task = Task {
            id: child_id,
            parent_task_id: Some(req.requesting_task_id),
            status: TaskStatus::Pending,
            assigned_agent: agent_type.clone(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            initial_message,
        };
        if store.insert(&child_task).is_err() {
            let _ = req.reply_tx.send(Err("store insert failed".to_string()));
            continue;
        }
        // Drop the span guard before any await point: EnteredSpan is not Send and must
        // not be held across await boundaries in a Send future.
        drop(span_guard);
        // Register a completion notifier *before* sending to conv_tx so the worker can notify
        // even if it completes before the spawned waiter calls notified().
        let notify = Arc::new(tokio::sync::Notify::new());
        {
            let mut reg = task_completion.write().await;
            reg.insert(child_id, notify.clone());
        }
        if conv_tx
            .send(OrchestratorTask {
                task_id: child_id,
                message: req.message.clone(),
                session_id: String::new(),
                image_data_urls: None,
                execution_mode: None,
            })
            .await
            .is_err()
        {
            task_completion.write().await.remove(&child_id);
            let _ = req.reply_tx.send(Err("conv_tx closed".to_string()));
            continue;
        }
        let reply_tx = req.reply_tx;
        let store_path = store_path.clone();
        let progress = progress.clone();
        let task_completion = task_completion.clone();
        let child_id_span = child_id;
        let agent_type_span = agent_type.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let span = tracing::info_span!(
                "delegation_wait",
                child_task_id = %child_id_span,
                assigned_agent = %agent_type_span
            );
            const DELEGATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
            let timed_out = tokio::time::timeout(
                DELEGATION_TIMEOUT,
                notify.notified().instrument(span),
            )
            .await
            .is_err();
            // Ensure the registry entry is removed regardless of outcome.
            task_completion.write().await.remove(&child_id_span);
            if timed_out {
                let _ = reply_tx.send(Err("delegation timeout (5 min)".to_string()));
                return;
            }
            // Single store read to retrieve the final task status and result message.
            let store = match TaskStore::open(&store_path) {
                Ok(s) => s,
                Err(e) => {
                    let _ = reply_tx.send(Err(format!("TaskStore open failed: {}", e)));
                    return;
                }
            };
            let task = match store.get(child_id_span) {
                Ok(Some(t)) => t,
                _ => {
                    let _ = reply_tx.send(Err("child task not found in store".to_string()));
                    return;
                }
            };
            let msg = {
                let g = progress.read().await;
                g.get(&child_id_span)
                    .and_then(|q| q.back())
                    .map(|e| e.message.clone())
                    .unwrap_or_else(|| if matches!(task.status, TaskStatus::Failed) { "Échec.".to_string() } else { "Terminé.".to_string() })
            };
            let _ = reply_tx.send(match task.status {
                TaskStatus::Completed => Ok(msg),
                TaskStatus::Failed => Err(msg),
                _ => Err("child task did not complete successfully".to_string()),
            });
        });
    }
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

/// Parse headers from the first chunk to get header length and Content-Length. Returns (header_body_sep_index, content_length).
/// header_body_sep_index is the index of the start of "\r\n\r\n"; body starts at header_body_sep_index + 4.
pub fn parse_content_length(buf: &[u8]) -> Option<(usize, usize)> {
    let sep = b"\r\n\r\n";
    let header_end = buf.windows(sep.len()).position(|w| w == sep)?;
    let header_slice = &buf[..header_end];
    let mut content_length: Option<usize> = None;
    for line in header_slice.split(|&b| b == b'\n') {
        let line_str = String::from_utf8_lossy(line).to_string();
        let line_str = line_str.trim_end_matches('\r');
        if let Some((name, value)) = line_str.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                if let Ok(n) = value.trim().parse::<usize>() {
                    content_length = Some(n);
                }
                break;
            }
        }
    }
    content_length.map(|cl| (header_end, cl))
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
    const MAX_BODY_PARSE: usize = 10 * 1024 * 1024; // 10 MiB — refuse to allocate larger body (hypothesis B)
    let will_allocate = content_length > 0 && content_length <= MAX_BODY_PARSE && rest.len() >= content_length;
    // #region agent log
    if content_length > 0 {
        crate::debug_log::log("api.rs:parse_request", "body allocation check", &serde_json::json!({"content_length": content_length, "rest_len": rest.len(), "will_allocate": will_allocate, "max_body_parse": MAX_BODY_PARSE}), "B");
    }
    // #endregion
    let body = if will_allocate {
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
    ("read_file", "read_file <path> — lire le contenu d'un fichier texte. Path réel ou workspace:/<path> pour le workspace virtuel de la tâche."),
    ("write_file", "write_file <path> <content> — écrire du texte dans un fichier. Path réel (Windows/Unix) ou workspace:/<path> pour le workspace virtuel (temporaire). À UTILISER dès que l'utilisateur demande d'enregistrer, sauvegarder ou écrire un fichier ; ne jamais refuser ni proposer de copier-coller."),
    ("search_files", "search_files <dir> <pattern> — chercher des fichiers (glob) sous un répertoire"),
    ("grep_content", "grep_content <dir> <pattern> [file_glob] — chercher le motif dans le contenu des fichiers (ex. grep_content . \"fn \" \"*.rs\")"),
    ("run_command", "run_command <cmd> [arg1 arg2 ...] — exécuter une commande (autorisée par la politique)"),
    ("run_terminal", "run_terminal <cmd> [args...] — exécuter une commande (même que run_command)"),
    ("run_command_background", "run_command_background <cmd> [args...] — lancer en arrière-plan, retourne session_id pour process poll/kill"),
    ("terminal_session", "terminal_session — session PTY interactive (prévue ultérieurement, spec 43). Pour l’instant utiliser run_command / run_terminal pour une commande, run_command_background + process pour suivi."),
    ("process", "process list | process poll <session_id> | process kill <session_id> — lister, consulter ou arrêter des commandes en arrière-plan"),
    ("file_diff", "file_diff <path_a> <path_b> — diff texte entre deux fichiers"),
    ("edit_file", "edit_file <path> <start_line> <end_line> <new_content> — remplacer les lignes start..end par new_content (lignes 1-based)"),
    ("apply_patch", "apply_patch <path> <patch_content> — appliquer un patch unifié (contenu du patch après le path)"),
    ("search_replace", "search_replace <path> <search> | <replace> — remplacer toutes les occurrences de search par replace dans le fichier (séparateur \" | \")"),
    ("web_fetch", "web_fetch <url> — récupérer le contenu d'une URL (domaine autorisé dans tools_policy allowed_web_domains)"),
    ("web_search", "web_search <query> [max_results] — rechercher sur le web (Brave API; BRAVE_API_KEY, web_search_enabled)"),
    ("run_in_container", "run_in_container <work_dir> <image> <command> [args...] — exécuter une commande dans un conteneur (work_dir autorisé en lecture, ex. node:20 node index.js)"),
    ("memory_search", "memory_search <query> [top_k] — rechercher dans la mémoire long terme (si activée)"),
    ("memory_store", "memory_store <content> <source> [link_to: id1,id2...] [link_kind: spouse|child|birth_date|residence|same_person|...] — stocker en mémoire long terme ; optionnellement lier à des entrées (UUIDs) avec un type de relation."),
    ("memory_delete", "memory_delete <id> — supprimer une entrée de la mémoire long terme par son id (UUID)"),
    ("memory_forget", "memory_forget <query> — supprimer les entrées dont le contenu correspond aux mots-clés (plan moyen terme 9)"),
    ("memory_stats", "memory_stats — nombre d'entrées et taille approximative de la mémoire long terme"),
    ("memory_gc", "memory_gc [retention_days] [protect_sources...] — supprimer les entrées plus anciennes que N jours (sources protégées optionnelles, ex. user_fact project)"),
    ("sessions_list", "sessions_list [limit] — lister les tâches/sessions récentes"),
    ("sessions_spawn", "sessions_spawn <message> [session_id] — créer une sous-tâche et la lancer"),
    ("session_status", "session_status <task_id> — statut d'une tâche donnée"),
    ("message", "message send <channel> <text> — envoyer un message vers un canal (webhook configuré via AKASHA_MESSAGE_WEBHOOK_URL)"),
    ("browser", "browser navigate <url> | browser screenshot | browser snapshot — screenshot via device_invoke synthetic_input shortcut; navigate → use web_fetch for content"),
    ("image", "image <path|url> [prompt] — vision: joindre l'image en pièce jointe au chat (modèle vision dans llm_router)"),
    ("pdf", "pdf <path> — extraire le texte d'un PDF (path dans allowed_read_paths)"),
    ("ask_user", "ask_user — demande une information à l'utilisateur (human in the loop). Ligne suivante : JSON avec question (requis), context (optionnel), choices (optionnel, tableau de chaînes pour choix multiples). Exemple : {\"question\":\"Quel fichier ?\",\"context\":\"...\",\"choices\":[\"a.txt\",\"b.txt\"]}"),
    ("delegate_to_agent", "delegate_to_agent <agent_type> <message> — déléguer à un sous-agent. agent_type: search | code | conversation | financial | documentalist | project_manager | technical_writer | research | security_audit | creative | analyst | architect | frontend | backend | database | integration | qa | system | image_generation. Un seul niveau de délégation autorisé."),
    ("install_skill", "install_skill <url> — installer un skill depuis une URL GitHub (ex. https://github.com/BankrBot/skills/tree/main/bankr). Télécharge SKILL.md, l'enregistre dans le dossier skills, puis recharge les skills."),
    ("uninstall_skill", "uninstall_skill <name> — désinstaller un skill (supprime data_dir/skills/<name>, retire la commande de tools_policy si présente, recharge les skills)."),
    ("device_discover", "device_discover [interface] — lister les appareils accessibles (optionnel: local_media, system, network, usb). Filtre par politique allowed_device_interfaces / blocked_device_interfaces."),
    ("device_invoke", "device_invoke <interface> <device_id> <action> [params] — exécuter une action sur un appareil. local_media: caméra (device_id camera, action capture), micro (device_id microphone, action record). Appelle directement ; une fenêtre d'autorisation s'affichera dans l'UI. Ne pas demander à l'utilisateur d'« ouvrir l'UI » — utiliser l'outil. synthetic_input: device_id keyboard|mouse, action shortcut|key|type|mouse_move|mouse_click|..."),
    ("generate_image", "generate_image <prompt> [size] — générer une image par IA (ex. OpenAI DALL·E). Prompt en texte libre ; size optionnel (1024x1024, 512x512). Retourne l'image en data URL dans la réponse (spec 42)."),
    ("write_todos", "write_todos <payload> — définir la liste d'étapes (todo) de la tâche. Payload: JSON array [{\"title\":\"...\", \"status\":\"pending\"|\"done\"|\"cancelled\"}] ou une ligne par étape. Remplace toute la liste. Utiliser pour décomposer une tâche complexe et suivre la progression."),
    ("read_todos", "read_todos — retourne la liste des étapes (todos) de la tâche courante."),
    ("update_todo", "update_todo <index> <status> — marquer l'étape à l'index (1-based) comme status (done, cancelled)."),
    ("list_skills", "list_skills — retourne la liste des skills installés (nom et description). Utiliser avant read_skill pour charger le détail d'un skill."),
    ("read_skill", "read_skill <name> — charge le contenu (instructions, usage) du skill. À utiliser quand tu as besoin du détail d'un skill avant de l'invoquer par son nom."),
];

fn available_tools_instruction(allowed_tools: Option<&[String]>) -> String {
    let iter: Box<dyn Iterator<Item = &(&str, &str)>> = if let Some(allowed) = allowed_tools {
        Box::new(
            AVAILABLE_TOOLS
                .iter()
                .filter(move |(name, _)| {
                    *name == "ask_user" || *name == "install_skill" || *name == "uninstall_skill" || *name == "write_todos" || *name == "read_todos" || *name == "update_todo" || *name == "list_skills" || *name == "read_skill" || allowed.iter().any(|a| a == *name)
                }),
        )
    } else {
        Box::new(AVAILABLE_TOOLS.iter())
    };
    iter.map(|(_, desc)| *desc)
        .collect::<Vec<_>>()
        .join(" ; ")
}

/// Single-pass intent flags for a message (one to_lowercase() shared by all checks).
struct MessageIntentFlags {
    save_file: bool,
    external_info: bool,
    camera_or_mic: bool,
    image_generation: bool,
    code_generation: bool,
    /// User asks for GitHub repo/API info and mentions vault or GITHUB_TOKEN.
    github_with_vault: bool,
}

fn compute_message_intent_flags(message: &str) -> MessageIntentFlags {
    let m = message.to_lowercase();
    MessageIntentFlags {
        save_file: [
            "enregistre", "enregistrer", "sauvegarde", "sauvegarder", "écris dans", "ecris dans",
            "write to file", "save to", "save the file", "write the file", "dans le dossier",
            "dans le fichier", "dans un fichier", "sur le disque", "to disk", "to the file",
        ]
        .iter()
        .any(|k| m.contains(k)),
        external_info: [
            "météo", "meteo", "weather", "prévisions", "previsions", "actualités", "actualites",
            "horaires", "trafic", "prix", "cours ", "bourse", "news", "nouvelle", "semaine à",
            "aujourd'hui", "demain", "connaître la", "connaitre la", "quelle est la météo",
            "quel temps", "prévision", "prevision",
        ]
        .iter()
        .any(|k| m.contains(k)),
        camera_or_mic: [
            "webcam", "caméra", "camera", "prend une photo", "prends une photo", "prendre une photo",
            "take a photo", "take a picture", "prends moi en photo", "photo avec la webcam",
            "accède à la webcam", "accede a la webcam", "utilise la caméra", "utilise la camera",
            "affiche-la dans le chat", "afficher dans le chat", "display in the chat", "show in the chat",
            "micro", "microphone", "enregistre avec le micro", "enregistrer avec le micro",
            "obtenir une image", "get an image", "image avec la webcam", "image avec la caméra",
            "continuer pour obtenir une image", "proceed to get an image",
        ]
        .iter()
        .any(|k| m.contains(k)),
        image_generation: [
            "génère une image", "genere une image", "générer une image", "génère moi une image",
            "generate an image", "generate a picture", "draw", "dessine", "dessiner",
            "crée une image", "cree une image", "créer une image", "create an image",
            "image par ia", "ai image", "dall-e", "dalle",
        ]
        .iter()
        .any(|k| m.contains(k)),
        code_generation: [
            "écris un script", "ecris un script", "écrire un script", "ecrire un script",
            "write a script", "write the script", "génère du code", "genere du code",
            "generate code", "génère le code", "code python", "python script",
            "un programme qui", "a program that", "fonction qui", "function that",
            "snippet", "extrait de code", "piece of code", "exemple de code",
        ]
        .iter()
        .any(|k| m.contains(k)),
        github_with_vault: {
            let github = ["github", "dépôt privé", "depot prive", "private repo", "api.github.com"].iter().any(|k| m.contains(k));
            let vault = ["vault", "github_token", "clé du vault", "cle du vault", "key in the vault", "token dans le vault", "clef dans le vault"].iter().any(|k| m.contains(k));
            github && vault
        },
    }
}

/// True if the user message suggests a tool-only action (camera, web search, save file, image gen) without asking for code generation. Used by the orchestrator to override mistaken "code" decomposition.
pub fn message_suggests_tool_only_action(message: &str) -> bool {
    let flags = compute_message_intent_flags(message);
    (flags.camera_or_mic || flags.save_file || flags.external_info || flags.image_generation)
        && !flags.code_generation
}

/// True if the user message suggests a long-running project (novel, comic, code project) or continuing one.
/// Capture limits for post-reply memory promotion (plan court terme 7, inspired by OpenClaw plugin).
const CAPTURE_MIN_CHARS: usize = 16;
const CAPTURE_MAX_CHARS: usize = 4000;
const CAPTURE_MAX_PER_TURN: usize = 20;

/// Short acknowledgments that should not be stored in long-term memory.
static CAPTURE_ACK_PATTERNS: &[&str] = &[
    "ok", "okay", "oui", "non", "merci", "thanks", "thank you", "d'accord", "daccord",
    "👍", "👌", "ok.", "parfait", "super", "cool", "noted", "compris", "c'est noté",
];

/// If the text ends with an unclosed fenced code block (odd number of ```), appends "\n```\n" so that
/// content appended after (e.g. image markdown) is not rendered inside a code block.
pub(crate) fn ensure_no_open_code_block(text: &str) -> String {
    let count = text.matches("```").count();
    if count % 2 == 1 {
        format!("{}\n```\n", text.trim_end())
    } else {
        text.to_string()
    }
}

/// Build markdown image snippet: `\n\n![label](<url>)`. Angle brackets around URL allow parens in data URLs.
pub(crate) fn build_image_markdown(label: &str, url: &str) -> String {
    format!("\n\n![{}](<{}>)", label, url)
}

/// Returns true if content should be skipped when capturing to long-term memory (noise, loop risk, or too short/long).
fn should_skip_capture_content(content: &str) -> bool {
    let t = content.trim();
    if t.is_empty() {
        return true;
    }
    if t.len() < CAPTURE_MIN_CHARS {
        return true;
    }
    if t.len() > CAPTURE_MAX_CHARS {
        return true;
    }
    // Avoid storing the injected memory block (would cause recall loops).
    if t.contains("[Mémoire à long terme]")
        || t.contains("<relevant-memories>")
        || t.contains("[Projet en cours")
    {
        return true;
    }
    let lower = t.to_lowercase();
    let lower_trim = lower.trim();
    if CAPTURE_ACK_PATTERNS.iter().any(|p| lower_trim == *p || lower_trim.starts_with(&format!("{} ", p))) {
        return true;
    }
    false
}

fn message_suggests_project(message: &str) -> bool {
    let m = message.to_lowercase();
    let keywords = [
        "roman", "bd", "bande dessinée", "bande dessinee", "comic", "novel",
        "projet de code", "code project", "écris un", "ecris un", "écris le", "ecris le",
        "chapitre", "chapter", "continue", "la suite", "and the rest", "poursuis", "reprends",
        "crée un projet", "cree un projet", "create a project", "set up a project",
    ];
    keywords.iter().any(|k| m.contains(k))
}

/// Default hosts when policy does not set allowed_skill_install_hosts (GitHub only).
const INSTALL_SKILL_DEFAULT_HOSTS: &[&str] = &["github.com", "raw.githubusercontent.com", "www.github.com"];

fn is_github_host(host: &str) -> bool {
    INSTALL_SKILL_DEFAULT_HOSTS
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{}", *h)))
}

/// Result of parsing a skill install URL: raw SKILL.md URL, skill name, and optional GitHub API path for listing contents.
struct ParsedSkillUrl {
    raw_skill_url: String,
    skill_name: String,
    /// (owner, repo, branch, path) for GitHub API only; None for other hosts (single-file install).
    api_path: Option<(String, String, String, String)>,
}

/// Parse a skill install URL (GitHub, GitLab, or any allowed HTTPS host) into raw SKILL.md URL, skill name, and optional API path.
/// allowed_hosts: from policy; if it contains "*", any HTTPS host is allowed. Otherwise only listed hosts (and subdomains) are allowed.
fn parse_skill_install_url(url: &str, allowed_hosts: &[String]) -> Option<ParsedSkillUrl> {
    let url = url.trim();
    let parsed = url.parse::<url::Url>().ok()?;
    let scheme = parsed.scheme();
    if scheme != "https" {
        return None;
    }
    let host = parsed.host_str()?.to_lowercase();
    let host_allowed = allowed_hosts.iter().any(|h| h == "*")
        || allowed_hosts.iter().any(|h| host == *h || host.ends_with(&format!(".{}", h)));
    if !host_allowed {
        return None;
    }
    let path = parsed.path().trim_matches('/');
    if path.is_empty() {
        return None;
    }
    let segments: Vec<&str> = path.split('/').collect();

    if is_github_host(&host) {
        if host.contains("raw.githubusercontent.com") {
            if segments.len() < 4 {
                return None;
            }
            let (raw_skill_url, skill_name) = if path.ends_with("SKILL.md") {
                let skill_name = segments.get(segments.len().saturating_sub(2)).copied().unwrap_or("skill").to_string();
                if !crate::user_rag::is_safe_relative_filename(&skill_name) {
                    return None;
                }
                (url.to_string(), skill_name)
            } else {
                let raw_url = format!("https://raw.githubusercontent.com/{}", path.trim_end_matches('/'));
                let raw_skill_url = if raw_url.ends_with(".md") { raw_url } else { format!("{}/SKILL.md", raw_url) };
                let skill_name = segments.last().copied().unwrap_or("skill").to_string();
                if !crate::user_rag::is_safe_relative_filename(&skill_name) {
                    return None;
                }
                (raw_skill_url, skill_name)
            };
            let api_path = if segments.len() >= 4 {
                let owner = segments[0].to_string();
                let repo = segments[1].to_string();
                let branch = segments[2].to_string();
                let path_part = segments[3..].join("/");
                let path_part = path_part.strip_suffix("SKILL.md").map(|s| s.trim_end_matches('/')).unwrap_or(&path_part).to_string();
                Some((owner, repo, branch, path_part))
            } else {
                None
            };
            Some(ParsedSkillUrl { raw_skill_url, skill_name, api_path })
        } else {
            let tree_idx = segments.iter().position(|s| *s == "tree")?;
            if tree_idx + 2 > segments.len() {
                return None;
            }
            let owner = (*segments.get(0)?).to_string();
            let repo = (*segments.get(1)?).to_string();
            let branch = (*segments.get(tree_idx + 1)?).to_string();
            let path_segments = &segments[tree_idx + 2..];
            let path_part = path_segments.join("/");
            let skill_name = path_segments.last().copied().unwrap_or("skill").to_string();
            if !crate::user_rag::is_safe_relative_filename(&skill_name) {
                return None;
            }
            let raw_skill_url = if path_part.is_empty() {
                format!("https://raw.githubusercontent.com/{}/{}/{}/SKILL.md", owner, repo, branch)
            } else {
                format!(
                    "https://raw.githubusercontent.com/{}/{}/{}/{}/SKILL.md",
                    owner, repo, branch, path_part
                )
            };
            let api_path = Some((owner, repo, branch, path_part));
            Some(ParsedSkillUrl { raw_skill_url, skill_name, api_path })
        }
    } else {
        // Generic host (site web, GitLab, etc.): single-file install. URL must point to a .md file or we use path as skill name.
        let raw_skill_url = if path.ends_with(".md") {
            url.to_string()
        } else if path.ends_with('/') {
            format!("{}SKILL.md", url.trim_end_matches('/'))
        } else {
            format!("{}/SKILL.md", url.trim_end_matches('/'))
        };
        let skill_name = segments
            .last()
            .and_then(|s| s.strip_suffix(".md"))
            .or_else(|| segments.last().copied())
            .unwrap_or("skill")
            .to_string();
        let skill_name = skill_name.to_lowercase().replace(' ', "-");
        if !crate::user_rag::is_safe_relative_filename(&skill_name) {
            return None;
        }
        Some(ParsedSkillUrl {
            raw_skill_url,
            skill_name,
            api_path: None,
        })
    }
}

/// Extract required CLI commands (bins) from SKILL.md front matter.
/// Looks for metadata.<key>.requires.bins or requires.bins (array of strings). Falls back to [skill_name] for CLI skills.
fn skill_required_bins(skill_md_content: &str, skill_name: &str) -> Vec<String> {
    let yaml_str = match skill_md_content.strip_prefix("---") {
        Some(r) => r,
        None => return vec![skill_name.to_string()],
    };
    let end = match yaml_str.find("\n---") {
        Some(i) => i,
        None => return vec![skill_name.to_string()],
    };
    let yaml_str = yaml_str[..end].trim();
    let value: serde_yaml::Value = match serde_yaml::from_str(yaml_str) {
        Ok(v) => v,
        Err(_) => return vec![skill_name.to_string()],
    };
    fn bins_from_value(v: &serde_yaml::Value) -> Option<Vec<String>> {
        let arr = v.get("bins")?.as_sequence()?;
        let list: Vec<String> = arr
            .iter()
            .filter_map(|a| a.as_str().map(String::from))
            .collect();
        if list.is_empty() {
            None
        } else {
            Some(list)
        }
    }
    if let Some(requires) = value.get("requires") {
        if let Some(bins) = bins_from_value(requires) {
            return bins;
        }
    }
    if let Some(metadata) = value.get("metadata").and_then(|m| m.as_mapping()) {
        for (_key, val) in metadata {
            if let Some(requires) = val.get("requires") {
                if let Some(bins) = bins_from_value(requires) {
                    return bins;
                }
            }
        }
    }
    vec![skill_name.to_string()]
}

/// Extract the Markdown body (instructions) from SKILL.md content (after the second ---).
fn skill_md_body(content: &str) -> &str {
    let rest = match content.strip_prefix("---") {
        Some(r) => r,
        None => return content,
    };
    let body_start = match rest.find("\n---") {
        Some(i) => 3 + 1 + i + 4, // "---" + "\n" + "---" + "\n" after second ---
        None => return content,
    };
    content.get(body_start..).unwrap_or(content).trim()
}

/// Fetch additional files from a GitHub repo path (scripts, references, etc.) and write into skill_dir. Uses a queue to avoid recursive async.
async fn fetch_github_skill_extra_files(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    branch: &str,
    root_path: &str,
    skill_dir: &Path,
) -> Vec<String> {
    let mut downloaded = Vec::new();
    let mut queue: Vec<(String, PathBuf)> = vec![(root_path.to_string(), skill_dir.to_path_buf())];
    while let Some((path, dir)) = queue.pop() {
        let url = format!(
            "https://api.github.com/repos/{}/{}/contents/{}?ref={}",
            owner, repo, path, branch
        );
        let resp = match client.get(&url).header("User-Agent", "Akasha").send().await {
            Ok(r) if r.status().is_success() => r,
            _ => continue,
        };
        let items: Vec<serde_json::Value> = match resp.json().await {
            Ok(arr) => arr,
            Err(_) => continue,
        };
        for item in items {
            let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let typ = item.get("type").and_then(|t| t.as_str()).unwrap_or("file");
            if !crate::user_rag::is_safe_relative_filename(name) {
                continue;
            }
            if typ == "file" && name != "SKILL.md" {
                let download_url = item.get("download_url").and_then(|u| u.as_str());
                if let Some(url) = download_url {
                    if let Ok(resp) = client.get(url).send().await {
                        if let Ok(content) = resp.text().await {
                            let dest = dir.join(name);
                            if std::fs::write(&dest, &content).is_ok() {
                                let rel = if path == root_path { name.to_string() } else { format!("{}/{}", path.strip_prefix(root_path).unwrap_or(path.as_str()).trim_start_matches('/'), name) };
                                downloaded.push(rel);
                            }
                        }
                    }
                }
            } else if typ == "dir" {
                let subpath = if path.is_empty() { name.to_string() } else { format!("{}/{}", path, name) };
                let subdir = dir.join(name);
                let _ = std::fs::create_dir_all(&subdir);
                queue.push((subpath, subdir));
            }
        }
    }
    downloaded
}

/// Install a skill from a URL (Agent Skills spec: https://agentskills.io/specification).
/// Supports GitHub (with directory listing), or any HTTPS host allowed in tools_policy (allowed_skill_install_hosts).
/// Fetches SKILL.md, optional scripts/references/assets (GitHub only), writes to data_dir/skills/<name>/,
/// reloads the registry, adds required commands to tools_policy.yaml, and optionally hot-reloads the in-memory policy.
async fn do_install_skill(
    url: &str,
    data_dir: &Path,
    spec_dir: &Path,
    skill_registry: &crate::skills::SkillRegistry,
    allowed_hosts: &[String],
    tools_reload: Option<(
        &std::sync::Arc<tokio::sync::RwLock<std::sync::Arc<akasha_tools::ToolExecutor>>>,
        &Path,
    )>,
) -> (bool, String) {
    let parsed = match parse_skill_install_url(url, allowed_hosts) {
        Some(x) => x,
        None => {
            let hint = if allowed_hosts.iter().any(|h| h == "*") {
                "URL invalide ou schéma non supporté (utilisez https://).".to_string()
            } else {
                format!(
                    "URL non autorisée ou invalide. Hôtes autorisés (tools_policy.yaml allowed_skill_install_hosts) : {}.",
                    allowed_hosts.join(", ")
                )
            };
            return (false, format!("[install_skill] {}", hint));
        }
    };
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("Akasha")
        .build()
    {
        Ok(c) => c,
        Err(e) => return (false, format!("[install_skill] client HTTP: {}", e)),
    };
    let body = match client.get(&parsed.raw_skill_url).send().await {
        Ok(r) if r.status().is_success() => match r.text().await {
            Ok(t) => t,
            Err(e) => return (false, format!("[install_skill] lecture réponse: {}", e)),
        },
        Ok(r) => return (false, format!("[install_skill] HTTP {} — {}", r.status(), parsed.raw_skill_url)),
        Err(e) => return (false, format!("[install_skill] requête: {}", e)),
    };
    if body.trim().is_empty() {
        return (false, format!("[install_skill] SKILL.md vide ou introuvable: {}", parsed.raw_skill_url));
    }
    let skill_dir = data_dir.join("skills").join(&parsed.skill_name);
    if let Err(e) = std::fs::create_dir_all(&skill_dir) {
        return (
            false,
            format!("[install_skill] impossible de créer le dossier {}: {}", skill_dir.display(), e),
        );
    }
    let skill_md_path = skill_dir.join("SKILL.md");
    if let Err(e) = std::fs::write(&skill_md_path, &body) {
        return (
            false,
            format!("[install_skill] écriture {}: {}", skill_md_path.display(), e),
        );
    }
    let extra_files = if let Some((ref owner, ref repo, ref branch, ref path)) = parsed.api_path {
        fetch_github_skill_extra_files(&client, owner, repo, branch, path, &skill_dir).await
    } else {
        vec![]
    };
    match skill_registry.reload(data_dir, spec_dir).await {
        Ok(count) => {
            let body_instructions = skill_md_body(&body);
            let total_chars = body_instructions.chars().count();
            let body_preview = if total_chars > 8000 {
                let truncated: String = body_instructions.chars().take(8000).collect();
                format!("{}... [tronqué, {} caractères au total]", truncated, total_chars)
            } else {
                body_instructions.to_string()
            };
            let extra_msg = if extra_files.is_empty() {
                String::new()
            } else {
                format!(" Fichiers additionnels récupérés (scripts/, references/, assets/) : {}.", extra_files.join(", "))
            };
            let (mut commands_added_msg, need_hot_reload) = {
                let bins = skill_required_bins(&body, &parsed.skill_name);
                let policy_path = data_dir.join("tools_policy.yaml");
                if let Ok(mut policy) = akasha_tools::ToolsPolicy::load_from_path(&policy_path) {
                    let added = policy.add_allowed_commands(&bins);
                    if !added.is_empty() {
                        if let Err(e) = policy.save_to_path(&policy_path) {
                            (format!(" Commandes {} non ajoutées à tools_policy.yaml (écriture: {}).", added.join(", "), e), false)
                        } else {
                            let added_joined = added.join(", ");
                            (format!(" Commande(s) ajoutée(s) à tools_policy.yaml (allowed_commands) : {}.", added_joined), true)
                        }
                    } else {
                        (String::new(), false)
                    }
                } else {
                    (String::new(), false)
                }
            };
            if need_hot_reload {
                if let Some((r, path)) = tools_reload {
                    if let Ok(mut reloaded) = akasha_tools::ToolsPolicy::load_from_path(path) {
                        if let Ok(v) = akasha_vault::open_vault(data_dir) {
                            reloaded.brave_api_key = v.get("brave_api_key").ok();
                        }
                        *r.write().await = std::sync::Arc::new(akasha_tools::ToolExecutor::new(reloaded));
                        commands_added_msg = commands_added_msg.replace(" (allowed_commands) :", " ; politique rechargée à chaud :");
                    }
                }
            }
            let skill_dir_display = skill_dir.display().to_string();
            let msg = format!(
                "[install_skill] Skill « {} » installé et rechargé ({} skill(s) chargé(s)).{}{}\n\
                 Répertoire du skill (pour read_file sur references/, scripts/, assets/) : {}\n\n\
                 Contenu du skill (à utiliser pour savoir comment l'utiliser) :\n\n---\n{}",
                parsed.skill_name, count, extra_msg, commands_added_msg, skill_dir_display, body_preview
            );
            (true, msg)
        }
        Err(e) => (
            false,
            format!("[install_skill] skill écrit mais rechargement échoué: {}", e),
        ),
    }
}

/// Uninstall a skill by name: remove data_dir/skills/<name>, remove command from tools_policy, reload registry (and optionally hot-reload executor).
async fn do_uninstall_skill(
    name: &str,
    data_dir: &Path,
    spec_dir: &Path,
    skill_registry: &crate::skills::SkillRegistry,
    tools_reload: Option<(
        &std::sync::Arc<tokio::sync::RwLock<std::sync::Arc<akasha_tools::ToolExecutor>>>,
        &Path,
    )>,
) -> (bool, String) {
    let name = name.trim();
    if name.is_empty() {
        return (false, "[uninstall_skill] usage: uninstall_skill <name> (ex. uninstall_skill bankr)".to_string());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return (
            false,
            "[uninstall_skill] nom invalide (utiliser uniquement lettres, chiffres, _ et -)".to_string(),
        );
    }
    let skill_dir = data_dir.join("skills").join(name);
    if !skill_dir.exists() {
        return (
            false,
            format!(
                "[uninstall_skill] skill « {} » introuvable dans data_dir/skills/ (dossier absent).",
                name
            ),
        );
    }
    if !skill_dir.is_dir() {
        return (
            false,
            format!("[uninstall_skill] « {} » n'est pas un répertoire.", skill_dir.display()),
        );
    }
    if let Err(e) = std::fs::remove_dir_all(&skill_dir) {
        return (
            false,
            format!("[uninstall_skill] impossible de supprimer {}: {}", skill_dir.display(), e),
        );
    }
    let policy_path = data_dir.join("tools_policy.yaml");
    let mut policy_updated = false;
    if policy_path.exists() {
        if let Ok(mut policy) = akasha_tools::ToolsPolicy::load_from_path(&policy_path) {
            if policy.remove_allowed_command(name) {
                if policy.save_to_path(&policy_path).is_ok() {
                    policy_updated = true;
                }
            }
        }
    }
    if policy_updated {
        if let Some((r, path)) = tools_reload {
            if let Ok(mut reloaded) = akasha_tools::ToolsPolicy::load_from_path(path) {
                if let Ok(v) = akasha_vault::open_vault(data_dir) {
                    reloaded.brave_api_key = v.get("brave_api_key").ok();
                }
                *r.write().await = std::sync::Arc::new(akasha_tools::ToolExecutor::new(reloaded));
            }
        }
    }
    match skill_registry.reload(data_dir, spec_dir).await {
        Ok(count) => {
            let policy_msg = if policy_updated {
                format!(" Commande « {} » retirée de tools_policy.yaml (allowed_commands).", name)
            } else {
                String::new()
            };
            (
                true,
                format!(
                    "[uninstall_skill] Skill « {} » désinstallé ({} skill(s) chargé(s) restants).{}",
                    name, count, policy_msg
                ),
            )
        }
        Err(e) => (
            false,
            format!("[uninstall_skill] dossier supprimé mais rechargement du registry échoué: {}", e),
        ),
    }
}

const WRITE_FILE_REMINDER: &str = "\n[Reminder: the user is asking to save a file. You MUST reply ONLY with the line TOOL: write_file <full_path> then the file content on the following lines. Never say you cannot write to disk.]\n\n";

const WEB_SEARCH_REMINDER: &str = "\n[Reminder: the user is asking for external information (weather/météo, news, etc.). You MUST use TOOL: web_search <query> to search — do NOT use bankr or portfolio for weather. Then reply with the results. Do not suggest visiting a site without having used web_search first.]\n\n";

const DEVICE_CAMERA_REMINDER: &str = "\n[Reminder: webcam/camera photo request. You MUST chain directly: TOOL: device_discover local_media then TOOL: device_invoke local_media camera capture. Do NOT ask the user \"which device action?\" with ask_user — they already said they want a photo; call device_invoke camera capture. Do NOT suggest: file upload, open UI, AI image. Do NOT mention tools_policy.yaml or allowed_write_paths for this request: the user wants a camera photo, not to configure file writing. If the user asked to \"display the photo in the chat\", after capture reply ONLY with a short confirmation in their language (e.g. \"Photo captured. It is shown below.\"): do NOT suggest \"save to file\", \"get a description\", \"take another photo\" or \"What would you like to do next?\" — the image is added automatically below your reply. Reply in the same language as the user.]\n\n";
const IMAGE_GENERATION_REMINDER: &str = "\n[Reminder: request to \"generate an image\", \"draw\", \"create an image\" (by AI, not webcam). You MUST use TOOL: generate_image <prompt> (e.g. TOOL: generate_image a cat on a sofa). Spec 42.]\n\n";

/// Reminder when the user asks for GitHub (private repo / API) and mentions the vault (e.g. GITHUB_TOKEN).
const GITHUB_VAULT_REMINDER: &str = "\n[Reminder GitHub + vault: you MUST run the request yourself via TOOL: run_command. Exact format: TOOL: run_command VAULT:GITHUB_TOKEN=GITHUB_TOKEN curl -sS -H \"Authorization: Bearer $GITHUB_TOKEN\" https://api.github.com/repos/owner/repo (or gh repo view owner/repo). FORBIDDEN: telling the user to do GITHUB_TOKEN=VAULT:... or export GITHUB_TOKEN=... or to put the token in plain text — you must emit the TOOL: line so the system injects the secret. Do not reply \"I did not find\" without having called run_command with VAULT:GITHUB_TOKEN=GITHUB_TOKEN.]\n\n";

/// Application context injected into the prompt: the agent knows it runs inside Akasha and can talk about it.
const APP_CONTEXT: &str = concat!(
    "[Akasha context] You are the assistant embedded in Akasha. Akasha is the application you are currently running in. ",
    "If the user talks about Akasha, the program, the app or how it works, you can explain: ",
    "commands (akasha start, akasha init, akasha doctor), interfaces (TUI with Chat/Router/Memory/Doc/Activity tabs), ",
    "slash commands in Chat (/help, /status, /doctor, /advice, /config, /models, /routes, /newsession, /skills reload, etc.). ",
    "To install a CLI globally (e.g. \"install the bankr CLI\", \"npm install -g @bankr/cli\"), reply with TOOL: run_command npm install -g <package> (do not generate a script for the user to run). ",
    "To use a vault key in a command: TOOL: run_command VAULT:bankr_api_key=BANKR_API_KEY bankr whoami (the system injects the vault value). ",
    "GitHub + vault: run TOOL: run_command VAULT:GITHUB_TOKEN=GITHUB_TOKEN curl -sS -H \"Authorization: Bearer $GITHUB_TOKEN\" https://api.github.com/repos/owner/repo (not GITHUB_TOKEN=VAULT:... or export or plain token). ",
    "Skills (extra capabilities): the user can add them without changing code. When the user asks to install, download, fetch or add a skill from a URL (e.g. \"install the bankr skill from …\", \"download the skill at this url\"), you MUST reply with TOOL: install_skill <url>. If the user says \"follow the SKILL.md instructions\", you must first do TOOL: install_skill <url> (the system registers the skill); then the skill is available and can be invoked by name (e.g. TOOL: security-audit <args>). Do not fetch SKILL.md with web_fetch to execute its content manually. ",
    "To uninstall a skill: TOOL: uninstall_skill <name> (e.g. TOOL: uninstall_skill bankr). ",
    "When the user asks you to perform an action with a skill (e.g. \"check my bankr wallet\", \"run bankr whoami\"), you MUST reply ONLY with one line TOOL: <skill_name> <arguments> (e.g. TOOL: bankr whoami) so the system runs the command; do not tell the user to run the command themselves. ",
    "Otherwise the user can place files in the skills folder and run /skills reload. ",
    "Full documentation is available in the Doc tab of the interface. ",
    "Language: ALWAYS reply in the same language as the user's last message (French → French, English → English, etc.). Do not switch language even if tool results or context are in another language. ",
    "Never invent data. If you do not have the information to answer, say so clearly (e.g. \"I did not find that information\"). ",
    "For questions about information you do not have (weather, forecasts, news, schedules, etc.), you must use the web_search tool to search yourself then reply with the results. When you have just received tool results (e.g. web_search, web_fetch), you must answer immediately with the synthesized result — do not reply with a promise (e.g. \"I will fetch…\", \"Action in progress\"); the task ends after your message, so give the actual answer. ",
    "Do not suggest the user visit a site without having used web_search first if you have access to that tool. ",
    "If web_search returns an error (e.g. not enabled), you can then suggest sites and explain how to enable web search (tools_policy.yaml, web_search_enabled, BRAVE_API_KEY). ",
    "You have access to the write_file tool: you MUST use it whenever the user asks to save, store or write a file (e.g. \"save the code to …\", \"write to file\"). ",
    "Reply ONLY with one line TOOL: write_file <full_path> then the file content on the following lines. ",
    "Never say \"I cannot write to disk\" or \"copy-paste the code yourself\" — if the path is denied by policy, the tool will return an error and you then explain how to add the prefix in tools_policy.yaml (allowed_write_paths). Paths can be Windows (C:\\Users\\...) or Unix. ",
    "Important rule: whenever you need to ask the user for a choice, confirmation or information (options to choose, path, credentials, etc.) and then continue in the same task, you MUST use the ask_user tool (TOOL: ask_user then JSON with question/context/choices). ",
    "Do not ask the question in free text, or the reply will open a new task and you will not be able to continue. ",
    "For access to an external service (GitHub, API, etc.), do not reply \"I cannot\"; use ask_user to ask for the token or explain how to configure. ",
    "If the user has already confirmed (e.g. \"key in the vault\", \"it's configured\"), do not send ask_user again; continue. ",
    "You have access to the camera and microphone via device_discover and device_invoke (local_media interface). When the user asks for a photo with the camera/webcam / \"take a photo\" / \"display in the chat\", you MUST chain: TOOL: device_discover local_media then TOOL: device_invoke local_media camera capture, without asking with ask_user. After capture, if the user asked to display the photo in the chat, reply ONLY with a short confirmation in their language (e.g. \"Photo captured. It is shown below.\"); do NOT suggest \"save to file\", \"scene description\", \"take another photo\" or \"What would you like to do next?\" — the image is added automatically below your reply. Always reply in the user's language. Never mention tools_policy.yaml for a simple webcam photo request. ",
    "Do not invent commands (e.g. /status repo:... does not exist); commands are in /help.\n\n",
);

/// Returns an English [Role] system prompt for the given agent type, or None for conversation/unknown.
/// Plan: Architecture agents et pipeline — Phase 2 (new roles).
pub fn agent_role_system_prompt(agent_type: &str) -> Option<&'static str> {
    match agent_type {
        "conversation" => None,
        "code" => Some("You are the code generation agent. Produce correct, readable code. Prefer run_command or write_file when the user asks to create or run code. Do not invent APIs; use read_file when needed to match existing code. When the user asks to *perform* an action (take a photo, run a command, search the web, save a file), use the appropriate TOOL; do not generate a script. Use code only when the user explicitly asks to *write* or *generate* code or a script."),
        "search" => Some("You are the search agent. Use web_search to find external information (weather, news, facts). Synthesize results and cite sources. Do not claim information you have not retrieved via web_search when it is available."),
        "financial" => Some("You are the financial specialist. Help with budgets, cost analysis, financial reports, numeric reasoning. Be precise with figures and units. Do not invent data; state what is missing if needed."),
        "documentalist" => Some("You are the documentalist. Transform a pile of files into exploitable data. Answer from the user's document base (RAG). Prioritize [User documents] and [Long-term memory]. Use memory_search when relevant. Quote or summarize from excerpts; if insufficient, say so and suggest adding documents. Produce structured summaries when asked."),
        "project_manager" => Some("You are the project manager. Help with project tracking, milestones, task breakdown, planning. Refer to schedules and recurring tasks when relevant. Propose clear next steps and deliverables."),
        "technical_writer" => Some("You are the technical writing agent. Produce clear technical documentation, procedures, tutorials. Use a structured style (headings, steps, code blocks when relevant). Prefer clarity and precision. Use write_file when the user asks to save documentation."),
        "research" => Some("You are the research agent. Perform in-depth research using web_search, memory_search, and the document base. Synthesize multiple sources; cite or summarize clearly. Do not invent facts."),
        "security_audit" => Some("You are the security audit agent. Review code, config, or practices for security. Be methodical; highlight risks and suggest mitigations. Do not claim certainty where you lack context; recommend human review for critical decisions."),
        "creative" => Some("You are the creative agent. You have a strong creative sense for text and images. Produce marketing copy, creative content, stories, and audience-adapted text. Match tone and format to the requested channel and goal. When the task is to get a photo from the user's webcam/camera, use TOOL: device_invoke local_media camera capture first; for AI-generated images use generate_image."),
        "analyst" => Some("You are the product / functional analyst. Formalize the need before any production. Output: reformulated need, scope, assumptions, acceptance criteria, initial backlog. Do not jump to implementation; clarify and structure the request first."),
        "architect" => Some("You are the technical architect. Design the skeleton of the project. Output: proposed architecture, task list, dependencies between tasks, execution order, definition of done. Stay at design level; do not write full implementation."),
        "frontend" => Some("You are the frontend agent. Produce UI components, views, and client-side logic. Focus on UX, accessibility, responsive layout, and integration with the design system. List impacted components. Do not modify database schema unless explicitly asked."),
        "backend" => Some("You are the backend agent. Produce server-side logic, APIs, and business rules. Focus on correctness, performance, and clear contracts. Do not change frontend or DB schema unless the task explicitly requires it."),
        "database" => Some("You are the database / data agent. Produce schemas, migrations, queries, and data pipelines. Focus on consistency, indexing, and data integrity. Output clear DDL or migration steps when applicable."),
        "integration" => Some("You are the integration agent. Wire components together: APIs, events, external services. Focus on contracts, error handling, and end-to-end flows. Produce a precise deliverable (config, glue code, or runbook)."),
        "qa" => Some("You are the quality control agent. You prevent false 'work done'. Verify coherence, requirement coverage, missing files, hidden TODOs, incomplete sections. Do not rewrite; report defects and gaps by severity. Do not validate if acceptance criteria are incomplete; output a clear report for rework."),
        "system" => Some("You are the system agent. You have full knowledge of the Akasha application: commands (akasha start, init, doctor), interfaces (TUI, Chat, Router, Memory, Doc, Calendar), slash commands, skills, tools, and configuration. You can resolve issues and answer any question about how Akasha works. Be precise and refer to real features only."),
        "image_generation" => Some("You are the image generation agent. Produce images from text prompts using the generate_image tool. Focus on clear, concrete prompts that yield the requested visual. One precise deliverable per request."),
        _ => None,
    }
}

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

/// Split by whitespace but keep double-quoted segments as a single token (e.g. -H "Authorization: Bearer $X" -> [-H, "Authorization: Bearer $X"]).
fn split_whitespace_respecting_quotes(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        if rest.starts_with('"') {
            let mut end = 1usize;
            while end < rest.len() {
                let b = rest.as_bytes()[end];
                if b == b'\\' && end + 1 < rest.len() {
                    end += 2;
                    continue;
                }
                if b == b'"' {
                    out.push(rest[1..end].replace("\\\"", "\""));
                    rest = &rest[end + 1..];
                    break;
                }
                end += 1;
            }
            if end >= rest.len() {
                // Unclosed quote: slice only if rest has content after the opening quote (avoid rest[1..] when len==1)
                let quoted = if rest.len() > 1 { &rest[1..] } else { "" };
                out.push(quoted.replace("\\\"", "\""));
                rest = "";
            }
        } else {
            let next_quote = rest.find('"').unwrap_or(rest.len());
            let word_end = rest[..next_quote]
                .find(|c: char| c.is_whitespace())
                .unwrap_or(rest.len());
            let word = rest[..word_end].trim();
            if !word.is_empty() {
                out.push(word.to_string());
            }
            rest = &rest[word_end..];
        }
    }
    out
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
            // Split by whitespace, respecting double-quoted args (so run_command -H "Bearer $VAR" works)
            let parts: Vec<String> = split_whitespace_respecting_quotes(rest);
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

/// Parse run_command args: leading VAULT:vault_key=ENV_VAR entries are extracted;
/// the first non-VAULT arg is the command, the rest are command arguments.
/// Returns (vault_specs: (vault_key, env_var), command, cmd_args).
fn parse_run_command_args(args: &[String]) -> (Vec<(String, String)>, String, Vec<String>) {
    let mut vault_specs = Vec::new();
    let mut rest = Vec::new();
    for arg in args {
        if let Some(s) = arg.strip_prefix("VAULT:") {
            if let Some((vault_key, env_var)) = s.split_once('=') {
                vault_specs.push((vault_key.trim().to_string(), env_var.trim().to_string()));
            }
            continue;
        }
        rest.push(arg.clone());
    }
    let (command, cmd_args) = rest
        .split_first()
        .map(|(c, a)| (c.clone(), a.to_vec()))
        .unwrap_or_else(|| (String::new(), Vec::new()));
    (vault_specs, command, cmd_args)
}

/// Parse `device_invoke` params from the tail of the args list (args[3..]).
/// - No extra args → `{}`
/// - Single arg that is valid JSON → that JSON value
/// - Single arg that is not valid JSON → `{}`
/// - Multiple args → JSON array of strings
fn parse_device_invoke_params(args: &[String]) -> serde_json::Value {
    if args.len() <= 3 {
        serde_json::json!({})
    } else if args.len() == 4 {
        serde_json::from_str::<serde_json::Value>(&args[3])
            .unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!(args[3..].to_vec())
    }
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
    device_bridge: Option<&std::sync::Arc<crate::device_bridge::DeviceBridge>>,
    workspace_store: Option<&TaskWorkspaceStore>,
) -> (bool, String, Option<String>) {
    use std::path::Path;
    if !executor.policy.can_use_tool(tool_name) {
        return (false, format!("[{}] tool not allowed by current profile", tool_name), None);
    }
    let path_arg = |i: usize| args.get(i).map(|s| Path::new(s.as_str()));
    let result = match tool_name {
        "read_file" => {
            if let Some(path_str) = args.get(0) {
                let path_str = path_str.as_str();
                if path_str.starts_with("workspace:/") || path_str.starts_with("workspace:") {
                    match workspace_store {
                        Some(ws) => {
                            let key = path_str
                                .trim_start_matches("workspace:/")
                                .trim_start_matches("workspace:")
                                .trim_start_matches('/')
                                .to_string();
                            let guard = ws.read().await;
                            if let Some(map) = guard.get(&task_id) {
                                if let Some(content) = map.get(&key) {
                                    let preview = if content.len() <= 500 { content.as_str() } else { &content[..content.floor_char_boundary(500)] };
                                    return (true, format!("[read_file workspace:{}] {} chars: {}", key, content.len(), preview), None);
                                }
                            }
                            return (false, format!("[read_file workspace] path not found: {}", key), None);
                        }
                        None => return (false, "[read_file] workspace paths require a workspace store.".to_string(), None),
                    }
                }
            }
            if let Some(p) = path_arg(0) {
                match executor.read_file(p).await {
                    Ok((content, res)) => {
                        let msg = if res.success {
                            format!("[read_file {}] {} chars: {}", p.display(), content.len(), if content.len() <= 500 { content.as_str() } else { &content[..content.floor_char_boundary(500)] })
                        } else {
                            format!("[read_file] denied or error: {}", res.summary)
                        };
                        (res.success, msg, None)
                    }
                    Err(e) => (false, format!("[read_file] error: {}", e), None),
                }
            } else {
                (false, "[read_file] usage: read_file <path>".to_string(), None)
            }
        }
        "run_command" => {
            let (vault_specs, cmd, cmd_args) = parse_run_command_args(args);
            let extra_env = if vault_specs.is_empty() {
                None
            } else {
                let data_dir = store_path.and_then(|p| p.parent());
                match data_dir.and_then(|d| akasha_vault::open_vault(d).ok()) {
                    Some(vault) => {
                        let mut env = Vec::new();
                        for (vault_key, env_var) in &vault_specs {
                            let value = vault.get(vault_key).or_else(|_| {
                                // Fallback: common keys may be stored with different casing (e.g. github_token vs GITHUB_TOKEN)
                                if vault_key.eq_ignore_ascii_case("GITHUB_TOKEN") && vault_key != "github_token" {
                                    vault.get("github_token")
                                } else if vault_key == "github_token" {
                                    vault.get("GITHUB_TOKEN")
                                } else {
                                    Err(akasha_vault::VaultError::NotFound(vault_key.to_string()))
                                }
                            });
                            match value {
                                Ok(v) => env.push((env_var.clone(), v)),
                                Err(_) => {
                                    return (
                                        false,
                                        format!("[run_command] vault key not found: {}", vault_key),
                                        None,
                                    );
                                }
                            }
                        }
                        Some(env)
                    }
                    None => {
                        return (
                            false,
                            "[run_command] vault not available (no store_path or open failed)".to_string(),
                            None,
                        );
                    }
                }
            };
            let env_ref = extra_env.as_deref();
            match executor.run_command(&cmd, &cmd_args, None, env_ref).await {
                Ok((out, res)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let msg = if res.success {
                        format!("[run_command {}] stdout: {} stderr: {}", cmd, stdout.trim(), stderr.trim())
                    } else {
                        format!("[run_command] {} stderr: {}", res.summary, stderr.trim())
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[run_command] error: {}", e), None),
            }
        }
        "run_terminal" => {
            let (_vault_specs, cmd, cmd_args) = parse_run_command_args(args);
            match executor.run_command(&cmd, &cmd_args, None, None).await {
                Ok((out, res)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let msg = if res.success {
                        format!("[run_terminal {}] stdout: {} stderr: {}", cmd, stdout.trim(), stderr.trim())
                    } else {
                        format!("[run_terminal] {} stderr: {}", res.summary, stderr.trim())
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[run_terminal] error: {}", e), None),
            }
        }
        "run_command_background" => {
            let (_vault_specs, cmd, cmd_args) = parse_run_command_args(args);
            let cmd_display = format!("{} {}", cmd, cmd_args.join(" "));
            match process_registry {
                Some(reg) => {
                    let session_id = Uuid::new_v4();
                    let exec = executor.clone();
                    let cell: BackgroundResultCell = Arc::new(RwLock::new(None));
                    let cell_clone = cell.clone();
                    let reg_clone = reg.clone();
                    let task = tokio::spawn(async move {
                        let result = exec.run_command(&cmd, &cmd_args, None, None).await;
                        *cell_clone.write().await = Some(result);
                        // Auto-cleanup after a TTL to prevent leaking sessions the client never polls.
                        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                        reg_clone.write().await.remove(&session_id);
                    });
                    reg.write().await.insert(session_id, (task, cell));
                    (true, format!("[run_command_background] session_id: {} (cmd: {})", session_id, cmd_display), None)
                }
                None => (false, "[run_command_background] process registry not available".to_string(), None),
            }
        }
        "terminal_session" => (true, "[terminal_session] Interactive PTY session planned (spec 43). Use run_command or run_terminal for a single command; run_command_background + process for background execution.".to_string(), None),
        "process" => {
            let sub = args.get(0).map(String::as_str).unwrap_or("");
            match (process_registry, sub) {
                (Some(reg), "list") => {
                    let ids: Vec<String> = reg.read().await.keys().map(|u| u.to_string()).collect();
                    (true, format!("[process list] {} session(s): {:?}", ids.len(), ids), None)
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
                                return (false, format!("[process poll] unknown session_id: {}", id), None);
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
                                        (true, base_msg, None)
                                    } else {
                                        (false, format!("{} | summary: {}", base_msg, res.summary), None)
                                    }
                                }
                                Some(Err(e)) => {
                                    reg.write().await.remove(&id);
                                    (false, format!("[process poll {}] error: {}", id, e), None)
                                }
                                None => (true, format!("[process poll {}] still running", id), None),
                            }
                        }
                        None => (false, "[process poll] usage: process poll <session_id>".to_string(), None),
                    }
                }
                (Some(reg), "kill") => {
                    let session_id = args.get(1).and_then(|s| Uuid::parse_str(s).ok());
                    match session_id {
                        Some(id) => {
                            let mut g = reg.write().await;
                            if let Some((task, _cell)) = g.remove(&id) {
                                task.abort();
                                (true, format!("[process kill {}] aborted", id), None)
                            } else {
                                (false, format!("[process kill] unknown session_id: {}", id), None)
                            }
                        }
                        None => (false, "[process kill] usage: process kill <session_id>".to_string(), None),
                    }
                }
                (_, _) => (false, "[process] usage: process list | process poll <session_id> | process kill <session_id>".to_string(), None),
            }
        }
        "memory_search" => {
            let query_str = args.get(0).map(|a| a.as_str()).unwrap_or("").trim();
            let top_k = args.get(1).and_then(|s| s.parse::<usize>().ok()).unwrap_or(5).min(20);
            if query_str.is_empty() {
                return (false, "[memory_search] usage: memory_search <query> [top_k]".to_string(), None);
            }
            match long_term_client {
                Some(client) => {
                    let client = client.clone();
                    let query = query_str.to_string();
                    let results = tokio::task::spawn_blocking(move || client.search(query, top_k, None))
                        .await
                        .ok()
                        .unwrap_or_default();
                    if results.is_empty() {
                        (true, format!("[memory_search] no results for \"{}\"", query_str), None)
                    } else {
                        let preview: Vec<String> = results.iter().take(5).map(|(id, content)| format!("id: {} — {}", id, content.replace('\n', " "))).collect();
                        (true, format!("[memory_search] {} result(s): {}", results.len(), preview.join(" | ")), None)
                    }
                }
                None => (false, "[memory_search] long-term memory not available".to_string(), None),
            }
        }
        "memory_store" => {
            let content = args.get(0).map(|a| a.as_str()).unwrap_or("");
            let source = args.get(1).map(|a| a.as_str()).unwrap_or("agent");
            if content.is_empty() {
                return (false, "[memory_store] usage: memory_store <content> <source> [link_to: id1,id2...] [link_kind: spouse|child|birth_date|residence|...]".to_string(), None);
            }
            let mut link_to_ids: Option<Vec<String>> = None;
            let mut link_kind: Option<String> = None;
            for arg in args.get(2..).unwrap_or(&[]).iter().map(|a| a.as_str()) {
                if let Some(rest) = arg.strip_prefix("link_to:") {
                    let ids: Vec<String> = rest
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect();
                    if !ids.is_empty() {
                        link_to_ids = Some(ids);
                    }
                } else if let Some(rest) = arg.strip_prefix("link_kind:") {
                    let k = rest.trim();
                    if !k.is_empty() {
                        link_kind = Some(k.to_string());
                    }
                }
            }
            match long_term_client {
                Some(client) => {
                    let client = client.clone();
                    let content = content.to_string();
                    let source = source.to_string();
                    let out = tokio::task::spawn_blocking(move || client.promote(content, source, None, None, None, None, None, None, link_to_ids, link_kind))
                        .await
                        .ok()
                        .and_then(|r| r.ok());
                    match out {
                        Some(()) => (true, "[memory_store] stored".to_string(), None),
                        None => (false, "[memory_store] failed or memory not available".to_string(), None),
                    }
                }
                None => (false, "[memory_store] long-term memory not available".to_string(), None),
            }
        }
        "memory_delete" => {
            let id = args.get(0).map(|a| a.as_str()).unwrap_or("").trim();
            if id.is_empty() {
                return (false, "[memory_delete] usage: memory_delete <id> (UUID de l'entrée)".to_string(), None);
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
                        Some(()) => (true, "[memory_delete] deleted".to_string(), None),
                        None => (false, "[memory_delete] failed or not found (vérifiez l'id)".to_string(), None),
                    }
                }
                None => (false, "[memory_delete] long-term memory not available".to_string(), None),
            }
        }
        "memory_forget" => {
            let query = args.get(0).map(|a| a.as_str()).unwrap_or("").trim();
            if query.is_empty() {
                return (false, "[memory_forget] usage: memory_forget <query> (mots-clés)".to_string(), None);
            }
            match long_term_client {
                Some(client) => {
                    let client = client.clone();
                    let q = query.to_string();
                    let out = tokio::task::spawn_blocking(move || client.forget_by_query(q)).await.ok().and_then(|r| r.ok());
                    match out {
                        Some(n) => (true, format!("[memory_forget] {} entrée(s) supprimée(s)", n), None),
                        None => (false, "[memory_forget] failed or long-term memory not available".to_string(), None),
                    }
                }
                None => (false, "[memory_forget] long-term memory not available".to_string(), None),
            }
        }
        "memory_stats" => {
            match long_term_client {
                Some(client) => {
                    let client = client.clone();
                    let out = tokio::task::spawn_blocking(move || client.stats()).await.ok().and_then(|r| r.ok());
                    match out {
                        Some((count, size)) => (true, format!("[memory_stats] {} entrée(s), ~{} octets", count, size), None),
                        None => (false, "[memory_stats] failed or long-term memory not available".to_string(), None),
                    }
                }
                None => (false, "[memory_stats] long-term memory not available".to_string(), None),
            }
        }
        "memory_gc" => {
            let retention_days = args.get(0).and_then(|s| s.parse::<u32>().ok()).unwrap_or(90);
            let protect_sources: Vec<String> = args.iter().skip(1).map(|a| a.as_str().trim().to_string()).filter(|s| !s.is_empty()).collect();
            let protect = if protect_sources.is_empty() { None } else { Some(protect_sources) };
            match long_term_client {
                Some(client) => {
                    let client = client.clone();
                    let out = tokio::task::spawn_blocking(move || client.gc(retention_days, protect)).await.ok().and_then(|r| r.ok());
                    match out {
                        Some(n) => (true, format!("[memory_gc] {} entrée(s) supprimée(s) (rétention {} j)", n, retention_days), None),
                        None => (false, "[memory_gc] failed or long-term memory not available".to_string(), None),
                    }
                }
                None => (false, "[memory_gc] long-term memory not available".to_string(), None),
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
                            (true, format!("[sessions_list] {} task(s): {}", list.len(), list.join(" ; ")), None)
                        }
                        Err(e) => (false, format!("[sessions_list] error: {}", e), None),
                    },
                    Err(e) => (false, format!("[sessions_list] store error: {}", e), None),
                },
                None => (false, "[sessions_list] store not available".to_string(), None),
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
                        ), None),
                        Ok(None) => (false, format!("[session_status] task {} not found", id), None),
                        Err(e) => (false, format!("[session_status] error: {}", e), None),
                    },
                    Err(e) => (false, format!("[session_status] store error: {}", e), None),
                },
                (_, _) => (false, "[session_status] usage: session_status <task_id>".to_string(), None),
            }
        }
        "sessions_spawn" => {
            let message = args.get(0).map(|a| a.as_str()).unwrap_or("").to_string();
            let child_session_id = args.get(1).map(|a| a.as_str()).unwrap_or("").to_string();
            if message.is_empty() {
                return (false, "[sessions_spawn] usage: sessions_spawn <message> [session_id]".to_string(), None);
            }
            match (store_path, conv_tx) {
                (Some(path), Some(tx)) => {
                    let new_id = Uuid::new_v4();
                    let now = chrono::Utc::now();
                    const MAX_MSG: usize = 500;
                    let initial_message = if message.len() > MAX_MSG {
                        Some(message.chars().take(MAX_MSG).chain(std::iter::once('…')).collect::<String>())
                    } else if message.is_empty() {
                        None
                    } else {
                        Some(message.clone())
                    };
                    let task = Task {
                        id: new_id,
                        parent_task_id: Some(task_id),
                        status: TaskStatus::Pending,
                        assigned_agent: "conversation".to_string(),
                        created_at: now,
                        updated_at: now,
                        initial_message,
                    };
                    match TaskStore::open(path) {
                        Ok(store) => {
                            if store.insert(&task).is_err() {
                                return (false, "[sessions_spawn] failed to insert task".to_string(), None);
                            }
                            let sid = if child_session_id.is_empty() {
                                new_id.to_string()
                            } else {
                                child_session_id
                            };
                            if tx
                                .send(OrchestratorTask {
                                    task_id: new_id,
                                    message,
                                    session_id: sid,
                                    image_data_urls: None,
                                    execution_mode: None,
                                })
                                .await
                                .is_err()
                            {
                                return (false, "[sessions_spawn] failed to send to conversation queue".to_string(), None);
                            }
                            (true, format!("[sessions_spawn] task_id: {} (queued)", new_id), None)
                        }
                        Err(e) => (false, format!("[sessions_spawn] store error: {}", e), None),
                    }
                }
                (_, _) => (false, "[sessions_spawn] store or conversation channel not available".to_string(), None),
            }
        }
        "message" => {
            let sub = args.get(0).map(String::as_str).unwrap_or("");
            if sub != "send" || args.len() < 3 {
                return (false, "[message] usage: message send <channel> <text>".to_string(), None);
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
                        Err(e) => return (false, format!("[message] client error: {}", e), None),
                    };
                    match client.post(url).json(&body).send().await {
                        Ok(res) if res.status().is_success() => (true, "[message] sent".to_string(), None),
                        Ok(res) => (false, format!("[message] send failed: {}", res.status()), None),
                        Err(e) => (false, format!("[message] error: {}", e), None),
                    }
                }
                None => (false, "[message] AKASHA_MESSAGE_WEBHOOK_URL not set".to_string(), None),
            }
        }
        "browser" => {
            let sub = args.get(0).map(String::as_str).unwrap_or("").trim();
            if sub == "screenshot" {
                (true, "[browser] For screenshot use: TOOL: device_invoke synthetic_input keyboard shortcut (e.g. Win+Shift+S on Windows, Cmd+Shift+4 on macOS) then paste or share the image.".to_string(), None)
            } else if sub == "navigate" && args.get(1).map(|s| s.starts_with("http")).unwrap_or(false) {
                (true, "[browser] Full browser automation (navigate) not implemented. Use web_fetch <url> to get page content.".to_string(), None)
            } else {
                (false, "[browser] usage: browser navigate <url> | browser screenshot | browser snapshot. Screenshot: use device_invoke synthetic_input keyboard shortcut.".to_string(), None)
            }
        }
        "image" => {
            let path_or_url = args.get(0).map(String::as_str).unwrap_or("").trim();
            if path_or_url.is_empty() {
                (false, "[image] usage: image <path|url> [prompt]. For vision analysis attach the image in chat (vision-capable model in llm_router) or use a local path.".to_string(), None)
            } else {
                (true, "[image] Vision analysis: use image as attachment in chat with a vision-capable model (llm_router). Local path metadata not yet implemented (spec 33).".to_string(), None)
            }
        }
        "pdf" => {
            let p = path_arg(0);
            match p {
                Some(path) if executor.policy.can_read(path) => {
                    match tokio::fs::read(path).await {
                        Ok(bytes) => match pdf_extract::extract_text_from_mem(&bytes) {
                            Ok(text) => {
                                let preview = if text.len() > 2000 { format!("{}…", text.chars().take(2000).collect::<String>()) } else { text.clone() };
                                (true, format!("[pdf {}] extracted {} chars:\n{}", path.display(), text.len(), preview), None)
                            }
                            Err(e) => (false, format!("[pdf] extraction failed: {}", e), None),
                        },
                        Err(e) => (false, format!("[pdf] read failed: {}", e), None),
                    }
                }
                Some(_) => (false, "[pdf] path not allowed by policy (allowed_read_paths)".to_string(), None),
                None => (false, "[pdf] usage: pdf <path> — path must be in allowed_read_paths".to_string(), None),
            }
        }
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
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[search_files] error: {}", e), None),
            }
        }
        "grep_content" => {
            let dir = path_arg(0).unwrap_or(Path::new("."));
            let pattern = args.get(1).map(String::as_str).unwrap_or("");
            let file_glob = args.get(2).map(String::as_str).filter(|s| !s.is_empty());
            if pattern.is_empty() {
                return (false, "[grep_content] usage: grep_content <dir> <pattern> [file_glob]".to_string(), None);
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
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[grep_content] error: {}", e), None),
            }
        }
        "write_file" => {
            let path_str_opt = args.get(0).map(String::as_str);
            if let Some(path_str) = path_str_opt {
                if path_str.starts_with("workspace:/") || path_str.starts_with("workspace:") {
                    match workspace_store {
                        Some(ws) => {
                            let key = path_str
                                .trim_start_matches("workspace:/")
                                .trim_start_matches("workspace:")
                                .trim_start_matches('/')
                                .to_string();
                            let content = args.get(1..).map(|a| a.join(" ")).unwrap_or_default();
                            let mut guard = ws.write().await;
                            let per_task = guard.entry(task_id).or_default();
                            per_task.insert(key.clone(), content);
                            return (true, format!("[write_file workspace:{}] saved.", key), None);
                        }
                        None => return (false, "[write_file] workspace paths require a workspace store.".to_string(), None),
                    }
                }
            }
            let path = match path_arg(0) {
                Some(p) => p,
                None => return (false, "[write_file] usage: write_file <path> <content>".to_string(), None),
            };
            let content = args.get(1..).map(|a| a.join(" ")).unwrap_or_default();
            match executor.write_file(path, &content).await {
                Ok(res) => {
                    let msg = if res.success {
                        format!("[write_file {}] {}", path.display(), res.summary)
                    } else {
                        format!("[write_file] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[write_file] error: {}", e), None),
            }
        }
        "search_replace" => {
            let path = path_arg(0);
            let rest = args.get(1..).map(|a| a.join(" ")).unwrap_or_default();
            let Some((search, replace)) = rest
                .split_once('|')
                .map(|(s, r)| (s.trim().to_string(), r.trim().to_string()))
            else {
                return (false, "[search_replace] usage: search_replace <path> <search> | <replace>".to_string(), None);
            };
            match path {
                Some(p) => match executor.search_replace(p, &search, &replace).await {
                    Ok(res) => {
                        let msg = if res.success {
                            format!("[search_replace {}] {}", p.display(), res.summary)
                        } else {
                            format!("[search_replace] {}", res.summary)
                        };
                        (res.success, msg, None)
                    }
                    Err(e) => (false, format!("[search_replace] error: {}", e), None),
                },
                None => (false, "[search_replace] usage: search_replace <path> <search> | <replace>".to_string(), None),
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
                        (res.success, msg, None)
                    }
                    Err(e) => (false, format!("[edit_file] error: {}", e), None),
                },
                None => (false, "[edit_file] usage: edit_file <path> <start_line> <end_line> <new_content>".to_string(), None),
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
                        (res.success, msg, None)
                    }
                    Err(e) => (false, format!("[apply_patch] error: {}", e), None),
                },
                None => (false, "[apply_patch] usage: apply_patch <path> <patch_content>".to_string(), None),
            }
        }
        "file_diff" => {
            let path_a = path_arg(0);
            let path_b = path_arg(1);
            match (path_a, path_b) {
                (Some(a), Some(b)) => match executor.file_diff(a, b).await {
                    Ok((diff, res)) => {
                        let msg = if res.success {
                            let preview = if diff.len() <= 400 { diff.as_str() } else { &diff[..diff.floor_char_boundary(400)] };
                            format!("[file_diff] {} — {}", res.summary, preview)
                        } else {
                            format!("[file_diff] {}", res.summary)
                        };
                        (res.success, msg, None)
                    }
                    Err(e) => (false, format!("[file_diff] error: {}", e), None),
                },
                _ => (false, "[file_diff] usage: file_diff <path_a> <path_b>".to_string(), None),
            }
        }
        "web_fetch" => {
            let url = args.get(0).map(String::as_str).unwrap_or("");
            if url.is_empty() {
                return (false, "[web_fetch] usage: web_fetch <url>".to_string(), None);
            }
            match executor.web_fetch(url).await {
                Ok((body, res)) => {
                    let msg = if res.success {
                        let preview = if body.len() <= 500 { body.as_str() } else { &body[..body.floor_char_boundary(500)] };
                        format!("[web_fetch] {} — {}", res.summary, preview)
                    } else {
                        format!("[web_fetch] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[web_fetch] error: {}", e), None),
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
                return (false, "[web_search] usage: web_search <query> [max_results]".to_string(), None);
            }
            match executor.web_search(query.trim(), max_results).await {
                Ok((body, res)) => {
                    let msg = if res.success {
                        let preview = if body.len() <= 600 { body.as_str() } else { &body[..body.floor_char_boundary(600)] };
                        format!("[web_search] {} — {}", res.summary, preview)
                    } else {
                        format!("[web_search] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[web_search] error: {}", e), None),
            }
        }
        "run_in_container" => {
            let work_dir = path_arg(0);
            let image = args.get(1).map(String::as_str).unwrap_or("");
            let command = args.get(2).map(String::as_str).unwrap_or("");
            if work_dir.is_none() || image.is_empty() || command.is_empty() {
                return (false, "[run_in_container] usage: run_in_container <work_dir> <image> <command> [args...]".to_string(), None);
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
                        if out.len() > 400 { format!("{}...", &out[..out.floor_char_boundary(400)]) } else { out.to_string() },
                        if err.len() > 200 { format!("{}...", &err[..err.floor_char_boundary(200)]) } else { err.to_string() }
                    ), None)
                }
                Err(e) => (false, format!("[run_in_container] error: {}", e), None),
            }
        }
        "device_discover" => {
            let interface = args.get(0).map(String::as_str).unwrap_or("").trim();
            let interfaces_to_list: Vec<String> = if interface.is_empty() {
                let allowed = &executor.policy.allowed_device_interfaces;
                let base: Vec<String> = if allowed.iter().any(|a| a.trim().eq_ignore_ascii_case("*")) {
                    vec!["local_media".to_string(), "system".to_string(), "synthetic_input".to_string()]
                } else {
                    allowed.clone()
                };
                // Always apply policy filter (respects blocked_device_interfaces)
                base.into_iter()
                    .filter(|iface| executor.policy.can_use_device_interface(iface))
                    .collect()
            } else {
                if !executor.policy.can_use_device_interface(interface) {
                    return (false, format!("[device_discover] interface '{}' not allowed by policy (allowed_device_interfaces / blocked_device_interfaces)", interface), None);
                }
                vec![interface.to_string()]
            };
            let mut devices: Vec<serde_json::Value> = Vec::new();
            for iface in &interfaces_to_list {
                match iface.as_str() {
                    "local_media" => {
                        devices.push(serde_json::json!({ "interface": "local_media", "id": "camera", "name": "Camera" }));
                        devices.push(serde_json::json!({ "interface": "local_media", "id": "microphone", "name": "Microphone" }));
                        devices.push(serde_json::json!({ "interface": "local_media", "id": "speaker", "name": "Speaker" }));
                    }
                    "system" => {
                        devices.push(serde_json::json!({ "interface": "system", "id": "printer", "name": "System printers" }));
                    }
                    "synthetic_input" => {
                        devices.push(serde_json::json!({ "interface": "synthetic_input", "id": "keyboard", "name": "Keyboard (shortcuts, type)" }));
                        devices.push(serde_json::json!({ "interface": "synthetic_input", "id": "mouse", "name": "Mouse (move, click, scroll, drag)" }));
                    }
                    _ => {
                        devices.push(serde_json::json!({ "interface": iface, "id": "default", "name": format!("{} (discovery stub)", iface) }));
                    }
                }
            }
            let body = serde_json::json!({ "devices": devices });
            (true, format!("[device_discover] {} device(s): {}", devices.len(), body.to_string()), None)
        }
        "device_invoke" => {
            let interface = args.get(0).map(String::as_str).unwrap_or("");
            let device_id = args.get(1).map(String::as_str).unwrap_or("");
            let action = args.get(2).map(String::as_str).unwrap_or("");
            if interface.is_empty() || device_id.is_empty() || action.is_empty() {
                return (false, "[device_invoke] usage: device_invoke <interface> <device_id> <action> [params...]".to_string(), None);
            }
            if !executor.policy.can_use_device_interface(interface) {
                return (false, format!("[device_invoke] interface '{}' not allowed by policy", interface), None);
            }
            // Params:
            // - if a single 4th arg is valid JSON, use it; else {}
            // - if multiple params are provided, pass them as a JSON array of strings
            let params = parse_device_invoke_params(args);
            if interface == "local_media" {
                let bridge = match device_bridge {
                    Some(b) => b,
                    None => return (false, "[device_invoke] device bridge not available (no UI client for local_media)".to_string(), None),
                };
                let (request_id, rx) = bridge
                    .submit_request(interface.to_string(), device_id.to_string(), action.to_string(), params)
                    .await;
                let data_dir = store_path.and_then(|p| p.parent()).unwrap_or_else(|| std::path::Path::new("."));
                tracing::info!(request_id = %request_id, interface = %interface, device_id = %device_id, action = %action, "[device_bridge] submitted request, waiting for UI");
                const DEVICE_TIMEOUT_SECS: u64 = 120;
                match tokio::time::timeout(
                    std::time::Duration::from_secs(DEVICE_TIMEOUT_SECS),
                    rx,
                )
                .await
                {
                    Ok(Ok(result)) => {
                        tracing::info!(request_id = %request_id, success = result.success, "[device_bridge] received result from UI");
                        let msg = if result.success {
                            let data_preview = result.data.as_deref().map(|d| if d.len() > 200 { format!("{}...", &d[..d.floor_char_boundary(200)]) } else { d.to_string() }).unwrap_or_else(|| "ok".to_string());
                            format!("[device_invoke local_media {}] success — {}", action, data_preview)
                        } else {
                            format!("[device_invoke local_media {}] failed or refused", action)
                        };
                        // Save captured image to data_dir/captures and return base64 for UI display
                        let out_image_base64 = if result.success && action == "capture" {
                            if let Some(ref data) = result.data {
                                if !data.is_empty() {
                                    let captures_dir = data_dir.join("captures");
                                    let _ = std::fs::create_dir_all(&captures_dir);
                                    let filename = format!("photo_{}.jpg", chrono::Utc::now().format("%Y-%m-%d_%H-%M-%S"));
                                    let path = captures_dir.join(&filename);
                                    if let Ok(decoded) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data) {
                                        let _ = std::fs::write(&path, decoded);
                                    }
                                    Some(data.clone())
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        (result.success, msg, out_image_base64)
                    }
                    Ok(Err(_)) => {
                        tracing::warn!(request_id = %request_id, "[device_bridge] channel closed without result");
                        (false, "[device_invoke] channel closed without result".to_string(), None)
                    }
                    Err(_) => {
                        tracing::warn!(request_id = %request_id, "[device_bridge] timeout, no POST /api/device/result received");
                        bridge.cancel(&request_id).await;
                        (false, format!("[device_invoke] timeout after {}s (no UI client responded)", DEVICE_TIMEOUT_SECS), None)
                    }
                }
            } else if interface == "synthetic_input" {
                let bridge = match device_bridge {
                    Some(b) => b,
                    None => return (false, "[device_invoke] device bridge not available (no UI client for synthetic_input)".to_string(), None),
                };
                let (request_id, rx) = bridge
                    .submit_request(interface.to_string(), device_id.to_string(), action.to_string(), params)
                    .await;
                const DEVICE_TIMEOUT_SECS: u64 = 120;
                match tokio::time::timeout(
                    std::time::Duration::from_secs(DEVICE_TIMEOUT_SECS),
                    rx,
                )
                .await
                {
                    Ok(Ok(result)) => {
                        let msg = if result.success {
                            let data_preview = result.data.as_deref().map(|d| if d.len() > 200 { format!("{}...", &d[..d.floor_char_boundary(200)]) } else { d.to_string() }).unwrap_or_else(|| "ok".to_string());
                            format!("[device_invoke synthetic_input {}] success — {}", action, data_preview)
                        } else {
                            format!("[device_invoke synthetic_input {}] failed or refused", action)
                        };
                        (result.success, msg, None)
                    }
                    Ok(Err(_)) => (false, "[device_invoke] channel closed without result".to_string(), None),
                    Err(_) => {
                        bridge.cancel(&request_id).await;
                        (false, format!("[device_invoke] timeout after {}s (no UI client responded)", DEVICE_TIMEOUT_SECS), None)
                    }
                }
            } else if interface == "system" {
                (false, "[device_invoke] system interface (e.g. print) not yet implemented".to_string(), None)
            } else {
                (false, format!("[device_invoke] interface '{}' handler not yet implemented", interface), None)
            }
        }
        "generate_image" => {
            let prompt = args.get(0).map(String::as_str).unwrap_or("").trim();
            if prompt.is_empty() {
                (false, "[generate_image] usage: generate_image <prompt> [size]".to_string(), None)
            } else {
                let size = args.get(1).map(String::as_str).filter(|s| !s.is_empty());
                let data_dir = store_path.and_then(|p| p.parent()).unwrap_or_else(|| Path::new("."));
                match crate::image_generation::generate_image_impl(data_dir, prompt, size).await {
                    Ok((msg, data_url)) => (true, msg, Some(data_url)),
                    Err(e) => (false, e, None),
                }
            }
        }
        _ => {
            if executor.policy.can_run_command(tool_name) {
                match executor.run_command(tool_name, args, None, None).await {
                    Ok((out, res)) => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        let msg = if res.success {
                            format!("[run_command {}] stdout: {} stderr: {}", tool_name, stdout.trim(), stderr.trim())
                        } else {
                            format!("[run_command] {} stderr: {}", res.summary, stderr.trim())
                        };
                        (res.success, msg, None)
                    }
                    Err(e) => (false, format!("[run_command] error: {}", e), None),
                }
            } else {
                let names: Vec<&str> = AVAILABLE_TOOLS.iter().map(|(n, _)| *n).collect();
                (false, format!("[{}] unknown tool. Available: {}.", tool_name, names.join(", ")), None)
            }
        }
    };
    result
}

/// Compact short-term memory when it would exceed context: summarize oldest turns via LLM and replace in store.
/// Optionally promote the summary to long-term memory (embed + store).
/// Refuses compaction beyond MAX_COMPACTIONS_PER_SESSION per session to avoid costly loops.
async fn compact_short_term_if_needed(
    short_term: &Arc<ShortTermStore>,
    session_id: &str,
    llm_router: &akasha_llm::LLMRouter,
    new_message_tokens: usize,
    long_term_client: Option<&LongTermMemoryClient>,
) {
    if short_term.get_compaction_count(session_id).await >= crate::memory::MAX_COMPACTIONS_PER_SESSION {
        tracing::info!(session_id, "Short-term compaction skipped: max compactions per session reached (start a new session if context is too long)");
        return;
    }
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
        "Summarize in a short paragraph in English, keeping important facts and decisions:\n\n{}",
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
        system_prompt: None,
        image_data_urls: None,
    };
    match llm_router.complete(&req).await {
        Ok(resp) => {
            let summary = resp.text.trim();
            if !summary.is_empty() {
                short_term.replace_oldest_with_summary(session_id, summary.to_string(), to_summarize).await;
                short_term.increment_compaction_count(session_id).await;
                tracing::debug!(session_id, to_summarize, "Short-term memory compacted");
                // Promote summary to long-term memory (spec 06)
                if let Some(client) = long_term_client {
                    let summary = summary.to_string();
                    let session_id_attr = session_id.to_string();
                    let client = client.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = client.promote(summary.clone(), "compaction".to_string(), None, None, Some(session_id_attr.clone()), Some(1), Some("session".to_string()), None, None, None) {
                            tracing::warn!(error = %e, "Long-term promote after compaction failed");
                        } else if let Err(e) = client.emit_event("memory_promoted".to_string(), summary, None, None, Some(session_id_attr), None, Some(1), Some("session".to_string()), Some("compaction".to_string())) {
                            tracing::debug!(error = %e, "Episodic emit after compaction promote failed");
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
/// Skips if a daily summary for that date already exists in long-term memory.
pub async fn summarize_yesterday_and_promote(
    short_term_dir: PathBuf,
    llm_router: Arc<akasha_llm::LLMRouter>,
    long_term_client: Option<LongTermMemoryClient>,
) {
    let Some(client) = long_term_client else { return };
    let yesterday = chrono::Utc::now() - chrono::Duration::days(1);
    let yesterday_str = yesterday.format("%Y-%m-%d").to_string();
    let session_id = format!("day-{}", yesterday_str);
    let turns = match ShortTermStore::read_day_from_disk(&session_id, &short_term_dir) {
        Some(t) if !t.is_empty() => t,
        _ => return,
    };
    let has_summary = {
        let client = client.clone();
        let date = yesterday_str.clone();
        tokio::task::spawn_blocking(move || client.has_daily_summary_for_date(date))
            .await
            .unwrap_or(false)
    };
    if has_summary {
        tracing::debug!(session_id = %session_id, "Daily summary for yesterday already in long-term memory, skipping");
        return;
    }
    let blob = ShortTermStore::turns_to_context(&turns);
    let summary_prompt = format!(
        "Summarize in a short synthetic paragraph (5 to 10 lines) the day of {}: topics covered, decisions, projects or important information. \
Factual response in English.\n\n{}",
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
        system_prompt: None,
        image_data_urls: None,
    };
    match llm_router.complete(&req).await {
        Ok(resp) => {
            let summary = resp.text.trim();
            if !summary.is_empty() {
                let content = format!("Summary for {}: {}", session_id.trim_start_matches("day-"), summary);
                let client = client.clone();
                match tokio::task::spawn_blocking(move || {
                    let res = client.promote(content.clone(), "daily_summary".to_string(), None, None, None, Some(1), None, None, None, None);
                    if res.is_ok() {
                        let _ = client.emit_event("memory_promoted".to_string(), content, None, None, None, None, Some(1), None, Some("daily_summary".to_string()));
                    }
                    res
                }).await {
                    Ok(Ok(())) => tracing::info!(session_id = %session_id, "Yesterday summarized and stored in long-term memory"),
                    Ok(Err(e)) => tracing::warn!(error = %e, "Daily summary promote to long-term failed"),
                    Err(e) => tracing::warn!(error = %e, "Daily summary task join failed"),
                }
            }
        }
        Err(e) => tracing::debug!(error = %e, "Daily summary LLM call failed"),
    }
}

/// Returns a short, user-friendly progress message for a tool invocation (for key steps).
fn progress_message_for_tool(tool: &str, args: &[String]) -> String {
    let first_arg = args.first().map(|s| s.as_str()).unwrap_or("");
    let second_arg = args.get(1).map(|s| s.as_str()).unwrap_or("");
    let lower = tool.to_lowercase();
    if lower.contains("web_search") || lower == "search" {
        let query_preview = first_arg.chars().take(40).collect::<String>();
        if query_preview.is_empty() {
            "Searching the web…".to_string()
        } else {
            format!("Searching the web for “{}”…", query_preview.trim())
        }
    } else if lower.contains("web_fetch") || lower == "fetch" {
        "Fetching the page…".to_string()
    } else if lower == "bankr" || first_arg.eq_ignore_ascii_case("bankr") {
        if second_arg.eq_ignore_ascii_case("portfolio") {
            "Checking your portfolio…".to_string()
        } else {
            "Running Bankr…".to_string()
        }
    } else if lower.contains("run_command") && first_arg.eq_ignore_ascii_case("bankr") {
        if second_arg.eq_ignore_ascii_case("portfolio") {
            "Checking your portfolio…".to_string()
        } else {
            "Running Bankr…".to_string()
        }
    } else if lower.contains("write_file") || lower.contains("search_replace") || lower.contains("edit_file") {
        "Writing the file…".to_string()
    } else if lower.contains("read_file") {
        "Reading the file…".to_string()
    } else if lower.contains("generate_image") {
        "Generating the image…".to_string()
    } else if lower.contains("device_invoke") && first_arg.to_lowercase().contains("camera") {
        "Capturing with camera…".to_string()
    } else if lower.contains("run_command") {
        "Running the command…".to_string()
    } else if lower == "ask_user" {
        "Waiting for your input…".to_string()
    } else {
        format!("Running {}…", tool)
    }
}

/// Builds a short, task-specific acknowledgment message so the user sees a real take-over instead of a generic placeholder.
/// Uses the start of the user message for context; one coherent sentence, same tone (formal "you").
fn build_ack_message(user_message: &str) -> String {
    let trimmed = user_message.trim();
    let preview = if trimmed.is_empty() {
        "your request".to_string()
    } else {
        let max_len = 50;
        let truncated: String = trimmed.chars().take(max_len).collect();
        if truncated.chars().count() >= max_len {
            format!("« {}… »", truncated.trim_end())
        } else {
            format!("« {} »", truncated)
        }
    };
    format!("On it — looking into {}. You can follow progress in the Tasks tab.", preview)
}

/// Returns true if the text looks like a placeholder / promise ("I'll do it", "one second") rather than an actual answer.
/// Used after tool calls to avoid completing the task with "I will fetch…" instead of the real result.
fn looks_like_placeholder_after_tools(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    let placeholder_phrases = [
        "une seconde",
        "one second",
        "one moment",
        "un instant",
        "je vais ",
        "i'll ",
        "i will ",
        "i'm fetching",
        "i'm retrieving",
        "i'm getting",
        "i'm checking",
        "let me fetch",
        "let me get",
        "let me check",
        "let me find",
        "je vais récupérer",
        "je vais chercher",
        "je vais consulter",
        "génération",
        "récupération",
        "attendez",
        "wait ",
        "action en cours",
        "en cours",
        "puis te les résumer",
        "puis vous les",
        "then i'll ",
        "then i will ",
        "and then give you",
        "and then summarize",
        "will summarize",
        "will give you",
    ];
    placeholder_phrases.iter().any(|p| lower.contains(p))
}

/// Run LLM completion for a user message, with short-term + long-term memory (and compaction), optional tool-use loop. Push reply as progress, mark task completed.
/// image_data_urls: optional list of data URLs (data:image/...;base64,...) for vision-capable models.
pub(crate) async fn run_message_via_llm(
    bus: EventBus,
    llm_router: Arc<akasha_llm::LLMRouter>,
    store_path: std::path::PathBuf,
    spec_dir: std::path::PathBuf,
    task_id: Uuid,
    message: String,
    session_id: String,
    image_data_urls: Option<Vec<String>>,
    short_term: Option<std::sync::Arc<ShortTermStore>>,
    long_term_client: Option<LongTermMemoryClient>,
    tools_executor: Option<std::sync::Arc<tokio::sync::RwLock<std::sync::Arc<akasha_tools::ToolExecutor>>>>,
    tools_policy_path: Option<std::path::PathBuf>,
    skill_registry: Option<std::sync::Arc<crate::skills::SkillRegistry>>,
    process_registry: Option<ProcessRegistry>,
    conv_tx: Option<mpsc::Sender<OrchestratorTask>>,
    human_input_store: Option<HumanInputStore>,
    delegation_tx: Option<mpsc::Sender<DelegationRequest>>,
    task_completion_registry: Option<TaskCompletionRegistry>,
    agent_profile_cache: Option<AgentProfileCache>,
    task_usage_store: Option<std::sync::Arc<TaskUsageStore>>,
    device_bridge: Option<std::sync::Arc<crate::device_bridge::DeviceBridge>>,
    workspace_store: Option<TaskWorkspaceStore>,
) {
    let store = match TaskStore::open(&store_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(task_id = %task_id, error_kind = "store_open", error = %e, "LLM task: store open failed");
            notify_task_completion(&task_completion_registry, task_id).await;
            return;
        }
    };
    let _ = store.update_status(task_id, TaskStatus::Running);
    let structured = interpret_message(&message);
    let assigned_agent = store
        .get(task_id)
        .ok()
        .flatten()
        .map(|t| t.assigned_agent.clone())
        .unwrap_or_else(|| "conversation".to_string());

    let tools_executor_snapshot = match &tools_executor {
        Some(r) => Some((*r.read().await).clone()),
        None => None,
    };

    let message_webhook_url = std::env::var("AKASHA_MESSAGE_WEBHOOK_URL").ok();

    // Emit user message so TUI/API can show "what this task is about"
    let _ = bus.send(
        EventEnvelope::new(
            EventType::UserRequestReceived,
            Some(serde_json::json!({ "message": message })),
        )
        .with_correlation(task_id),
    );

    // Progress to show we have started (model may be loading on first call).
    let _ = bus.send(
        EventEnvelope::new(
            EventType::ProgressUpdate,
            Some(serde_json::json!({
                "task_id": task_id.to_string(),
                "progress_pct": 10,
                "message": "Analyzing your request…"
            })),
        )
        .with_correlation(task_id),
    );

    let max_tokens = std::env::var("AKASHA_MAX_RESPONSE_TOKENS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(4096);

    let tool_instruction = if tools_executor_snapshot.is_some() {
        let mut allowed_tools = tools_executor_snapshot.as_ref().and_then(|e| e.policy.allowed_tool_list());
        if let Some(ref list) = allowed_tools {
            let has_wildcard = tools_executor_snapshot.as_ref().map(|e| e.policy.allowed_commands.iter().any(|c| c.trim().eq_ignore_ascii_case("*"))).unwrap_or(false);
            if has_wildcard && !list.iter().any(|t| t == "run_command") {
                let mut list = list.clone();
                list.push("run_command".to_string());
                allowed_tools = Some(list);
            }
        }
        let base = available_tools_instruction(allowed_tools.as_deref());
        let (skills_part, skills_rule) = match &skill_registry {
            Some(reg) => {
                let list = reg.list().await;
                if list.is_empty() {
                    (String::new(), String::new())
                } else {
                    let skills_desc: Vec<String> = list
                        .iter()
                        .map(|s| format!("{} ({})", s.name, s.description))
                        .collect();
                    let names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
                    let part = format!(" ; Skills (use skill name as tool): {}", skills_desc.join(", "));
                    let rule = format!(
                        " INSTALLED SKILLS RULE: You have access to skills (extra capabilities). To see the list use TOOL: list_skills. To load full instructions for a skill use TOOL: read_skill <name> before invoking it by name. Currently installed: {}. Do NOT say they are not installed or suggest install_skill for them. Use bankr ONLY for balance/solde/wallet/portfolio/Base — never for weather, météo, or news (use web_search for those). For balance/solde/wallet/Base requests, if \"bankr\" is in the list, reply ONLY with TOOL: bankr <args> (e.g. TOOL: bankr portfolio 7d). Use the skill name as the tool name.\n\
             ",
                        names.join(", ")
                    );
                    (part, rule)
                }
            }
            None => (String::new(), String::new()),
        };
        let run_command_os_rule = match std::env::consts::OS {
            "windows" => "RUN_COMMAND OS: You are on Windows. Prefer cmd, PowerShell, curl.exe; avoid grep, cat, sed (not in default PATH). Use full path or .exe when needed. To test that the vault token works (e.g. GitHub API), use Invoke-WebRequest: TOOL: run_command VAULT:GITHUB_TOKEN=GITHUB_TOKEN powershell -NoProfile -Command \"Invoke-WebRequest -Uri 'https://api.github.com/repos/owner/repo' -Headers @{ Authorization = 'Bearer ' + $env:GITHUB_TOKEN } | Select-Object -Expand Content\" (replace owner/repo). Ensure 'powershell' is in allowed_commands in tools_policy.yaml. The system injects the vault value into the environment for the command.\n\
             ",
            _ => "RUN_COMMAND OS: You are on Linux/macos. Standard Unix commands (curl, grep, etc.) are available.\n\
             ",
        };
        format!(
            "\n\nYou may request tools by writing a line: TOOL: tool_name arg1 arg2 ...\nAvailable: {}{}.\n\
             Whenever you need the user to make a choice, confirm something, or provide information (e.g. choose between options, confirm a path, give credentials) before continuing, you MUST reply ONLY with TOOL: ask_user (then JSON with question/context/choices). Do not ask in plain text or the user's reply will start a new task and you cannot continue. Example: {{\"question\":\"Which option?\", \"choices\":[\"A\", \"B\"]}}.\n\
             CONNECTION RULE: If the user asks you to connect to an external service (GitHub repo, API, etc.), do NOT reply with a plain-text message. Use TOOL: ask_user. If the user has already confirmed credentials are configured, do NOT send another ask_user; proceed. Do not invent commands (e.g. /status repo:... does not exist); real commands are in /help.\n\
             CAMERA RULE (PRIORITAIRE sur WRITE): When the user asks for a webcam/camera photo (e.g. \"prends une photo\", \"take a photo\", \"photo depuis la webcam\", \"affiche-la dans le chat\", \"display it in the chat\"), you MUST reply ONLY with TOOL: device_discover local_media then TOOL: device_invoke local_media camera capture. Do NOT mention tools_policy.yaml, allowed_write_paths, or file writing. After the tool returns, if the user asked to \"display in the chat\" / \"affiche-la dans le chat\" / \"show it in the chat\", reply with ONLY a short confirmation in the user's language (e.g. in French: \"Photo prise. Elle s'affiche ci-dessous.\"; in English: \"Photo captured. It is shown below.\"). Do NOT offer \"save to file\", \"get a description\", \"take another photo\", or \"What would you like to do next?\" — the image is appended automatically below your message. Use the same language as the user (French if they wrote in French).\n\
             WRITE RULE (OBLIGATOIRE): When the user asks to save, record, or write a file (e.g. \"enregistre\", \"sauvegarde\", \"save to\", \"write to file\", or gives a folder path), you MUST reply ONLY with: a first line \"TOOL: write_file <full_path>\" then on the following lines the exact file content. Do NOT answer with \"I cannot write to disk\" or \"copy-paste the code yourself\". Use write_file; if the path is denied, the tool returns an error and you then explain tools_policy.yaml (allowed_write_paths). Paths can be Windows (C:\\Users\\...\\file.py) or Unix. Do NOT apply this rule when the user only asked for a webcam photo.\n\
             WEATHER/MÉTEO RULE (PRIORITY): When the user asks for weather, météo, or forecasts (e.g. \"quel temps\", \"météo demain\", \"weather in X\"), you MUST use TOOL: web_search <query> (and optionally web_fetch) to get the forecast. Do NOT use bankr, portfolio, or any other skill for weather — only web_search and web_fetch.\n\
             WEB SEARCH RULE: When the user asks for external information (weather, news, forecasts, schedules, etc.) that you do not have, you MUST use TOOL: web_search <query> first to search, then answer from the results. Do NOT reply with \"I did not find it\" or suggest sites without having called web_search. For météo/actualités: use web_search to find the info yourself, then summarize for the user.\n\
             INSTALL CLI RULE: When the user asks to install a CLI or package globally (e.g. \"install bankr CLI\", \"npm install -g @bankr/cli\", \"install the bankr cli in global\"), you MUST reply ONLY with TOOL: run_command <cmd> <args> (e.g. TOOL: run_command npm install -g @bankr/cli). Do NOT generate a script or ask the user to run commands themselves; run the installation command via the tool.\n\
             VAULT ENV RULE: To use a vault secret in a command you MUST call TOOL: run_command with VAULT:<vault_key>=<ENV_VAR> as the FIRST argument(s), then the command. The system injects the secret value into ENV_VAR for that command only. Example: TOOL: run_command VAULT:GITHUB_TOKEN=GITHUB_TOKEN curl -sS -H \"Authorization: Bearer $GITHUB_TOKEN\" https://api.github.com/repos/owner/repo. FORBIDDEN: never tell the user to run GITHUB_TOKEN=VAULT:GITHUB_TOKEN or export GITHUB_TOKEN=... or VAULT:GITHUB_TOKEN=ghp_... — you must output the TOOL: line yourself so the system runs the command and injects the token. For GitHub with token in vault: use TOOL: run_command VAULT:GITHUB_TOKEN=GITHUB_TOKEN curl -sS -H \"Authorization: Bearer $GITHUB_TOKEN\" https://api.github.com/repos/owner/repo (or gh repo view owner/repo). The vault key may be GITHUB_TOKEN or github_token; the part after = is the env var name the command uses (e.g. $GITHUB_TOKEN). Do NOT say you cannot access the repo without having called run_command with VAULT:... first.\n\
             {}\
             INSTALL SKILL RULE: When the user asks to install, download, get, fetch, or add a skill from a URL (e.g. \"install the bankr skill from …\", \"download the skill at this url\", \"get the skill from this url\", \"récupère le skill …\"), you MUST reply ONLY with TOOL: install_skill <url>. Do not give manual steps; perform the installation yourself. If the user says \"follow the SKILL.md instructions\" or \"follow the instructions in SKILL.md\", you MUST first reply with TOOL: install_skill <url> so the skill is registered; only after it is installed can you invoke it by name (e.g. TOOL: <skill_name> <args>). Do NOT use web_fetch or read_file to fetch SKILL.md and then execute its steps manually.\n\
             UNINSTALL SKILL RULE: When the user asks to uninstall or remove a skill (e.g. \"désinstalle bankr\", \"remove the bankr skill\"), you MUST reply ONLY with TOOL: uninstall_skill <name> (e.g. TOOL: uninstall_skill bankr).\n\
             SKILL USE RULE: When the user asks you to perform an action using a skill (e.g. \"vérifie mon wallet bankr\", \"check my balance with bankr\", \"run bankr whoami\"), you MUST reply ONLY with a single line: TOOL: <skill_name> <args> (e.g. TOOL: bankr whoami). The system will execute the command and return the result. Do NOT tell the user to run the command themselves or to \"use TOOL: bankr whoami\"; you must output that line yourself so the tool is executed.\n\
             PROJECT RULE: For requests that imply a substantial deliverable (novel, comic/BD, code project, series of chapters or files), never claim completion after one response if the full scope is not delivered. State clearly what was done, what remains to do, and that you will continue on the user's next message (or via a sub-task). Do not say \"C'est terminé\" or \"Voilà, c'est fait\" until all requested deliverables are done. If the user says \"continue\", \"la suite\", or \"and the rest\", resume the project in progress (use memory_search for project context if available) and continue without saying \"terminé\" until the full scope is delivered. For project-like work, use memory_store to save project state (objective, steps done, deliverables) after each significant progress, with source project:<name> so context is reloaded on the next message. For multi-step tasks, you can use TOOL: write_todos to define and track steps (then read_todos/update_todo to mark progress); the UI will show the list.\n\
             {}\
             If you need no tool, reply normally with your answer.\n\
             If write_file or read_file returns \"path not allowed by policy\" or \"denied\", tell the user that they CAN configure this: edit the file tools_policy.yaml \
             (in the Akasha data directory) and add path prefixes under allowed_write_paths or allowed_read_paths. It is not impossible — the user controls this YAML file.",
            base, skills_part, run_command_os_rule, skills_rule
        )
    } else {
        String::new()
    };

    // Build prompt: system (rules + role + personality) vs user (reminder + memory + turns + message).
    let data_dir = store_path.parent().unwrap_or_else(|| store_path.as_ref());
    let agent_profile = match &agent_profile_cache {
        Some(cache) => get_or_load_agent_profile(data_dir, cache).await,
        None => AgentProfile::load(data_dir),
    };
    let profile_block = crate::personality::build_personality_prompt(
        spec_dir.as_path(),
        &agent_profile,
        Some(&assigned_agent),
    );
    let os_env_block = match std::env::consts::OS {
        "windows" => "[Environment] The daemon runs on Windows. For run_command, prefer cmd, PowerShell, curl.exe; avoid Unix-only commands (grep, cat, sed) that are not in the default PATH (except WSL).\n\n",
        _ => "[Environment] The daemon runs on Linux/macOS. You can use usual Unix commands (curl, grep, etc.).\n\n",
    };
    let mut system_prompt = String::with_capacity(8192);
    system_prompt.push_str(APP_CONTEXT);
    system_prompt.push_str(os_env_block);
    if let Some(role_prompt) = agent_role_system_prompt(&assigned_agent) {
        system_prompt.push_str("[Role]\n");
        system_prompt.push_str(role_prompt);
        system_prompt.push_str("\n\n");
    }
    if !profile_block.is_empty() {
        system_prompt.push_str(&profile_block);
    }
    // Enforce language and personality so the model does not switch language (e.g. when tool output is in English).
    system_prompt.push_str(
        "\n\n[Response]\n\
        - Language: reply ONLY in the same language as the user's message. If the user writes in French, reply entirely in French; in English, in English. Do not adopt the language of tool results or context.\n\
        - Personality: always apply your identity (name), tone and form of address (formal/informal as configured).\n\n",
    );
    let system_prompt: Option<String> = if system_prompt.trim().is_empty() {
        None
    } else {
        Some(system_prompt.trim_end().to_string())
    };

    let personality_reminder = crate::personality::build_personality_reminder_line(
        spec_dir.as_path(),
        &agent_profile,
        Some(&assigned_agent),
    );
    let mut user_prefix = String::with_capacity(8192);
    user_prefix.push_str(&personality_reminder);
    user_prefix.push_str("Reply in the same language as the user message below (French, English, etc.).\n\n");
    if let Some(ref st) = short_term {
        let turns = st.get_turns(&session_id).await;
        let last_15: Vec<_> = turns.iter().rev().take(15).cloned().rev().collect();
        if !last_15.is_empty() {
            let short_ctx = ShortTermStore::turns_to_context(&last_15);
            let capped = if short_ctx.chars().count() > 2000 {
                short_ctx.chars().take(2000).collect::<String>() + "…"
            } else {
                short_ctx
            };
            if !capped.is_empty() {
                user_prefix.push_str("[Recent context (this session)]\n");
                user_prefix.push_str(&capped);
                user_prefix.push_str("\n\n");
            }
        }
    }
    let turns_empty = match &short_term {
        Some(st) => st.get_turns(&session_id).await.is_empty(),
        None => true,
    };
    let expand_by_graph = std::env::var("AKASHA_GRAPH_EXPAND").ok().as_deref() == Some("1");
    let user_profile = UserProfile::load(data_dir);
    let user_identity_prefix = user_profile.format_for_prompt();
    let recall_params = crate::memory_orchestrator::RecallParams {
        message: message.clone(),
        session_id: session_id.clone(),
        filter_by_session: !turns_empty,
        suggest_project: message_suggests_project(&message),
        is_first_message: turns_empty,
        expand_by_graph,
        user_identity_prefix: if user_identity_prefix.is_empty() {
            None
        } else {
            Some(user_identity_prefix)
        },
        ..Default::default()
    };
    let fused = crate::memory_orchestrator::recall_context(long_term_client.as_ref(), recall_params).await;
    let fused_str = fused.to_context_string();
    if !fused_str.is_empty() {
        user_prefix.push_str(&fused_str);
    }
    let user_rag_store = crate::user_rag::UserRagStore::new(data_dir);
    let rag_query = message.clone();
    let chunks = tokio::task::spawn_blocking(move || user_rag_store.retrieve(&rag_query, 5))
        .await
        .ok()
        .and_then(|res| res.ok())
        .unwrap_or_default();
    if !chunks.is_empty() {
        user_prefix.push_str("[User documents — use these excerpts if relevant to answer]\n");
        for c in &chunks {
            user_prefix.push_str("- ");
            user_prefix.push_str(&c.replace('\n', " "));
            user_prefix.push_str("\n");
        }
        user_prefix.push_str("\n");
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
            user_prefix.push_str(short_ctx.trim_end());
            user_prefix.push_str("\n\n");
        }
    }
    let intent_flags = compute_message_intent_flags(&message);
    let write_reminder = if intent_flags.save_file {
        WRITE_FILE_REMINDER
    } else {
        ""
    };
    let web_search_reminder = if intent_flags.external_info
        && tools_executor_snapshot
            .as_ref()
            .map(|e| e.policy.can_use_tool("web_search"))
            .unwrap_or(false)
    {
        WEB_SEARCH_REMINDER
    } else {
        ""
    };
    let device_camera_reminder = if intent_flags.camera_or_mic
        && tools_executor_snapshot
            .as_ref()
            .map(|e| e.policy.can_use_device_interface("local_media"))
            .unwrap_or(false)
    {
        DEVICE_CAMERA_REMINDER
    } else {
        ""
    };
    let image_generation_reminder = if intent_flags.image_generation {
        IMAGE_GENERATION_REMINDER
    } else {
        ""
    };
    let github_vault_reminder = if intent_flags.github_with_vault
        && tools_executor_snapshot
            .as_ref()
            .map(|e| e.policy.can_use_tool("run_command"))
            .unwrap_or(false)
    {
        GITHUB_VAULT_REMINDER
    } else {
        ""
    };
    // When user clearly wants a photo from camera, prefix the message with an imperative so the model responds with device_invoke directly (no ask_user).
    let user_message = if !device_camera_reminder.is_empty() {
        format!(
            "[Répondre par: TOOL: device_discover local_media puis TOOL: device_invoke local_media camera capture. Ne pas utiliser ask_user.]\n\n{}",
            message
        )
    } else {
        message.clone()
    };
    let mut current_prompt = if user_prefix.trim().is_empty() {
        format!(
            "{}{}{}{}{}User:\n{}",
            write_reminder,
            web_search_reminder,
            device_camera_reminder,
            image_generation_reminder,
            github_vault_reminder,
            user_message
        )
    } else {
        format!(
            "{}{}{}{}{}{}User:\n{}",
            user_prefix.trim_end(),
            write_reminder,
            web_search_reminder,
            device_camera_reminder,
            image_generation_reminder,
            github_vault_reminder,
            user_message
        )
    };
    let reply_text;
    const MAX_TOOL_ROUNDS: u32 = 5;
    let mut round = 0u32;
    let mut tool_loop_history: Vec<(String, String)> = Vec::new();
    let mut last_tool_results_blob: Option<String> = None;
    let mut force_synthesis_attempted = false;
    let mut last_captured_image_base64: Option<String> = None;
    let mut last_llm_model_used: Option<String> = None;

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
        // Quota: stop task if session cost or tokens exceed configured limits (Phase 2.3).
        if let Some(ref store) = task_usage_store {
            let (session_tokens, session_cost) = store.get_session(&session_id).await.unwrap_or((0, 0.0));
            if let Some(max_cost) = std::env::var("AKASHA_MAX_COST_PER_SESSION_USD").ok().and_then(|s| s.parse::<f64>().ok()) {
                if max_cost > 0.0 && session_cost >= max_cost {
                    reply_text = "Budget dépassé pour cette session (AKASHA_MAX_COST_PER_SESSION_USD). Démarrez une nouvelle session ou augmentez le plafond.".to_string();
                    break 'tool_rounds;
                }
            }
            if let Some(max_tokens) = std::env::var("AKASHA_MAX_TOKENS_PER_SESSION").ok().and_then(|s| s.parse::<u64>().ok()) {
                if max_tokens > 0 && session_tokens >= max_tokens {
                    reply_text = "Quota de tokens dépassé pour cette session (AKASHA_MAX_TOKENS_PER_SESSION). Démarrez une nouvelle session ou augmentez le plafond.".to_string();
                    break 'tool_rounds;
                }
            }
        }
        let preferred_task_type = Some(llm_router.resolve_task_type_for_agent(&assigned_agent));
        let request = CompletionRequest {
            prompt: format!("{}{}", current_prompt, tool_instruction),
            max_tokens: Some(max_tokens),
            temperature: Some(0.7),
            preferred_task_type,
            system_prompt: system_prompt.clone(),
            image_data_urls: if tool_loop_history.is_empty() {
                image_data_urls.clone()
            } else {
                None
            },
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
                    // #region agent log
                    const MAX_ACCUMULATED: usize = 2 * 1024 * 1024; // 2 MiB cap to prevent unbounded allocation (hypothesis D)
                    if accumulated.len() + chunk.len() > MAX_ACCUMULATED {
                        crate::debug_log::log("api.rs:stream_accumulated", "accumulated cap hit", &serde_json::json!({"accumulated_len": accumulated.len(), "chunk_len": chunk.len(), "max": MAX_ACCUMULATED}), "D");
                        accumulated.truncate(MAX_ACCUMULATED.saturating_sub(chunk.len()));
                    }
                    accumulated.push_str(&chunk);
                    if accumulated.len() > 512 * 1024 {
                        crate::debug_log::log("api.rs:stream_accumulated", "accumulated size", &serde_json::json!({"accumulated_len": accumulated.len(), "chunk_len": chunk.len()}), "D");
                    }
                    // #endregion
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
                Ok(None) => break,
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
                Ok(Ok(Ok(resp))) => {
                    last_llm_model_used = Some(resp.model_used.clone());
                    if let Some(ref store) = task_usage_store {
                        let tokens = resp.usage.as_ref().map(|u| u.prompt_tokens + u.completion_tokens).unwrap_or(0);
                        let cost = resp.cost_usd.unwrap_or(0.0);
                        store.add(task_id, &session_id, tokens, cost).await;
                    }
                    resp.text.trim().to_string()
                }
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

        let tool_calls = tools_executor_snapshot.as_ref().and_then(|_| {
            let calls = parse_tool_calls(&response);
            if calls.is_empty() { None } else { Some(calls) }
        });

        if let (Some(exec), Some(calls)) = (tools_executor_snapshot.as_ref(), tool_calls) {
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
                // User-friendly progress at key step: what we are doing right now (use skill name when actual_tool is empty, e.g. bankr skill).
                let display_tool = if actual_tool.is_empty() { name.as_str() } else { &actual_tool };
                let progress_msg = progress_message_for_tool(display_tool, args);
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": task_id.to_string(),
                            "progress_pct": 50,
                            "message": progress_msg
                        })),
                    )
                    .with_correlation(task_id),
                );
                // Phase 3.1: tools in require_approval need user confirmation before execution.
                if exec.policy.requires_approval(&actual_tool) {
                    match &human_input_store {
                        Some(store) => {
                            const APPROVAL_TIMEOUT_SECS: u64 = 300;
                            // Redact write-like tool args entirely; truncate others to avoid leaking secrets/blobs.
                            const MAX_APPROVAL_ARG_LEN: usize = 80;
                            let args_preview: String = if matches!(actual_tool.as_str(), "apply_patch" | "edit_file" | "write_file") {
                                "[redacted]".to_string()
                            } else {
                                let truncated: Vec<String> = args.iter()
                                    .take(3)
                                    .map(|a| if a.chars().count() > MAX_APPROVAL_ARG_LEN {
                                        format!("{}…", a.chars().take(MAX_APPROVAL_ARG_LEN).collect::<String>())
                                    } else {
                                        a.clone()
                                    })
                                    .collect();
                                let suffix = if args.len() > 3 { format!(" … ({} args)", args.len()) } else { String::new() };
                                truncated.join(" ") + &suffix
                            };
                            let question = format!("Approuver l'action : {} — {} ?", actual_tool, args_preview);
                            let choices = vec!["Approuver".to_string(), "Refuser".to_string()];
                            let (tx, rx) = tokio::sync::oneshot::channel();
                            let pending = PendingHumanInput {
                                question: question.clone(),
                                context: format!("Outil sensible (nécessite confirmation) : {}", actual_tool),
                                choices: Some(choices.clone()),
                                response_tx: tx,
                            };
                            {
                                let mut g = store.write().await;
                                g.insert(task_id, pending);
                            }
                            let payload = serde_json::json!({
                                "task_id": task_id.to_string(),
                                "question": question,
                                "context": format!("Outil : {}", actual_tool),
                                "choices": choices,
                                "tool_approval": true
                            });
                            let _ = bus.send(
                                EventEnvelope::new(EventType::TaskWaitingUserInput, Some(payload.clone())).with_correlation(task_id),
                            );
                            let approval_payload = serde_json::json!({
                                "tool": actual_tool,
                                "args_redacted": args_preview,
                                "task_id": task_id.to_string()
                            });
                            let _ = bus.send(
                                EventEnvelope::new(EventType::ToolApprovalRequest, Some(approval_payload)).with_correlation(task_id),
                            );
                            let granted = match tokio::time::timeout(
                                std::time::Duration::from_secs(APPROVAL_TIMEOUT_SECS),
                                rx,
                            ).await {
                                Ok(Ok(reply)) => reply.trim().eq_ignore_ascii_case("Approuver"),
                                _ => {
                                    // Timeout or channel error: remove stale pending entry to avoid it staying forever.
                                    {
                                        let mut g = store.write().await;
                                        g.remove(&task_id);
                                    }
                                    let expired_payload = serde_json::json!({
                                        "task_id": task_id.to_string(),
                                        "tool": actual_tool,
                                    });
                                    let _ = bus.send(
                                        EventEnvelope::new(EventType::ToolApprovalExpired, Some(expired_payload)).with_correlation(task_id),
                                    );
                                    false
                                }
                            };
                            if !granted {
                                tool_results.push("Action refusée par l'utilisateur (approbation requise).".to_string());
                                let payload = serde_json::json!({
                                    "tool": actual_tool,
                                    "approved": false
                                });
                                let _ = bus.send(
                                    EventEnvelope::new(EventType::ToolInvoked, Some(payload)).with_correlation(task_id),
                                );
                                continue;
                            }
                        }
                        None => {
                            tool_results.push("Action nécessitant approbation impossible (human_input_store indisponible).".to_string());
                            continue;
                        }
                    }
                }
                let (success, res, captured_image): (bool, String, Option<String>) = if actual_tool == "ask_user" {
                    // Human in the loop: register pending request, emit event, wait for user reply.
                    match &human_input_store {
                        Some(store) => {
                            let body_joined_string = args.join(" ");
                            let body = body_joined_string.trim();
                            let body = if body.is_empty() { "{}" } else { body };
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
                                (false, format!("[ask_user] invalid JSON: question required. Got: {}", body.chars().take(100).collect::<String>()), None)
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
                                    Ok(Ok(reply)) => (true, format!("[ask_user] User replied: {}", reply), None),
                                    Ok(Err(_)) => {
                                        let mut g = store.write().await;
                                        g.remove(&task_id);
                                        (false, "[ask_user] Channel closed.".to_string(), None)
                                    }
                                    Err(_) => {
                                        let mut g = store.write().await;
                                        g.remove(&task_id);
                                        (false, format!("[ask_user] Timeout after {}s; no user reply.", HUMAN_INPUT_TIMEOUT_SECS), None)
                                    }
                                }
                            }
                        }
                        None => (false, "[ask_user] Human-in-the-loop not available.".to_string(), None),
                    }
                } else if actual_tool == "delegate_to_agent" {
                    match &delegation_tx {
                        Some(tx) => {
                            let (reply_tx, reply_rx) = oneshot::channel();
                            let agent_type = args.get(0).cloned().unwrap_or_else(|| "conversation".to_string());
                            let message = args.get(1..).map(|a| a.join(" ")).unwrap_or_else(|| args.get(0).cloned().unwrap_or_default());
                            if tx.send(DelegationRequest {
                                requesting_task_id: task_id,
                                agent_type,
                                message,
                                reply_tx,
                            }).await.is_ok() {
                                match tokio::time::timeout(std::time::Duration::from_secs(310), reply_rx).await {
                                    Ok(Ok(Ok(msg))) => (true, format!("[delegate_to_agent] {}", msg), None),
                                    Ok(Ok(Err(e))) => (false, format!("[delegate_to_agent] {}", e), None),
                                    _ => (false, "[delegate_to_agent] timeout or channel closed".to_string(), None),
                                }
                            } else {
                                (false, "[delegate_to_agent] channel closed".to_string(), None)
                            }
                        }
                        None => (false, "[delegate_to_agent] not available".to_string(), None),
                    }
                } else if actual_tool == "install_skill" {
                    let url = args.get(0).map(String::as_str).unwrap_or("").trim();
                    if url.is_empty() {
                        (false, "[install_skill] usage: install_skill <url> (ex. https://github.com/BankrBot/skills/tree/main/bankr ou toute URL HTTPS autorisée dans tools_policy allowed_skill_install_hosts)".to_string(), None)
                    } else {
                        let data_dir = store_path.parent().unwrap_or_else(|| store_path.as_ref());
                        let allowed_hosts = exec.policy.skill_install_allowed_hosts();
                        let tools_reload = tools_executor.as_ref().and_then(|arc| {
                            tools_policy_path.as_ref().map(|p| (arc, p.as_path()))
                        });
                        match &skill_registry {
                            Some(reg) => {
                                let (s, r) = do_install_skill(url, data_dir, &spec_dir, reg, &allowed_hosts, tools_reload).await;
                                (s, r, None)
                            }
                            None => (false, "[install_skill] skill registry not available".to_string(), None),
                        }
                    }
                } else if actual_tool == "uninstall_skill" {
                    let skill_name = args.get(0).map(String::as_str).unwrap_or("").trim();
                    let data_dir = store_path.parent().unwrap_or_else(|| store_path.as_ref());
                    let tools_reload = tools_executor.as_ref().and_then(|arc| {
                        tools_policy_path.as_ref().map(|p| (arc, p.as_path()))
                    });
                    match &skill_registry {
                        Some(reg) => {
                            let (s, r) = do_uninstall_skill(skill_name, data_dir, &spec_dir, reg, tools_reload).await;
                            (s, r, None)
                        }
                        None => (false, "[uninstall_skill] skill registry not available".to_string(), None),
                    }
                } else if actual_tool == "write_todos" {
                    let payload = args.join(" ").trim().to_string();
                    match TaskStore::open(&store_path) {
                        Ok(store) => {
                            let todos = parse_todos_from_payload(&payload);
                            if let Err(e) = store.set_todos(task_id, &todos) {
                                (false, format!("[write_todos] error: {}", e), None)
                            } else {
                                let payload_json = serde_json::json!({
                                    "task_id": task_id.to_string(),
                                    "todos": todos.iter().map(|t| serde_json::json!({ "id": t.id, "title": t.title, "status": t.status.as_str() })).collect::<Vec<_>>()
                                });
                                let _ = bus.send(
                                    EventEnvelope::new(EventType::TodoListUpdated, Some(payload_json)).with_correlation(task_id),
                                );
                                (true, format!("[write_todos] {} step(s) saved.", todos.len()), None)
                            }
                        }
                        Err(e) => (false, format!("[write_todos] store error: {}", e), None),
                    }
                } else if actual_tool == "read_todos" {
                    match TaskStore::open(&store_path) {
                        Ok(store) => match store.get_todos(task_id) {
                            Ok(todos) => {
                                let summary: Vec<serde_json::Value> = todos.iter().enumerate().map(|(i, t)| {
                                    serde_json::json!({ "index": i + 1, "title": t.title, "status": t.status.as_str() })
                                }).collect();
                                (true, format!("[read_todos] {} step(s): {}", todos.len(), serde_json::to_string(&summary).unwrap_or_default()), None)
                            }
                            Err(e) => (false, format!("[read_todos] error: {}", e), None),
                        },
                        Err(e) => (false, format!("[read_todos] store error: {}", e), None),
                    }
                } else if actual_tool == "update_todo" {
                    let index_str = args.get(0).map(String::as_str).unwrap_or("").trim();
                    let status_str = args.get(1).map(String::as_str).unwrap_or("pending").trim();
                    let index: usize = index_str.parse().unwrap_or(0);
                    match TaskStore::open(&store_path) {
                        Ok(store) => match store.get_todos(task_id) {
                            Ok(mut todos) => {
                                if index == 0 || index > todos.len() {
                                    (false, format!("[update_todo] invalid index (1..{}): {}", todos.len(), index_str), None)
                                } else {
                                    let status = match status_str.to_lowercase().as_str() {
                                        "done" => TodoStatus::Done,
                                        "cancelled" => TodoStatus::Cancelled,
                                        _ => TodoStatus::Pending,
                                    };
                                    todos[index - 1].status = status;
                                    if let Err(e) = store.set_todos(task_id, &todos) {
                                        (false, format!("[update_todo] error: {}", e), None)
                                    } else {
                                        let payload_json = serde_json::json!({
                                            "task_id": task_id.to_string(),
                                            "todos": todos.iter().map(|t| serde_json::json!({ "id": t.id, "title": t.title, "status": t.status.as_str() })).collect::<Vec<_>>()
                                        });
                                        let _ = bus.send(
                                            EventEnvelope::new(EventType::TodoListUpdated, Some(payload_json)).with_correlation(task_id),
                                        );
                                        (true, format!("[update_todo] step {} set to {}.", index, status_str), None)
                                    }
                                }
                            }
                            Err(e) => (false, format!("[update_todo] error: {}", e), None),
                        },
                        Err(e) => (false, format!("[update_todo] store error: {}", e), None),
                    }
                } else if actual_tool == "list_skills" {
                    match &skill_registry {
                        Some(reg) => {
                            let list = reg.list().await;
                            let summary: Vec<String> = list.iter().map(|s| format!("{}: {}", s.name, s.description)).collect();
                            (true, format!("[list_skills] {} skill(s): {}", list.len(), summary.join(" ; ")), None)
                        }
                        None => (false, "[list_skills] skill registry not available.".to_string(), None),
                    }
                } else if actual_tool == "read_skill" {
                    let skill_name = args.get(0).map(String::as_str).unwrap_or("").trim();
                    if skill_name.is_empty() {
                        (false, "[read_skill] usage: read_skill <name>".to_string(), None)
                    } else {
                        match &skill_registry {
                            Some(reg) => {
                                if let Some(body) = reg.get_body(skill_name).await {
                                    (true, format!("[read_skill {}] Instructions:\n{}", skill_name, body), None)
                                } else {
                                    (false, format!("[read_skill] skill '{}' not found or has no body.", skill_name), None)
                                }
                            }
                            None => (false, "[read_skill] skill registry not available.".to_string(), None),
                        }
                    }
                } else if actual_tool.is_empty() {
                    // Skill with no tool_ref: if args provided, run as run_command(skill_name, ...args) (e.g. bankr whoami)
                    if !args.is_empty() {
                        let run_args: Vec<String> = std::iter::once(name.clone()).chain(args.iter().cloned()).collect();
                        let (s, r, _) = execute_tool_call(
                            exec,
                            "run_command",
                            &run_args,
                            process_registry.as_ref(),
                            long_term_client.as_ref(),
                            task_id,
                            Some(store_path.as_path()),
                            conv_tx.clone(),
                            message_webhook_url.as_deref(),
                            device_bridge.as_ref(),
                            workspace_store.as_ref(),
                        )
                        .await;
                        (s, r, None)
                    } else {
                        // No args: inject SKILL.md body as context for next round (doc-only)
                        match &skill_registry {
                            Some(reg) => {
                                if let Some(body) = reg.get_body(name).await {
                                    (true, format!("[Skill: {}] Instructions:\n{}", name, body), None)
                                } else {
                                    (false, format!("[Skill: {}] No instructions body.", name), None)
                                }
                            }
                            None => (false, "Skill registry not available.".to_string(), None),
                        }
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
                        device_bridge.as_ref(),
                        workspace_store.as_ref(),
                    )
                    .await
                };
                if let Some(img) = captured_image {
                    last_captured_image_base64 = Some(img);
                }
                // Phase F: emit ToolInvoked for Actions tab (spec 33)
                // Redact or truncate args in the event to avoid leaking large blobs or secrets.
                let redacted_args: Vec<String> = if matches!(actual_tool.as_str(), "apply_patch" | "edit_file" | "write_file") {
                    vec!["[redacted for write-like tool]".to_string()]
                } else {
                    const MAX_ARG_PREVIEW_LEN: usize = 512;
                    args.iter()
                        .map(|arg| {
                            if arg.len() > MAX_ARG_PREVIEW_LEN {
                                format!("{}...[truncated {} chars]", &arg[..arg.floor_char_boundary(MAX_ARG_PREVIEW_LEN)], arg.len().saturating_sub(MAX_ARG_PREVIEW_LEN))
                            } else {
                                arg.clone()
                            }
                        })
                        .collect()
                };
                let tool_display = if actual_tool.is_empty() { name.as_str() } else { actual_tool.as_str() };
                let payload = serde_json::json!({
                    "tool": tool_display,
                    "skill": if &actual_tool != name { Some(name.as_str()) } else { None::<&str> },
                    "args": redacted_args,
                    "result_preview": if res.len() > 300 { format!("{}...", &res[..res.floor_char_boundary(300)]) } else { res.clone() },
                    "success": success,
                    "explanation": serde_json::Value::Null
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
            last_tool_results_blob = Some(results_blob.clone());
            // Re-inject the user's request so the model always knows what to answer (avoids treating another demand or losing context).
            current_prompt = format!(
                "User request: {}\n\nYour previous reply:\n{}\n\nTool results:\n{}\n\nUsing ONLY the tool results above, answer the user's request now. Do NOT reply with a promise (e.g. \"I will fetch…\", \"Action in progress\"). The task ends after this message — give the actual answer (e.g. weather forecast, search summary). No TOOL: lines.",
                user_message, response, results_blob
            );
            if round >= MAX_TOOL_ROUNDS {
                let response_for_user = response
                    .lines()
                    .filter(|l| !l.trim_start().starts_with("TOOL:"))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_string();
                let limit_msg = if tool_results.iter().any(|r| r.contains("device_invoke") && (r.contains("timeout") || r.contains("refused"))) {
                    "L'accès à l'appareil (caméra/micro) a expiré ou a été refusé. Vous pouvez réessayer en renvoyant votre demande."
                } else if tool_results.iter().any(|r| r.contains("generate_image") && r.contains("générée")) {
                    "Image générée."
                } else if tool_results.iter().any(|r| r.contains("device_invoke") && r.contains("success")) {
                    "Photo reçue."
                } else {
                    "Limite de tours d'outils atteinte."
                };
                let image_md = last_captured_image_base64.as_ref()
                    .filter(|b| !b.is_empty())
                    .map(|b| {
                        let url = if b.starts_with("data:") { b.clone() } else { format!("data:image/jpeg;base64,{}", b) };
                        let label = if b.starts_with("data:") { "Image générée" } else { "Photo capturée" };
                        build_image_markdown(&label, &url)
                    })
                    .unwrap_or_default();
                let response_clean = ensure_no_open_code_block(&response_for_user);
                reply_text = if response_for_user.is_empty() {
                    format!("{}{}", limit_msg, image_md)
                } else {
                    format!("{}\n\n[{}]{}", response_clean, limit_msg, image_md)
                };
                break;
            }
            continue;
        }

        let response_for_user = response
            .lines()
            .filter(|l| !l.trim_start().starts_with("TOOL:"))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        // If we already ran tools but the model returned a placeholder ("Je vais… Une seconde."), force one more round to get the actual answer.
        if !tool_loop_history.is_empty()
            && last_tool_results_blob.as_ref().map_or(false, |b| !b.is_empty())
            && looks_like_placeholder_after_tools(&response_for_user)
            && !force_synthesis_attempted
        {
            force_synthesis_attempted = true;
            current_prompt = format!(
                "User request: {}\n\nTool results:\n{}\n\nThe user is waiting for the actual answer. Your previous message was a promise — the task is about to close, so you must answer NOW. Using the tool results above, write ONLY the final answer to the user's request. No TOOL: lines, no \"action in progress\".",
                user_message,
                last_tool_results_blob.as_deref().unwrap_or("")
            );
            continue;
        }
        let image_md = last_captured_image_base64.as_ref()
            .filter(|b| !b.is_empty())
            .map(|b| {
                let url = if b.starts_with("data:") { b.clone() } else { format!("data:image/jpeg;base64,{}", b) };
                let label = if b.starts_with("data:") { "Image générée" } else { "Photo capturée" };
                build_image_markdown(&label, &url)
            })
            .unwrap_or_default();
        let response_clean = ensure_no_open_code_block(&response_for_user);
        reply_text = if response_for_user.is_empty() {
            format!("{}{}", response, image_md)
        } else {
            format!("{}{}", response_clean, image_md)
        };
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
            match tokio::task::spawn_blocking(move || {
                let res = client.promote(fact.clone(), "user_fact".to_string(), None, None, None, Some(2), Some("global_user".to_string()), None, None, None);
                if res.is_ok() {
                    let _ = client.emit_event("user_preference".to_string(), fact, None, None, None, None, Some(2), Some("global_user".to_string()), Some("user_fact".to_string()));
                }
                res
            }).await {
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
        let agent_profile_cache_for_extract = agent_profile_cache.clone();
        tokio::spawn(async move {
            let _permit = match sem.try_acquire() {
                Ok(p) => p,
                Err(_) => {
                    tracing::debug!("Background fact extraction skipped: semaphore full");
                    return;
                }
            };
            let extract_prompt = format!(
                "Extract items to remember. One line per item, each line starts with exactly one of these prefixes:\n\
FACT: personal facts (name, preferences, decisions)\n\
PROJECT: projects created or mentioned\n\
INTEREST: interests\n\
IMPORTANT: important information to remember\n\
AGENT_NAME: the name the user gives the agent (e.g. You are called X)\n\
AGENT_PERSONALITY: personality or tone requested for the agent\n\
AGENT_RULE: a rule the agent must follow\n\
AGENT_CAN: what the agent can do (allowed)\n\
AGENT_CANNOT: what the agent must not do (forbidden)\n\
Write only lines with these prefixes, or NOTHING if none. No other text.\n\
Extract only facts explicitly mentioned (by the user or the assistant). Do not invent anything.\n\nUser: {}\n\nAssistant: {}",
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
                system_prompt: None,
                image_data_urls: None,
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
                        if !content.is_empty()
                            && !heuristic_set.contains(&content)
                            && !should_skip_capture_content(&content)
                        {
                            let content_trimmed = if content.len() > CAPTURE_MAX_CHARS {
                                content.chars().take(CAPTURE_MAX_CHARS).collect::<String>()
                            } else {
                                content
                            };
                            to_promote.push((content_trimmed, source));
                        }
                    }
                }
            }
            // Cap number of items promoted per turn (plan court terme 7).
            if to_promote.len() > CAPTURE_MAX_PER_TURN {
                to_promote.truncate(CAPTURE_MAX_PER_TURN);
            }
            if !agent_updates.is_empty() {
                let data_dir_extract = data_dir_for_extract.clone();
                let cache = agent_profile_cache_for_extract.clone();
                let mut profile = match &cache {
                    Some(c) => get_or_load_agent_profile(&data_dir_extract, c).await,
                    None => AgentProfile::load(&data_dir_for_extract),
                };
                for (kind, value) in agent_updates {
                    profile.apply_extracted(&kind, value);
                }
                if let Err(e) = profile.save(&data_dir_for_extract) {
                    tracing::warn!(error = %e, "Failed to save agent profile");
                } else {
                    if let Some(c) = &cache {
                        set_agent_profile_cache(c, profile).await;
                    }
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
                match tokio::task::spawn_blocking(move || client.promote(c, s, None, None, None, None, None, None, None, None)).await {
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
    // Phase 3.3: when task ends with an error-like outcome, emit TaskEscalatedToHuman for UI banner / retry.
    let reply_lower = reply_text.to_lowercase();
    let is_error_outcome = reply_lower.contains("sorry, i couldn't")
        || reply_lower.contains("timed out")
        || reply_lower.contains("timeout")
        || reply_lower.contains("budget dépassé")
        || reply_lower.contains("quota de tokens")
        || reply_lower.contains("loop detected")
        || reply_lower.contains("refusée par l'utilisateur")
        || reply_lower.contains("action refusée");
    if is_error_outcome {
        let _ = bus.send(
            EventEnvelope::new(
                EventType::TaskEscalatedToHuman,
                Some(serde_json::json!({
                    "task_id": task_id.to_string(),
                    "reason": reply_text.chars().take(500).collect::<String>()
                })),
            )
            .with_correlation(task_id),
        );
    }
    let _ = bus.send(
        EventEnvelope::new(
            EventType::TaskCompleted,
            Some(serde_json::json!({
                "task_id": task_id.to_string(),
                "status": "completed",
                "model_used": last_llm_model_used
            })),
        )
        .with_correlation(task_id),
    );
    let _ = store.update_status(task_id, TaskStatus::Completed);
    notify_task_completion(&task_completion_registry, task_id).await;
    let summary_preview: String = reply_text.chars().take(300).collect();
    learn_from_task_outcome_async(
        long_term_client.clone(),
        task_id,
        message.clone(),
        "completed".to_string(),
        summary_preview,
        Some(session_id.clone()),
        structured.intent_slug.clone(),
    )
    .await;
}

/// Notify any waiter in the TaskCompletionRegistry that `task_id` has finished.
async fn notify_task_completion(registry: &Option<TaskCompletionRegistry>, task_id: Uuid) {
    if let Some(reg) = registry {
        if let Some(notify) = reg.write().await.remove(&task_id) {
            notify.notify_one();
        }
    }
}

/// Learn step: emit a `task_outcome` episodic event after a task completes (conversation or orchestrator).
/// Payload: task_id, initial_message_preview (200 chars), status, summary_preview (300 chars), optional intent.
/// Synchronous; must not be called from a tokio runtime thread (use learn_from_task_outcome_async from async code).
pub(crate) fn learn_from_task_outcome(
    client: Option<&LongTermMemoryClient>,
    task_id: Uuid,
    initial_message: &str,
    status: &str,
    summary: &str,
    session_id: Option<&str>,
    intent: Option<&str>,
) {
    let Some(client) = client else { return };
    let initial_preview: String = initial_message.chars().take(200).collect();
    let summary_preview: String = summary.chars().take(300).collect();
    let payload = serde_json::json!({
        "task_id": task_id.to_string(),
        "initial_message_preview": initial_preview,
        "status": status,
        "summary_preview": summary_preview,
        "intent": intent,
    });
    if let Err(e) = client.emit_event(
        "task_outcome".to_string(),
        payload.to_string(),
        None,
        None,
        session_id.map(String::from),
        Some(task_id.to_string()),
        None,
        None,
        None,
    ) {
        tracing::debug!(task_id = %task_id, error = %e, "task_outcome emit failed");
    }
}

/// Async wrapper: runs learn_from_task_outcome in spawn_blocking so it is safe to call from async (avoids blocking the runtime).
pub(crate) async fn learn_from_task_outcome_async(
    client: Option<LongTermMemoryClient>,
    task_id: Uuid,
    initial_message: String,
    status: String,
    summary: String,
    session_id: Option<String>,
    intent: Option<String>,
) {
    let Some(client) = client else { return };
    let _ = tokio::task::spawn_blocking(move || {
        learn_from_task_outcome(
            Some(&client),
            task_id,
            &initial_message,
            &status,
            &summary,
            session_id.as_deref(),
            intent.as_deref(),
        );
    })
    .await;
}

/// Optional channel to trigger daemon shutdown (for POST /api/restart).
pub type RestartTx = Option<tokio::sync::mpsc::Sender<()>>;

/// Stream event-bus events as Server-Sent Events (GET /api/events). Keeps connection open until client disconnects.
pub async fn stream_sse_events<W>(bus: &EventBus, stream: &mut W) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut rx = crate::agents::subscribe(bus);
    let headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nAccess-Control-Allow-Origin: *\r\n\r\n";
    stream.write_all(headers.as_bytes()).await?;
    stream.flush().await?;
    loop {
        match rx.recv().await {
            Ok(envelope) => {
                let payload = serde_json::json!({
                    "id": envelope.id.to_string(),
                    "event_type": envelope.event_type.as_str(),
                    "payload": envelope.payload,
                    "timestamp": envelope.timestamp.to_rfc3339(),
                    "correlation_id": envelope.correlation_id.map(|u| u.to_string()),
                });
                let line = format!("data: {}\n\n", payload.to_string());
                if stream.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stream.flush().await.is_err() {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
    Ok(())
}

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
    _tools_executor: Option<&std::sync::Arc<tokio::sync::RwLock<std::sync::Arc<akasha_tools::ToolExecutor>>>>,
    skill_registry: &std::sync::Arc<crate::skills::SkillRegistry>,
    short_term: Option<std::sync::Arc<ShortTermStore>>,
    long_term_client: Option<LongTermMemoryClient>,
    human_input_store: Option<HumanInputStore>,
    user_rag_store: &crate::user_rag::SharedUserRagStore,
    agent_profile_cache: &AgentProfileCache,
    update_cache: &UpdateCheckCache,
    task_usage_store: &TaskUsageStore,
    device_bridge: Option<&std::sync::Arc<crate::device_bridge::DeviceBridge>>,
) -> String {
    let data_dir = store_path.parent().unwrap_or_else(|| store_path.as_ref());

    // CSRF protection: reject state-changing requests that originate from a non-local web page.
    // Browsers include an Origin header on cross-origin requests; same-origin or non-browser clients
    // (curl, CLI) typically do not. Allowing only localhost/tauri origins for mutating methods blocks
    // attacks from malicious web pages opened in the same browser as the Tauri app.
    if matches!(method, "POST" | "PUT" | "DELETE" | "PATCH") {
        if let Some(origin) = headers.get("origin") {
            let origin = origin.trim();
            let is_local = origin == "null"
                || origin.starts_with("http://localhost")
                || origin.starts_with("http://127.0.0.1")
                || origin.starts_with("https://localhost")
                || origin.starts_with("https://127.0.0.1")
                || origin.starts_with("tauri://")
                || origin.starts_with("https://tauri.localhost");
            if !is_local {
                tracing::warn!(origin = %origin, method = %method, path = %path, "CSRF: rejected request from non-local origin");
                return json_response("403 Forbidden", r#"{"error":"origin_not_allowed"}"#);
            }
        }
    }

    if method == "GET" && (path == "/" || path.is_empty()) {
        return json_response("200 OK", r#"{"status":"ok"}"#);
    }

    // GET /api/update/status — cached result of latest.json from Akasha_app (for UI update banner)
    if method == "GET" && path == "/api/update/status" {
        let status = update_cache.read().await;
        let body = serde_json::json!({
            "remote_version": status.remote_version,
            "download_url": status.download_url,
            "release_notes_url": status.release_notes_url,
            "last_checked_at": status.last_checked_at.map(|t| t.to_rfc3339()),
            "error": status.error,
        });
        return json_response("200 OK", &body.to_string());
    }

    // GET /api/device/pending — oldest pending device request (for UI to fulfill: camera, mic, etc.)
    if method == "GET" && path == "/api/device/pending" {
        if let Some(bridge) = device_bridge {
            match bridge.get_pending().await {
                Some((request_id, interface, device_id, action, params)) => {
                    tracing::info!(request_id = %request_id, interface = %interface, action = %action, "[device_bridge] GET /pending → returning request to UI");
                    let body = serde_json::json!({
                        "request_id": request_id,
                        "interface": interface,
                        "device_id": device_id,
                        "action": action,
                        "params": params,
                    });
                    return json_response("200 OK", &body.to_string());
                }
                None => {
                    return json_response("200 OK", r#"{"pending":false}"#);
                }
            }
        } else {
            return json_response("200 OK", r#"{"pending":false}"#);
        }
    }

    // POST /api/device/result — UI sends result of device action (e.g. image base64, audio base64)
    if method == "POST" && path == "/api/device/result" {
        if let Some(bridge) = device_bridge {
            let body_json = body.as_deref().and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
            let request_id = body_json.as_ref().and_then(|j| j.get("request_id")).and_then(|v| v.as_str());
            let success = body_json.as_ref().and_then(|j| j.get("success")).and_then(|v| v.as_bool()).unwrap_or(false);
            let data = body_json.as_ref().and_then(|j| j.get("data")).and_then(|v| v.as_str()).map(String::from);
            let data_len = data.as_ref().map(|s| s.len()).unwrap_or(0);
            let request_id = match request_id.filter(|s| !s.is_empty()) {
                Some(id) => id,
                None => {
                    tracing::warn!("[device_bridge] POST /result missing or empty request_id");
                    return json_response("400 Bad Request", r#"{"error":"request_id_required"}"#);
                }
            };
            tracing::info!(request_id = %request_id, success = success, data_len = data_len, "[device_bridge] POST /result received");
            let result = crate::device_bridge::DeviceResult { success, data };
            let fulfilled = bridge.fulfill(request_id, result).await;
            if !fulfilled {
                tracing::warn!(request_id = %request_id, "[device_bridge] POST /result request_id not found in claimed (stale or wrong id)");
            }
            let body = serde_json::json!({ "ok": fulfilled });
            return json_response("200 OK", &body.to_string());
        } else {
            return json_response("501 Not Implemented", r#"{"error":"device_bridge_unavailable"}"#);
        }
    }

    // GET /api/agent-profile — read agent profile (name, personality, role, gender, avatar, rules, can_do, cannot_do, traits_override, preferred_mode)
    if method == "GET" && path == "/api/agent-profile" {
        let profile = get_or_load_agent_profile(data_dir, agent_profile_cache).await;
        let body_json = serde_json::json!({
            "name": profile.name,
            "personality": profile.personality,
            "role": profile.role,
            "gender": profile.gender,
            "avatar": profile.avatar,
            "rules": profile.rules,
            "can_do": profile.can_do,
            "cannot_do": profile.cannot_do,
            "traits_override": profile.traits_override,
            "preferred_mode": profile.preferred_mode
        });
        return json_response("200 OK", &body_json.to_string());
    }

    // POST /api/agent-profile — update agent profile (merge with existing). Body: { name?, personality?, role?, gender?, avatar?, rules?, can_do?, cannot_do?, traits_override?, preferred_mode? }
    if method == "POST" && path == "/api/agent-profile" {
        let mut profile = get_or_load_agent_profile(data_dir, agent_profile_cache).await;
        if let Some(body) = body.as_deref() {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) {
                if let Some(s) = v.get("name").and_then(|x| x.as_str()) {
                    profile.name = Some(s.to_string());
                }
                if let Some(s) = v.get("personality").and_then(|x| x.as_str()) {
                    profile.personality = Some(s.to_string());
                }
                if v.get("role").is_some() {
                    profile.role = v.get("role").and_then(|x| x.as_str()).map(String::from);
                }
                if v.get("gender").is_some() {
                    profile.gender = v.get("gender").and_then(|x| x.as_str()).map(String::from);
                }
                if v.get("avatar").is_some() {
                    profile.avatar = v.get("avatar").and_then(|x| x.as_str()).map(String::from);
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
                if let Some(obj) = v.get("traits_override").and_then(|x| x.as_object()) {
                    let mut map = std::collections::HashMap::new();
                    for (k, val) in obj {
                        if let Some(n) = val.as_f64() {
                            map.insert(k.clone(), n);
                        }
                    }
                    profile.traits_override = if map.is_empty() { None } else { Some(map) };
                }
                if v.get("preferred_mode").is_some() {
                    profile.preferred_mode = v.get("preferred_mode").and_then(|x| x.as_str()).map(String::from);
                }
            }
        }
        // Persist default name if none or empty so the agent always has an identity on disk
        if profile.name.as_deref().map(|s| s.trim().is_empty()).unwrap_or(true) {
            profile.name = Some(AgentProfile::DEFAULT_NAME.to_string());
        }
        match profile.save(data_dir) {
            Ok(()) => {
                set_agent_profile_cache(agent_profile_cache, profile).await;
                return json_response("200 OK", r#"{"ok":true,"message":"Profil agent mis à jour"}"#);
            }
            Err(e) => return json_response("500 Internal Server Error", &serde_json::json!({ "error": e.to_string() }).to_string()),
        }
    }

    // GET /api/user-profile — read user profile (first_name, last_name, how_to_call, onboarding_completed)
    if method == "GET" && path == "/api/user-profile" {
        let profile = UserProfile::load(data_dir);
        let body_json = serde_json::json!({
            "first_name": profile.first_name,
            "last_name": profile.last_name,
            "how_to_call": profile.how_to_call,
            "onboarding_completed": profile.onboarding_completed,
        });
        return json_response("200 OK", &body_json.to_string());
    }

    // POST /api/user-profile — update user profile. Body: { first_name?, last_name?, how_to_call?, onboarding_completed? }
    if method == "POST" && path == "/api/user-profile" {
        let mut profile = UserProfile::load(data_dir);
        if let Some(body) = body.as_deref() {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) {
                if v.get("first_name").is_some() {
                    profile.first_name = v.get("first_name").and_then(|x| x.as_str()).map(String::from);
                }
                if v.get("last_name").is_some() {
                    profile.last_name = v.get("last_name").and_then(|x| x.as_str()).map(String::from);
                }
                if v.get("how_to_call").is_some() {
                    profile.how_to_call = v.get("how_to_call").and_then(|x| x.as_str()).map(|s| s.trim().to_string());
                }
                if let Some(b) = v.get("onboarding_completed").and_then(|x| x.as_bool()) {
                    profile.onboarding_completed = b;
                }
            }
        }
        match profile.save(data_dir) {
            Ok(()) => return json_response("200 OK", r#"{"ok":true,"message":"Profil utilisateur mis à jour"}"#),
            Err(e) => return json_response("500 Internal Server Error", &serde_json::json!({ "error": e.to_string() }).to_string()),
        }
    }

    // POST /api/personality-memory — store a structured personality preference (Phase 3). Body: { "key": "preferred_tone"|"technical_depth_preference"|..., "value": "..." }
    if method == "POST" && path == "/api/personality-memory" {
        let Some(client) = long_term_client.clone() else {
            return json_response("503 Service Unavailable", r#"{"error":"long_term_memory_unavailable"}"#);
        };
        let Some(body) = body.as_deref() else {
            return json_response("400 Bad Request", r#"{"error":"body_required"}"#);
        };
        let v: serde_json::Value = match serde_json::from_slice(body) {
            Ok(x) => x,
            Err(_) => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
        };
        let key = match v.get("key").and_then(|x| x.as_str()) {
            Some(k) if !k.trim().is_empty() => k.trim().to_string(),
            _ => return json_response("400 Bad Request", r#"{"error":"key_required"}"#),
        };
        let value = v.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let payload = serde_json::json!({ "key": key, "value": value }).to_string();
        let result = tokio::task::spawn_blocking(move || {
            client.emit_event(
                "personality_memory".to_string(),
                payload,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
        })
        .await;
        match result {
            Ok(Ok(id)) => return json_response("200 OK", &serde_json::json!({ "ok": true, "id": id.to_string() }).to_string()),
            Ok(Err(e)) => return json_response("500 Internal Server Error", &serde_json::json!({ "error": e }).to_string()),
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

    // GET /api/memory/search?q=...&top_k=... — semantic search in long-term memory
    if method == "GET" && path.starts_with("/api/memory/search") {
        let (q, top_k) = path
            .split('?')
            .nth(1)
            .map(|query_str| {
                let mut q = None;
                let mut top_k = 10u32;
                for part in query_str.split('&') {
                    if let Some(v) = part.strip_prefix("q=") {
                        let decoded = urlencoding::decode(v).unwrap_or_else(|_| std::borrow::Cow::Borrowed(v));
                        q = Some(decoded.trim().to_string());
                    } else if let Some(v) = part.strip_prefix("top_k=") {
                        if let Ok(n) = v.parse::<u32>() {
                            top_k = n.min(20);
                        }
                    }
                }
                (q, top_k)
            })
            .unwrap_or((None, 10));
        let query = q.as_deref().map(|s| s.trim()).unwrap_or("");
        if query.is_empty() {
            return json_response("400 Bad Request", r#"{"error":"missing or empty q"}"#);
        }
        let result = match long_term_client {
            Some(ref client) => {
                let client = client.clone();
                let query = query.to_string();
                let top_k = top_k as usize;
                tokio::task::spawn_blocking(move || client.search(query, top_k, None))
                    .await
                    .unwrap_or_default()
            }
            None => {
                return json_response(
                    "503 Service Unavailable",
                    r#"{"error":"long-term memory not available","long_term_available":false}"#,
                );
            }
        };
        let results: Vec<serde_json::Value> = result
            .iter()
            .map(|(id, content)| serde_json::json!({ "id": id, "content": content }))
            .collect();
        let body_json = serde_json::json!({
            "results": results,
            "long_term_available": true
        });
        return json_response("200 OK", &body_json.to_string());
    }

    // GET /api/memory/long-term?limit=50 — recent long-term entries (content, created_at, source, related)
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
        let relations = if !entries.is_empty() {
            if let Some(ref client) = long_term_client {
                let ids: Vec<String> = entries.iter().map(|(id, _, _, _)| id.clone()).collect();
                let client = client.clone();
                tokio::task::spawn_blocking(move || client.get_relations_for_entries(ids))
                    .await
                    .unwrap_or_default()
            } else {
                std::collections::HashMap::new()
            }
        } else {
            std::collections::HashMap::new()
        };
        let list: Vec<serde_json::Value> = entries
            .iter()
            .map(|(id, content, created_at, source)| {
                let related = relations
                    .get(id)
                    .map(|v| {
                        v.iter()
                            .map(|(to_id, kind)| serde_json::json!({ "id": to_id, "kind": kind }))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                serde_json::json!({ "id": id, "content": content, "created_at": created_at, "source": source, "related": related })
            })
            .collect();
        let body_json = serde_json::json!({
            "entries": list,
            "long_term_available": long_term_client.is_some()
        });
        return json_response("200 OK", &body_json.to_string());
    }

    // POST /api/memory/rebuild-relations — recompute "similar" relations for all existing entries
    if method == "POST" && path == "/api/memory/rebuild-relations" {
        const DEFAULT_MAX_PER_ENTRY: usize = 5;
        let result = match long_term_client {
            Some(ref client) => {
                let client = client.clone();
                tokio::task::spawn_blocking(move || client.rebuild_similar_relations(DEFAULT_MAX_PER_ENTRY))
                    .await
                    .unwrap_or_else(|e| Err(format!("task join error: {}", e)))
            }
            None => Err("long-term memory not available".to_string()),
        };
        match result {
            Ok(inserted) => {
                let body_json = serde_json::json!({ "inserted": inserted, "ok": true });
                return json_response("200 OK", &body_json.to_string());
            }
            Err(e) => {
                let body_json = serde_json::json!({ "error": e, "ok": false });
                return json_response("500 Internal Server Error", &body_json.to_string());
            }
        }
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
        checks.push(serde_json::json!({ "id": "os", "ok": true, "description": std::env::consts::OS }));

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
            .or_else(|| std::fs::read_to_string(data_dir.join("docs").join("user_guide.md")).ok())
            .unwrap_or_else(|| {
                "# Documentation\n\nDocumentation non disponible. Placez docs/user_guide.md dans le dossier d'extraction ou dans le data_dir (voir akasha paths).\n"
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

    // Phase 4: Microsoft Teams Bot Framework webhook
    if method == "POST" && (path == "/channels/teams" || path == "/channels/teams/message") {
        if let (Some(ref app_id), Some(ref app_password)) =
            (&channel_config.teams_app_id, &channel_config.teams_app_password)
        {
            let auth_header = headers.get("authorization").map(String::as_str);
            return crate::channels::teams::handle_teams_message(
                body,
                auth_header,
                app_id,
                app_password,
                channel_config.port,
                main_agent,
                store_path,
            );
        }
        return json_response("404 Not Found", r#"{"error":"teams_not_configured"}"#);
    }

    if method == "POST" && path == "/api/message" {
        let body_json = body.as_deref().and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let mut message = body_json
            .as_ref()
            .and_then(|v| v.get("message").and_then(|v| v.as_str().map(String::from)))
            .unwrap_or_default();
        // Parse attachments: images -> data URLs for vision; documents -> append extracted text to message.
        let image_data_urls: Option<Vec<String>> = {
            let arr = body_json.as_ref().and_then(|v| v.get("attachments").and_then(|a| a.as_array()));
            let mut urls = Vec::new();
            let mut doc_texts = Vec::new();
            if let Some(arr) = arr {
                for att in arr {
                    let typ = att.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    let content_base64 = att.get("content_base64").and_then(|c| c.as_str()).unwrap_or("");
                    let mime = att.get("mime_type").and_then(|m| m.as_str()).unwrap_or("image/png");
                    let name = att.get("name").and_then(|n| n.as_str()).unwrap_or("file");
                    if content_base64.is_empty() {
                        continue;
                    }
                    if typ == "image" || mime.starts_with("image/") {
                        let data_url = format!("data:{};base64,{}", mime, content_base64);
                        urls.push(data_url);
                    } else if typ == "document" || mime.starts_with("text/") || mime == "application/pdf" {
                        if let Ok(decoded) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, content_base64) {
                            let text = if mime == "application/pdf" {
                                pdf_extract::extract_text_from_mem(&decoded)
                                    .unwrap_or_else(|_| String::from("[Extraction du texte PDF impossible ou PDF vide.]"))
                            } else if let Ok(t) = String::from_utf8(decoded) {
                                t
                            } else {
                                continue;
                            };
                            if !text.trim().is_empty() {
                                doc_texts.push(format!("[Document « {} »]\n{}", name, text.trim()));
                            } else if mime == "application/pdf" {
                                doc_texts.push(format!("[Document « {} »]\n[PDF joint : extraction du texte vide (image ou PDF scanné).]", name));
                            }
                        }
                    }
                }
            }
            if !doc_texts.is_empty() {
                let user_msg = message.trim_end();
                let user_msg = if user_msg.is_empty() {
                    "(Pièce(s) jointe(s))"
                } else {
                    user_msg
                };
                message = format!(
                    "[Pièce(s) jointe(s) à ce message : quand l'utilisateur dit « ce document », « ce fichier », « analyse-le », « analyse ce document », etc., il parle du contenu joint ci-dessous, pas des échanges précédents.]\n\nMessage : {}\n\n--- Document(s) joint(s) ---\n{}",
                    user_msg,
                    doc_texts.join("\n\n")
                );
            } else if message.trim().is_empty() && !urls.is_empty() {
                // Pièces jointes images uniquement : éviter message vide pour la tâche.
                message = "(Pièce(s) jointe(s))".to_string();
            }
            if urls.is_empty() {
                None
            } else {
                Some(urls)
            }
        };
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
        let priority = body_json
            .as_ref()
            .and_then(|v| v.get("priority").and_then(|p| p.as_str()))
            .map(|s| if s.eq_ignore_ascii_case("high") { TaskPriority::UserHigh } else { TaskPriority::UserNormal })
            .unwrap_or(TaskPriority::UserNormal);
        // User talks only to orchestrator: ack immediately, delegate to conversation worker in background (non-blocking). session_id used for short-term memory.
        match main_agent.handle_message(store_path, &message, correlation_id, true, &session_id, image_data_urls, priority) {
            Ok(task_id) => {
                let ack_message = build_ack_message(&message);
                let body = serde_json::json!({
                    "ack": true,
                    "task_id": task_id.to_string(),
                    "session_id": session_id,
                    "message": ack_message
                });
                return json_response("200 OK", &body.to_string());
            }
            Err(_) => return json_response("500 Internal Server Error", r#"{"error":"handle_failed"}"#),
        }
    }

    if method == "GET" && path == "/api/tasks" {
        return get_task_list(store_path).await;
    }
    // GET /api/pending-human-input — list all tasks waiting for user input (so UI can show notifications after reload or when user was away)
    if method == "GET" && path == "/api/pending-human-input" {
        if let Some(ref store) = human_input_store {
            let g = store.read().await;
            let pending: Vec<_> = g
                .iter()
                .map(|(id, p)| {
                    serde_json::json!({
                        "task_id": id.to_string(),
                        "question": p.question,
                        "context": p.context,
                        "choices": p.choices
                    })
                })
                .collect();
            let body = serde_json::json!({ "pending": pending });
            return json_response("200 OK", &body.to_string());
        }
        return json_response("200 OK", r#"{"pending":[]}"#);
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
                    return get_task_status(store_path, progress, task_usage_store, id).await;
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
    if method == "GET" && path.starts_with("/api/calendar/events") {
        return get_calendar_events(store_path, path).await;
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

    // Phase D: Skills (loadable skills for agents; Agent Skills spec + flat YAML)
    if method == "GET" && path == "/api/skills" {
        let list = skill_registry.list().await;
        let body = serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string());
        return json_response("200 OK", &body);
    }
    if method == "POST" && path == "/api/skills/reload" {
        match skill_registry.reload(data_dir, spec_dir).await {
            Ok(count) => {
                let body = serde_json::json!({ "reloaded": true, "count": count }).to_string();
                return json_response("200 OK", &body);
            }
            Err(e) => {
                let body = serde_json::json!({ "error": "reload_failed", "detail": e.to_string() }).to_string();
                return json_response("500 Internal Server Error", &body);
            }
        }
    }
    if method == "POST" && path == "/api/skills/uninstall" {
        let body_json = body.as_deref().and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let name = body_json
            .as_ref()
            .and_then(|j| j.get("name"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        match name {
            Some(skill_name) => {
                let policy_path = data_dir.join("tools_policy.yaml");
                let tools_reload = _tools_executor.map(|r| (r, policy_path.as_path()));
                let (ok, msg) = do_uninstall_skill(
                    &skill_name,
                    data_dir,
                    spec_dir,
                    skill_registry,
                    tools_reload,
                )
                .await;
                if ok {
                    let body = serde_json::json!({ "uninstalled": true, "name": skill_name, "message": msg }).to_string();
                    return json_response("200 OK", &body);
                }
                let body = serde_json::json!({ "error": "uninstall_failed", "detail": msg }).to_string();
                return json_response("400 Bad Request", &body);
            }
            None => {
                let body = serde_json::json!({ "error": "missing_name", "detail": "Body must be JSON with \"name\": \"<skill_name>\"" }).to_string();
                return json_response("400 Bad Request", &body);
            }
        }
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

    // User RAG: list documents
    if method == "GET" && path == "/api/user-rag/documents" {
        let store = user_rag_store.lock().await;
        match store.list_documents() {
            Ok(docs) => {
                let body = serde_json::to_string(&serde_json::json!({ "documents": docs })).unwrap_or_else(|_| "[]".to_string());
                return json_response("200 OK", &body);
            }
            Err(e) => {
                let body = serde_json::json!({ "error": "list_failed", "detail": e.to_string() });
                return json_response("500 Internal Server Error", &body.to_string());
            }
        }
    }

    // User RAG: upload document (body: { name, content_base64, mime_type? })
    if method == "POST" && path == "/api/user-rag/documents" {
        let body_json = body.as_deref().and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let name = body_json.as_ref().and_then(|j| j.get("name")).and_then(|v| v.as_str()).map(String::from);
        let content_base64 = body_json.as_ref().and_then(|j| j.get("content_base64")).and_then(|v| v.as_str()).map(String::from);
        let mime_type = body_json.as_ref().and_then(|j| j.get("mime_type")).and_then(|v| v.as_str()).map(String::from).unwrap_or_else(|| "application/octet-stream".to_string());
        let name = match name.filter(|n| !n.is_empty()) {
            Some(n) => n,
            None => return json_response("400 Bad Request", r#"{"error":"name_required"}"#),
        };
        let content_base64 = match content_base64.filter(|c| !c.is_empty()) {
            Some(c) => c,
            None => return json_response("400 Bad Request", r#"{"error":"content_base64_required"}"#),
        };
        let store = user_rag_store.lock().await;
        match store.add_document(&content_base64, &name, &mime_type) {
            Ok(id) => {
                let body = serde_json::json!({ "id": id, "name": name, "message": "Document ajouté." });
                return json_response("200 OK", &body.to_string());
            }
            Err(e) => {
                let body = serde_json::json!({ "error": "add_failed", "detail": e.to_string() });
                return json_response("500 Internal Server Error", &body.to_string());
            }
        }
    }

    // User RAG: delete document by id
    if method == "DELETE" && path.starts_with("/api/user-rag/documents/") {
        let id = path.trim_start_matches("/api/user-rag/documents/").split('?').next().unwrap_or("").trim();
        if id.is_empty() {
            return json_response("400 Bad Request", r#"{"error":"id_required"}"#);
        }
        let store = user_rag_store.lock().await;
        match store.delete_document(id) {
            Ok(true) => return json_response("200 OK", r#"{"ok":true,"message":"Document supprimé."}"#),
            Ok(false) => return json_response("404 Not Found", r#"{"error":"document_not_found"}"#),
            Err(e) => {
                let body = serde_json::json!({ "error": "delete_failed", "detail": e.to_string() });
                return json_response("500 Internal Server Error", &body.to_string());
            }
        }
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
            system_prompt: None,
            image_data_urls: None,
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

    if method == "GET" && path.starts_with("/api/router/metrics") {
        let period = path.split('?').nth(1)
            .and_then(|q| q.split('&').find(|p| p.starts_with("period=")))
            .and_then(|p| p.strip_prefix("period="));
        let list: std::collections::HashMap<String, akasha_llm::ModelMetrics> = if let Some(period) = period {
            let (from_ts, to_ts) = match period {
                "day" => {
                    let now = chrono::Utc::now();
                    let start = now - chrono::Duration::days(1);
                    (Some(start), Some(now))
                }
                "week" => {
                    let now = chrono::Utc::now();
                    let start = now - chrono::Duration::days(7);
                    (Some(start), Some(now))
                }
                "month" => {
                    let now = chrono::Utc::now();
                    let start = now - chrono::Duration::days(30);
                    (Some(start), Some(now))
                }
                "year" => {
                    let now = chrono::Utc::now();
                    let start = now - chrono::Duration::days(365);
                    (Some(start), Some(now))
                }
                _ => (None, None),
            };
            match (from_ts, to_ts) {
                (Some(from), Some(to)) => {
                    match akasha_store::MetricsStore::open(store_path) {
                        Ok(store) => store.aggregate(Some(from), Some(to)).ok()
                            .map(|rows| rows.into_iter().map(|(k, v)| (k, akasha_llm::ModelMetrics {
                                total_requests: v.total_requests,
                                successful_requests: v.successful_requests,
                                failed_requests: v.failed_requests,
                                total_latency_ms: v.total_latency_ms,
                                total_tokens: v.total_tokens,
                                total_cost_usd: v.total_cost_usd,
                                fallback_triggered: v.fallback_triggered,
                                fallback_success: v.fallback_success,
                                last_success: v.last_success,
                                last_failure: v.last_failure,
                                latency_samples: std::collections::VecDeque::new(),
                            })).collect())
                            .unwrap_or_default(),
                        Err(_) => llm_router.metrics().list(),
                    }
                }
                _ => llm_router.metrics().list(),
            }
        } else {
            llm_router.metrics().list()
        };
        let body = serde_json::to_string(&list).unwrap_or_else(|_| "{}".to_string());
        return json_response("200 OK", &body);
    }

    // GET /api/metrics/summary — metrics with latency percentiles (P50, P95, P99) per provider/model
    if method == "GET" && path == "/api/metrics/summary" {
        let summary = llm_router.metrics().summary();
        let body = serde_json::to_string(&summary).unwrap_or_else(|_| "{}".to_string());
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
                        "parameters_preview": if parameters.len() > 200 { format!("{}...", &parameters[..parameters.floor_char_boundary(200)]) } else { parameters.to_string() }
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
            system_prompt: None,
            image_data_urls: None,
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

async fn get_task_status(store_path: &Path, progress: &ProgressCache, task_usage_store: &TaskUsageStore, id: Uuid) -> String {
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
    let (tokens_used, cost_usd) = task_usage_store.get_task(id).await.unwrap_or((0, 0.0));
    let body = serde_json::json!({
        "task_id": task.id.to_string(),
        "status": task.status.as_str(),
        "assigned_agent": task.assigned_agent,
        "created_at": task.created_at.to_rfc3339(),
        "updated_at": task.updated_at.to_rfc3339(),
        "progress": progress_list,
        "tokens_used": tokens_used,
        "cost_usd": cost_usd
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
    if let Some(v) = json.get("channel_context") {
        if v.is_null() {
            s.channel_context = None;
        } else if let Some(s_val) = v.as_str() {
            s.channel_context = Some(s_val.to_string());
        } else {
            return json_response("400 Bad Request", r#"{"error":"invalid_field_type","field":"channel_context"}"#);
        }
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

fn task_label(initial_message: Option<&String>, task_id: &Uuid) -> String {
    const LABEL_MAX: usize = 100;
    match initial_message {
        Some(m) if !m.trim().is_empty() => {
            let s = m.trim();
            if s.chars().count() > LABEL_MAX {
                format!("{}…", s.chars().take(LABEL_MAX).collect::<String>())
            } else {
                s.to_string()
            }
        }
        _ => {
            let s = task_id.to_string();
            let suffix = if s.len() >= 8 { &s[s.len() - 8..] } else { s.as_str() };
            format!("Tâche …{suffix}")
        }
    }
}

async fn get_calendar_events(store_path: &Path, path: &str) -> String {
    let query = path.split('?').nth(1).unwrap_or("");
    let from_ts = query
        .split('&')
        .find(|p| p.starts_with("from="))
        .and_then(|p| p.strip_prefix("from="))
        .and_then(|s| urlencoding::decode(s).ok())
        .and_then(|decoded| chrono::DateTime::parse_from_rfc3339(&decoded).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let to_ts = query
        .split('&')
        .find(|p| p.starts_with("to="))
        .and_then(|p| p.strip_prefix("to="))
        .and_then(|s| urlencoding::decode(s).ok())
        .and_then(|decoded| chrono::DateTime::parse_from_rfc3339(&decoded).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let (from_ts, to_ts) = match (from_ts, to_ts) {
        (Some(f), Some(t)) if f <= t => (f, t),
        _ => {
            let now = chrono::Utc::now();
            let start = now - chrono::Duration::days(7);
            (start, now)
        }
    };
    let task_store = TaskStore::open(store_path);
    let mut events: Vec<serde_json::Value> = Vec::new();
    if let Ok(schedule_store) = ScheduleStore::open(store_path) {
        if let Ok(runs) = schedule_store.list_task_runs_between(from_ts, to_ts, 500) {
            for r in runs {
                let at = r.started_at.unwrap_or(r.planned_for);
                let label = task_store
                    .as_ref()
                    .ok()
                    .and_then(|ts| ts.get(r.task_id).ok().flatten())
                    .map(|t| task_label(t.initial_message.as_ref(), &r.task_id))
                    .unwrap_or_else(|| task_label(None, &r.task_id));
                events.push(serde_json::json!({
                    "at": at.to_rfc3339(),
                    "task_id": r.task_id.to_string(),
                    "type": "run",
                    "status": r.status.as_str(),
                    "run_id": r.id.to_string(),
                    "planned_for": r.planned_for.to_rfc3339(),
                    "label": label,
                    "schedule_id": r.schedule_id.map(|u| u.to_string()),
                }));
            }
        }
    }
    if let Ok(ref task_store) = task_store {
        if let Ok(tasks) = task_store.list_tasks_created_between(from_ts, to_ts, 500) {
            for t in tasks {
                if t.parent_task_id.is_none() {
                    let label = task_label(t.initial_message.as_ref(), &t.id);
                    events.push(serde_json::json!({
                        "at": t.created_at.to_rfc3339(),
                        "task_id": t.id.to_string(),
                        "type": "ad_hoc",
                        "status": t.status.as_str(),
                        "label": label,
                    }));
                }
            }
        }
    }
    events.sort_by(|a, b| {
        let a_at = a.get("at").and_then(|v| v.as_str()).unwrap_or("");
        let b_at = b.get("at").and_then(|v| v.as_str()).unwrap_or("");
        a_at.cmp(b_at)
    });
    let body = serde_json::json!({ "events": events });
    json_response("200 OK", &body.to_string())
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
    let task_store = TaskStore::open(store_path);
    let task_map: std::collections::HashMap<Uuid, akasha_store::Task> = task_store
        .ok()
        .and_then(|ts| ts.get_all().ok())
        .unwrap_or_default()
        .into_iter()
        .map(|t| (t.id, t))
        .collect();
    let arr: Vec<serde_json::Value> = list
        .into_iter()
        .map(|r| {
            let label = task_map
                .get(&r.task_id)
                .map(|t| task_label(t.initial_message.as_ref(), &r.task_id))
                .unwrap_or_else(|| task_label(None, &r.task_id));
            serde_json::json!({
                "id": r.id.to_string(),
                "schedule_id": r.schedule_id.map(|u| u.to_string()),
                "task_id": r.task_id.to_string(),
                "status": r.status.as_str(),
                "planned_for": r.planned_for.to_rfc3339(),
                "started_at": r.started_at.map(|t| t.to_rfc3339()),
                "ended_at": r.ended_at.map(|t| t.to_rfc3339()),
                "dedup_key": r.dedup_key,
                "label": label,
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

#[cfg(test)]
mod tests {
    use super::{
        agent_role_system_prompt, build_image_markdown, ensure_no_open_code_block,
        message_suggests_tool_only_action, parse_content_length, parse_device_invoke_params,
    };

    #[test]
    fn parse_content_length_returns_header_end_and_content_length() {
        let buf = b"POST /api/message HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello";
        let r = parse_content_length(buf);
        assert!(r.is_some());
        let (header_end, content_length) = r.unwrap();
        assert_eq!(header_end, 45); // start of "\r\n\r\n" after "Content-Length: 5\r\n"
        assert_eq!(content_length, 5);
    }

    #[test]
    fn parse_content_length_no_separator_returns_none() {
        let buf = b"POST /api/message HTTP/1.1";
        assert!(parse_content_length(buf).is_none());
    }

    #[test]
    fn parse_content_length_empty_returns_none() {
        assert!(parse_content_length(&[]).is_none());
    }

    // --- parse_device_invoke_params ---

    fn s(v: &str) -> String { v.to_string() }

    #[test]
    fn device_invoke_params_no_extra_args_returns_empty_object() {
        let args: Vec<String> = vec![s("local_media"), s("camera"), s("capture")];
        let p = parse_device_invoke_params(&args);
        assert_eq!(p, serde_json::json!({}));
    }

    #[test]
    fn device_invoke_params_single_valid_json_arg() {
        let args = vec![s("synthetic_input"), s("keyboard"), s("shortcut"), s(r#"{"keys":["Control","C"]}"#)];
        let p = parse_device_invoke_params(&args);
        assert_eq!(p, serde_json::json!({"keys": ["Control", "C"]}));
    }

    #[test]
    fn device_invoke_params_single_invalid_json_falls_back_to_empty_object() {
        let args = vec![s("local_media"), s("microphone"), s("record"), s("not-json")];
        let p = parse_device_invoke_params(&args);
        assert_eq!(p, serde_json::json!({}));
    }

    #[test]
    fn device_invoke_params_multiple_args_become_json_array() {
        let args = vec![s("synthetic_input"), s("keyboard"), s("type"), s("hello"), s("world")];
        let p = parse_device_invoke_params(&args);
        assert_eq!(p, serde_json::json!(["hello", "world"]));
    }

    // --- message_suggests_tool_only_action ---

    #[test]
    fn tool_only_action_true_for_camera() {
        assert!(message_suggests_tool_only_action("Prends une photo avec la caméra"));
        assert!(message_suggests_tool_only_action("Take a photo from the webcam"));
    }

    #[test]
    fn tool_only_action_true_for_weather() {
        assert!(message_suggests_tool_only_action("Quelle est la météo à Paris ?"));
    }

    #[test]
    fn tool_only_action_true_for_save_file() {
        assert!(message_suggests_tool_only_action("Sauvegarde ce code dans /tmp/foo.py"));
    }

    #[test]
    fn tool_only_action_true_for_image_generation() {
        assert!(message_suggests_tool_only_action("Génère une image d'un lapin"));
    }

    #[test]
    fn tool_only_action_false_for_code_generation() {
        assert!(!message_suggests_tool_only_action("Écris un script Python qui lit un fichier"));
        assert!(!message_suggests_tool_only_action("Génère du code pour trier une liste"));
    }

    #[test]
    fn tool_only_action_false_when_code_intent_dominates() {
        // Explicit code request even if it mentions photo → do not override to conversation
        assert!(!message_suggests_tool_only_action("écris un script qui prend une photo"));
    }

    // --- agent_role_system_prompt ---

    #[test]
    fn agent_role_prompt_some_for_recognized_types() {
        let with_role = [
            "code", "search", "financial", "documentalist", "project_manager",
            "technical_writer", "research", "security_audit", "creative",
            "analyst", "architect", "frontend", "backend", "database", "integration", "qa", "system", "image_generation",
        ];
        for t in &with_role {
            let s = agent_role_system_prompt(t);
            assert!(s.is_some(), "agent_type {:?} should have a role prompt", t);
            assert!(!s.unwrap().is_empty(), "agent_type {:?} role prompt must be non-empty", t);
        }
    }

    #[test]
    fn agent_role_prompt_none_for_conversation_and_unknown() {
        assert!(agent_role_system_prompt("conversation").is_none());
        assert!(agent_role_system_prompt("").is_none());
        // schedule is handled specially (recurring task), no role prompt
        assert!(agent_role_system_prompt("schedule").is_none());
    }

    // --- ensure_no_open_code_block ---

    #[test]
    fn ensure_no_open_code_block_zero_backticks() {
        let s = "hello";
        assert_eq!(ensure_no_open_code_block(s), "hello");
    }

    #[test]
    fn ensure_no_open_code_block_one_backtick_open() {
        let s = "code:\n```";
        assert_eq!(ensure_no_open_code_block(s), "code:\n```\n```\n");
    }

    #[test]
    fn ensure_no_open_code_block_two_backticks_closed() {
        let s = "```\nfn x() {}\n```";
        assert_eq!(ensure_no_open_code_block(s), "```\nfn x() {}\n```");
    }

    // --- build_image_markdown ---

    #[test]
    fn build_image_markdown_data_url() {
        let url = "data:image/jpeg;base64,ABC";
        let out = build_image_markdown("Photo", url);
        assert!(out.contains("](<data:image/"), "output should contain ](<data:image/: {:?}", out);
        assert!(out.ends_with(">)"), "output should end with >): {:?}", out);
        assert_eq!(out, "\n\n![Photo](<data:image/jpeg;base64,ABC>)");
    }
}
