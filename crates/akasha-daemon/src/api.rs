//! Simple HTTP API: POST /api/message, GET /api/tasks/:id, GET / (health)

use crate::agent_profile::AgentProfile;
use crate::agents::{interpret_message, EventBus, OrchestratorTask, TaskPriority};
use crate::latency::{
    clear_task_milestones, emit_timeline_once_for_task, env_duration_ms, log_latency_metric,
    resolve_root_task_id,
};
use crate::memory::ShortTermStore;
use crate::memory_actor::LongTermMemoryClient;
use crate::protocol_adapter::unknown_external_message_count;
use crate::autonomous_mission_config::{
    merge_from_json_partial, persist_config_and_snapshot, AutonomousMissionConfig, Horizon,
    MissionStatusYaml,
};
use crate::user_profile::UserProfile;
use akasha_core::{EventEnvelope, EventType};
use akasha_llm::CompletionRequest;
pub use akasha_store::tasks::MAX_PROGRESS_PER_TASK;
use akasha_store::{
    format_todos_plan_block, parse_todos_from_payload, AutonomousMissionStore, Schedule,
    ScheduleException, ScheduleExceptionType, ScheduleStore, Task, TaskRunStatus, TaskStatus,
    TaskStore, TodoStatus, WorkspaceGraphStore,
};
use akasha_vault::Vault;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// On Windows, paths with verbatim prefix `\\?\` can cause "file not found" with some APIs. Return a path without it.
#[cfg(windows)]
fn strip_verbatim_prefix(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if s.starts_with(r"\\?\") {
        PathBuf::from(s.replace(r"\\?\", ""))
    } else {
        p
    }
}
#[cfg(not(windows))]
fn strip_verbatim_prefix(p: PathBuf) -> PathBuf {
    p
}

/// Normalize common Unicode apostrophes in filenames (e.g. ’ -> ').
/// LLM tool calls may use typographic quotes, while files on disk typically use ASCII quotes.
fn normalize_apostrophes(s: &str) -> String {
    s.replace('’', "'").replace('‘', "'")
}

fn normalize_tool_path_hint(raw: &str) -> String {
    let mut s = raw
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string();
    s = normalize_apostrophes(&s);
    if s.starts_with("workspace:/") {
        return format!(
            "workspace:/{}",
            s.trim_start_matches("workspace:/")
                .trim_start_matches(['/', '\\'])
        );
    }
    if let Some(rest) = s.strip_prefix("workspace:") {
        return format!("workspace:/{}", rest.trim_start_matches(['/', '\\']));
    }
    if let Some(rest) = s.strip_prefix("workspace/") {
        return format!("workspace:/{}", rest.trim_start_matches(['/', '\\']));
    }
    if let Some(rest) = s.strip_prefix("workspace\\") {
        return format!("workspace:/{}", rest.trim_start_matches(['/', '\\']));
    }
    s
}

fn is_workspace_virtual_path(raw: &str) -> bool {
    normalize_tool_path_hint(raw).starts_with("workspace:/")
}

/// True if `s` is `WxH` dimensions (e.g. 1024x1024).
fn looks_like_image_size_token(s: &str) -> bool {
    let s = s.trim();
    let mut parts = s.split('x');
    let (Some(w), Some(h)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    w.parse::<u32>().is_ok() && h.parse::<u32>().is_ok()
}

/// The tool layer splits `TOOL:` lines on whitespace, so multi-word prompts become many args.
/// Rejoin into a single prompt; treat a trailing `WxH` token as optional size.
fn parse_generate_image_tool_args(args: &[String]) -> (String, Option<String>) {
    if args.is_empty() {
        return (String::new(), None);
    }
    if args.len() >= 2 {
        if let Some(last) = args.last() {
            if looks_like_image_size_token(last) {
                return (
                    args[..args.len() - 1].join(" "),
                    Some(last.trim().to_string()),
                );
            }
        }
    }
    (args.join(" "), None)
}

fn parse_write_file_request(args: &[String]) -> Option<(String, String)> {
    if args.is_empty() {
        return None;
    }

    let joined = args.join("\n");
    let trimmed = joined.trim();
    if trimmed.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let path = value
                .get("path")
                .or_else(|| value.get("filePath"))
                .or_else(|| value.get("file"))
                .and_then(|v| v.as_str())
                .map(normalize_tool_path_hint);
            let content = value
                .get("content")
                .or_else(|| value.get("text"))
                .or_else(|| value.get("body"))
                .or_else(|| value.get("data"))
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Array(items) => items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    other => {
                        serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string())
                    }
                });
            if let Some(path) = path {
                return Some((path, content.unwrap_or_default()));
            }
        }
    }

    let path = normalize_tool_path_hint(args.first()?.as_str());
    let content = args.get(1..).map(|a| a.join("\n")).unwrap_or_default();
    Some((path, content))
}

/// Parse `memory_store` optional `link_to:` / `link_kind:` into `(target_uuid, relation_kind)` pairs.
/// Supports `link_to: uuid1+excludes,uuid2+relates_to` (per-target kind after `+`) or
/// `link_to: uuid1,uuid2` with `link_kind: relates_to` (one kind for all UUIDs; default `related`).
/// Repeated `link_to:` arguments append segments.
fn parse_memory_store_explicit_links(extra_args: &[String]) -> Option<Vec<(String, String)>> {
    let mut segments: Vec<String> = Vec::new();
    let mut global_kind: Option<String> = None;
    for arg in extra_args {
        if let Some(rest) = arg.strip_prefix("link_to:") {
            for part in rest.split(',') {
                let p = part.trim();
                if !p.is_empty() {
                    segments.push(p.to_string());
                }
            }
        } else if let Some(rest) = arg.strip_prefix("link_kind:") {
            let k = rest.trim();
            if !k.is_empty() {
                global_kind = Some(k.to_string());
            }
        }
    }
    if segments.is_empty() {
        return None;
    }
    let default_kind = global_kind.unwrap_or_else(|| "related".to_string());
    let mut out: Vec<(String, String)> = Vec::new();
    for seg in segments {
        let seg = seg.trim();
        if let Some(idx) = seg.rfind('+') {
            let uuid_part = seg[..idx].trim();
            let kind_part = seg[idx + 1..].trim();
            if Uuid::parse_str(uuid_part).is_ok() && !kind_part.is_empty() {
                out.push((uuid_part.to_string(), kind_part.to_string()));
                continue;
            }
        }
        if Uuid::parse_str(seg).is_ok() {
            out.push((seg.to_string(), default_kind.clone()));
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

enum PluginReputationResetBody {
    Empty,
    Parsed(serde_json::Value),
    Invalid(String),
}

fn parse_plugin_reputation_reset_body(body: Option<&[u8]>) -> PluginReputationResetBody {
    match body {
        Some(raw) if !raw.is_empty() => match serde_json::from_slice::<serde_json::Value>(raw) {
            Ok(v) => PluginReputationResetBody::Parsed(v),
            Err(e) => PluginReputationResetBody::Invalid(e.to_string()),
        },
        _ => PluginReputationResetBody::Empty,
    }
}

fn canonicalize_tool_name(tool_name: &str) -> String {
    match tool_name.to_lowercase().as_str() {
        "create_todos" => "write_todos".to_string(),
        "append_todos" => "merge_todos".to_string(),
        _ => tool_name.to_string(),
    }
}

/// Strips `--no-ignore`, `--no-gitignore`, `--regex`, `-r` from tool args (any position).
/// Returns filtered args, `respect_gitignore` (default true), `use_regex` (default false; grep only).
fn strip_file_search_flags(args: &[String]) -> (Vec<String>, bool, bool) {
    let mut respect_gitignore = true;
    let mut use_regex = false;
    let mut out = Vec::new();
    for a in args {
        match a.as_str() {
            "--no-ignore" | "--no-gitignore" => respect_gitignore = false,
            "--regex" | "-r" => use_regex = true,
            _ => out.push(a.clone()),
        }
    }
    (out, respect_gitignore, use_regex)
}

/// Banner injected by `compose_orchestrated_child_message(..., deliverables_required: true)` for subtasks and remediation.
const ORCH_DISK_DELIVERABLES_MARKER: &str = "[Orchestrated — disk deliverables REQUIRED]";

/// Resolve `workspace:/rel` or a normal filesystem path to a concrete disk path for tools that only call `read_dir` / globs on real paths.
/// True if path should be read as PDF (text extraction), not as UTF-8/plain text.
fn path_extension_is_pdf(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Read bytes from disk and return the same user-visible message shape as `pdf` tool.
async fn pdf_extract_message_from_disk(
    disk_path: &Path,
    via_tool: &str,
) -> (bool, String, Option<String>) {
    match tokio::fs::read(disk_path).await {
        Ok(bytes) => match pdf_extract::extract_text_from_mem(&bytes) {
            Ok(text) => {
                let preview = if text.len() > 2000 {
                    format!("{}…", text.chars().take(2000).collect::<String>())
                } else {
                    text.clone()
                };
                (
                    true,
                    format!(
                        "[{}; PDF→text {}] extracted {} chars:\n{}",
                        via_tool,
                        disk_path.display(),
                        text.len(),
                        preview
                    ),
                    None,
                )
            }
            Err(e) => (
                false,
                format!(
                    "[{}] PDF extraction failed for {}: {}",
                    via_tool,
                    disk_path.display(),
                    e
                ),
                None,
            ),
        },
        Err(e) => (
            false,
            format!(
                "[{}] read failed for {}: {}",
                via_tool,
                disk_path.display(),
                e
            ),
            None,
        ),
    }
}

fn resolve_tool_disk_path(raw: &str, workspace_root: Option<&Path>) -> PathBuf {
    let raw = normalize_tool_path_hint(raw);
    let raw = raw.trim();
    if raw.starts_with("workspace:/") || raw.starts_with("workspace:") {
        let key = raw
            .trim_start_matches("workspace:/")
            .trim_start_matches("workspace:")
            .trim_start_matches('/');
        let key = normalize_apostrophes(key);
        strip_verbatim_prefix(
            workspace_root
                .map(|root| root.join(&key))
                .or_else(|| std::env::current_dir().ok().map(|cwd| cwd.join(&key)))
                .unwrap_or_else(|| Path::new(&key).to_path_buf()),
        )
    } else {
        strip_verbatim_prefix(Path::new(raw).to_path_buf())
    }
}

use std::collections::{BinaryHeap, VecDeque};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::RwLock;
use tracing::Instrument;
use uuid::Uuid;

/// In-memory cache for AgentProfile to avoid repeated disk reads (invalidated on POST /api/agent-profile and after profile save in run_message_via_llm).
pub type AgentProfileCache = Arc<RwLock<Option<AgentProfile>>>;

/// Virtual workspace per task (Deep Agents-style). Paths prefixed with "workspace:/" or "workspace:" are read/written here instead of disk.
pub type TaskWorkspaceStore =
    Arc<RwLock<std::collections::HashMap<Uuid, std::collections::HashMap<String, String>>>>;

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
    g.release_notes_url = data
        .get("release_notes_url")
        .and_then(|v| v.as_str())
        .map(String::from);
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
static EXTRACT_SEMAPHORE: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> =
    std::sync::OnceLock::new();

fn extract_semaphore() -> Arc<tokio::sync::Semaphore> {
    EXTRACT_SEMAPHORE
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
        .clone()
}

async fn get_task_list(store_path: &Path, status_filter: Option<String>) -> String {
    let store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let mut tasks = match store.get_all() {
        Ok(t) => t,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    if let Some(ref status) = status_filter {
        let status = status.trim().to_lowercase();
        if !status.is_empty() {
            tasks.retain(|t| t.status.as_str() == status);
        }
    }
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

async fn get_task_events(store_path: &Path, events: &EventsCache, id: Uuid) -> String {
    let mut list: Vec<TaskEventEntry> = {
        let mut root_events = Vec::new();
        match TaskStore::open(store_path) {
            Ok(store) => {
                if let Ok(persisted) = store.get_events(id) {
                    root_events.extend(persisted.into_iter().map(|e| TaskEventEntry {
                        event_type: e.event_type,
                        payload: e.payload,
                        at: e.at,
                        task_id: Some(id.to_string()),
                    }));
                }
            }
            Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
        }
        let g = events.read().await;
        if let Some(q) = g.get(&id) {
            root_events.extend(q.iter().cloned());
        }
        root_events
    };

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
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if !child_ids.is_empty() {
        let g = events.read().await;
        for child_id in child_ids {
            if let Some(q) = g.get(&child_id) {
                list.extend(q.iter().cloned());
            }
        }
        drop(g);
        if let Ok(store) = TaskStore::open(store_path) {
            for child_id in list
                .iter()
                .filter(|e| e.event_type == "sub_agent_spawned")
                .filter_map(|e| {
                    e.payload
                        .as_ref()
                        .and_then(|p| p.get("task_id"))
                        .and_then(|v| v.as_str())
                        .and_then(|s| Uuid::parse_str(s).ok())
                })
                .collect::<std::collections::BTreeSet<_>>()
            {
                if let Ok(persisted) = store.get_events(child_id) {
                    list.extend(persisted.into_iter().map(|e| TaskEventEntry {
                        event_type: e.event_type,
                        payload: e.payload,
                        at: e.at,
                        task_id: Some(child_id.to_string()),
                    }));
                }
            }
        }
    }

    let mut seen = std::collections::HashSet::new();
    list.retain(|entry| {
        let payload_key = entry
            .payload
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap_or_default())
            .unwrap_or_default();
        let key = format!(
            "{}|{}|{}|{}",
            entry.task_id.as_deref().unwrap_or_default(),
            entry.event_type,
            entry.at,
            payload_key
        );
        seen.insert(key)
    });
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

pub type EventsCache = Arc<RwLock<std::collections::HashMap<Uuid, VecDeque<TaskEventEntry>>>>;

/// Heap entry used for `/api/timeline` bounded min-heap of recent events.
/// Ordering uses `at_ms` (milliseconds since Unix epoch) to avoid relying on
/// lexicographic comparison of RFC3339 strings, which can be unreliable when
/// `to_rfc3339()` omits fractional seconds for timestamps at whole-second boundaries.
struct TimelineHeapEntry {
    at: String,
    /// Milliseconds since Unix epoch parsed from `at`; used for all comparisons.
    at_ms: i64,
    counter: usize,
    task_id: String,
    event_type: String,
    payload: Option<serde_json::Value>,
}

impl PartialEq for TimelineHeapEntry {
    fn eq(&self, other: &Self) -> bool {
        (self.at_ms, self.counter) == (other.at_ms, other.counter)
    }
}

impl Eq for TimelineHeapEntry {}

impl PartialOrd for TimelineHeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimelineHeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // We want the *oldest* event to be considered "greatest" so that
        // `BinaryHeap::pop()` removes the oldest when the heap exceeds `limit`.
        (other.at_ms, other.counter).cmp(&(self.at_ms, self.counter))
    }
}

pub fn new_progress_cache() -> ProgressCache {
    Arc::new(RwLock::new(std::collections::HashMap::new()))
}

pub fn new_events_cache() -> EventsCache {
    Arc::new(RwLock::new(std::collections::HashMap::new()))
}

/// Per-session result cell for background commands. The spawned task writes the result when done.
type BackgroundResultCell =
    Arc<RwLock<Option<anyhow::Result<(std::process::Output, akasha_tools::ToolResult)>>>>;

/// Registry of background command sessions: session_id -> (task handle to abort, result cell).
pub type ProcessRegistry = Arc<
    RwLock<std::collections::HashMap<Uuid, (tokio::task::JoinHandle<()>, BackgroundResultCell)>>,
>;

pub fn new_process_registry() -> ProcessRegistry {
    Arc::new(RwLock::new(std::collections::HashMap::new()))
}

/// Registry of task-completion notifiers: task_id → Notify. Written by the conversation worker
/// on task completion; awaited by run_delegation_handler and the orchestrator aggregator so they
/// can react immediately instead of polling TaskStore every 500 ms.
pub type TaskCompletionRegistry =
    Arc<RwLock<std::collections::HashMap<Uuid, Arc<tokio::sync::Notify>>>>;

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
                let _ = req
                    .reply_tx
                    .send(Err("Système surchargé, réessayez plus tard.".to_string()));
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
                let _ = req
                    .reply_tx
                    .send(Err("requesting task not found".to_string()));
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
                    let _ = req.reply_tx.send(Err(
                        "max delegation depth (sous-sous-agent non autorisé)".to_string(),
                    ));
                    continue;
                }
            }
        }
        // Build a delegation message that always includes the root user request context,
        // so child agents don't lose intent when the delegating LLM emits a terse/ambiguous subtask.
        let root_user_request = {
            let mut current = requesting.clone();
            let mut depth = 0usize;
            let mut root = current.initial_message.clone().unwrap_or_default();
            while let Some(pid) = current.parent_task_id {
                if depth >= 8 {
                    break;
                }
                match store.get(pid) {
                    Ok(Some(parent)) => {
                        if let Some(msg) = parent.initial_message.clone() {
                            if !msg.trim().is_empty() {
                                root = msg;
                            }
                        }
                        current = parent;
                        depth += 1;
                    }
                    _ => break,
                }
            }
            root
        };
        let delegated_message = if req.message.trim_start().starts_with("[Task]\n") {
            req.message.clone()
        } else {
            let root_trim = root_user_request.trim();
            if root_trim.is_empty() {
                req.message.clone()
            } else {
                format!(
                    "[Parent user request]\n{}\n\n[Delegated subtask]\n{}",
                    root_trim,
                    req.message.trim()
                )
            }
        };
        let child_id = Uuid::new_v4();
        const WORKER_AGENT_TYPES: &[&str] = &[
            "search",
            "code",
            "conversation",
            "financial",
            "documentalist",
            "project_manager",
            "technical_writer",
            "research",
            "security_audit",
            "creative",
        ];
        let agent_type = if WORKER_AGENT_TYPES.contains(&req.agent_type.as_str()) {
            req.agent_type.clone()
        } else {
            "conversation".to_string()
        };
        const MAX_INITIAL_MSG: usize = 500;
        let initial_message = if delegated_message.chars().count() > MAX_INITIAL_MSG {
            Some(
                delegated_message
                    .chars()
                    .take(MAX_INITIAL_MSG)
                    .chain(std::iter::once('…'))
                    .collect::<String>(),
            )
        } else if delegated_message.is_empty() {
            None
        } else {
            Some(delegated_message.clone())
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
                message: delegated_message,
                session_id: String::new(),
                image_data_urls: None,
                execution_mode: None,
                preferred_task_type: None,
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
            let first_activity_timeout =
                env_duration_ms("AKASHA_DELEGATION_FIRST_ACTIVITY_TIMEOUT_MS", 15_000);
            let completion_timeout =
                env_duration_ms("AKASHA_DELEGATION_COMPLETION_TIMEOUT_MS", 300_000);
            let had_first_activity =
                wait_for_task_activity(&progress, child_id_span, first_activity_timeout).await;
            if !had_first_activity {
                task_completion.write().await.remove(&child_id_span);
                let _ = reply_tx.send(Err(format!(
                    "delegation startup timeout ({} ms without visible activity)",
                    first_activity_timeout.as_millis()
                )));
                return;
            }
            let timed_out =
                tokio::time::timeout(completion_timeout, notify.notified().instrument(span))
                    .await
                    .is_err();
            // Ensure the registry entry is removed regardless of outcome.
            task_completion.write().await.remove(&child_id_span);
            if timed_out {
                let _ = reply_tx.send(Err(format!(
                    "delegation completion timeout ({} ms)",
                    completion_timeout.as_millis()
                )));
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
                    .unwrap_or_else(|| {
                        if matches!(task.status, TaskStatus::Failed) {
                            "Échec.".to_string()
                        } else {
                            "Terminé.".to_string()
                        }
                    })
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
pub fn parse_request(
    buf: &[u8],
) -> (
    String,
    String,
    Option<Vec<u8>>,
    std::collections::HashMap<String, String>,
) {
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
        let line_str = String::from_utf8_lossy(line)
            .trim_end_matches('\r')
            .to_string();
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
    const MAX_BODY_PARSE: usize = 10 * 1024 * 1024; // 10 MiB — refuse to allocate larger body
    let will_allocate =
        content_length > 0 && content_length <= MAX_BODY_PARSE && rest.len() >= content_length;
    let body = if will_allocate {
        Some(rest[..content_length].to_vec())
    } else {
        None
    };
    (method, path, body, headers)
}

pub fn json_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    )
}

/// Liste des outils disponibles (source unique pour le prompt et la doc).
/// Format: une ligne par outil "nom — usage".
/// Note: "Session terminal" (spec 33) est optionnel et prévu pour une version ultérieure.
pub const AVAILABLE_TOOLS: &[(&str, &str)] = &[
    ("read_file", "read_file <path> — lire le contenu d'un fichier texte. Pour les fichiers .pdf, le texte est extrait automatiquement (équivalent à pdf <path>) ; ne vous attendez pas au binaire PDF. Pour les gros fichiers, lire d'abord le fichier puis cibler seulement les sections utiles avec grep_content/search_files avant d'éditer. Path réel ou workspace:/<path> pour le workspace virtuel de la tâche."),
    ("write_file", "write_file <path> <content> — écrire du texte dans un fichier (création/remplacement complet). Préférer workspace:/<fichier> si l'utilisateur n'a pas donné de chemin (ex. workspace:/script.py). TOUJOURS utiliser le chemin EXACT fourni par l'utilisateur. Si le fichier existe déjà et qu'il faut modifier une partie, préférer edit_file ou search_replace plutôt que de tout réécrire. Path réel (Windows/Unix) ou workspace:/ pour le workspace virtuel."),
    ("search_files", "search_files <dir> <pattern> [--no-ignore] — chercher des fichiers (glob) sous un répertoire ; par défaut respecte .gitignore et ignore node_modules/target/dist/… ; --no-ignore pour tout parcourir."),
    ("grep_content", "grep_content <dir> <pattern> [file_glob] [--regex|-r] [--no-ignore] — chercher dans les fichiers ; défaut = sous-chaîne insensible à la casse + .gitignore ; --regex = motif regex insensible à la casse ; --no-ignore = ignorer .gitignore."),
    ("run_command", "run_command [--cwd <path>] <cmd> [arg1 arg2 ...] — exécuter une commande (autorisée par la politique). Optionnel : --cwd workspace:/ ou chemin disque (allowed_read_paths). Si tools_policy run_command_default_cwd_workspace: true, cwd par défaut = workspace de la tâche. Pour GitHub depuis le shell, préférer gh-axi (npm install -g gh-axi ; principes AXI https://axi.md/) s'il est installé — sorties compactes pour l'agent. Pour l'automation navigateur en CLI, chrome-devtools-axi (même dépôt https://github.com/kunchenguid/axi) en complément d'Akasha browser."),
    ("run_terminal", "run_terminal [--cwd <path>] <cmd> [args...] — exécuter une commande (même que run_command)"),
    ("run_command_background", "run_command_background [--cwd <path>] <cmd> [args...] — lancer en arrière-plan, retourne session_id pour process poll/kill"),
    ("terminal_session", "terminal_session — session PTY interactive (prévue ultérieurement, spec 43). Pour l’instant utiliser run_command / run_terminal pour une commande, run_command_background + process pour suivi."),
    ("process", "process list | process poll <session_id> | process kill <session_id> — lister, consulter ou arrêter des commandes en arrière-plan"),
    ("file_diff", "file_diff <path_a> <path_b> — diff texte entre deux fichiers (ligne à ligne)"),
    ("diff_unified", "diff_unified <path_a> <path_b> [context_lines] — diff unifié style patch (défaut context_lines=3) ; chemins réels ou workspace:/"),
    ("dir_compare", "dir_compare <dir_a> <dir_b> [max_depth] [max_files] — comparer deux arborescences (fichiers uniquement dans A/B, contenu différent) ; profondeur et nombre de fichiers bornés (défaut 8 et 100)"),
    ("git_status", "git_status <repo> — git status --porcelain=v1 -b dans le dépôt (nécessite git dans allowed_commands)"),
    ("git_diff", "git_diff <repo> [--staged] [pathspec...] — git diff ; pathspecs relatifs au dépôt, validés sous la racine"),
    ("git_log", "git_log <repo> [n] — git log -n N --oneline (N entre 1 et 100, défaut 20)"),
    ("git_rev_parse", "git_rev_parse <repo> — git rev-parse HEAD"),
    ("edit_file", "edit_file <path> <start_line> <end_line> <new_content> — remplacer les lignes start..end par new_content (lignes 1-based)"),
    ("apply_patch", "apply_patch <path> <patch_content> — appliquer un patch unifié (contenu du patch après le path)"),
    ("search_replace", "search_replace <path> <search> | <replace> — remplacer toutes les occurrences de search par replace dans le fichier (séparateur \" | \")"),
    ("web_fetch", "web_fetch <url> — récupérer le contenu d'une URL (domaine autorisé dans tools_policy allowed_web_domains)"),
    ("web_search", "web_search <query> [max_results] — rechercher sur le web (Brave API; BRAVE_API_KEY, web_search_enabled)"),
    ("run_in_container", "run_in_container <work_dir> <image> <command> [args...] — exécuter une commande dans un conteneur (work_dir autorisé en lecture, ex. node:20 node index.js)"),
    ("memory_search", "memory_search <query> [top_k] — rechercher dans la mémoire long terme (si activée)"),
    ("workspace_graph_search", "workspace_graph_search <query> [--workspace <uuid>] — rechercher dans les graphes projet indexés (nœuds label/chemin) ; limite ~20 lignes ; --workspace pour un espace enregistré uniquement"),
    ("memory_store", "memory_store <content> <source> [link_to: uuid1+kind1,uuid2+kind2,...] [link_kind: default_kind] — mémoire long terme. Types recommandés : similar, relates_to, related, updates, supersedes, excludes, contradicts, supports, derived_from, same_as, spouse, child, birth_date, … ; par cible utiliser uuid+kind, ou uuid seuls avec link_kind (défaut related)."),
    ("memory_delete", "memory_delete <id> — supprimer une entrée de la mémoire long terme par son id (UUID)"),
    ("memory_forget", "memory_forget <query> — supprimer les entrées dont le contenu correspond aux mots-clés (plan moyen terme 9)"),
    ("memory_stats", "memory_stats — nombre d'entrées et taille approximative de la mémoire long terme"),
    ("memory_gc", "memory_gc [retention_days] [protect_sources...] — supprimer les entrées plus anciennes que N jours (sources protégées optionnelles, ex. user_fact project)"),
    ("sessions_list", "sessions_list [limit] — lister les tâches/sessions récentes"),
    ("sessions_spawn", "sessions_spawn <message> [session_id] — créer une sous-tâche et la lancer"),
    ("session_status", "session_status <task_id> — statut d'une tâche donnée"),
    ("message", "message send <channel> <text> — envoyer un message vers un canal (webhook configuré via AKASHA_MESSAGE_WEBHOOK_URL)"),
    ("browser", "browser navigate <url> — open URL in managed browser (http/https; domain allowed by tools_policy). browser snapshot — text + links of current page. Phase 2: click, fill, screenshot, wait (see spec 39)."),
    ("install_playwright", "install_playwright — run npm install and npx playwright install chromium in the Playwright runner directory (scripts/playwright-runner or AKASHA_PLAYWRIGHT_RUNNER). Requires browser_enabled. Use after ask_user consent if you need explicit approval before download; optional require_approval in tools_policy."),
    ("image", "image <path|url> [prompt] — vision: joindre l'image en pièce jointe au chat (modèle vision dans llm_router)"),
    ("pdf", "pdf <path> — extraire le texte d'un PDF (path dans allowed_read_paths)"),
    ("ask_user", "ask_user — demande une information à l'utilisateur (human in the loop). Ligne suivante : JSON avec question (requis), context (optionnel), choices (optionnel, tableau de chaînes pour choix multiples). Pour une réponse ouverte (chemin, texte libre, secret), omettre choices ou laisser un tableau vide. Si choices est fourni, l'UI propose quand même une saisie libre en plus des boutons. Exemple : {\"question\":\"Quel fichier ?\",\"context\":\"...\",\"choices\":[\"a.txt\",\"b.txt\"]}"),
    ("delegate_to_agent", "delegate_to_agent <agent_type> <message> — déléguer à un sous-agent. agent_type: search | code | conversation | financial | documentalist | project_manager | technical_writer | research | security_audit | creative | analyst | architect | frontend | backend | database | integration | qa | system | image_generation. Un seul niveau de délégation autorisé."),
    ("install_skill", "install_skill <url> — installer un skill depuis une URL GitHub (ex. https://github.com/BankrBot/skills/tree/main/bankr). Télécharge SKILL.md, l'enregistre dans le dossier skills, puis recharge les skills."),
    ("uninstall_skill", "uninstall_skill <name> — désinstaller un skill (supprime data_dir/skills/<name>, retire la commande de tools_policy si présente, recharge les skills)."),
    ("device_discover", "device_discover [interface] — lister les appareils accessibles (optionnel: local_media, system, network, usb). Filtre par politique allowed_device_interfaces / blocked_device_interfaces."),
    ("device_invoke", "device_invoke <interface> <device_id> <action> [params] — exécuter une action sur un appareil. local_media: caméra (device_id camera, action capture), micro (device_id microphone, action record). Appelle directement ; une fenêtre d'autorisation s'affichera dans l'UI. Ne pas demander à l'utilisateur d'« ouvrir l'UI » — utiliser l'outil. synthetic_input: device_id keyboard|mouse, action shortcut|key|type|mouse_move|mouse_click|..."),
    ("generate_image", "generate_image <prompt> [size] — générer une image par IA (ex. OpenAI DALL·E). Prompt en texte libre (plusieurs mots après generate_image sont un seul prompt) ; size optionnel en dernier (1024x1024, 512x512). Retourne l'image en data URL dans la réponse (spec 42)."),
    ("speech_synthesize", "speech_synthesize <text> — TTS: synthétiser le texte en audio (Kyutai Unmute/Pocket TTS). Retourne une data URL audio (voice_router.yaml tts.base_url)."),
    ("speech_transcribe", "speech_transcribe <data_url_audio> — STT: transcrire l'audio en texte. Passer la data URL de l'audio (ex. après device_invoke local_media microphone record). La data URL est obligatoire (voice_router.yaml stt.base_url)."),
    ("write_todos", "write_todos <payload> — définir la liste d'étapes (todo). Remplace toute la liste (plan initial ou re-découpage complet). Pour ajouter sans effacer : merge_todos. Payload: JSON array ou lignes."),
    ("merge_todos", "merge_todos <payload> — ajoute des étapes (même format que write_todos) sans supprimer les existantes ; titres déjà présents ignorés (casse insensible)."),
    ("read_todos", "read_todos — retourne la liste des étapes (todos) de la tâche courante."),
    ("update_todo", "update_todo <index> <status> — marquer l'étape à l'index (1-based) comme status (done, cancelled)."),
    ("list_skills", "list_skills — retourne la liste des skills installés (nom et description). Utiliser avant read_skill pour charger le détail d'un skill."),
    ("read_skill", "read_skill <name> — charge le contenu (instructions, usage) du skill. À utiliser quand tu as besoin du détail d'un skill avant de l'invoquer par son nom."),
    ("plugin.call", "plugin.call <plugin_id> <json_or_args...> — exécuter un plugin de type tool chargé dans le daemon. Exemple: TOOL: plugin.call maps {\"action\":\"distance\",\"from\":{\"lat\":45.698,\"lon\":0.328},\"to\":{\"lat\":49.009,\"lon\":2.547},\"mode\":\"car\"}"),
    ("maps_distance", "maps_distance <from_lat> <from_lon> <to_lat> <to_lon> [mode] — via plugin maps, calcule distance et durée estimée."),
    ("maps_route", "maps_route <from_lat> <from_lon> <to_lat> <to_lon> [mode] — via plugin maps, retourne un itinéraire simplifié avec geometry map-ready."),
    ("graph_plot", "graph_plot <chart> <y1> <y2> ... | plugin.call graph <json> — via plugin graph, génère une figure Plotly (line/bar/scatter/histogram)."),
    ("graph_stats", "graph_stats <json_or_args...> — via plugin graph, calcule min/max/moyenne/compte par série et retourne une vue table."),
    ("sim_run", "sim_run <initial> <growth_rate> <noise> <horizon> | plugin.call simulation <json> — via plugin simulation, exécute une simulation déterministe et retourne une vue timeseries + métriques."),
    ("sim_compare", "sim_compare <initial> <growth_rate> <noise> <horizon> | plugin.call simulation <json> — via plugin simulation, compare scénario de base et alternatif, retourne delta + tableau de résultats."),
];

fn available_tools_instruction(allowed_tools: Option<&[String]>) -> String {
    let iter: Box<dyn Iterator<Item = &(&str, &str)>> = if let Some(allowed) = allowed_tools {
        Box::new(AVAILABLE_TOOLS.iter().filter(move |(name, _)| {
            let always_misc = *name == "ask_user"
                || *name == "install_skill"
                || *name == "uninstall_skill"
                || *name == "write_todos"
                || *name == "merge_todos"
                || *name == "read_todos"
                || *name == "update_todo"
                || *name == "list_skills"
                || *name == "read_skill";
            let in_profile = allowed.iter().any(|a| a == *name);
            always_misc || in_profile
        }))
    } else {
        Box::new(AVAILABLE_TOOLS.iter())
    };
    iter.map(|(_, desc)| *desc).collect::<Vec<_>>().join(" ; ")
}

/// Single-pass intent flags for a message (one to_lowercase() shared by all checks).
struct MessageIntentFlags {
    save_file: bool,
    external_info: bool,
    /// User wants posts/timeline from X, Twitter, or similar (inject SOCIAL_FEED_REMINDER).
    social_feed_fetch: bool,
    camera_or_mic: bool,
    image_generation: bool,
    code_generation: bool,
    /// User asks for GitHub repo/API info and mentions vault or GITHUB_TOKEN.
    github_with_vault: bool,
    /// User asks about transport schedules, routes, or travel info (train, bus, flight, etc.)
    transport: bool,
    /// User asks for a geographic distance/route between two places.
    geolocation_distance: bool,
}

#[derive(Debug, Clone, Default)]
struct RuntimeToolRoutingEnforcer {
    preferred_tools: std::collections::HashSet<String>,
    forbidden_tools: std::collections::HashSet<String>,
}

impl RuntimeToolRoutingEnforcer {
    fn from_rules(rules: &[crate::plugins::registry::MatchedRoutingRule]) -> Option<Self> {
        if rules.is_empty() {
            return None;
        }
        let mut preferred_tools = std::collections::HashSet::new();
        let mut forbidden_tools = std::collections::HashSet::new();
        for rule in rules {
            for tool in &rule.preferred_tools {
                let t = tool.trim().to_lowercase();
                if !t.is_empty() {
                    preferred_tools.insert(t);
                }
            }
            for tool in &rule.forbidden_tools {
                let t = tool.trim().to_lowercase();
                if !t.is_empty() {
                    forbidden_tools.insert(t);
                }
            }
        }
        Some(Self {
            preferred_tools,
            forbidden_tools,
        })
    }

    fn is_tool_allowed(&self, tool_name: &str, args: &[String]) -> bool {
        let tool = canonicalize_tool_name(tool_name).to_lowercase();

        if self.is_forbidden(&tool, args) {
            return false;
        }

        // If no preferred list is declared, only forbidden list is enforced.
        if self.preferred_tools.is_empty() {
            return true;
        }

        // Always allow ask_user to unblock missing parameters.
        if tool == "ask_user" {
            return true;
        }

        if self.preferred_tools.contains(&tool) {
            return true;
        }

        // Allow plugin.call / plugin.<id> when it targets a preferred plugin/tool family.
        if tool == "plugin.call" || tool == "plugin_call" {
            if let Some(plugin_id) = args.first().map(|s| s.trim().to_lowercase()) {
                if self.preferred_tools.contains(&plugin_id)
                    || self
                        .preferred_tools
                        .iter()
                        .any(|p| p.starts_with(&(plugin_id.clone() + "_")))
                {
                    return true;
                }
            }
        }

        if let Some(plugin_id) = tool.strip_prefix("plugin.") {
            let plugin_id = plugin_id.trim().to_lowercase();
            if self.preferred_tools.contains(&plugin_id)
                || self
                    .preferred_tools
                    .iter()
                    .any(|p| p.starts_with(&(plugin_id.clone() + "_")))
            {
                return true;
            }
        }

        false
    }

    fn is_forbidden(&self, tool_name: &str, args: &[String]) -> bool {
        if self.forbidden_tools.is_empty() {
            return false;
        }
        if self.forbidden_tools.contains(tool_name) {
            return true;
        }
        if (tool_name == "plugin.call" || tool_name == "plugin_call")
            && args
                .first()
                .map(|s| self.forbidden_tools.contains(&s.trim().to_lowercase()))
                .unwrap_or(false)
        {
            return true;
        }
        false
    }
}

fn active_intents_from_flags(flags: &MessageIntentFlags) -> Vec<&'static str> {
    let mut out = Vec::new();
    if flags.save_file {
        out.push("save_file");
    }
    if flags.external_info {
        out.push("external_info");
    }
    if flags.social_feed_fetch {
        out.push("social_feed_fetch");
    }
    if flags.camera_or_mic {
        out.push("camera_or_mic");
    }
    if flags.image_generation {
        out.push("image_generation");
    }
    if flags.code_generation {
        out.push("code_generation");
    }
    if flags.github_with_vault {
        out.push("github_with_vault");
    }
    if flags.transport {
        out.push("transport");
    }
    if flags.geolocation_distance {
        out.push("geolocation_distance");
    }
    out
}

fn code_studio_disable_plugin_intents(flags: &mut MessageIntentFlags) {
    // Code Studio requests are code/project scoped; plugin routing intents for travel/geolocation
    // can hijack tool selection and force unrelated plugins.
    flags.transport = false;
    flags.geolocation_distance = false;
}

fn build_plugin_routing_reminders(
    plugin_registry: Option<&std::sync::Arc<crate::plugins::PluginRegistry>>,
    message: &str,
    flags: &MessageIntentFlags,
    tools_executor_snapshot: Option<&std::sync::Arc<akasha_tools::ToolExecutor>>,
) -> (String, bool) {
    let Some(registry) = plugin_registry else {
        return (String::new(), false);
    };
    let intents = active_intents_from_flags(flags);
    if intents.is_empty() {
        return (String::new(), false);
    }

    let rules = registry.match_routing_rules(message, &intents, |tool_name| {
        tools_executor_snapshot
            .as_ref()
            .map(|e| e.policy.can_use_tool(tool_name))
            .unwrap_or(false)
    });
    if rules.is_empty() {
        return (String::new(), false);
    }

    let matched_plugins: Vec<String> = rules.iter().map(|r| r.plugin_id.clone()).collect();
    tracing::info!(
        intents = ?intents,
        matched_rules = rules.len(),
        plugins = ?matched_plugins,
        "Dynamic plugin routing rules matched"
    );

    let mut seen = std::collections::HashSet::new();
    let mut lines = Vec::new();
    let mut geolocation_handled = false;
    for rule in rules.into_iter().take(4) {
        if rule
            .intent
            .as_deref()
            .is_some_and(|intent| intent.eq_ignore_ascii_case("geolocation_distance"))
        {
            geolocation_handled = true;
        }
        let instruction = rule.instruction.trim();
        if instruction.is_empty() {
            continue;
        }
        if seen.insert(instruction.to_string()) {
            lines.push(format!("- {}", instruction));
        }
    }
    if lines.is_empty() {
        return (String::new(), geolocation_handled);
    }

    let block = format!(
        "\n[Dynamic plugin routing rules — auto-loaded from installed plugin manifests]\n{}\n\n",
        lines.join("\n")
    );
    (block, geolocation_handled)
}

fn build_runtime_tool_routing_enforcer(
    plugin_registry: Option<&std::sync::Arc<crate::plugins::PluginRegistry>>,
    message: &str,
    flags: &MessageIntentFlags,
    tools_executor_snapshot: Option<&std::sync::Arc<akasha_tools::ToolExecutor>>,
) -> Option<RuntimeToolRoutingEnforcer> {
    let registry = plugin_registry?;
    let intents = active_intents_from_flags(flags);
    if intents.is_empty() {
        return None;
    }

    let rules = registry.match_routing_rules(message, &intents, |tool_name| {
        tools_executor_snapshot
            .as_ref()
            .map(|e| e.policy.can_use_tool(tool_name))
            .unwrap_or(false)
    });

    let enforcer = RuntimeToolRoutingEnforcer::from_rules(&rules)?;
    tracing::info!(
        intents = ?intents,
        preferred_tools = ?enforcer.preferred_tools,
        forbidden_tools = ?enforcer.forbidden_tools,
        "Runtime tool routing enforcement enabled"
    );
    Some(enforcer)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SmallTalkLanguage {
    French,
    English,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SmallTalkIntent {
    language: SmallTalkLanguage,
    asks_status: bool,
}

fn classify_small_talk_message(message: &str) -> Option<SmallTalkIntent> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed
        .to_lowercase()
        .replace('’', "'")
        .replace(['!', '?', '.', ',', ';', ':'], " ");
    let collapsed = lower.split_whitespace().collect::<Vec<_>>().join(" ");

    // Reject messages that mingle small-talk with "real request" keywords
    let real_request_keywords = [
        "peux tu",
        "peux-tu",
        "peut tu",
        "peut-tu",
        "pouvez vous",
        "pouvez-vous",
        "tu peux",
        "peux me",
        "peux nous",
        "could you",
        "can you",
        "would you",
        "can you help",
        "can you tell",
        "me rappeler",
        "me rappelle",
        "help me",
        "show me",
        "tell me",
        "lire",
        "read",
        "fichier",
        "file",
        "faire",
        "do",
        "créer",
        "create",
        "écrire",
        "write",
        "générer",
        "generate",
        "projet",
        "project",
        "avait",
        "avaient",
        "avez",
        "have",
        "has",
        "fait",
        "done",
        "hier",
        "yesterday",
        "dernier",
        "last",
    ];
    for keyword in real_request_keywords {
        if collapsed.contains(keyword) {
            return None;
        }
    }

    let has_french = [
        "salut",
        "bonjour",
        "bonsoir",
        "coucou",
        "ca va",
        "ça va",
        "comment ca va",
        "comment ça va",
    ]
    .iter()
    .any(|k| collapsed.contains(k));
    let has_english = [
        "hello",
        "hi",
        "hey",
        "good morning",
        "good evening",
        "how are you",
        "hows it going",
        "how's it going",
    ]
    .iter()
    .any(|k| collapsed.contains(k));
    let asks_status = [
        "ca va",
        "ça va",
        "comment ca va",
        "comment ça va",
        "how are you",
        "hows it going",
        "how's it going",
    ]
    .iter()
    .any(|k| collapsed.contains(k));
    if has_french {
        Some(SmallTalkIntent {
            language: SmallTalkLanguage::French,
            asks_status,
        })
    } else if has_english {
        Some(SmallTalkIntent {
            language: SmallTalkLanguage::English,
            asks_status,
        })
    } else {
        None
    }
}

fn small_talk_fast_lane(message: &str) -> Option<SmallTalkIntent> {
    let intent = classify_small_talk_message(message)?;
    let trimmed = message.trim();
    if trimmed.contains('\n') || trimmed.chars().count() > 80 {
        return None;
    }
    let word_count = trimmed
        .split_whitespace()
        .filter(|w| {
            !w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-')
                .is_empty()
        })
        .count();
    if word_count > 8 {
        return None;
    }
    let lower = trimmed.to_lowercase();
    let flags = compute_message_intent_flags(trimmed);
    if flags.save_file
        || flags.external_info
        || flags.social_feed_fetch
        || flags.camera_or_mic
        || flags.image_generation
        || flags.code_generation
        || flags.github_with_vault
        || message_suggests_project(trimmed)
    {
        return None;
    }
    if [
        "fichier",
        "file",
        "code",
        "projet",
        "project",
        "browser",
        "navigateur",
        "outil",
        "tool",
        "github",
        "api",
        "cargo",
        "rust",
        "erreur",
        "error",
        "bug",
    ]
    .iter()
    .any(|k| lower.contains(k))
    {
        return None;
    }
    Some(intent)
}

fn small_talk_fast_reply(message: &str, intent: SmallTalkIntent) -> String {
    let lower = message.to_lowercase();
    match intent.language {
        SmallTalkLanguage::French => {
            if intent.asks_status {
                "Salut ! Oui, ça va bien 😊 Et toi ?".to_string()
            } else if lower.contains("bonsoir") {
                "Bonsoir ! 👋".to_string()
            } else if lower.contains("bonjour") {
                "Bonjour ! 👋".to_string()
            } else if lower.contains("coucou") {
                "Coucou ! 👋".to_string()
            } else {
                "Salut ! 👋".to_string()
            }
        }
        SmallTalkLanguage::English => {
            if intent.asks_status {
                "Hi! I'm doing well 😊 How about you?".to_string()
            } else if lower.contains("good morning") {
                "Good morning! 👋".to_string()
            } else if lower.contains("good evening") {
                "Good evening! 👋".to_string()
            } else {
                "Hi! 👋".to_string()
            }
        }
    }
}

fn response_looks_off_topic_for_small_talk(response: &str) -> bool {
    let lower = response.to_lowercase();
    lower.contains("tool:")
        || lower.contains("tools_policy")
        || lower.contains("allowed_write_paths")
        || lower.contains("allowed_read_paths")
        || lower.contains("write_file")
        || lower.contains("read_file")
        || lower.contains("browser navigate")
        || lower.contains("web_search")
        || lower.contains("memory_store")
        || lower.contains("delegate_to_agent")
        || lower.contains("vault:")
        || lower.len() > 240
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionRecallRange {
    Yesterday,
    CurrentDay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionRecallIntent {
    range: SessionRecallRange,
    language: SmallTalkLanguage,
}

fn detect_session_recall_intent(message: &str) -> Option<SessionRecallIntent> {
    let lower = message
        .trim()
        .to_lowercase()
        .replace('’', "'")
        .replace(['!', '?', '.', ',', ';', ':'], " ");
    if lower.is_empty() {
        return None;
    }
    let asks_recall = [
        "rappeler",
        "rappelle",
        "rappel",
        "ce qu'on a fait",
        "ce qu on a fait",
        "on a fait",
        "what we did",
        "remind me",
        "recap",
        "recap what we did",
        "what did we do",
    ]
    .iter()
    .any(|k| lower.contains(k));
    if !asks_recall {
        return None;
    }
    let range = if ["hier", "yesterday", "last night", "hier soir"]
        .iter()
        .any(|k| lower.contains(k))
    {
        SessionRecallRange::Yesterday
    } else {
        SessionRecallRange::CurrentDay
    };
    let language = if [
        "bonjour", "salut", "merci", "hier", "rappeler", "qu'on", "quoi",
    ]
    .iter()
    .any(|k| lower.contains(k))
    {
        SmallTalkLanguage::French
    } else {
        SmallTalkLanguage::English
    };
    Some(SessionRecallIntent { range, language })
}

fn build_session_recap_reply(
    turns: &[crate::memory::ConversationTurn],
    intent: SessionRecallIntent,
) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for t in turns {
        let c = t.content.trim();
        if c.is_empty() {
            continue;
        }
        let lower = c.to_lowercase();
        // Filter out system/error messages
        if lower.contains("tools_policy")
            || lower.contains("allowed_write_paths")
            || lower.contains("allowed_read_paths")
            || lower.contains("allowed_paths")
            || lower.contains("llm response timed out")
            || lower.contains("timed out")
            || lower.starts_with("[recent context")
            || lower.starts_with("[mémoire")
            || lower.starts_with("[system")
            || lower.starts_with("[error")
            || lower.contains("not authorized")
            || lower.contains("pas autorisé")
            || lower.contains("cannot create")
            || lower.contains("ne peux pas créer")
            || lower.contains("cannot write")
            || lower.contains("cannot read")
            // Filter out previous recap generations
            || lower.starts_with("bien sûr — voici")
            || lower.starts_with("sure — here's")
            || lower.starts_with("voici ce qu'on a fait")
            || lower.starts_with("here's what we")
            // Filter out generic greetings/closings
            || (lower.contains("bonjour") && lower.len() < 100)
            || (lower.contains("salut") && lower.len() < 80)
            || lower == "oui" || lower == "yes"
            || lower == "ok" || lower == "d'accord"
            || lower == "merci" || lower == "thanks"
            || lower == "merci beaucoup" || lower == "thank you"
        {
            continue;
        }
        let compact = c.replace('\n', " ").trim().to_string();
        // Skip very short fragments
        if compact.len() < 20 {
            continue;
        }
        lines.push(compact);
    }
    lines.dedup();
    if lines.is_empty() {
        return None;
    }
    let selected: Vec<String> = lines
        .into_iter()
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    match intent.language {
        SmallTalkLanguage::French => {
            let period = match intent.range {
                SessionRecallRange::Yesterday => "hier",
                SessionRecallRange::CurrentDay => "dans cette session",
            };
            let mut out = format!("Bien sûr — voici ce qu'on a fait {} :\n", period);
            for item in selected {
                out.push_str("- ");
                let truncated = if item.len() > 400 {
                    format!("{}…", item.chars().take(397).collect::<String>())
                } else {
                    item
                };
                out.push_str(&truncated);
                out.push('\n');
            }
            Some(out.trim_end().to_string())
        }
        SmallTalkLanguage::English => {
            let period = match intent.range {
                SessionRecallRange::Yesterday => "yesterday",
                SessionRecallRange::CurrentDay => "in this session",
            };
            let mut out = format!("Sure — here's what we did {}:\n", period);
            for item in selected {
                out.push_str("- ");
                let truncated = if item.len() > 400 {
                    format!("{}…", item.chars().take(397).collect::<String>())
                } else {
                    item
                };
                out.push_str(&truncated);
                out.push('\n');
            }
            Some(out.trim_end().to_string())
        }
    }
}

/// Best-effort X handle from user text (e.g. `@akasha_anthiam` → `akasha_anthiam`).
fn extract_x_profile_handle(message: &str) -> Option<String> {
    for token in message.split_whitespace() {
        let t = token.trim_end_matches(|c| matches!(c, '.' | ',' | ':' | ';'));
        if let Some(rest) = t.strip_prefix('@') {
            let handle: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if handle.len() >= 2 {
                return Some(handle);
            }
        }
    }
    None
}

/// True when policy allows at least one follow-up path after web_search: HTTP fetch and/or Playwright browser.
fn web_followup_tools_configured(policy: &akasha_tools::ToolsPolicy) -> bool {
    let web_fetch_ok = policy.can_use_tool("web_fetch") && !policy.allowed_web_domains.is_empty();
    let browser_ok = policy.can_use_tool("browser")
        && policy.browser_enabled
        && (policy
            .browser_allowed_domains
            .iter()
            .any(|d| d.trim().eq_ignore_ascii_case("*"))
            || !policy.browser_allowed_domains.is_empty());
    web_fetch_ok || browser_ok
}

/// True when the user likely refers to the SNCF regional product "TER".
/// Substring `ter ` must not be used: it matches inside "connecter", "twitter", etc.
fn message_mentions_ter_train_line(message_lower: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?i)\bter\b").expect("valid regex"));
    re.is_match(message_lower)
}

fn compute_message_intent_flags(message: &str) -> MessageIntentFlags {
    let m = message.to_lowercase();
    MessageIntentFlags {
        save_file: [
            "enregistre",
            "enregistrer",
            "sauvegarde",
            "sauvegarder",
            "écris dans",
            "ecris dans",
            "write to file",
            "save to",
            "save the file",
            "write the file",
            "dans le dossier",
            "dans le fichier",
            "dans un fichier",
            "sur le disque",
            "to disk",
            "to the file",
        ]
        .iter()
        .any(|k| m.contains(k)),
        external_info: [
            "météo",
            "meteo",
            "weather",
            "prévisions",
            "previsions",
            "actualités",
            "actualites",
            "horaires",
            "trafic",
            "prix",
            "cours ",
            "bourse",
            "news",
            "nouvelle",
            "semaine à",
            "aujourd'hui",
            "demain",
            "connaître la",
            "connaitre la",
            "quelle est la météo",
            "quel temps",
            "prévision",
            "prevision",
        ]
        .iter()
        .any(|k| m.contains(k)),
        social_feed_fetch: {
            let on_x_or_twitter = m.contains("x.com")
                || m.contains("twitter.com")
                || m.contains(" sur x")
                || m.contains("sur x.")
                || (m.contains("twitter") && m.contains('@'));
            let wants_posts = m.contains("post")
                || m.contains("tweet")
                || m.contains("récup")
                || m.contains("recup")
                || m.contains("retrieve")
                || m.contains("latest")
                || m.contains("dernier")
                || m.contains("timeline")
                || m.contains("fil d'actualité")
                || m.contains("actualité de @");
            on_x_or_twitter && wants_posts
        },
        camera_or_mic: [
            "webcam",
            "caméra",
            "camera",
            "prend une photo",
            "prends une photo",
            "prendre une photo",
            "take a photo",
            "take a picture",
            "prends moi en photo",
            "photo avec la webcam",
            "accède à la webcam",
            "accede a la webcam",
            "utilise la caméra",
            "utilise la camera",
            "affiche-la dans le chat",
            "afficher dans le chat",
            "display in the chat",
            "show in the chat",
            "micro",
            "microphone",
            "enregistre avec le micro",
            "enregistrer avec le micro",
            "obtenir une image",
            "get an image",
            "image avec la webcam",
            "image avec la caméra",
            "continuer pour obtenir une image",
            "proceed to get an image",
        ]
        .iter()
        .any(|k| m.contains(k)),
        image_generation: [
            "génère une image",
            "genere une image",
            "générer une image",
            "génère moi une image",
            "generate an image",
            "generate a picture",
            "draw",
            "dessine",
            "dessiner",
            "crée une image",
            "cree une image",
            "creer une image",
            "creer moi une image",
            "créer une image",
            "create an image",
            "image par ia",
            "ai image",
            "dall-e",
            "dalle",
        ]
        .iter()
        .any(|k| m.contains(k)),
        code_generation: [
            "écris un script",
            "ecris un script",
            "écrire un script",
            "ecrire un script",
            "write a script",
            "write the script",
            "génère du code",
            "genere du code",
            "generate code",
            "génère le code",
            "code python",
            "python script",
            "un programme qui",
            "a program that",
            "fonction qui",
            "function that",
            "snippet",
            "extrait de code",
            "piece of code",
            "exemple de code",
            "analyse ce projet",
            "analyze this project",
            "analyse le projet",
            "analyze the project",
            "review the codebase",
            "auditer le code",
            "code review",
            "dépôt git",
            "depot git",
        ]
        .iter()
        .any(|k| m.contains(k)),
        github_with_vault: {
            let github = [
                "github",
                "dépôt privé",
                "depot prive",
                "private repo",
                "api.github.com",
            ]
            .iter()
            .any(|k| m.contains(k));
            let vault = [
                "vault",
                "github_token",
                "clé du vault",
                "cle du vault",
                "key in the vault",
                "token dans le vault",
                "clef dans le vault",
            ]
            .iter()
            .any(|k| m.contains(k));
            github && vault
        },
        transport: message_mentions_ter_train_line(&m)
            || [
                "train",
                "tgv",
                "sncf",
                "gare ",
                "horaires de train",
                "horaires de bus",
                "horaires du train",
                "billet de train",
                "trajet en train",
                "rer ",
                "transilien",
                "bus ",
                "métro ",
                "metro ",
                "tramway",
                "tram ",
                "vol ",
                "aéroport",
                "aeroport",
                "airport",
                "terminal ",
                "itinéraire",
                "itineraire",
                "horaires de métro",
                "horaires du métro",
                "départ de ",
                "depart de ",
                "arrivée à ",
                "arrivee a ",
            ]
            .iter()
            .any(|k| m.contains(k)),
        geolocation_distance: [
            "distance entre",
            "distance between",
            "distance from",
            "distance to",
            "combien de km",
            "how far",
            "itinéraire entre",
            "itineraire entre",
            "route entre",
            "trajet entre",
            "km entre",
            "kilomètre",
            "kilometre",
        ]
        .iter()
        .any(|k| m.contains(k)),
    }
}

/// True if the user message suggests a tool-only action (camera, web search, save file, image gen) without asking for code generation. Used by the orchestrator to override mistaken "code" decomposition.
pub fn message_suggests_tool_only_action(message: &str) -> bool {
    let flags = compute_message_intent_flags(message);
    (flags.camera_or_mic
        || flags.save_file
        || flags.external_info
        || flags.social_feed_fetch
        || flags.image_generation)
        && !flags.code_generation
}

/// True if the user message suggests a long-running project (novel, comic, code project) or continuing one.
/// Capture limits for post-reply memory promotion (plan court terme 7, inspired by OpenClaw plugin).
const CAPTURE_MIN_CHARS: usize = 16;
const CAPTURE_MAX_CHARS: usize = 4000;
const CAPTURE_MAX_PER_TURN: usize = 20;

/// Short acknowledgments that should not be stored in long-term memory.
static CAPTURE_ACK_PATTERNS: &[&str] = &[
    "ok",
    "okay",
    "oui",
    "non",
    "merci",
    "thanks",
    "thank you",
    "d'accord",
    "daccord",
    "👍",
    "👌",
    "ok.",
    "parfait",
    "super",
    "cool",
    "noted",
    "compris",
    "c'est noté",
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
    if CAPTURE_ACK_PATTERNS
        .iter()
        .any(|p| lower_trim == *p || lower_trim.starts_with(&format!("{} ", p)))
    {
        return true;
    }
    false
}

fn message_suggests_project(message: &str) -> bool {
    let m = message.to_lowercase();
    let keywords = [
        "résumé du projet",
        "resume du projet",
        "summary of project",
        "workspace",
        "graphe projet",
        "project graph",
        "roman",
        "bd",
        "bande dessinée",
        "bande dessinee",
        "comic",
        "novel",
        "projet de code",
        "code project",
        "écris un",
        "ecris un",
        "écris le",
        "ecris le",
        "chapitre",
        "chapter",
        "continue",
        "la suite",
        "and the rest",
        "poursuis",
        "reprends",
        "crée un projet",
        "cree un projet",
        "create a project",
        "set up a project",
    ];
    keywords.iter().any(|k| m.contains(k))
}

/// Default hosts when policy does not set allowed_skill_install_hosts (GitHub only).
const INSTALL_SKILL_DEFAULT_HOSTS: &[&str] =
    &["github.com", "raw.githubusercontent.com", "www.github.com"];

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
        || allowed_hosts
            .iter()
            .any(|h| host == *h || host.ends_with(&format!(".{}", h)));
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
                let skill_name = segments
                    .get(segments.len().saturating_sub(2))
                    .copied()
                    .unwrap_or("skill")
                    .to_string();
                if !crate::user_rag::is_safe_relative_filename(&skill_name) {
                    return None;
                }
                (url.to_string(), skill_name)
            } else {
                let raw_url = format!(
                    "https://raw.githubusercontent.com/{}",
                    path.trim_end_matches('/')
                );
                let raw_skill_url = if raw_url.ends_with(".md") {
                    raw_url
                } else {
                    format!("{}/SKILL.md", raw_url)
                };
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
                let path_part = path_part
                    .strip_suffix("SKILL.md")
                    .map(|s| s.trim_end_matches('/'))
                    .unwrap_or(&path_part)
                    .to_string();
                Some((owner, repo, branch, path_part))
            } else {
                None
            };
            Some(ParsedSkillUrl {
                raw_skill_url,
                skill_name,
                api_path,
            })
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
                format!(
                    "https://raw.githubusercontent.com/{}/{}/{}/SKILL.md",
                    owner, repo, branch
                )
            } else {
                format!(
                    "https://raw.githubusercontent.com/{}/{}/{}/{}/SKILL.md",
                    owner, repo, branch, path_part
                )
            };
            let api_path = Some((owner, repo, branch, path_part));
            Some(ParsedSkillUrl {
                raw_skill_url,
                skill_name,
                api_path,
            })
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
                                let rel = if path == root_path {
                                    name.to_string()
                                } else {
                                    format!(
                                        "{}/{}",
                                        path.strip_prefix(root_path)
                                            .unwrap_or(path.as_str())
                                            .trim_start_matches('/'),
                                        name
                                    )
                                };
                                downloaded.push(rel);
                            }
                        }
                    }
                }
            } else if typ == "dir" {
                let subpath = if path.is_empty() {
                    name.to_string()
                } else {
                    format!("{}/{}", path, name)
                };
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
        Ok(r) => {
            return (
                false,
                format!(
                    "[install_skill] HTTP {} — {}",
                    r.status(),
                    parsed.raw_skill_url
                ),
            )
        }
        Err(e) => return (false, format!("[install_skill] requête: {}", e)),
    };
    if body.trim().is_empty() {
        return (
            false,
            format!(
                "[install_skill] SKILL.md vide ou introuvable: {}",
                parsed.raw_skill_url
            ),
        );
    }
    let skill_dir = data_dir.join("skills").join(&parsed.skill_name);
    if let Err(e) = std::fs::create_dir_all(&skill_dir) {
        return (
            false,
            format!(
                "[install_skill] impossible de créer le dossier {}: {}",
                skill_dir.display(),
                e
            ),
        );
    }
    let skill_md_path = skill_dir.join("SKILL.md");
    if let Err(e) = std::fs::write(&skill_md_path, &body) {
        return (
            false,
            format!(
                "[install_skill] écriture {}: {}",
                skill_md_path.display(),
                e
            ),
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
                format!(
                    "{}... [tronqué, {} caractères au total]",
                    truncated, total_chars
                )
            } else {
                body_instructions.to_string()
            };
            let extra_msg = if extra_files.is_empty() {
                String::new()
            } else {
                format!(
                    " Fichiers additionnels récupérés (scripts/, references/, assets/) : {}.",
                    extra_files.join(", ")
                )
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
                        *r.write().await =
                            std::sync::Arc::new(akasha_tools::ToolExecutor::new(reloaded));
                        commands_added_msg = commands_added_msg
                            .replace(" (allowed_commands) :", " ; politique rechargée à chaud :");
                    }
                }
            }
            let skill_dir_display = skill_dir.display().to_string();
            let msg = format!(
                "[install_skill] Skill « {} » installé et rechargé ({} skill(s) chargé(s)).{}{}\n\
                 Répertoire du skill (pour read_file sur references/, scripts/, assets/) : {}\n\n\
                 Contenu du skill (à utiliser pour savoir comment l'utiliser) :\n\n---\n{}",
                parsed.skill_name,
                count,
                extra_msg,
                commands_added_msg,
                skill_dir_display,
                body_preview
            );
            (true, msg)
        }
        Err(e) => (
            false,
            format!(
                "[install_skill] skill écrit mais rechargement échoué: {}",
                e
            ),
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
        return (
            false,
            "[uninstall_skill] usage: uninstall_skill <name> (ex. uninstall_skill bankr)"
                .to_string(),
        );
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return (
            false,
            "[uninstall_skill] nom invalide (utiliser uniquement lettres, chiffres, _ et -)"
                .to_string(),
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
            format!(
                "[uninstall_skill] « {} » n'est pas un répertoire.",
                skill_dir.display()
            ),
        );
    }
    if let Err(e) = std::fs::remove_dir_all(&skill_dir) {
        return (
            false,
            format!(
                "[uninstall_skill] impossible de supprimer {}: {}",
                skill_dir.display(),
                e
            ),
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
                format!(
                    " Commande « {} » retirée de tools_policy.yaml (allowed_commands).",
                    name
                )
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
            format!(
                "[uninstall_skill] dossier supprimé mais rechargement du registry échoué: {}",
                e
            ),
        ),
    }
}

const WRITE_FILE_REMINDER: &str = "\n[Reminder: the user is asking to save a file. You MUST reply ONLY with the line TOOL: write_file <full_path> then the file content on the following lines. Never say you cannot write to disk.]\n\n";

const WEB_SEARCH_REMINDER: &str = "\n[Reminder: the user is asking for external information (weather/météo, news, etc.). You MUST use TOOL: web_search <query> first — do NOT use bankr or portfolio for weather. If search snippets do not contain the precise facts (temperatures, sky state, rain risk, figures, tables), you MUST follow up with TOOL: web_fetch <url> on a relevant result URL, or TOOL: browser navigate <url> then TOOL: browser snapshot for JS-heavy or dynamic pages (e.g. many weather portals). Do not end by telling the user to visit links yourself if web_fetch or browser snapshot is available in your tool list and policy allows those domains — fetch and summarize. Do not suggest visiting a site without having used web_search first.]\n\n";

/// Injected only when web_search is available and policy allows web_fetch and/or configured browser follow-up.
const WEB_SEARCH_FOLLOWUP_REMINDER: &str = "\n[Reminder — page fetch: Search snippets are often incomplete. After web_search, if you still lack concrete details, call web_fetch on a result URL and/or browser navigate + browser snapshot (then answer from that output). Do not reply with only URLs for the user to open when these tools work.]\n\n";

/// Reminder injected when the user asks for external information (weather, news, etc.) but
/// web_search is not available in the current tools policy. Prevents the model from ignoring
/// the question and falling back to a generic capability introduction.
const WEB_SEARCH_UNAVAILABLE_REMINDER: &str = "\n[Note: the user is asking for weather, news, or other live external information. web_search is not currently enabled. Answer as best you can from your training knowledge, clearly state that the data may be outdated, and explain how to enable web search: set web_search_enabled: true in tools_policy.yaml and configure BRAVE_API_KEY. Do NOT respond with a generic capabilities introduction — address the user's question directly.]\n\n";

const TRANSPORT_REMINDER: &str = "\n[Reminder: the user is asking about transport schedules, routes, or travel information. You MUST use TOOL: web_search <query> first (e.g. web_search \"horaires train Angoulême Paris CDG dimanche\"). Do NOT write any files, generate HTML, or ask about project file paths — the user wants travel information only. If web_search is unavailable, say so clearly and suggest the relevant site (e.g. sncf.com, ratp.fr, transilien.com).]\n\n";

/// Transport reminder when web_search is not enabled: model cannot use the tool so we only anchor it to the domain.
const TRANSPORT_REMINDER_NO_SEARCH: &str = "\n[Reminder: the user is asking about transport routes or travel (train, car, bus, etc.). Do NOT write any files, create HTML pages, or ask about project file paths — the user wants travel information only. Answer from your knowledge (e.g. compare train vs car for this route). If you cannot give accurate live schedules, say so clearly and suggest the relevant site (e.g. sncf.com, ratp.fr, transilien.com).  Do NOT ask about file paths or project details — this is a travel question.]\n\n";

/// Generic distance reminder used when no plugin-specific routing rule matched.
const GEO_DISTANCE_REMINDER_WITH_TOOLS: &str = "\n[Reminder: the user asks for a geographic distance/route between places. PRIORITY: use a relevant distance/route tool available in the current tools list. If location details are missing, use TOOL: web_search with a focused distance query. STRICTLY FORBIDDEN for this request: memory_store, project planning, code generation, file writes, and unrelated queries.]\n\n";

const GEO_DISTANCE_REMINDER_NO_TOOL: &str = "\n[Reminder: the user asks for a geographic distance/route between places. No distance/search tool is available. Provide a concise best-effort estimate and clearly state uncertainty. Do NOT write files, ask for project paths, or perform unrelated tasks.]\n\n";

/// X/Twitter/social feed fetches: do not use ask_user for unrelated onboarding; use tools first.
const SOCIAL_FEED_REMINDER: &str = "\n[Reminder: SOCIAL / X / TWITTER — PRIORITY: The user wants posts, tweets, or timeline content from X (Twitter) or similar. Do NOT use TOOL: ask_user for generic greetings or unrelated menu choices — fulfill this request with tools. First TOOL: web_search <query> (e.g. site:x.com handle latest posts). If results are empty or insufficient, use TOOL: browser navigate <profile URL> then TOOL: browser snapshot (if browser is enabled in policy). Do not answer \"no context\" or \"blocked\" without having called web_search or browser.]\n\n";

const DEVICE_CAMERA_REMINDER: &str = "\n[Reminder: webcam/camera photo request. You MUST chain directly: TOOL: device_discover local_media then TOOL: device_invoke local_media camera capture. Do NOT ask the user \"which device action?\" with ask_user — they already said they want a photo; call device_invoke camera capture. Do NOT suggest: file upload, open UI, AI image. Do NOT mention tools_policy.yaml or allowed_write_paths for this request: the user wants a camera photo, not to configure file writing. If the user asked to \"display the photo in the chat\", after capture reply ONLY with a short confirmation in their language (e.g. \"Photo captured. It is shown below.\"): do NOT suggest \"save to file\", \"get a description\", \"take another photo\" or \"What would you like to do next?\" — the image is added automatically below your reply. Reply in the same language as the user.]\n\n";
const IMAGE_GENERATION_REMINDER: &str = "\n[Reminder: request to \"generate an image\", \"draw\", \"create an image\" (by AI, not webcam). You MUST use TOOL: generate_image <prompt> (e.g. TOOL: generate_image a cat on a sofa). Spec 42.]\n\n";

/// Reminder when the user asks for GitHub (private repo / API) and mentions the vault (e.g. GITHUB_TOKEN).
const GITHUB_VAULT_REMINDER: &str = "\n[Reminder GitHub + vault: you MUST run the request yourself via TOOL: run_command. Exact format: TOOL: run_command VAULT:GITHUB_TOKEN=GITHUB_TOKEN curl -sS -H \"Authorization: Bearer $GITHUB_TOKEN\" https://api.github.com/repos/owner/repo (or gh repo view owner/repo). FORBIDDEN: telling the user to do GITHUB_TOKEN=VAULT:... or export GITHUB_TOKEN=... or to put the token in plain text — you must emit the TOOL: line so the system injects the secret. Do not reply \"I did not find\" without having called run_command with VAULT:GITHUB_TOKEN=GITHUB_TOKEN.]\n\n";

/// Injected when the message looks like code/script work: prefer workspace paths, git/diff tools, and explicit cwd for commands.
const CODE_DEV_SANDBOX_REMINDER: &str = "\n[Reminder — code / project work: use workspace:/ paths for files in this task when no absolute path is given. For Git operations prefer TOOL: git_status, git_diff, git_log, git_rev_parse on the repo path (e.g. workspace:/ or an allowed folder) instead of raw git via run_command, unless you need a subcommand not covered. For file comparison use diff_unified or file_diff; for two trees use dir_compare. For build/test commands use TOOL: run_command --cwd workspace:/ cargo test (or npm test, etc.) so the command runs in the project root; or set run_command_default_cwd_workspace: true in tools_policy.yaml. For isolated execution with a toolchain image, use run_in_container when policy allows.]\n\n";
const STUDIO_DISK_REMINDER: &str = "\n[Code Studio — périmètre disque: cette tâche s'exécute sous le dossier projet studio uniquement (miroir workspace:/ et cwd des outils). Ne pas cibler de chemins hors de ce répertoire. Pour npm install / builds à risque, privilégier run_in_container si la politique d'outils l'autorise.]\n\n";

/// Injected with STUDIO_DISK_REMINDER: raise quality bar and user-visible wrap-up for Code Studio agents.
const STUDIO_AGENT_QUALITY_REMINDER: &str = concat!(
    "\n[Code Studio — exigences avant de considérer la demande comme terminée]\n",
    "- Quand tu écris un fichier, son contenu doit être STRICTEMENT le contenu attendu du fichier (code, JSON, Markdown, config). ",
    "Interdiction d'y ajouter du texte conversationnel, des explications, des statuts, des raisonnements, ou des phrases comme ",
    "\"fichier corrigé\", \"je relance le build\", \"voici la correction\". Ces messages vont uniquement dans la réponse chat.\n",
    "- Pour les fichiers de code (ex: .ts, .tsx, .js, .rs, .py), n'écris que du code syntaxiquement valide pour ce langage ; ",
    "ne mets jamais de prose libre hors commentaires valides du langage.\n",
    "- Ne déclare PAS la tâche terminée tant que le livrable n'est pas vérifié quand c'est possible : lance un build ou des tests ",
    "via TOOL: run_command avec --cwd workspace:/ (ou la racine du projet) quand la politique d'outils le permet — ",
    "par ex. npm run build, npm test, cargo build, cargo test, pytest, tsc --noEmit. Corrige les erreurs de compilation ",
    "ou de typage que tu peux corriger sans dériver du besoin utilisateur.\n",
    "- Après des changements qui touchent au comportement à l'exécution, exécute aussi une vérification d'exécution minimale ",
    "quand c'est raisonnable (tests automatisés, ou une commande courte qui exerce le chemin modifié avec timeout). ",
    "Ne te contente pas d'un build seul si la demande porte sur un bug ou un comportement runtime.\n",
    "- Si un build complet est trop lourd ou bloqué par la politique, exécute au moins une vérification ciblée (lint, typecheck) ",
    "ou explique clairement ce que tu n'as pas pu valider et pourquoi.\n",
    "- Ta dernière réponse à l'utilisateur (même langue que lui) doit résumer en langage accessible : ce qui a été ajouté ou modifié, ",
    "comment lancer ou essayer le résultat, et les limites éventuelles. Pas de jargon inutile sauf si l'utilisateur demande le détail technique.\n",
    "- N'achève pas seulement par « Terminé » / « Done » : fournis un paragraphe utile lisible sans ouvrir les fichiers.\n",
    "- Le fichier CODE_STUDIO_PLAN.md à la racine du projet (créé automatiquement) décrit objectif, étapes et historique : mets-le à jour après chaque lot de modifications (fichiers touchés, commandes de vérif, reste à faire). S'il manque (projet importé), crée-le en synthétisant l'existant.\n\n",
);

/// Application context injected into the prompt: the agent knows it runs inside Akasha and can talk about it.
const APP_CONTEXT: &str = concat!(
    "[Akasha context] You are the assistant embedded in Akasha. Akasha is the application you are currently running in. ",
    "If the user talks about Akasha, the program, the app or how it works, you can explain: ",
    "commands (akasha start, akasha init, akasha doctor), interfaces (TUI with Chat/Router/Memory/Doc/Activity tabs), ",
    "slash commands in Chat (/help, /status, /doctor, /advice, /config, /models, /routes, /newsession, /skills reload, etc.). ",
    "To install a CLI globally (e.g. \"install the bankr CLI\", \"npm install -g @bankr/cli\"), reply with TOOL: run_command npm install -g <package> (do not generate a script for the user to run). ",
    "To use a vault key in a command: TOOL: run_command VAULT:bankr_api_key=BANKR_API_KEY bankr whoami (the system injects the vault value). ",
    "GitHub + vault: run TOOL: run_command VAULT:GITHUB_TOKEN=GITHUB_TOKEN curl -sS -H \"Authorization: Bearer $GITHUB_TOKEN\" https://api.github.com/repos/owner/repo (not GITHUB_TOKEN=VAULT:... or export or plain token). If gh-axi is installed (npm install -g gh-axi), prefer it for issues/PRs/repos — token-efficient CLI per AXI (https://axi.md/, https://github.com/kunchenguid/axi). ",
    "Browser automation from shell: chrome-devtools-axi (same repo) can complement Akasha TOOL: browser navigate + browser snapshot for heavy browsing tasks; use whichever fits policy and environment. ",
    "Skills (extra capabilities): the user can add them without changing code. When the user asks to install, download, fetch or add a skill from a URL (e.g. \"install the bankr skill from …\", \"download the skill at this url\"), you MUST reply with TOOL: install_skill <url>. If the user says \"follow the SKILL.md instructions\", you must first do TOOL: install_skill <url> (the system registers the skill); then the skill is available and can be invoked by name (e.g. TOOL: security-audit <args>). Do not fetch SKILL.md with web_fetch to execute its content manually. ",
    "To uninstall a skill: TOOL: uninstall_skill <name> (e.g. TOOL: uninstall_skill bankr). ",
    "When the user asks you to perform an action with a skill (e.g. \"check my bankr wallet\", \"run bankr whoami\"), you MUST reply ONLY with one line TOOL: <skill_name> <arguments> (e.g. TOOL: bankr whoami) so the system runs the command; do not tell the user to run the command themselves. ",
    "Otherwise the user can place files in the skills folder and run /skills reload. ",
    "Full documentation is available in the Doc tab of the interface. ",
    "Language: ALWAYS reply in the same language as the user's last message (French → French, English → English, etc.). Do not switch language even if tool results or context are in another language. ",
    "Autonomy: work autonomously until the user's request is completely resolved. Do not stop in the middle of a task to ask for confirmation unless you hit a hard blocker (missing credentials, genuinely ambiguous requirements that cannot be inferred from context). For anything you can discover via a tool (read_file, web_search, run_command, etc.), prefer the tool over asking the user. ",
    "Research before acting: when you are unsure about file contents or codebase structure, use read_file and search tools before editing or answering. Never guess or invent code — your answer must be grounded in actual research. ",
    "Code conventions: when editing code, first read the file to understand its existing conventions, imports, and style. Mimic existing patterns. Never assume a library or dependency is available — verify it is already declared in the project's dependency file (Cargo.toml, package.json, requirements.txt, etc.) before using it. ",
    "Tests discipline: when tests fail, never modify the tests themselves unless the user explicitly asks you to. Assume the bug is in the code under test. If the same test or CI still fails after three consecutive attempts, stop and ask the user for guidance rather than iterating blindly. ",
    "Debugging: when debugging, address the root cause rather than the symptoms. Add descriptive logging statements to track variable state. If multiple approaches have failed, step back and think big-picture before making more changes. ",
    "Never invent data. If you do not have the information to answer, say so clearly (e.g. \"I did not find that information\"). ",
    "For questions about information you do not have (weather, forecasts, news, schedules, etc.), you must use the web_search tool first, then if snippets are insufficient use web_fetch and/or browser navigate plus browser snapshot to read the page itself and reply with the synthesized facts. When you have just received tool results (e.g. web_search, web_fetch, browser snapshot), you must answer immediately with the synthesized result — do not reply with a promise (e.g. \"I will fetch…\", \"Action in progress\"); the task ends after your message, so give the actual answer. ",
    "Do not suggest the user visit a site without having used web_search first if you have access to that tool; do not only list URLs for the user when web_fetch or browser snapshot can retrieve the content. ",
    "If web_search returns an error (e.g. not enabled), you can then suggest sites and explain how to enable web search (tools_policy.yaml, web_search_enabled, BRAVE_API_KEY). ",
    "Playwright / managed browser: the daemon may auto-install Chromium on first browser use unless AKASHA_PLAYWRIGHT_AUTO_INSTALL=0. If browser fails for missing Chromium, the runner is missing, or the user must explicitly approve a large download, use TOOL: ask_user (e.g. choices agreeing to install), then TOOL: install_playwright. List install_playwright in tool_profiles when using a profile. tools_policy require_approval can include install_playwright for UI approval before the install runs. For other dependencies (npm, cargo, etc.), use run_command with allowed_commands after ask_user consent. ",
    "You have access to the write_file tool: you MUST use it whenever the user asks to save, store or write a file (e.g. \"save the code to …\", \"write to file\"). ",
    "Reply ONLY with one line TOOL: write_file <full_path> then the file content on the following lines. ",
    "Never say \"I cannot write to disk\" or \"copy-paste the code yourself\" — if the path is denied by policy, the tool will return an error and you then explain how to add the prefix in tools_policy.yaml (allowed_write_paths). Paths can be Windows (C:\\Users\\...) or Unix. When the user did not specify a path, prefer workspace:/<filename> (e.g. workspace:/script.py) so the file is saved in the task workspace without policy errors. ",
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
        "code" => Some("You are the code generation agent. Produce correct, readable code. Prefer write_file and workspace:/ paths for new files. For Git inspection use git_status, git_diff, git_log, git_rev_parse on the repo path when available; use diff_unified or dir_compare to compare files or trees. Run builds and tests with run_command --cwd workspace:/ (or the project root). Use run_in_container when policy allows and you need an isolated toolchain. Do not invent APIs; use read_file when needed to match existing code. When the user asks to *perform* an action (take a photo, search the web, save a file), use the appropriate TOOL; do not generate a script for that. Use code only when the user explicitly asks to *write* or *generate* code or a script. Before editing any file, read it to understand its conventions, imports, and style; mimic existing patterns. Never assume a library is available — verify it is already declared in the project dependency file (Cargo.toml, package.json, etc.). When tests fail, never modify the tests themselves; fix the code under test. If the same test still fails after three attempts, stop and ask the user for guidance."),
        "search" => Some("You are the search agent. Use web_search to find external information (weather, news, facts). When search snippets lack concrete details, use web_fetch on a result URL and/or browser navigate plus browser snapshot, then synthesize and cite sources. Do not claim information you have not retrieved via tools when they are available."),
        "financial" => Some("You are the financial specialist. Help with budgets, cost analysis, financial reports, numeric reasoning. Be precise with figures and units. Do not invent data; state what is missing if needed."),
        "documentalist" => Some("You are the documentalist. Transform a pile of files into exploitable data. Answer from the user's document base (RAG). Prioritize [User documents] and [Long-term memory]. Use memory_search when relevant. Quote or summarize from excerpts; if insufficient, say so and suggest adding documents. Produce structured summaries when asked."),
        "project_manager" => Some("You are the project manager. Help with project tracking, milestones, task breakdown, planning. Refer to schedules and recurring tasks when relevant. Propose clear next steps and deliverables."),
        "technical_writer" => Some("You are the technical writing agent. Produce clear technical documentation, procedures, tutorials. Use a structured style (headings, steps, code blocks when relevant). Prefer clarity and precision. Use write_file when the user asks to save documentation."),
        "research" => Some("You are the research agent. Perform in-depth research using web_search, memory_search, and the document base. After web_search, use web_fetch and/or browser navigate plus browser snapshot when snippets or excerpts are insufficient to answer factually. Synthesize multiple sources; cite or summarize clearly. Do not invent facts."),
        "security_audit" => Some("You are the security audit agent. Review code, config, or practices for security. Be methodical; highlight risks and suggest mitigations. Do not claim certainty where you lack context; recommend human review for critical decisions."),
        "creative" => Some("You are the creative agent. You have a strong creative sense for text and images. Produce marketing copy, creative content, stories, and audience-adapted text. Match tone and format to the requested channel and goal. When the task is to get a photo from the user's webcam/camera, use TOOL: device_invoke local_media camera capture first; for AI-generated images use generate_image."),
        "analyst" => Some("You are the product / functional analyst. Formalize the need before any production. Output: reformulated need, scope, assumptions, acceptance criteria, initial backlog. Do not jump to implementation; clarify and structure the request first."),
        "architect" => Some("You are the technical architect. Design the skeleton of the project. Output: proposed architecture, task list, dependencies between tasks, execution order, definition of done. Stay at design level; do not write full implementation."),
        "frontend" => Some("You are the frontend agent. Produce UI components, views, and client-side logic. Focus on UX, accessibility, responsive layout, and integration with the design system. List impacted components. Do not modify database schema unless explicitly asked. Before editing any file, read it to understand existing conventions and patterns; mimic the existing code style. Verify that any library you use is already declared in package.json before importing it."),
        "backend" => Some("You are the backend agent. Produce server-side logic, APIs, and business rules. Focus on correctness, performance, and clear contracts. Do not change frontend or DB schema unless the task explicitly requires it. Before editing any file, read it to understand existing conventions, imports, and patterns; mimic the existing code style. Never assume a dependency is available — verify it in the project's dependency file. When debugging failures, address the root cause rather than the symptoms; add targeted logging to isolate the issue. If the same problem persists after three attempts, stop and ask the user."),
        "database" => Some("You are the database / data agent. Produce schemas, migrations, queries, and data pipelines. Focus on consistency, indexing, and data integrity. Output clear DDL or migration steps when applicable."),
        "integration" => Some("You are the integration agent. Wire components together: APIs, events, external services. Focus on contracts, error handling, and end-to-end flows. Produce a precise deliverable (config, glue code, or runbook)."),
        "qa" => Some("You are the quality control agent. You prevent false 'work done'. Verify coherence, requirement coverage, missing files, hidden TODOs, incomplete sections. Do not rewrite; report defects and gaps by severity. Do not validate if acceptance criteria are incomplete; output a clear report for rework."),
        "system" => Some("You are the system agent. You have full knowledge of the Akasha application: commands (akasha start, init, doctor), interfaces (TUI, Chat, Router, Memory, Doc, Calendar), slash commands, skills, tools, and configuration. You can resolve issues and answer any question about how Akasha works. Be precise and refer to real features only."),
        "image_generation" => Some("You are the image generation agent. Produce images from text prompts using the generate_image tool. Focus on clear, concrete prompts that yield the requested visual. One precise deliverable per request."),
        "studio_scaffold" => Some("You are the Code Studio scaffold agent. Create a minimal, runnable project skeleton (README, package.json or Cargo.toml, clear entrypoints). Prefer workspace:/ paths when no absolute path is given; mirror files to the studio disk root. When the user message contains a [Stack technique du projet] block at the top, follow it strictly for languages, frameworks, package manager, and tooling; otherwise align with the stack recorded for the project or keep the skeleton generic. Do not add dead files; keep structure conventional. Update CODE_STUDIO_PLAN.md at the project root after substantive changes. FILE OUTPUT RULE (strict): when writing files, write only the file content itself; never insert chat prose/status/explanations/reflection inside files. If a previous generation polluted a file with prose, clean it and keep only valid file content. Before finishing: run an appropriate build or typecheck when possible; in your final reply summarize what you created and how to run it in plain language."),
        "studio_frontend" => Some("You are the Code Studio frontend agent. Build UI components, routing, and styles with accessibility in mind. Prefer workspace:/ paths. When a [Stack technique du projet] block is present in the user message, obey it for UI libraries, bundler, CSS approach, and TypeScript/JavaScript choice. Verify dependencies exist in package.json before importing. Use read_file before editing. Update CODE_STUDIO_PLAN.md after substantive edits. FILE OUTPUT RULE (strict): when writing files, write only the file content itself; never insert chat prose/status/explanations/reflection inside files. For code files, output syntactically valid code only (except valid language comments). Run build/lint/typecheck via run_command --cwd workspace:/ when policy allows, and fix issues you introduced. End with a clear user-facing summary of changes and how to preview or test — not only \"Done\"."),
        "studio_backend" => Some("You are the Code Studio backend agent. Add APIs, env-based config, and CORS as needed. Prefer workspace:/ paths. When a [Stack technique du projet] block is present, follow it for runtime (Node, Python, Rust, etc.), framework, and persistence choices. Never assume dependencies exist without checking the manifest. Use git_* tools on the project root when inspecting history. Update CODE_STUDIO_PLAN.md after substantive edits. FILE OUTPUT RULE (strict): when writing files, write only the file content itself; never insert chat prose/status/explanations/reflection inside files. For code files, output syntactically valid code only (except valid language comments). Before declaring completion: run tests or at least start/build checks when feasible; summarize APIs and behavior for the user in accessible terms."),
        "studio_fullstack" => Some("You are the Code Studio full-stack agent. Coordinate frontend and backend changes in one pass: clear API contracts, shared types when applicable, and a coherent folder layout. Prefer workspace:/ paths; use run_in_container when policy allows for installs and builds. When a [Stack technique du projet] block is present in the user message, treat it as binding for the whole stack unless the user explicitly contradicts it in the same message. Update CODE_STUDIO_PLAN.md after substantive edits. FILE OUTPUT RULE (strict): when writing files, write only the file content itself; never insert chat prose/status/explanations/reflection inside files. If prose was accidentally inserted in a source file, remove it and keep only valid syntax for that file type. Verify end-to-end coherence; run combined build/test when policy allows. Close with a plain-language recap of what changed and how to run the app."),
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
            result_preview
                .replace('\n', " ")
                .chars()
                .take(200)
                .collect::<String>()
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

/// Strip `- ` / `* ` / `1. ` list prefixes so tool lines can be detected.
fn strip_optional_list_prefix(line: &str) -> &str {
    let mut s = line.trim_start();
    for p in ["- ", "* ", "+ ", "• "] {
        if let Some(r) = s.strip_prefix(p) {
            s = r.trim_start();
            break;
        }
    }
    if s.is_empty() {
        return s;
    }
    if s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        if let Some(dot) = s.find('.') {
            if dot > 0 && s[..dot].chars().all(|c| c.is_ascii_digit()) {
                return s[dot + 1..].trim_start();
            }
        }
    }
    s
}

/// Strip ATX heading hashes (`### Title` → `Title`).
fn strip_markdown_heading_hashes(line: &str) -> &str {
    let mut s = line.trim_start();
    let mut n = 0usize;
    while s.starts_with('#') && n < 7 {
        n += 1;
        s = &s[1..];
    }
    if n > 0 {
        s = s.trim_start();
    }
    s
}

/// Leading markdown noise before `TOOL` (table pipes, headings, lists, bold, backticks).
fn strip_leading_tool_line_noise(line: &str) -> &str {
    let mut s = line.trim_start();
    while s.starts_with('|') {
        s = s[1..].trim_start();
    }
    s = strip_markdown_heading_hashes(s);
    s = strip_optional_list_prefix(s);
    s = s
        .trim_start_matches(|c: char| matches!(c, '*' | '`'))
        .trim_start();
    s
}

/// True if `prefix` is only whitespace and common markdown punctuation (no letters/words — avoids matching prose before `TOOL:`).
fn tool_line_prefix_is_markdown_junk_only(prefix: &str) -> bool {
    prefix.trim().chars().all(|c| {
        c.is_whitespace()
            || matches!(
                c,
                '#' | '*'
                    | '`'
                    | '|'
                    | '•'
                    | '-'
                    | '+'
                    | ':'
                    | '.'
                    | ';'
                    | ','
                    | '/'
                    | '\\'
                    | '('
                    | ')'
                    | '['
                    | ']'
            )
            || c.is_ascii_digit()
    })
}

/// First word after `TOOL:` must match a real tool when using the "inline" heuristic (avoids prose `… TOOL: …`).
const ORCH_INLINE_TOOL_FIRST_WORDS: &[&str] = &[
    "write_file",
    "read_file",
    "edit_file",
    "search_replace",
    "apply_patch",
    "list_dir",
    "grep_content",
    "web_search",
    "web_fetch",
    "run_command",
    "browser",
    "install_playwright",
    "read_skill",
    "memory_store",
    "workspace_graph_search",
    "device_invoke",
    "ask_user",
    "generate_image",
    "pdf",
];

/// `### Step — TOOL: write_file`, table junk, or other lines where `TOOL:` is not at column 0 after strips.
fn inline_ascii_tool_colon_rest(line: &str) -> Option<&str> {
    let t = line.trim();
    let mut search = t;
    let mut last_ok: Option<&str> = None;
    while let Some(pos) = search.find("TOOL:") {
        let abs = t.len() - search.len() + pos;
        let prefix = &t[..abs];
        let rest = t[abs + 5..].trim_start();
        if let Some(tok) = rest.split_whitespace().next() {
            let tl = tok.to_lowercase();
            if ORCH_INLINE_TOOL_FIRST_WORDS
                .iter()
                .any(|&n| n == tl.as_str())
            {
                if tool_line_prefix_is_markdown_junk_only(prefix) {
                    last_ok = Some(rest);
                }
            }
        }
        search = &t[abs + 5..];
    }
    last_ok
}

/// If the line begins with `tool` (case-insensitive) as a keyword followed by optional `*` / `` ` `` and `:`, return the rest.
/// Handles models that emit `- Tool:`, `**TOOL:**`, table cells `| TOOL: ... |`, etc., when substring `TOOL:` is present but strict parse failed.
fn line_rest_after_leading_tool(line: &str) -> Option<&str> {
    line_rest_after_leading_tool_at_start(line).or_else(|| inline_ascii_tool_colon_rest(line))
}

fn line_rest_after_leading_tool_at_start(line: &str) -> Option<&str> {
    let s = strip_leading_tool_line_noise(line);
    if s.len() < 4 {
        return None;
    }
    if !s.get(0..4)?.eq_ignore_ascii_case("tool") {
        return None;
    }
    // Reject `tools:` / `toolkit:` — fifth char must not be ASCII letter.
    if s.as_bytes().get(4).is_some_and(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let mut rest = &s[4..];
    rest = rest.trim_start();
    while rest.starts_with('*') || rest.starts_with('`') {
        rest = &rest[1..];
    }
    rest = rest.strip_prefix(':')?;
    rest = rest.trim_start();
    while rest.starts_with('*') || rest.starts_with('`') {
        rest = &rest[1..];
    }
    // Drop trailing table pipe from first cell
    let rest = rest.trim_end();
    let rest = rest.strip_suffix('|').map(|x| x.trim_end()).unwrap_or(rest);
    Some(rest.trim_start())
}

/// Returns true if `tool_name` (already lower-cased) takes a multiline body argument.
fn tool_supports_multiline_body(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "apply_patch" | "edit_file" | "write_file" | "ask_user"
    )
}

fn tool_name_is_safe_identifier(tool_name: &str) -> bool {
    !tool_name.is_empty()
        && tool_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

/// Rewrite lines so strict `TOOL:` prefix parsing succeeds (see `parse_tool_calls`).
///
/// Lines that are inside the body of a multiline tool (`write_file`, `edit_file`,
/// `apply_patch`, `ask_user`) are **not** normalized — only a real tool header
/// (canonical `TOOL: …` or a sloppy markdown-style prefix recognized by
/// `line_rest_after_leading_tool`, e.g. `- Tool: …` or `**TOOL:** …`) can
/// terminate a body.  Lines that merely *mention* a tool mid-sentence
/// (e.g. `### Step — TOOL: read_file`) remain body content because
/// `line_rest_after_leading_tool` requires the keyword to appear at the effective
/// start of the line after stripping markdown noise.
fn normalize_response_tool_prefixes(response: &str) -> String {
    let mut in_fence = false;
    let mut in_multiline_body = false;
    let mut out_lines = Vec::new();

    for raw in response.lines() {
        let trimmed = raw.trim();

        // Track fenced code blocks (``` or ```lang) — never normalize inside them.
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            out_lines.push(raw.to_string());
            continue;
        }

        if in_fence {
            out_lines.push(raw.to_string());
            continue;
        }

        // Inside a multiline body, exit body mode only when the line is a real
        // tool header — either the canonical "TOOL: …" form or a sloppy
        // markdown-style prefix that `line_rest_after_leading_tool` recognises
        // (e.g. "- Tool: …", "**TOOL:** …").  Lines where TOOL: is embedded
        // after non-noise text (e.g. "### Step — TOOL: read_file") are kept as
        // body content because `line_rest_after_leading_tool` requires the
        // keyword at the effective start of the line.
        if in_multiline_body {
            if raw.trim_start().starts_with("TOOL:") || line_rest_after_leading_tool(raw).is_some()
            {
                // Real tool header (canonical or sloppy) — exit body mode and fall through.
                in_multiline_body = false;
            } else {
                out_lines.push(raw.to_string());
                continue;
            }
        }

        if let Some(rest) = line_rest_after_leading_tool(raw) {
            let normalized = format!("TOOL: {}", rest);
            // If this tool supports a multiline body, subsequent lines are body content.
            if let Some(name) = rest.split_whitespace().next() {
                if tool_supports_multiline_body(&name.to_lowercase()) {
                    in_multiline_body = true;
                }
            }
            out_lines.push(normalized);
        } else {
            // Already canonical TOOL: line — still need to track body-mode entry.
            if let Some(rest) = trimmed.strip_prefix("TOOL:") {
                if let Some(name) = rest.trim().split_whitespace().next() {
                    if tool_supports_multiline_body(&name.to_lowercase()) {
                        in_multiline_body = true;
                    }
                }
            }
            out_lines.push(raw.to_string());
        }
    }

    out_lines.join("\n")
}

/// Parse tool calls from LLM response: lines "TOOL: tool_name arg1 arg2 ...".
fn parse_tool_calls(response: &str) -> Vec<(String, Vec<String>)> {
    let normalized = normalize_response_tool_prefixes(response);
    parse_tool_calls_strict(&normalized)
}

/// Parse already-normalized tool lines (internal).
fn parse_tool_calls_strict(response: &str) -> Vec<(String, Vec<String>)> {
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
                let tool_name_lc = name.to_lowercase();
                if !tool_name_is_safe_identifier(&tool_name_lc) {
                    i += 1;
                    continue;
                }
                let mut args = fixed_args.to_vec();
                // Only certain tools support a multi-line body argument.
                let supports_body = tool_supports_multiline_body(&tool_name_lc);

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
/// optional `--cwd <path>` (after VAULT lines) sets the working directory;
/// the next token is the command, the rest are command arguments.
/// Returns (vault_specs, cwd_flag, command, cmd_args).
fn parse_run_command_args(
    args: &[String],
) -> (Vec<(String, String)>, Option<String>, String, Vec<String>) {
    let mut vault_specs = Vec::new();
    let mut rest: Vec<String> = Vec::new();
    for arg in args {
        if let Some(s) = arg.strip_prefix("VAULT:") {
            if let Some((vault_key, env_var)) = s.split_once('=') {
                vault_specs.push((vault_key.trim().to_string(), env_var.trim().to_string()));
            }
            continue;
        }
        rest.push(arg.clone());
    }
    let mut cwd_flag: Option<String> = None;
    if rest.len() >= 2 && rest[0] == "--cwd" {
        cwd_flag = Some(rest[1].clone());
        rest.drain(..2);
    }
    let (command, cmd_args) = rest
        .split_first()
        .map(|(c, a)| (c.clone(), a.to_vec()))
        .unwrap_or_else(|| (String::new(), Vec::new()));
    (vault_specs, cwd_flag, command, cmd_args)
}

/// Resolve working directory for `run_command` / `run_terminal` / `run_command_background`.
/// Returns `None` for legacy behavior (daemon process cwd). Returns `Some(path)` when `--cwd` was set
/// or `run_command_default_cwd_workspace` applies.
fn resolve_run_command_working_dir(
    cwd_flag: Option<&str>,
    workspace_root: Option<&std::path::Path>,
    policy: &akasha_tools::ToolsPolicy,
) -> Result<Option<std::path::PathBuf>, String> {
    if let Some(raw) = cwd_flag {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("--cwd requires a non-empty path".to_string());
        }
        let p = resolve_tool_disk_path(raw, workspace_root);
        let p = strip_verbatim_prefix(p);
        let meta = std::fs::metadata(&p).map_err(|e| format!("cwd {}: {}", p.display(), e))?;
        if !meta.is_dir() {
            return Err(format!("--cwd is not a directory: {}", p.display()));
        }
        if !policy.can_read(&p) {
            return Err(format!(
                "cwd not allowed by policy (allowed_read_paths): {}",
                p.display()
            ));
        }
        return Ok(Some(p));
    }
    if policy.run_command_default_cwd_workspace {
        if let Some(root) = workspace_root {
            let root_pb = strip_verbatim_prefix(root.to_path_buf());
            if root_pb.is_dir() && policy.can_read(&root_pb) {
                return Ok(Some(root_pb));
            }
        }
    }
    Ok(None)
}

/// Open a URL in the system default browser. Only http and https URLs are allowed.
#[allow(dead_code)]
fn open_url_in_browser(url: &str) -> Result<(), String> {
    let url = url.trim();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("Only http and https URLs are allowed".to_string());
    }
    let parsed = url
        .parse::<url::Url>()
        .map_err(|e| format!("Invalid URL: {}", e))?;
    let scheme = parsed.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err("Only http and https URLs are allowed".to_string());
    }
    let status = match std::env::consts::OS {
        "windows" => std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .status()
            .map_err(|e| e.to_string())?,
        "macos" => std::process::Command::new("open")
            .arg(url)
            .status()
            .map_err(|e| e.to_string())?,
        _ => std::process::Command::new("xdg-open")
            .arg(url)
            .status()
            .map_err(|e| e.to_string())?,
    };
    if status.success() {
        Ok(())
    } else {
        Err(format!("Command exited with: {}", status))
    }
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

/// Root task id for the `workspace:/` virtual store (same bucket as `write_file`).
/// Orchestrated children must read the lineage root map; otherwise they miss files written under the parent id.
fn workspace_lineage_root_task_id(task_id: Uuid, store_path: Option<&std::path::Path>) -> Uuid {
    store_path
        .and_then(|sp| TaskStore::open(sp).ok())
        .map(|s| {
            let mut current = task_id;
            for _ in 0..8 {
                let parent = s.get(current).ok().flatten().and_then(|t| t.parent_task_id);
                match parent {
                    Some(p) => current = p,
                    None => break,
                }
            }
            current
        })
        .unwrap_or(task_id)
}

/// LLMs often paste a **stale** root id into `workspace:/.akasha/plan_<uuid>.md` (e.g. from an older run).
/// Rewrite to this task's lineage root so reads/writes target the live orchestration plan.
fn rewrite_workspace_plan_key_to_lineage_root(
    key: &str,
    lineage_root: Uuid,
) -> (String, Option<Uuid>) {
    const PREFIX: &str = ".akasha/plan_";
    const SUFFIX: &str = ".md";
    let k = key.replace('\\', "/");
    if !k.starts_with(PREFIX) || !k.ends_with(SUFFIX) {
        return (key.to_string(), None);
    }
    let mid = &k[PREFIX.len()..k.len() - SUFFIX.len()];
    let Ok(parsed) = Uuid::parse_str(mid) else {
        return (key.to_string(), None);
    };
    if parsed == lineage_root {
        return (k, None);
    }
    let new_key = format!("{PREFIX}{lineage_root}{SUFFIX}");
    (new_key, Some(parsed))
}

/// Rewrite a full `workspace:/...` path string so that any stale plan UUID is replaced by the
/// lineage-root UUID.  Non-workspace paths and non-plan-trace paths are returned unchanged.
/// Used before calling `resolve_tool_disk_path` for partial-edit tools (`search_replace`,
/// `edit_file`, `apply_patch`) so they operate on the live plan file rather than a stale copy.
fn rewrite_workspace_plan_path_str(
    path_str: &str,
    task_id: Uuid,
    store_path: Option<&std::path::Path>,
) -> String {
    if !(path_str.starts_with("workspace:/") || path_str.starts_with("workspace:")) {
        return path_str.to_string();
    }
    let key = path_str
        .trim_start_matches("workspace:/")
        .trim_start_matches("workspace:")
        .trim_start_matches('/');
    let lineage_id = workspace_lineage_root_task_id(task_id, store_path);
    let (new_key, _) = rewrite_workspace_plan_key_to_lineage_root(key, lineage_id);
    format!("workspace:/{new_key}")
}

/// After a successful partial edit (`edit_file`, `search_replace`, `apply_patch`) on a `workspace:/` path,
/// re-read the updated disk file and insert it into the in-memory workspace store so that subsequent
/// `read_file workspace:/` calls return the latest content.
async fn sync_workspace_store_from_disk(
    workspace_store: Option<&TaskWorkspaceStore>,
    task_id: Uuid,
    store_path: Option<&std::path::Path>,
    workspace_path_str: &str,
    disk_path: &std::path::Path,
) {
    let Some(ws) = workspace_store else { return };
    let key = workspace_path_str
        .trim_start_matches("workspace:/")
        .trim_start_matches("workspace:")
        .trim_start_matches('/')
        .to_string();
    let lineage_task_id = workspace_lineage_root_task_id(task_id, store_path);
    let (key, _) = rewrite_workspace_plan_key_to_lineage_root(&key, lineage_task_id);
    if let Ok(updated) = tokio::fs::read_to_string(disk_path).await {
        let mut guard = ws.write().await;
        guard
            .entry(lineage_task_id)
            .or_default()
            .insert(key, updated);
    }
}

fn parse_plugin_tool_invocation(
    plugin_registry: Option<&std::sync::Arc<crate::plugins::PluginRegistry>>,
    tool_name: &str,
    args: &[String],
) -> Option<(String, String)> {
    let registry = plugin_registry?;
    let available_ids: std::collections::HashSet<String> =
        registry.list().into_iter().map(|p| p.id).collect();

    if available_ids.is_empty() {
        return None;
    }

    let (plugin_id, action, forwarded_args): (String, Option<String>, Vec<String>) = if tool_name
        .eq_ignore_ascii_case("plugin.call")
        || tool_name.eq_ignore_ascii_case("plugin_call")
    {
        let plugin_id = args.first()?.trim().to_string();
        let forwarded = args.get(1..).map(|v| v.to_vec()).unwrap_or_default();
        (plugin_id, None, forwarded)
    } else if let Some(id) = tool_name.strip_prefix("plugin.") {
        (id.trim().to_string(), None, args.to_vec())
    } else if available_ids.contains(tool_name) {
        (tool_name.to_string(), None, args.to_vec())
    } else if let Some((prefix, suffix)) = tool_name.split_once('_') {
        if available_ids.contains(prefix) {
            (
                prefix.to_string(),
                Some(suffix.trim().to_string()),
                args.to_vec(),
            )
        } else {
            return None;
        }
    } else {
        return None;
    };

    if plugin_id.is_empty() || !available_ids.contains(&plugin_id) {
        return None;
    }

    let payload = serde_json::json!({
        "tool": tool_name,
        "plugin_id": plugin_id,
        "action": action,
        "args": forwarded_args,
    });
    Some((
        payload
            .get("plugin_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        payload.to_string(),
    ))
}

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
    plugin_registry: Option<&std::sync::Arc<crate::plugins::PluginRegistry>>,
    device_bridge: Option<&std::sync::Arc<crate::device_bridge::DeviceBridge>>,
    workspace_store: Option<&TaskWorkspaceStore>,
    browser_registry: Option<&crate::browser::BrowserSessionRegistry>,
    workspace_root: Option<&std::path::Path>,
) -> (bool, String, Option<String>) {
    use std::path::Path;
    let plugin_invocation = parse_plugin_tool_invocation(plugin_registry, tool_name, args);
    let is_plugin_candidate = plugin_invocation.is_some();
    let can_use_named_tool = executor.policy.can_use_tool(tool_name);
    let can_use_plugin_call =
        executor.policy.can_use_tool("plugin.call") || executor.policy.can_use_tool("plugin_call");
    if is_plugin_candidate && !can_use_plugin_call {
        return (
            false,
            "[plugin] tool not allowed by current profile (enable plugin.call)".to_string(),
            None,
        );
    }
    if !can_use_named_tool && !is_plugin_candidate {
        return (
            false,
            format!("[{}] tool not allowed by current profile", tool_name),
            None,
        );
    }
    let path_arg = |i: usize| args.get(i).map(|s| Path::new(s.as_str()));
    // For tools that take a single path arg: rejoin args so paths with spaces (e.g. "Cas d'usage.pdf") work when the LLM splits them.
    let path_arg_joined = |args: &[String]| -> String {
        if args.is_empty() {
            String::new()
        } else {
            args.join(" ").trim().to_string()
        }
    };
    let result = match tool_name {
        "workspace_graph_search" => {
            let Some(sp) = store_path else {
                return (
                    false,
                    "[workspace_graph_search] no task store path".to_string(),
                    None,
                );
            };
            let sp = sp.to_path_buf();
            let mut ws_id: Option<String> = None;
            let mut rest: Vec<String> = Vec::new();
            let mut it = args.iter().peekable();
            while let Some(a) = it.next() {
                if a == "--workspace" {
                    ws_id = it
                        .next()
                        .cloned()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                } else {
                    rest.push(a.clone());
                }
            }
            let query = rest.join(" ").trim().to_string();
            if query.is_empty() {
                (
                    false,
                    "[workspace_graph_search] usage: workspace_graph_search <query> [--workspace <uuid>]"
                        .to_string(),
                    None,
                )
            } else {
                let limit = 20usize;
                match tokio::task::spawn_blocking(move || {
                    let store = WorkspaceGraphStore::open(&sp)?;
                    store.search_graph_context(&query, limit, ws_id.as_deref())
                })
                .await
                {
                    Ok(Ok(lines)) => {
                        if lines.is_empty() {
                            (
                                true,
                                "[workspace_graph_search] no matching nodes".to_string(),
                                None,
                            )
                        } else {
                            (
                                true,
                                format!("[workspace_graph_search]\n{}", lines.join("\n")),
                                None,
                            )
                        }
                    }
                    Ok(Err(e)) => (false, format!("[workspace_graph_search] {}", e), None),
                    Err(e) => (
                        false,
                        format!("[workspace_graph_search] join: {}", e),
                        None,
                    ),
                }
            }
        }
        "read_file" => {
            let path_str = normalize_tool_path_hint(&path_arg_joined(args));
            if path_str.is_empty() {
                (false, "[read_file] usage: read_file <path>".to_string(), None)
            } else if is_workspace_virtual_path(&path_str) {
                let key = path_str
                    .trim_start_matches("workspace:/")
                    .trim_start_matches("workspace:")
                    .trim_start_matches('/')
                    .to_string();
                let key = normalize_apostrophes(&key);
                let lineage_id = workspace_lineage_root_task_id(task_id, store_path);
                let (key, _) = rewrite_workspace_plan_key_to_lineage_root(&key, lineage_id);
                if let Some(ws) = workspace_store {
                    let guard = ws.read().await;
                    // Prefer lineage root (where write_file stores); fall back to this task id.
                    let mem_map = guard
                        .get(&lineage_id)
                        .or_else(|| guard.get(&task_id));
                    if let Some(map) = mem_map {
                        if let Some(content) = map.get(&key) {
                            // Empty in-memory entry must not mask the real file on disk (orchestrator
                            // writes `.akasha/plan_*.md` with tokio::fs, not via this map).
                            if !content.is_empty() {
                                let (preview, truncated, total) = crate::tool_output::read_file_preview(content);
                                let body = format!(
                                    "[read_file workspace:{}] {} bytes: {}",
                                    key, total, preview
                                );
                                return (
                                    true,
                                    crate::tool_output::with_truncation_footer(
                                        body,
                                        truncated,
                                        total,
                                        "use grep_content or search_files to narrow, then read_file again",
                                    ),
                                    None,
                                );
                            }
                        }
                    }
                } else {
                    return (false, "[read_file] workspace paths require a workspace store.".to_string(), None);
                }
                let disk_path = strip_verbatim_prefix(
                    workspace_root
                        .map(|root| root.join(&key))
                        .or_else(|| std::env::current_dir().ok().map(|cwd| cwd.join(&key)))
                        .unwrap_or_else(|| Path::new(&key).to_path_buf()),
                );
                if !executor.policy.can_read(&disk_path) {
                    return (false, format!("[read_file workspace] path not allowed: {} (allowed_read_paths)", key), None);
                }
                if path_extension_is_pdf(&disk_path) {
                    return pdf_extract_message_from_disk(&disk_path, "read_file").await;
                }
                match executor.read_file(&disk_path).await {
                    Ok((content, res)) => {
                        let msg = if res.success {
                            let (preview, truncated, total) = crate::tool_output::read_file_preview(&content);
                            let body = format!(
                                "[read_file {}] {} bytes: {}",
                                disk_path.display(),
                                total,
                                preview
                            );
                            crate::tool_output::with_truncation_footer(
                                body,
                                truncated,
                                total,
                                "use grep_content or search_files to narrow, then read_file again",
                            )
                        } else {
                            format!("[read_file workspace] failed: {}", res.summary)
                        };
                        return (res.success, msg, None);
                    }
                    Err(e) => return (
                        false,
                        format!(
                            "[read_file workspace] failed: read error for {} at {}: {}",
                            key,
                            disk_path.display(),
                            e
                        ),
                        None,
                    ),
                };
            } else {
                let p = Path::new(&path_str);
                if path_extension_is_pdf(p) && executor.policy.can_read(p) {
                    pdf_extract_message_from_disk(p, "read_file").await
                } else {
                    match executor.read_file(p).await {
                        Ok((content, res)) => {
                            let msg = if res.success {
                                let (preview, truncated, total) = crate::tool_output::read_file_preview(&content);
                                let body = format!(
                                    "[read_file {}] {} bytes: {}",
                                    p.display(),
                                    total,
                                    preview
                                );
                                crate::tool_output::with_truncation_footer(
                                    body,
                                    truncated,
                                    total,
                                    "use grep_content or search_files to narrow, then read_file again",
                                )
                            } else {
                                format!("[read_file] failed: {}", res.summary)
                            };
                            (res.success, msg, None)
                        }
                        Err(e) => (false, format!("[read_file] failed: {}", e), None),
                    }
                }
            }
        }
        "run_command" => {
            let (vault_specs, cwd_flag, cmd, cmd_args) = parse_run_command_args(args);
            let cwd_resolved = match resolve_run_command_working_dir(cwd_flag.as_deref(), workspace_root, &executor.policy) {
                Ok(c) => c,
                Err(e) => return (false, format!("[run_command] {}", e), None),
            };
            let cwd_ref = cwd_resolved.as_deref();
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
            match executor.run_command(&cmd, &cmd_args, cwd_ref, env_ref).await {
                Ok((out, res)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let cwd_note = cwd_ref
                        .map(|p| format!(" cwd={}", p.display()))
                        .unwrap_or_default();
                    let exit = out
                        .status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "?".to_string());
                    let msg = if res.success {
                        crate::tool_output::shell_tool_success(
                            "run_command",
                            &cmd,
                            &cwd_note,
                            &exit,
                            stdout.as_ref(),
                            stderr.as_ref(),
                        )
                    } else {
                        crate::tool_output::shell_tool_failure(
                            "run_command",
                            &res.summary,
                            &exit,
                            stdout.as_ref(),
                            stderr.as_ref(),
                        )
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[run_command] failed: {}", e), None),
            }
        }
        "run_terminal" => {
            let (_vault_specs, cwd_flag, cmd, cmd_args) = parse_run_command_args(args);
            let cwd_resolved = match resolve_run_command_working_dir(cwd_flag.as_deref(), workspace_root, &executor.policy) {
                Ok(c) => c,
                Err(e) => return (false, format!("[run_terminal] {}", e), None),
            };
            let cwd_ref = cwd_resolved.as_deref();
            match executor.run_command(&cmd, &cmd_args, cwd_ref, None).await {
                Ok((out, res)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let cwd_note = cwd_ref
                        .map(|p| format!(" cwd={}", p.display()))
                        .unwrap_or_default();
                    let exit = out
                        .status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "?".to_string());
                    let msg = if res.success {
                        crate::tool_output::shell_tool_success(
                            "run_terminal",
                            &cmd,
                            &cwd_note,
                            &exit,
                            stdout.as_ref(),
                            stderr.as_ref(),
                        )
                    } else {
                        crate::tool_output::shell_tool_failure(
                            "run_terminal",
                            &res.summary,
                            &exit,
                            stdout.as_ref(),
                            stderr.as_ref(),
                        )
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[run_terminal] failed: {}", e), None),
            }
        }
        "run_command_background" => {
            let (_vault_specs, cwd_flag, cmd, cmd_args) = parse_run_command_args(args);
            let cwd_resolved = match resolve_run_command_working_dir(cwd_flag.as_deref(), workspace_root, &executor.policy) {
                Ok(c) => c,
                Err(e) => return (false, format!("[run_command_background] {}", e), None),
            };
            let cmd_display = format!("{} {}", cmd, cmd_args.join(" "));
            match process_registry {
                Some(reg) => {
                    let session_id = Uuid::new_v4();
                    let exec = executor.clone();
                    let cell: BackgroundResultCell = Arc::new(RwLock::new(None));
                    let cell_clone = cell.clone();
                    let reg_clone = reg.clone();
                    let task = tokio::spawn(async move {
                        let cwd_ref = cwd_resolved.as_deref();
                        let result = exec.run_command(&cmd, &cmd_args, cwd_ref, None).await;
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
                                    let exit = out
                                        .status
                                        .code()
                                        .map(|c| c.to_string())
                                        .unwrap_or_else(|| "?".to_string());
                                    let (so, se, trunc, note) = crate::tool_output::format_truncated_streams(
                                        stdout.as_ref(),
                                        stderr.as_ref(),
                                        crate::tool_output::RUN_COMMAND_STDOUT_MAX,
                                        crate::tool_output::RUN_COMMAND_STDERR_MAX,
                                    );
                                    let mut base_msg = format!(
                                        "[process poll {}] done — exit {} stdout: {} stderr: {}",
                                        id, exit, so, se
                                    );
                                    if trunc {
                                        base_msg.push('\n');
                                        base_msg.push_str(&note);
                                    }
                                    if res.success {
                                        (true, base_msg, None)
                                    } else {
                                        (
                                            false,
                                            format!("{} | summary: {}", base_msg, res.summary),
                                            None,
                                        )
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
                return (false, "[memory_store] usage: memory_store <content> <source> [link_to: uuid+kind,...|uuid,...] [link_kind: default_for_plain_uuids]".to_string(), None);
            }
            let explicit_links = parse_memory_store_explicit_links(args.get(2..).unwrap_or(&[]));
            match long_term_client {
                Some(client) => {
                    let client = client.clone();
                    let content = content.to_string();
                    let source = source.to_string();
                    let out = tokio::task::spawn_blocking(move || client.promote(content, source, None, None, None, None, None, None, explicit_links))
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
                                    preferred_task_type: None,
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
            if !executor.policy.browser_enabled {
                return (
                    false,
                    "[browser] Browser automation is disabled. Set browser_enabled: true in tools_policy.yaml and install Playwright (npx playwright install chromium).".to_string(),
                    None,
                );
            }
            let Some(registry) = browser_registry else {
                return (false, "[browser] Browser registry not available.".to_string(), None);
            };
            let Some(runner_path) = crate::browser::find_playwright_runner_path() else {
                return (
                    false,
                    "[browser] Playwright runner not found. Install: copy scripts/playwright-runner next to the executable (playwright-runner/run.mjs), or under %USERPROFILE%\\akasha\\playwright-runner, or set AKASHA_PLAYWRIGHT_RUNNER / AKASHA_DATA_DIR (see docs). Then npm install in that folder.".to_string(),
                    None,
                );
            };
            let headless = executor.policy.browser_headless;
            let action_timeout = executor.policy.browser_action_timeout_secs;
            let session_timeout = executor.policy.browser_session_timeout_secs;

            // Enforce per-session max duration: if the existing session has exceeded the
            // configured timeout, close and evict it before dispatching the command.
            {
                let mut g = registry.write().await;
                if let Some(s) = g.get(&task_id) {
                    if s.started_at.elapsed().as_secs() >= session_timeout {
                        tracing::info!(task_id = %task_id, timeout_secs = session_timeout, "[browser] session timed out, closing");
                        if let Some(mut sess) = g.remove(&task_id) {
                            let _ = sess.close().await;
                        }
                    }
                }
            }

            if sub == "navigate" {
                let Some(url_arg) = args.get(1) else {
                    return (false, "[browser] usage: browser navigate <url>".to_string(), None);
                };
                let url = url_arg.trim();
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    return (false, "[browser] navigate requires an http or https URL".to_string(), None);
                }
                let host = url.parse::<url::Url>().ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
                if !executor.policy.can_use_browser_domain(&host) {
                    return (false, format!("[browser] Domain not allowed: {}", host), None);
                }
                let mut g = registry.write().await;
                let session = if let Some(s) = g.get_mut(&task_id) {
                    let res = s.send_command(&serde_json::json!({ "cmd": "navigate", "params": { "url": url, "timeout_secs": action_timeout } })).await;
                    drop(g);
                    res
                } else {
                    drop(g);
                    match crate::browser::create_browser_session(&runner_path, headless, action_timeout).await {
                        Ok(mut new_session) => {
                            let res = new_session
                                .send_command(&serde_json::json!({ "cmd": "navigate", "params": { "url": url, "timeout_secs": action_timeout } }))
                                .await;
                            // Only register if IPC still works; on Err (e.g. runner stdout EOF) the child may
                            // already be reaped — inserting a dead session poisons later navigate/snapshot calls.
                            if res.is_ok() {
                                let mut g = registry.write().await;
                                g.insert(task_id, new_session);
                            }
                            res
                        }
                        Err(e) => return (false, format!("[browser] error: {}", e), None),
                    }
                };
                match session {
                    Ok(resp) => {
                        let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        if ok {
                            let result = resp.get("result");
                            let msg = if let Some(r) = result {
                                if let Some(title) = r.get("title").and_then(|v| v.as_str()) {
                                    format!("[browser] Navigated to {} (title: {}).", url, title)
                                } else {
                                    format!("[browser] Navigated to {}.", url)
                                }
                            } else {
                                format!("[browser] Navigated to {}.", url)
                            };
                            (true, msg, None)
                        } else {
                            let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("Navigate failed");
                            (false, format!("[browser] {}", err), None)
                        }
                    }
                    Err(e) => (false, format!("[browser] error: {}", e), None),
                }
            } else if sub == "snapshot" {
                let mut g = registry.write().await;
                let Some(session) = g.get_mut(&task_id) else {
                    return (false, "[browser] Navigate to a page first (browser navigate <url>).".to_string(), None);
                };
                let resp = session.send_command(&serde_json::json!({ "cmd": "snapshot" })).await;
                drop(g);
                match resp {
                    Ok(resp) => {
                        let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        if ok {
                            let result = resp.get("result").and_then(|r| r.get("text").and_then(|t| t.as_str())).unwrap_or("");
                            let (preview, total, trunc) =
                                crate::tool_output::truncate_utf8_by_bytes(result, crate::tool_output::BROWSER_SNAPSHOT_TEXT_MAX);
                            let base = format!(
                                "[browser] Snapshot ({} bytes):\n{}",
                                total,
                                preview
                            );
                            let msg = crate::tool_output::with_truncation_footer(
                                base,
                                trunc,
                                total,
                                "use web_fetch on static URLs when applicable or navigate to a narrower page",
                            );
                            (true, msg, None)
                        } else {
                            let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("Snapshot failed");
                            (false, format!("[browser] {}", err), None)
                        }
                    }
                    Err(e) => (false, format!("[browser] error: {}", e), None),
                }
            } else if sub == "screenshot" {
                (true, "[browser] Screenshot is planned for Phase 2. For now use device_invoke synthetic_input keyboard shortcut (e.g. Win+Shift+S).".to_string(), None)
            } else if sub == "click" || sub == "fill" || sub == "wait" {
                (true, "[browser] click, fill, wait are planned for Phase 2.".to_string(), None)
            } else {
                (false, "[browser] usage: browser navigate <url> | browser snapshot | browser screenshot (Phase 2).".to_string(), None)
            }
        }
        "install_playwright" => {
            if !executor.policy.browser_enabled {
                return (
                    false,
                    "[install_playwright] Browser automation is disabled. Set browser_enabled: true in tools_policy.yaml.".to_string(),
                    None,
                );
            }
            let Some(runner_path) = crate::browser::find_playwright_runner_path() else {
                return (
                    false,
                    "[install_playwright] Playwright runner not found. Copy scripts/playwright-runner beside the executable or under ~/akasha/playwright-runner, or set AKASHA_PLAYWRIGHT_RUNNER / AKASHA_DATA_DIR.".to_string(),
                    None,
                );
            };
            let Some(runner_dir) = crate::browser::playwright_runner_dir(&runner_path) else {
                return (
                    false,
                    "[install_playwright] Could not resolve runner directory.".to_string(),
                    None,
                );
            };
            match crate::browser::ensure_playwright_chromium(&runner_dir).await {
                Ok(()) => (
                    true,
                    format!(
                        "[install_playwright] npm install and playwright install chromium completed in {}.",
                        runner_dir.display()
                    ),
                    None,
                ),
                Err(e) => (false, format!("[install_playwright] {}", e), None),
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
            let path_str = path_arg_joined(args);
            let path_str = path_str.trim();
            if path_str.is_empty() {
                (false, "[pdf] usage: pdf <path> — path must be in allowed_read_paths".to_string(), None)
            } else if path_str.starts_with("workspace:/") || path_str.starts_with("workspace:") {
                let key = path_str
                    .trim_start_matches("workspace:/")
                    .trim_start_matches("workspace:")
                    .trim_start_matches('/')
                    .to_string();
                let key = normalize_apostrophes(&key);
                let disk_path = strip_verbatim_prefix(
                    workspace_root
                        .map(|root| root.join(&key))
                        .or_else(|| std::env::current_dir().ok().map(|cwd| cwd.join(&key)))
                        .unwrap_or_else(|| Path::new(&key).to_path_buf()),
                );
                if !executor.policy.can_read(&disk_path) {
                    (false, "[pdf] path not allowed by policy (allowed_read_paths)".to_string(), None)
                } else {
                    match tokio::fs::read(&disk_path).await {
                        Ok(bytes) => match pdf_extract::extract_text_from_mem(&bytes) {
                            Ok(text) => {
                                let preview = if text.len() > 2000 { format!("{}…", text.chars().take(2000).collect::<String>()) } else { text.clone() };
                                (true, format!("[pdf {}] extracted {} chars:\n{}", disk_path.display(), text.len(), preview), None)
                            }
                            Err(e) => (false, format!("[pdf] extraction failed: {}", e), None),
                        },
                        Err(e) => (false, format!("[pdf] read failed: {} (ensure file exists at {})", e, disk_path.display()), None),
                    }
                }
            } else {
                let path = Path::new(path_str);
                if !executor.policy.can_read(path) {
                    (false, "[pdf] path not allowed by policy (allowed_read_paths)".to_string(), None)
                } else {
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
            }
        }
        "search_files" => {
            let (args, respect_gitignore, _) = strip_file_search_flags(args);
            let raw_dir = args.get(0).map(String::as_str).unwrap_or(".");
            let dir_pb = resolve_tool_disk_path(raw_dir, workspace_root);
            let pattern = args.get(1).map(String::as_str).unwrap_or("*");
            let dir = dir_pb.as_path();
            match executor.search_files(dir, pattern, respect_gitignore).await {
                Ok((paths, res)) => {
                    let msg = if res.success {
                        let n = paths.len();
                        const SHOW: usize = 20;
                        let lines: Vec<String> = paths.iter().take(SHOW).map(|p| p.display().to_string()).collect();
                        let listing = lines.join("\n");
                        let mut m = format!(
                            "[search_files] count: {}\ndir: {}\npattern: {}\n",
                            n,
                            dir.display(),
                            pattern
                        );
                        if n == 0 {
                            m.push_str("matches: (none — 0 files)\n");
                        } else {
                            m.push_str("paths:\n");
                            m.push_str(&listing);
                            m.push('\n');
                            if n > SHOW {
                                m.push_str(&format!(
                                    "showing: {} of {} — narrow pattern or dir to list fewer\n",
                                    SHOW, n
                                ));
                            }
                        }
                        m
                    } else {
                        format!("[search_files] failed: {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[search_files] failed: {}", e), None),
            }
        }
        "grep_content" => {
            let (args, respect_gitignore, use_regex) = strip_file_search_flags(args);
            let raw_dir = args.get(0).map(String::as_str).unwrap_or(".");
            let dir_pb = resolve_tool_disk_path(raw_dir, workspace_root);
            let dir = dir_pb.as_path();
            let pattern = args.get(1).map(String::as_str).unwrap_or("");
            let file_glob = args.get(2).map(String::as_str).filter(|s| !s.is_empty());
            if pattern.is_empty() {
                return (
                    false,
                    "[grep_content] usage: grep_content <dir> <pattern> [file_glob] [--regex] [--no-ignore]".to_string(),
                    None,
                );
            }
            match executor
                .grep_content(dir, pattern, file_glob, 50, use_regex, respect_gitignore)
                .await
            {
                Ok((matches, res)) => {
                    let msg = if res.success {
                        let collected = matches.len();
                        const DISPLAY: usize = 30;
                        const CAP: usize = 50;
                        if collected == 0 {
                            format!(
                                "[grep_content] 0 matches (dir={} pattern={} regex={})",
                                dir.display(),
                                pattern,
                                use_regex
                            )
                        } else {
                            let lines: Vec<String> = matches
                                .iter()
                                .take(DISPLAY)
                                .map(|(p, n, line)| format!("{}:{}: {}", p.display(), n, line.trim()))
                                .collect();
                            let listing = lines.join("\n");
                            let mut m = format!(
                                "[grep_content] matches_collected: {} (showing up to {} lines)\n{}",
                                collected, DISPLAY, listing
                            );
                            if collected >= CAP {
                                m.push_str(&format!(
                                    "\n(at least {} matches — output capped; narrow pattern, dir, or file_glob)\n",
                                    CAP
                                ));
                            }
                            m
                        }
                    } else {
                        format!("[grep_content] failed: {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[grep_content] failed: {}", e), None),
            }
        }
        "write_file" => {
            let Some((path_str, content)) = parse_write_file_request(args) else {
                return (false, "[write_file] usage: write_file <path> <content>".to_string(), None);
            };
            if is_workspace_virtual_path(&path_str) {
                match workspace_store {
                    Some(ws) => {
                        let key = path_str
                            .trim_start_matches("workspace:/")
                            .trim_start_matches("workspace:")
                            .trim_start_matches('/')
                            .to_string();
                        let mut key = key.trim().trim_matches('`').trim_matches('"').to_string();
                        if key.ends_with('#') {
                            key.pop();
                        }
                        let lineage_task_id = workspace_lineage_root_task_id(task_id, store_path);
                        let (key, _) =
                            rewrite_workspace_plan_key_to_lineage_root(&key, lineage_task_id);
                        let mut guard = ws.write().await;
                        let per_task = guard.entry(lineage_task_id).or_default();
                        let previous_mem = per_task.get(&key).cloned().unwrap_or_default();
                        // Only prefer the longer previous content for plan-trace files
                        // (.akasha/plan_*.md) where provider truncation is a known risk.
                        // For all other files, always use the new content so legitimate
                        // shortening edits (e.g. removing placeholder sections) are honoured.
                        let is_plan_trace = key.starts_with(".akasha/plan_") && key.ends_with(".md");
                        let effective_content = if is_plan_trace
                            && !previous_mem.is_empty()
                            && content.chars().count() < previous_mem.chars().count()
                        {
                            previous_mem.clone()
                        } else {
                            content.clone()
                        };
                        per_task.insert(key.clone(), effective_content.clone());
                        drop(guard);
                        // Also write to disk under data_dir (workspace root = store_path.parent())
                        let rel_path = Path::new(&key);
                        if executor.policy.can_write(rel_path) {
                            let disk_path_opt = workspace_root.map(|root| root.join(&key))
                                .or_else(|| std::env::current_dir().ok().map(|cwd| cwd.join(&key)))
                                .map(strip_verbatim_prefix);
                            if let Some(disk_path) = disk_path_opt {
                                if let Some(parent) = disk_path.parent() {
                                    let _ = tokio::fs::create_dir_all(parent).await;
                                }
                                if let Some(r) = workspace_root {
                                    if crate::studio::is_strictly_under_studio_root(&disk_path, r) {
                                        if let Some(msg) =
                                            crate::api_studio::studio_reject_polluted_code_content(&disk_path, &effective_content)
                                        {
                                            return (false, format!("[write_file] {}", msg), None);
                                        }
                                    }
                                }
                                if tokio::fs::write(&disk_path, &effective_content).await.is_ok() {
                                    return (true, format!("[write_file workspace:{}] saved (disk).", key), None);
                                }
                            }
                        }
                        return (true, format!("[write_file workspace:{}] saved.", key), None);
                    }
                    None => return (false, "[write_file] workspace paths require a workspace store.".to_string(), None),
                }
            }
            let disk_path = resolve_tool_disk_path(path_str.trim(), workspace_root);
            if let Some(root) = workspace_root {
                if crate::studio::is_strictly_under_studio_root(&disk_path, root) {
                    if let Some(msg) = crate::api_studio::studio_reject_polluted_code_content(&disk_path, &content) {
                        return (false, format!("[write_file] {}", msg), None);
                    }
                }
            }
            let path = Path::new(&path_str);
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
            let path_str = match args.get(0) {
                Some(s) => s.as_str(),
                None => return (false, "[search_replace] usage: search_replace <path> <search> | <replace>".to_string(), None),
            };
            let is_workspace = path_str.starts_with("workspace:/") || path_str.starts_with("workspace:");
            let path_str_rewritten = rewrite_workspace_plan_path_str(path_str, task_id, store_path);
            let path_str = path_str_rewritten.as_str();
            let disk_path = resolve_tool_disk_path(path_str, workspace_root);
            let rest = args.get(1..).map(|a| a.join(" ")).unwrap_or_default();
            let Some((search, replace)) = rest
                .split_once('|')
                .map(|(s, r)| (s.trim().to_string(), r.trim().to_string()))
            else {
                return (false, "[search_replace] usage: search_replace <path> <search> | <replace>".to_string(), None);
            };
            match executor.search_replace(&disk_path, &search, &replace).await {
                Ok(res) => {
                    if res.success && is_workspace {
                        sync_workspace_store_from_disk(workspace_store, task_id, store_path, path_str, &disk_path).await;
                    }
                    let msg = if res.success {
                        format!("[search_replace {}] {}", disk_path.display(), res.summary)
                    } else {
                        format!("[search_replace] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[search_replace] error: {}", e), None),
            }
        }
        "edit_file" => {
            let path_str = match args.get(0) {
                Some(s) => s.as_str(),
                None => return (false, "[edit_file] usage: edit_file <path> <start_line> <end_line> <new_content>".to_string(), None),
            };
            let is_workspace = path_str.starts_with("workspace:/") || path_str.starts_with("workspace:");
            let path_str_rewritten = rewrite_workspace_plan_path_str(path_str, task_id, store_path);
            let path_str = path_str_rewritten.as_str();
            let disk_path = resolve_tool_disk_path(path_str, workspace_root);
            let start_line = args.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let end_line = args.get(2).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let new_content = args.get(3..).map(|a| a.join("\n")).unwrap_or_default();
            match executor.edit_file(&disk_path, start_line, end_line, &new_content).await {
                Ok(res) => {
                    if res.success && is_workspace {
                        sync_workspace_store_from_disk(workspace_store, task_id, store_path, path_str, &disk_path).await;
                    }
                    let msg = if res.success {
                        format!("[edit_file {}] {}", disk_path.display(), res.summary)
                    } else {
                        format!("[edit_file] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[edit_file] error: {}", e), None),
            }
        }
        "apply_patch" => {
            let path_str = match args.get(0) {
                Some(s) => s.as_str(),
                None => return (false, "[apply_patch] usage: apply_patch <path> <patch_content>".to_string(), None),
            };
            let is_workspace = path_str.starts_with("workspace:/") || path_str.starts_with("workspace:");
            let path_str_rewritten = rewrite_workspace_plan_path_str(path_str, task_id, store_path);
            let path_str = path_str_rewritten.as_str();
            let disk_path = resolve_tool_disk_path(path_str, workspace_root);
            let patch_content = args.get(1..).map(|a| a.join("\n")).unwrap_or_default();
            match executor.apply_patch(&disk_path, &patch_content).await {
                Ok(res) => {
                    if res.success && is_workspace {
                        sync_workspace_store_from_disk(workspace_store, task_id, store_path, path_str, &disk_path).await;
                    }
                    let msg = if res.success {
                        format!("[apply_patch {}] {}", disk_path.display(), res.summary)
                    } else {
                        format!("[apply_patch] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[apply_patch] error: {}", e), None),
            }
        }
        "file_diff" => {
            let path_a_str = args.get(0).map(String::as_str).unwrap_or("");
            let path_b_str = args.get(1).map(String::as_str).unwrap_or("");
            if path_a_str.is_empty() || path_b_str.is_empty() {
                return (false, "[file_diff] usage: file_diff <path_a> <path_b>".to_string(), None);
            }
            let disk_a = resolve_tool_disk_path(path_a_str, workspace_root);
            let disk_b = resolve_tool_disk_path(path_b_str, workspace_root);
            match executor.file_diff(&disk_a, &disk_b).await {
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
            }
        }
        "diff_unified" => {
            let path_a_str = args.get(0).map(String::as_str).unwrap_or("");
            let path_b_str = args.get(1).map(String::as_str).unwrap_or("");
            if path_a_str.is_empty() || path_b_str.is_empty() {
                return (false, "[diff_unified] usage: diff_unified <path_a> <path_b> [context_lines]".to_string(), None);
            }
            let ctx = args
                .get(2)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(3);
            let disk_a = resolve_tool_disk_path(path_a_str, workspace_root);
            let disk_b = resolve_tool_disk_path(path_b_str, workspace_root);
            match executor.file_diff_unified(&disk_a, &disk_b, ctx).await {
                Ok((diff, res)) => {
                    let msg = if res.success {
                        let preview = if diff.len() <= 800 {
                            diff.as_str()
                        } else {
                            &diff[..diff.floor_char_boundary(800)]
                        };
                        format!("[diff_unified] {} — {}", res.summary, preview)
                    } else {
                        format!("[diff_unified] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[diff_unified] error: {}", e), None),
            }
        }
        "dir_compare" => {
            let dir_a_str = args.get(0).map(String::as_str).unwrap_or("");
            let dir_b_str = args.get(1).map(String::as_str).unwrap_or("");
            if dir_a_str.is_empty() || dir_b_str.is_empty() {
                return (false, "[dir_compare] usage: dir_compare <dir_a> <dir_b> [max_depth] [max_files]".to_string(), None);
            }
            let max_depth = args
                .get(2)
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(8);
            let max_files = args
                .get(3)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(100);
            let disk_a = resolve_tool_disk_path(dir_a_str, workspace_root);
            let disk_b = resolve_tool_disk_path(dir_b_str, workspace_root);
            match executor
                .compare_dirs(&disk_a, &disk_b, max_depth, max_files)
                .await
            {
                Ok((report, res)) => {
                    let msg = if res.success {
                        let preview = if report.len() <= 1200 {
                            report.as_str()
                        } else {
                            &report[..report.floor_char_boundary(1200)]
                        };
                        format!("[dir_compare] {} — {}", res.summary, preview)
                    } else {
                        format!("[dir_compare] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[dir_compare] error: {}", e), None),
            }
        }
        "git_status" => {
            let repo_str = args.get(0).map(String::as_str).unwrap_or("");
            if repo_str.is_empty() {
                return (false, "[git_status] usage: git_status <repo>".to_string(), None);
            }
            let disk = resolve_tool_disk_path(repo_str, workspace_root);
            match executor.git_status(&disk).await {
                Ok((out, res)) => {
                    let text = out.trim();
                    let (frag, total, trunc) =
                        crate::tool_output::truncate_utf8_by_bytes(text, crate::tool_output::GIT_TEXT_MAX);
                    let base = format!("[git_status] {} — {}", res.summary, frag);
                    let msg = crate::tool_output::with_truncation_footer(
                        base,
                        trunc,
                        total,
                        "output truncated — run git status in run_command with > file if you need the full text",
                    );
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[git_status] failed: {}", e), None),
            }
        }
        "git_diff" => {
            let repo_str = args.get(0).map(String::as_str).unwrap_or("");
            if repo_str.is_empty() {
                return (false, "[git_diff] usage: git_diff <repo> [--staged] [pathspec...]".to_string(), None);
            }
            let disk = resolve_tool_disk_path(repo_str, workspace_root);
            let mut staged = false;
            let mut rest_start = 1usize;
            if args.get(1).map(|s| s.as_str()) == Some("--staged") {
                staged = true;
                rest_start = 2;
            }
            let pathspecs: Vec<String> = args.get(rest_start..).map(|s| s.to_vec()).unwrap_or_default();
            match executor.git_diff(&disk, staged, &pathspecs).await {
                Ok((out, res)) => {
                    let text = out.trim();
                    let (frag, total, trunc) =
                        crate::tool_output::truncate_utf8_by_bytes(text, crate::tool_output::GIT_TEXT_MAX);
                    let base = format!("[git_diff] {} — {}", res.summary, frag);
                    let msg = crate::tool_output::with_truncation_footer(
                        base,
                        trunc,
                        total,
                        "narrow with pathspec arguments on git_diff or use file_diff for two files",
                    );
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[git_diff] failed: {}", e), None),
            }
        }
        "git_log" => {
            let repo_str = args.get(0).map(String::as_str).unwrap_or("");
            if repo_str.is_empty() {
                return (false, "[git_log] usage: git_log <repo> [n]".to_string(), None);
            }
            let n = args.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(20);
            let disk = resolve_tool_disk_path(repo_str, workspace_root);
            match executor.git_log(&disk, n).await {
                Ok((out, res)) => {
                    let text = out.trim();
                    let (frag, total, trunc) =
                        crate::tool_output::truncate_utf8_by_bytes(text, crate::tool_output::GIT_TEXT_MAX);
                    let base = format!("[git_log] {} — {}", res.summary, frag);
                    let msg = crate::tool_output::with_truncation_footer(
                        base,
                        trunc,
                        total,
                        "reduce n on git_log or save to a file and read_file with grep_content",
                    );
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[git_log] failed: {}", e), None),
            }
        }
        "git_rev_parse" => {
            let repo_str = args.get(0).map(String::as_str).unwrap_or("");
            if repo_str.is_empty() {
                return (false, "[git_rev_parse] usage: git_rev_parse <repo>".to_string(), None);
            }
            let disk = resolve_tool_disk_path(repo_str, workspace_root);
            match executor.git_rev_parse_head(&disk).await {
                Ok((out, res)) => {
                    let text = out.trim();
                    let (frag, total, trunc) =
                        crate::tool_output::truncate_utf8_by_bytes(text, crate::tool_output::GIT_TEXT_MAX);
                    let base = format!("[git_rev_parse] {} — {}", res.summary, frag);
                    let msg = crate::tool_output::with_truncation_footer(
                        base,
                        trunc,
                        total,
                        "output truncated — retry via run_command if needed",
                    );
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[git_rev_parse] failed: {}", e), None),
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
                        let (preview, total, trunc) = crate::tool_output::truncate_utf8_by_bytes(&body, 500);
                        let base = format!("[web_fetch] {} — {}", res.summary, preview);
                        crate::tool_output::with_truncation_footer(
                            base,
                            trunc,
                            total,
                            "use a more specific URL or browser snapshot for long pages",
                        )
                    } else {
                        format!("[web_fetch] failed: {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[web_fetch] failed: {}", e), None),
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
                        let (preview, total, trunc) = crate::tool_output::truncate_utf8_by_bytes(&body, 600);
                        let base = format!("[web_search] {} — {}", res.summary, preview);
                        crate::tool_output::with_truncation_footer(
                            base,
                            trunc,
                            total,
                            "use web_fetch on a result URL for full article text",
                        )
                    } else {
                        format!("[web_search] failed: {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[web_search] failed: {}", e), None),
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
            let (prompt, size_owned) = parse_generate_image_tool_args(args);
            let prompt = prompt.trim();
            if prompt.is_empty() {
                (false, "[generate_image] usage: generate_image <prompt> [size]".to_string(), None)
            } else {
                let size = size_owned.as_deref();
                let data_dir = store_path.and_then(|p| p.parent()).unwrap_or_else(|| Path::new("."));
                match crate::image_generation::generate_image_impl(data_dir, prompt, size).await {
                    Ok((msg, data_url)) => (true, msg, Some(data_url)),
                    Err(e) => (false, e, None),
                }
            }
        }
        "speech_synthesize" => {
            let text = args.get(0).map(String::as_str).unwrap_or("").trim();
            if text.is_empty() {
                (false, "[speech_synthesize] usage: speech_synthesize <text>".to_string(), None)
            } else {
                let data_dir = store_path.and_then(|p| p.parent()).unwrap_or_else(|| Path::new("."));
                match crate::voice::speech_synthesize_impl(data_dir, text).await {
                    Ok((msg, data_url)) => (true, msg, Some(data_url)),
                    Err(e) => (false, e, None),
                }
            }
        }
        "speech_transcribe" => {
            let audio_input = args.get(0).map(String::as_str).unwrap_or("").trim();
            let data_dir = store_path.and_then(|p| p.parent()).unwrap_or_else(|| Path::new("."));
            match crate::voice::speech_transcribe_impl(data_dir, audio_input).await {
                Ok(text) => (true, format!("[speech_transcribe] {}", text), None),
                Err(e) => (false, e, None),
            }
        }
        _ => {
            if let Some((plugin_id, plugin_payload)) = plugin_invocation {
                match plugin_registry {
                    Some(r) => {
                        // WASM + host imports (e.g. http_fetch) must not run on the Tokio async worker:
                        // blocking HTTP and Wasmtime host callbacks can abort the process if nested on a worker thread.
                        let reg = Arc::clone(r);
                        let pid = plugin_id.clone();
                        let pl = plugin_payload.clone();
                        match tokio::task::spawn_blocking(move || reg.call_tool(&pid, &pl)).await {
                            Ok(Ok(out)) => {
                                if out.trim().is_empty() {
                                    return (
                                        false,
                                        format!(
                                            "[plugin:{}] execution returned empty output (check plugin input/action)",
                                            plugin_id
                                        ),
                                        None,
                                    );
                                }
                                let plugin_ok = serde_json::from_str::<serde_json::Value>(&out)
                                    .ok()
                                    .and_then(|v| v.get("ok").and_then(|b| b.as_bool()));
                                let preview = if out.chars().count() > 600 {
                                    format!("{}…", out.chars().take(600).collect::<String>())
                                } else {
                                    out
                                };
                                if plugin_ok == Some(false) {
                                    return (
                                        false,
                                        format!(
                                            "[plugin:{}] execution failed: plugin returned ok=false: {}",
                                            plugin_id, preview
                                        ),
                                        None,
                                    );
                                }
                                (
                                    true,
                                    format!("[plugin:{}] {}", plugin_id, preview),
                                    None,
                                )
                            }
                            Ok(Err(err)) => (
                                false,
                                format!("[plugin:{}] execution failed: {}", plugin_id, err),
                                None,
                            ),
                            Err(join_err) => (
                                false,
                                format!(
                                    "[plugin:{}] execution failed: {}",
                                    plugin_id,
                                    if join_err.is_panic() {
                                        "internal error (plugin task panicked)".to_string()
                                    } else {
                                        join_err.to_string()
                                    }
                                ),
                                None,
                            ),
                        }
                    }
                    None => (
                        false,
                        format!(
                            "[plugin:{}] execution failed: plugin registry unavailable",
                            plugin_id
                        ),
                        None,
                    ),
                }
            } else {
                let names: Vec<&str> = AVAILABLE_TOOLS.iter().map(|(n, _)| *n).collect();
                (false, format!("[{}] unknown tool. For shell commands, use: TOOL: run_command <cmd> .... Available: {}.", tool_name, names.join(", ")), None)
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
    if short_term.get_compaction_count(session_id).await
        >= crate::memory::MAX_COMPACTIONS_PER_SESSION
    {
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
        top_p: None,
        top_k: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        num_ctx: None,
        num_gpu: None,
        thinking_level: None,
    };
    let compaction_timeout = env_duration_ms("AKASHA_COMPACTION_TIMEOUT_MS", 1_500);
    match tokio::time::timeout(compaction_timeout, llm_router.complete(&req)).await {
        Err(_) => {
            tracing::debug!(
                session_id,
                timeout_ms = compaction_timeout.as_millis() as u64,
                "Short-term compaction skipped: budget exceeded"
            );
        }
        Ok(Err(e)) => tracing::warn!(error = %e, "Compaction LLM failed, keeping full history"),
        Ok(Ok(resp)) => {
            let summary = resp.text.trim();
            if !summary.is_empty() {
                short_term
                    .replace_oldest_with_summary(session_id, summary.to_string(), to_summarize)
                    .await;
                short_term.increment_compaction_count(session_id).await;
                tracing::debug!(session_id, to_summarize, "Short-term memory compacted");
                // Promote summary to long-term memory (spec 06)
                if let Some(client) = long_term_client {
                    let summary = summary.to_string();
                    let session_id_attr = session_id.to_string();
                    let client = client.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = client.promote(summary.clone(), "compaction".to_string(), None, None, Some(session_id_attr.clone()), Some(1), Some("session".to_string()), None, None) {
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
    }
}

/// At daemon startup: if yesterday's short-term file exists, summarize it via LLM and promote to long-term (source "daily_summary").
/// Skips if a daily summary for that date already exists in long-term memory.
pub async fn summarize_yesterday_and_promote(
    short_term_dir: PathBuf,
    llm_router: Arc<akasha_llm::LLMRouter>,
    long_term_client: Option<LongTermMemoryClient>,
) {
    let Some(client) = long_term_client else {
        return;
    };
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
    let blob_capped = crate::llm_prompt_cap::truncate_utf8_bytes(
        &blob,
        crate::llm_prompt_cap::SYSTEM_PROMPT_FIELD_MAX_BYTES,
    );
    let summary_prompt = format!(
        "Summarize in a short synthetic paragraph (5 to 10 lines) the day of {}: topics covered, decisions, projects or important information. \
Factual response in English.\n\n{}",
        session_id.trim_start_matches("day-"),
        blob_capped
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
        top_p: None,
        top_k: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        num_ctx: None,
        num_gpu: None,
        thinking_level: None,
    };
    match llm_router.complete(&req).await {
        Ok(resp) => {
            let summary = resp.text.trim();
            if !summary.is_empty() {
                let content = format!(
                    "Summary for {}: {}",
                    session_id.trim_start_matches("day-"),
                    summary
                );
                let client = client.clone();
                match tokio::task::spawn_blocking(move || {
                    let res = client.promote(
                        content.clone(),
                        "daily_summary".to_string(),
                        None,
                        None,
                        None,
                        Some(1),
                        None,
                        None,
                        None,
                    );
                    if res.is_ok() {
                        let _ = client.emit_event(
                            "memory_promoted".to_string(),
                            content,
                            None,
                            None,
                            None,
                            None,
                            Some(1),
                            None,
                            Some("daily_summary".to_string()),
                        );
                    }
                    res
                })
                .await
                {
                    Ok(Ok(())) => {
                        tracing::info!(session_id = %session_id, "Yesterday summarized and stored in long-term memory")
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, "Daily summary promote to long-term failed")
                    }
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
    } else if lower.contains("write_file")
        || lower.contains("search_replace")
        || lower.contains("edit_file")
    {
        "Writing the file…".to_string()
    } else if lower.contains("read_file") {
        "Reading the file…".to_string()
    } else if lower.contains("generate_image") {
        "Generating the image…".to_string()
    } else if lower.contains("speech_synthesize") {
        "Synthesizing speech…".to_string()
    } else if lower.contains("speech_transcribe") {
        "Transcribing audio…".to_string()
    } else if lower.contains("device_invoke") && first_arg.to_lowercase().contains("camera") {
        "Capturing with camera…".to_string()
    } else if lower == "browser" && first_arg.eq_ignore_ascii_case("navigate") {
        "Opening in browser…".to_string()
    } else if lower == "install_playwright" {
        "Installing Playwright Chromium…".to_string()
    } else if lower.contains("run_command") {
        "Running the command…".to_string()
    } else if lower == "ask_user" {
        "Waiting for your input…".to_string()
    } else {
        format!("Running {}…", tool)
    }
}

/// Extract a short name (how to call the user) from a message during onboarding.
/// Simple heuristic: first word, or first two words if the message is very short. Skips common prefixes.
fn extract_how_to_call_from_message(msg: &str) -> Option<String> {
    let msg = msg.trim();
    if msg.is_empty() || msg.len() > 80 {
        return None;
    }
    if classify_small_talk_message(msg).is_some() {
        return None;
    }
    let lower = msg.to_lowercase();
    let skip_prefixes = [
        "je m'appelle",
        "je suis",
        "c'est",
        "my name is",
        "i'm",
        "i am",
        "call me",
        "moi c'est",
    ];
    let mut text = msg;
    for prefix in skip_prefixes {
        if lower.starts_with(prefix) {
            text = msg[prefix.len()..].trim();
            if text.is_empty() {
                return None;
            }
            break;
        }
    }
    let words: Vec<&str> = text.split_whitespace().take(2).collect();
    let name = if words.len() == 1 {
        words[0].trim().to_string()
    } else if words.len() == 2 && text.len() <= 30 {
        format!("{} {}", words[0].trim(), words[1].trim())
    } else {
        words[0].trim().to_string()
    };
    let name = name
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '\'')
        .to_string();
    if name.is_empty() || name.len() > 50 {
        None
    } else {
        Some(name)
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
    format!(
        "On it — looking into {}. You can follow progress in the Tasks tab.",
        preview
    )
}

#[derive(Debug, Clone, Copy)]
struct MemoryProfile {
    recent_turns_limit: usize,
    recent_context_max_chars: usize,
    semantic_top_k: usize,
    episodic_limit: usize,
    facts_limit: usize,
    user_rag_top_k: usize,
    workspace_graph_top_k: usize,
    expand_by_graph: bool,
    compact_before_prompt: bool,
    allow_project_recall: bool,
    allow_identity_lookup: bool,
}

fn memory_fast_path_enabled() -> bool {
    std::env::var("AKASHA_MEMORY_FAST_PATH")
        .ok()
        .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

fn memory_profile_for_task(
    message: &str,
    assigned_agent: &str,
    is_subagent: bool,
    orch_disk_deliverables: bool,
) -> MemoryProfile {
    if is_subagent {
        return MemoryProfile {
            recent_turns_limit: 0,
            recent_context_max_chars: 0,
            semantic_top_k: 0,
            episodic_limit: 0,
            facts_limit: 0,
            user_rag_top_k: 0,
            workspace_graph_top_k: 0,
            expand_by_graph: false,
            compact_before_prompt: false,
            allow_project_recall: false,
            allow_identity_lookup: false,
        };
    }

    let enriched = !memory_fast_path_enabled()
        || orch_disk_deliverables
        || message_suggests_project(message)
        || assigned_agent != "conversation"
        || message.chars().count() > 280;

    if enriched {
        MemoryProfile {
            recent_turns_limit: 15,
            recent_context_max_chars: 2_000,
            semantic_top_k: 5,
            episodic_limit: 5,
            facts_limit: 10,
            user_rag_top_k: 5,
            workspace_graph_top_k: 5,
            expand_by_graph: std::env::var("AKASHA_GRAPH_EXPAND").ok().as_deref() == Some("1"),
            compact_before_prompt: true,
            allow_project_recall: true,
            allow_identity_lookup: true,
        }
    } else {
        MemoryProfile {
            recent_turns_limit: 6,
            recent_context_max_chars: 800,
            semantic_top_k: 2,
            episodic_limit: 1,
            facts_limit: 0,
            user_rag_top_k: 0,
            workspace_graph_top_k: 0,
            expand_by_graph: false,
            compact_before_prompt: false,
            allow_project_recall: false,
            allow_identity_lookup: true,
        }
    }
}

fn spawn_progress_watchdog(
    bus: EventBus,
    correlation_id: Uuid,
    task_id: Uuid,
    mut cancel_rx: oneshot::Receiver<()>,
) {
    tokio::spawn(async move {
        let checkpoints = [
            (2_u64, 12_u8, "Still spinning up the worker…"),
            (5_u64, 18_u8, "Still working — routing tools and context…"),
            (
                10_u64,
                24_u8,
                "Still working — using a fallback path if needed…",
            ),
        ];
        for (secs, pct, message) in checkpoints {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(secs)) => {
                    let _ = bus.send(
                        EventEnvelope::new(
                            EventType::ProgressUpdate,
                            Some(serde_json::json!({
                                "task_id": task_id.to_string(),
                                "progress_pct": pct,
                                "message": message,
                            })),
                        )
                        .with_correlation(correlation_id),
                    );
                }
                _ = &mut cancel_rx => return,
            }
        }
    });
}

fn cancel_progress_watchdog(cancel_tx: &mut Option<oneshot::Sender<()>>) {
    if let Some(tx) = cancel_tx.take() {
        let _ = tx.send(());
    }
}

async fn wait_for_task_activity(
    progress: &ProgressCache,
    task_id: Uuid,
    timeout: std::time::Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        {
            let guard = progress.read().await;
            let has_activity = guard
                .get(&task_id)
                .and_then(|q| q.back())
                .map(|entry| !entry.message.trim().is_empty())
                .unwrap_or(false);
            if has_activity {
                return true;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
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

fn looks_like_meta_agent_response(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    [
        "i am ready to assist",
        "i'm ready to assist",
        "based on the instructions provided",
        "according to the instructions provided",
        "following the instructions provided",
        "i am configured as",
        "i'm configured as",
        "as an ai assistant",
        "as an ai language model",
        "today, i will execute the phase 2 task",
        "phase 2 task",
        "click, fill, screenshot, and wait",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

/// Returns true when the model output looks like generic greeting/small-talk
/// instead of an answer to a concrete tool-backed request.
fn looks_like_off_topic_greeting(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    [
        "bonjour ! je suis prêt à vous aider",
        "bonjour ! je suis pret a vous aider",
        "que souhaitez-vous faire aujourd'hui",
        "que souhaitez vous faire aujourd'hui",
        "hi! i'm ready to help",
        "what would you like to do today",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

/// Run LLM completion for a user message, with short-term + long-term memory (and compaction), optional tool-use loop. Push reply as progress, mark task completed.
/// image_data_urls: optional list of data URLs (data:image/...;base64,...) for vision-capable models.
/// preferred_task_type_override: when set (e.g. system selector task_type), overrides routing/reminders vs. assigned_agent alone.
pub(crate) async fn run_message_via_llm(
    bus: EventBus,
    llm_router: Arc<akasha_llm::LLMRouter>,
    store_path: std::path::PathBuf,
    spec_dir: std::path::PathBuf,
    task_id: Uuid,
    message: String,
    session_id: String,
    image_data_urls: Option<Vec<String>>,
    // When set (e.g. system selector task_type), overrides resolve_task_type_for_agent(assigned_agent) for LLM routing and image-gen reminders.
    preferred_task_type_override: Option<String>,
    short_term: Option<std::sync::Arc<ShortTermStore>>,
    long_term_client: Option<LongTermMemoryClient>,
    tools_executor: Option<
        std::sync::Arc<tokio::sync::RwLock<std::sync::Arc<akasha_tools::ToolExecutor>>>,
    >,
    tools_policy_path: Option<std::path::PathBuf>,
    skill_registry: Option<std::sync::Arc<crate::skills::SkillRegistry>>,
    plugin_registry: Option<std::sync::Arc<crate::plugins::PluginRegistry>>,
    process_registry: Option<ProcessRegistry>,
    conv_tx: Option<mpsc::Sender<OrchestratorTask>>,
    human_input_store: Option<HumanInputStore>,
    delegation_tx: Option<mpsc::Sender<DelegationRequest>>,
    task_completion_registry: Option<TaskCompletionRegistry>,
    agent_profile_cache: Option<AgentProfileCache>,
    task_usage_store: Option<std::sync::Arc<TaskUsageStore>>,
    device_bridge: Option<std::sync::Arc<crate::device_bridge::DeviceBridge>>,
    workspace_store: Option<TaskWorkspaceStore>,
    browser_registry: Option<crate::browser::BrowserSessionRegistry>,
    autonomous_mission: Option<Arc<RwLock<AutonomousMissionConfig>>>,
    studio_disk_registry: crate::studio::StudioDiskRootRegistry,
) {
    let store = match TaskStore::open(&store_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(task_id = %task_id, error_kind = "store_open", error = %e, "LLM task: store open failed");
            if let Some(reg) = &browser_registry {
                crate::browser::close_task(reg, task_id).await;
            }
            notify_task_completion(&task_completion_registry, task_id).await;
            return;
        }
    };
    let _ = store.update_status(task_id, TaskStatus::Running);
    let lineage_for_studio = workspace_lineage_root_task_id(task_id, Some(store_path.as_path()));
    let tool_disk_workspace_root: std::path::PathBuf = {
        let reg = studio_disk_registry.read().await;
        if let Some(p) = reg.get(&lineage_for_studio) {
            p.clone()
        } else {
            drop(reg);
            store_path
                .parent()
                .map(|x| x.to_path_buf())
                .unwrap_or_else(|| std::path::PathBuf::from("."))
        }
    };
    let data_dir_for_studio_flags = store_path.parent().unwrap_or_else(|| store_path.as_ref());
    let code_studio_disk_task = tool_disk_workspace_root
        .starts_with(crate::studio::studio_projects_base(data_dir_for_studio_flags));
    // NOTE: interpret_message is called below, after guardrail extraction, so it uses clean_message.
    let task_snapshot = store.get(task_id).ok().flatten();
    let assigned_agent = task_snapshot
        .as_ref()
        .map(|t| t.assigned_agent.clone())
        .unwrap_or_else(|| "conversation".to_string());
    let role_agent_for_system_prompt =
        if preferred_task_type_override.as_deref() == Some("image_generation") {
            "image_generation"
        } else {
            assigned_agent.as_str()
        };
    let is_subagent = task_snapshot
        .as_ref()
        .and_then(|t| t.parent_task_id)
        .is_some();
    // When main_agent prepends a guardrail block on selector timeout, extract the real user message.
    // Format: "[Guardrail: …]\n\n<actual message>"
    const GUARDRAIL_MARKER: &str = "[Guardrail:";
    const GUARDRAIL_END: &str = "]\n\n";
    let (guardrail_reminder_block, clean_message): (String, &str) =
        if message.starts_with(GUARDRAIL_MARKER) {
            if let Some(end) = message.find(GUARDRAIL_END) {
                let block = format!("{}\n\n", &message[..end + 1]);
                let actual = message[end + GUARDRAIL_END.len()..].trim_start();
                (block, actual)
            } else {
                (String::new(), message.as_str())
            }
        } else {
            (String::new(), message.as_str())
        };
    // Whether this is an orchestrated subtask message (starts with "[Task]\n").
    // These messages must not pollute session goals, short-term history, or trigger full
    // memory context — they are internal planner artefacts, not real user messages.
    let is_orchestrated_task_msg = clean_message.starts_with("[Task]\n");

    // Anchor the CLEAN user goal (without guardrail prefix) in session state at task start.
    // Skip for orchestrated task messages — their [Task]\nObjective text is not a user goal.
    if !is_subagent && !is_orchestrated_task_msg && !clean_message.trim().is_empty() {
        let data_dir_goal = store_path.parent().unwrap_or_else(|| store_path.as_ref());
        let goal_text = clean_message.chars().take(240).collect::<String>();
        let _ = crate::session_state::merge(data_dir_goal, &session_id, |s| {
            if s.goals.iter().all(|g| g != &goal_text) {
                s.goals.push(goal_text);
            }
        });
    }
    let timeline_correlation = resolve_root_task_id(&store_path, task_id)
        .or_else(|| task_snapshot.as_ref().and_then(|t| t.parent_task_id))
        .unwrap_or(task_id);

    // All intent detection / classification uses clean_message so a guardrail prefix never
    // breaks fast-lane matching or memory profile selection.
    let structured = interpret_message(clean_message);
    let orch_disk_deliverables = clean_message.contains(ORCH_DISK_DELIVERABLES_MARKER);
    // Code Studio tasks run on studio-projects/* disk roots: do not treat user prompts as
    // "small talk" or suppress tool-heavy LLM replies — that blocked write_file / TOOL lines.
    let small_talk_intent = if code_studio_disk_task {
        None
    } else {
        classify_small_talk_message(clean_message)
    };
    let session_recall_intent = detect_session_recall_intent(clean_message);
    tracing::debug!(
        ?session_recall_intent,
        "[RECALL_DEBUG] session_recall_intent"
    );
    let is_small_talk_fast_lane = if code_studio_disk_task {
        false
    } else {
        small_talk_fast_lane(clean_message).is_some()
    };
    let is_session_recall = session_recall_intent.is_some();
    let mut memory_profile = if is_small_talk_fast_lane || is_session_recall {
        MemoryProfile {
            recent_turns_limit: 0,
            recent_context_max_chars: 0,
            semantic_top_k: 0,
            episodic_limit: 0,
            facts_limit: 0,
            user_rag_top_k: 0,
            workspace_graph_top_k: 0,
            expand_by_graph: false,
            compact_before_prompt: false,
            allow_project_recall: false,
            allow_identity_lookup: false,
        }
    } else {
        memory_profile_for_task(
            clean_message,
            &assigned_agent,
            is_subagent,
            orch_disk_deliverables,
        )
    };
    // For external/transport/general-knowledge queries, always isolate from recent context.
    // Loading previous dev/Akasha-specific turns from short-term history actively misleads
    // small local models: they latch onto the most recent topic (e.g. Akasha CLI discussion)
    // and copy it instead of answering the actual question.
    // This cap is unconditional — it does NOT require a guardrail prefix to be active.
    let message_intent_flags_clean = compute_message_intent_flags(clean_message);
    if !is_small_talk_fast_lane && !is_subagent {
        if message_intent_flags_clean.external_info || message_intent_flags_clean.transport {
            memory_profile.recent_turns_limit = 0;
            memory_profile.episodic_limit = 0;
            memory_profile.semantic_top_k = 0;
            memory_profile.facts_limit = 0;
            memory_profile.allow_project_recall = false;
        }
    }
    // Orchestrated task messages ([Task]\n prefix) already carry full context inside the message.
    // Loading unrelated recent_turns from the short-term store only introduces noise and causes
    // context contamination (e.g. bankr venv script appearing for a train/car question).
    if is_orchestrated_task_msg {
        memory_profile.recent_turns_limit = 0;
        memory_profile.semantic_top_k = memory_profile.semantic_top_k.min(1);
        memory_profile.compact_before_prompt = false;
    }

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
    let (watchdog_tx, watchdog_rx) = oneshot::channel();
    let mut watchdog_cancel = Some(watchdog_tx);
    spawn_progress_watchdog(bus.clone(), timeline_correlation, task_id, watchdog_rx);

    let max_tokens = std::env::var("AKASHA_MAX_RESPONSE_TOKENS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(4096);
    let completion_max_tokens = if is_small_talk_fast_lane {
        max_tokens.min(256).max(64)
    } else {
        max_tokens
    };

    let tool_instruction = if is_small_talk_fast_lane {
        String::new()
    } else if tools_executor_snapshot.is_some() {
        let mut allowed_tools = tools_executor_snapshot
            .as_ref()
            .and_then(|e| e.policy.allowed_tool_list());
        if let Some(ref list) = allowed_tools {
            let has_wildcard = tools_executor_snapshot
                .as_ref()
                .map(|e| {
                    e.policy
                        .allowed_commands
                        .iter()
                        .any(|c| c.trim().eq_ignore_ascii_case("*"))
                })
                .unwrap_or(false);
            if has_wildcard && !list.iter().any(|t| t == "run_command") {
                let mut list = list.clone();
                list.push("run_command".to_string());
                allowed_tools = Some(list);
            }
        }
        if orch_disk_deliverables {
            if let Some(ref list) = allowed_tools {
                if !list.iter().any(|t| t == "write_file") {
                    tracing::warn!(
                        task_id = %task_id,
                        "Orchestrated deliverables: tools_policy default_profile omits write_file; \
                         workspace file tools will NOT be advertised to the model — add write_file \
                         (and other file tools) to the profile to enable disk deliverables."
                    );
                }
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
                    let part = format!(
                        " ; Skills (use skill name as tool): {}",
                        skills_desc.join(", ")
                    );
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
        let compact_worker_tool_instruction = format!(
            "\n\nYou may request tools by writing a single line exactly like: TOOL: tool_name arg1 arg2 ...\nAvailable: {}{}.\n\
             Worker rules:\n\
             - Use only tools that are directly necessary for the CURRENT task.\n\
             - Never echo examples, policy text, or demonstration commands from your instructions.\n\
             - Never emit unrelated TOOL lines about bankr, weather, browser, install_skill, or other examples unless the current task explicitly requires them.\n\
             - If the task asks to save/write a file, use TOOL: write_file <path> then the exact content, or TOOL: write_file {{\"path\":\"...\",\"content\":\"...\"}}.\n\
             - Use write_todos / merge_todos (not create_todos) for todo lists.\n\
             - If you need missing user information, use TOOL: ask_user with JSON.\n\
             - If no tool is needed, answer normally.\n",
            base, skills_part
        );
        if is_subagent || assigned_agent != "conversation" || orch_disk_deliverables {
            compact_worker_tool_instruction
        } else {
            format!(
            "\n\nYou may request tools by writing a line: TOOL: tool_name arg1 arg2 ...\nAvailable: {}{}.\n\
             Whenever you need the user to make a choice, confirm something, or provide information (e.g. choose between options, confirm a path, give credentials) before continuing, you MUST reply ONLY with TOOL: ask_user (then JSON with question/context/choices). Do not ask in plain text or the user's reply will start a new task and you cannot continue. Example: {{\"question\":\"Which option?\", \"choices\":[\"A\", \"B\"]}}.\n\
             CONNECTION RULE: If the user asks you to connect to an external service (GitHub repo, API, etc.), do NOT reply with a plain-text message. Use TOOL: ask_user. If the user has already confirmed credentials are configured, do NOT send another ask_user; proceed. Do not invent commands (e.g. /status repo:... does not exist); real commands are in /help.\n\
             CAMERA RULE (priority over WRITE): When the user asks for a webcam/camera photo (e.g. \"prends une photo\", \"take a photo\", \"photo depuis la webcam\", \"affiche-la dans le chat\", \"display it in the chat\"), you MUST reply ONLY with TOOL: device_discover local_media then TOOL: device_invoke local_media camera capture. Do NOT mention tools_policy.yaml, allowed_write_paths, or file writing. After the tool returns, if the user asked to \"display in the chat\" / \"affiche-la dans le chat\" / \"show it in the chat\", reply with ONLY a short confirmation in the user's language (e.g. in French: \"Photo prise. Elle s'affiche ci-dessous.\"; in English: \"Photo captured. It is shown below.\"). Do NOT offer \"save to file\", \"get a description\", \"take another photo\", or \"What would you like to do next?\" — the image is appended automatically below your message. Use the same language as the user (French if they wrote in French).\n\
             WRITE RULE (mandatory): When the user asks to save, record, or write a file (e.g. \"enregistre\", \"sauvegarde\", \"save to\", \"write to file\", or gives a folder path), you MUST reply ONLY with: a first line \"TOOL: write_file <full_path>\" then on the following lines the exact file content. Do NOT answer with \"I cannot write to disk\" or \"copy-paste the code yourself\". Use write_file; if the path is denied, the tool returns an error and you then explain tools_policy.yaml (allowed_write_paths). Paths can be Windows (C:\\Users\\...\\file.py) or Unix. Do NOT apply this rule when the user only asked for a webcam photo.\n\
             WEATHER RULE (PRIORITY): When the user asks for weather, météo, or forecasts (e.g. \"quel temps\", \"météo demain\", \"weather in X\"), you MUST use TOOL: web_search <query> first, then if snippets lack numeric detail use TOOL: web_fetch <url> on a trusted result URL and/or TOOL: browser navigate <url> then TOOL: browser snapshot (many weather sites are JS-heavy). Do NOT use bankr, portfolio, or any other skill for weather — use web_search plus web_fetch/browser as needed.\n\
             BROWSER RULE (PRIORITY): When the user explicitly asks to open the browser, go to a website, or show something on X/Twitter (e.g. \"ouvre le navigateur\", \"open the browser\", \"va sur X\", \"go to twitter\", \"cherche sur X\", \"ouvre le navigateur et cherche\"), you MUST use TOOL: browser navigate <url> first with the appropriate URL (e.g. https://x.com/akashabot for a profile, https://x.com for the home page). You may then add a short message. Do NOT use only web_search when the user asked to open the browser or go to X/Twitter.\n\
             SOCIAL / LOGGED-IN SITES RULE: If tools_policy allows the domain, use TOOL: browser navigate <https URL> and TOOL: browser snapshot when the user asks to open or inspect X/Twitter or similar. Do NOT refuse with vague \"security\", \"confidentiality\", or \"structural policy\" claims — the user runs Akasha locally and controls tools_policy. Real limitation: you cannot type the user's password or complete interactive MFA inside the managed browser on their behalf; if a login wall blocks content, say that clearly and offer practical options (user logs in manually in that same browser session if their environment keeps the session, or official API access via TOOL: run_command with VAULT:... when applicable). Do NOT state that vault-backed API access is forbidden when the user has configured secrets — follow VAULT ENV RULE.\n\
             WEB SEARCH RULE: When the user asks for external information (weather, news, forecasts, schedules, etc.) that you do not have, you MUST use TOOL: web_search <query> first, then answer from fetched content — not only from snippets. If snippets are insufficient, use TOOL: web_fetch <url> on a relevant result URL, or TOOL: browser navigate <url> then TOOL: browser snapshot so YOU retrieve the page text inside Akasha (managed browser), then summarize for the user. Do NOT reply with \"I did not find it\" or suggest sites without having called web_search. Do NOT tell the user to open links in their own browser when web_fetch or browser snapshot is available and allowed — retrieve and answer yourself.\n\
             INSTALL CLI RULE: When the user asks to install a CLI or package globally (e.g. \"install bankr CLI\", \"npm install -g @bankr/cli\", \"install the bankr cli in global\"), you MUST reply ONLY with TOOL: run_command <cmd> <args> (e.g. TOOL: run_command npm install -g @bankr/cli). Do NOT generate a script or ask the user to run commands themselves; run the installation command via the tool.\n\
             VAULT ENV RULE: To use a vault secret in a command you MUST call TOOL: run_command with VAULT:<vault_key>=<ENV_VAR> as the FIRST argument(s), then the command. The system injects the secret value into ENV_VAR for that command only. Example: TOOL: run_command VAULT:GITHUB_TOKEN=GITHUB_TOKEN curl -sS -H \"Authorization: Bearer $GITHUB_TOKEN\" https://api.github.com/repos/owner/repo. FORBIDDEN: never tell the user to run GITHUB_TOKEN=VAULT:GITHUB_TOKEN or export GITHUB_TOKEN=... or VAULT:GITHUB_TOKEN=ghp_... — you must output the TOOL: line yourself so the system runs the command and injects the token. For GitHub with token in vault: use TOOL: run_command VAULT:GITHUB_TOKEN=GITHUB_TOKEN curl -sS -H \"Authorization: Bearer $GITHUB_TOKEN\" https://api.github.com/repos/owner/repo (or gh repo view owner/repo). The vault key may be GITHUB_TOKEN or github_token; the part after = is the env var name the command uses (e.g. $GITHUB_TOKEN). Do NOT say you cannot access the repo without having called run_command with VAULT:... first.\n\
             {}\
             INSTALL SKILL RULE: When the user asks to install, download, get, fetch, or add a skill from a URL (e.g. \"install the bankr skill from …\", \"download the skill at this url\", \"get the skill from this url\", \"récupère le skill …\"), you MUST reply ONLY with TOOL: install_skill <url>. Do not give manual steps; perform the installation yourself. If the user says \"follow the SKILL.md instructions\" or \"follow the instructions in SKILL.md\", you MUST first reply with TOOL: install_skill <url> so the skill is registered; only after it is installed can you invoke it by name (e.g. TOOL: <skill_name> <args>). Do NOT use web_fetch or read_file to fetch SKILL.md and then execute its steps manually.\n\
             UNINSTALL SKILL RULE: When the user asks to uninstall or remove a skill (e.g. \"désinstalle bankr\", \"remove the bankr skill\"), you MUST reply ONLY with TOOL: uninstall_skill <name> (e.g. TOOL: uninstall_skill bankr).\n\
             SKILL USE RULE: When the user asks you to perform an action using a skill (e.g. \"vérifie mon wallet bankr\", \"check my balance with bankr\", \"run bankr whoami\"), you MUST reply ONLY with a single line: TOOL: <skill_name> <args> (e.g. TOOL: bankr whoami). The system will execute the command and return the result. Do NOT tell the user to run the command themselves or to \"use TOOL: bankr whoami\"; you must output that line yourself so the tool is executed.\n\
             PROJECT RULE: For requests that imply a substantial deliverable (novel, comic/BD, code project, series of chapters or files), never claim completion after one response if the full scope is not delivered. State clearly what was done, what remains to do, and that you will continue on the user's next message (or via a sub-task). Do not say \"C'est terminé\" or \"Voilà, c'est fait\" until all requested deliverables are done. If the user says \"continue\", \"la suite\", or \"and the rest\", resume the project in progress (use memory_search for project context if available) and continue without saying \"terminé\" until the full scope is delivered. For project-like work, use memory_store to save project state (objective, steps done, deliverables) after each significant progress, with source project:<name> so context is reloaded on the next message. For multi-step tasks, use TOOL: write_todos for the initial plan (or full replan only); use TOOL: merge_todos to add steps without wiping the list; use read_todos/update_todo to mark progress. If the user message is prefixed with a block [Plan de la tâche — à respecter], execute the \"Prochaine étape\" (next pending step) before broad replanning.\n\
             {}\
             If you need no tool, reply normally with your answer.\n\
             If write_file or read_file returns \"path not allowed by policy\" or \"denied\", tell the user that they CAN configure this: edit the file tools_policy.yaml \
             (in the Akasha data directory) and add path prefixes under allowed_write_paths or allowed_read_paths. It is not impossible — the user controls this YAML file.",
            base, skills_part, run_command_os_rule, skills_rule
        )
        }
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
    if let Some(role_prompt) = agent_role_system_prompt(role_agent_for_system_prompt) {
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
        - Personality: always apply your identity (name), tone, and form of address as defined in [Agent profile and instructions] (including formality when set).\n\n",
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
    user_prefix.push_str(
        "Reply in the same language as the user message below (French, English, etc.).\n\n",
    );
    if let Some(ref am) = autonomous_mission {
        let g = am.read().await;
        if g.enabled && g.status == MissionStatusYaml::Active && session_id == g.session_id {
            user_prefix.push_str(
                "\n\n[Autonomous mission mode — do not ask the user questions]\n\
                - Do NOT ask clarifying questions unless a hard blocker remains (missing vault credentials, or tools_policy denies the action).\n\
                - Prefer tools (read_file, write_file, memory_store, web_search, run_command) and record decisions in markdown under the mission report directory.\n",
            );
            if !g.operating_rules.trim().is_empty() {
                user_prefix.push_str("- Mission operating rules:\n");
                for line in g.operating_rules.lines().take(32) {
                    user_prefix.push_str("  ");
                    user_prefix.push_str(line);
                    user_prefix.push('\n');
                }
                user_prefix.push('\n');
            }
        }
    }
    let turns_empty = match &short_term {
        Some(st) => st.get_turns(&session_id).await.is_empty(),
        None => true,
    };
    let user_profile = UserProfile::load(data_dir);
    let user_identity_prefix = user_profile.format_for_prompt();
    let recall_params = crate::memory_orchestrator::RecallParams {
        message: message.clone(),
        session_id: session_id.clone(),
        semantic_top_k: memory_profile.semantic_top_k,
        episodic_limit: memory_profile.episodic_limit,
        facts_limit: memory_profile.facts_limit,
        filter_by_session: !turns_empty,
        suggest_project: memory_profile.allow_project_recall
            && message_suggests_project(clean_message),
        is_first_message: turns_empty && memory_profile.allow_identity_lookup,
        expand_by_graph: memory_profile.expand_by_graph,
        user_identity_prefix: if user_identity_prefix.is_empty()
            || !memory_profile.allow_identity_lookup
        {
            None
        } else {
            Some(user_identity_prefix)
        },
        ..Default::default()
    };
    if memory_profile.semantic_top_k > 0
        || memory_profile.episodic_limit > 0
        || memory_profile.facts_limit > 0
        || recall_params.user_identity_prefix.is_some()
    {
        let fused =
            crate::memory_orchestrator::recall_context(long_term_client.as_ref(), recall_params)
                .await;
        let fused_str = fused.to_context_string();
        if !fused_str.is_empty() {
            user_prefix.push_str(&fused_str);
        }
    }
    if memory_profile.user_rag_top_k > 0 {
        let user_rag_store = crate::user_rag::UserRagStore::new(data_dir);
        let rag_query = message.clone();
        let rag_top_k = memory_profile.user_rag_top_k;
        let chunks =
            tokio::task::spawn_blocking(move || user_rag_store.retrieve(&rag_query, rag_top_k))
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
    }
    if memory_profile.workspace_graph_top_k > 0 {
        let graph_query = message.clone();
        let graph_k = memory_profile.workspace_graph_top_k;
        let sp = store_path.clone();
        let (workspace_lines, graph_lines) = tokio::task::spawn_blocking(move || {
            let store = WorkspaceGraphStore::open(&sp)?;
            let workspaces = store.list_workspaces()?;
            let workspace_lines: Vec<String> = workspaces
                .iter()
                .map(|w| {
                    format!(
                        "- \"{}\" — id {} — {}",
                        w.name, w.id, w.root_path
                    )
                })
                .collect();
            let graph_lines = store.search_graph_context(&graph_query, graph_k, None)?;
            Ok::<_, anyhow::Error>((workspace_lines, graph_lines))
        })
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();
        if workspace_lines.len() > 1 {
            const MAX_WORKSPACE_LINES: usize = 8;
            user_prefix.push_str(
                "[Project knowledge graphs — registered workspaces (use id with workspace_graph_search --workspace)]\n",
            );
            for line in workspace_lines.iter().take(MAX_WORKSPACE_LINES) {
                user_prefix.push_str(line);
                user_prefix.push_str("\n");
            }
            if workspace_lines.len() > MAX_WORKSPACE_LINES {
                user_prefix.push_str(&format!(
                    "- … ({} more workspaces not shown)\n",
                    workspace_lines.len() - MAX_WORKSPACE_LINES
                ));
            }
            user_prefix.push_str(
                "To fetch more symbols or files from the index, call: workspace_graph_search <keywords> [--workspace <id>]\n\n",
            );
        }
        if !graph_lines.is_empty() {
            user_prefix.push_str(
                "[Project knowledge graphs — excerpts matching this message (indexed folders)]\n",
            );
            for line in &graph_lines {
                user_prefix.push_str("- ");
                user_prefix.push_str(line);
                user_prefix.push_str("\n");
            }
            user_prefix.push_str("\n");
        }
    }
    if let Some(ref st) = short_term {
        if memory_profile.compact_before_prompt {
            let new_msg_tokens = ShortTermStore::estimate_tokens(&message);
            compact_short_term_if_needed(
                st,
                &session_id,
                &llm_router,
                new_msg_tokens,
                long_term_client.as_ref(),
            )
            .await;
        }
        let turns = st.get_turns(&session_id).await;
        let final_turns: Vec<_> = turns
            .iter()
            .rev()
            .take(memory_profile.recent_turns_limit)
            .cloned()
            .rev()
            .collect();
        let short_ctx = ShortTermStore::turns_to_context(&final_turns);
        if !short_ctx.is_empty() {
            let capped = if memory_profile.recent_context_max_chars > 0
                && short_ctx.chars().count() > memory_profile.recent_context_max_chars
            {
                short_ctx
                    .chars()
                    .take(memory_profile.recent_context_max_chars)
                    .collect::<String>()
                    + "…"
            } else {
                short_ctx
            };
            if !capped.is_empty() {
                user_prefix.push_str("[Recent context (this session)]\n");
                user_prefix.push_str(capped.trim_end());
                user_prefix.push_str("\n\n");
            }
        }
    }
    if !is_small_talk_fast_lane {
        if let Ok(todos) = store.get_todos(task_id) {
            if let Some(block) = format_todos_plan_block(&todos) {
                user_prefix.push_str(&block);
                user_prefix.push_str("\n");
            }
        }
    }
    if is_small_talk_fast_lane {
        user_prefix.push_str(
            "\n[Brief small-talk only: reply in 1–3 short sentences. Do not use tools. \
             Follow your identity, personality, and traits from the system instructions.]\n",
        );
    }
    // IMPORTANT: intent classification must use the clean user message (without guardrail/prefix
    // injections). Using the raw `message` can falsely trigger intents (e.g. transport/maps)
    // from injected context blocks and activate unrelated plugin routing.
    let mut intent_flags = compute_message_intent_flags(clean_message);
    if code_studio_disk_task {
        code_studio_disable_plugin_intents(&mut intent_flags);
    }
    let runtime_tool_routing_enforcer = if code_studio_disk_task {
        // Hard guard: Code Studio tasks must stay project/code oriented and must not be
        // hijacked by dynamic plugin routing (maps/graph/external intents).
        None
    } else {
        build_runtime_tool_routing_enforcer(
            plugin_registry.as_ref(),
            clean_message,
            &intent_flags,
            tools_executor_snapshot.as_ref(),
        )
    };
    let write_reminder = if intent_flags.save_file {
        WRITE_FILE_REMINDER
    } else {
        ""
    };
    // web_search is effectively available only when the tool is allowed by the active profile,
    // web_search_enabled is true in the policy, and a Brave API key is present (vault or env).
    let web_search_effectively_available = tools_executor_snapshot
        .as_ref()
        .map(|e| {
            e.policy.can_use_tool("web_search")
                && e.policy.web_search_enabled
                && (e.policy.brave_api_key.is_some() || std::env::var("BRAVE_API_KEY").is_ok())
        })
        .unwrap_or(false);
    let web_search_reminder: &str = if intent_flags.external_info {
        if web_search_effectively_available {
            // web_search is available: instruct the model to use it.
            WEB_SEARCH_REMINDER
        } else {
            // web_search is absent (not allowed, not enabled, or no API key): prevent the model
            // from ignoring the question and returning a generic capability introduction.
            WEB_SEARCH_UNAVAILABLE_REMINDER
        }
    } else {
        ""
    };
    let web_search_followup_reminder: &str = if intent_flags.external_info
        && web_search_effectively_available
        && tools_executor_snapshot
            .as_ref()
            .map(|e| web_followup_tools_configured(&e.policy))
            .unwrap_or(false)
    {
        WEB_SEARCH_FOLLOWUP_REMINDER
    } else {
        ""
    };
    let transport_reminder: &str = if intent_flags.transport {
        // Always inject a transport reminder so the model cannot mistake a travel question
        // for a file-creation or project task (e.g. "Quel est le chemin complet du fichier").
        // Use the full reminder when web_search is available; use the no-search fallback otherwise.
        let has_web_search = web_search_effectively_available;
        if has_web_search {
            TRANSPORT_REMINDER
        } else {
            TRANSPORT_REMINDER_NO_SEARCH
        }
    } else {
        ""
    };
    let (plugin_routing_reminder, plugin_handles_geo_distance) = if code_studio_disk_task {
        (String::new(), false)
    } else {
        build_plugin_routing_reminders(
            plugin_registry.as_ref(),
            clean_message,
            &intent_flags,
            tools_executor_snapshot.as_ref(),
        )
    };
    let geolocation_distance_reminder: &str = if intent_flags.geolocation_distance
        && !plugin_handles_geo_distance
    {
        let has_any_tool = tools_executor_snapshot
            .as_ref()
            .map(|e| e.policy.can_use_tool("web_search") || e.policy.can_use_tool("plugin.call"))
            .unwrap_or(false);
        tracing::info!(
            task_id = %task_id,
            has_any_tool,
            "geolocation-distance intent detected; applying generic fallback guardrail"
        );
        if has_any_tool {
            GEO_DISTANCE_REMINDER_WITH_TOOLS
        } else {
            GEO_DISTANCE_REMINDER_NO_TOOL
        }
    } else {
        ""
    };
    let social_feed_reminder = if intent_flags.social_feed_fetch
        && tools_executor_snapshot.as_ref().map_or(false, |e| {
            e.policy.can_use_tool("web_search") || e.policy.can_use_tool("browser")
        }) {
        SOCIAL_FEED_REMINDER
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
    let image_generation_reminder = if intent_flags.image_generation
        || preferred_task_type_override.as_deref() == Some("image_generation")
    {
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
    let code_dev_sandbox_reminder = if intent_flags.code_generation {
        CODE_DEV_SANDBOX_REMINDER
    } else {
        ""
    };
    let data_dir_for_studio = store_path.parent().unwrap_or_else(|| store_path.as_ref());
    let studio_disk_reminder = if tool_disk_workspace_root
        .starts_with(crate::studio::studio_projects_base(data_dir_for_studio))
    {
        STUDIO_DISK_REMINDER
    } else {
        ""
    };
    let studio_quality_reminder = if tool_disk_workspace_root
        .starts_with(crate::studio::studio_projects_base(data_dir_for_studio))
    {
        STUDIO_AGENT_QUALITY_REMINDER
    } else {
        ""
    };
    // When user clearly wants a photo from camera, prefix the message with an imperative so the model responds with device_invoke directly (no ask_user).
    let user_message = if !device_camera_reminder.is_empty() {
        format!(
            "[Répondre par: TOOL: device_discover local_media puis TOOL: device_invoke local_media camera capture. Ne pas utiliser ask_user.]\n\n{}",
            clean_message
        )
    } else {
        clean_message.to_string()
    };
    let mut current_prompt = if user_prefix.trim().is_empty() {
        format!(
            "{}{}{}{}{}{}{}{}{}{}{}{}{}{}User:\n{}",
            guardrail_reminder_block,
            write_reminder,
            web_search_reminder,
            web_search_followup_reminder,
            transport_reminder,
            geolocation_distance_reminder,
            plugin_routing_reminder,
            social_feed_reminder,
            device_camera_reminder,
            image_generation_reminder,
            github_vault_reminder,
            code_dev_sandbox_reminder,
            studio_disk_reminder,
            studio_quality_reminder,
            user_message
        )
    } else {
        format!(
            "{}{}{}{}{}{}{}{}{}{}{}{}{}{}{}User:\n{}",
            user_prefix.trim_end(),
            guardrail_reminder_block,
            write_reminder,
            web_search_reminder,
            web_search_followup_reminder,
            transport_reminder,
            geolocation_distance_reminder,
            plugin_routing_reminder,
            social_feed_reminder,
            device_camera_reminder,
            image_generation_reminder,
            github_vault_reminder,
            code_dev_sandbox_reminder,
            studio_disk_reminder,
            studio_quality_reminder,
            user_message
        )
    };
    let reply_text;
    let mut last_llm_model_used: Option<String> = None;
    let mut first_meaningful_progress_sent = false;

    if let Some(intent) = session_recall_intent {
        tracing::debug!(?intent, "[RECALL_LOAD] loading recall turns");
        let recall_turns = match intent.range {
            SessionRecallRange::Yesterday => {
                tracing::debug!("[RECALL_LOAD] reading YESTERDAY data");
                if short_term.is_some() {
                    let short_term_dir = data_dir.join("short_term");
                    let yesterday = chrono::Utc::now() - chrono::Duration::days(1);
                    let sid = format!("day-{}", yesterday.format("%Y-%m-%d"));
                    tracing::debug!(session_id = %sid, "[RECALL_LOAD] yesterday session_id");
                    let turns =
                        crate::memory::ShortTermStore::read_day_from_disk(&sid, &short_term_dir)
                            .unwrap_or_default();
                    tracing::debug!(
                        count = turns.len(),
                        "[RECALL_LOAD] read turns from yesterday"
                    );
                    turns
                } else {
                    tracing::debug!("[RECALL_LOAD] short_term is None, returning empty");
                    Vec::new()
                }
            }
            SessionRecallRange::CurrentDay => {
                tracing::debug!("[RECALL_LOAD] reading CURRENT_DAY data");
                if let Some(st) = short_term.as_ref() {
                    let turns = st.get_turns(&session_id).await;
                    tracing::debug!(
                        count = turns.len(),
                        "[RECALL_LOAD] read turns from current day"
                    );
                    turns
                } else {
                    tracing::debug!("[RECALL_LOAD] short_term is None, returning empty");
                    Vec::new()
                }
            }
        };
        tracing::debug!(count = recall_turns.len(), "[RECALL_BUILD] building recap");
        reply_text = build_session_recap_reply(&recall_turns, intent).unwrap_or_else(|| {
            match intent.language {
                SmallTalkLanguage::French => "Je n'ai pas encore de résumé fiable à te partager pour cette période. Si tu veux, je peux te faire un récap dès qu'on a un peu plus d'historique utile.".to_string(),
                SmallTalkLanguage::English => "I don't have a reliable recap for that period yet. If you want, I can provide one as soon as we have a bit more useful history.".to_string(),
            }
        });
        first_meaningful_progress_sent = true;
        cancel_progress_watchdog(&mut watchdog_cancel);
        if emit_timeline_once_for_task(
            &bus,
            Some(store_path.as_path()),
            task_id,
            "first_meaningful_progress",
            Some(serde_json::json!({ "source": "session_recap_fast_path" })),
        ) {
            log_latency_metric(store_path.as_path(), task_id, "ttfr_ms");
        }
    } else {
        let mut max_tool_rounds = std::env::var("AKASHA_MAX_TOOL_ROUNDS")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(10);
        // Orchestrated subtasks / remediation: extra tool rounds (models often read/search first).
        if orch_disk_deliverables {
            let floor = std::env::var("AKASHA_MAX_TOOL_ROUNDS_ORCH_DELIVERABLES")
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(24);
            if max_tool_rounds < floor {
                max_tool_rounds = floor;
            }
        }
        let mut round = 0u32;
        let mut social_snapshot_seen = false;
        let mut tool_loop_history: Vec<(String, String)> = Vec::new();
        let mut last_tool_results_blob: Option<String> = None;
        let mut force_synthesis_attempted = false;
        let mut meta_response_retry_count = 0u32;
        let mut small_talk_off_topic_retries = 0u32;
        let strict_tools_first = runtime_tool_routing_enforcer
            .as_ref()
            .map(|e| !e.preferred_tools.is_empty())
            .unwrap_or(false);
        let strict_tools_instruction = if strict_tools_first {
            let preferred = runtime_tool_routing_enforcer
                .as_ref()
                .map(|e| {
                    let mut v = e.preferred_tools.iter().cloned().collect::<Vec<_>>();
                    v.sort();
                    v.join(", ")
                })
                .unwrap_or_default();
            format!(
                "\n\n[TOOLS-FIRST STRICT MODE]\n- PRIMARY USER REQUEST (must be satisfied): {}\n- You MUST output TOOL lines only until at least one allowed tool succeeds.\n- Preferred tools: {}\n- If inputs are missing, output ONLY: TOOL: ask_user {{\"question\":\"...\",\"context\":\"...\",\"choices\":[...]}}\n- Do NOT output prose, role acknowledgements, policy acknowledgements, or generic greetings.\n",
                user_message, preferred
            )
        } else {
            String::new()
        };
        let mut strict_no_tool_rounds = 0u32;
        let mut strict_successful_tool_calls = 0u32;
        let mut strict_preferred_tool_replay_input: Option<String> = None;
        // In strict tools-first mode, deterministic preferred-tool attempts must run only once before
        // the first LLM call. `round` stays 0 until the model emits parseable TOOL lines, so without
        // this flag we would re-run deterministic maps (etc.) on every strict re-prompt and burn a
        // full LLM timeout budget on a duplicate hung stream.
        let mut deterministic_preferred_attempted = false;
        // Orchestrated deliverables: re-prompts when the model returns no parseable TOOL lines.
        let mut orch_disk_write_nags = 0u32;
        let mut last_captured_image_base64: Option<String> = None;

        let llm_timeout_secs = std::env::var("AKASHA_LLM_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or_else(|| llm_router.default_timeout_secs());
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
            let strict_mode_active = strict_tools_first && strict_successful_tool_calls == 0;
            if strict_mode_active {
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": task_id.to_string(),
                            "progress_pct": 30,
                            "message": "Applying dynamic plugin routing rules (tools-first)…"
                        })),
                    )
                    .with_correlation(task_id),
                );
            }

            // Deterministic first attempt in strict tools-first mode:
            // try preferred tools with the full user request as input before asking the LLM again.
            let run_deterministic_preferred = strict_mode_active
                && (strict_preferred_tool_replay_input.is_some()
                    || (!deterministic_preferred_attempted && round == 0));
            if run_deterministic_preferred {
                if let (Some(enforcer), Some(exec)) = (
                    &runtime_tool_routing_enforcer,
                    tools_executor_snapshot.as_ref(),
                ) {
                    let preferred_tool_request = strict_preferred_tool_replay_input
                        .take()
                        .unwrap_or_else(|| user_message.clone());
                    let mut deterministic_results: Vec<String> = Vec::new();
                    const MAPS_RESULT_FULL_MAX: usize = 400_000;
                    for preferred_tool in enforcer.preferred_tools.iter().take(3) {
                        let args_preview = if preferred_tool_request.chars().count() > 240 {
                            format!(
                                "{}…",
                                preferred_tool_request.chars().take(240).collect::<String>()
                            )
                        } else {
                            preferred_tool_request.clone()
                        };
                        let _ = bus.send(
                            EventEnvelope::new(
                                EventType::TimelineMilestone,
                                Some(serde_json::json!({
                                    "name": "deterministic_preferred_tool_attempt",
                                    "task_id": task_id.to_string(),
                                    "round": round,
                                    "tool": preferred_tool,
                                    "args_preview": args_preview,
                                })),
                            )
                            .with_correlation(timeline_correlation),
                        );
                        let auto_args = vec![preferred_tool_request.clone()];
                        let (success, res, captured_image) = execute_tool_call(
                            exec,
                            preferred_tool,
                            &auto_args,
                            process_registry.as_ref(),
                            long_term_client.as_ref(),
                            task_id,
                            Some(store_path.as_path()),
                            conv_tx.clone(),
                            message_webhook_url.as_deref(),
                            plugin_registry.as_ref(),
                            device_bridge.as_ref(),
                            workspace_store.as_ref(),
                            browser_registry.as_ref(),
                            Some(tool_disk_workspace_root.as_path()),
                        )
                        .await;
                        let result_preview = if res.chars().count() > 320 {
                            format!("{}…", res.chars().take(320).collect::<String>())
                        } else {
                            res.clone()
                        };
                        // UI (chat) needs full plugin JSON for rich views (e.g. maps); preview is truncated.
                        let mut milestone = serde_json::json!({
                            "name": "deterministic_preferred_tool_result",
                            "task_id": task_id.to_string(),
                            "round": round,
                            "tool": preferred_tool,
                            "success": success,
                            "result_preview": result_preview,
                        });
                        if success
                            && preferred_tool.starts_with("maps_")
                            && res.len() <= MAPS_RESULT_FULL_MAX
                        {
                            milestone["result_full"] = serde_json::Value::String(res.clone());
                        }
                        let _ = bus.send(
                            EventEnvelope::new(EventType::TimelineMilestone, Some(milestone))
                                .with_correlation(timeline_correlation),
                        );
                        tool_loop_history.push((
                            preferred_tool.clone(),
                            if success { "success" } else { "failure" }.to_string(),
                        ));
                        if let Some(img) = captured_image {
                            last_captured_image_base64 = Some(img);
                        }
                        deterministic_results.push(res.clone());
                        if success {
                            strict_successful_tool_calls =
                                strict_successful_tool_calls.saturating_add(1);
                            log_tool_journal_if_write(preferred_tool, &auto_args, &res).await;
                            break;
                        }
                    }
                    if strict_successful_tool_calls > 0 {
                        let results_blob = deterministic_results.join("\n");
                        last_tool_results_blob = Some(results_blob.clone());
                        current_prompt = format!(
                        "User request: {}\n\nTool results:\n{}\n\nUsing ONLY the tool results above, answer the user's request now. Do NOT reply with a promise. No TOOL: lines.",
                        user_message,
                        results_blob
                    );
                        // Continue to next round so the model synthesizes from concrete tool results.
                        continue;
                    }
                    let _ = bus.send(
                    EventEnvelope::new(
                        EventType::TimelineMilestone,
                        Some(serde_json::json!({
                            "name": "deterministic_preferred_tool_no_success",
                            "task_id": task_id.to_string(),
                            "round": round,
                            "attempted_tools": enforcer.preferred_tools.iter().cloned().collect::<Vec<_>>(),
                        })),
                    )
                    .with_correlation(timeline_correlation),
                );
                }
                deterministic_preferred_attempted = true;
            }

            // Quota: stop task if session cost or tokens exceed configured limits (Phase 2.3).
            if let Some(ref store) = task_usage_store {
                let (session_tokens, session_cost) =
                    store.get_session(&session_id).await.unwrap_or((0, 0.0));
                if let Some(max_cost) = std::env::var("AKASHA_MAX_COST_PER_SESSION_USD")
                    .ok()
                    .and_then(|s| s.parse::<f64>().ok())
                {
                    if max_cost > 0.0 && session_cost >= max_cost {
                        reply_text = "Budget dépassé pour cette session (AKASHA_MAX_COST_PER_SESSION_USD). Démarrez une nouvelle session ou augmentez le plafond.".to_string();
                        break 'tool_rounds;
                    }
                }
                if let Some(max_tokens) = std::env::var("AKASHA_MAX_TOKENS_PER_SESSION")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    if max_tokens > 0 && session_tokens >= max_tokens {
                        reply_text = "Quota de tokens dépassé pour cette session (AKASHA_MAX_TOKENS_PER_SESSION). Démarrez une nouvelle session ou augmentez le plafond.".to_string();
                        break 'tool_rounds;
                    }
                }
            }
            // `task_types.image_generation` in llm_router.yaml configures the *pixel backend* for the
            // `generate_image` tool (see image_generation.rs). Routing chat completion to that task
            // type sends image-only models (e.g. Ollama z-image) through the text completion path, which
            // expects a `response` string — those models return images/empty text and trigger fallback warnings.
            // Here we always use a normal text route for the LLM turn; the tool call still uses image_generation config.
            let router_task_type_for_llm = if preferred_task_type_override.as_deref()
                == Some("image_generation")
                || assigned_agent == "image_generation"
            {
                llm_router.resolve_task_type_for_agent("conversation")
            } else {
                preferred_task_type_override
                    .clone()
                    .unwrap_or_else(|| llm_router.resolve_task_type_for_agent(&assigned_agent))
            };
            let preferred_task_type = Some(router_task_type_for_llm);
            let request = CompletionRequest {
                prompt: if strict_mode_active {
                    format!("{}{}", current_prompt, strict_tools_instruction)
                } else {
                    format!("{}{}", current_prompt, tool_instruction)
                },
                max_tokens: Some(completion_max_tokens),
                temperature: Some(0.7),
                preferred_task_type,
                system_prompt: system_prompt.clone(),
                image_data_urls: if tool_loop_history.is_empty() {
                    image_data_urls.clone()
                } else {
                    None
                },
                top_p: None,
                top_k: None,
                frequency_penalty: None,
                presence_penalty: None,
                repeat_penalty: None,
                num_ctx: None,
                num_gpu: None,
                thinking_level: None,
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
            let stream_join =
                tokio::spawn(async move { router.complete_stream(&request, stream_tx).await });
            let mut accumulated = String::new();
            let mut first_wait = true;
            let overall_deadline =
                tokio::time::Instant::now() + std::time::Duration::from_secs(llm_timeout_secs);
            loop {
                // Check the overall deadline before waiting for a chunk to avoid spurious zero-duration timeouts.
                if tokio::time::Instant::now() >= overall_deadline {
                    tracing::warn!(
                        timeout_secs = llm_timeout_secs,
                        "Overall LLM timeout exceeded; aborting task"
                    );
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
                        if !first_meaningful_progress_sent && !chunk.trim().is_empty() {
                            first_meaningful_progress_sent = true;
                            cancel_progress_watchdog(&mut watchdog_cancel);
                            if emit_timeline_once_for_task(
                                &bus,
                                Some(store_path.as_path()),
                                task_id,
                                "first_meaningful_progress",
                                Some(serde_json::json!({ "source": "stream_chunk" })),
                            ) {
                                log_latency_metric(store_path.as_path(), task_id, "ttfr_ms");
                            }
                        }
                        const MAX_ACCUMULATED: usize = 2 * 1024 * 1024; // 2 MiB cap to prevent unbounded allocation on long streams
                        if accumulated.len() + chunk.len() > MAX_ACCUMULATED {
                            accumulated.truncate(MAX_ACCUMULATED.saturating_sub(chunk.len()));
                        }
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
                    Ok(None) => break,
                    Err(_) => {
                        tracing::debug!(
                            idle_secs = idle_timeout_secs,
                            "Stream idle timeout, waiting for final response"
                        );
                        break;
                    }
                }
            }
            // Wrap stream_join.await with remaining overall budget; guard against zero remaining.
            let remaining = overall_deadline.saturating_duration_since(tokio::time::Instant::now());
            let response = if remaining.is_zero() {
                tracing::warn!(
                    timeout_secs = llm_timeout_secs,
                    "Overall LLM timeout on stream completion"
                );
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
                            let tokens = resp
                                .usage
                                .as_ref()
                                .map(|u| u.prompt_tokens + u.completion_tokens)
                                .unwrap_or(0);
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
                        tracing::warn!(
                            timeout_secs = llm_timeout_secs,
                            "Overall LLM timeout on stream completion"
                        );
                        reply_text = if accumulated.is_empty() {
                            format!("LLM response timed out after {} seconds.", llm_timeout_secs)
                        } else {
                            accumulated
                        };
                        break;
                    }
                }
            };
            // Some providers return the full text only in stream chunks while `resp.text` is empty, or drop `TOOL:` lines
            // from the final body. Parsing tools only from `resp.text` then skips execution entirely (user sees text, no disk writes).
            // Use `parse_tool_calls` (which normalizes sloppy prefixes like `- Tool:` / `**TOOL:**`) instead of a raw
            // `contains("TOOL:")` check so that any provider-specific formatting is handled consistently.
            let r = response.trim();
            let a = accumulated.trim();
            let a_has_tools = !a.is_empty() && !parse_tool_calls(a).is_empty();
            let r_has_tools = !r.is_empty() && !parse_tool_calls(r).is_empty();
            let merged_for_tools = if r.is_empty() && !a.is_empty() {
                a.to_string()
            } else if a_has_tools && !r_has_tools {
                a.to_string()
            } else if !r.is_empty() {
                r.to_string()
            } else {
                a.to_string()
            };
            let response = merged_for_tools;

            if let Some(intent) = small_talk_intent {
                if response_looks_off_topic_for_small_talk(&response) {
                    if is_small_talk_fast_lane && small_talk_off_topic_retries < 1 {
                        small_talk_off_topic_retries += 1;
                        tracing::warn!(
                            task_id = %task_id,
                            "Small-talk guardrail: retrying with stricter brief-reply instruction"
                        );
                        current_prompt = format!(
                            "{}\n\n[Regeneration]: Your previous reply was not appropriate for simple small talk \
                             (tools, policies, file paths, or too long). Reply ONLY with a brief polite exchange \
                             (1–2 short sentences) in the same language as the user, following your identity and \
                             personality from the system instructions. No tools.\n",
                            current_prompt
                        );
                        continue;
                    }
                    tracing::warn!(task_id = %task_id, "Small-talk guardrail triggered; suppressing off-topic/tool-heavy reply");
                    reply_text = small_talk_fast_reply(&message, intent);
                    break 'tool_rounds;
                }
            }

            let parsed_tool_calls = tools_executor_snapshot.as_ref().and_then(|_| {
                let calls = parse_tool_calls(&response);
                if calls.is_empty() {
                    None
                } else {
                    Some(calls)
                }
            });
            let no_parseable_tools_this_round = parsed_tool_calls.is_none();
            let response_plain = response
                .lines()
                .filter(|l| !l.trim_start().starts_with("TOOL:"))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();

            if strict_tools_first
                && no_parseable_tools_this_round
                && strict_successful_tool_calls == 0
            {
                strict_no_tool_rounds = strict_no_tool_rounds.saturating_add(1);
                let preferred_tools_hint = runtime_tool_routing_enforcer
                    .as_ref()
                    .map(|e| {
                        let mut v = e.preferred_tools.iter().cloned().collect::<Vec<_>>();
                        v.sort();
                        v.join(", ")
                    })
                    .unwrap_or_default();

                if strict_no_tool_rounds <= 1 {
                    current_prompt = format!(
                    "User request: {}\n\nYour previous reply:\n{}\n\nDynamic plugin routing rules are active for this request. You MUST emit TOOL lines only. Preferred tools: {}. If inputs are missing, call TOOL: ask_user with one precise question. Do NOT output prose-only answers now.",
                    user_message,
                    response_plain,
                    preferred_tools_hint
                );
                    continue;
                }

                reply_text = format!(
                "Impossible de répondre de façon fiable sans exécuter un outil autorisé. Outils attendus: {}. Vérifiez les règles de routage des plugins installés ou fournissez les paramètres manquants.",
                preferred_tools_hint
            );
                break 'tool_rounds;
            }

            if no_parseable_tools_this_round
                && meta_response_retry_count < 2
                && (is_subagent || assigned_agent != "conversation" || orch_disk_deliverables)
                && looks_like_meta_agent_response(&response_plain)
            {
                meta_response_retry_count += 1;
                current_prompt = format!(
                "User request: {}\n\nYour previous reply:\n{}\n\nThat reply was meta/instruction recitation, not actual progress on the assigned task. Do the work now. Do NOT describe your role, say you are ready, mention instructions, or narrate a generic Phase 2 plan. If files are required, start with TOOL: read_file / write_file on the exact workspace paths. If you are blocked, state only the concrete missing input or exact tool failure.",
                user_message,
                response_plain
            );
                continue;
            }

            if let (Some(exec), Some(calls)) = (tools_executor_snapshot.as_ref(), parsed_tool_calls)
            {
                round += 1;
                let mut tool_results = Vec::new();
                let scheduled_lanes = akasha_tools::schedule_tool_calls(&calls);
                for (lane, lane_calls) in scheduled_lanes {
                    let lane_name = match lane {
                        akasha_tools::ToolExecutionLane::ParallelSafe => "parallel_safe",
                        akasha_tools::ToolExecutionLane::SerialExclusive => "serial_exclusive",
                    };
                    let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": task_id.to_string(),
                            "progress_pct": 50,
                            "message": format!("Tool lane execution: {} ({} call(s))", lane_name, lane_calls.len())
                        })),
                    )
                    .with_correlation(task_id),
                );
                    for (name, args) in &lane_calls {
                        // Phase D: resolve skill name to tool_ref (spec 33)
                        let actual_tool = match &skill_registry {
                            Some(reg) => reg
                                .get(name)
                                .await
                                .map(|s| s.tool_ref)
                                .unwrap_or_else(|| name.clone()),
                            None => name.clone(),
                        };
                        let actual_tool = canonicalize_tool_name(&actual_tool);
                        let should_forward_user_request = strict_tools_first
                            && args.is_empty()
                            && !actual_tool.eq_ignore_ascii_case("ask_user")
                            && runtime_tool_routing_enforcer
                                .as_ref()
                                .map(|e| e.preferred_tools.contains(&actual_tool))
                                .unwrap_or(false);
                        let forwarded_args: Vec<String> = if should_forward_user_request {
                            vec![user_message.clone()]
                        } else {
                            args.clone()
                        };
                        let tool_args: &[String] = &forwarded_args;
                        let effective_tool_for_routing = if actual_tool.is_empty() {
                            name.as_str()
                        } else {
                            actual_tool.as_str()
                        };
                        let device_routing_bypass = intent_flags.camera_or_mic
                            && (effective_tool_for_routing.eq_ignore_ascii_case("device_discover")
                                || effective_tool_for_routing
                                    .eq_ignore_ascii_case("device_invoke"));
                        if let Some(enforcer) = &runtime_tool_routing_enforcer {
                            if !device_routing_bypass
                                && !enforcer.is_tool_allowed(effective_tool_for_routing, tool_args)
                            {
                                let blocked = format!(
                            "[tool_blocked_by_routing_rules] tool={} blocked by dynamic plugin routing rules",
                            effective_tool_for_routing
                        );
                                let payload = serde_json::json!({
                                    "tool": effective_tool_for_routing,
                                    "args": tool_args,
                                    "result_preview": blocked,
                                    "success": false,
                                    "reason": "blocked_by_dynamic_plugin_routing_rules"
                                });
                                let _ = bus.send(
                                    EventEnvelope::new(EventType::ToolInvoked, Some(payload))
                                        .with_correlation(timeline_correlation),
                                );
                                tracing::warn!(
                                    task_id = %task_id,
                                    tool = %effective_tool_for_routing,
                                    preferred = ?enforcer.preferred_tools,
                                    forbidden = ?enforcer.forbidden_tools,
                                    "Tool blocked by runtime routing enforcer"
                                );
                                tool_results.push(blocked);
                                continue;
                            }
                        }
                        let args_str = tool_args.join(" ");
                        tool_loop_history.push((actual_tool.clone(), args_str.clone()));
                        // Phase 4: loop detection — same tool+args repeated 3 times
                        if tool_loop_history.len() >= 3 {
                            let last = tool_loop_history.last().unwrap();
                            if tool_loop_history
                                .iter()
                                .rev()
                                .take(3)
                                .all(|e| e.0 == last.0 && e.1 == last.1)
                            {
                                reply_text =
                                    "Loop detected: same tool and arguments repeated. Stopping."
                                        .to_string();
                                break 'tool_rounds;
                            }
                        }
                        // User-friendly progress at key step: what we are doing right now (use skill name when actual_tool is empty, e.g. bankr skill).
                        let display_tool = if actual_tool.is_empty() {
                            name.as_str()
                        } else {
                            &actual_tool
                        };
                        let progress_msg = progress_message_for_tool(display_tool, tool_args);
                        if !first_meaningful_progress_sent {
                            first_meaningful_progress_sent = true;
                            cancel_progress_watchdog(&mut watchdog_cancel);
                            if emit_timeline_once_for_task(
                                &bus,
                                Some(store_path.as_path()),
                                task_id,
                                "first_meaningful_progress",
                                Some(serde_json::json!({
                                    "source": "tool_progress",
                                    "tool": display_tool,
                                })),
                            ) {
                                log_latency_metric(store_path.as_path(), task_id, "ttfr_ms");
                            }
                        }
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
                                    let args_preview: String = if matches!(
                                        actual_tool.as_str(),
                                        "apply_patch" | "edit_file" | "write_file"
                                    ) {
                                        "[redacted]".to_string()
                                    } else {
                                        let truncated: Vec<String> = tool_args
                                            .iter()
                                            .take(3)
                                            .map(|a| {
                                                if a.chars().count() > MAX_APPROVAL_ARG_LEN {
                                                    format!(
                                                        "{}…",
                                                        a.chars()
                                                            .take(MAX_APPROVAL_ARG_LEN)
                                                            .collect::<String>()
                                                    )
                                                } else {
                                                    a.clone()
                                                }
                                            })
                                            .collect();
                                        let suffix = if tool_args.len() > 3 {
                                            format!(" … ({} args)", tool_args.len())
                                        } else {
                                            String::new()
                                        };
                                        truncated.join(" ") + &suffix
                                    };
                                    let question = format!(
                                        "Approuver l'action : {} — {} ?",
                                        actual_tool, args_preview
                                    );
                                    let choices =
                                        vec!["Approuver".to_string(), "Refuser".to_string()];
                                    let (tx, rx) = tokio::sync::oneshot::channel();
                                    let pending = PendingHumanInput {
                                        question: question.clone(),
                                        context: format!(
                                            "Outil sensible (nécessite confirmation) : {}",
                                            actual_tool
                                        ),
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
                                        EventEnvelope::new(
                                            EventType::TaskWaitingUserInput,
                                            Some(payload.clone()),
                                        )
                                        .with_correlation(task_id),
                                    );
                                    let approval_payload = serde_json::json!({
                                        "tool": actual_tool,
                                        "args_redacted": args_preview,
                                        "task_id": task_id.to_string()
                                    });
                                    let _ = bus.send(
                                        EventEnvelope::new(
                                            EventType::ToolApprovalRequest,
                                            Some(approval_payload),
                                        )
                                        .with_correlation(task_id),
                                    );
                                    let granted = match tokio::time::timeout(
                                        std::time::Duration::from_secs(APPROVAL_TIMEOUT_SECS),
                                        rx,
                                    )
                                    .await
                                    {
                                        Ok(Ok(reply)) => {
                                            reply.trim().eq_ignore_ascii_case("Approuver")
                                        }
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
                                                EventEnvelope::new(
                                                    EventType::ToolApprovalExpired,
                                                    Some(expired_payload),
                                                )
                                                .with_correlation(task_id),
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
                                            EventEnvelope::new(
                                                EventType::ToolInvoked,
                                                Some(payload),
                                            )
                                            .with_correlation(timeline_correlation),
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
                        let tool_t0 = std::time::Instant::now();
                        let call_id = Uuid::new_v4();
                        let args_preview_tc: String = {
                            const L: usize = 100;
                            args.iter()
                                .take(3)
                                .map(|a| {
                                    if a.len() > L {
                                        format!("{}…", &a[..a.floor_char_boundary(L)])
                                    } else {
                                        a.clone()
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(" ")
                        };
                        let _ = bus.send(
                            EventEnvelope::new(
                                EventType::ToolCallStarted,
                                Some(serde_json::json!({
                                    "schema_version": 1,
                                    "task_id": task_id.to_string(),
                                    "call_id": call_id.to_string(),
                                    "tool": display_tool,
                                    "args_preview": args_preview_tc,
                                })),
                            )
                            .with_correlation(timeline_correlation),
                        );
                        let (success, res, captured_image): (bool, String, Option<String>) =
                            if actual_tool == "ask_user" {
                                // Human in the loop: register pending request, emit event, wait for user reply.
                                match &human_input_store {
                                    Some(store) => {
                                        let body_joined_string = args.join(" ");
                                        let body = body_joined_string.trim();
                                        let body = if body.is_empty() { "{}" } else { body };
                                        let v =
                                            serde_json::from_str::<serde_json::Value>(body).ok();
                                        let (question, context, choices) = match &v {
                                            Some(v) => (
                                                v.get("question")
                                                    .and_then(|q| q.as_str())
                                                    .unwrap_or("")
                                                    .to_string(),
                                                v.get("context")
                                                    .and_then(|c| c.as_str())
                                                    .unwrap_or("")
                                                    .to_string(),
                                                v.get("choices").and_then(|c| c.as_array()).map(
                                                    |a| {
                                                        a.iter()
                                                            .filter_map(|x| {
                                                                x.as_str().map(String::from)
                                                            })
                                                            .collect::<Vec<_>>()
                                                    },
                                                ),
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
                                                EventEnvelope::new(
                                                    EventType::TaskWaitingUserInput,
                                                    Some(payload),
                                                )
                                                .with_correlation(task_id),
                                            );
                                            const HUMAN_INPUT_TIMEOUT_SECS: u64 = 3600;
                                            match tokio::time::timeout(
                                                std::time::Duration::from_secs(
                                                    HUMAN_INPUT_TIMEOUT_SECS,
                                                ),
                                                rx,
                                            )
                                            .await
                                            {
                                                Ok(Ok(reply)) => (
                                                    true,
                                                    format!("[ask_user] User replied: {}", reply),
                                                    None,
                                                ),
                                                Ok(Err(_)) => {
                                                    let mut g = store.write().await;
                                                    g.remove(&task_id);
                                                    (
                                                        false,
                                                        "[ask_user] Channel closed.".to_string(),
                                                        None,
                                                    )
                                                }
                                                Err(_) => {
                                                    let mut g = store.write().await;
                                                    g.remove(&task_id);
                                                    (false, format!("[ask_user] Timeout after {}s; no user reply.", HUMAN_INPUT_TIMEOUT_SECS), None)
                                                }
                                            }
                                        }
                                    }
                                    None => (
                                        false,
                                        "[ask_user] Human-in-the-loop not available.".to_string(),
                                        None,
                                    ),
                                }
                            } else if actual_tool == "delegate_to_agent" {
                                match &delegation_tx {
                                    Some(tx) => {
                                        let (reply_tx, reply_rx) = oneshot::channel();
                                        let agent_type = args
                                            .get(0)
                                            .cloned()
                                            .unwrap_or_else(|| "conversation".to_string());
                                        let message =
                                            args.get(1..).map(|a| a.join(" ")).unwrap_or_else(
                                                || args.get(0).cloned().unwrap_or_default(),
                                            );
                                        if tx
                                            .send(DelegationRequest {
                                                requesting_task_id: task_id,
                                                agent_type,
                                                message,
                                                reply_tx,
                                            })
                                            .await
                                            .is_ok()
                                        {
                                            match tokio::time::timeout(
                                                std::time::Duration::from_secs(310),
                                                reply_rx,
                                            )
                                            .await
                                            {
                                                Ok(Ok(Ok(msg))) => (
                                                    true,
                                                    format!("[delegate_to_agent] {}", msg),
                                                    None,
                                                ),
                                                Ok(Ok(Err(e))) => (
                                                    false,
                                                    format!("[delegate_to_agent] {}", e),
                                                    None,
                                                ),
                                                _ => (
                                                    false,
                                                    "[delegate_to_agent] timeout or channel closed"
                                                        .to_string(),
                                                    None,
                                                ),
                                            }
                                        } else {
                                            (
                                                false,
                                                "[delegate_to_agent] channel closed".to_string(),
                                                None,
                                            )
                                        }
                                    }
                                    None => (
                                        false,
                                        "[delegate_to_agent] not available".to_string(),
                                        None,
                                    ),
                                }
                            } else if actual_tool == "install_skill" {
                                let url = args.get(0).map(String::as_str).unwrap_or("").trim();
                                if url.is_empty() {
                                    (false, "[install_skill] usage: install_skill <url> (ex. https://github.com/BankrBot/skills/tree/main/bankr ou toute URL HTTPS autorisée dans tools_policy allowed_skill_install_hosts)".to_string(), None)
                                } else {
                                    let data_dir =
                                        store_path.parent().unwrap_or_else(|| store_path.as_ref());
                                    let allowed_hosts = exec.policy.skill_install_allowed_hosts();
                                    let tools_reload = tools_executor.as_ref().and_then(|arc| {
                                        tools_policy_path.as_ref().map(|p| (arc, p.as_path()))
                                    });
                                    match &skill_registry {
                                        Some(reg) => {
                                            let (s, r) = do_install_skill(
                                                url,
                                                data_dir,
                                                &spec_dir,
                                                reg,
                                                &allowed_hosts,
                                                tools_reload,
                                            )
                                            .await;
                                            (s, r, None)
                                        }
                                        None => (
                                            false,
                                            "[install_skill] skill registry not available"
                                                .to_string(),
                                            None,
                                        ),
                                    }
                                }
                            } else if actual_tool == "uninstall_skill" {
                                let skill_name =
                                    tool_args.get(0).map(String::as_str).unwrap_or("").trim();
                                let data_dir =
                                    store_path.parent().unwrap_or_else(|| store_path.as_ref());
                                let tools_reload = tools_executor.as_ref().and_then(|arc| {
                                    tools_policy_path.as_ref().map(|p| (arc, p.as_path()))
                                });
                                match &skill_registry {
                                    Some(reg) => {
                                        let (s, r) = do_uninstall_skill(
                                            skill_name,
                                            data_dir,
                                            &spec_dir,
                                            reg,
                                            tools_reload,
                                        )
                                        .await;
                                        (s, r, None)
                                    }
                                    None => (
                                        false,
                                        "[uninstall_skill] skill registry not available"
                                            .to_string(),
                                        None,
                                    ),
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
                                                EventEnvelope::new(
                                                    EventType::TodoListUpdated,
                                                    Some(payload_json),
                                                )
                                                .with_correlation(task_id),
                                            );
                                            (
                                                true,
                                                format!(
                                                    "[write_todos] {} step(s) saved.",
                                                    todos.len()
                                                ),
                                                None,
                                            )
                                        }
                                    }
                                    Err(e) => {
                                        (false, format!("[write_todos] store error: {}", e), None)
                                    }
                                }
                            } else if actual_tool == "merge_todos" {
                                let payload = args.join(" ").trim().to_string();
                                match TaskStore::open(&store_path) {
                                    Ok(store) => {
                                        match store.merge_todos_from_payload(task_id, &payload) {
                                            Ok(todos) => {
                                                let payload_json = serde_json::json!({
                                                    "task_id": task_id.to_string(),
                                                    "todos": todos.iter().map(|t| serde_json::json!({ "id": t.id, "title": t.title, "status": t.status.as_str() })).collect::<Vec<_>>()
                                                });
                                                let _ = bus.send(
                                                    EventEnvelope::new(
                                                        EventType::TodoListUpdated,
                                                        Some(payload_json),
                                                    )
                                                    .with_correlation(task_id),
                                                );
                                                (
                                                    true,
                                                    format!(
                                                        "[merge_todos] list now has {} step(s).",
                                                        todos.len()
                                                    ),
                                                    None,
                                                )
                                            }
                                            Err(e) => {
                                                (false, format!("[merge_todos] error: {}", e), None)
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        (false, format!("[merge_todos] store error: {}", e), None)
                                    }
                                }
                            } else if actual_tool == "read_todos" {
                                match TaskStore::open(&store_path) {
                                    Ok(store) => match store.get_todos(task_id) {
                                        Ok(todos) => {
                                            let summary: Vec<serde_json::Value> = todos.iter().enumerate().map(|(i, t)| {
                                    serde_json::json!({ "index": i + 1, "title": t.title, "status": t.status.as_str() })
                                }).collect();
                                            (
                                                true,
                                                format!(
                                                    "[read_todos] {} step(s): {}",
                                                    todos.len(),
                                                    serde_json::to_string(&summary)
                                                        .unwrap_or_default()
                                                ),
                                                None,
                                            )
                                        }
                                        Err(e) => {
                                            (false, format!("[read_todos] error: {}", e), None)
                                        }
                                    },
                                    Err(e) => {
                                        (false, format!("[read_todos] store error: {}", e), None)
                                    }
                                }
                            } else if actual_tool == "update_todo" {
                                let index_str =
                                    args.get(0).map(String::as_str).unwrap_or("").trim();
                                let status_str =
                                    args.get(1).map(String::as_str).unwrap_or("pending").trim();
                                let index: usize = index_str.parse().unwrap_or(0);
                                match TaskStore::open(&store_path) {
                                    Ok(store) => {
                                        match store.get_todos(task_id) {
                                            Ok(mut todos) => {
                                                if index == 0 || index > todos.len() {
                                                    (false, format!("[update_todo] invalid index (1..{}): {}", todos.len(), index_str), None)
                                                } else {
                                                    let status =
                                                        match status_str.to_lowercase().as_str() {
                                                            "done" => TodoStatus::Done,
                                                            "cancelled" => TodoStatus::Cancelled,
                                                            _ => TodoStatus::Pending,
                                                        };
                                                    todos[index - 1].status = status;
                                                    if let Err(e) = store.set_todos(task_id, &todos)
                                                    {
                                                        (
                                                            false,
                                                            format!("[update_todo] error: {}", e),
                                                            None,
                                                        )
                                                    } else {
                                                        let payload_json = serde_json::json!({
                                                            "task_id": task_id.to_string(),
                                                            "todos": todos.iter().map(|t| serde_json::json!({ "id": t.id, "title": t.title, "status": t.status.as_str() })).collect::<Vec<_>>()
                                                        });
                                                        let _ = bus.send(
                                                            EventEnvelope::new(
                                                                EventType::TodoListUpdated,
                                                                Some(payload_json),
                                                            )
                                                            .with_correlation(task_id),
                                                        );
                                                        (
                                                            true,
                                                            format!(
                                                                "[update_todo] step {} set to {}.",
                                                                index, status_str
                                                            ),
                                                            None,
                                                        )
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                (false, format!("[update_todo] error: {}", e), None)
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        (false, format!("[update_todo] store error: {}", e), None)
                                    }
                                }
                            } else if actual_tool == "list_skills" {
                                match &skill_registry {
                                    Some(reg) => {
                                        let list = reg.list().await;
                                        let summary: Vec<String> = list
                                            .iter()
                                            .map(|s| format!("{}: {}", s.name, s.description))
                                            .collect();
                                        (
                                            true,
                                            format!(
                                                "[list_skills] {} skill(s): {}",
                                                list.len(),
                                                summary.join(" ; ")
                                            ),
                                            None,
                                        )
                                    }
                                    None => (
                                        false,
                                        "[list_skills] skill registry not available.".to_string(),
                                        None,
                                    ),
                                }
                            } else if actual_tool == "read_skill" {
                                let skill_name =
                                    tool_args.get(0).map(String::as_str).unwrap_or("").trim();
                                if skill_name.is_empty() {
                                    (
                                        false,
                                        "[read_skill] usage: read_skill <name>".to_string(),
                                        None,
                                    )
                                } else {
                                    match &skill_registry {
                                        Some(reg) => {
                                            if let Some(body) = reg.get_body(skill_name).await {
                                                (
                                                    true,
                                                    format!(
                                                        "[read_skill {}] Instructions:\n{}",
                                                        skill_name, body
                                                    ),
                                                    None,
                                                )
                                            } else {
                                                (false, format!("[read_skill] skill '{}' not found or has no body.", skill_name), None)
                                            }
                                        }
                                        None => (
                                            false,
                                            "[read_skill] skill registry not available."
                                                .to_string(),
                                            None,
                                        ),
                                    }
                                }
                            } else if actual_tool.is_empty() {
                                // Skill with no tool_ref: if args provided, run as run_command(skill_name, ...args) (e.g. bankr whoami)
                                if !tool_args.is_empty() {
                                    let run_args: Vec<String> = std::iter::once(name.clone())
                                        .chain(tool_args.iter().cloned())
                                        .collect();
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
                                        plugin_registry.as_ref(),
                                        device_bridge.as_ref(),
                                        workspace_store.as_ref(),
                                        browser_registry.as_ref(),
                                        Some(tool_disk_workspace_root.as_path()),
                                    )
                                    .await;
                                    (s, r, None)
                                } else {
                                    // No args: inject SKILL.md body as context for next round (doc-only)
                                    match &skill_registry {
                                        Some(reg) => {
                                            if let Some(body) = reg.get_body(name).await {
                                                (
                                                    true,
                                                    format!(
                                                        "[Skill: {}] Instructions:\n{}",
                                                        name, body
                                                    ),
                                                    None,
                                                )
                                            } else {
                                                (
                                                    false,
                                                    format!(
                                                        "[Skill: {}] No instructions body.",
                                                        name
                                                    ),
                                                    None,
                                                )
                                            }
                                        }
                                        None => (
                                            false,
                                            "Skill registry not available.".to_string(),
                                            None,
                                        ),
                                    }
                                }
                            } else {
                                execute_tool_call(
                                    exec,
                                    &actual_tool,
                                    tool_args,
                                    process_registry.as_ref(),
                                    long_term_client.as_ref(),
                                    task_id,
                                    Some(store_path.as_path()),
                                    conv_tx.clone(),
                                    message_webhook_url.as_deref(),
                                    plugin_registry.as_ref(),
                                    device_bridge.as_ref(),
                                    workspace_store.as_ref(),
                                    browser_registry.as_ref(),
                                    Some(tool_disk_workspace_root.as_path()),
                                )
                                .await
                            };
                        if let Some(img) = captured_image {
                            last_captured_image_base64 = Some(img);
                        }
                        if success
                            && strict_tools_first
                            && actual_tool.eq_ignore_ascii_case("ask_user")
                        {
                            if let Some(reply) = res.strip_prefix("[ask_user] User replied: ") {
                                let reply = reply.trim();
                                if !reply.is_empty() {
                                    strict_preferred_tool_replay_input = Some(format!(
                                        "Original user request: {}\n\nUser clarification: {}",
                                        user_message, reply
                                    ));
                                }
                            }
                        }
                        // Phase F: emit ToolInvoked for Actions tab (spec 33)
                        // Redact or truncate args in the event to avoid leaking large blobs or secrets.
                        let redacted_args: Vec<String> = if matches!(
                            actual_tool.as_str(),
                            "apply_patch" | "edit_file" | "write_file"
                        ) {
                            vec!["[redacted for write-like tool]".to_string()]
                        } else {
                            const MAX_ARG_PREVIEW_LEN: usize = 512;
                            tool_args
                                .iter()
                                .map(|arg| {
                                    if arg.len() > MAX_ARG_PREVIEW_LEN {
                                        format!(
                                            "{}...[truncated {} chars]",
                                            &arg[..arg.floor_char_boundary(MAX_ARG_PREVIEW_LEN)],
                                            arg.len().saturating_sub(MAX_ARG_PREVIEW_LEN)
                                        )
                                    } else {
                                        arg.clone()
                                    }
                                })
                                .collect()
                        };
                        let tool_display = if actual_tool.is_empty() {
                            name.as_str()
                        } else {
                            actual_tool.as_str()
                        };
                        let payload = serde_json::json!({
                            "tool": tool_display,
                            "skill": if &actual_tool != name { Some(name.as_str()) } else { None::<&str> },
                            "args": redacted_args,
                            "result_preview": if res.len() > 300 { format!("{}...", &res[..res.floor_char_boundary(300)]) } else { res.clone() },
                            "success": success,
                            "explanation": serde_json::Value::Null
                        });
                        let _ = bus.send(
                            EventEnvelope::new(EventType::ToolInvoked, Some(payload))
                                .with_correlation(timeline_correlation),
                        );
                        let _ = bus.send(
                            EventEnvelope::new(
                                EventType::ToolCallFinished,
                                Some(serde_json::json!({
                                    "schema_version": 1,
                                    "task_id": task_id.to_string(),
                                    "call_id": call_id.to_string(),
                                    "tool": tool_display,
                                    "success": success,
                                    "duration_ms": tool_t0.elapsed().as_millis() as u64,
                                })),
                            )
                            .with_correlation(timeline_correlation),
                        );
                        // Chat UI loads map / rich views from timeline_milestone + result_full (same as strict tools-first path).
                        if success && tool_display.starts_with("maps_") && res.len() <= 400_000usize
                        {
                            let result_preview = if res.chars().count() > 320 {
                                format!("{}…", res.chars().take(320).collect::<String>())
                            } else {
                                res.clone()
                            };
                            let milestone = serde_json::json!({
                                "name": "deterministic_preferred_tool_result",
                                "task_id": task_id.to_string(),
                                "round": round,
                                "tool": tool_display,
                                "success": true,
                                "result_preview": result_preview,
                                "result_full": res.clone(),
                            });
                            let _ = bus.send(
                                EventEnvelope::new(EventType::TimelineMilestone, Some(milestone))
                                    .with_correlation(timeline_correlation),
                            );
                        }
                        if success {
                            // In tools-first strict mode, ask_user is a clarification step, not
                            // a terminal success for the primary objective. Keep strict mode active
                            // until a non-ask_user tool actually succeeds.
                            if !actual_tool.eq_ignore_ascii_case("ask_user") {
                                strict_successful_tool_calls =
                                    strict_successful_tool_calls.saturating_add(1);
                            }
                            log_tool_journal_if_write(&actual_tool, tool_args, &res).await;
                        }
                        tool_results.push(res);
                    }
                }
                let results_blob = tool_results.join("\n");
                last_tool_results_blob = Some(results_blob.clone());
                if results_blob.contains("[browser] Snapshot") {
                    social_snapshot_seen = true;
                }
                let round_had_ask_user = calls.iter().any(|(name, _)| name == "ask_user");
                let msg_social = compute_message_intent_flags(&message).social_feed_fetch;
                let had_web_search = tool_loop_history.iter().any(|(t, _)| t == "web_search");
                let browser_ok = tools_executor_snapshot
                    .as_ref()
                    .map(|e| e.policy.can_use_tool("browser") && e.policy.browser_enabled)
                    .unwrap_or(false);
                let can_ws = tools_executor_snapshot
                    .as_ref()
                    .map(|e| e.policy.can_use_tool("web_search"))
                    .unwrap_or(false);
                let can_wf = tools_executor_snapshot
                    .as_ref()
                    .map(|e| e.policy.can_use_tool("web_fetch"))
                    .unwrap_or(false);
                let had_web_fetch = tool_loop_history.iter().any(|(t, _)| t == "web_fetch");
                let x_profile_url =
                    extract_x_profile_handle(&message).map(|h| format!("https://x.com/{}", h));
                let browser_line = results_blob.contains("[browser]");
                let navigated_ok = results_blob.contains("[browser] Navigated");
                // Social/X: after web_search (any prior round), chain browser navigate → snapshot; ws retry if browser fails or disabled.
                let social_pending = msg_social && had_web_search && !social_snapshot_seen;
                let ws_count = tool_loop_history
                    .iter()
                    .filter(|(t, _)| t == "web_search")
                    .count();
                // Re-inject the user's request so the model always knows what to answer (avoids treating another demand or losing context).
                current_prompt = if round_had_ask_user {
                    format!(
                    "User request (PRIMARY — you must still fulfill this): {}\n\nYour previous assistant reply:\n{}\n\nask_user step result (user's choice; may be unrelated to the primary request):\n{}\n\nContinue the task. If the PRIMARY request is not satisfied yet, you MUST emit TOOL: lines next (web_search, web_fetch, browser navigate + browser snapshot, etc.). Do not reply \"blocked\" or \"no information\" without trying web_search first. When the primary request is fully answered, reply in plain text only (no TOOL: lines).",
                    user_message, response, results_blob
                )
                } else if social_pending && browser_ok && navigated_ok {
                    format!(
                    "User request: {}\n\nYour previous reply:\n{}\n\nTool results (this round):\n{}\n\nThe profile page is open. Run TOOL: browser snapshot now, then answer in plain text with the latest posts visible in the snapshot.",
                    user_message, response, results_blob
                )
                } else if social_pending && browser_ok && !browser_line {
                    format!(
                    "User request: {}\n\nYour previous reply:\n{}\n\nTool results so far:\n{}\n\nSearch snippets may be the wrong account. Run TOOL: browser navigate https://x.com/<handle> (exact @handle from the user message) then TOOL: browser snapshot. Plain-text answer only after snapshot.",
                    user_message, response, results_blob
                )
                } else if social_pending && browser_ok && browser_line && !navigated_ok && can_ws {
                    format!(
                    "User request: {}\n\nYour previous reply:\n{}\n\nTool results:\n{}\n\nBrowser step failed or was blocked. Emit TOOL: web_search with the exact handle (e.g. site:x.com akasha_anthiam). If web_search already failed twice ({} calls), answer in plain text with limitations.",
                    user_message, response, results_blob, ws_count
                )
                } else if social_pending
                    && !browser_ok
                    && can_wf
                    && x_profile_url.is_some()
                    && !had_web_fetch
                {
                    format!(
                    "User request: {}\n\nYour previous reply:\n{}\n\nTool results so far:\n{}\n\nWeb search often returns the wrong X account. Fetch the exact profile HTML: TOOL: web_fetch {}\nThen, if the HTML is a login wall or has no post text, say so. Otherwise quote only text that appears in the fetch result. Emit TOOL: web_fetch now (one URL only).",
                    user_message,
                    response,
                    results_blob,
                    x_profile_url.as_deref().unwrap_or("https://x.com/")
                )
                } else if social_pending && !browser_ok && can_ws && ws_count < 2 {
                    format!(
                    "User request: {}\n\nYour previous reply:\n{}\n\nTool results:\n{}\n\nEmit TOOL: web_search including the EXACT @handle (e.g. site:x.com handle posts). Then answer from results or explain if impossible.",
                    user_message, response, results_blob
                )
                } else if social_pending && !browser_ok && ws_count >= 2 {
                    format!(
                    "User request: {}\n\nTool attempts (summary):\n{}\n\nSTOP: Do NOT invent or fabricate tweet/post text. Reply in plain text ONLY (no TOOL: lines):\n- State that automated retrieval did not reliably return the 3 latest posts for the handle the user asked for (X blocks many scrapers; search snippets often mismatch the account).\n- To get a real timeline in Akasha: set browser_enabled: true in tools_policy.yaml, install Playwright (npx playwright install chromium in scripts/playwright-runner), then ask again — the agent can use browser navigate + snapshot.\n- Optionally give the direct link https://x.com/{} for manual viewing.\n- You may list only URLs or titles that literally appeared in the tool output above — never make up post bodies.",
                    user_message,
                    results_blob,
                    extract_x_profile_handle(&message).as_deref().unwrap_or("handle")
                )
                } else {
                    format!(
                    "User request: {}\n\nYour previous reply:\n{}\n\nTool results:\n{}\n\nUsing ONLY the tool results above, answer the user's request now. Do NOT reply with a promise (e.g. \"I will fetch…\", \"Action in progress\"). The task ends after this message — give the actual answer (e.g. weather forecast, search summary). No TOOL: lines.",
                    user_message, response, results_blob
                )
                };
                if round >= max_tool_rounds {
                    let response_for_user = response
                        .lines()
                        .filter(|l| !l.trim_start().starts_with("TOOL:"))
                        .collect::<Vec<_>>()
                        .join("\n")
                        .trim()
                        .to_string();
                    let limit_msg = if tool_results.iter().any(|r| {
                        r.contains("device_invoke")
                            && (r.contains("timeout") || r.contains("refused"))
                    }) {
                        "L'accès à l'appareil (caméra/micro) a expiré ou a été refusé. Vous pouvez réessayer en renvoyant votre demande."
                    } else if tool_results
                        .iter()
                        .any(|r| r.contains("generate_image") && r.contains("générée"))
                    {
                        "Image générée."
                    } else if tool_results
                        .iter()
                        .any(|r| r.contains("speech_synthesize") && r.contains("synthétisé"))
                    {
                        "Audio synthétisé."
                    } else if tool_results
                        .iter()
                        .any(|r| r.contains("device_invoke") && r.contains("success"))
                    {
                        "Photo reçue."
                    } else {
                        "Limite de tours d'outils atteinte."
                    };
                    let image_md = last_captured_image_base64
                        .as_ref()
                        .filter(|b| !b.is_empty())
                        .map(|b| {
                            let url = if b.starts_with("data:") {
                                b.clone()
                            } else {
                                format!("data:image/jpeg;base64,{}", b)
                            };
                            let label = if url.starts_with("data:audio/") {
                                "Audio synthétisé"
                            } else if b.starts_with("data:") {
                                "Image générée"
                            } else {
                                "Photo capturée"
                            };
                            build_image_markdown(&label, &url)
                        })
                        .unwrap_or_default();
                    let response_clean = ensure_no_open_code_block(&response_for_user);
                    let default_reply = if response_for_user.is_empty() {
                        format!("{}{}", limit_msg, image_md)
                    } else {
                        format!("{}\n\n[{}]{}", response_clean, limit_msg, image_md)
                    };

                    // When human_input_store is available, ask user whether to continue (+10 rounds), reset and continue, or stop.
                    let should_stop = match &human_input_store {
                        Some(store) => {
                            const TOOL_ROUND_LIMIT_TIMEOUT_SECS: u64 = 300;
                            let question = "Limite de tours d'outils atteinte. Souhaitez-vous continuer la tâche ?".to_string();
                            let context = format!(
                            "La tâche a utilisé {} tours d'outils (max {}). Vous pouvez ajouter 10 tours, réinitialiser le compteur et ajouter 10 tours, ou arrêter.",
                            round, max_tool_rounds
                        );
                            let choices = vec![
                                "Continuer (+10 tours)".to_string(),
                                "Réinitialiser et continuer (+10 tours)".to_string(),
                                "Arrêter".to_string(),
                            ];
                            let (tx, rx) = tokio::sync::oneshot::channel();
                            let pending = PendingHumanInput {
                                question: question.clone(),
                                context: context.clone(),
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
                                "context": context,
                                "choices": choices,
                                "tool_round_limit": true,
                                "current_round": round,
                                "max_tool_rounds": max_tool_rounds
                            });
                            let _ = bus.send(
                                EventEnvelope::new(EventType::TaskWaitingUserInput, Some(payload))
                                    .with_correlation(task_id),
                            );
                            let reply = tokio::time::timeout(
                                std::time::Duration::from_secs(TOOL_ROUND_LIMIT_TIMEOUT_SECS),
                                rx,
                            )
                            .await;
                            {
                                let mut g = store.write().await;
                                g.remove(&task_id);
                            }
                            match reply {
                                Ok(Ok(user_choice)) => {
                                    let choice = user_choice.trim();
                                    if choice == "Continuer (+10 tours)" {
                                        max_tool_rounds += 10;
                                        false
                                    } else if choice == "Réinitialiser et continuer (+10 tours)" {
                                        round = 0;
                                        max_tool_rounds += 10;
                                        false
                                    } else {
                                        true
                                    }
                                }
                                _ => true,
                            }
                        }
                        None => true,
                    };
                    if should_stop {
                        reply_text = default_reply;
                        break;
                    }
                    continue;
                }
                continue;
            }

            // When the model returns prose / JSON only, this loop would exit with zero tool rounds — UI shows an
            // answer but nothing is written. For orchestrated disk deliverables, nudge additional LLM rounds until
            // a write-like tool appears in history. Only nag when the active policy actually permits write tools;
            // if the profile blocks them the nudge would just churn through "tool not allowed" errors.
            const MAX_ORCH_DISK_WRITE_NAGS: u32 = 8;
            if orch_disk_deliverables
                && tools_executor_snapshot.is_some()
                && no_parseable_tools_this_round
            {
                let policy_allows_write = tools_executor_snapshot
                    .as_ref()
                    .map(|e| e.policy.can_use_tool("write_file"))
                    .unwrap_or(false);
                if policy_allows_write {
                    let disk_write_attempted = tool_loop_history.iter().any(|(t, _)| {
                        matches!(
                            t.as_str(),
                            "write_file" | "edit_file" | "search_replace" | "apply_patch"
                        )
                    });
                    if !disk_write_attempted && orch_disk_write_nags < MAX_ORCH_DISK_WRITE_NAGS {
                        orch_disk_write_nags += 1;
                        current_prompt = format!(
                        "{}\n\n[Orchestrator — disk deliverables] Your last assistant message did not include any executable TOOL: lines (or they were not parsed). This step MUST call tools: use TOOL: read_file on the shared plan trace if needed, then TOOL: write_file / edit_file / search_replace for every mandatory workspace path and update the plan sections **Fait (agent)** / **Reste (agent)**. Do not finish with prose-only or ```json``` — emit TOOL lines now.",
                        current_prompt
                    );
                        continue;
                    }
                }
            }

            let response_for_user = response
                .lines()
                .filter(|l| !l.trim_start().starts_with("TOOL:"))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();
            if strict_tools_first && strict_successful_tool_calls == 0 {
                let preferred_tools_hint = runtime_tool_routing_enforcer
                    .as_ref()
                    .map(|e| {
                        let mut v = e.preferred_tools.iter().cloned().collect::<Vec<_>>();
                        v.sort();
                        v.join(", ")
                    })
                    .unwrap_or_default();
                reply_text = format!(
                "Réponse bloquée: aucune exécution d'outil autorisé n'a réussi pour cette demande. Outils attendus: {}. Merci de vérifier la configuration des plugins/routing rules ou de préciser les paramètres requis.",
                preferred_tools_hint
            );
                break;
            }
            // If we already ran tools but the model returned a placeholder ("Je vais… Une seconde."), force one more round to get the actual answer.
            if !tool_loop_history.is_empty()
                && last_tool_results_blob
                    .as_ref()
                    .map_or(false, |b| !b.is_empty())
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
            // Guardrail: when a plugin/tool already succeeded, reject generic
            // greeting responses and force one synthesis round from tool outputs.
            if strict_tools_first
                && strict_successful_tool_calls > 0
                && last_tool_results_blob
                    .as_ref()
                    .map_or(false, |b| !b.is_empty())
                && looks_like_off_topic_greeting(&response_for_user)
                && !force_synthesis_attempted
            {
                force_synthesis_attempted = true;
                current_prompt = format!(
                "User request: {}\n\nTool results:\n{}\n\nYour previous reply was off-topic greeting text. Answer the user's request NOW using the tool results above. Return only the concrete final answer (distance/itinerary if available). No greeting, no TOOL: lines.",
                user_message,
                last_tool_results_blob.as_deref().unwrap_or("")
            );
                continue;
            }
            let image_md = last_captured_image_base64
                .as_ref()
                .filter(|b| !b.is_empty())
                .map(|b| {
                    let url = if b.starts_with("data:") {
                        b.clone()
                    } else {
                        format!("data:image/jpeg;base64,{}", b)
                    };
                    let label = if url.starts_with("data:audio/") {
                        "Audio synthétisé"
                    } else if b.starts_with("data:") {
                        "Image générée"
                    } else {
                        "Photo capturée"
                    };
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
    if !first_meaningful_progress_sent && !reply_text.trim().is_empty() {
        cancel_progress_watchdog(&mut watchdog_cancel);
        if emit_timeline_once_for_task(
            &bus,
            Some(store_path.as_path()),
            task_id,
            "first_meaningful_progress",
            Some(serde_json::json!({ "source": "final_reply" })),
        ) {
            log_latency_metric(store_path.as_path(), task_id, "ttfr_ms");
        }
    } else {
        cancel_progress_watchdog(&mut watchdog_cancel);
    }

    // Persist this exchange in short-term memory (spec 06)
    // Skip orchestrated task messages ([Task]\n prefix): they are internal planner artefacts,
    // not real user/assistant turns. Storing them pollutes future context with unrelated content.
    if !is_small_talk_fast_lane && !is_orchestrated_task_msg {
        if let Some(ref st) = short_term {
            // Store the clean user message (without any guardrail prefix) so history is human-readable.
            st.append(&session_id, "user", clean_message.to_string())
                .await;
            st.append(&session_id, "assistant", reply_text.clone())
                .await;
        }
    }

    // Extract and promote personal facts to long-term memory (spec 06: nom, préférences, décisions).
    if !is_small_talk_fast_lane {
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
                    let res = client.promote(
                        fact.clone(),
                        "user_fact".to_string(),
                        None,
                        None,
                        None,
                        Some(2),
                        Some("global_user".to_string()),
                        None,
                        None,
                    );
                    if res.is_ok() {
                        let _ = client.emit_event(
                            "user_preference".to_string(),
                            fact,
                            None,
                            None,
                            None,
                            None,
                            Some(2),
                            Some("global_user".to_string()),
                            Some("user_fact".to_string()),
                        );
                    }
                    res
                })
                .await
                {
                    Ok(Ok(())) => {
                        tracing::info!("Personal fact stored in long-term memory (heuristic)")
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, "Long-term promote failed — check that embeddings/tract model loads (see daemon logs)")
                    }
                    Err(e) => tracing::debug!(error = %e, "Promote task join error"),
                }
            }

            // Then spawn LLM extraction for projects, interests, important info, personal facts, and agent profile (async).
            let msg = message.clone();
            let reply = reply_text.clone();
            let client = long_term.clone();
            let router = llm_router.clone();
            let heuristic_set: std::collections::HashSet<String> =
                heuristic_facts.iter().cloned().collect();
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
                crate::llm_prompt_cap::truncate_utf8_bytes(
                    msg.trim(),
                    crate::llm_prompt_cap::SYSTEM_PROMPT_FIELD_MAX_BYTES,
                ),
                crate::llm_prompt_cap::truncate_utf8_bytes(
                    reply.trim(),
                    crate::llm_prompt_cap::SYSTEM_PROMPT_FIELD_MAX_BYTES,
                )
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
                    top_p: None,
                    top_k: None,
                    frequency_penalty: None,
                    presence_penalty: None,
                    repeat_penalty: None,
                    num_ctx: None,
                    num_gpu: None,
                    thinking_level: None,
                };
                let mut to_promote: Vec<(String, String)> = Vec::new();
                let mut agent_updates: Vec<(String, String)> = Vec::new();
                if let Ok(Ok(resp)) =
                    tokio::time::timeout(std::time::Duration::from_secs(30), router.complete(&req))
                        .await
                {
                    for line in resp.text.lines() {
                        let line = line.trim();
                        if let Some(rest) = line.strip_prefix("AGENT_NAME:") {
                            agent_updates.push(("AGENT_NAME".to_string(), rest.trim().to_string()));
                        } else if let Some(rest) = line.strip_prefix("AGENT_PERSONALITY:") {
                            agent_updates
                                .push(("AGENT_PERSONALITY".to_string(), rest.trim().to_string()));
                        } else if let Some(rest) = line.strip_prefix("AGENT_RULE:") {
                            agent_updates.push(("AGENT_RULE".to_string(), rest.trim().to_string()));
                        } else if let Some(rest) = line.strip_prefix("AGENT_CAN:") {
                            agent_updates.push(("AGENT_CAN".to_string(), rest.trim().to_string()));
                        } else if let Some(rest) = line.strip_prefix("AGENT_CANNOT:") {
                            agent_updates
                                .push(("AGENT_CANNOT".to_string(), rest.trim().to_string()));
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
                    tracing::debug!(
                        count = to_promote.len(),
                        "Promoting extracted items to long-term memory"
                    );
                }
                for (content, source) in to_promote {
                    let client = client.clone();
                    let c = content.clone();
                    let s = source.clone();
                    match tokio::task::spawn_blocking(move || {
                        client.promote(c, s, None, None, None, None, None, None, None)
                    })
                    .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => tracing::warn!(error = %e, "Long-term promote failed"),
                        Err(e) => tracing::debug!(error = %e, "Promote task join error"),
                    }
                }
            });
        }
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
        || reply_lower.contains("action refusée")
        || reply_lower.contains("limite de tours d'outils atteinte");
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
    // Phase 2 AI OS: do not overwrite Paused with Completed (user paused the task).
    // Determine if the task was paused during execution. If so, we must not emit
    // TaskCompleted nor mark it as completed; instead, emit TaskPaused to keep
    // the event stream consistent with the stored status.
    let is_paused = matches!(
        store.get(task_id),
        Ok(Some(Task {
            status: TaskStatus::Paused,
            ..
        }))
    );

    if let Some(reg) = &browser_registry {
        crate::browser::close_task(reg, task_id).await;
    }
    // Release per-task workspace memory immediately — no longer needed once the task finishes.
    if let Some(ws) = &workspace_store {
        ws.write().await.remove(&task_id);
    }

    let mut studio_verify_error: Option<String> = None;
    if !is_paused && code_studio_disk_task {
        match crate::api_studio::studio_verify_after_agent_task(&tool_disk_workspace_root).await {
            Ok(()) => {}
            Err(e) => {
                studio_verify_error = Some(e);
            }
        }
    }
    if let Some(ref err) = studio_verify_error {
        let _ = bus.send(
            EventEnvelope::new(
                EventType::ProgressUpdate,
                Some(serde_json::json!({
                    "task_id": task_id.to_string(),
                    "progress_pct": 100,
                    "message": format!(
                        "Échec vérification automatique (build/check) — la tâche est marquée en échec.\n{}",
                        err.chars().take(1800).collect::<String>()
                    )
                })),
            )
            .with_correlation(task_id),
        );
    }

    let final_event_type = if is_paused {
        EventType::TaskPaused
    } else if studio_verify_error.is_some() {
        EventType::TaskFailed
    } else {
        EventType::TaskCompleted
    };
    let final_status_str = if is_paused {
        "paused"
    } else if studio_verify_error.is_some() {
        "failed"
    } else {
        "completed"
    };

    let _ = bus.send(
        EventEnvelope::new(
            final_event_type,
            Some(serde_json::json!({
                "task_id": task_id.to_string(),
                "status": final_status_str,
                "model_used": last_llm_model_used
            })),
        )
        .with_correlation(task_id),
    );
    emit_timeline_once_for_task(
        &bus,
        Some(store_path.as_path()),
        task_id,
        "task_completed",
        Some(serde_json::json!({ "status": final_status_str })),
    );
    clear_task_milestones(task_id, Some(store_path.as_path()));

    // Phase 2 AI OS: do not overwrite Paused with Completed (user paused the task).
    if !is_paused {
        if studio_verify_error.is_some() {
            let _ = store.update_status(task_id, TaskStatus::Failed);
        } else {
            let _ = store.update_status(task_id, TaskStatus::Completed);
        }
        notify_task_completion(&task_completion_registry, task_id).await;
        let data_dir_sess = store_path.parent().unwrap_or_else(|| store_path.as_ref());
        let is_root_task = store
            .get(task_id)
            .ok()
            .flatten()
            .map(|t| t.parent_task_id.is_none())
            .unwrap_or(true);
        if is_root_task
            && !is_small_talk_fast_lane
            && !is_orchestrated_task_msg
            && studio_verify_error.is_none()
        {
            if let Ok(st) = crate::session_state::merge(data_dir_sess, &session_id, |s| {
                let fact = reply_text.chars().take(240).collect::<String>();
                // Skip storing agent confusion/redirects as facts — they poison future context.
                let fact_lower = fact.to_lowercase();
                let is_confused_redirect = fact_lower.contains("chemin complet")
                    || fact_lower.contains("quel chemin")
                    || fact_lower.contains("pouvez-vous me préciser")
                    || fact_lower.contains("pouvez-vous préciser")
                    || fact_lower.contains("plan du projet")
                    || fact_lower.contains("quel texte voulez")
                    || (fact_lower.contains("fichier") && fact_lower.contains("chemin") && fact_lower.ends_with('?'))
                    || (fact_lower.contains("dossier") && fact_lower.contains("préciser"))
                    // Generic LLM "here is…" answers are one-off responses, not session facts.
                    // Storing them causes context contamination on future unrelated requests.
                    || fact_lower.starts_with("voici ")
                    || fact_lower.starts_with("here is ")
                    || fact_lower.starts_with("here's ")
                    || fact_lower.starts_with("voilà ")
                    || fact_lower.contains("```")
                    || fact_lower.contains("## ")
                    || fact_lower.contains("| étape ")
                    || fact_lower.contains("| step ")
                    || fact_lower.contains("souhaitez-vous")
                    || fact.starts_with("[Task]\n")
                    // Confused assistant responses: clarifying questions, enthusiastic capability
                    // listings, or structured documents that are not user-relevant session facts.
                    || fact_lower.starts_with("could you ")
                    || fact_lower.starts_with("sure! ")
                    || fact_lower.starts_with("sure, ")
                    || fact_lower.starts_with("bien sûr !")
                    || fact_lower.starts_with("bien sur !")
                    || fact_lower.starts_with("bien sûr,")
                    || fact_lower.starts_with("bien sur,")
                    || fact_lower.starts_with("vous pouvez me ")
                    || fact_lower.starts_with("vous pouvez m'")
                    || fact_lower.starts_with("certainly! ")
                    || fact_lower.starts_with("certainly, ")
                    || fact_lower.starts_with("of course! ")
                    || fact_lower.starts_with("of course, ");
                if !fact.trim().is_empty() && !is_confused_redirect {
                    s.facts.push(fact);
                }
            }) {
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::SessionStateSnapshot,
                        Some(serde_json::json!({
                            "schema_version": 1,
                            "session_id": session_id,
                            "state": st,
                        })),
                    )
                    .with_correlation(task_id),
                );
            }
        }
        if !is_small_talk_fast_lane {
            let outcome_label = if studio_verify_error.is_some() {
                "failed"
            } else {
                "completed"
            };
            let summary_preview: String = if let Some(ref e) = studio_verify_error {
                e.chars().take(300).collect()
            } else {
                reply_text.chars().take(300).collect()
            };
            learn_from_task_outcome_async(
                long_term_client.clone(),
                task_id,
                message.clone(),
                outcome_label.to_string(),
                summary_preview,
                Some(session_id.clone()),
                structured.intent_slug.clone(),
            )
            .await;
        }
    }
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

fn split_path_query(path: &str) -> (&str, &str) {
    path.split_once('?').map(|(p, q)| (p, q)).unwrap_or((path, ""))
}

fn parse_query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(
                    urlencoding::decode(v)
                        .map(|c| c.into_owned())
                        .unwrap_or_else(|_| v.to_string()),
                );
            }
        }
    }
    None
}

async fn get_autonomous_mission_state(
    data_dir: &Path,
    store_path: &Path,
    am: &Arc<RwLock<AutonomousMissionConfig>>,
) -> String {
    let cfg = am.read().await;
    let am_store = match AutonomousMissionStore::open(store_path) {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let (last_hb, last_tid) = am_store.get_meta().unwrap_or((None, None));
    let report_abs = data_dir.join(&cfg.report_dir);
    let horizon_s = match cfg.horizon {
        Horizon::Short => "short",
        Horizon::Medium => "medium",
        Horizon::Long => "long",
    };
    let status_s = match cfg.status {
        MissionStatusYaml::Active => "active",
        MissionStatusYaml::Paused => "paused",
        MissionStatusYaml::Completed => "completed",
    };
    let next_hb = last_hb.map(|t| t + chrono::Duration::minutes(cfg.heartbeat_interval_minutes as i64));
    let role_definitions: Vec<serde_json::Value> = cfg
        .role_definitions
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.name,
                "responsibility": r.responsibility,
                "preferred_agent_type": r.preferred_agent_type,
            })
        })
        .collect();
    let body = serde_json::json!({
        "enabled": cfg.enabled,
        "global_context": cfg.global_context.as_str(),
        "horizon": horizon_s,
        "objective": cfg.objective.as_str(),
        "heartbeat_interval_minutes": cfg.heartbeat_interval_minutes,
        "report_dir": cfg.report_dir.as_str(),
        "report_path_absolute": report_abs.display().to_string(),
        "session_id": cfg.session_id.as_str(),
        "status": status_s,
        "operating_rules": cfg.operating_rules.as_str(),
        "role_definitions": role_definitions,
        "heartbeat_preferred_task_type": cfg.heartbeat_preferred_task_type.as_str(),
        "last_heartbeat_at": last_hb.map(|t| t.to_rfc3339()),
        "last_task_id": last_tid.map(|u| u.to_string()),
        "next_heartbeat_approx_at": next_hb.map(|t| t.to_rfc3339()),
    });
    json_response("200 OK", &body.to_string())
}

async fn put_autonomous_mission_state(
    data_dir: &Path,
    store_path: &Path,
    am: &Arc<RwLock<AutonomousMissionConfig>>,
    body: &[u8],
) -> String {
    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(x) => x,
        Err(_) => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
    };
    {
        let mut w = am.write().await;
        let _ = merge_from_json_partial(&mut *w, &v);
        if let Err(e) = persist_config_and_snapshot(data_dir, store_path, &*w) {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    }
    get_autonomous_mission_state(data_dir, store_path, am).await
}

async fn get_autonomous_mission_events_list(store_path: &Path, query: &str) -> String {
    let limit = parse_query_param(query, "limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100)
        .min(1000);
    let since = parse_query_param(query, "since").and_then(|s| {
        chrono::DateTime::parse_from_rfc3339(s.trim())
            .ok()
            .map(|d| d.with_timezone(&chrono::Utc))
    });
    let am_store = match AutonomousMissionStore::open(store_path) {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let events = match am_store.list_events_since(since, limit) {
        Ok(e) => e,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let arr: Vec<_> = events
        .iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id,
                "at": e.at.to_rfc3339(),
                "event_type": e.event_type,
                "payload": e.payload,
            })
        })
        .collect();
    json_response("200 OK", &serde_json::json!({ "events": arr }).to_string())
}

async fn post_autonomous_mission_status(
    data_dir: &Path,
    store_path: &Path,
    am: &Arc<RwLock<AutonomousMissionConfig>>,
    status: MissionStatusYaml,
) -> String {
    {
        let mut w = am.write().await;
        w.status = status;
        if let Err(e) = persist_config_and_snapshot(data_dir, store_path, &*w) {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    }
    get_autonomous_mission_state(data_dir, store_path, am).await
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
    _tools_executor: Option<
        &std::sync::Arc<tokio::sync::RwLock<std::sync::Arc<akasha_tools::ToolExecutor>>>,
    >,
    skill_registry: &std::sync::Arc<crate::skills::SkillRegistry>,
    short_term: Option<std::sync::Arc<ShortTermStore>>,
    long_term_client: Option<LongTermMemoryClient>,
    human_input_store: Option<HumanInputStore>,
    user_rag_store: &crate::user_rag::SharedUserRagStore,
    agent_profile_cache: &AgentProfileCache,
    update_cache: &UpdateCheckCache,
    task_usage_store: &TaskUsageStore,
    device_bridge: Option<&std::sync::Arc<crate::device_bridge::DeviceBridge>>,
    autonomous_mission: Option<Arc<RwLock<AutonomousMissionConfig>>>,
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
                || origin.starts_with("http://tauri.localhost")
                || origin.starts_with("https://tauri.localhost")
                || origin.starts_with("tauri://");
            if !is_local {
                tracing::warn!(origin = %origin, method = %method, path = %path, "CSRF: rejected request from non-local origin");
                return json_response("403 Forbidden", r#"{"error":"origin_not_allowed"}"#);
            }
        }
    }

    let (path_only, query_str) = split_path_query(path);

    if let Some(resp) = crate::api_studio::handle_studio_route(
        method,
        path_only,
        query_str,
        body.as_deref(),
        data_dir,
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_workspace_graph::handle_workspace_graph(
        method,
        path_only,
        body.as_deref(),
        data_dir,
        store_path,
    )
    .await
    {
        return resp;
    }

    if path_only == "/api/autonomous-mission" {
        let Some(ref am) = autonomous_mission else {
            return json_response(
                "503 Service Unavailable",
                r#"{"error":"autonomous_mission_unavailable"}"#,
            );
        };
        if method == "GET" {
            return get_autonomous_mission_state(data_dir, store_path, am).await;
        }
        if method == "PUT" {
            let Some(ref b) = body else {
                return json_response("400 Bad Request", r#"{"error":"body_required"}"#);
            };
            return put_autonomous_mission_state(data_dir, store_path, am, b).await;
        }
        return json_response("405 Method Not Allowed", r#"{"error":"method_not_allowed"}"#);
    }
    if path_only == "/api/autonomous-mission/events" && method == "GET" {
        if autonomous_mission.is_none() {
            return json_response(
                "503 Service Unavailable",
                r#"{"error":"autonomous_mission_unavailable"}"#,
            );
        }
        return get_autonomous_mission_events_list(store_path, query_str).await;
    }
    if path_only == "/api/autonomous-mission/pause" && method == "POST" {
        let Some(ref am) = autonomous_mission else {
            return json_response(
                "503 Service Unavailable",
                r#"{"error":"autonomous_mission_unavailable"}"#,
            );
        };
        return post_autonomous_mission_status(data_dir, store_path, am, MissionStatusYaml::Paused).await;
    }
    if path_only == "/api/autonomous-mission/resume" && method == "POST" {
        let Some(ref am) = autonomous_mission else {
            return json_response(
                "503 Service Unavailable",
                r#"{"error":"autonomous_mission_unavailable"}"#,
            );
        };
        return post_autonomous_mission_status(data_dir, store_path, am, MissionStatusYaml::Active).await;
    }

    if method == "GET" && (path == "/" || path.is_empty()) {
        return json_response("200 OK", r#"{"status":"ok"}"#);
    }

    // GET /api/update/status — cached result of latest.json from Akasha_app (for UI update banner)
    if method == "GET" && path.starts_with("/api/session-state") {
        let session_id = path
            .split('?')
            .nth(1)
            .and_then(|q| {
                q.split('&').find_map(|p| {
                    if let Some(v) = p.strip_prefix("session_id=") {
                        Some(
                            urlencoding::decode(v)
                                .map(|c| c.into_owned())
                                .unwrap_or_else(|_| v.to_string()),
                        )
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_default();
        if session_id.is_empty() {
            return json_response(
                "400 Bad Request",
                r#"{"error":"session_id query parameter required"}"#,
            );
        }
        let st = crate::session_state::load(data_dir, &session_id);
        return json_response(
            "200 OK",
            &serde_json::to_string(&serde_json::json!({
                "session_id": session_id,
                "state": st,
            }))
            .unwrap_or_else(|_| "{}".into()),
        );
    }

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
            let body_json = body
                .as_deref()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
            let request_id = body_json
                .as_ref()
                .and_then(|j| j.get("request_id"))
                .and_then(|v| v.as_str());
            let success = body_json
                .as_ref()
                .and_then(|j| j.get("success"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let data = body_json
                .as_ref()
                .and_then(|j| j.get("data"))
                .and_then(|v| v.as_str())
                .map(String::from);
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
            return json_response(
                "501 Not Implemented",
                r#"{"error":"device_bridge_unavailable"}"#,
            );
        }
    }

    // GET /api/voice/status — whether TTS/STT are configured (voice_router.yaml)
    if method == "GET" && path == "/api/voice/status" {
        let config = crate::voice::load_voice_config(data_dir);
        let body = serde_json::json!({
            "tts_configured": config.as_ref().map_or(false, |c| c.tts_configured()),
            "stt_configured": config.as_ref().map_or(false, |c| c.stt_configured()),
        });
        return json_response("200 OK", &body.to_string());
    }

    // POST /api/voice/tts — synthesize text to audio. Body: { "text": "..." }. Returns { "data_url": "data:audio/wav;base64,...", "message": "..." }.
    if method == "POST" && path == "/api/voice/tts" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let text = body_json
            .as_ref()
            .and_then(|j| j.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if text.is_empty() {
            return json_response("400 Bad Request", r#"{"error":"text_required"}"#);
        }
        match crate::voice::speech_synthesize_impl(data_dir, text).await {
            Ok((msg, data_url)) => {
                let body = serde_json::json!({ "message": msg, "data_url": data_url });
                return json_response("200 OK", &body.to_string());
            }
            Err(e) => {
                return json_response(
                    "502 Bad Gateway",
                    &serde_json::json!({ "error": e }).to_string(),
                );
            }
        }
    }

    // POST /api/voice/stt — transcribe audio to text. Body: { "data_url": "data:audio/...;base64,..." } or { "audio_base64": "..." }.
    if method == "POST" && path == "/api/voice/stt" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let audio_input = body_json
            .as_ref()
            .and_then(|j| j.get("data_url").and_then(|v| v.as_str()))
            .or_else(|| {
                body_json
                    .as_ref()
                    .and_then(|j| j.get("audio_base64").and_then(|v| v.as_str()))
            })
            .unwrap_or("");
        let audio_input = audio_input.trim();
        if audio_input.is_empty() {
            return json_response(
                "400 Bad Request",
                r#"{"error":"data_url_or_audio_base64_required"}"#,
            );
        }
        match crate::voice::speech_transcribe_impl(data_dir, audio_input).await {
            Ok(text) => {
                let body = serde_json::json!({ "text": text });
                return json_response("200 OK", &body.to_string());
            }
            Err(e) => {
                return json_response(
                    "502 Bad Gateway",
                    &serde_json::json!({ "error": e }).to_string(),
                );
            }
        }
    }

    // GET /api/agent-profile — read agent profile (name, personality, role, gender, formality, avatar, rules, can_do, cannot_do, traits_override, preferred_mode)
    if method == "GET" && path == "/api/agent-profile" {
        let profile = get_or_load_agent_profile(data_dir, agent_profile_cache).await;
        let body_json = serde_json::json!({
            "name": profile.name,
            "personality": profile.personality,
            "role": profile.role,
            "gender": profile.gender,
            "formality": profile.formality,
            "avatar": profile.avatar,
            "rules": profile.rules,
            "can_do": profile.can_do,
            "cannot_do": profile.cannot_do,
            "traits_override": profile.traits_override,
            "preferred_mode": profile.preferred_mode
        });
        return json_response("200 OK", &body_json.to_string());
    }

    // POST /api/agent-profile — update agent profile (merge with existing). Body: { name?, personality?, role?, gender?, formality?, avatar?, rules?, can_do?, cannot_do?, traits_override?, preferred_mode? }
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
                if let Some(fv) = v.get("formality") {
                    if fv.is_null() {
                        profile.formality = None;
                    } else if let Some(s) = fv.as_str() {
                        let t = s.trim().to_lowercase();
                        profile.formality = match t.as_str() {
                            "formal" => Some("formal".to_string()),
                            "informal" => Some("informal".to_string()),
                            _ => None,
                        };
                    }
                }
                if v.get("avatar").is_some() {
                    profile.avatar = v.get("avatar").and_then(|x| x.as_str()).map(String::from);
                }
                if let Some(arr) = v.get("rules").and_then(|x| x.as_array()) {
                    profile.rules = arr
                        .iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect();
                }
                if let Some(arr) = v.get("can_do").and_then(|x| x.as_array()) {
                    profile.can_do = arr
                        .iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect();
                }
                if let Some(arr) = v.get("cannot_do").and_then(|x| x.as_array()) {
                    profile.cannot_do = arr
                        .iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect();
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
                    profile.preferred_mode = v
                        .get("preferred_mode")
                        .and_then(|x| x.as_str())
                        .map(String::from);
                }
            }
        }
        // Persist default name if none or empty so the agent always has an identity on disk
        if profile
            .name
            .as_deref()
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
        {
            profile.name = Some(AgentProfile::DEFAULT_NAME.to_string());
        }
        match profile.save(data_dir) {
            Ok(()) => {
                set_agent_profile_cache(agent_profile_cache, profile).await;
                return json_response(
                    "200 OK",
                    r#"{"ok":true,"message":"Profil agent mis à jour"}"#,
                );
            }
            Err(e) => {
                return json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error": e.to_string() }).to_string(),
                )
            }
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
            "proactive_check_in_enabled": profile.proactive_check_in_enabled,
            "proactive_check_in_interval_days": profile.proactive_check_in_interval_days,
        });
        return json_response("200 OK", &body_json.to_string());
    }

    // POST /api/user-profile — update user profile. Body: { first_name?, last_name?, how_to_call?, onboarding_completed?, proactive_check_in_enabled?, proactive_check_in_interval_days? }
    if method == "POST" && path == "/api/user-profile" {
        let mut profile = UserProfile::load(data_dir);
        if let Some(body) = body.as_deref() {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) {
                if v.get("first_name").is_some() {
                    profile.first_name = v
                        .get("first_name")
                        .and_then(|x| x.as_str())
                        .map(String::from);
                }
                if v.get("last_name").is_some() {
                    profile.last_name = v
                        .get("last_name")
                        .and_then(|x| x.as_str())
                        .map(String::from);
                }
                if v.get("how_to_call").is_some() {
                    profile.how_to_call = v
                        .get("how_to_call")
                        .and_then(|x| x.as_str())
                        .map(|s| s.trim().to_string());
                }
                if let Some(b) = v.get("onboarding_completed").and_then(|x| x.as_bool()) {
                    profile.onboarding_completed = b;
                }
                if v.get("proactive_check_in_enabled").is_some() {
                    profile.proactive_check_in_enabled = v
                        .get("proactive_check_in_enabled")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false);
                }
                if v.get("proactive_check_in_interval_days").is_some() {
                    profile.proactive_check_in_interval_days =
                        v.get("proactive_check_in_interval_days")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0) as u32;
                }
            }
        }
        match profile.save(data_dir) {
            Ok(()) => {
                return json_response(
                    "200 OK",
                    r#"{"ok":true,"message":"Profil utilisateur mis à jour"}"#,
                )
            }
            Err(e) => {
                return json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error": e.to_string() }).to_string(),
                )
            }
        }
    }

    // GET /api/first-message?context=onboarding|first_today|proactive — agent-initiated first message (onboarding, daily greeting, proactive check-in)
    if method == "GET" && path.starts_with("/api/first-message") {
        let context = path
            .split('?')
            .nth(1)
            .and_then(|q| {
                q.split('&').find(|p| p.starts_with("context=")).map(|p| {
                    urlencoding::decode(p.trim_start_matches("context="))
                        .unwrap_or_default()
                        .into_owned()
                })
            })
            .unwrap_or_default();
        let session_id = format!("day-{}", chrono::Utc::now().format("%Y-%m-%d"));
        let user_profile = UserProfile::load(data_dir);

        if context == "onboarding" && !user_profile.has_how_to_call() {
            let message = "Bonjour ! Pour personnaliser nos échanges, comment dois-je vous appeler ? (prénom ou surnom)";
            if let Some(ref st) = short_term {
                st.append(&session_id, "assistant", message.to_string())
                    .await;
            }
            let body_json = serde_json::json!({ "message": message, "session_id": session_id });
            return json_response("200 OK", &body_json.to_string());
        }

        if context == "proactive"
            && user_profile.has_how_to_call()
            && user_profile.proactive_check_in_enabled
            && user_profile.proactive_check_in_interval_days > 0
        {
            let now = chrono::Utc::now();
            let last = UserProfile::load_last_activity(data_dir);
            let interval_days = user_profile.proactive_check_in_interval_days as i64;
            let show = match last {
                None => true,
                Some(t) => (now - t).num_days() >= interval_days,
            };
            if show {
                let how = user_profile.how_to_call.as_deref().unwrap_or("").trim();
                let message = format!(
                    "Ça fait un moment, {} ! Tu veux qu'on travaille sur quelque chose ?",
                    how
                );
                if let Some(ref st) = short_term {
                    st.append(&session_id, "assistant", message.clone()).await;
                }
                let body_json = serde_json::json!({ "message": message, "session_id": session_id });
                return json_response("200 OK", &body_json.to_string());
            }
        }

        if context == "first_today" && user_profile.has_how_to_call() {
            let how = user_profile.how_to_call.as_deref().unwrap_or("").trim();
            let message = format!("Bonjour {}, quoi de neuf aujourd'hui ?", how);
            if let Some(ref st) = short_term {
                st.append(&session_id, "assistant", message.clone()).await;
            }
            let body_json = serde_json::json!({ "message": message, "session_id": session_id });
            return json_response("200 OK", &body_json.to_string());
        }

        // Other contexts: return empty so UI does not show a duplicate message
        let body_json = serde_json::json!({ "message": "", "session_id": session_id });
        return json_response("200 OK", &body_json.to_string());
    }

    // POST /api/personality-memory — store a structured personality preference (Phase 3). Body: { "key": "preferred_tone"|"technical_depth_preference"|..., "value": "..." }
    if method == "POST" && path == "/api/personality-memory" {
        let Some(client) = long_term_client.clone() else {
            return json_response(
                "503 Service Unavailable",
                r#"{"error":"long_term_memory_unavailable"}"#,
            );
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
        let value = v
            .get("value")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
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
            Ok(Ok(id)) => {
                return json_response(
                    "200 OK",
                    &serde_json::json!({ "ok": true, "id": id.to_string() }).to_string(),
                )
            }
            Ok(Err(e)) => {
                return json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error": e }).to_string(),
                )
            }
            Err(e) => {
                return json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error": e.to_string() }).to_string(),
                )
            }
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
                    .map(|p| {
                        urlencoding::decode(p.trim_start_matches("session_id="))
                            .unwrap_or_default()
                            .into_owned()
                    })
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

    // DELETE /api/memory/session?session_id=... — remove short-term turns and session_state for this session
    if method == "DELETE" && path.starts_with("/api/memory/session") {
        let session_id = path
            .split('?')
            .nth(1)
            .and_then(|q| {
                q.split('&').find_map(|p| {
                    if let Some(v) = p.strip_prefix("session_id=") {
                        Some(
                            urlencoding::decode(v)
                                .map(|c| c.into_owned())
                                .unwrap_or_else(|_| v.to_string()),
                        )
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_default();
        if session_id.is_empty() || !crate::memory::is_safe_session_id(&session_id) {
            return json_response(
                "400 Bad Request",
                r#"{"error":"invalid or missing session_id"}"#,
            );
        }
        if let Some(ref st) = short_term {
            st.delete_session(&session_id).await;
        } else {
            let path = data_dir
                .join("short_term")
                .join(format!("{}.json", session_id));
            let _ = tokio::fs::remove_file(path).await;
        }
        let _ = crate::session_state::delete(data_dir, &session_id);
        let body_json = serde_json::json!({ "ok": true, "session_id": session_id });
        return json_response("200 OK", &body_json.to_string());
    }

    // POST /api/chat/suggest-thread-title — short title from first user message (UI chat threads)
    if method == "POST" && path == "/api/chat/suggest-thread-title" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let message = body_json
            .as_ref()
            .and_then(|v| v.get("message").and_then(|v| v.as_str()))
            .unwrap_or("")
            .trim();
        if message.is_empty() {
            return json_response("400 Bad Request", r#"{"error":"missing message"}"#);
        }
        fn fallback_title(msg: &str) -> String {
            const MAX: usize = 48;
            let t = msg.trim();
            let n = t.chars().count();
            if n <= MAX {
                t.to_string()
            } else {
                format!("{}…", t.chars().take(MAX).collect::<String>())
            }
        }
        let fallback = fallback_title(message);
        let prompt = format!(
            "Reply with ONLY a short conversation title (max 60 characters, no quotation marks, same language as the user message).\n\nUser message:\n{}",
            message
        );
        let req = CompletionRequest {
            prompt,
            max_tokens: Some(80),
            temperature: Some(0.3),
            preferred_task_type: None,
            system_prompt: Some(
                "Output only the title text. No quotes. No leading 'Title:'.".to_string(),
            ),
            image_data_urls: None,
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            repeat_penalty: None,
            num_ctx: None,
            num_gpu: None,
            thinking_level: None,
        };
        let title_timeout = std::time::Duration::from_secs(15);
        let title = match tokio::time::timeout(title_timeout, llm_router.complete(&req)).await {
            Ok(Ok(resp)) => {
                let mut t = resp.text.trim().to_string();
                if let Some(i) = t.find('\n') {
                    t.truncate(i);
                }
                t = t
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .trim()
                    .to_string();
                if t.starts_with("Title:") || t.starts_with("Titre:") {
                    t = t
                        .trim_start_matches("Title:")
                        .trim_start_matches("Titre:")
                        .trim()
                        .to_string();
                }
                if t.chars().count() > 60 {
                    t = t.chars().take(60).collect();
                }
                if t.is_empty() {
                    fallback
                } else {
                    t
                }
            }
            _ => fallback,
        };
        let body = serde_json::json!({ "title": title });
        return json_response("200 OK", &body.to_string());
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
                        let decoded = urlencoding::decode(v)
                            .unwrap_or_else(|_| std::borrow::Cow::Borrowed(v));
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

    // GET /api/memory/long-term?limit=200&offset=0 — recent long-term entries (content, created_at, source, related), paginated
    if method == "GET" && path.starts_with("/api/memory/long-term") {
        let (limit, offset) = path
            .split('?')
            .nth(1)
            .map(|q| {
                let mut limit = 200usize;
                let mut offset = 0usize;
                for part in q.split('&') {
                    if let Some(v) = part.strip_prefix("limit=") {
                        if let Ok(n) = v.parse::<usize>() {
                            limit = n.min(200);
                        }
                    } else if let Some(v) = part.strip_prefix("offset=") {
                        if let Ok(n) = v.parse::<usize>() {
                            offset = n;
                        }
                    }
                }
                (limit, offset)
            })
            .unwrap_or((200, 0));
        let (entries, total) = if let Some(ref client) = long_term_client {
            let client = client.clone();
            tokio::task::spawn_blocking(move || client.list(limit, offset))
                .await
                .unwrap_or((vec![], 0))
        } else {
            (vec![], 0)
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
            "total": total,
            "long_term_available": long_term_client.is_some()
        });
        return json_response("200 OK", &body_json.to_string());
    }

    // POST /api/memory/rebuild-relations — recompute embedding-based relations (similar / relates_to tiers)
    if method == "POST" && path == "/api/memory/rebuild-relations" {
        const DEFAULT_MAX_PER_ENTRY: usize = 5;
        let result = match long_term_client {
            Some(ref client) => {
                let client = client.clone();
                tokio::task::spawn_blocking(move || {
                    client.rebuild_similar_relations(DEFAULT_MAX_PER_ENTRY)
                })
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
        let id = path
            .trim_start_matches("/api/memory/long-term/")
            .split('?')
            .next()
            .unwrap_or("")
            .trim();
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
            Err(e) => {
                return json_response(
                    "500 Internal Server Error",
                    &format!(r#"{{"error":"{}"}}"#, e),
                )
            }
        }
    }

    // GET /api/status — same as / but explicit for slash commands
    if method == "GET" && path == "/api/status" {
        return json_response("200 OK", r#"{"status":"ok"}"#);
    }

    // GET /api/timeline — unified timeline of recent events (Phase 5 AI OS). Query: ?limit=50&task_id=uuid (optional).
    if method == "GET" && path.starts_with("/api/timeline") {
        let (limit, task_id_filter) = path
            .split('?')
            .nth(1)
            .map(|q| {
                let mut limit = 50u32;
                let mut task_id_filter = None;
                for part in q.split('&') {
                    if let Some(v) = part.strip_prefix("limit=") {
                        if let Ok(n) = v.parse::<u32>() {
                            limit = n.min(200);
                        }
                    } else if let Some(v) = part.strip_prefix("task_id=") {
                        if let Ok(id) = Uuid::parse_str(v) {
                            task_id_filter = Some(id);
                        }
                    }
                }
                (limit, task_id_filter)
            })
            .unwrap_or((50, None));

        // Use a bounded heap (by timestamp) to keep only the `limit` most recent events.
        // This avoids allocating and sorting a Vec of *all* events.
        let mut heap: BinaryHeap<TimelineHeapEntry> = BinaryHeap::new();
        {
            let g = events.read().await;
            let mut counter: usize = 0;
            for (tid, list) in g.iter() {
                if let Some(filter) = task_id_filter {
                    if *tid != filter {
                        continue;
                    }
                }
                for e in list.iter() {
                    // The custom Ord for TimelineHeapEntry inverts the natural timestamp order so that
                    // the *oldest* entry is the "greatest" and gets popped first when the heap exceeds `limit`.
                    // This keeps only the `limit` most-recent events without a full sort.
                    let at = e.at.clone();
                    // Parse to milliseconds for correct ordering (lexicographic RFC3339 comparison
                    // is unreliable when fractional seconds are omitted for whole-second values).
                    let at_ms = chrono::DateTime::parse_from_rfc3339(&at)
                        .map(|dt| dt.timestamp_millis())
                        .unwrap_or(0);
                    let task_id = tid.to_string();
                    let event_type = e.event_type.clone();
                    let payload = e.payload.clone();
                    heap.push(TimelineHeapEntry {
                        at,
                        at_ms,
                        counter,
                        task_id,
                        event_type,
                        payload,
                    });
                    counter = counter.wrapping_add(1);
                    if heap.len() > limit as usize {
                        heap.pop();
                    }
                }
            }
        }

        // Extract the top `limit` events and sort them chronologically (oldest to newest).
        let mut selected: Vec<(String, String, Option<serde_json::Value>, String, i64)> = heap
            .into_iter()
            .map(|entry| {
                (
                    entry.task_id,
                    entry.event_type,
                    entry.payload,
                    entry.at,
                    entry.at_ms,
                )
            })
            .collect();
        selected.sort_by(|(_, _, _, _, a_ms), (_, _, _, _, b_ms)| a_ms.cmp(b_ms));

        let list: Vec<serde_json::Value> = selected
            .into_iter()
            .map(|(task_id, event_type, payload, at, _)| {
                serde_json::json!({ "task_id": task_id, "event_type": event_type, "payload": payload, "at": at })
            })
            .collect();
        let body_json = serde_json::json!({ "events": list });
        return json_response("200 OK", &body_json.to_string());
    }

    // GET /api/metrics — task counts and simple metrics (Phase 5 AI OS).
    if method == "GET" && path == "/api/metrics" {
        let (pending, running, completed, failed, paused, interrupted) =
            match TaskStore::open(store_path) {
                Ok(store) => {
                    let tasks = store.get_all().unwrap_or_default();
                    let mut pending = 0;
                    let mut running = 0;
                    let mut completed = 0;
                    let mut failed = 0;
                    let mut paused = 0;
                    let mut interrupted = 0;
                    for t in &tasks {
                        match t.status {
                            TaskStatus::Pending
                            | TaskStatus::Queued
                            | TaskStatus::WaitingUserInput => pending += 1,
                            TaskStatus::Running => running += 1,
                            TaskStatus::Completed => completed += 1,
                            TaskStatus::Failed | TaskStatus::Cancelled => failed += 1,
                            TaskStatus::Paused => paused += 1,
                            TaskStatus::Interrupted => interrupted += 1,
                        }
                    }
                    (pending, running, completed, failed, paused, interrupted)
                }
                Err(_) => (0, 0, 0, 0, 0, 0),
            };
        let body_json = serde_json::json!({
            "tasks": { "pending": pending, "running": running, "completed": completed, "failed": failed, "paused": paused, "interrupted": interrupted },
            "stability": llm_router.metrics().stability_summary(),
            "protocol": { "unknown_message_count": unknown_external_message_count() }
        });
        return json_response("200 OK", &body_json.to_string());
    }

    // GET /api/agents — list known agent roles (Phase 6 AI OS cockpit).
    if method == "GET" && path == "/api/agents" {
        const AGENT_ROLES: &[&str] = &[
            "conversation",
            "search",
            "code",
            "financial",
            "documentalist",
            "project_manager",
            "technical_writer",
            "research",
            "security_audit",
            "creative",
            "analyst",
            "architect",
            "frontend",
            "backend",
            "database",
            "integration",
            "qa",
            "system",
            "image_generation",
        ];
        let list: Vec<serde_json::Value> = AGENT_ROLES
            .iter()
            .map(|name| serde_json::json!({ "id": name, "name": name }))
            .collect();
        let body_json = serde_json::json!({ "agents": list });
        return json_response("200 OK", &body_json.to_string());
    }

    // GET /api/plugins — voir plus bas (Phase 5 PluginRegistry) : tableau JSON pour CLI/Tauri/TUI.
    // Les skills chargeables pour agents sont sur GET /api/skills (pas de doublon ici).

    // GET /api/doctor — health checks from daemon (for slash /doctor)
    if method == "GET" && path == "/api/doctor" {
        let mut checks: Vec<serde_json::Value> = Vec::new();
        checks.push(
            serde_json::json!({ "id": "daemon", "ok": true, "description": "Daemon running" }),
        );
        checks.push(
            serde_json::json!({ "id": "os", "ok": true, "description": std::env::consts::OS }),
        );

        let ollama_ok = if let Some(u) = ollama_base_url {
            let test_url = format!("{}/api/tags", u.trim_end_matches('/'));
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new());
            client
                .get(&test_url)
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
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

        let spec_ok = packaged_spec_check_ok(spec_dir);
        checks.push(serde_json::json!({
            "id": "spec_dir",
            "ok": spec_ok,
            "description": if spec_dir.exists() {
                "Spec directory present"
            } else if std::env::var_os("AKASHA_SPEC_DIR").is_some() {
                "Spec directory missing"
            } else {
                "Spec directory not bundled (OK for installed binaries)"
            }
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

        // Playwright managed browser (optional): runner path, npm package, node/npm on PATH
        let runner_path = crate::browser::find_playwright_runner_path();
        let runner_path_str = runner_path.as_ref().map(|p| p.display().to_string());
        let runner_ok = runner_path.is_some();
        let playwright_pkg_path = runner_path.as_ref().and_then(|p| {
            crate::browser::playwright_runner_dir(p)
                .map(|d| d.join("node_modules").join("playwright"))
        });
        let playwright_pkg_present = playwright_pkg_path
            .as_ref()
            .map(|p| p.is_dir())
            .unwrap_or(false);
        let auto_install_off = std::env::var("AKASHA_PLAYWRIGHT_AUTO_INSTALL")
            .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
            .unwrap_or(false);
        let playwright_pkg_ok = if playwright_pkg_present {
            true
        } else if !runner_ok {
            false
        } else {
            !auto_install_off
        };
        let playwright_pkg_desc = if playwright_pkg_present {
            "node_modules/playwright present"
        } else if !runner_ok {
            "Playwright runner not resolved"
        } else if auto_install_off {
            "node_modules/playwright missing (set AKASHA_PLAYWRIGHT_AUTO_INSTALL or run npm install in runner dir)"
        } else {
            "node_modules/playwright not installed yet (will auto-install on first browser use)"
        };

        let (node_on_path, node_version_line) = match tokio::time::timeout(
            std::time::Duration::from_secs(3),
            tokio::process::Command::new("node")
                .arg("--version")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .output(),
        )
        .await
        {
            Ok(Ok(o)) if o.status.success() => (
                true,
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string()),
            ),
            _ => (false, None),
        };

        let npm_exe = if cfg!(windows) { "npm.cmd" } else { "npm" };
        let npm_on_path = matches!(
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                tokio::process::Command::new(npm_exe)
                    .arg("--version")
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .output(),
            )
            .await,
            Ok(Ok(ref o)) if o.status.success()
        );

        checks.push(serde_json::json!({
            "id": "playwright_runner",
            "ok": runner_ok,
            "description": if runner_ok {
                format!("Playwright runner: {}", runner_path_str.as_deref().unwrap_or("?"))
            } else {
                "Playwright runner not found (use release layout, AKASHA_PLAYWRIGHT_RUNNER, or data dir)".to_string()
            }
        }));
        checks.push(serde_json::json!({
            "id": "playwright_node",
            "ok": node_on_path,
            "description": if node_on_path {
                format!(
                    "node on PATH ({})",
                    node_version_line.as_deref().unwrap_or("?")
                )
            } else {
                "node not found on PATH (install Node.js for managed browser)".to_string()
            }
        }));
        checks.push(serde_json::json!({
            "id": "playwright_npm",
            "ok": npm_on_path,
            "description": if npm_on_path { "npm on PATH" } else { "npm not found on PATH" }
        }));
        checks.push(serde_json::json!({
            "id": "playwright_package",
            "ok": playwright_pkg_ok,
            "description": playwright_pkg_desc
        }));

        let playwright_json = serde_json::json!({
            "runner_path": runner_path_str,
            "runner_found": runner_ok,
            "node_modules_playwright": playwright_pkg_present,
            "auto_install_disabled": auto_install_off,
            "node_on_path": node_on_path,
            "node_version": node_version_line,
            "npm_on_path": npm_on_path,
        });

        let all_ok = checks
            .iter()
            .all(|c| c.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));
        let body_json =
            serde_json::json!({ "ok": all_ok, "checks": checks, "playwright": playwright_json })
                .to_string();
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
        let body_json = serde_json::to_string(&serde_json::json!({ "vars": vars }))
            .unwrap_or_else(|_| "{}".into());
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
        let key = body_json
            .as_ref()
            .and_then(|j| j.get("key"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let value = body_json
            .as_ref()
            .and_then(|j| j.get("value"))
            .and_then(|v| v.as_str())
            .map(String::from);
        match (key, value) {
            (Some(k), Some(v)) if !k.is_empty() => {
                let env_path = data_dir.join("akasha.env");
                let mut lines: Vec<String> = if env_path.exists() {
                    std::fs::read_to_string(&env_path)
                        .unwrap_or_default()
                        .lines()
                        .map(String::from)
                        .collect()
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
                    return json_response(
                        "200 OK",
                        &serde_json::json!({ "ok": true, "key": k }).to_string(),
                    );
                }
            }
            _ => {}
        }
        return json_response("400 Bad Request", r#"{"error":"missing key or value"}"#);
    }

    // GET /api/vault/keys — list vault key names (no values)
    if method == "GET" && path == "/api/vault/keys" {
        let keys = akasha_vault::open_vault(data_dir)
            .ok()
            .and_then(|v| v.list_keys().ok())
            .unwrap_or_default();
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
        let key = body_json
            .as_ref()
            .and_then(|j| j.get("key"))
            .and_then(|v| v.as_str())
            .map(String::from);
        match key {
            Some(k) if !k.is_empty() => match akasha_vault::open_vault(data_dir) {
                Ok(v) => match v.delete(&k) {
                    Ok(()) => {
                        return json_response(
                            "200 OK",
                            &serde_json::json!({ "ok": true, "key": k }).to_string(),
                        )
                    }
                    Err(akasha_vault::VaultError::NotFound(_)) => {
                        return json_response(
                            "404 Not Found",
                            &serde_json::json!({ "error": "not_found", "key": k }).to_string(),
                        )
                    }
                    Err(e) => {
                        return json_response(
                            "500 Internal Server Error",
                            &serde_json::json!({ "error": e.to_string() }).to_string(),
                        )
                    }
                },
                Err(e) => {
                    return json_response(
                        "503 Service Unavailable",
                        &serde_json::json!({ "error": e.to_string() }).to_string(),
                    )
                }
            },
            _ => return json_response("400 Bad Request", r#"{"error":"missing or empty key"}"#),
        }
    }

    // POST /api/restart — signal daemon to exit (supervisor restarts it)
    if method == "POST" && path == "/api/restart" {
        if let Some(ref tx) = restart_tx {
            let _ = tx.send(()).await;
            return json_response("200 OK", r#"{"ok":true,"message":"Redémarrage demandé"}"#);
        }
        return json_response(
            "503 Service Unavailable",
            r#"{"error":"restart_not_available"}"#,
        );
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
        if let (Some(ref app_id), Some(ref app_password)) = (
            &channel_config.teams_app_id,
            &channel_config.teams_app_password,
        ) {
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
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let mut message = body_json
            .as_ref()
            .and_then(|v| v.get("message").and_then(|v| v.as_str().map(String::from)))
            .unwrap_or_default();
        // Parse attachments: images -> data URLs for vision; documents -> append extracted text to message.
        let image_data_urls: Option<Vec<String>> = {
            let arr = body_json
                .as_ref()
                .and_then(|v| v.get("attachments").and_then(|a| a.as_array()));
            let mut urls = Vec::new();
            let mut doc_texts = Vec::new();
            if let Some(arr) = arr {
                for att in arr {
                    let typ = att.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    let content_base64 = att
                        .get("content_base64")
                        .and_then(|c| c.as_str())
                        .unwrap_or("");
                    let mime = att
                        .get("mime_type")
                        .and_then(|m| m.as_str())
                        .unwrap_or("image/png");
                    let name = att.get("name").and_then(|n| n.as_str()).unwrap_or("file");
                    if content_base64.is_empty() {
                        continue;
                    }
                    if typ == "image" || mime.starts_with("image/") {
                        let data_url = format!("data:{};base64,{}", mime, content_base64);
                        urls.push(data_url);
                    } else if typ == "document"
                        || mime.starts_with("text/")
                        || mime == "application/pdf"
                    {
                        if let Ok(decoded) = base64::Engine::decode(
                            &base64::engine::general_purpose::STANDARD,
                            content_base64,
                        ) {
                            let text = if mime == "application/pdf" {
                                pdf_extract::extract_text_from_mem(&decoded).unwrap_or_else(|_| {
                                    String::from(
                                        "[Extraction du texte PDF impossible ou PDF vide.]",
                                    )
                                })
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
            let new_session = body_json
                .as_ref()
                .and_then(|v| v.get("new_session"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let provided = body_json.as_ref().and_then(|v| {
                v.get("session_id")
                    .and_then(|v| v.as_str().map(String::from))
            });
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
        // Onboarding: if user profile has no how_to_call, try to extract from message and save
        let mut message = message;
        {
            let mut user_profile = UserProfile::load(data_dir);
            if !user_profile.has_how_to_call() && !message.trim().is_empty() {
                let extracted = extract_how_to_call_from_message(message.trim());
                if let Some(name) = extracted {
                    user_profile.how_to_call = Some(name.clone());
                    user_profile.onboarding_completed = true;
                    if user_profile.first_name.is_none()
                        || user_profile
                            .first_name
                            .as_deref()
                            .unwrap_or("")
                            .trim()
                            .is_empty()
                    {
                        user_profile.first_name = Some(name.clone());
                    }
                    let _ = user_profile.save(data_dir);
                    message = format!(
                        "[L'utilisateur vient de vous indiquer son prénom : {}. Accueillez-le chaleureusement (ex. « Ravi de te connaître, {} ! ») puis répondez à son message.]\n\n{}",
                        name,
                        name,
                        message
                    );
                }
            }
        }
        // Reconnect recap temporarily disabled: it polluted the real user request when the daemon
        // is restarted several times during the same day and could generate repeated summaries.
        // Update last user activity for proactive check-in
        let _ = UserProfile::save_last_activity(data_dir, chrono::Utc::now());
        let priority = body_json
            .as_ref()
            .and_then(|v| v.get("priority").and_then(|p| p.as_str()))
            .map(|s| {
                if s.eq_ignore_ascii_case("high") {
                    TaskPriority::UserHigh
                } else {
                    TaskPriority::UserNormal
                }
            })
            .unwrap_or(TaskPriority::UserNormal);
        let studio_disk_root = if let Some(pid) = body_json
            .as_ref()
            .and_then(|v| v.get("studio_project_id").and_then(|x| x.as_str()))
        {
            match crate::studio::resolve_studio_project_dir(data_dir, pid.trim()) {
                Ok(p) => {
                    let _ = std::fs::create_dir_all(&p);
                    Some(p)
                }
                Err(e) => {
                    let body = serde_json::json!({ "error": "invalid_studio_project", "detail": e });
                    return json_response("400 Bad Request", &body.to_string());
                }
            }
        } else {
            None
        };
        let studio_forced_agent = body_json
            .as_ref()
            .and_then(|v| v.get("studio_assigned_agent").and_then(|x| x.as_str()))
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        let mut studio_evolution_branch = body_json
            .as_ref()
            .and_then(|v| v.get("studio_evolution_branch").and_then(|x| x.as_str()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if studio_evolution_branch.is_none() {
            if let (Some(pid), Some(eid)) = (
                body_json
                    .as_ref()
                    .and_then(|v| v.get("studio_project_id").and_then(|x| x.as_str())),
                body_json
                    .as_ref()
                    .and_then(|v| v.get("studio_evolution_id").and_then(|x| x.as_str())),
            ) {
                studio_evolution_branch =
                    crate::api_studio::evolution_branch_for_id(data_dir, pid.trim(), eid.trim());
            }
        }
        // Build acknowledgment message before moving `message` into the envelope.
        let ack_message = build_ack_message(&message);
        let mut message_for_llm = message;
        if let Some(ref root) = studio_disk_root {
            if let Some(plan) = crate::api_studio::studio_code_plan_message_prefix(root) {
                message_for_llm = format!("{plan}{message_for_llm}");
            }
            if studio_evolution_branch.is_some() {
                message_for_llm = format!(
                    "[Évolution Code Studio — conserver le même périmètre produit et le même type d’application que le dépôt (cf. CODE_STUDIO_PLAN.md ci-dessus et code existant) ; ne pas remplacer par un autre jeu, une autre app ou un autre domaine fonctionnel sauf instruction explicite de l’utilisateur.]\n\n{}",
                    message_for_llm
                );
            }
            if let Some(prefix) = crate::api_studio::studio_tech_stack_message_prefix(root) {
                message_for_llm = format!("{prefix}{message_for_llm}");
            }
        }
        let mut envelope = crate::gateway::MessageEnvelope::api(
            session_id.clone(),
            message_for_llm,
            image_data_urls,
            priority,
        );
        envelope.studio_disk_root = studio_disk_root;
        envelope.studio_forced_agent = studio_forced_agent;
        envelope.studio_evolution_branch = studio_evolution_branch;
        match crate::gateway::handle_envelope(main_agent, store_path, envelope).await {
            Ok(task_id) => {
                let body = serde_json::json!({
                    "ack": true,
                    "task_id": task_id.to_string(),
                    "session_id": session_id,
                    "message": ack_message
                });
                return json_response("200 OK", &body.to_string());
            }
            Err(_) => {
                return json_response("500 Internal Server Error", r#"{"error":"handle_failed"}"#)
            }
        }
    }

    fn decode_url_component(s: &str) -> String {
        urlencoding::decode(s)
            .unwrap_or_else(|_| s.to_string().into())
            .into_owned()
    }

    if method == "GET" && (path == "/api/tasks" || path.starts_with("/api/tasks?")) {
        let status_filter = path.split('?').nth(1).and_then(|q| {
            q.split('&').find(|p| p.starts_with("status=")).map(|p| {
                let raw = p.trim_start_matches("status=");
                decode_url_component(raw)
            })
        });
        return get_task_list(store_path, status_filter).await;
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
                if method == "POST" && parts.get(1) == Some(&"pause") {
                    return pause_task(store_path, id, main_agent).await;
                }
                if method == "POST" && parts.get(1) == Some(&"resume") {
                    return resume_task(store_path, id, main_agent).await;
                }
                if method == "GET" && parts.get(1) == Some(&"events") {
                    return get_task_events(store_path, events, id).await;
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
                    let response_text = body
                        .as_deref()
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
                if parts.get(1) == Some(&"exceptions") {
                    return get_schedule_exceptions(store_path, id).await;
                }
                return get_schedule_by_id(store_path, id).await;
            }
        }
    }
    if method == "POST" && path.contains("/exceptions") {
        let rest = path.trim_start_matches("/api/schedules/");
        let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if parts.get(1) == Some(&"exceptions") {
            if let Some(&schedule_id_str) = parts.first() {
                if let Ok(schedule_id) = Uuid::parse_str(schedule_id_str) {
                    return post_schedule_exception(store_path, schedule_id, body).await;
                }
            }
        }
    }
    if method == "DELETE" && path.contains("/exceptions/") {
        let rest = path.trim_start_matches("/api/schedules/");
        let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if parts.get(1) == Some(&"exceptions") {
            if let (Some(&schedule_id_str), Some(&exception_id_str)) = (parts.first(), parts.get(2))
            {
                if let (Ok(schedule_id), Ok(exception_id)) = (
                    Uuid::parse_str(schedule_id_str),
                    Uuid::parse_str(exception_id_str),
                ) {
                    return delete_schedule_exception(store_path, schedule_id, exception_id).await;
                }
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
        return get_schedule_run_reports(store_path).await;
    }

    // Phase 5: Plugins
    if method == "GET" && path == "/api/plugins" {
        let list = plugin_registry.list();
        let body = serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string());
        return json_response("200 OK", &body);
    }
    // GET /api/plugins/routing_rules[?message=...] — debug dynamic plugin routing rules
    // - Without message: returns all declared routing rules from loaded plugin manifests.
    // - With message: also returns active intents and matched rules for this message.
    if method == "GET"
        && (path == "/api/plugins/routing_rules" || path.starts_with("/api/plugins/routing_rules?"))
    {
        let query = path.split('?').nth(1).unwrap_or("");
        let message = query
            .split('&')
            .find(|p| p.starts_with("message="))
            .and_then(|p| p.strip_prefix("message="))
            .and_then(|raw| urlencoding::decode(raw).ok().map(|d| d.into_owned()));

        let manifests = plugin_registry.manifests();
        let declared_rules: Vec<serde_json::Value> = manifests
            .iter()
            .flat_map(|m| {
                m.routing_rules.iter().map(|r| {
                    serde_json::json!({
                        "plugin_id": m.id,
                        "intent": r.intent,
                        "keywords": r.keywords,
                        "preferred_tools": r.preferred_tools,
                        "forbidden_tools": r.forbidden_tools,
                        "instruction": r.instruction,
                        "priority": r.priority,
                    })
                })
            })
            .collect();

        let (active_intents, matched_rules) = if let Some(ref msg) = message {
            let flags = compute_message_intent_flags(msg);
            let intents = active_intents_from_flags(&flags);
            let tools_executor_snapshot = if let Some(exec_lock) = _tools_executor {
                Some(exec_lock.read().await.clone())
            } else {
                None
            };
            let matched = plugin_registry.match_routing_rules(msg, &intents, |tool_name| {
                tools_executor_snapshot
                    .as_ref()
                    .map(|e| e.policy.can_use_tool(tool_name))
                    .unwrap_or(true)
            });
            (intents, matched)
        } else {
            (Vec::new(), Vec::new())
        };

        let body = serde_json::json!({
            "rules_count": declared_rules.len(),
            "rules": declared_rules,
            "message": message,
            "active_intents": active_intents,
            "matched_rules_count": matched_rules.len(),
            "matched_rules": matched_rules,
        });
        return json_response("200 OK", &body.to_string());
    }
    // POST /api/plugins/routing_rules/match — debug dynamic plugin routing rules with JSON body
    // Body: { "message": "...", "allowed_tools": ["tool_a", "tool_b"] (optional) }
    if method == "POST" && path == "/api/plugins/routing_rules/match" {
        let Some(raw_body) = body.as_deref() else {
            return json_response("400 Bad Request", r#"{"error":"body_required"}"#);
        };

        let parsed: serde_json::Value = match serde_json::from_slice(raw_body) {
            Ok(v) => v,
            Err(_) => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
        };

        let Some(message) = parsed
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return json_response("400 Bad Request", r#"{"error":"message_required"}"#);
        };

        let allowed_tools: Option<std::collections::HashSet<String>> = parsed
            .get("allowed_tools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            });

        let manifests = plugin_registry.manifests();
        let declared_rules: Vec<serde_json::Value> = manifests
            .iter()
            .flat_map(|m| {
                m.routing_rules.iter().map(|r| {
                    serde_json::json!({
                        "plugin_id": m.id,
                        "intent": r.intent,
                        "keywords": r.keywords,
                        "preferred_tools": r.preferred_tools,
                        "forbidden_tools": r.forbidden_tools,
                        "instruction": r.instruction,
                        "priority": r.priority,
                    })
                })
            })
            .collect();

        let flags = compute_message_intent_flags(message);
        let intents = active_intents_from_flags(&flags);
        let tools_executor_snapshot = if let Some(exec_lock) = _tools_executor {
            Some(exec_lock.read().await.clone())
        } else {
            None
        };
        let matched_rules = plugin_registry.match_routing_rules(message, &intents, |tool_name| {
            let policy_allowed = tools_executor_snapshot
                .as_ref()
                .map(|e| e.policy.can_use_tool(tool_name))
                .unwrap_or(true);
            let list_allowed = allowed_tools
                .as_ref()
                .map(|set| set.contains(tool_name))
                .unwrap_or(true);
            policy_allowed && list_allowed
        });

        let body = serde_json::json!({
            "message": message,
            "rules_count": declared_rules.len(),
            "rules": declared_rules,
            "active_intents": intents,
            "allowed_tools": allowed_tools,
            "matched_rules_count": matched_rules.len(),
            "matched_rules": matched_rules,
        });
        return json_response("200 OK", &body.to_string());
    }
    if method == "POST" && path == "/api/plugins/reload" {
        plugin_registry.reload();
        return json_response("200 OK", r#"{"reloaded":true}"#);
    }
    // POST /api/plugins/reputation/reset
    // Body optional:
    // - { "plugin_id": "maps" } to reset one plugin
    // - {} or empty body to reset all plugins
    if method == "POST" && path == "/api/plugins/reputation/reset" {
        let body_json = match parse_plugin_reputation_reset_body(body.as_deref()) {
            PluginReputationResetBody::Parsed(v) => Some(v),
            PluginReputationResetBody::Empty => None,
            PluginReputationResetBody::Invalid(detail) => {
                let body = serde_json::json!({
                    "error": "invalid_json",
                    "detail": detail
                });
                return json_response("400 Bad Request", &body.to_string());
            }
        };
        let plugin_id = body_json
            .as_ref()
            .and_then(|j| j.get("plugin_id"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);

        let result = if let Some(id) = plugin_id.as_deref() {
            plugin_registry.reset_reputation(id)
        } else {
            plugin_registry.reset_all_reputation()
        };

        match result {
            Ok(_) => {
                plugin_registry.reload();
                let body = serde_json::json!({
                    "ok": true,
                    "plugin_id": plugin_id,
                    "reloaded": true,
                });
                return json_response("200 OK", &body.to_string());
            }
            Err(e) => {
                let body = serde_json::json!({ "error": "reputation_reset_failed", "detail": e.to_string() });
                return json_response("500 Internal Server Error", &body.to_string());
            }
        }
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
                let body = serde_json::json!({ "error": "reload_failed", "detail": e.to_string() })
                    .to_string();
                return json_response("500 Internal Server Error", &body);
            }
        }
    }
    if method == "POST" && path == "/api/skills/uninstall" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
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
                let body =
                    serde_json::json!({ "error": "uninstall_failed", "detail": msg }).to_string();
                return json_response("400 Bad Request", &body);
            }
            None => {
                let body = serde_json::json!({ "error": "missing_name", "detail": "Body must be JSON with \"name\": \"<skill_name>\"" }).to_string();
                return json_response("400 Bad Request", &body);
            }
        }
    }

    // POST /api/skills/install — install a skill from a URL (GitHub or any allowed HTTPS host).
    // Body: { "url": "<skill_url>" }
    if method == "POST" && path == "/api/skills/install" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let url = body_json
            .as_ref()
            .and_then(|j| j.get("url"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        match url {
            Some(skill_url) => {
                let allowed_hosts = if let Some(exec_lock) = _tools_executor {
                    exec_lock.read().await.policy.skill_install_allowed_hosts()
                } else {
                    vec![
                        "github.com".into(),
                        "raw.githubusercontent.com".into(),
                        "www.github.com".into(),
                    ]
                };
                let policy_path = data_dir.join("tools_policy.yaml");
                // tools_reload: passed to do_install_skill so it can hot-reload tools_policy
                // after the new skill command entry is registered.
                let tools_reload = _tools_executor.map(|r| (r, policy_path.as_path()));
                let (ok, msg) = do_install_skill(
                    &skill_url,
                    data_dir,
                    spec_dir,
                    skill_registry,
                    &allowed_hosts,
                    tools_reload,
                )
                .await;
                if ok {
                    let body = serde_json::json!({ "installed": true, "message": msg }).to_string();
                    return json_response("200 OK", &body);
                }
                let body =
                    serde_json::json!({ "error": "install_failed", "detail": msg }).to_string();
                return json_response("400 Bad Request", &body);
            }
            None => {
                let body = serde_json::json!({ "error": "missing_url", "detail": "Body must be JSON with \"url\": \"<skill_url>\"" }).to_string();
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
        let body = serde_json::to_string(&serde_json::json!({ "tools": list }))
            .unwrap_or_else(|_| "{}".to_string());
        return json_response("200 OK", &body);
    }

    // User RAG: list documents
    if method == "GET" && path == "/api/user-rag/documents" {
        let store = user_rag_store.lock().await;
        match store.list_documents() {
            Ok(docs) => {
                let body = serde_json::to_string(&serde_json::json!({ "documents": docs }))
                    .unwrap_or_else(|_| "[]".to_string());
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
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let name = body_json
            .as_ref()
            .and_then(|j| j.get("name"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let content_base64 = body_json
            .as_ref()
            .and_then(|j| j.get("content_base64"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let mime_type = body_json
            .as_ref()
            .and_then(|j| j.get("mime_type"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let name = match name.filter(|n| !n.is_empty()) {
            Some(n) => n,
            None => return json_response("400 Bad Request", r#"{"error":"name_required"}"#),
        };
        let content_base64 = match content_base64.filter(|c| !c.is_empty()) {
            Some(c) => c,
            None => {
                return json_response("400 Bad Request", r#"{"error":"content_base64_required"}"#)
            }
        };
        let store = user_rag_store.lock().await;
        match store.add_document(&content_base64, &name, &mime_type) {
            Ok(id) => {
                let body =
                    serde_json::json!({ "id": id, "name": name, "message": "Document ajouté." });
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
        let id = path
            .trim_start_matches("/api/user-rag/documents/")
            .split('?')
            .next()
            .unwrap_or("")
            .trim();
        if id.is_empty() {
            return json_response("400 Bad Request", r#"{"error":"id_required"}"#);
        }
        let store = user_rag_store.lock().await;
        match store.delete_document(id) {
            Ok(true) => {
                return json_response("200 OK", r#"{"ok":true,"message":"Document supprimé."}"#)
            }
            Ok(false) => {
                return json_response("404 Not Found", r#"{"error":"document_not_found"}"#)
            }
            Err(e) => {
                let body = serde_json::json!({ "error": "delete_failed", "detail": e.to_string() });
                return json_response("500 Internal Server Error", &body.to_string());
            }
        }
    }

    // Phase 6: LLM completion via router
    if method == "POST" && path == "/api/complete" {
        let body = match body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        {
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
            max_tokens: body
                .get("max_tokens")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32),
            temperature: body
                .get("temperature")
                .and_then(|v| v.as_f64())
                .map(|f| f as f32),
            preferred_task_type: None,
            system_prompt: None,
            image_data_urls: None,
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            repeat_penalty: None,
            num_ctx: None,
            num_gpu: None,
            thinking_level: None,
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
        let period = path
            .split('?')
            .nth(1)
            .and_then(|q| q.split('&').find(|p| p.starts_with("period=")))
            .and_then(|p| p.strip_prefix("period="));
        let list: std::collections::HashMap<String, akasha_llm::ModelMetrics> =
            if let Some(period) = period {
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
                    (Some(from), Some(to)) => match akasha_store::MetricsStore::open(store_path) {
                        Ok(store) => store
                            .aggregate(Some(from), Some(to))
                            .ok()
                            .map(|rows| {
                                rows.into_iter()
                                    .map(|(k, v)| {
                                        (
                                            k,
                                            akasha_llm::ModelMetrics {
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
                                            },
                                        )
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        Err(_) => llm_router.metrics().list(),
                    },
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
        let category = body_json
            .as_ref()
            .and_then(|j| j.get("category"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let provider = body_json
            .as_ref()
            .and_then(|j| j.get("provider"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let model = body_json
            .as_ref()
            .and_then(|j| j.get("model"))
            .and_then(|v| v.as_str())
            .map(String::from);
        match (category, provider, model) {
            (Some(cat), Some(prov), Some(modl))
                if !cat.is_empty() && !prov.is_empty() && !modl.is_empty() =>
            {
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
                    let body_err =
                        serde_json::json!({ "ok": false, "error": format!("save failed: {}", e) });
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
                let body_err =
                    serde_json::json!({ "error": "missing or empty category, provider, or model" });
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
                                        .or_else(|| {
                                            m.get("name").and_then(|n| n.as_str()).map(String::from)
                                        })
                                        .or_else(|| {
                                            m.get("model")
                                                .and_then(|n| n.as_str())
                                                .map(String::from)
                                        })
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
            return json_response(
                "400 Bad Request",
                r#"{"error":"missing query: model=<name>"}"#,
            );
        }
        let url = format!("{}/api/show", base_url);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        match client
            .post(&url)
            .json(&serde_json::json!({ "model": model }))
            .send()
            .await
        {
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
                    let parameters = json
                        .get("parameters")
                        .and_then(|p| p.as_str())
                        .unwrap_or("");
                    let num_ctx = parameters
                        .lines()
                        .find(|l| l.trim().starts_with("num_ctx"))
                        .and_then(|l| {
                            l.trim()
                                .trim_start_matches("num_ctx")
                                .trim()
                                .split_whitespace()
                                .next()
                        })
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
                    .map(|a| {
                        a.iter()
                            .all(|c| c.get("ok").and_then(|v| v.as_bool()).unwrap_or(false))
                    })
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
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            repeat_penalty: None,
            num_ctx: None,
            num_gpu: None,
            thinking_level: None,
        };
        let advice_timeout = std::time::Duration::from_secs(120);
        match tokio::time::timeout(advice_timeout, llm_router.complete(&req)).await {
            Ok(Ok(resp)) => {
                let body =
                    serde_json::json!({ "advice": resp.text, "model_used": resp.model_used });
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

async fn cancel_task(store_path: &Path, id: Uuid, main_agent: &crate::agents::MainAgent) -> String {
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

/// Returns true when a task in `status` can be paused.
/// Only `Pending` and `Queued` tasks can be paused safely; `Running` tasks cannot be cooperatively
/// interrupted and must be allowed to complete or be cancelled instead.
pub(crate) fn is_pausable(status: &TaskStatus) -> bool {
    matches!(status, TaskStatus::Pending | TaskStatus::Queued)
}

/// Returns true when a task in `status` can be resumed.
pub(crate) fn is_resumable(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Paused | TaskStatus::Interrupted | TaskStatus::Failed
    )
}

async fn pause_task(store_path: &Path, id: Uuid, main_agent: &crate::agents::MainAgent) -> String {
    let store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let task = match store.get(id) {
        Ok(Some(t)) => t,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"task_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    if !is_pausable(&task.status) {
        let body = serde_json::json!({
            "error": "task_not_pausable",
            "detail": "La tâche ne peut pas être mise en pause dans son état actuel (déjà terminée, annulée, en pause ou en cours d'exécution).",
            "status": task.status.as_str()
        });
        return json_response("400 Bad Request", &body.to_string());
    }
    if store.update_status(id, TaskStatus::Paused).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    let _ = main_agent.bus().send(
        EventEnvelope::new(
            EventType::TaskPaused,
            Some(serde_json::json!({ "task_id": id.to_string() })),
        )
        .with_correlation(id),
    );
    let body = serde_json::json!({ "paused": true, "task_id": id.to_string() });
    json_response("200 OK", &body.to_string())
}

/// Progress text that must not be shown as the sole final reply in chat when a richer answer exists on subtasks.
fn task_progress_is_chat_stub(msg: &str) -> bool {
    let t = msg.trim();
    t.is_empty()
        || matches!(
            t,
            "Terminé."
                | "Done."
                | "Échec."
                | "Annulé."
                | "Failed."
                | "Cancelled."
                | "Sous-tâches en cours."
        )
        || t.starts_with("Task delegated to agent")
}

fn merged_last_progress_snapshot(
    store: &TaskStore,
    mem: &std::collections::HashMap<Uuid, VecDeque<ProgressEntry>>,
    task_id: Uuid,
) -> Option<String> {
    let disk: Option<String> = store
        .get_progress(task_id)
        .ok()
        .and_then(|v| v.last().map(|(_, m)| m.clone()));
    let in_mem: Option<String> = mem
        .get(&task_id)
        .and_then(|q| q.back())
        .map(|e| e.message.clone());
    [disk, in_mem]
        .into_iter()
        .flatten()
        .max_by_key(|s| s.len())
}

fn best_substantive_progress_in_subtree(
    store: &TaskStore,
    mem: &std::collections::HashMap<Uuid, VecDeque<ProgressEntry>>,
    task_id: Uuid,
) -> Option<String> {
    if let Some(line) = merged_last_progress_snapshot(store, mem, task_id) {
        let tr = line.trim();
        if !task_progress_is_chat_stub(tr) {
            return Some(tr.to_string());
        }
    }
    let Ok(children) = store.get_children(task_id) else {
        return None;
    };
    let mut best: Option<String> = None;
    for c in children {
        if let Some(m) = best_substantive_progress_in_subtree(store, mem, c.id) {
            if best.as_ref().map(|b| b.len()).unwrap_or(0) < m.len() {
                best = Some(m);
            }
        }
    }
    best
}

async fn resume_task(store_path: &Path, id: Uuid, main_agent: &crate::agents::MainAgent) -> String {
    let store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let task = match store.get(id) {
        Ok(Some(t)) => t,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"task_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    if !is_resumable(&task.status) {
        let body = serde_json::json!({
            "error": "task_not_resumable",
            "detail": "Seules les tâches en pause ou interrompues peuvent être reprises.",
            "status": task.status.as_str()
        });
        return json_response("400 Bad Request", &body.to_string());
    }
    match main_agent.resume_task(store_path, id) {
        Ok(()) => {
            let body = serde_json::json!({ "resumed": true, "task_id": id.to_string() });
            json_response("200 OK", &body.to_string())
        }
        Err(e) => {
            let body = serde_json::json!({ "error": "resume_failed", "detail": e.to_string() });
            json_response("500 Internal Server Error", &body.to_string())
        }
    }
}

async fn get_task_status(
    store_path: &Path,
    progress: &ProgressCache,
    task_usage_store: &TaskUsageStore,
    id: Uuid,
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
    let mut progress_list: Vec<ProgressEntry> = store
        .get_progress(id)
        .map(|v| {
            v.into_iter()
                .map(|(pct, msg)| ProgressEntry {
                    progress_pct: pct,
                    message: msg,
                })
                .collect()
        })
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
            // Take a snapshot of in-memory progress for all children and drop the lock
            // before doing any SQLite traversal to avoid holding the RwLock during DB I/O.
            let mem_snapshot: std::collections::HashMap<Uuid, VecDeque<ProgressEntry>> = {
                let g = progress.read().await;
                g.iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect()
            };
            let mut sum: u32 = 0;
            for child in &children {
                let child_pct = store
                    .get_progress(child.id)
                    .ok()
                    .and_then(|v| v.last().map(|(pct, _)| *pct as u32))
                    .or_else(|| {
                        mem_snapshot
                            .get(&child.id)
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
            // Chat UI polls root task_id: last progress line must reflect the real answer when the root row is only a generic completion stub while children hold the substantive reply.
            if progress_list
                .last()
                .map(|e| task_progress_is_chat_stub(&e.message))
                .unwrap_or(false)
            {
                let mut best: Option<String> = None;
                for c in &children {
                    if let Some(m) = best_substantive_progress_in_subtree(&store, &mem_snapshot, c.id) {
                        if best.as_ref().map(|b| b.len()).unwrap_or(0) < m.len() {
                            best = Some(m);
                        }
                    }
                }
                if let (Some(last_mut), Some(b)) = (progress_list.last_mut(), best) {
                    last_mut.message = b;
                }
            }
        }
    }
    let (tokens_used, cost_usd) = task_usage_store.get_task(id).await.unwrap_or((0, 0.0));
    let (todos, todos_updated_at) = store
        .get_todos_with_updated_at(id)
        .unwrap_or_else(|_| (Vec::new(), None));
    let todos_json: Vec<serde_json::Value> = todos
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "title": t.title,
                "status": t.status.as_str()
            })
        })
        .collect();
    let mut body = serde_json::json!({
        "task_id": task.id.to_string(),
        "status": task.status.as_str(),
        "assigned_agent": task.assigned_agent,
        "created_at": task.created_at.to_rfc3339(),
        "updated_at": task.updated_at.to_rfc3339(),
        "progress": progress_list,
        "tokens_used": tokens_used,
        "cost_usd": cost_usd,
        "todos": todos_json
    });
    if let Some(u) = todos_updated_at {
        body["todos_updated_at"] = serde_json::Value::String(u);
    }
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

async fn get_schedule_exceptions(store_path: &Path, schedule_id: Uuid) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let list = match store.get_exceptions_for_schedule(schedule_id) {
        Ok(l) => l,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let arr: Vec<serde_json::Value> = list
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id.to_string(),
                "schedule_id": e.schedule_id.to_string(),
                "type": e.type_.as_str(),
                "date": e.date.format("%Y-%m-%d").to_string(),
                "override_payload": e.override_payload
            })
        })
        .collect();
    let body = serde_json::json!({ "exceptions": arr });
    json_response("200 OK", &body.to_string())
}

async fn post_schedule_exception(
    store_path: &Path,
    schedule_id: Uuid,
    body: Option<Vec<u8>>,
) -> String {
    let json: serde_json::Value = match body.as_deref().and_then(|b| serde_json::from_slice(b).ok())
    {
        Some(j) => j,
        None => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
    };
    let date_str = json.get("date").and_then(|v| v.as_str()).unwrap_or("");
    let date = match chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => {
            return json_response(
                "400 Bad Request",
                r#"{"error":"invalid_date","expected":"YYYY-MM-DD"}"#,
            )
        }
    };
    let type_str = json.get("type").and_then(|v| v.as_str()).unwrap_or("skip");
    let type_ = match type_str {
        "override" => ScheduleExceptionType::Override,
        _ => ScheduleExceptionType::Skip,
    };
    let override_payload = json
        .get("override_payload")
        .and_then(|v| v.as_str())
        .map(String::from);
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let e = ScheduleException {
        id: Uuid::new_v4(),
        schedule_id,
        type_,
        date,
        override_payload,
    };
    if store.insert_exception(&e).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    let body = serde_json::json!({
        "id": e.id.to_string(),
        "schedule_id": e.schedule_id.to_string(),
        "type": e.type_.as_str(),
        "date": e.date.format("%Y-%m-%d").to_string()
    });
    json_response("200 OK", &body.to_string())
}

async fn delete_schedule_exception(
    store_path: &Path,
    schedule_id: Uuid,
    exception_id: Uuid,
) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    match store.delete_exception(schedule_id, exception_id) {
        Ok(true) => {
            let body =
                serde_json::json!({ "deleted": true, "exception_id": exception_id.to_string() });
            json_response("200 OK", &body.to_string())
        }
        Ok(false) => json_response("404 Not Found", r#"{"error":"exception_not_found"}"#),
        Err(_) => json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    }
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
    let json: serde_json::Value = match body.as_deref().and_then(|b| serde_json::from_slice(b).ok())
    {
        Some(j) => j,
        None => return json_response("400 Bad Request", r#"{"error":"invalid_json"}"#),
    };
    let now = chrono::Utc::now();
    let id = Uuid::new_v4();
    let schedule = Schedule {
        id,
        name: json
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        description: json
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        enabled: json
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        timezone: json
            .get("timezone")
            .and_then(|v| v.as_str())
            .unwrap_or("UTC")
            .to_string(),
        rrule: json
            .get("rrule")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
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
        channel_context: json
            .get("channel_context")
            .and_then(|v| v.as_str())
            .map(String::from),
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
    let json: serde_json::Value = match body.as_deref().and_then(|b| serde_json::from_slice(b).ok())
    {
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
            return json_response(
                "400 Bad Request",
                r#"{"error":"invalid_field_type","field":"channel_context"}"#,
            );
        }
    }
    if store.update_schedule(&s).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    json_response(
        "200 OK",
        &serde_json::json!({ "id": id.to_string() }).to_string(),
    )
}

async fn delete_schedule(store_path: &Path, id: Uuid) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    if store.delete_schedule(id).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    json_response(
        "200 OK",
        &serde_json::json!({ "deleted": id.to_string() }).to_string(),
    )
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
            let suffix = if s.len() >= 8 {
                &s[s.len() - 8..]
            } else {
                s.as_str()
            };
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
async fn get_schedule_run_reports(store_path: &Path) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let task_store = match TaskStore::open(store_path) {
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
    let reports: Vec<serde_json::Value> = completed
        .into_iter()
        .filter_map(|r| {
            let schedule_id = r.schedule_id?;
            let schedule_name = schedule_names.get(&schedule_id)?.clone();
            let message = task_store
                .get_progress(r.task_id)
                .ok()
                .and_then(|entries| entries.last().map(|(_, msg)| msg.clone()))
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

fn packaged_spec_check_ok(spec_dir: &Path) -> bool {
    spec_dir.exists() || std::env::var_os("AKASHA_SPEC_DIR").is_none()
}

#[cfg(test)]
mod tests {
    use super::{
        agent_role_system_prompt, build_image_markdown, build_session_recap_reply,
        canonicalize_tool_name, classify_small_talk_message, detect_session_recall_intent,
        ensure_no_open_code_block, extract_how_to_call_from_message, is_pausable, is_resumable,
        looks_like_meta_agent_response, memory_profile_for_task, message_suggests_tool_only_action,
        normalize_tool_path_hint, packaged_spec_check_ok, parse_content_length,
        parse_device_invoke_params, parse_generate_image_tool_args,
        parse_memory_store_explicit_links, parse_plugin_reputation_reset_body, parse_run_command_args,
        parse_skill_install_url, parse_tool_calls, parse_write_file_request,
        resolve_run_command_working_dir,
        response_looks_off_topic_for_small_talk, rewrite_workspace_plan_key_to_lineage_root,
        rewrite_workspace_plan_path_str, small_talk_fast_lane, PluginReputationResetBody,
        SessionRecallIntent, SessionRecallRange, SmallTalkLanguage,
    };
    use akasha_store::TaskStatus;
    use uuid::Uuid;

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

    fn s(v: &str) -> String {
        v.to_string()
    }

    #[test]
    fn device_invoke_params_no_extra_args_returns_empty_object() {
        let args: Vec<String> = vec![s("local_media"), s("camera"), s("capture")];
        let p = parse_device_invoke_params(&args);
        assert_eq!(p, serde_json::json!({}));
    }

    #[test]
    fn device_invoke_params_single_valid_json_arg() {
        let args = vec![
            s("synthetic_input"),
            s("keyboard"),
            s("shortcut"),
            s(r#"{"keys":["Control","C"]}"#),
        ];
        let p = parse_device_invoke_params(&args);
        assert_eq!(p, serde_json::json!({"keys": ["Control", "C"]}));
    }

    #[test]
    fn device_invoke_params_single_invalid_json_falls_back_to_empty_object() {
        let args = vec![
            s("local_media"),
            s("microphone"),
            s("record"),
            s("not-json"),
        ];
        let p = parse_device_invoke_params(&args);
        assert_eq!(p, serde_json::json!({}));
    }

    #[test]
    fn device_invoke_params_multiple_args_become_json_array() {
        let args = vec![
            s("synthetic_input"),
            s("keyboard"),
            s("type"),
            s("hello"),
            s("world"),
        ];
        let p = parse_device_invoke_params(&args);
        assert_eq!(p, serde_json::json!(["hello", "world"]));
    }

    // --- parse_generate_image_tool_args (whitespace-split TOOL: lines) ---

    #[test]
    fn generate_image_args_join_words_into_prompt() {
        let args = vec![s("A"), s("cute"), s("cat"), s("playing"), s("guitar")];
        let (prompt, size) = parse_generate_image_tool_args(&args);
        assert_eq!(prompt, "A cute cat playing guitar");
        assert!(size.is_none());
    }

    #[test]
    fn generate_image_args_trailing_size_token() {
        let args = vec![s("cat"), s("on"), s("sofa"), s("1024x1024")];
        let (prompt, size) = parse_generate_image_tool_args(&args);
        assert_eq!(prompt, "cat on sofa");
        assert_eq!(size.as_deref(), Some("1024x1024"));
    }

    // --- message_suggests_tool_only_action ---

    #[test]
    fn tool_only_action_true_for_camera() {
        assert!(message_suggests_tool_only_action(
            "Prends une photo avec la caméra"
        ));
        assert!(message_suggests_tool_only_action(
            "Take a photo from the webcam"
        ));
    }

    #[test]
    fn tool_only_action_true_for_weather() {
        assert!(message_suggests_tool_only_action(
            "Quelle est la météo à Paris ?"
        ));
    }

    #[test]
    fn tool_only_action_true_for_save_file() {
        assert!(message_suggests_tool_only_action(
            "Sauvegarde ce code dans /tmp/foo.py"
        ));
    }

    #[test]
    fn tool_only_action_true_for_image_generation() {
        assert!(message_suggests_tool_only_action(
            "Génère une image d'un lapin"
        ));
    }

    #[test]
    fn tool_only_action_false_for_code_generation() {
        assert!(!message_suggests_tool_only_action(
            "Écris un script Python qui lit un fichier"
        ));
        assert!(!message_suggests_tool_only_action(
            "Génère du code pour trier une liste"
        ));
    }

    #[test]
    fn tool_only_action_false_when_code_intent_dominates() {
        // Explicit code request even if it mentions photo → do not override to conversation
        assert!(!message_suggests_tool_only_action(
            "écris un script qui prend une photo"
        ));
    }

    #[test]
    fn memory_profile_uses_fast_path_for_simple_root_requests() {
        let profile = memory_profile_for_task("Bonjour, ça va ?", "conversation", false, false);
        assert_eq!(profile.semantic_top_k, 2);
        assert_eq!(profile.user_rag_top_k, 0);
        assert_eq!(profile.workspace_graph_top_k, 0);
        assert!(!profile.expand_by_graph);
        assert!(!profile.compact_before_prompt);
    }

    #[test]
    fn memory_profile_is_lean_for_subagents() {
        let profile = memory_profile_for_task(
            "Implémente la route demandée dans le plan partagé",
            "backend",
            true,
            true,
        );
        assert_eq!(profile.semantic_top_k, 0);
        assert_eq!(profile.episodic_limit, 0);
        assert_eq!(profile.user_rag_top_k, 0);
        assert_eq!(profile.workspace_graph_top_k, 0);
        assert!(!profile.compact_before_prompt);
    }

    #[test]
    fn small_talk_fast_lane_detects_simple_greeting() {
        let intent = small_talk_fast_lane("Salut, ça va ?").expect("small-talk should be detected");
        assert_eq!(intent.language, SmallTalkLanguage::French);
        assert!(intent.asks_status);
    }

    #[test]
    fn small_talk_fast_lane_rejects_real_request_after_greeting() {
        assert!(small_talk_fast_lane("Bonjour, peux-tu lire ce fichier ?").is_none());
    }

    #[test]
    fn extract_how_to_call_ignores_greetings() {
        assert!(extract_how_to_call_from_message("salut").is_none());
        assert!(extract_how_to_call_from_message("bonjour").is_none());
    }

    #[test]
    fn classify_small_talk_rejects_small_talk_with_real_request() {
        // "ça va merci, tu peux me rappeler ce qu'on a fait hier ?"
        // Should be rejected because it contains "peux me" + "rappeler" + "hier"
        assert!(classify_small_talk_message(
            "ça va merci, tu peux me rappeler ce qu'on a fait hier ?"
        )
        .is_none());

        // "ça va, peux-tu lire ce fichier ?" should be rejected
        assert!(classify_small_talk_message("ça va, peux-tu lire ce fichier ?").is_none());

        // But "salut, ça va ?" should still pass
        assert!(classify_small_talk_message("salut, ça va ?").is_some());
    }

    #[test]
    fn small_talk_guardrail_flags_tool_leaks() {
        assert!(response_looks_off_topic_for_small_talk(
            "TOOL: write_file c:/tmp/x.txt\nJe vais d'abord modifier tools_policy.yaml"
        ));
        assert!(!response_looks_off_topic_for_small_talk("Salut ! 👋"));
    }

    #[test]
    fn detect_session_recall_intent_for_yesterday() {
        let intent = detect_session_recall_intent("tu peux me rappeler ce qu'on a fait hier ?")
            .expect("intent should be detected");
        assert_eq!(intent.range, SessionRecallRange::Yesterday);
        assert_eq!(intent.language, SmallTalkLanguage::French);
    }

    #[test]
    fn build_session_recap_reply_filters_noise() {
        let turns = vec![
            crate::memory::ConversationTurn {
                role: "assistant".to_string(),
                content: "tools_policy.yaml blocked write_file".to_string(),
            },
            crate::memory::ConversationTurn {
                role: "assistant".to_string(),
                content: "On a mis en place le fast-path small-talk et ajouté des garde-fous de pertinence.".to_string(),
            },
        ];
        let intent = SessionRecallIntent {
            range: SessionRecallRange::Yesterday,
            language: SmallTalkLanguage::French,
        };
        let recap = build_session_recap_reply(&turns, intent).expect("recap should exist");
        assert!(recap.contains("fast-path small-talk"));
        assert!(!recap.to_lowercase().contains("tools_policy.yaml"));
    }

    // --- parse_tool_calls (normalized markdown / list TOOL lines) ---

    #[test]
    fn parse_tool_calls_markdown_list_and_title_case_tool() {
        let s = "- Tool: write_file workspace:/out.md hello world";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].0, "write_file");
        assert!(c[0].1[0].contains("workspace:"), "{:?}", c[0].1);
    }

    #[test]
    fn parse_tool_calls_bold_wrapped_tool_keyword() {
        let s = "**TOOL:** write_file workspace:/x.md\nhello block";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].0, "write_file");
        assert_eq!(c[0].1[0], "workspace:/x.md");
        assert_eq!(c[0].1[1], "hello block");
    }

    #[test]
    fn parse_tool_calls_table_cell_with_leading_pipe() {
        let s = "| TOOL: read_file workspace:/plan.md |";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].0, "read_file");
    }

    #[test]
    fn parse_tool_calls_rejects_tools_plain_word() {
        let s = "tools: hammer and nail";
        let c = parse_tool_calls(s);
        assert!(c.is_empty());
    }

    #[test]
    fn parse_tool_calls_rejects_nested_tool_keyword_as_name() {
        let s = "TOOL: TOOL: install_skill https://github.com/bankr/cli";
        let c = parse_tool_calls(s);
        assert!(
            c.is_empty(),
            "nested TOOL: should not become a tool named TOOL:"
        );
    }

    #[test]
    fn parse_tool_calls_rejects_non_ascii_tool_name() {
        let s = "TOOL: prévision météo Paris";
        let c = parse_tool_calls(s);
        assert!(c.is_empty(), "non-ASCII pseudo tool names must be ignored");
    }

    #[test]
    fn normalize_tool_path_hint_accepts_workspace_slash_form() {
        assert_eq!(
            normalize_tool_path_hint("workspace/analyze/comparatif.md"),
            "workspace:/analyze/comparatif.md"
        );
        assert_eq!(
            normalize_tool_path_hint("workspace\\analyze\\comparatif.md"),
            "workspace:/analyze\\comparatif.md"
        );
    }

    #[test]
    fn parse_write_file_request_accepts_json_payload() {
        let args = vec![
            "{".to_string(),
            "\"path\": \"workspace/project_plan.md\",".to_string(),
            "\"content\": \"# Plan\\n- item\"".to_string(),
            "}".to_string(),
        ];
        let (path, content) = parse_write_file_request(&args).expect("json payload should parse");
        assert_eq!(path, "workspace:/project_plan.md");
        assert!(content.contains("# Plan"));
    }

    #[test]
    fn parse_memory_store_explicit_links_uuid_plus_kind() {
        let u = "550e8400-e29b-41d4-a716-446655440000";
        let args = vec![format!("link_to:{u}+excludes")];
        let p = parse_memory_store_explicit_links(&args).expect("links");
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].0, u);
        assert_eq!(p[0].1, "excludes");
    }

    #[test]
    fn parse_memory_store_explicit_links_plain_uuids_and_global_kind() {
        let a = "550e8400-e29b-41d4-a716-446655440001";
        let b = "550e8400-e29b-41d4-a716-446655440002";
        let args = vec![
            format!("link_to:{a},{b}"),
            "link_kind:relates_to".to_string(),
        ];
        let p = parse_memory_store_explicit_links(&args).expect("links");
        assert_eq!(p, vec![
            (a.to_string(), "relates_to".to_string()),
            (b.to_string(), "relates_to".to_string()),
        ]);
    }

    #[test]
    fn parse_plugin_reputation_reset_body_rejects_invalid_json() {
        let parsed = parse_plugin_reputation_reset_body(Some(br#"{"plugin_id":"maps""#));
        assert!(matches!(parsed, PluginReputationResetBody::Invalid(_)));
    }

    #[test]
    fn parse_plugin_reputation_reset_body_accepts_empty_body_as_reset_all() {
        let parsed = parse_plugin_reputation_reset_body(None);
        assert!(matches!(parsed, PluginReputationResetBody::Empty));
    }

    #[test]
    fn canonicalize_tool_name_supports_create_todos_alias() {
        assert_eq!(canonicalize_tool_name("create_todos"), "write_todos");
        assert_eq!(canonicalize_tool_name("append_todos"), "merge_todos");
    }

    #[test]
    fn looks_like_meta_agent_response_flags_instruction_recitation() {
        assert!(looks_like_meta_agent_response(
            "Based on the instructions provided, I am ready to assist with this Phase 2 task."
        ));
        assert!(!looks_like_meta_agent_response(
            "Voici le rapport demandé et le fichier a été écrit dans workspace:/analyze/comparatif.md."
        ));
    }

    // Headings with textual content before `TOOL:` are not treated as tool calls.
    // Only markdown "junk" (e.g. `#` heading markers, list bullets) may precede `TOOL:`.
    #[test]
    fn parse_tool_calls_heading_then_tool_on_same_line() {
        let s = "### Step 4 — TOOL: write_file workspace:/out.md hello";
        let c = parse_tool_calls(s);
        assert!(
            c.is_empty(),
            "alphabetic heading text before `TOOL:` must not be parsed as a tool call"
        );
    }

    #[test]
    fn parse_tool_calls_rejects_tool_in_long_prose_line() {
        let s = "According to the documentation and the specification we note that TOOL: write_file workspace:/x y";
        let c = parse_tool_calls(s);
        assert!(
            c.is_empty(),
            "long alphabetic prefix before TOOL: must not become a false tool call"
        );
    }

    // After the fix, sloppy "- Tool: …" headers DO terminate multiline bodies.
    // A line like "- Tool: read_file …" inside a body is treated as the start of a new tool call.
    #[test]
    fn parse_tool_calls_sloppy_header_terminates_multiline_body() {
        // write_file body is ended when "- Tool: read_file …" appears — new tool starts.
        let s = "TOOL: write_file workspace:/docs/tools.md\n# Available tools\n\n- Tool: read_file — reads a file\n- Tool: write_file — writes a file\n\nEnd of list";
        let c = parse_tool_calls(s);
        // The "- Tool: read_file" line terminates the write_file body and starts a new call.
        assert!(c.len() >= 2, "expected at least 2 tool calls, got {:?}", c);
        assert_eq!(c[0].0, "write_file");
        // The write_file body ends just before "- Tool: read_file …".
        let body = &c[0].1[1];
        assert!(
            !body.contains("End of list"),
            "write_file body should be truncated at the sloppy header, got: {:?}",
            body
        );
        // The second call should be read_file (started by the sloppy header).
        assert_eq!(c[1].0, "read_file");
    }

    #[test]
    fn parse_tool_calls_body_with_heading_tool_mention_not_truncated() {
        // edit_file body contains "### Step — TOOL: read_file …" — must NOT split here.
        let s = "- Tool: edit_file workspace:/spec.md\n### Step — TOOL: read_file some args\nmore content\nTOOL: read_file workspace:/next.md";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 2, "expected edit_file + read_file, got {:?}", c);
        assert_eq!(c[0].0, "edit_file");
        let body = &c[0].1[1];
        assert!(
            body.contains("### Step — TOOL: read_file"),
            "body should contain the heading tool mention verbatim, got: {:?}",
            body
        );
        assert_eq!(c[1].0, "read_file");
    }

    #[test]
    fn parse_tool_calls_body_terminated_by_canonical_tool_line() {
        // After a body-supporting tool, a canonical TOOL: line correctly ends the body.
        let s = "TOOL: write_file workspace:/a.txt\nsome content\nTOOL: read_file workspace:/b.txt";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].0, "write_file");
        assert_eq!(c[1].0, "read_file");
    }

    #[test]
    fn parse_tool_calls_sloppy_provider_all_headers_after_multiline_tool() {
        // Providers that always emit sloppy "- Tool: …" headers must not lose tool calls
        // that come after a multiline-body tool (write_file / edit_file / apply_patch).
        let s = concat!(
            "- Tool: write_file workspace:/out.txt\n",
            "hello content\n",
            "- Tool: read_file workspace:/in.txt\n",
            "- Tool: list_dir workspace:/\n",
        );
        let c = parse_tool_calls(s);
        assert_eq!(
            c.len(),
            3,
            "expected write_file + read_file + list_dir, got {:?}",
            c
        );
        assert_eq!(c[0].0, "write_file");
        assert_eq!(c[1].0, "read_file");
        assert_eq!(c[2].0, "list_dir");
    }

    #[test]
    fn parse_tool_calls_bold_sloppy_provider_after_multiline_tool() {
        // "**TOOL:** …" style also terminates a multiline body.
        let s = concat!(
            "**TOOL:** write_file workspace:/out.txt\n",
            "some generated content\n",
            "**TOOL:** read_file workspace:/plan.md\n",
        );
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 2, "expected write_file + read_file, got {:?}", c);
        assert_eq!(c[0].0, "write_file");
        assert_eq!(c[1].0, "read_file");
    }

    // --- merged_for_tools selection logic ---
    // These tests mirror the merge branch `a_has_tools && !r_has_tools`
    // introduced to fix the sloppy-prefix streaming case.

    /// Helper that reproduces the merge logic from `run_message_via_llm`.
    fn merged_for_tools(response_text: &str, accumulated: &str) -> String {
        let r = response_text.trim();
        let a = accumulated.trim();
        let a_has_tools = !a.is_empty() && !parse_tool_calls(a).is_empty();
        let r_has_tools = !r.is_empty() && !parse_tool_calls(r).is_empty();
        if r.is_empty() && !a.is_empty() {
            a.to_string()
        } else if a_has_tools && !r_has_tools {
            a.to_string()
        } else if !r.is_empty() {
            r.to_string()
        } else {
            a.to_string()
        }
    }

    #[test]
    fn merge_prefers_accumulated_when_streamed_has_title_case_tool_and_resp_does_not() {
        // Provider streams `- Tool: write_file …` but the final body omits tool lines.
        let accumulated = "- Tool: write_file workspace:/out.md hello";
        let resp_text = "I will write the file for you.";
        let merged = merged_for_tools(resp_text, accumulated);
        assert_eq!(merged, accumulated);
        // The merged string must still parse as a tool call.
        let calls = parse_tool_calls(&merged);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "write_file");
    }

    #[test]
    fn merge_prefers_accumulated_when_streamed_has_bold_tool_and_resp_does_not() {
        // Provider streams `**TOOL:** write_file …` but the final body is plain text.
        let accumulated = "**TOOL:** write_file workspace:/x.md\nhello block";
        let resp_text = "Here is your file.";
        let merged = merged_for_tools(resp_text, accumulated);
        assert_eq!(merged, accumulated);
        let calls = parse_tool_calls(&merged);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "write_file");
    }

    #[test]
    fn merge_uses_resp_text_when_both_have_tool_calls() {
        // When both accumulated and resp_text have parseable tool calls, prefer resp_text (the
        // final body is authoritative as long as it contains tools).
        let accumulated = "TOOL: write_file workspace:/a.txt\nfoo";
        let resp_text = "TOOL: write_file workspace:/b.txt\nbar";
        let merged = merged_for_tools(resp_text, accumulated);
        assert_eq!(merged, resp_text);
    }

    #[test]
    fn merge_uses_resp_text_when_accumulated_has_no_tool_calls() {
        // If the streamed chunk contains no parseable tools at all, use resp_text.
        let accumulated = "Thinking about what to do…";
        let resp_text = "TOOL: read_file workspace:/plan.md";
        let merged = merged_for_tools(resp_text, accumulated);
        assert_eq!(merged, resp_text);
    }

    // --- rewrite_workspace_plan_key_to_lineage_root ---

    #[test]
    fn rewrite_plan_key_stale_uuid_rewritten_to_lineage_root() {
        let root = Uuid::parse_str("a80fec14-d91a-4d91-a09e-77ea286c21fd").unwrap();
        let stale = Uuid::parse_str("216364b2-9308-47a7-9c2b-1c3b84c55c8c").unwrap();
        let (k, bad) =
            rewrite_workspace_plan_key_to_lineage_root(&format!(".akasha/plan_{stale}.md"), root);
        assert_eq!(bad, Some(stale));
        assert_eq!(k, format!(".akasha/plan_{root}.md"));
    }

    #[test]
    fn rewrite_plan_key_matching_root_unchanged() {
        let root = Uuid::parse_str("a80fec14-d91a-4d91-a09e-77ea286c21fd").unwrap();
        let (k, bad) =
            rewrite_workspace_plan_key_to_lineage_root(&format!(".akasha/plan_{root}.md"), root);
        assert!(bad.is_none());
        assert_eq!(k, format!(".akasha/plan_{root}.md"));
    }

    #[test]
    fn rewrite_plan_key_non_plan_paths_passthrough() {
        let root = Uuid::nil();
        let (k, bad) =
            rewrite_workspace_plan_key_to_lineage_root("certification_ai/exo/x.md", root);
        assert!(bad.is_none());
        assert_eq!(k, "certification_ai/exo/x.md");
    }

    // --- rewrite_workspace_plan_path_str ---

    #[test]
    fn rewrite_workspace_plan_path_str_stale_uuid_rewritten() {
        let root = Uuid::parse_str("a80fec14-d91a-4d91-a09e-77ea286c21fd").unwrap();
        let stale = Uuid::parse_str("216364b2-9308-47a7-9c2b-1c3b84c55c8c").unwrap();
        // rewrite_workspace_plan_path_str calls workspace_lineage_root_task_id which needs a real
        // store, so we test the path-string wrapper via a nil store_path (lineage = task_id = root).
        let result = rewrite_workspace_plan_path_str(
            &format!("workspace:/.akasha/plan_{stale}.md"),
            root,
            None,
        );
        assert_eq!(result, format!("workspace:/.akasha/plan_{root}.md"));
    }

    #[test]
    fn rewrite_workspace_plan_path_str_non_workspace_unchanged() {
        let root = Uuid::parse_str("a80fec14-d91a-4d91-a09e-77ea286c21fd").unwrap();
        let stale = Uuid::parse_str("216364b2-9308-47a7-9c2b-1c3b84c55c8c").unwrap();
        let input = format!("/abs/path/.akasha/plan_{stale}.md");
        let result = rewrite_workspace_plan_path_str(&input, root, None);
        assert_eq!(result, input);
    }

    #[test]
    fn rewrite_workspace_plan_path_str_non_plan_workspace_path_unchanged() {
        let root = Uuid::parse_str("a80fec14-d91a-4d91-a09e-77ea286c21fd").unwrap();
        let result =
            rewrite_workspace_plan_path_str("workspace:/certification_ai/exo/x.md", root, None);
        assert_eq!(result, "workspace:/certification_ai/exo/x.md");
    }

    // --- agent_role_system_prompt ---

    #[test]
    fn agent_role_prompt_some_for_recognized_types() {
        let with_role = [
            "code",
            "search",
            "financial",
            "documentalist",
            "project_manager",
            "technical_writer",
            "research",
            "security_audit",
            "creative",
            "analyst",
            "architect",
            "frontend",
            "backend",
            "database",
            "integration",
            "qa",
            "system",
            "image_generation",
        ];
        for t in &with_role {
            let s = agent_role_system_prompt(t);
            assert!(s.is_some(), "agent_type {:?} should have a role prompt", t);
            assert!(
                !s.unwrap().is_empty(),
                "agent_type {:?} role prompt must be non-empty",
                t
            );
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
        assert!(
            out.contains("](<data:image/"),
            "output should contain ](<data:image/: {:?}",
            out
        );
        assert!(out.ends_with(">)"), "output should end with >): {:?}", out);
        assert_eq!(out, "\n\n![Photo](<data:image/jpeg;base64,ABC>)");
    }

    // --- is_pausable / is_resumable state transitions ---

    #[test]
    fn pausable_only_pending_and_queued() {
        assert!(is_pausable(&TaskStatus::Pending));
        assert!(is_pausable(&TaskStatus::Queued));
        // Running tasks cannot be cooperatively paused
        assert!(!is_pausable(&TaskStatus::Running));
        assert!(!is_pausable(&TaskStatus::Paused));
        assert!(!is_pausable(&TaskStatus::Completed));
        assert!(!is_pausable(&TaskStatus::Failed));
        assert!(!is_pausable(&TaskStatus::Cancelled));
        assert!(!is_pausable(&TaskStatus::Interrupted));
        assert!(!is_pausable(&TaskStatus::WaitingUserInput));
    }

    #[test]
    fn resumable_only_paused_and_interrupted() {
        assert!(is_resumable(&TaskStatus::Paused));
        assert!(is_resumable(&TaskStatus::Interrupted));
        // All other statuses are not resumable
        assert!(!is_resumable(&TaskStatus::Pending));
        assert!(!is_resumable(&TaskStatus::Queued));
        assert!(!is_resumable(&TaskStatus::Running));
        assert!(!is_resumable(&TaskStatus::Completed));
        assert!(is_resumable(&TaskStatus::Failed));
        assert!(!is_resumable(&TaskStatus::Cancelled));
        assert!(!is_resumable(&TaskStatus::WaitingUserInput));
    }

    #[test]
    fn interrupted_status_is_resumable_but_not_pausable() {
        // Interrupted tasks (daemon-restart survivors) must be resumable, not pausable
        assert!(is_resumable(&TaskStatus::Interrupted));
        assert!(!is_pausable(&TaskStatus::Interrupted));
    }

    #[test]
    fn task_status_filter_strings_are_canonical() {
        // Verify that status as_str() values match the strings used in query-param filtering
        assert_eq!(TaskStatus::Pending.as_str(), "pending");
        assert_eq!(TaskStatus::Queued.as_str(), "queued");
        assert_eq!(TaskStatus::Running.as_str(), "running");
        assert_eq!(TaskStatus::Completed.as_str(), "completed");
        assert_eq!(TaskStatus::Failed.as_str(), "failed");
        assert_eq!(TaskStatus::Paused.as_str(), "paused");
        assert_eq!(TaskStatus::Cancelled.as_str(), "cancelled");
        assert_eq!(TaskStatus::Interrupted.as_str(), "interrupted");
        assert_eq!(TaskStatus::WaitingUserInput.as_str(), "waiting_user_input");
    }

    #[test]
    fn packaged_spec_check_is_ok_without_override_for_installed_daemon() {
        let dir = tempfile::tempdir().unwrap();
        std::env::remove_var("AKASHA_SPEC_DIR");

        assert!(packaged_spec_check_ok(dir.path()));
    }

    #[test]
    fn parse_skill_install_url_accepts_github_tree_url() {
        // Standard GitHub tree URL for a skill directory
        let allowed = vec!["github.com".into(), "raw.githubusercontent.com".into()];
        let r = parse_skill_install_url(
            "https://github.com/BankrBot/skills/tree/main/bankr",
            &allowed,
        );
        assert!(r.is_some(), "GitHub tree URL must be accepted");
        let parsed = r.unwrap();
        assert_eq!(parsed.skill_name, "bankr");
        assert!(
            parsed.raw_skill_url.contains("raw.githubusercontent.com"),
            "raw URL must point to raw.githubusercontent.com"
        );
    }

    #[test]
    fn parse_skill_install_url_rejects_disallowed_host() {
        // Host not in the allowed list must be rejected to prevent SSRF
        let allowed = vec!["github.com".into(), "raw.githubusercontent.com".into()];
        let r = parse_skill_install_url("https://evil.example.com/bad.md", &allowed);
        assert!(
            r.is_none(),
            "URL from a non-allowed host must be rejected by parse_skill_install_url"
        );
    }

    #[test]
    fn parse_skill_install_url_wildcard_allows_any_https() {
        // When allowed_hosts contains "*", any HTTPS URL is allowed
        let allowed = vec!["*".into()];
        let r = parse_skill_install_url("https://example.com/my-skill/SKILL.md", &allowed);
        assert!(
            r.is_some(),
            "Wildcard allowed_hosts must accept any HTTPS URL"
        );
    }

    #[test]
    fn parse_skill_install_url_rejects_http_scheme() {
        // Only HTTPS is allowed; plain HTTP must be rejected
        let allowed = vec!["*".into()];
        let r = parse_skill_install_url("http://github.com/user/skills/tree/main/bankr", &allowed);
        assert!(
            r.is_none(),
            "HTTP scheme must be rejected (only HTTPS is allowed)"
        );
    }

    // --- parse_run_command_args / resolve_run_command_working_dir ---

    #[test]
    fn parse_run_command_args_extracts_cwd_after_vault() {
        let args = vec![
            s("VAULT:foo=BAR"),
            s("--cwd"),
            s("workspace:/"),
            s("git"),
            s("status"),
        ];
        let (vault, cwd, cmd, a) = parse_run_command_args(&args);
        assert_eq!(vault.len(), 1);
        assert_eq!(cwd.as_deref(), Some("workspace:/"));
        assert_eq!(cmd, "git");
        assert_eq!(a, vec![s("status")]);
    }

    #[test]
    fn parse_run_command_args_no_cwd() {
        let args = vec![s("cargo"), s("build")];
        let (_, cwd, cmd, a) = parse_run_command_args(&args);
        assert!(cwd.is_none());
        assert_eq!(cmd, "cargo");
        assert_eq!(a, vec![s("build")]);
    }

    #[test]
    fn resolve_run_command_working_dir_default_off_returns_none() {
        let mut p = akasha_tools::ToolsPolicy::default();
        p.run_command_default_cwd_workspace = false;
        let tmp = tempfile::tempdir().unwrap();
        p.allowed_read_paths = vec![tmp.path().to_string_lossy().to_string()];
        let r = resolve_run_command_working_dir(None, Some(tmp.path()), &p).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn resolve_run_command_working_dir_workspace_when_policy_on() {
        let mut p = akasha_tools::ToolsPolicy::default();
        p.run_command_default_cwd_workspace = true;
        let tmp = tempfile::tempdir().unwrap();
        p.allowed_read_paths = vec![tmp.path().to_string_lossy().to_string()];
        let r = resolve_run_command_working_dir(None, Some(tmp.path()), &p).unwrap();
        let got = r.expect("expected workspace cwd");
        let exp = std::fs::canonicalize(tmp.path()).unwrap();
        let got_c = std::fs::canonicalize(&got).unwrap();
        assert_eq!(got_c, exp);
    }
}
