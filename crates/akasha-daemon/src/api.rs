//! Simple HTTP API: POST /api/message, GET /api/tasks/:id, GET / (health)

pub use crate::agent_profile::{
    get_or_load_agent_profile, new_agent_profile_cache, set_agent_profile_cache, AgentProfile,
    AgentProfileCache,
};
use crate::agents::{interpret_message, EventBus, OrchestratorTask, TaskPriority};
use crate::latency::{
    clear_task_milestones, emit_timeline_once_for_task, env_duration_ms, log_latency_metric,
    resolve_root_task_id,
};
use crate::memory::ShortTermStore;
use crate::memory_actor::LongTermMemoryClient;
use crate::protocol_adapter::unknown_external_message_count;
use crate::autonomous_mission_config::{AutonomousMissionConfig, MissionStatusYaml};
use crate::user_profile::UserProfile;
use akasha_core::{EventEnvelope, EventType};
use akasha_llm::CompletionRequest;
use akasha_plugin_api::{PluginKind, PluginManifest};
pub use akasha_store::tasks::MAX_PROGRESS_PER_TASK;
use akasha_store::{
    format_todos_plan_block, parse_todos_from_payload, Schedule,
    ScheduleException, ScheduleExceptionType, ScheduleStore, Task, TaskRunStatus, TaskStatus,
    TaskStore, TodoStatus, WorkspaceGraphStore,
};
use akasha_vault::Vault;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub use crate::api_http::{json_response, parse_content_length, parse_request};
pub use crate::api_path_utils::{
    normalize_apostrophes, parse_read_file_args, strip_verbatim_prefix, READ_FILE_DEFAULT_MAX_LINES,
    READ_FILE_FULL_OUTPUT_MAX_BYTES, READ_FILE_PARTIAL_DEFAULT_MARKER,
};

/// Séparateur recommandé entre l’ancien et le nouveau texte (`TOOL:` est découpé sur les espaces, d’où un token `|` seul).
const SEARCH_REPLACE_DELIM: &str = " | ";

const BUDGET_SETTINGS_FILE: &str = "budget_settings.json";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct BudgetSettings {
    daily_token_limit: u64,
    warn_ratio: f64,
    auto_concise: bool,
}

impl Default for BudgetSettings {
    fn default() -> Self {
        Self {
            daily_token_limit: 1_000_000,
            warn_ratio: 0.7,
            auto_concise: true,
        }
    }
}

const SECOND_BRAIN_SETTINGS_FILE: &str = "memory_second_brain.json";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct SecondBrainSettings {
    enabled: bool,
    paused: bool,
}

impl Default for SecondBrainSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            paused: false,
        }
    }
}

fn load_second_brain_settings(data_dir: &Path) -> SecondBrainSettings {
    let path = data_dir.join(SECOND_BRAIN_SETTINGS_FILE);
    let raw = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return SecondBrainSettings::default(),
    };
    serde_json::from_str::<SecondBrainSettings>(&raw).unwrap_or_default()
}

fn save_second_brain_settings(data_dir: &Path, settings: &SecondBrainSettings) -> anyhow::Result<()> {
    let path = data_dir.join(SECOND_BRAIN_SETTINGS_FILE);
    std::fs::write(path, serde_json::to_string_pretty(settings)?)?;
    Ok(())
}

fn load_budget_settings(data_dir: &Path) -> BudgetSettings {
    let path = data_dir.join(BUDGET_SETTINGS_FILE);
    let raw = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return BudgetSettings::default(),
    };
    serde_json::from_str::<BudgetSettings>(&raw).unwrap_or_default()
}

fn save_budget_settings(data_dir: &Path, settings: &BudgetSettings) -> anyhow::Result<()> {
    let path = data_dir.join(BUDGET_SETTINGS_FILE);
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, serde_json::to_string_pretty(settings)?.as_bytes())?;
    std::fs::rename(&tmp_path, &path)?;
    Ok(())
}

fn tool_scope_key(tool: &str, tool_args: &[String]) -> String {
    match tool {
        "run_command" | "run_terminal" | "run_command_background" => tool_args
            .iter()
            .take(2)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string(),
        "write_file" => {
            // write_file may receive a JSON payload with a `path` key, or plain args[0]
            if let Some(path_from_json) = parse_write_file_request(tool_args)
                .map(|(p, _)| p)
                .filter(|p| !p.is_empty())
            {
                path_from_json
            } else {
                tool_args.get(0).cloned().unwrap_or_else(|| "global".to_string())
            }
        }
        "delete_file" => {
            // delete_file joins all args as a single path (spaces in filenames)
            let joined = tool_args.join(" ").trim().to_string();
            if joined.is_empty() { "global".to_string() } else { joined }
        }
        "edit_file" | "apply_patch" | "rename_path" | "move_tree" => {
            tool_args.get(0).cloned().unwrap_or_else(|| "global".to_string())
        }
        _ => "global".to_string(),
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct PermissionModeState {
    mode: String,
    updated_at: String,
}

impl Default for PermissionModeState {
    fn default() -> Self {
        Self {
            mode: "ask_me".to_string(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

fn load_permission_mode(data_dir: &Path) -> PermissionModeState {
    let path = data_dir.join("permissions_mode.json");
    let raw = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return PermissionModeState::default(),
    };
    serde_json::from_str::<PermissionModeState>(&raw).unwrap_or_default()
}

fn save_permission_mode(data_dir: &Path, state: &PermissionModeState) -> anyhow::Result<()> {
    let path = data_dir.join("permissions_mode.json");
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, serde_json::to_string_pretty(state)?.as_bytes())?;
    std::fs::rename(&tmp_path, &path)?;
    Ok(())
}

/// `args[0]` = chemin ; le reste = « ancien » puis ` | ` (recommandé) ou `|`, puis « nouveau ».
/// Retire les `|` initiaux issus du découpage (`path | old | new` → `old | new`).
fn parse_search_replace_payload(args: &[String]) -> Result<(String, String), &'static str> {
    let mut rest: String = args.get(1..).map(|a| a.join(" ")).unwrap_or_default();
    rest = rest.trim().to_string();
    while rest.starts_with('|') {
        rest = rest.trim_start_matches('|').trim_start().to_string();
    }
    if rest.is_empty() {
        return Err("usage");
    }
    let (search, replace) = if let Some((s, r)) = rest.split_once(SEARCH_REPLACE_DELIM) {
        (s.trim().to_string(), r.trim().to_string())
    } else if let Some((s, r)) = rest.split_once('|') {
        (s.trim().to_string(), r.trim().to_string())
    } else {
        return Err("usage");
    };
    if search.is_empty() {
        return Err("empty_search");
    }
    Ok((search, replace))
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

/// Code Studio prepends retrieved index chunks to the user message unless explicitly disabled.
/// `AKASHA_STUDIO_CODE_RAG_DISABLED=1|true|yes|on` turns that prefix off; unset or other values keep RAG on.
fn studio_code_rag_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("AKASHA_STUDIO_CODE_RAG_DISABLED")
            .ok()
            .map(|v| {
                let t = v.trim().to_ascii_lowercase();
                if t.is_empty() {
                    return true;
                }
                !matches!(t.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(true)
    })
}

fn schedule_code_studio_index_for_root(store_path: &Path, tool_disk_workspace_root: &Path) {
    let data_dir = store_path.parent().unwrap_or_else(|| store_path);
    let Some(project_id) =
        crate::studio::studio_project_id_from_disk_root(data_dir, tool_disk_workspace_root)
    else {
        return;
    };
    crate::api_studio::schedule_studio_code_rag_index(
        data_dir,
        &project_id,
        tool_disk_workspace_root,
        false,
    );
}

fn debug_log(hypothesis_id: &str, location: &str, message: &str, data: serde_json::Value) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let enabled = *ENABLED.get_or_init(|| {
        std::env::var_os("AKASHA_DEBUG_LOG")
            .map(|v| {
                let v = v.to_string_lossy().to_ascii_lowercase();
                matches!(v.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false)
    });
    if !enabled {
        return;
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let run_id =
        std::env::var("AKASHA_DEBUG_RUN_ID").unwrap_or_else(|_| "pre-fix".to_string());
    tracing::debug!(
        run_id = %run_id,
        hypothesis_id = hypothesis_id,
        location = location,
        message = message,
        timestamp = timestamp,
        data = %data,
        "api debug log"
    );
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
    let content_args = args.get(1..).unwrap_or(&[]);
    let content = match content_args {
        [] => String::new(),
        [only] => only.clone(),
        many => {
            // `TOOL:` headers are split on whitespace. Some models put file content on the
            // same line as the `write_file` header (`TOOL: write_file path { "x": ... }`)
            // and may continue it on following lines. The multiline parser always places the
            // collected body as the last element (including single-line bodies), so always
            // join the header prefix and the last element with a newline to preserve line
            // breaks between inline header content and any collected body.
            let (last, prefix) = many.split_last().expect("non-empty by match arm");
            let header = prefix.join(" ");
            if header.trim().is_empty() {
                last.clone()
            } else {
                format!("{}\n{}", header, last)
            }
        }
    };
    Some((path, content))
}

/// If the model wrapped the entire `write_file` body in a markdown fence, strip one layer (repeat up to 3×).
fn strip_markdown_fences_from_write_content(content: &str) -> String {
    let mut s = content.to_string();
    for _ in 0..3 {
        let lead = s.trim_start();
        if !lead.starts_with("```") {
            break;
        }
        s = if let Some(i) = s.find('\n') {
            s[i + 1..].to_string()
        } else {
            String::new()
        };
        let te = s.trim_end();
        if te.ends_with("```") {
            if let Some(i) = te.rfind('\n') {
                let last = te[i + 1..].trim();
                if last == "```" {
                    s = te[..i].to_string();
                    continue;
                }
            } else if te.trim() == "```" {
                s.clear();
                break;
            }
        }
        break;
    }
    s
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

pub(crate) fn resolve_tool_disk_path(raw: &str, workspace_root: Option<&Path>) -> PathBuf {
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
        let p = Path::new(raw);
        if p.is_absolute() {
            strip_verbatim_prefix(p.to_path_buf())
        } else {
            strip_verbatim_prefix(
                workspace_root
                    .map(|root| root.join(p))
                    .or_else(|| std::env::current_dir().ok().map(|cwd| cwd.join(p)))
                    .unwrap_or_else(|| p.to_path_buf()),
            )
        }
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

/// Virtual workspace per task (Deep Agents-style). Paths prefixed with "workspace:/" or "workspace:" are read/written here instead of disk.
pub type TaskWorkspaceStore =
    Arc<RwLock<std::collections::HashMap<Uuid, std::collections::HashMap<String, String>>>>;

pub fn new_task_workspace_store() -> TaskWorkspaceStore {
    Arc::new(RwLock::new(std::collections::HashMap::new()))
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
                        schema_version: 1,
                        kind: e.event_type.clone(),
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
            root_events.extend(q.iter().map(|e| {
                let mut e = e.clone();
                if e.task_id.is_none() {
                    e.task_id = Some(id.to_string());
                }
                e
            }));
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
                list.extend(q.iter().map(|e| {
                    let mut e = e.clone();
                    e.task_id = Some(child_id.to_string());
                    e
                }));
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
                        schema_version: 1,
                        kind: e.event_type.clone(),
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

    // Studio swarm MVP: synthesize worker lifecycle events from existing delegation/task events.
    // This keeps backward compatibility while exposing stable status nodes to Code Studio Cockpit.
    let mut synthetic: Vec<TaskEventEntry> = Vec::new();
    let mut spawned_workers: Vec<String> = Vec::new();
    let mut saw_failed = false;
    for entry in &list {
        if entry.event_type == "sub_agent_spawned" {
            let worker_task_id = entry
                .payload
                .as_ref()
                .and_then(|p| p.get("task_id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let assigned_agent = entry
                .payload
                .as_ref()
                .and_then(|p| p.get("agent"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            synthetic.push(TaskEventEntry {
                schema_version: 1,
                kind: "studio_worker_state_changed".to_string(),
                event_type: "studio_worker_state_changed".to_string(),
                payload: Some(serde_json::json!({
                    "state": "spawned",
                    "worker_task_id": worker_task_id.clone(),
                    "assigned_agent": assigned_agent.clone(),
                })),
                at: entry.at.clone(),
                task_id: entry.task_id.clone(),
            });
            synthetic.push(TaskEventEntry {
                schema_version: 1,
                kind: "studio_worker_state_changed".to_string(),
                event_type: "studio_worker_state_changed".to_string(),
                payload: Some(serde_json::json!({
                    "state": "running",
                    "worker_task_id": worker_task_id.clone(),
                    "assigned_agent": assigned_agent,
                })),
                at: entry.at.clone(),
                task_id: entry.task_id.clone(),
            });
            if let Some(w) = worker_task_id {
                spawned_workers.push(w);
            }
        } else if entry.event_type == "task_completed" {
            synthetic.push(TaskEventEntry {
                schema_version: 1,
                kind: "studio_worker_state_changed".to_string(),
                event_type: "studio_worker_state_changed".to_string(),
                payload: Some(serde_json::json!({
                    "state": "completed",
                    "worker_task_id": entry.task_id.clone(),
                })),
                at: entry.at.clone(),
                task_id: entry.task_id.clone(),
            });
        } else if entry.event_type == "task_failed" {
            saw_failed = true;
            synthetic.push(TaskEventEntry {
                schema_version: 1,
                kind: "studio_worker_state_changed".to_string(),
                event_type: "studio_worker_state_changed".to_string(),
                payload: Some(serde_json::json!({
                    "state": "failed",
                    "worker_task_id": entry.task_id.clone(),
                })),
                at: entry.at.clone(),
                task_id: entry.task_id.clone(),
            });
        }
    }
    if saw_failed && spawned_workers.len() > 1 {
        synthetic.push(TaskEventEntry {
            schema_version: 1,
            kind: "studio_conflict_notice".to_string(),
            event_type: "studio_conflict_notice".to_string(),
            payload: Some(serde_json::json!({
                "reason": "Concurrent workers ended in failure; review potential file touch conflicts.",
                "workers": spawned_workers,
            })),
            at: chrono::Utc::now().to_rfc3339(),
            task_id: Some(id.to_string()),
        });
    }
    list.extend(synthetic);

    list.sort_by(|a, b| a.at.cmp(&b.at));
    let body = serde_json::json!({ "task_id": id.to_string(), "events": list });
    json_response("200 OK", &body.to_string())
}

async fn get_task_report(store_path: &Path, events: &EventsCache, id: Uuid) -> String {
    let store = match TaskStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let task = match store.get(id) {
        Ok(Some(t)) => t,
        Ok(None) => return json_response("404 Not Found", r#"{"error":"task_not_found"}"#),
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let persisted_progress = store.get_progress(id).unwrap_or_default();
    let mut done: Vec<String> = persisted_progress
        .iter()
        .filter(|(pct, msg)| *pct >= 100 && !task_progress_is_chat_stub(msg))
        .map(|(_, msg)| msg.clone())
        .collect();
    done.truncate(5);

    let mut failed: Vec<String> = Vec::new();
    if matches!(task.status, TaskStatus::Failed | TaskStatus::Cancelled) {
        if let Some((_, msg)) = persisted_progress
            .iter()
            .rev()
            .find(|(_, msg)| !task_progress_is_chat_stub(msg))
        {
            failed.push(msg.chars().take(1200).collect());
        } else {
            failed.push("La tâche a échoué sans détail explicite.".to_string());
        }
    }

    let mut needs_review: Vec<String> = Vec::new();
    let mut saw_approval_request = store
        .get_events(id)
        .unwrap_or_default()
        .iter()
        .any(|e| e.event_type == "tool_approval_request");
    if !saw_approval_request {
        let g = events.read().await;
        saw_approval_request = g
            .get(&id)
            .map(|q| q.iter().any(|e| e.event_type == "tool_approval_request"))
            .unwrap_or(false);
    }
    if saw_approval_request {
        needs_review.push("Une ou plusieurs actions sensibles ont demandé validation.".to_string());
    }
    let pending_queue = store_path
        .parent()
        .map(crate::permissions_queue::load)
        .map(|q| {
            q.requests
                .into_iter()
                .filter(|r| r.task_id == id.to_string() && r.status == crate::permissions_queue::QueueStatus::Pending)
                .count()
        })
        .unwrap_or(0);
    if pending_queue > 0 {
        needs_review.push(format!("{pending_queue} demande(s) d'approbation en attente."));
    }

    let mut next_steps: Vec<String> = match task.status {
        TaskStatus::Completed => vec![
            "Relire le diff studio et exécuter une vérification locale.".to_string(),
            "Si résultat valide, poursuivre avec la prochaine sous-tâche planifiée.".to_string(),
        ],
        TaskStatus::WaitingUserInput => vec![
            "Répondre à la demande d'approbation ou d'information de l'agent.".to_string(),
        ],
        TaskStatus::Failed | TaskStatus::Cancelled => vec![
            "Analyser la cause d'échec puis relancer avec une consigne ciblée.".to_string(),
        ],
        _ => vec!["Attendre la fin de la tâche puis relire le rapport.".to_string()],
    };
    next_steps.truncate(5);

    let transcript_path = store_path
        .parent()
        .map(|d| d.join("transcripts").join(format!("{id}.json")));
    let transcript = transcript_path
        .as_ref()
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().to_string());

    let body = serde_json::json!({
        "task_id": id.to_string(),
        "status": task.status.as_str(),
        "done": done,
        "needs_review": needs_review,
        "failed": failed,
        "next_steps": next_steps,
        "transcript_path": transcript
    });
    json_response("200 OK", &body.to_string())
}

/// `GET /api/tasks/:id/studio-diff` — fichiers texte modifiés depuis le snapshot de début de tâche (Code Studio racine).
async fn get_task_studio_diff(store_path: &Path, task_id: Uuid) -> String {
    let Some(data_dir) = store_path.parent().map(Path::to_path_buf) else {
        return json_response("500 Internal Server Error", r#"{"error":"no_data_dir"}"#);
    };
    let snap_path = data_dir.join("studio-task-snapshots").join(format!("{task_id}.json"));
    if !snap_path.is_file() {
        return json_response(
            "404 Not Found",
            &serde_json::json!({
                "error": "no_snapshot",
                "task_id": task_id.to_string(),
                "hint": "snapshots are created for root Code Studio tasks only"
            })
            .to_string(),
        );
    }
    let snap_json = match std::fs::read_to_string(&snap_path) {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let snap: crate::studio_task_snapshot::StudioTaskSnapshot = match serde_json::from_str(&snap_json) {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            );
        }
    };
    let captured_at = snap.captured_at_rfc3339.clone();
    match tokio::task::spawn_blocking(move || crate::studio_task_snapshot::compute_studio_task_diff_from_snapshot(snap)).await {
        Ok(Ok(entries)) => {
            let body = serde_json::json!({
                "task_id": task_id.to_string(),
                "captured_at": captured_at,
                "files": entries,
            });
            json_response("200 OK", &body.to_string())
        }
        Ok(Err(e)) => json_response(
            "500 Internal Server Error",
            &serde_json::json!({ "error": e.to_string() }).to_string(),
        ),
        Err(e) => json_response(
            "500 Internal Server Error",
            &serde_json::json!({ "error": e.to_string() }).to_string(),
        ),
    }
}

pub const MAX_EVENTS_PER_TASK: usize = 64;

#[derive(Clone, serde::Serialize)]
pub struct ProgressEntry {
    pub progress_pct: u8,
    pub message: String,
    /// Tâche à laquelle cette ligne se rapporte (pour dédoublonnage côté UI).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

pub type ProgressCache = Arc<RwLock<std::collections::HashMap<Uuid, VecDeque<ProgressEntry>>>>;

#[derive(Clone, serde::Serialize)]
pub struct TaskEventEntry {
    /// Contract version for client event envelopes.
    pub schema_version: u8,
    /// Normalized event kind for clients (kept in sync with event_type).
    pub kind: String,
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
    by_task_last_turn: RwLock<std::collections::HashMap<Uuid, (u64, u64, f64)>>,
}

impl TaskUsageStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn add(
        &self,
        task_id: Uuid,
        session_id: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
        cost_usd: f64,
    ) {
        let tokens = prompt_tokens.saturating_add(completion_tokens);
        {
            let mut g = self.by_task.write().await;
            let e = g.entry(task_id).or_insert((0, 0.0));
            e.0 += tokens;
            e.1 += cost_usd;
        }
        {
            let mut g = self.by_task_last_turn.write().await;
            g.insert(task_id, (prompt_tokens, completion_tokens, cost_usd));
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
    pub async fn get_last_turn(&self, task_id: Uuid) -> Option<(u64, u64, f64)> {
        self.by_task_last_turn.read().await.get(&task_id).copied()
    }
    pub async fn reset_session(&self, session_id: &str) {
        self.by_session.write().await.remove(session_id);
    }
    pub async fn totals(&self) -> (u64, f64) {
        let g = self.by_session.read().await;
        g.values().fold((0u64, 0.0f64), |acc, v| (acc.0 + v.0, acc.1 + v.1))
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
    bus: EventBus,
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
        let agent_type = if crate::agents::is_specialist_agent(&req.agent_type) {
            req.agent_type.trim().to_lowercase()
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
        let _ = bus.send(
            EventEnvelope::new(
                EventType::SubAgentSpawned,
                Some(serde_json::json!({
                    "task_id": child_id.to_string(),
                    "parent_id": req.requesting_task_id.to_string(),
                    "agent": agent_type,
                    "delegation_reason": "delegate_to_agent"
                })),
            )
            .with_correlation(req.requesting_task_id),
        );
        let _ = bus.send(
            EventEnvelope::new(
                EventType::SubtaskStarted,
                Some(serde_json::json!({
                    "schema_version": 1,
                    "root_task_id": req.requesting_task_id.to_string(),
                    "subtask_id": child_id.to_string(),
                    "agent_type": agent_type,
                    "source": "delegate_to_agent",
                })),
            )
            .with_correlation(req.requesting_task_id),
        );
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
        let parent_task_id_span = req.requesting_task_id;
        let agent_type_span = agent_type.clone();
        let bus_for_waiter = bus.clone();
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
                TaskStatus::Completed => {
                    let _ = bus_for_waiter.send(
                        EventEnvelope::new(
                            EventType::SubtaskCompleted,
                            Some(serde_json::json!({
                                "schema_version": 1,
                                "root_task_id": parent_task_id_span.to_string(),
                                "subtask_id": child_id_span.to_string(),
                                "agent_type": agent_type_span,
                                "status": "completed",
                                "content_preview": msg.chars().take(600).collect::<String>(),
                                "source": "delegate_to_agent",
                            })),
                        )
                        .with_correlation(parent_task_id_span),
                    );
                    Ok(msg)
                }
                TaskStatus::Failed => {
                    let _ = bus_for_waiter.send(
                        EventEnvelope::new(
                            EventType::SubtaskCompleted,
                            Some(serde_json::json!({
                                "schema_version": 1,
                                "root_task_id": parent_task_id_span.to_string(),
                                "subtask_id": child_id_span.to_string(),
                                "agent_type": agent_type_span,
                                "status": "failed",
                                "content_preview": msg.chars().take(600).collect::<String>(),
                                "source": "delegate_to_agent",
                            })),
                        )
                        .with_correlation(parent_task_id_span),
                    );
                    Err(msg)
                }
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

/// Liste des outils disponibles (source unique pour le prompt et la doc).
/// Format: une ligne par outil "nom — usage".
/// Note: "Session terminal" (spec 33) est optionnel et prévu pour une version ultérieure.
pub const AVAILABLE_TOOLS: &[(&str, &str)] = &[
    ("read_file", "read_file <path> [--full] [<offset_ligne> <nb_lignes>] — lire un fichier texte. Par défaut : **500 premières lignes** seulement (évite de saturer le contexte). `TOOL: read_file <chemin> --full` pour tout le fichier (plafond octets côté daemon si très gros). Fenêtre explicite : `read_file workspace:/fichier.ts 1 200`. PDF : texte extrait automatiquement. Path réel ou workspace:/<path>."),
    ("write_file", "write_file <path> puis contenu sur les lignes suivantes — écrire un fichier complet (création/remplacement). Format préféré : première ligne `TOOL: write_file workspace:/fichier`, puis le corps du fichier seul sur les lignes suivantes. Ne pas compresser un fichier entier sur la même ligne que le header. Préférer workspace:/<fichier> si l'utilisateur n'a pas donné de chemin. Si le fichier existe déjà et qu'il faut modifier une partie, préférer edit_file ou search_replace."),
    ("delete_file", "delete_file <path> — supprimer un fichier (pas un répertoire). Chemin workspace:/ ou disque autorisé par tools_policy (mêmes règles que write_file). Code Studio : préférer workspace:/chemin/relatif."),
    ("rename_path", "rename_path <from> <to> — renommer ou déplacer un fichier ou un répertoire (rename atomique si possible ; copie+suppression pour un fichier en cross-device). La destination ne doit pas exister. Deux arguments : le chemin source est le premier token ; tout le reste forme le chemin cible (espaces dans <to> OK). Pas d’espaces dans <from> sans utiliser workspace:/…"),
    ("move_tree", "move_tree <from_dir> <to_dir> — déplacer un répertoire et son contenu (rename atomique si possible, sinon copie récursive + suppression). La destination ne doit pas exister. Même convention d’arguments que rename_path (cible = args après le premier token)."),
    ("search_files", "search_files <dir> <pattern> [--no-ignore] — chercher des fichiers (glob) sous un répertoire ; par défaut respecte .gitignore et ignore node_modules/target/dist/… ; --no-ignore pour tout parcourir."),
    ("grep_content", "grep_content <dir> <pattern> [file_glob] [--regex|-r] [--no-ignore] — chercher dans les fichiers ; défaut = sous-chaîne insensible à la casse + .gitignore ; --regex = motif regex insensible à la casse ; --no-ignore = ignorer .gitignore."),
    ("run_command", "run_command [--cwd <path>] <cmd> [arg1 arg2 ...] — exécuter une commande (autorisée par la politique). Optionnel : --cwd workspace:/ ou chemin disque (allowed_read_paths). Si tools_policy run_command_default_cwd_workspace: true, cwd par défaut = workspace de la tâche. Pour GitHub depuis le shell, préférer gh-axi (npm install -g gh-axi ; principes AXI https://axi.md/) s'il est installé — sorties compactes pour l'agent. Pour l'automation navigateur en CLI, chrome-devtools-axi (même dépôt https://github.com/kunchenguid/axi) en complément d'Akasha browser."),
    ("run_terminal", "run_terminal [--cwd <path>] <cmd> [args...] — exécuter une commande (même que run_command)"),
    ("run_command_background", "run_command_background [--cwd <path>] <cmd> [args...] — lancer en arrière-plan, retourne session_id pour process poll/kill"),
    ("terminal_session", "terminal_session — PTY interactif: GET /api/terminal/capabilities ; API HTTP /api/terminal/pty/sessions (spec/43_session_terminal.md). Sinon: run_command / run_terminal, run_command_background + process list|poll|kill, GET /api/process/watch/recent."),
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
    ("search_replace", "search_replace <path> <ancien_texte> | <nouveau_texte> — une seule ligne TOOL:. Séparateur : **espace | espace** (` | `). Après le chemin, mettre tout de suite le texte exact à remplacer (pas un `|` seul : le découpage sur espaces le transforme en token et vide la recherche). Si le motif contient ` | `, utiliser edit_file ou apply_patch. Exemple : TOOL: search_replace workspace:/src/App.tsx const x = 1 | const x = 2"),
    ("web_fetch", "web_fetch <url> — récupérer le contenu d'une URL (domaine autorisé dans tools_policy allowed_web_domains)"),
    ("web_search", "web_search <query> [max_results] — rechercher sur le web (Brave API; BRAVE_API_KEY, web_search_enabled)"),
    ("web_crawl", "web_crawl <url> [limit] — lancer un crawl Cloudflare Browser Rendering (web_crawl_enabled, cloudflare_account_id, token vault cloudflare_api_token ou CLOUDFLARE_API_TOKEN ; domaines = allowed_web_domains). Retourne un job_id ; poller avec web_crawl_status."),
    ("web_crawl_status", "web_crawl_status <job_id> — statut / résultat d’un job crawl Cloudflare (même config que web_crawl)."),
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
    ("schedule_task", "schedule_task <cron> <prompt> [title] — créer une tâche planifiée active."),
    ("list_scheduled_tasks", "list_scheduled_tasks [limit] — lister les schedules actifs."),
    ("cancel_scheduled_task", "cancel_scheduled_task <schedule_id> — supprimer un schedule par UUID."),
    ("budget_status", "budget_status [session_id] — état budget (usage tokens/coût, seuil, auto-concise)."),
    ("message", "message send <channel> <text> — envoyer un message vers un canal (webhook configuré via AKASHA_MESSAGE_WEBHOOK_URL)"),
    ("browser", "browser navigate <url> — navigate (http/https; domain allowed). browser snapshot — texte + liens. browser screenshot | browser click <css> | browser fill <css> <texte> | browser wait <css_selector|ms> — automation Playwright (spec 39)."),
    ("install_playwright", "install_playwright — run npm install and npx playwright install chromium in the Playwright runner directory (scripts/playwright-runner or AKASHA_PLAYWRIGHT_RUNNER). Requires browser_enabled. Use after ask_user consent if you need explicit approval before download; optional require_approval in tools_policy."),
    ("image", "image <path|url> [prompt] — vision: joindre l'image en pièce jointe au chat (modèle vision dans llm_router)"),
    ("pdf", "pdf <path> — extraire le texte d'un PDF (path dans allowed_read_paths)"),
    ("ask_user", "ask_user — demande une information à l'utilisateur (human in the loop). Ligne suivante : JSON avec question (requis), context (optionnel), choices (optionnel, tableau de chaînes pour choix multiples). Pour une réponse ouverte (chemin, texte libre, secret), omettre choices ou laisser un tableau vide. Si choices est fourni, l'UI propose quand même une saisie libre en plus des boutons. Exemple : {\"question\":\"Quel fichier ?\",\"context\":\"...\",\"choices\":[\"a.txt\",\"b.txt\"]}"),
    ("delegate_to_agent", "delegate_to_agent <agent_type> <message> — déléguer à un sous-agent (Code Studio : réservé à studio_project_manager). agent_type utiles : studio_frontend | studio_backend | studio_fullstack | studio_scaffold | studio_planner | qa | code | conversation | … (voir liste des spécialistes). Un seul niveau depuis la tâche racine : les sous-agents ne rappellent pas delegate_to_agent."),
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

/// Tools advertised in the Code Studio prompt: dev/repo tools only (policy still gates execution).
/// Omits browser, memory_*, sessions_*, device_*, speech, maps plugins, etc.
/// `delegate_to_agent` n’est ajouté que pour `studio_project_manager` et seulement si la politique l’autorise.
fn code_studio_tools_for_prompt(allowed_tools: Option<&[String]>, assigned_agent: &str) -> Vec<String> {
    const STUDIO: &[&str] = &[
        "read_file",
        "write_file",
        "delete_file",
        "rename_path",
        "move_tree",
        "search_files",
        "grep_content",
        "run_command",
        "run_terminal",
        "run_command_background",
        "process",
        "file_diff",
        "diff_unified",
        "dir_compare",
        "git_status",
        "git_diff",
        "git_log",
        "git_rev_parse",
        "edit_file",
        "apply_patch",
        "search_replace",
        "web_fetch",
        "web_search",
        "run_in_container",
        "ask_user",
        "write_todos",
        "merge_todos",
        "read_todos",
        "update_todo",
        "list_skills",
        "read_skill",
        "workspace_graph_search",
        "pdf",
        "image",
    ];
    let mut out: Vec<String> = match allowed_tools {
        None => STUDIO.iter().map(|s| (*s).to_string()).collect(),
        Some(list) => {
            let allowed_lc: std::collections::HashSet<String> =
                list.iter().map(|s| s.to_ascii_lowercase()).collect();
            STUDIO
                .iter()
                .filter(|t| allowed_lc.contains(&t.to_ascii_lowercase()))
                .map(|s| (*s).to_string())
                .collect()
        }
    };
    if out.is_empty() {
        out.extend(
            ["read_file", "write_file", "grep_content", "run_command", "ask_user"]
                .iter()
                .map(|s| (*s).to_string()),
        );
    }
    let delegate_allowed_by_policy = allowed_tools.map_or(true, |l| {
        l.iter()
            .any(|t| t.eq_ignore_ascii_case("delegate_to_agent"))
    });
    if assigned_agent.eq_ignore_ascii_case("studio_project_manager") && delegate_allowed_by_policy {
        if !out
            .iter()
            .any(|t| t.eq_ignore_ascii_case("delegate_to_agent"))
        {
            out.push("delegate_to_agent".to_string());
        }
    }
    out
}

/// Like [`available_tools_instruction`] but **only** listed tool names — no `always_misc` merge
/// (Code Studio must not advertise install_skill, browser, delegate_to_agent, etc.).
fn available_tools_instruction_exact(allowed: &[String]) -> String {
    if allowed.is_empty() {
        return String::new();
    }
    let allowed_lc: std::collections::HashSet<String> =
        allowed.iter().map(|s| s.to_ascii_lowercase()).collect();
    AVAILABLE_TOOLS
        .iter()
        .filter(|(name, _)| allowed_lc.contains(&name.to_ascii_lowercase()))
        .map(|(_, desc)| *desc)
        .collect::<Vec<_>>()
        .join(" ; ")
}

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

/// Heuristic intent labels for **debug only** (`GET/POST /api/plugins/routing_rules`).
/// Runtime tool execution is no longer gated on manifest `routing_rules` / these intents.
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

/// True when the user likely asks for a session recap using the English word "recap".
/// Avoid `contains("recap")` — it false-positives on phrases like "task recap" in long
/// Code Studio DESIGN.md instructions shipped by the UI.
fn english_session_recap_word(lower: &str) -> bool {
    let sans_doc_phrase = lower.replace("task recap", " ");
    sans_doc_phrase
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| w == "recap")
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
        "recap what we did",
        "what did we do",
    ]
    .iter()
    .any(|k| lower.contains(k))
        || english_session_recap_word(&lower);
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

/// Append one JSON line to `data_dir/skills.lock.jsonl` (supply-chain / pin trail).
fn append_skills_lock_entry(data_dir: &Path, entry: &serde_json::Value) {
    let Ok(mut line) = serde_json::to_string(entry) else {
        eprintln!("failed to serialize skills.lock.jsonl entry");
        return;
    };
    line.push('\n');
    let path = data_dir.join("skills.lock.jsonl");
    let write = move || {
        match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(mut f) => {
                if let Err(e) = std::io::Write::write_all(&mut f, line.as_bytes()) {
                    eprintln!("failed to append to {}: {}", path.display(), e);
                }
            }
            Err(e) => {
                eprintln!("failed to open {}: {}", path.display(), e);
            }
        }
    };
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let _ = handle.spawn_blocking(write);
    } else {
        std::thread::spawn(write);
    }
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
            let pin_ref = parsed
                .api_path
                .as_ref()
                .map(|(_, _, branch, _)| branch.clone())
                .unwrap_or_default();
            let digest = hex::encode(Sha256::digest(body.as_bytes()));
            append_skills_lock_entry(
                data_dir,
                &serde_json::json!({
                    "v": 1,
                    "skill_name": parsed.skill_name,
                    "source_url": url,
                    "raw_skill_url": parsed.raw_skill_url,
                    "ref": pin_ref,
                    "skill_md_sha256": digest,
                    "installed_at": chrono::Utc::now().to_rfc3339(),
                }),
            );
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
                            reloaded.cloudflare_api_token = v.get("cloudflare_api_token").ok();
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
                    reloaded.cloudflare_api_token = v.get("cloudflare_api_token").ok();
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

const WRITE_FILE_REMINDER: &str = "\n[Reminder: the user is asking to save a file. You MUST reply ONLY with a header line TOOL: write_file <full_path>, then the file content on the following lines. Do not put the full file content on the same TOOL line. Never say you cannot write to disk.]\n\n";

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
const STUDIO_DISK_REMINDER: &str = "\n[Code Studio — périmètre disque: cette tâche s'exécute sous le dossier projet studio uniquement (miroir workspace:/ et cwd des outils). Ne pas cibler de chemins hors de ce répertoire. Pour npm install / builds à risque, privilégier run_in_container si la politique d'outils l'autorise. Renommer/déplacer un dossier: si `run_command` est autorisé, utiliser `git mv` ou `mv` avec `--cwd workspace:/` puis corriger tous les imports; sinon `read_file` chaque fichier concerné puis `write_file workspace:/nouveau/chemin/...` (les répertoires parents sont créés) et `grep_content`/`search_replace` pour les imports — ne pas boucler sur une tactique qui ne modifie pas réellement les chemins sur disque.]\n\n";

/// Injected with STUDIO_DISK_REMINDER: raise quality bar and user-visible wrap-up for Code Studio agents.
const STUDIO_AGENT_QUALITY_REMINDER: &str = concat!(
    "\n[Code Studio — exigences avant de considérer la demande comme terminée]\n",
    "- Quand tu écris un fichier, son contenu doit être STRICTEMENT le contenu attendu du fichier (code, JSON, Markdown, config). ",
    "Interdiction d'y ajouter du texte conversationnel, des explications, des statuts, des raisonnements, ou des phrases comme ",
    "\"fichier corrigé\", \"je relance le build\", \"voici la correction\". Ces messages vont uniquement dans la réponse chat.\n",
    "- Pour les fichiers de code (ex: .ts, .tsx, .js, .rs, .py), n'écris que du code syntaxiquement valide pour ce langage ; ",
    "ne mets jamais de prose libre hors commentaires valides du langage.\n",
    "- Interdiction dans les fichiers .ts / .tsx / .js / .jsx : lignes « documentation » pour l’utilisateur (titres markdown `**…**`, listes du type « 4. fichier.ts — … », résumés de lot) — le daemon **rejette** l’écriture ; ce texte va **uniquement** dans le chat.\n",
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
    "- Après un **échec de tests** ou de build : lire la sortie d’erreur ; corriger le **minimum** de fichiers (idéalement ceux cités par la stack trace) ; **interdiction** de refactoriser ou réécrire tout le projet « au hasard ». Ne pas modifier les fichiers de tests sauf demande explicite de l’utilisateur. Si trois tentatives ciblées échouent encore, utiliser `ask_user` plutôt que de boucler.\n",
    "- Fichier `CODE_STUDIO_PLAN.md` (racine) : **gabarit fixe** — ligne d'ouverture `# Titre : …` puis dans l'ordre les sections `## Description`, `## Scope`, `## Stack`, `## Structure du projet`, `## Commandes`, `## Fichiers hors scope`, `## Demandes d'évolutions utilisateur par phase`, `## Recommandations`, `## Todos`, `## Informations complémentaires` (conserver ces titres et cet ordre).\n",
    "  Avant d'écrire : `read_file workspace:/CODE_STUDIO_PLAN.md`. Ne **pas** remplacer tout le fichier pour une modification ciblée : mettre à jour **par section** (search_replace ciblé ou une seule section réécrite), en conservant les titres `##` et le reste inchangé.\n",
    "  Ne **jamais** dupliquer une section `## …` déjà présente (pas de second gabarit collé en bas du fichier) : le daemon rejette les écritures qui répètent les titres de section.\n",
    "  Suivi des lots : ajouter une **ligne datée courte** dans `## Informations complémentaires` ou `## Demandes d'évolutions utilisateur par phase` plutôt que de réécrire l'ensemble du plan.\n",
    "  Si le fichier est absent (import), le créer avec ce gabarit en synthétisant le dépôt. Remplacement complet réservé à une demande **explicite** de réinitialisation du plan (bouton ou consigne utilisateur).\n",
    "- Premier lot d'un projet Code Studio : après avoir créé ou mis à jour `CODE_STUDIO_PLAN.md`, créer `workspace:/DESIGN.md` **avant** les développements applicatifs si le fichier est absent. `DESIGN.md` doit fixer le contrat design (front matter YAML + sections markdown) à partir de la demande, de la stack et du plan ; ensuite seulement générer/modifier `src/`, configs, tests, etc.\n",
    "- **Corrections sur le disque (obligatoire quand les outils le permettent)** : pour corriger du code (imports, erreurs TS/build, etc.), utiliser des lignes `TOOL:` — `search_replace` pour des changements localisés, `edit_file` pour un intervalle de lignes, `write_file` seulement si un remplacement de fichier entier est justifié, `apply_patch` si adapté. ",
    "Ne pas faire du **chat** le canal principal de livraison : éviter « voici le fichier corrigé à coller dans workspace:/… », les longs blocs de remplacement manuel ou les résumés à la place d’écritures réelles tant que la politique d’outils autorise les écritures.\n",
    "- **Si une écriture est impossible** (outil refusé, erreur explicite de `write_file` / `search_replace` / etc., chemin hors périmètre) : indiquer **pourquoi** tu ne peux pas appliquer la correction toi-même (citer le message d’erreur ou la contrainte), puis seulement proposer un secours (diff, extrait à copier).\n\n",
);

/// Contexte système court pour les tâches dont le disque outil est sous `studio-projects/` (Code Studio).
/// Remplace le bloc général `APP_CONTEXT` (TUI, skills globales, caméra, etc.).
const CODE_STUDIO_APP_CONTEXT: &str = concat!(
    "[Code Studio — contexte]\n",
    "Tu travailles sur le dépôt du projet ouvert dans Akasha Code Studio. ",
    "Chemins : préfère `workspace:/…` (racine virtuelle de la tâche) ; les fichiers sont synchronisés sur le disque du projet studio.\n",
    "Outils usuels : read_file, write_file, delete_file, rename_path, move_tree (si autorisés), search_replace, edit_file, apply_patch, run_command (avec `--cwd workspace:/` pour builds/tests), git_* si exposés, ask_user pour une question bloquante dans la même tâche.\n",
    "Concentre-toi sur le code et la documentation de ce dépôt — pas sur l’interface générale d’Akasha (TUI, onglets, skills hors projet, caméra, météo). ",
    "Si une capacité externe est indispensable, indique brièvement ce qu’il faudrait côté utilisateur (clé, politique d’outils).\n",
    "Réponds dans la même langue que le dernier message utilisateur. ",
    "Avant d’éditer : lire les fichiers concernés ; ne pas inventer de dépendances — vérifier le manifeste (package.json, Cargo.toml, etc.).\n",
    "Sur le premier lot d’un projet : stabiliser d’abord `CODE_STUDIO_PLAN.md`, puis créer `workspace:/DESIGN.md` avant de commencer le développement applicatif si ce fichier est absent.\n",
    "Corrections : appliquer les changements sur le dépôt avec les outils (`search_replace`, `edit_file`, `write_file`, `apply_patch`, chemins `workspace:/…`) — ne pas se contenter de décrire ou coller un fichier entier pour que l’utilisateur le fasse à ta place. ",
    "Si un outil d’écriture échoue ou est interdit, expliquer clairement la raison avant toute solution de secours.\n\n",
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
    "Reply ONLY with a header line TOOL: write_file <full_path>, then the file content on the following lines. Do not put the full file content on the same TOOL line. ",
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
        "studio_project_manager" => Some("You are the Code Studio project manager (chef de projet). Tu coordonnes chaque demande sur le dépôt ouvert (chemins `workspace:/…`).\n\
Règles d’orchestration :\n\
- **Dossier `specs/`** : pour toute demande d’**évolution** (nouvelle fonctionnalité, changement de comportement, refonte ciblée, branche d’évolution active, ou demande explicitement traitée comme évolution), crée un fichier plan dédié `workspace:/specs/<YYYYMMDD>-<slug-court>.md` avant de lancer l’implémentation. Le plan doit contenir : objectif, périmètre, critères d’acceptation, liste d’étapes numérotées, **marquage des étapes parallélisables** (ex. « (parallèle avec 3) »), risques, et une section **Iterations** pour suivre les passes de correction.\n\
- **Délégation** : tu es le **seul** à appeler `TOOL: delegate_to_agent <agent> <message>` vers des sous-agents (`studio_frontend`, `studio_backend`, `studio_fullstack`, `studio_scaffold`, `studio_planner` pour lecture/plan seul, `qa`, `code`, etc.). Les sous-agents **ne** doivent **pas** rappeler `delegate_to_agent`. Pour plusieurs lots parallèles, enchaîne plusieurs `delegate_to_agent` dans le même tour si la politique d’outils le permet.\n\
- **Boucle de correction** : après chaque vague de sous-agents, lis les résultats / erreurs de build (`run_command --cwd workspace:/` quand autorisé), mets à jour le plan dans `specs/…` et relance des sous-tâches ciblées. **Maximum 5** vagues de retours sous-agents pour la même demande racine ; si au-delà le besoin n’est pas satisfait, réponds à l’utilisateur avec ce qui a été fait, les blocages, et des suggestions concrètes.\n\
- **Synthèse utilisateur** : une fois le besoin rempli (ou en échec contrôlé), termine par un résumé clair en langage accessible.\n\
- **Fichiers** : respecte les règles Code Studio existantes pour `CODE_STUDIO_PLAN.md` et `DESIGN.md` ; n’écrase pas le plan global sans nécessité.\n\
Langue : aligne-toi sur le dernier message utilisateur."),
        "studio_scaffold" => Some("You are the Code Studio scaffold agent. Create a minimal, runnable project skeleton (README, package.json or Cargo.toml, clear entrypoints). Prefer workspace:/ paths when no absolute path is given; mirror files to the studio disk root. When the user message contains a [Stack technique du projet] block at the top, follow it strictly for languages, frameworks, package manager, and tooling; otherwise align with the stack recorded for the project or keep the skeleton generic. Do not add dead files; keep structure conventional. Maintain workspace:/CODE_STUDIO_PLAN.md per the injected Code Studio plan rules (read_file first; section-wise edits only—never replace the whole file for a routine change). FILE OUTPUT RULE (strict): when writing files, write only the file content itself; never insert chat prose/status/explanations/reflection inside files. If a previous generation polluted a file with prose, clean it and keep only valid file content. Before finishing: run an appropriate build or typecheck when possible; in your final reply summarize what you created and how to run it in plain language."),
        "studio_frontend" => Some("You are the Code Studio frontend agent. Build UI components, routing, and styles with accessibility in mind. Prefer workspace:/ paths. When a [Stack technique du projet] block is present in the user message, obey it for UI libraries, bundler, CSS approach, and TypeScript/JavaScript choice. Verify dependencies exist in package.json before importing. Use read_file before editing. Maintain workspace:/CODE_STUDIO_PLAN.md per the injected Code Studio plan rules (section-wise updates; no full-file rewrite for small tasks). FILE OUTPUT RULE (strict): when writing files, write only the file content itself; never insert chat prose/status/explanations/reflection inside files. For code files, output syntactically valid code only (except valid language comments). Run build/lint/typecheck via run_command --cwd workspace:/ when policy allows, and fix issues you introduced. End with a clear user-facing summary of changes and how to preview or test — not only \"Done\"."),
        "studio_backend" => Some("You are the Code Studio backend agent. Add APIs, env-based config, and CORS as needed. Prefer workspace:/ paths. When a [Stack technique du projet] block is present, follow it for runtime (Node, Python, Rust, etc.), framework, and persistence choices. Never assume dependencies exist without checking the manifest. Use git_* tools on the project root when inspecting history. Maintain workspace:/CODE_STUDIO_PLAN.md per the injected Code Studio plan rules (section-wise updates; no full-file rewrite for small tasks). FILE OUTPUT RULE (strict): when writing files, write only the file content itself; never insert chat prose/status/explanations/reflection inside files. For code files, output syntactically valid code only (except valid language comments). Before declaring completion: run tests or at least start/build checks when feasible; summarize APIs and behavior for the user in accessible terms."),
        "studio_fullstack" => Some("You are the Code Studio full-stack agent. Coordinate frontend and backend changes in one pass: clear API contracts, shared types when applicable, and a coherent folder layout. Prefer workspace:/ paths; use run_in_container when policy allows for installs and builds. When a [Stack technique du projet] block is present in the user message, treat it as binding for the whole stack unless the user explicitly contradicts it in the same message. Maintain workspace:/CODE_STUDIO_PLAN.md per the injected Code Studio plan rules (section-wise updates; no full-file rewrite for small tasks). FILE OUTPUT RULE (strict): when writing files, write only the file content itself; never insert chat prose/status/explanations/reflection inside files. If prose was accidentally inserted in a source file, remove it and keep only valid syntax for that file type. Verify end-to-end coherence; run combined build/test when policy allows. Close with a plain-language recap of what changed and how to run the app."),
        "studio_planner" => Some("You are the Code Studio planning agent. READ-ONLY on application source: do NOT write_file, edit_file, delete_file, rename_path, move_tree, search_replace, or apply_patch to any path except workspace:/CODE_STUDIO_PLAN.md. Do NOT run_command except read-only diagnostics (git status, git log, git diff, ls, cat, npm/yarn/pnpm only if the user explicitly asked for a read-only check). You MAY update workspace:/CODE_STUDIO_PLAN.md by sections to capture the plan. Explore with read_file, list_dir, grep_content. Deliver a clear implementation plan, critical files, and risks; end with next steps for a human or for an implement agent."),
        _ => None,
    }
}

/// If AKASHA_TOOLS_JOURNAL_PATH is set, append a line for write tool invocations (Phase 4 modification journal).
async fn log_tool_journal_if_write(tool: &str, args: &[String], result_preview: &str) {
    const WRITE_TOOLS: &[&str] = &[
        "write_file",
        "delete_file",
        "rename_path",
        "move_tree",
        "search_replace",
        "edit_file",
        "apply_patch",
    ];
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

pub(crate) use crate::api_tool_parser::parse_tool_calls;

pub(crate) use crate::api_tool_dispatch::process_ops::{parse_run_command_args, resolve_run_command_working_dir};

pub(crate) use crate::api_tool_dispatch::web_device_memory_ops::parse_device_invoke_params;

pub(crate) use crate::api_tool_dispatch::fs_ops::{rewrite_workspace_plan_key_to_lineage_root, rewrite_workspace_plan_path_str, sync_workspace_store_from_disk, workspace_lineage_root_task_id};

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
    crate::api_tool_dispatch::execute_tool_call(
        crate::api_tool_dispatch::ToolCallContext {
            process_registry,
            long_term_client,
            task_id,
            store_path,
            conv_tx,
            message_webhook_url,
            plugin_registry,
            device_bridge,
            workspace_store,
            browser_registry,
            workspace_root,
        },
        executor,
        tool_name,
        args,
    )
    .await
}

pub(crate) async fn execute_tool_call_impl(
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
            let (path_tokens, explicit_window, want_full) = parse_read_file_args(args);
            let path_input = path_tokens.join(" ");
            let path_str = normalize_tool_path_hint(&path_input);
            let slice_for_line_window = |content: &str, path_label: &str, offset: usize, limit: usize| -> String {
                let lines: Vec<&str> = content.lines().collect();
                let total = lines.len();
                if total == 0 {
                    return format!("[read_file {}] file is empty", path_label);
                }
                let req_start_1 = if offset == 0 { 1 } else { offset };
                let req_start_idx = req_start_1.saturating_sub(1);
                let start_idx = if req_start_idx >= total {
                    total.saturating_sub(limit)
                } else {
                    req_start_idx
                };
                let end_idx_excl = (start_idx + limit).min(total);
                let body = if start_idx < end_idx_excl {
                    lines[start_idx..end_idx_excl].join("\n")
                } else {
                    String::new()
                };
                let actual_start_1 = start_idx + 1;
                let actual_end_1 = end_idx_excl;
                let req_end_1 = req_start_1.saturating_add(limit.saturating_sub(1));
                if actual_start_1 != req_start_1 || actual_end_1 != req_end_1 {
                    format!(
                        "[read_file {}] requested lines {}..{}; returned lines {}..{} (file has {} lines):\n{}",
                        path_label, req_start_1, req_end_1, actual_start_1, actual_end_1, total, body
                    )
                } else {
                    format!(
                        "[read_file {}] lines {}..{} (file has {} lines):\n{}",
                        path_label, actual_start_1, actual_end_1, total, body
                    )
                }
            };
            let format_read_content = |content: &str, path_label: &str| -> String {
                if want_full {
                    let line_count = content.lines().count();
                    let total_bytes = content.len();
                    if total_bytes <= READ_FILE_FULL_OUTPUT_MAX_BYTES {
                        format!(
                            "[read_file {}] full file ({} lines, {} bytes):\n{}",
                            path_label, line_count, total_bytes, content
                        )
                    } else {
                        let (frag, total, truncated) = crate::tool_output::truncate_utf8_by_bytes(
                            content,
                            READ_FILE_FULL_OUTPUT_MAX_BYTES,
                        );
                        let frag_s = frag.into_owned();
                        let mut body = format!(
                            "[read_file {}] --full: first {} UTF-8 bytes shown ({} lines in file, {} bytes total):\n{}",
                            path_label,
                            frag_s.len(),
                            line_count,
                            total,
                            frag_s
                        );
                        if truncated {
                            body.push('\n');
                            body.push_str(&crate::tool_output::truncation_footer_bytes(
                                total,
                                "narrow with grep_content/search_files or read_file with a line window",
                            ));
                        }
                        body
                    }
                } else {
                    let (off, lim) = explicit_window.unwrap_or((1, READ_FILE_DEFAULT_MAX_LINES));
                    let total_lines = content.lines().count();
                    let mut out = slice_for_line_window(content, path_label, off, lim);
                    if explicit_window.is_none() && total_lines > READ_FILE_DEFAULT_MAX_LINES {
                        out.push_str(&format!(
                            "\n{} — {} lines total. Use TOOL: read_file <same_path> --full for entire file, or TOOL: read_file <same_path> <offset> <limit> for a chunk.",
                            READ_FILE_PARTIAL_DEFAULT_MARKER, total_lines
                        ));
                    }
                    out
                }
            };
            if path_str.is_empty() {
                (
                    false,
                    "[read_file] usage: read_file <path> [--full] [<offset_line> <limit_lines>]".to_string(),
                    None,
                )
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
                                return (
                                    true,
                                    format_read_content(content, &format!("workspace:{}", key)),
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
                            format_read_content(&content, &disk_path.display().to_string())
                        } else {
                            format!("[read_file workspace] failed: {}", res.summary)
                        };
                        return (res.success, msg, None);
                    }
                    Err(e) => {
                        let detail = format!("{:#}", e);
                        let hint = if detail.to_ascii_lowercase().contains("not found")
                            || detail.to_ascii_lowercase().contains("cannot find")
                            || detail.to_ascii_lowercase().contains("no such file")
                        {
                            " (hint: verify path/casing and use search_files . <filename> first)"
                        } else {
                            ""
                        };
                        return (
                            false,
                            format!(
                                "[read_file workspace] failed: read error for {} at {}: {}{}",
                                key,
                                disk_path.display(),
                                detail,
                                hint
                            ),
                            None,
                        );
                    }
                };
            } else {
                let p = Path::new(&path_str);
                if path_extension_is_pdf(p) && executor.policy.can_read(p) {
                    pdf_extract_message_from_disk(p, "read_file").await
                } else {
                    match executor.read_file(p).await {
                        Ok((content, res)) => {
                            let msg = if res.success {
                                format_read_content(&content, &p.display().to_string())
                            } else {
                                format!("[read_file] failed: {}", res.summary)
                            };
                            (res.success, msg, None)
                        }
                        Err(e) => {
                            let detail = format!("{:#}", e);
                            let hint = if detail.to_ascii_lowercase().contains("not found")
                                || detail.to_ascii_lowercase().contains("cannot find")
                                || detail.to_ascii_lowercase().contains("no such file")
                            {
                                " (hint: verify path/casing and use search_files . <filename> first)"
                            } else {
                                ""
                            };
                            (
                                false,
                                format!("[read_file] failed: {}{}", detail, hint),
                                None,
                            )
                        }
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
                        let (success, exit_code) = match &result {
                            Ok((out, res)) => (res.success, out.status.code()),
                            Err(_) => (false, None),
                        };
                        *cell_clone.write().await = Some(result);
                        crate::process_watch::push_event(crate::process_watch::ProcessWatchEvent {
                            ts_rfc3339: chrono::Utc::now().to_rfc3339(),
                            session_id: session_id.to_string(),
                            cmd_line: format!("{} {}", cmd, cmd_args.join(" ")),
                            success,
                            exit_code,
                        })
                        .await;
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
        "terminal_session" => {
            let sub = args.get(0).map(String::as_str).unwrap_or("");
            match sub {
                "list" => {
                    match tokio::task::spawn_blocking(move || crate::terminal_pty::PtyManager::global().list_sessions()).await {
                        Ok(Ok(v)) => (true, serde_json::to_string_pretty(&v).unwrap_or_else(|_| "[]".to_string()), None),
                        Ok(Err(e)) => (false, format!("[terminal_session list] {}", e), None),
                        Err(e) => (false, format!("[terminal_session list] join: {}", e), None),
                    }
                }
                "start" => {
                    let create = crate::terminal_pty::PtyCreateBody {
                        argv: None,
                        cwd: None,
                        transcript_name: Some(format!("session-{}", uuid::Uuid::new_v4())),
                        idle_timeout_secs: Some(900),
                        cols: 80,
                        rows: 24,
                    };
                    match tokio::task::spawn_blocking(move || crate::terminal_pty::PtyManager::global().create(create)).await {
                        Ok(Ok(v)) => (
                            true,
                            format!(
                                "[terminal_session start] session_id={} idle_timeout_secs={} (use terminal_session read|write|resize|stop)",
                                v.session_id, v.idle_timeout_secs
                            ),
                            None
                        ),
                        Ok(Err(e)) => (false, format!("[terminal_session start] {}", e), None),
                        Err(e) => (false, format!("[terminal_session start] join: {}", e), None),
                    }
                }
                "read" => {
                    let sid = args.get(1).cloned().unwrap_or_default();
                    if sid.trim().is_empty() {
                        return (false, "[terminal_session read] usage: terminal_session read <session_id> [max]".to_string(), None);
                    }
                    let max = args.get(2).and_then(|x| x.parse::<usize>().ok()).unwrap_or(4096);
                    match tokio::task::spawn_blocking(move || crate::terminal_pty::PtyManager::global().read_output(&sid, max)).await {
                        Ok(Ok(v)) => (true, serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string()), None),
                        Ok(Err(e)) => (false, format!("[terminal_session read] {}", e), None),
                        Err(e) => (false, format!("[terminal_session read] join: {}", e), None),
                    }
                }
                "write" => {
                    let sid = args.get(1).cloned().unwrap_or_default();
                    let payload = args.get(2..).unwrap_or(&[]).join(" ");
                    if sid.trim().is_empty() || payload.is_empty() {
                        return (false, "[terminal_session write] usage: terminal_session write <session_id> <text...>".to_string(), None);
                    }
                    let body = crate::terminal_pty::PtyInputBody { text: Some(payload), bytes_b64: None };
                    match tokio::task::spawn_blocking(move || crate::terminal_pty::PtyManager::global().write_input(&sid, body)).await {
                        Ok(Ok(())) => (true, "[terminal_session write] ok".to_string(), None),
                        Ok(Err(e)) => (false, format!("[terminal_session write] {}", e), None),
                        Err(e) => (false, format!("[terminal_session write] join: {}", e), None),
                    }
                }
                "resize" => {
                    let sid = args.get(1).cloned().unwrap_or_default();
                    let cols = args.get(2).and_then(|x| x.parse::<u16>().ok()).unwrap_or(80);
                    let rows = args.get(3).and_then(|x| x.parse::<u16>().ok()).unwrap_or(24);
                    if sid.trim().is_empty() {
                        return (false, "[terminal_session resize] usage: terminal_session resize <session_id> <cols> <rows>".to_string(), None);
                    }
                    let body = crate::terminal_pty::PtyResizeBody { cols, rows };
                    match tokio::task::spawn_blocking(move || crate::terminal_pty::PtyManager::global().resize(&sid, body)).await {
                        Ok(Ok(())) => (true, "[terminal_session resize] ok".to_string(), None),
                        Ok(Err(e)) => (false, format!("[terminal_session resize] {}", e), None),
                        Err(e) => (false, format!("[terminal_session resize] join: {}", e), None),
                    }
                }
                "stop" => {
                    let sid = args.get(1).cloned().unwrap_or_default();
                    if sid.trim().is_empty() {
                        return (false, "[terminal_session stop] usage: terminal_session stop <session_id>".to_string(), None);
                    }
                    match tokio::task::spawn_blocking(move || crate::terminal_pty::PtyManager::global().close(&sid)).await {
                        Ok(Ok(())) => (true, "[terminal_session stop] ok".to_string(), None),
                        Ok(Err(e)) => (false, format!("[terminal_session stop] {}", e), None),
                        Err(e) => (false, format!("[terminal_session stop] join: {}", e), None),
                    }
                }
                _ => (true, "[terminal_session] usage: terminal_session start|list|read|write|resize|stop. HTTP API: GET /api/terminal/capabilities.".to_string(), None),
            }
        },
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
        "schedule_task" => {
            let cron = args.get(0).map(String::as_str).unwrap_or("").trim().to_string();
            let mut title = "Agent scheduled task".to_string();
            let mut prompt_parts: Vec<&str> = Vec::new();
            let mut i = 1;
            while i < args.len() {
                if args[i] == "--title" {
                    if let Some(value) = args.get(i + 1) {
                        title = value.clone();
                        i += 2;
                        continue;
                    }
                }
                prompt_parts.push(args[i].as_str());
                i += 1;
            }
            let prompt = prompt_parts.join(" ");
            let prompt = prompt.trim();
            if cron.is_empty() || prompt.is_empty() {
                return (
                    false,
                    "[schedule_task] usage: schedule_task <cron> <prompt...> [--title <title>]".to_string(),
                    None,
                );
            }
            match store_path {
                Some(path) => match ScheduleStore::open(path) {
                    Ok(store) => {
                        let now = chrono::Utc::now();
                        let s = Schedule {
                            id: Uuid::new_v4(),
                            name: title,
                            description: prompt.to_string(),
                            timezone: "UTC".to_string(),
                            rrule: cron.to_string(),
                            interval_seconds: None,
                            start_at: now,
                            end_at: None,
                            channel_context: Some(
                                serde_json::json!({
                                    "message": prompt,
                                    "session_id": format!("task:{}", task_id),
                                    "source": "tool:schedule_task",
                                    "created_by_task_id": task_id.to_string()
                                })
                                .to_string(),
                            ),
                            enabled: true,
                            created_at: now,
                            updated_at: now,
                        };
                        match store.insert_schedule(&s) {
                            Ok(()) => (
                                true,
                                format!(
                                    "[schedule_task] created schedule {} ({})",
                                    s.id, cron
                                ),
                                None,
                            ),
                            Err(e) => (false, format!("[schedule_task] {}", e), None),
                        }
                    }
                    Err(e) => (false, format!("[schedule_task] store error: {}", e), None),
                },
                None => (false, "[schedule_task] store not available".to_string(), None),
            }
        }
        "list_scheduled_tasks" => {
            let limit = args
                .get(0)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(20)
                .min(100);
            match store_path {
                Some(path) => match ScheduleStore::open(path) {
                    Ok(store) => match store.list_schedules() {
                        Ok(list) => {
                            let lines: Vec<String> = list
                                .into_iter()
                                .rev()
                                .take(limit)
                                .map(|s| {
                                    format!(
                                        "{} {} {} {}",
                                        s.id,
                                        if s.enabled { "enabled" } else { "disabled" },
                                        "cron",
                                        s.rrule
                                    )
                                })
                                .collect();
                            (
                                true,
                                format!(
                                    "[list_scheduled_tasks] {} schedule(s): {}",
                                    lines.len(),
                                    lines.join(" ; ")
                                ),
                                None,
                            )
                        }
                        Err(e) => (false, format!("[list_scheduled_tasks] {}", e), None),
                    },
                    Err(e) => (false, format!("[list_scheduled_tasks] store error: {}", e), None),
                },
                None => (false, "[list_scheduled_tasks] store not available".to_string(), None),
            }
        }
        "cancel_scheduled_task" => {
            let id = args
                .get(0)
                .and_then(|s| Uuid::parse_str(s).ok());
            match (store_path, id) {
                (Some(path), Some(schedule_id)) => match ScheduleStore::open(path) {
                    Ok(store) => match store.get_schedule(schedule_id) {
                        Ok(Some(_)) => match store.delete_schedule(schedule_id) {
                            Ok(()) => (
                                true,
                                format!("[cancel_scheduled_task] deleted {}", schedule_id),
                                None,
                            ),
                            Err(e) => (false, format!("[cancel_scheduled_task] {}", e), None),
                        },
                        Ok(None) => (
                            false,
                            format!("[cancel_scheduled_task] {} not found", schedule_id),
                            None,
                        ),
                        Err(e) => (false, format!("[cancel_scheduled_task] {}", e), None),
                    },
                    Err(e) => (false, format!("[cancel_scheduled_task] store error: {}", e), None),
                },
                (_, _) => (
                    false,
                    "[cancel_scheduled_task] usage: cancel_scheduled_task <schedule_id>".to_string(),
                    None,
                ),
            }
        }
        "budget_status" => {
            let session_id = args.get(0).cloned().unwrap_or_default();
            let settings = match store_path {
                Some(path) => path.parent().map(load_budget_settings).unwrap_or_default(),
                None => BudgetSettings::default(),
            };
            let scope = if session_id.trim().is_empty() {
                "global".to_string()
            } else {
                format!("session {}", session_id)
            };
            (
                false,
                format!(
                    "[budget_status] limit={} warn_ratio={:.2} auto_concise={} scope={} usage_unavailable=true message=\"live usage is not available from this tool path; query the local /api/budget endpoint for accurate totals\"",
                    settings.daily_token_limit,
                    settings.warn_ratio,
                    settings.auto_concise,
                    scope
                ),
                None,
            )
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
                let session = if let Some(mut s) = g.remove(&task_id) {
                    drop(g);
                    let res = s.send_command(&serde_json::json!({ "cmd": "navigate", "params": { "url": url, "timeout_secs": action_timeout } })).await;
                    // Always reinsert the session — a navigate failure doesn't mean the browser is dead.
                    registry.write().await.insert(task_id, s);
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
                let Some(mut session) = g.remove(&task_id) else {
                    return (false, "[browser] Navigate to a page first (browser navigate <url>).".to_string(), None);
                };
                drop(g);
                let resp = session.send_command(&serde_json::json!({ "cmd": "snapshot" })).await;
                registry.write().await.insert(task_id, session);
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
                let mut g = registry.write().await;
                let Some(mut session) = g.remove(&task_id) else {
                    return (false, "[browser] Navigate to a page first (browser navigate <url>).".to_string(), None);
                };
                drop(g);
                let resp = session
                    .send_command(&serde_json::json!({ "cmd": "screenshot", "params": { "full_page": false } }))
                    .await;
                registry.write().await.insert(task_id, session);
                match resp {
                    Ok(resp) => {
                        let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        if ok {
                            let b64 = resp
                                .pointer("/result/data_base64")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let (preview, total, trunc) = crate::tool_output::truncate_utf8_by_bytes(
                                b64,
                                crate::tool_output::BROWSER_SCREENSHOT_B64_MAX,
                            );
                            let base = format!(
                                "[browser] Screenshot PNG (base64, {} bytes of payload):\n{}",
                                total, preview
                            );
                            let msg = crate::tool_output::with_truncation_footer(
                                base,
                                trunc,
                                total,
                                "truncated base64; decode externally or use a narrower viewport",
                            );
                            (true, msg, None)
                        } else {
                            let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("screenshot failed");
                            (false, format!("[browser] {}", err), None)
                        }
                    }
                    Err(e) => (false, format!("[browser] error: {}", e), None),
                }
            } else if sub == "click" {
                let selector = args[1..].join(" ").trim().to_string();
                if selector.is_empty() {
                    return (false, "[browser] usage: browser click <css_selector>".to_string(), None);
                }
                let mut g = registry.write().await;
                let Some(mut session) = g.remove(&task_id) else {
                    return (false, "[browser] Navigate to a page first (browser navigate <url>).".to_string(), None);
                };
                drop(g);
                let resp = session
                    .send_command(&serde_json::json!({
                        "cmd": "click",
                        "params": { "selector": selector, "timeout_secs": action_timeout }
                    }))
                    .await;
                registry.write().await.insert(task_id, session);
                match resp {
                    Ok(resp) => {
                        let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        if ok {
                            (true, "[browser] Click OK.".to_string(), None)
                        } else {
                            let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("click failed");
                            (false, format!("[browser] {}", err), None)
                        }
                    }
                    Err(e) => (false, format!("[browser] error: {}", e), None),
                }
            } else if sub == "fill" {
                let Some(sel) = args.get(1).map(|s| s.as_str()) else {
                    return (
                        false,
                        "[browser] usage: browser fill <css_selector> <text>".to_string(),
                        None,
                    );
                };
                let sel = sel.trim();
                if sel.is_empty() || args.len() < 3 {
                    return (
                        false,
                        "[browser] usage: browser fill <css_selector> <text>".to_string(),
                        None,
                    );
                }
                let value: String = args[2..].join(" ");
                let mut g = registry.write().await;
                let Some(mut session) = g.remove(&task_id) else {
                    return (false, "[browser] Navigate to a page first (browser navigate <url>).".to_string(), None);
                };
                drop(g);
                let resp = session
                    .send_command(&serde_json::json!({
                        "cmd": "fill",
                        "params": { "selector": sel, "value": value, "timeout_secs": action_timeout }
                    }))
                    .await;
                registry.write().await.insert(task_id, session);
                match resp {
                    Ok(resp) => {
                        let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        if ok {
                            (true, "[browser] Fill OK.".to_string(), None)
                        } else {
                            let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("fill failed");
                            (false, format!("[browser] {}", err), None)
                        }
                    }
                    Err(e) => (false, format!("[browser] error: {}", e), None),
                }
            } else if sub == "wait" {
                let arg = args[1..].join(" ").trim().to_string();
                if arg.is_empty() {
                    return (
                        false,
                        "[browser] usage: browser wait <css_selector> | browser wait <milliseconds>".to_string(),
                        None,
                    );
                }
                let mut g = registry.write().await;
                let Some(mut session) = g.remove(&task_id) else {
                    return (false, "[browser] Navigate to a page first (browser navigate <url>).".to_string(), None);
                };
                drop(g);
                let cmd = if arg.chars().all(|c| c.is_ascii_digit()) {
                    let ms: u64 = arg.parse().unwrap_or(0);
                    serde_json::json!({ "cmd": "wait", "params": { "milliseconds": ms } })
                } else {
                    serde_json::json!({
                        "cmd": "wait",
                        "params": { "selector": arg, "timeout_secs": action_timeout }
                    })
                };
                let resp = session.send_command(&cmd).await;
                registry.write().await.insert(task_id, session);
                match resp {
                    Ok(resp) => {
                        let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        if ok {
                            (true, "[browser] Wait completed.".to_string(), None)
                        } else {
                            let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("wait failed");
                            (false, format!("[browser] {}", err), None)
                        }
                    }
                    Err(e) => (false, format!("[browser] error: {}", e), None),
                }
            } else {
                (false, "[browser] usage: browser navigate <url> | browser snapshot | browser screenshot | browser click <selector> | browser fill <selector> <text> | browser wait <selector|ms>".to_string(), None)
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
                return (false, "[write_file] usage: write_file <path> then file content on following lines".to_string(), None);
            };
            let content = strip_markdown_fences_from_write_content(&content);
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
            match executor.write_file(&disk_path, &content).await {
                Ok(res) => {
                    let msg = if res.success {
                        format!("[write_file {}] {}", disk_path.display(), res.summary)
                    } else {
                        format!("[write_file] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[write_file] error: {}", e), None),
            }
        }
        "delete_file" => {
            let path_str = path_arg_joined(args);
            if path_str.is_empty() {
                return (
                    false,
                    "[delete_file] usage: delete_file <path>".to_string(),
                    None,
                );
            }
            let path_str = normalize_tool_path_hint(path_str.trim());
            if path_str.contains("..") {
                return (
                    false,
                    "[delete_file] invalid path (..)".to_string(),
                    None,
                );
            }
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
                        {
                            let mut guard = ws.write().await;
                            if let Some(per_task) = guard.get_mut(&lineage_task_id) {
                                per_task.remove(&key);
                            }
                        }
                        let rel_path = Path::new(&key);
                        if !executor.policy.can_write(rel_path) {
                            return (
                                false,
                                "[delete_file] path not allowed by policy".to_string(),
                                None,
                            );
                        }
                        let disk_path_opt = workspace_root
                            .map(|root| root.join(&key))
                            .or_else(|| std::env::current_dir().ok().map(|cwd| cwd.join(&key)))
                            .map(strip_verbatim_prefix);
                        if let Some(disk_path) = disk_path_opt {
                            if disk_path.is_dir() {
                                return (
                                    false,
                                    "[delete_file] path is a directory".to_string(),
                                    None,
                                );
                            }
                            if let Some(r) = workspace_root {
                                if crate::studio::is_strictly_under_studio_root(&disk_path, r)
                                    && !disk_path.exists()
                                {
                                    return (
                                        true,
                                        format!("[delete_file workspace:{}] absent (disk).", key),
                                        None,
                                    );
                                }
                            }
                            match tokio::fs::remove_file(&disk_path).await {
                                Ok(()) => {
                                    return (
                                        true,
                                        format!("[delete_file workspace:{}] deleted (disk).", key),
                                        None,
                                    );
                                }
                                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                    return (
                                        true,
                                        format!("[delete_file workspace:{}] absent (disk).", key),
                                        None,
                                    );
                                }
                                Err(e) => {
                                    return (
                                        false,
                                        format!("[delete_file] {}", e),
                                        None,
                                    );
                                }
                            }
                        }
                        return (
                            true,
                            format!("[delete_file workspace:{}] removed from workspace store.", key),
                            None,
                        );
                    }
                    None => {
                        return (
                            false,
                            "[delete_file] workspace paths require a workspace store.".to_string(),
                            None,
                        );
                    }
                }
            }
            let disk_path = resolve_tool_disk_path(path_str.trim(), workspace_root);
            if disk_path.is_dir() {
                return (
                    false,
                    "[delete_file] path is a directory".to_string(),
                    None,
                );
            }
            if !executor.policy.can_write(&disk_path) {
                return (
                    false,
                    "[delete_file] path not allowed by policy".to_string(),
                    None,
                );
            }
            if let Some(root) = workspace_root {
                if crate::studio::is_strictly_under_studio_root(&disk_path, root)
                    && !disk_path.exists()
                {
                    return (
                        true,
                        format!("[delete_file {}] absent.", disk_path.display()),
                        None,
                    );
                }
            }
            match tokio::fs::remove_file(&disk_path).await {
                Ok(()) => (
                    true,
                    format!("[delete_file {}] deleted.", disk_path.display()),
                    None,
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
                    true,
                    format!("[delete_file {}] absent.", disk_path.display()),
                    None,
                ),
                Err(e) => (false, format!("[delete_file] {}", e), None),
            }
        }
        "rename_path" => {
            if args.len() < 2 {
                return (
                    false,
                    "[rename_path] usage: rename_path <from> <to> — disque ou workspace:/ ; la cible est tout le texte après le premier argument (espaces OK dans <to>)."
                        .to_string(),
                    None,
                );
            }
            let from_s = normalize_tool_path_hint(args[0].trim());
            let to_s = normalize_tool_path_hint(args[1..].join(" ").trim());
            if from_s.is_empty() || to_s.is_empty() {
                return (
                    false,
                    "[rename_path] usage: rename_path <from> <to>".to_string(),
                    None,
                );
            }
            if from_s.contains("..") || to_s.contains("..") {
                return (
                    false,
                    "[rename_path] invalid path (..)".to_string(),
                    None,
                );
            }
            let from_is_workspace = is_workspace_virtual_path(&from_s);
            let to_is_workspace = is_workspace_virtual_path(&to_s);

            if from_is_workspace || to_is_workspace {
                if !(from_is_workspace && to_is_workspace) {
                    return (
                        false,
                        "[rename_path] both paths must use workspace:/ or neither".to_string(),
                        None,
                    );
                }
                match workspace_store {
                    Some(ws) => {
                        let normalize_ws_key = |s: &str| -> String {
                            let k = s
                                .trim_start_matches("workspace:/")
                                .trim_start_matches("workspace:")
                                .trim_start_matches('/');
                            let mut k = k.trim().trim_matches('`').trim_matches('"').to_string();
                            if k.ends_with('#') { k.pop(); }
                            k
                        };
                        let lineage_id = workspace_lineage_root_task_id(task_id, store_path);
                        let (from_key, _) = rewrite_workspace_plan_key_to_lineage_root(
                            &normalize_ws_key(&from_s), lineage_id,
                        );
                        let (to_key, _) = rewrite_workspace_plan_key_to_lineage_root(
                            &normalize_ws_key(&to_s), lineage_id,
                        );
                        // Phase 1: check preconditions and remove the key from the store under a
                        // brief write lock (acts as a reservation). The lock is released before
                        // the potentially-slow disk I/O so that unrelated read_file/write_file
                        // operations are not blocked.
                        let reserve_result = {
                            let mut guard = ws.write().await;
                            let per_task = guard.entry(lineage_id).or_default();
                            if per_task.contains_key(&to_key) {
                                Err(format!("[rename_path] destination workspace key already exists: {}", to_key))
                            } else if let Some(content) = per_task.remove(&from_key) {
                                Ok(Some(content))
                            } else {
                                Ok(None) // key absent from store
                            }
                            // write lock dropped here
                        };

                        match reserve_result {
                            Err(msg) => (false, msg, None),
                            Ok(Some(content)) => {
                                // Phase 2: disk rename without holding the lock.
                                let disk_result = if let Some(root) = workspace_root {
                                    let from_disk = root.join(&from_key);
                                    let to_disk = root.join(&to_key);
                                    match executor.rename_path(&from_disk, &to_disk).await {
                                        Ok(res) if !res.success => Err(res.summary),
                                        Ok(_) => Ok(()),
                                        Err(e) => Err(e.to_string()),
                                    }
                                } else {
                                    Ok(()) // no disk backing — in-memory rename only
                                };

                                // Phase 3: commit to_key or rollback from_key.
                                let mut guard = ws.write().await;
                                let per_task = guard.entry(lineage_id).or_default();
                                match disk_result {
                                    Ok(()) => {
                                        per_task.insert(to_key.clone(), content);
                                        (true, format!("[rename_path] workspace key renamed: {} -> {}", from_key, to_key), None)
                                    }
                                    Err(msg) => {
                                        per_task.insert(from_key.clone(), content);
                                        (false, format!("[rename_path] {}", msg), None)
                                    }
                                }
                            }
                            Ok(None) => {
                                // Key absent from store — try disk rename directly.
                                if let Some(root) = workspace_root {
                                    let from_disk = root.join(&from_key);
                                    let to_disk = root.join(&to_key);
                                    match executor.rename_path(&from_disk, &to_disk).await {
                                        Ok(res) => {
                                            let msg = format!("[rename_path] {}", res.summary);
                                            (res.success, msg, None)
                                        }
                                        Err(e) => (false, format!("[rename_path] {}", e), None),
                                    }
                                } else {
                                    (false, format!("[rename_path] workspace key not found: {}", from_key), None)
                                }
                            }
                        }
                    }
                    None => (false, "[rename_path] workspace paths require a workspace store.".to_string(), None),
                }
            } else {
                let from_disk = resolve_tool_disk_path(&from_s, workspace_root);
                let to_disk = resolve_tool_disk_path(&to_s, workspace_root);
                match executor.rename_path(&from_disk, &to_disk).await {
                    Ok(res) => {
                        let msg = format!("[rename_path] {}", res.summary);
                        (res.success, msg, None)
                    }
                    Err(e) => (false, format!("[rename_path] {}", e), None),
                }
            }
        }
        "move_tree" => {
            if args.len() < 2 {
                return (
                    false,
                    "[move_tree] usage: move_tree <from_dir> <to_dir> — répertoire source uniquement ; la cible est tout le texte après le premier argument."
                        .to_string(),
                    None,
                );
            }
            let from_s = normalize_tool_path_hint(args[0].trim());
            let to_s = normalize_tool_path_hint(args[1..].join(" ").trim());
            if from_s.is_empty() || to_s.is_empty() {
                return (
                    false,
                    "[move_tree] usage: move_tree <from_dir> <to_dir>".to_string(),
                    None,
                );
            }
            if from_s.contains("..") || to_s.contains("..") {
                return (
                    false,
                    "[move_tree] invalid path (..)".to_string(),
                    None,
                );
            }
            let from_s = rewrite_workspace_plan_path_str(&from_s, task_id, store_path);
            let to_s = rewrite_workspace_plan_path_str(&to_s, task_id, store_path);
            let from_disk = resolve_tool_disk_path(&from_s, workspace_root);
            let to_disk = resolve_tool_disk_path(&to_s, workspace_root);
            match executor.move_tree(&from_disk, &to_disk).await {
                Ok(res) => {
                    // After a successful disk move, rename all workspace store keys whose paths
                    // fall under the moved directory prefix (mirrors rename_path workspace sync).
                    if res.success && is_workspace_virtual_path(&from_s) && is_workspace_virtual_path(&to_s) {
                        if let Some(ws) = workspace_store {
                            let lineage_id = workspace_lineage_root_task_id(task_id, store_path);
                            let from_prefix = from_s
                                .trim_start_matches("workspace:/")
                                .trim_start_matches("workspace:")
                                .trim_start_matches('/')
                                .trim_end_matches('/')
                                .to_string();
                            let to_prefix = to_s
                                .trim_start_matches("workspace:/")
                                .trim_start_matches("workspace:")
                                .trim_start_matches('/')
                                .trim_end_matches('/')
                                .to_string();
                            let subtree_prefix = format!("{from_prefix}/");
                            let mut guard = ws.write().await;
                            let per_task = guard.entry(lineage_id).or_default();
                            // Collect moves first to avoid holding a mutable + immutable borrow.
                            let to_rename: Vec<(String, String, String)> = per_task
                                .iter()
                                .filter_map(|(key, val)| {
                                    if *key == from_prefix {
                                        // Exact directory entry.
                                        Some((key.clone(), to_prefix.clone(), val.clone()))
                                    } else if let Some(suffix) = key.strip_prefix(&subtree_prefix) {
                                        // Entry inside the subtree: suffix is the part after the "/".
                                        Some((key.clone(), format!("{to_prefix}/{suffix}"), val.clone()))
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            for (old_key, new_key, content) in to_rename {
                                per_task.remove(&old_key);
                                per_task.insert(new_key, content);
                            }
                        }
                    }
                    let msg = format!("[move_tree] {}", res.summary);
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[move_tree] {}", e), None),
            }
        }
        "search_replace" => {
            let path_str = match args.get(0) {
                Some(s) => s.as_str(),
                None => return (
                    false,
                    "[search_replace] usage: search_replace <path> <old_snippet> | <new_snippet> — delimiter must be SPACE PIPE SPACE (` | `); one TOOL line".to_string(),
                    None,
                ),
            };
            let is_workspace = path_str.starts_with("workspace:/") || path_str.starts_with("workspace:");
            let path_str_rewritten = rewrite_workspace_plan_path_str(path_str, task_id, store_path);
            let path_str = path_str_rewritten.as_str();
            let disk_path = resolve_tool_disk_path(path_str, workspace_root);
            let (search, replace) = match parse_search_replace_payload(args) {
                Ok(pair) => pair,
                Err("usage") => {
                    return (
                        false,
                        "[search_replace] usage: search_replace <path> <old_snippet> | <new_snippet> — use delimiter ` | ` (space-pipe-space) between the exact text to find and the replacement. Example: TOOL: search_replace workspace:/src/App.tsx const x = 1 | const x = 2".to_string(),
                        None,
                    );
                }
                Err("empty_search") => {
                    return (
                        false,
                        "[search_replace] search string is empty — the tokenizer often emits `|` as its own token after the path, so the payload must start with the OLD text to find, then ` | `, then the NEW text (not `| …` right after the path). Example: TOOL: search_replace workspace:/f.tsx oldLine | newLine".to_string(),
                        None,
                    );
                }
                Err(_) => {
                    return (
                        false,
                        "[search_replace] could not parse old/new segments".to_string(),
                        None,
                    );
                }
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
        "web_crawl" => {
            let url = args.get(0).map(String::as_str).unwrap_or("").trim();
            if url.is_empty() {
                return (false, "[web_crawl] usage: web_crawl <url> [limit]".to_string(), None);
            }
            let limit = args
                .get(1)
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(50);
            match executor.web_crawl(url, limit).await {
                Ok((body, res)) => {
                    let msg = if res.success {
                        format!("[web_crawl] {}\n{}", res.summary, body)
                    } else {
                        format!("[web_crawl] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[web_crawl] {}", e), None),
            }
        }
        "web_crawl_status" => {
            let job_id = args.get(0).map(String::as_str).unwrap_or("").trim();
            if job_id.is_empty() {
                return (
                    false,
                    "[web_crawl_status] usage: web_crawl_status <job_id>".to_string(),
                    None,
                );
            }
            match executor.web_crawl_job_status(job_id).await {
                Ok((body, res)) => {
                    let (preview, total, trunc) =
                        crate::tool_output::truncate_utf8_by_bytes(&body, 24_000);
                    let msg = if res.success {
                        crate::tool_output::with_truncation_footer(
                            format!("[web_crawl_status] {} — {}", res.summary, preview),
                            trunc,
                            total,
                            "response truncated; poll again if job still running",
                        )
                    } else {
                        format!("[web_crawl_status] {}", res.summary)
                    };
                    (res.success, msg, None)
                }
                Err(e) => (false, format!("[web_crawl_status] {}", e), None),
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
                        // #region agent log
                        debug_log(
                            "H3",
                            "crates/akasha-daemon/src/api.rs:5578",
                            "Spawning blocking plugin call task",
                            serde_json::json!({
                                "plugin_id": pid,
                                "payload_len": pl.len()
                            }),
                        );
                        // #endregion
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
                                        // #region agent log
                                        debug_log(
                                            "H3",
                                            "crates/akasha-daemon/src/api.rs:5629",
                                            "Blocking plugin task panicked",
                                            serde_json::json!({
                                                "plugin_id": plugin_id,
                                                "join_error": join_err.to_string()
                                            }),
                                        );
                                        // #endregion
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
    tokenizer_provider: &str,
    tokenizer_model: &str,
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
    let history_tokens =
        ShortTermStore::turns_tokens_calibrated(tokenizer_provider, tokenizer_model, &turns);
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

/// Returns true when the model asks the user to manually apply file edits
/// ("replace this file with...") instead of using write tools.
fn looks_like_manual_file_patch_reply(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    let cues = [
        "voici le contenu corrigé",
        "à remplacer dans",
        "copie ces blocs",
        "replace directly in",
        "here is the corrected content",
        "paste this into",
        "replace this file with",
        "manually apply",
        "je fournis le contenu corrigé",
        "je fournis les corrections",
    ];
    cues.iter().any(|c| lower.contains(c))
}

/// Returns true when a Code Studio implementation task received a prose-only audit/plan
/// even though write tools are available. This catches replies like "ce qui manque...",
/// "recommandations pour avancer", or "je ne peux pas modifier les fichiers" instead of
/// applying changes with `TOOL:` lines.
fn looks_like_code_studio_prose_only_implementation_reply(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    let refusal_or_meta = [
        "je ne peux pas modifier les fichiers",
        "je ne peux pas modifier",
        "i cannot modify files",
        "i can't modify files",
        "restriction « no tool lines »",
        "restriction \"no tool lines\"",
        "no tool lines",
        "dans ce tour",
        "in this turn",
    ]
    .iter()
    .any(|p| lower.contains(p));
    let plan_without_action = [
        "ce qui manque",
        "recommandations pour avancer",
        "il faudrait",
        "il faut ",
        "prochaine étape",
        "next steps",
        "recommendations",
        "should implement",
        "should add",
        "à mettre en place",
        "mettre en place",
    ]
    .iter()
    .any(|p| lower.contains(p));
    let implementation_surface = [
        "eslint",
        "prettier",
        "src/",
        "package.json",
        "composant",
        "component",
        "hook",
        "tests",
        "build",
        "fichier",
        "file",
    ]
    .iter()
    .any(|p| lower.contains(p));
    refusal_or_meta || (plan_without_action && implementation_surface)
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

/// Outils autorisés pendant la correction automatique post-échec `npm run build` / `cargo check` (Code Studio).
const STUDIO_VERIFY_AUTOFIX_TOOLS: &[&str] = &[
    "read_file",
    "grep_content",
    "search_files",
    "search_replace",
    "edit_file",
    "write_file",
    "apply_patch",
    "file_diff",
    "delete_file",
    "rename_path",
    "move_tree",
];

/// Injecté quand deux tours consécutifs produisent le même résumé d’outils (risque de boucle).
const STUDIO_VERIFY_AUTOFIX_STALL_WARNING: &str = "\n\n[STALL / ANTI-LOOP — READ CAREFULLY]\n\
The previous round’s tool results fingerprint matches this round’s: you are likely repeating a failing strategy.\n\
Rules for this sandbox:\n\
- There is NO `run_command`, NO shell, NO `mv`/`mkdir`/`git mv` here.\n\
- To rename/move a file or a single directory atomically: `rename_path <from> <to>` (destination must not exist).\n\
- To move a directory tree across volumes or when rename fails: `move_tree <from_dir> <to_dir>`.\n\
- If those are not applicable: `read_file` each source under the old path, then `write_file workspace:/new/path/...` with the same contents (write_file creates parent directories). Then `grep_content` + `search_replace` across the repo to fix imports. Optionally `delete_file` obsolete duplicates only if policy allows and you are sure.\n\
- If TS2305 says a symbol is not exported: `read_file` the module that the import resolves to on disk (path shown in the error) before editing; add the export OR change the import to a file that actually exists (`search_files workspace:/. <basename> true`).\n\
- If TS2554 wrong arity: fix the call site or the callee — read both sides.\n\
- Do NOT repeat the same import-only edit without ensuring the target file exists at that path.\n";

fn studio_verify_extract_ts_error_paths(verify_log: &str, cap: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in verify_log.lines() {
        if !line.contains(": error TS") {
            continue;
        }
        let Some((left, _)) = line.split_once('(') else {
            continue;
        };
        let p = left.trim();
        if p.is_empty() {
            continue;
        }
        if !(p.ends_with(".ts") || p.ends_with(".tsx") || p.ends_with(".js") || p.ends_with(".jsx"))
        {
            continue;
        }
        if out.iter().any(|x| x == p) {
            continue;
        }
        out.push(p.to_string());
        if out.len() >= cap {
            break;
        }
    }
    out
}

fn studio_verify_short_hash(input: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    input.hash(&mut h);
    format!("{:08x}", (h.finish() & 0xffff_ffff) as u32)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StudioVerifyUserLanguage {
    French,
    English,
    /// Spanish, German, etc. — LLM must follow the user excerpt language.
    MatchUserExcerpt,
}

fn studio_verify_detect_user_language(clean_user_message: &str) -> StudioVerifyUserLanguage {
    let t: String = clean_user_message.chars().take(6_000).collect();
    if t.trim().is_empty() {
        return StudioVerifyUserLanguage::MatchUserExcerpt;
    }
    let mut fr = 0i32;
    let mut en = 0i32;
    for ch in t.chars() {
        if matches!(
            ch,
            'à' | 'â' | 'é' | 'è' | 'ê' | 'ë' | 'î' | 'ï' | 'ô' | 'ù' | 'û' | 'ü' | 'ç' | 'œ' | 'æ'
        ) {
            fr += 3;
        }
    }
    let lower = t.to_lowercase();
    let pad = format!(" {lower} ");
    const FR_MARKERS: &[&str] = &[
        " tâche ",
        " créer ",
        " merci ",
        " échec ",
        " fichier ",
        " utilisateur ",
        " réinitialisation ",
        " veuillez ",
        " c'est ",
        " être ",
        " dans le ",
        " pas de ",
        " pour ",
        " avec ",
        " depuis ",
        " régénérer ",
        " mettre à jour ",
        " code studio ",
        " consigne ",
    ];
    const EN_MARKERS: &[&str] = &[
        " the ",
        " and ",
        " create ",
        " task ",
        " failed ",
        " please ",
        " update ",
        " build ",
        " error ",
        " file ",
        " user ",
        " request ",
        " with ",
        " from ",
        " regenerate ",
        " design ",
        " studio ",
        " workspace ",
    ];
    for m in FR_MARKERS {
        if pad.contains(m) {
            fr += 2;
        }
    }
    for m in EN_MARKERS {
        if pad.contains(m) {
            en += 2;
        }
    }
    if fr > en + 2 {
        StudioVerifyUserLanguage::French
    } else if en > fr + 2 {
        StudioVerifyUserLanguage::English
    } else {
        StudioVerifyUserLanguage::MatchUserExcerpt
    }
}

fn studio_verify_failure_banner(lang: StudioVerifyUserLanguage) -> &'static str {
    match lang {
        StudioVerifyUserLanguage::French => {
            "Échec vérification automatique (build/check) — la tâche est marquée en échec."
        }
        StudioVerifyUserLanguage::English => {
            "Automatic build/check verification failed — the task was marked as failed."
        }
        StudioVerifyUserLanguage::MatchUserExcerpt => {
            "Build/check verification failed — the task was marked as failed."
        }
    }
}

fn studio_verify_analyzing_progress_line(lang: StudioVerifyUserLanguage) -> &'static str {
    match lang {
        StudioVerifyUserLanguage::French => {
            "Échec de la vérification build — rédaction d'une synthèse lisible…"
        }
        StudioVerifyUserLanguage::English => {
            "Build verification failed — drafting a readable summary…"
        }
        StudioVerifyUserLanguage::MatchUserExcerpt => {
            "Verification failed — drafting a readable summary…"
        }
    }
}

fn studio_verify_build_excerpt_label(lang: StudioVerifyUserLanguage) -> &'static str {
    match lang {
        StudioVerifyUserLanguage::French => "--- Sortie build (extrait) ---",
        StudioVerifyUserLanguage::English => "--- Build output (excerpt) ---",
        StudioVerifyUserLanguage::MatchUserExcerpt => "--- Build output (excerpt) ---",
    }
}

fn studio_verify_summary_heading_markdown(lang: StudioVerifyUserLanguage) -> &'static str {
    match lang {
        StudioVerifyUserLanguage::French => "## Synthèse\n\n",
        StudioVerifyUserLanguage::English => "## Summary\n\n",
        StudioVerifyUserLanguage::MatchUserExcerpt => "",
    }
}

/// Revue légère post-build : critères **manuel** vs résumé des changements (1 appel LLM, sans outils). `None` = rien à signaler.
async fn studio_semantic_acceptance_review(
    llm_router: &std::sync::Arc<akasha_llm::LLMRouter>,
    manual_lines: &[String],
    diff_summary: &str,
    user_excerpt: &str,
    assistant_excerpt: &str,
) -> Option<Vec<String>> {
    if manual_lines.is_empty() {
        return None;
    }
    if std::env::var("AKASHA_STUDIO_SEMANTIC_VERIFY").ok().as_deref() == Some("0") {
        return None;
    }
    let crit = manual_lines.join("\n- ");
    let system = "You are a strict checklist reviewer for a software agent task. \
You ONLY compare the manual acceptance criteria list to the evidence (file change summary + short user goal + short assistant reply). \
Reply with a single JSON object and no markdown fences, no other text. Schema: {\"satisfied\":[\"...\"],\"missing\":[\"...\"]}. \
Put in \"missing\" any manual criterion that is clearly not addressed or contradicted by the evidence. If uncertain, prefer putting a short note in \"missing\" rather than assuming success. \
Use the same language as the criteria when writing strings.";
    let user = format!(
        "Manual criteria (each must be satisfied unless impossible and stated):\n- {crit}\n\n\
Summarized file diffs / changes (truncated):\n{}\n\nUser goal (excerpt):\n{}\n\nAssistant reply (excerpt):\n{}\n\n\
Output JSON only.",
        diff_summary.chars().take(10_000).collect::<String>(),
        user_excerpt.chars().take(1_800).collect::<String>(),
        assistant_excerpt.chars().take(1_800).collect::<String>()
    );
    let req = CompletionRequest {
        prompt: user,
        max_tokens: Some(500),
        temperature: Some(0.05),
        top_p: None,
        top_k: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        num_ctx: None,
        num_gpu: None,
        thinking_level: None,
        preferred_task_type: Some("conversation".to_string()),
        system_prompt: Some(system.to_string()),
        image_data_urls: None,
    };
    let raw = match tokio::time::timeout(
        std::time::Duration::from_secs(45),
        llm_router.complete(&req),
    )
    .await
    {
        Ok(Ok(r)) => r.text,
        _ => return None,
    };
    let trimmed = raw.trim();
    let trimmed = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed)
        .trim()
        .trim_end_matches('`')
        .trim();
    let v: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let missing = v
        .get("missing")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if missing.is_empty() {
        None
    } else {
        Some(missing)
    }
}

/// Synthèse courte (LLM, sans outils) après échec de `studio_verify_after_agent_task`, pour l’utilisateur final.
async fn studio_verify_explain_failure_to_user(
    llm_router: &std::sync::Arc<akasha_llm::LLMRouter>,
    user_message: &str,
    assistant_reply: &str,
    verify_log: &str,
    lang: StudioVerifyUserLanguage,
) -> Option<String> {
    const MAX_USER: usize = 900;
    const MAX_ASSIST: usize = 1_400;
    const MAX_LOG: usize = 14_000;
    let um: String = user_message.chars().take(MAX_USER).collect();
    let ar: String = assistant_reply.chars().take(MAX_ASSIST).collect();
    let log: String = verify_log.chars().take(MAX_LOG).collect();
    let system = match lang {
        StudioVerifyUserLanguage::French => {
            "Tu es un assistant Code Studio. Une tâche agent vient d'échouer à l'étape de vérification automatique (souvent `npm run build`, `tsc`, `vite build`, ou `cargo check`). \
Rédige une synthèse entièrement en français, claire pour quelqu'un qui n'est pas familier avec les logs npm. \
Ne pas inventer de chemins ou numéros de ligne absents du log. Si l'extrait est incomplet, le dire. \
Structure (titres ## ou **gras** courts autorisés) :\n\
1) **Contexte** — en 1–2 phrases : objectif apparent (consigne + extrait de la réponse agent).\n\
2) **Cause principale** — ce qui bloque le build en langage simple.\n\
3) **Fichiers / lignes** — ceux visibles dans le log, sinon \"non précisé dans l'extrait\".\n\
4) **Prochaines étapes** — 2 à 5 actions concrètes.\n\
Pas de copier-coller massif du log ; pas de blocs de code de plus de 6 lignes. Réponse max ~1600 caractères."
                .to_string()
        }
        StudioVerifyUserLanguage::English => {
            "You are a Code Studio assistant. An agent task just failed automatic verification (often `npm run build`, `tsc`, `vite build`, or `cargo check`). \
Write the entire summary in clear English for someone who is not used to npm logs. \
Do not invent file paths or line numbers that are not in the log. If the excerpt is incomplete, say so. \
Structure (short ## or **bold** headings allowed):\n\
1) **Context** — 1–2 sentences: apparent goal (user request + agent reply excerpt).\n\
2) **Root cause** — what broke the build in plain language.\n\
3) **Files / lines** — those visible in the log, otherwise \"not specified in the excerpt\".\n\
4) **Next steps** — 2–5 concrete actions.\n\
No huge copy-paste of the log; no code blocks longer than 6 lines. Max ~1600 characters."
                .to_string()
        }
        StudioVerifyUserLanguage::MatchUserExcerpt => {
            "You are a Code Studio assistant. An agent task just failed automatic verification (often `npm run build`, `tsc`, `vite build`, or `cargo check`). \
**Language (mandatory):** Write your entire summary in the **same natural language** as the \"User message (excerpt)\" block below (French if French, English if English, Spanish if Spanish, etc.). Use headings and bullets in that same language. \
If that excerpt is too short to infer the language, default to **French**. Do not mix two languages. \
Do not invent file paths or line numbers absent from the log; if the excerpt is incomplete, say so in that language. \
Use a short structure: context (1–2 sentences), root cause, files/lines from the log (or \"not in excerpt\"), next steps (2–5 bullets). No huge log paste; max ~1600 characters."
                .to_string()
        }
    };
    let user = match lang {
        StudioVerifyUserLanguage::French => format!(
            "Consigne utilisateur (extrait) :\n{um}\n\nTravail / réponse agent (extrait) :\n{ar}\n\nSortie vérification (build/check) :\n{log}\n\nRédige la synthèse demandée."
        ),
        StudioVerifyUserLanguage::English => format!(
            "User message (excerpt):\n{um}\n\nAgent work / reply (excerpt):\n{ar}\n\nVerification output (build/check):\n{log}\n\nWrite the requested summary."
        ),
        StudioVerifyUserLanguage::MatchUserExcerpt => format!(
            "User message (excerpt):\n{um}\n\nAgent work / reply (excerpt):\n{ar}\n\nVerification output (build/check):\n{log}\n\nWrite the summary in the same language as the user message excerpt."
        ),
    };
    let req = CompletionRequest {
        prompt: user,
        max_tokens: Some(800),
        temperature: Some(0.15),
        top_p: None,
        top_k: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        num_ctx: None,
        num_gpu: None,
        thinking_level: None,
        preferred_task_type: Some("conversation".to_string()),
        system_prompt: Some(system.to_string()),
        image_data_urls: None,
    };
    match tokio::time::timeout(
        std::time::Duration::from_secs(55),
        llm_router.complete(&req),
    )
    .await
    {
        Ok(Ok(r)) => {
            let t = r.text.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.chars().take(2_400).collect())
            }
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "studio verify explain: LLM complete failed");
            None
        }
        Err(_) => {
            tracing::warn!("studio verify explain: LLM timeout");
            None
        }
    }
}

/// Path argument for `read_file` (before optional line/offset window), normalized like `execute_tool_call`.
fn parse_read_file_tool_path(args: &[String]) -> Option<String> {
    let line_window = if args.len() >= 3 {
        let maybe_limit = args.last().and_then(|s| s.parse::<usize>().ok());
        let maybe_offset = args
            .get(args.len().saturating_sub(2))
            .and_then(|s| s.parse::<usize>().ok());
        match (maybe_offset, maybe_limit) {
            (Some(off), Some(lim)) if lim > 0 => Some((off, lim)),
            _ => None,
        }
    } else {
        None
    };
    let path_parts_len = if line_window.is_some() && args.len() >= 3 {
        args.len() - 2
    } else {
        args.len()
    };
    if path_parts_len == 0 {
        return None;
    }
    let path_input = args[..path_parts_len].join(" ");
    let path_str = normalize_tool_path_hint(path_input.trim());
    if path_str.is_empty() || path_str.contains("..") {
        return None;
    }
    Some(path_str)
}

/// Remove model reasoning wrappers that occasionally leak in plain text responses.
fn strip_reasoning_wrappers(input: &str) -> String {
    let mut out = input.to_string();
    // Fast path: nothing to strip.
    if !out.contains("<think>") && !out.contains("</think>") {
        return out;
    }
    loop {
        let Some(start) = out.find("<think>") else {
            break;
        };
        if let Some(end_rel) = out[start..].find("</think>") {
            let end = start + end_rel + "</think>".len();
            out.replace_range(start..end, "");
        } else {
            // Unclosed tag: remove tail from opening marker.
            out.truncate(start);
            break;
        }
    }
    out.replace("</think>", "").replace("<think>", "")
}

fn extract_ts2306_not_module_paths(verify_log: &str, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in verify_log.lines() {
        if !line.contains("error TS2306") || !line.contains("is not a module") {
            continue;
        }
        let Some((_, after)) = line.split_once("File '") else {
            continue;
        };
        let Some((path, _)) = after.split_once("' is not a module") else {
            continue;
        };
        let p = path.trim().replace('\\', "/");
        if p.is_empty() || out.iter().any(|x| x == &p) {
            continue;
        }
        out.push(p);
        if out.len() >= cap {
            break;
        }
    }
    out
}

/// Après un log d’échec de compilation, enchaîne quelques tours LLM + exécution d’outils pour corriger les sources.
/// Retourne `true` si au moins un outil d’écriture a réussi au moins une fois.
async fn studio_verify_run_llm_autofix_rounds(
    bus: &EventBus,
    llm_router: &std::sync::Arc<akasha_llm::LLMRouter>,
    tools_executor: &std::sync::Arc<RwLock<std::sync::Arc<akasha_tools::ToolExecutor>>>,
    skill_registry: Option<&std::sync::Arc<crate::skills::SkillRegistry>>,
    plugin_registry: Option<&std::sync::Arc<crate::plugins::PluginRegistry>>,
    process_registry: Option<&ProcessRegistry>,
    conv_tx: Option<mpsc::Sender<OrchestratorTask>>,
    long_term_client: Option<&LongTermMemoryClient>,
    workspace_store: Option<&TaskWorkspaceStore>,
    browser_registry: Option<&crate::browser::BrowserSessionRegistry>,
    device_bridge: Option<&std::sync::Arc<crate::device_bridge::DeviceBridge>>,
    task_id: Uuid,
    store_path: &Path,
    data_dir: &Path,
    tool_disk_root: &Path,
    verify_log: &str,
    max_llm_rounds: u32,
) -> bool {
    const VERIFY_SNIP: usize = 16_000;
    const LLM_TIMEOUT_SECS: u64 = 180;
    let timeline_correlation = resolve_root_task_id(store_path, task_id).unwrap_or(task_id);
    let message_webhook_url = std::env::var("AKASHA_MESSAGE_WEBHOOK_URL").ok();
    let system = "You repair a Code Studio project after `npm run build` or `cargo check` failed. \
Workspace paths are relative to the project root shown in the user message. \
Emit only lines starting with TOOL: using these tools: read_file, grep_content, search_files, search_replace, edit_file, write_file, apply_patch, file_diff, delete_file, rename_path, move_tree. \
STRICT tool-call format: `TOOL: <tool_name> <arg1> <arg2> ...` (space-separated args only). \
For ALL file/dir path arguments, ALWAYS use `workspace:/...` paths (including `workspace:/.` for project root scans). \
Never use bare relative paths like `src/...` or `.`; use `workspace:/src/...` and `workspace:/.`. \
Do NOT output JSON function-call style (invalid: `TOOL: read_file({\"path\":\"src/App.tsx\"})`). \
Do NOT output pipe syntax (invalid: `TOOL: search_files|pattern|*|path|.`). \
Valid examples: `TOOL: read_file workspace:/src/App.tsx 320 20`, `TOOL: search_files workspace:/. package.json true`. \
Do NOT use ask_user, delegate_to_agent, run_command, browser_*, install_skill, or any tool not in that list. \
IMPORTANT — this autofix sandbox has NO shell: you cannot `mv`, `git mv`, `mkdir` via run_command (it is disabled). \
To rename/move a file or directory when the destination does not exist: `TOOL: rename_path workspace:/old/path workspace:/new/path` (second path may contain spaces). \
To move a whole directory tree (e.g. cross-volume): `TOOL: move_tree workspace:/old_dir workspace:/new_dir`. \
If that is not enough: read each file under the old path, then write_file to `workspace:/new/dir/...` (parent dirs are created automatically), then search_replace / grep_content to fix imports across the project. \
If the build says a module has no exported member: read_file that module on disk at the path the error shows; either export the symbol or change imports to a module that actually exists (use search_files to locate the real file). \
Do not paste markdown fences or assistant prose into source files — only valid source code. \
Fix every compiler error; prefer minimal search_replace / edit_file over rewriting whole files. \
If a [Code index — …] block appears in the user message, use it as orientation only — you must still apply edits with search_replace / edit_file / write_file / apply_patch. \
When FILES_WITH_ERRORS or KNOWN_TS_ERRORS already pin down locations, prefer a concrete fix (search_replace / edit_file) over broad exploration; avoid read_file loops on the same path without editing. \
Large projects may require many reads across different files — that is fine. \
Do not deliver the fix only as prose or a full-file markdown block for the user to paste: emit write tools (`search_replace`, `edit_file`, `write_file`, `apply_patch`) unless a tool error or policy block forces you to explain why you cannot apply it yourself, then give a manual fallback. \
If tool results repeat without progress, change strategy (see any [STALL / ANTI-LOOP] block in the user message).";
    let root_disp = tool_disk_root.display().to_string();
    let log_snip: String = verify_log.chars().take(VERIFY_SNIP).collect();
    let focus_files = studio_verify_extract_ts_error_paths(verify_log, 10);
    let error_lines = verify_log
        .lines()
        .filter(|l| l.contains(": error TS"))
        .take(18)
        .collect::<Vec<_>>()
        .join("\n");
    let known_ts_errors_hash = studio_verify_short_hash(&error_lines);
    let sticky_context = format!(
        "CAVEMAN CONTEXT (reuse every round):\n\
ROOT: {}\n\
FILES_WITH_ERRORS: {}\n\
KNOWN_TS_ERRORS:\n{}\n",
        root_disp,
        if focus_files.is_empty() {
            "(none parsed)".to_string()
        } else {
            focus_files.join(", ")
        },
        if error_lines.trim().is_empty() {
            "(none captured)"
        } else {
            error_lines.as_str()
        }
    );
    let mut first_user = format!(
        "{}\nProject root (all relative paths are under this directory):\n{}\n\nBuild output:\n{}\n\nFix the project.",
        sticky_context, root_disp, log_snip
    );
    if studio_code_rag_enabled() {
        if let Some(pid) = crate::studio::studio_project_id_from_disk_root(data_dir, tool_disk_root) {
            let query = format!(
                "{}\n{}\n{}",
                error_lines,
                focus_files.join(" "),
                log_snip.chars().take(3_000).collect::<String>()
            );
            let data_dir_owned = data_dir.to_path_buf();
            let root_owned = tool_disk_root.to_path_buf();
            let code_rag_block = tokio::task::spawn_blocking(move || {
                let store = crate::code_rag::CodeRagStore::new(&data_dir_owned);
                let chunks = store.retrieve(
                    &pid,
                    &root_owned,
                    &query,
                    crate::code_rag::RetrieveOptions {
                        top_k: 8,
                        max_chars: 6_000,
                    },
                );
                let chunks = chunks.ok()?;
                if chunks.is_empty() {
                    return None;
                }
                crate::code_rag::format_retrieved_chunks(&chunks, 6_000)
            })
            .await
            .ok()
            .flatten();
            if let Some(block) = code_rag_block {
                first_user = format!(
                    "[Code index — extraits locaux (RAG Code Studio) pour guider la correction. Ce n’est pas une lecture complète des fichiers : vous devez quand même appliquer search_replace / edit_file / write_file / apply_patch sur les chemins en erreur.]\n\n{block}\n\n{first_user}"
                );
            }
        }
    }
    let exec = tools_executor.read().await.clone();
    let mut any_write_success = false;
    let mut follow_up = String::new();
    let mut short_or_no_tool_retries = 0u32;
    let mut writes_ok_count = 0u32;
    /// Nudge only when the same file is read repeatedly without an intervening write (exploration stays allowed across many files).
    const SAME_FILE_READ_THRESHOLD: u32 = 3;
    let mut last_read_file_path: Option<String> = None;
    let mut consecutive_same_file_reads: u32 = 0;
    let mut last_tool = "(none)".to_string();
    let mut last_failed_files = if focus_files.is_empty() {
        "(none parsed)".to_string()
    } else {
        focus_files.join(", ")
    };
    let mut prev_round_tool_fp: Option<String> = None;
    let mut pending_stall: Option<String> = None;
    let mut read_only_round_streak: u32 = 0;

    // Deterministic first-aid: TS2306 "is not a module" often comes from an empty .ts file.
    // Patch those files immediately so subsequent LLM rounds can focus on remaining errors.
    {
        let not_module_paths = extract_ts2306_not_module_paths(verify_log, 12);
        for p in &not_module_paths {
            let as_path = std::path::Path::new(p);
            if !as_path.starts_with(tool_disk_root) {
                continue;
            }
            if !as_path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("ts"))
            {
                continue;
            }
            let content = std::fs::read_to_string(as_path).unwrap_or_default();
            if content.trim().is_empty() {
                if std::fs::write(as_path, "export {}\n").is_ok() {
                    any_write_success = true;
                    writes_ok_count = writes_ok_count.saturating_add(1);
                    let _ = bus.send(
                        EventEnvelope::new(
                            EventType::ProgressUpdate,
                            Some(serde_json::json!({
                                "task_id": task_id.to_string(),
                                "progress_pct": 57,
                                "message": format!("Correctif auto TS2306: `{}` était vide, ajout de `export {{}}`.", p)
                            })),
                        )
                        .with_correlation(timeline_correlation),
                    );
                }
            }
        }
    }

    for round in 0..max_llm_rounds {
        let state_snapshot = format!(
            "STATE SNAPSHOT:\n\
ROUND_INDEX: {}\n\
LAST_TOOL: {}\n\
WRITES_OK_COUNT: {}\n\
NO_PARSEABLE_TOOL_RETRIES: {}\n\
KNOWN_TS_ERRORS_HASH: {}\n\
LAST_FAILED_FILES: {}\n",
            round + 1,
            last_tool,
            writes_ok_count,
            short_or_no_tool_retries,
            known_ts_errors_hash,
            last_failed_files
        );
        let user_block = if round == 0 {
            format!("{}\n{}", first_user, state_snapshot)
        } else {
            format!(
                "{}{}\n{}\nRound {} — previous tool results:\n{}\n\nEmit more TOOL: lines to fix remaining errors, or a single line AUTOFIX_DONE if the project should compile.",
                pending_stall.take().unwrap_or_default(),
                sticky_context,
                state_snapshot,
                round + 1,
                follow_up
            )
        };
        let req = CompletionRequest {
            prompt: user_block,
            max_tokens: Some(8192),
            temperature: Some(0.15),
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            repeat_penalty: None,
            num_ctx: None,
            num_gpu: None,
            thinking_level: None,
            preferred_task_type: Some("code_generation".to_string()),
            system_prompt: Some(system.to_string()),
            image_data_urls: None,
        };
        let resp_text = match tokio::time::timeout(
            std::time::Duration::from_secs(LLM_TIMEOUT_SECS),
            llm_router.complete(&req),
        )
        .await
        {
            Ok(Ok(r)) => strip_reasoning_wrappers(&r.text),
            Ok(Err(e)) => {
                tracing::warn!(task_id = %task_id, error = %e, "studio verify autofix: LLM complete failed");
                break;
            }
            Err(_) => {
                tracing::warn!(task_id = %task_id, "studio verify autofix: LLM timeout");
                break;
            }
        };
        let trimmed = resp_text.trim();
        if trimmed.contains("AUTOFIX_DONE") && parse_tool_calls(trimmed).is_empty() {
            tracing::info!(task_id = %task_id, round, "studio verify autofix: model signalled AUTOFIX_DONE");
            break;
        }
        let calls = parse_tool_calls(trimmed);
        if calls.is_empty() {
            let text_len = trimmed.chars().count();
            if short_or_no_tool_retries < 2 && round + 1 < max_llm_rounds {
                short_or_no_tool_retries += 1;
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": task_id.to_string(),
                            "progress_pct": 56,
                            "message": "Réponse modèle trop courte / sans TOOL, nouvelle tentative de correction auto…"
                        })),
                    )
                    .with_correlation(timeline_correlation),
                );
                follow_up = format!(
                    "Model returned no parseable TOOL lines (len={text_len}). \
Retry now. Return only TOOL: lines; if FILES_WITH_ERRORS is set, prefer one read_file on the primary error file then immediately search_replace or edit_file to fix it."
                );
                tracing::warn!(
                    task_id = %task_id,
                    round,
                    text_len,
                    retry = short_or_no_tool_retries,
                    "studio verify autofix: no parseable TOOL lines, retrying"
                );
                continue;
            }
            tracing::debug!(task_id = %task_id, round, text_len, "studio verify autofix: no parseable TOOL lines");
            break;
        }
        short_or_no_tool_retries = 0;
        let mut round_results: Vec<String> = Vec::new();
        let mut round_read_success = false;
        let mut round_write_like_success = false;
        let lanes = akasha_tools::schedule_tool_calls(&calls);
        for (_lane, lane_calls) in lanes {
            for (name, args) in &lane_calls {
                let actual_tool = match skill_registry {
                    Some(reg) => reg
                        .get(name)
                        .await
                        .map(|s| s.tool_ref)
                        .unwrap_or_else(|| name.clone()),
                    None => name.clone(),
                };
                let actual_tool = canonicalize_tool_name(&actual_tool);
                last_tool = actual_tool.clone();
                if !STUDIO_VERIFY_AUTOFIX_TOOLS
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case(&actual_tool))
                {
                    round_results.push(format!(
                        "[{}] skipped in studio autofix (not an allowed repair tool)",
                        actual_tool
                    ));
                    continue;
                }
                if exec.policy.requires_approval(&actual_tool) {
                    round_results.push(format!(
                        "[{}] skipped in studio autofix (requires human approval)",
                        actual_tool
                    ));
                    continue;
                }
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": task_id.to_string(),
                            "progress_pct": 58,
                            "message": format!("Correction auto compilation : {} …", actual_tool)
                        })),
                    )
                    .with_correlation(timeline_correlation),
                );
                let (success, res, _cap) = execute_tool_call(
                    &exec,
                    &actual_tool,
                    args,
                    process_registry,
                    long_term_client,
                    task_id,
                    Some(store_path),
                    conv_tx.clone(),
                    message_webhook_url.as_deref(),
                    plugin_registry,
                    device_bridge,
                    workspace_store,
                    browser_registry,
                    Some(tool_disk_root),
                )
                .await;
                let write_like = matches!(
                    actual_tool.as_str(),
                    "write_file" | "search_replace" | "edit_file" | "apply_patch" | "delete_file"
                        | "rename_path" | "move_tree"
                );
                if success && write_like {
                    any_write_success = true;
                    round_write_like_success = true;
                    writes_ok_count = writes_ok_count.saturating_add(1);
                    last_read_file_path = None;
                    consecutive_same_file_reads = 0;
                } else if success && actual_tool.as_str() == "read_file" {
                    round_read_success = true;
                    if let Some(path_key) = parse_read_file_tool_path(args) {
                        if last_read_file_path.as_ref() == Some(&path_key) {
                            consecutive_same_file_reads =
                                consecutive_same_file_reads.saturating_add(1);
                        } else {
                            last_read_file_path = Some(path_key);
                            consecutive_same_file_reads = 1;
                        }
                    }
                }
                round_results.push(format!("[{}] success={} {}", actual_tool, success, res));
            }
        }
        follow_up = round_results.join("\n");
        if round_read_success && !round_write_like_success {
            read_only_round_streak = read_only_round_streak.saturating_add(1);
        } else {
            read_only_round_streak = 0;
        }
        if read_only_round_streak >= 2 {
            let _ = bus.send(
                EventEnvelope::new(
                    EventType::ProgressUpdate,
                    Some(serde_json::json!({
                        "task_id": task_id.to_string(),
                        "progress_pct": 59,
                        "message": format!(
                            "[Étape: garde-fou autofix] {} round(s) lecture-only détecté(s) — correction d'écriture forcée (search_replace/edit_file/write_file).",
                            read_only_round_streak
                        )
                    })),
                )
                .with_correlation(timeline_correlation),
            );
            let not_module_paths = extract_ts2306_not_module_paths(verify_log, 6);
            let focus = if not_module_paths.is_empty() {
                "No TS2306 path parsed from compiler output.".to_string()
            } else {
                format!("TS2306 focus files: {}", not_module_paths.join(", "))
            };
            follow_up.push_str(&format!(
                "\n\n[HARD GUARD — READ-ONLY ROUNDS]\n\
You just completed {read_only_round_streak} round(s) with successful reads but no successful write tool.\n\
Stop broad exploration. Apply one concrete fix NOW with search_replace/edit_file/write_file.\n\
If a file is empty and TS2306 says 'is not a module', add at least `export {{}}` first, then continue with proper exported types.\n\
{focus}\n"
            ));
        }
        if read_only_round_streak >= 4 {
            let _ = bus.send(
                EventEnvelope::new(
                    EventType::ProgressUpdate,
                    Some(serde_json::json!({
                        "task_id": task_id.to_string(),
                        "progress_pct": 60,
                        "message": "[Étape: arrêt anti-boucle] Trop de rounds lecture-only successifs en autofix — arrêt de la boucle pour éviter l'exploration infinie."
                    })),
                )
                .with_correlation(timeline_correlation),
            );
            tracing::warn!(
                task_id = %task_id,
                round = round + 1,
                "studio verify autofix: aborting due to repeated read-only rounds"
            );
            break;
        }
        if consecutive_same_file_reads >= SAME_FILE_READ_THRESHOLD {
            let path_disp = last_read_file_path
                .as_deref()
                .unwrap_or("(unknown path)");
            follow_up.push_str(&format!(
                "\n\n[LOOP GUARD — REPEATED read_file]\n\
You successfully read_file `{path_disp}` {SAME_FILE_READ_THRESHOLD} times in a row without an intervening successful write tool.\n\
The compiler output already signals what is wrong: apply a minimal fix with `TOOL: search_replace … | …` or `TOOL: edit_file …` on this file (or switch to another path from FILES_WITH_ERRORS / KNOWN_TS_ERRORS instead of re-opening the same file).\n"
            ));
            last_read_file_path = None;
            consecutive_same_file_reads = 0;
        }
        let files_after_round = studio_verify_extract_ts_error_paths(&follow_up, 10);
        if !files_after_round.is_empty() {
            last_failed_files = files_after_round.join(", ");
        }
        if follow_up.trim().is_empty() {
            break;
        }
        let fp = studio_verify_short_hash(&follow_up.chars().take(4_500).collect::<String>());
        if let Some(prev) = prev_round_tool_fp.as_ref() {
            if *prev == fp && round >= 1 {
                pending_stall = Some(STUDIO_VERIFY_AUTOFIX_STALL_WARNING.to_string());
                tracing::warn!(
                    task_id = %task_id,
                    round = round + 1,
                    "studio verify autofix: repeated tool-output fingerprint (stall guard)"
                );
            }
        }
        prev_round_tool_fp = Some(fp);
    }
    if !any_write_success {
        let focus_files = studio_verify_extract_ts_error_paths(verify_log, 8);
        if !focus_files.is_empty() {
            let _ = bus.send(
                EventEnvelope::new(
                    EventType::ProgressUpdate,
                    Some(serde_json::json!({
                        "task_id": task_id.to_string(),
                        "progress_pct": 57,
                        "message": "Fallback auto-correction : lecture ciblée des fichiers en erreur TS…"
                    })),
                )
                .with_correlation(timeline_correlation),
            );
            let mut focused_reads: Vec<String> = Vec::new();
            for p in &focus_files {
                let read_args = vec![p.clone()];
                let (ok, res, _cap) = execute_tool_call(
                    &exec,
                    "read_file",
                    &read_args,
                    process_registry,
                    long_term_client,
                    task_id,
                    Some(store_path),
                    conv_tx.clone(),
                    message_webhook_url.as_deref(),
                    plugin_registry,
                    device_bridge,
                    workspace_store,
                    browser_registry,
                    Some(tool_disk_root),
                )
                .await;
                if ok {
                    focused_reads.push(format!(
                        "FILE: {}\n{}",
                        p,
                        res.chars().take(5000).collect::<String>()
                    ));
                }
            }
            if !focused_reads.is_empty() {
                let fallback_req = CompletionRequest {
                    prompt: format!(
                        "Build output:\n{}\n\nFocused file reads:\n{}\n\nReturn ONLY TOOL lines to fix the TS syntax/build errors now. \
Use only: search_replace, edit_file, write_file, apply_patch, file_diff, delete_file, rename_path, move_tree. \
No prose, no ask_user, no run_command. Prefer rename_path / move_tree for renames; otherwise use write_file under new paths (parents auto-created) and fix imports.",
                        verify_log.chars().take(12_000).collect::<String>(),
                        focused_reads.join("\n\n---\n\n")
                    ),
                    max_tokens: Some(8192),
                    temperature: Some(0.1),
                    top_p: None,
                    top_k: None,
                    frequency_penalty: None,
                    presence_penalty: None,
                    repeat_penalty: None,
                    num_ctx: None,
                    num_gpu: None,
                    thinking_level: None,
                    preferred_task_type: Some("code_generation".to_string()),
                    system_prompt: Some(
                        "You are in final deterministic Code Studio autofix fallback. Emit only TOOL lines. \
Use STRICT format: `TOOL: <tool_name> <arg1> <arg2> ...` with plain space-separated arguments. \
For ALL file/dir paths, ALWAYS use `workspace:/...` arguments (for root scans, use `workspace:/.`). \
Do not use bare relative paths (`src/...`, `.`) and do not use `tool(...)` JSON-call style or `|key|value|` syntax."
                            .to_string(),
                    ),
                    image_data_urls: None,
                };
                if let Ok(Ok(resp)) = tokio::time::timeout(
                    std::time::Duration::from_secs(LLM_TIMEOUT_SECS),
                    llm_router.complete(&fallback_req),
                )
                .await
                {
                    let calls = parse_tool_calls(resp.text.trim());
                    if !calls.is_empty() {
                        let lanes = akasha_tools::schedule_tool_calls(&calls);
                        for (_lane, lane_calls) in lanes {
                            for (name, args) in &lane_calls {
                                let actual_tool = canonicalize_tool_name(name);
                                if !matches!(
                                    actual_tool.as_str(),
                                    "search_replace" | "edit_file" | "write_file" | "apply_patch" | "file_diff" | "delete_file"
                                        | "rename_path" | "move_tree"
                                ) {
                                    continue;
                                }
                                let (success, _res, _cap) = execute_tool_call(
                                    &exec,
                                    &actual_tool,
                                    args,
                                    process_registry,
                                    long_term_client,
                                    task_id,
                                    Some(store_path),
                                    conv_tx.clone(),
                                    message_webhook_url.as_deref(),
                                    plugin_registry,
                                    device_bridge,
                                    workspace_store,
                                    browser_registry,
                                    Some(tool_disk_root),
                                )
                                .await;
                                if success
                                    && matches!(
                                        actual_tool.as_str(),
                                        "search_replace" | "edit_file" | "write_file" | "apply_patch" | "delete_file"
                                            | "rename_path" | "move_tree"
                                    )
                                {
                                    any_write_success = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    any_write_success
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
    let (message, embedded_studio_acceptance) =
        crate::api_studio::strip_embedded_acceptance_json(&message);
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
    let studio_disk_system_append = {
        let proj_base = crate::studio::studio_projects_base(data_dir_for_studio_flags);
        if tool_disk_workspace_root.starts_with(proj_base) {
            format!("{}{}", STUDIO_DISK_REMINDER, STUDIO_AGENT_QUALITY_REMINDER)
        } else {
            String::new()
        }
    };
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
    if code_studio_disk_task && !is_subagent {
        let dd = data_dir_for_studio_flags.to_path_buf();
        let root = tool_disk_workspace_root.clone();
        let tid = task_id;
        match tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let snap_path = dd.join("studio-task-snapshots").join(format!("{tid}.json"));
            if snap_path.exists() {
                return Ok(false);
            }
            crate::studio_task_snapshot::capture_task_snapshot(&dd, tid, &root)?;
            Ok(true)
        })
        .await
        {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => tracing::debug!(task_id = %tid, "studio task snapshot already captured; skipping"),
            Ok(Err(e)) => tracing::warn!(task_id = %tid, error = %e, "studio task snapshot capture failed"),
            Err(e) => tracing::warn!(task_id = %tid, error = %e, "studio task snapshot join failed"),
        }
    }
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
    // Code Studio: avoid global long-term memory, user document RAG, multi-workspace graph indexes,
    // and cross-session episodic bleed — the prompt already carries plan/stack and disk context.
    if code_studio_disk_task {
        memory_profile.semantic_top_k = 0;
        memory_profile.episodic_limit = 0;
        memory_profile.facts_limit = 0;
        memory_profile.user_rag_top_k = 0;
        memory_profile.workspace_graph_top_k = 0;
        memory_profile.expand_by_graph = false;
        memory_profile.allow_project_recall = false;
        memory_profile.recent_context_max_chars = memory_profile.recent_context_max_chars.min(12_000);
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

    let mut code_studio_system_tools_block = String::new();
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
        let studio_tool_list = if code_studio_disk_task {
            Some(code_studio_tools_for_prompt(
                allowed_tools.as_deref(),
                assigned_agent.as_str(),
            ))
        } else {
            None
        };
        let base = if let Some(ref v) = studio_tool_list {
            available_tools_instruction_exact(v)
        } else {
            available_tools_instruction(allowed_tools.as_deref())
        };
        let run_command_os_rule = match std::env::consts::OS {
            "windows" => "RUN_COMMAND OS: You are on Windows. Prefer cmd, PowerShell, curl.exe; avoid grep, cat, sed (not in default PATH). Use full path or .exe when needed. To test that the vault token works (e.g. GitHub API), use Invoke-WebRequest: TOOL: run_command VAULT:GITHUB_TOKEN=GITHUB_TOKEN powershell -NoProfile -Command \"Invoke-WebRequest -Uri 'https://api.github.com/repos/owner/repo' -Headers @{ Authorization = 'Bearer ' + $env:GITHUB_TOKEN } | Select-Object -Expand Content\" (replace owner/repo). Ensure 'powershell' is in allowed_commands in tools_policy.yaml. The system injects the vault value into the environment for the command.\n\
             ",
            _ => "RUN_COMMAND OS: You are on Linux/macos. Standard Unix commands (curl, grep, etc.) are available.\n\
             ",
        };
        if code_studio_disk_task {
            let skills_part_studio = match &skill_registry {
                Some(reg) => {
                    let list = reg.list().await;
                    if list.is_empty() {
                        String::new()
                    } else {
                        let names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
                        format!(
                            " ; Skills (n’utiliser que si pertinent pour ce dépôt ; sinon ignorer) : {}",
                            names.join(", ")
                        )
                    }
                }
                None => String::new(),
            };
            code_studio_system_tools_block = format!(
                "[Code Studio — outils]\n\
                 Une ligne par invocation : `TOOL: nom_outil arg1 …`.\n\
                 Disponibles : {}{}.\n\
                 Règles :\n\
                 - Outils strictement nécessaires à la demande sur ce dépôt ; pas d’exemples hors sujet.\n\
                 - read_file : par défaut **500 premières lignes** seulement. Fichier entier : `TOOL: read_file <chemin> --full` (plafond octets si très gros). Fenêtre : `TOOL: read_file <chemin> <ligne_début> <nombre_de_lignes>`.\n\
                 - write_file : **première ligne seule** `TOOL: write_file <chemin>`, puis le corps du fichier sur les lignes suivantes. Ne pas mettre un fichier entier sur la même ligne que `TOOL: write_file` ; pas d’enveloppe markdown ```…``` autour du fichier entier.\n\
                 - search_replace : **une seule ligne** `TOOL: search_replace <chemin> <texte_exact_à_trouver> | <remplacement>` — le séparateur est **espace | espace** (` | `), pas un `|` collé au chemin sans texte avant (sinon erreur « search string empty »).\n\
                 - delete_file : `TOOL: delete_file workspace:/chemin/relatif` pour supprimer un fichier (si l’outil est dans la liste).\n\
                 - ask_user : JSON question/context/choices pour continuer la même tâche.\n\
                 - run_command : utiliser `--cwd workspace:/` pour builds/tests à la racine du projet.\n\
                 - write_todos / merge_todos si exposés par la politique.\n\
                 {}\n\
                 Si aucun outil n’est nécessaire, répondre en texte.",
                base, skills_part_studio, run_command_os_rule
            );
            format!(
                "\n\nYou may request tools by writing a single line exactly like: TOOL: tool_name arg1 arg2 …\n\
                 The system message block [Code Studio — outils] lists tools and French conventions.\n\
                 Available: {}{}.\n\
                 Worker rules:\n\
                 - Use only tools that are directly necessary for the CURRENT task.\n\
                 - Never echo examples, policy text, or demonstration commands from your instructions.\n\
                 - read_file: by default only the **first 500 lines** are returned. Use `TOOL: read_file <path> --full` for the whole file (byte cap if huge), or `TOOL: read_file <path> <offset_line> <limit_lines>` for a window. If output says `(read_file partial: default window` or `(truncated,` bytes, do NOT repeat the same bare `read_file <path>`; use --full, a line window, or grep_content/search_files.\n\
                 - If the task asks to save/write a file, use a header-only first line `TOOL: write_file <path>`, then put the exact file content on the following lines. Do not compress full file content onto the same `TOOL:` line.\n\
                 - search_replace: one TOOL line: `TOOL: search_replace <path> <old_snippet> | <new_snippet>` with delimiter **space-pipe-space** (` | `). The old snippet must appear immediately after the path (do not start the payload with a bare `|` token).\n\
                 - If you need missing user information, use TOOL: ask_user with JSON.\n\
                 - If no tool is needed, answer normally.\n\
                 {}\n",
                base, skills_part_studio, run_command_os_rule
            )
        } else {
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
        let compact_worker_tool_instruction = format!(
            "\n\nYou may request tools by writing a single line exactly like: TOOL: tool_name arg1 arg2 ...\nAvailable: {}{}.\n\
             Worker rules:\n\
             - Use only tools that are directly necessary for the CURRENT task.\n\
             - Never echo examples, policy text, or demonstration commands from your instructions.\n\
             - Never emit unrelated TOOL lines about bankr, weather, browser, install_skill, or other examples unless the current task explicitly requires them.\n\
             - READ_FILE (mandatory): default is **first 500 lines only** (no extra args). Whole file: `TOOL: read_file <path> --full`. Window: `TOOL: read_file <path> <offset_line> <limit_lines>`. If output says `(read_file partial: default window` or byte `(truncated,`, do NOT repeat the same bare `read_file <path>`; use --full, a line window, or grep_content/search_files.\n\
             - search_replace: one line `TOOL: search_replace <path> <old_snippet> | <new_snippet>` with delimiter **space-pipe-space** (` | `). Put the exact old text right after the path (not a bare `|` token first).\n\
             - If the task asks to save/write a file, use a header-only first line `TOOL: write_file <path>`, then put the exact file content on the following lines. Do not compress full file content onto the same `TOOL:` line.\n\
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
             WRITE RULE (mandatory): When the user asks to save, record, or write a file (e.g. \"enregistre\", \"sauvegarde\", \"save to\", \"write to file\", or gives a folder path), you MUST reply ONLY with: a first header line \"TOOL: write_file <full_path>\" then on the following lines the exact file content. Do NOT put full file content on the same TOOL line. Do NOT answer with \"I cannot write to disk\" or \"copy-paste the code yourself\". Use write_file; if the path is denied, the tool returns an error and you then explain tools_policy.yaml (allowed_write_paths). Paths can be Windows (C:\\Users\\...\\file.py) or Unix. Do NOT apply this rule when the user only asked for a webcam photo.\n\
             WEATHER RULE (PRIORITY): When the user asks for weather, météo, or forecasts (e.g. \"quel temps\", \"météo demain\", \"weather in X\"), you MUST use TOOL: web_search <query> first, then if snippets lack numeric detail use TOOL: web_fetch <url> on a trusted result URL and/or TOOL: browser navigate <url> then TOOL: browser snapshot (many weather sites are JS-heavy). Do NOT use bankr, portfolio, or any other skill for weather — use web_search plus web_fetch/browser as needed.\n\
             BROWSER RULE (PRIORITY): When the user explicitly asks to open the browser, go to a website, or show something on X/Twitter (e.g. \"ouvre le navigateur\", \"open the browser\", \"va sur X\", \"go to twitter\", \"cherche sur X\", \"ouvre le navigateur et cherche\"), you MUST use TOOL: browser navigate <url> first with the appropriate URL (e.g. https://x.com/akashabot for a profile, https://x.com for the home page). You may then add a short message. Do NOT use only web_search when the user asked to open the browser or go to X/Twitter.\n\
             SOCIAL / LOGGED-IN SITES RULE: If tools_policy allows the domain, use TOOL: browser navigate <https URL> and TOOL: browser snapshot when the user asks to open or inspect X/Twitter or similar. Do NOT refuse with vague \"security\", \"confidentiality\", or \"structural policy\" claims — the user runs Akasha locally and controls tools_policy. Real limitation: you cannot type the user's password or complete interactive MFA inside the managed browser on their behalf; if a login wall blocks content, say that clearly and offer practical options (user logs in manually in that same browser session if their environment keeps the session, or official API access via TOOL: run_command with VAULT:... when applicable). Do NOT state that vault-backed API access is forbidden when the user has configured secrets — follow VAULT ENV RULE.\n\
             WEB SEARCH RULE: When the user asks for external information (weather, news, forecasts, schedules, etc.) that you do not have, you MUST use TOOL: web_search <query> first, then answer from fetched content — not only from snippets. If snippets are insufficient, use TOOL: web_fetch <url> on a relevant result URL, or TOOL: browser navigate <url> then TOOL: browser snapshot so YOU retrieve the page text inside Akasha (managed browser), then summarize for the user. Do NOT reply with \"I did not find it\" or suggest sites without having called web_search. Do NOT tell the user to open links in their own browser when web_fetch or browser snapshot is available and allowed — retrieve and answer yourself.\n\
             READ_FILE (mandatory): default is **first 500 lines only** (no extra args). Whole file: `TOOL: read_file <path> --full`. Window: `TOOL: read_file <path> <offset_line> <limit_lines>`. If output says `(read_file partial: default window` or byte `(truncated,`, do NOT repeat the same bare `read_file <path>`; use --full, a line window, or grep_content/search_files.\n\
             SEARCH_REPLACE: one line `TOOL: search_replace <path> <old_snippet> | <new_snippet>` — delimiter **space-pipe-space** (` | `). The old snippet must follow the path immediately (tokenizer may emit `|` as its own token; do not start the payload with `|` alone).\n\
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
    let profile_block = if code_studio_disk_task {
        String::new()
    } else {
        crate::personality::build_personality_prompt(
            spec_dir.as_path(),
            &agent_profile,
            Some(&assigned_agent),
        )
    };
    let os_env_block = match std::env::consts::OS {
        "windows" => "[Environment] The daemon runs on Windows. For run_command, prefer cmd, PowerShell, curl.exe; avoid Unix-only commands (grep, cat, sed) that are not in the default PATH (except WSL).\n\n",
        _ => "[Environment] The daemon runs on Linux/macOS. You can use usual Unix commands (curl, grep, etc.).\n\n",
    };
    let mut system_prompt = String::with_capacity(8192);
    if code_studio_disk_task {
        system_prompt.push_str(CODE_STUDIO_APP_CONTEXT);
    } else {
        system_prompt.push_str(APP_CONTEXT);
    }
    system_prompt.push_str(os_env_block);
    if let Some(role_prompt) = agent_role_system_prompt(role_agent_for_system_prompt) {
        system_prompt.push_str("[Role]\n");
        system_prompt.push_str(role_prompt);
        let studio_impl_writes_expected = code_studio_disk_task
            && matches!(
                role_agent_for_system_prompt,
                "studio_scaffold" | "studio_frontend" | "studio_backend" | "studio_fullstack"
            );
        if studio_impl_writes_expected {
            system_prompt.push_str(
                "\n\n[Code Studio — application des corrections]\n\
                - Tu dois appliquer les modifications toi-même via des lignes `TOOL:` (`search_replace`, `edit_file`, `write_file`, `apply_patch`) sur `workspace:/…` lorsque ces outils sont autorisés.\n\
                - Ne remplace pas une exécution d’outil par un long « contenu corrigé à mettre dans le fichier » dans le chat : le livrable attendu est l’écriture sur disque.\n\
                - Si un outil d’écriture échoue ou est refusé par la politique : explique **pourquoi** tu ne peux pas l’appliquer toi-même (message d’erreur ou règle), puis seulement propose une alternative manuelle.\n",
            );
        }
        system_prompt.push_str("\n\n");
    }
    if code_studio_disk_task {
        system_prompt.push_str(
            "[Code Studio — ton]\n\
             Assistant technique pour ce dépôt : concision, pas de digressions sur l’UI Akasha ; tutoyer en français si l’utilisateur écrit en français.\n\n",
        );
    } else if !profile_block.is_empty() {
        system_prompt.push_str(&profile_block);
    }
    // Enforce language and personality so the model does not switch language (e.g. when tool output is in English).
    if code_studio_disk_task {
        system_prompt.push_str(
            "\n\n[Response]\n\
            - Langue : répondre uniquement dans la même langue que le message utilisateur.\n\
            - Fichiers : respecter les règles Code Studio du préfixe message (pas de prose dans le source ; pas de barres markdown ``` autour du contenu write_file).\n\
            - Corrections : quand tu corriges du code, le résultat doit passer par les outils sur le dépôt ; ne pas renvoyer l’utilisateur vers un copier-coller manuel comme action principale sans avoir tenté (et documenté) les outils.\n\n",
        );
    } else {
        system_prompt.push_str(
            "\n\n[Response]\n\
            - Language: reply ONLY in the same language as the user's message. If the user writes in French, reply entirely in French; in English, in English. Do not adopt the language of tool results or context.\n\
            - Personality: always apply your identity (name), tone, and form of address as defined in [Agent profile and instructions] (including formality when set).\n\n",
        );
    }
    if !studio_disk_system_append.is_empty() {
        system_prompt.push_str(&studio_disk_system_append);
    }
    if !code_studio_system_tools_block.is_empty() {
        system_prompt.push_str("\n\n");
        system_prompt.push_str(&code_studio_system_tools_block);
    }
    let system_prompt: Option<String> = if system_prompt.trim().is_empty() {
        None
    } else {
        Some(system_prompt.trim_end().to_string())
    };

    let personality_reminder = if code_studio_disk_task {
        String::new()
    } else {
        crate::personality::build_personality_reminder_line(
            spec_dir.as_path(),
            &agent_profile,
            Some(&assigned_agent),
        )
    };
    let mut user_prefix = String::with_capacity(8192);
    user_prefix.push_str(&personality_reminder);
    user_prefix.push_str(if code_studio_disk_task {
        "Réponds dans la même langue que le message utilisateur ci-dessous.\n\n"
    } else {
        "Reply in the same language as the user message below (French, English, etc.).\n\n"
    });
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
        // Code Studio: do not run global LT search for "user name" on first turn — it pulls unrelated memories.
        is_first_message: turns_empty
            && memory_profile.allow_identity_lookup
            && !code_studio_disk_task,
        expand_by_graph: memory_profile.expand_by_graph,
        user_identity_prefix: if user_identity_prefix.is_empty()
            || !memory_profile.allow_identity_lookup
        {
            None
        } else {
            Some(user_identity_prefix)
        },
        task_outcomes_limit: if code_studio_disk_task { 6 } else { 8 },
        task_outcomes_scope_session: code_studio_disk_task,
        include_preference_and_personality_episodic: !code_studio_disk_task,
        ..Default::default()
    };
    if memory_profile.semantic_top_k > 0
        || memory_profile.episodic_limit > 0
        || memory_profile.facts_limit > 0
        || recall_params.user_identity_prefix.is_some()
        || recall_params.task_outcomes_limit > 0
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
    let router_task_type_for_compact = if preferred_task_type_override.as_deref()
        == Some("image_generation")
        || assigned_agent == "image_generation"
    {
        llm_router.resolve_task_type_for_agent("conversation")
    } else {
        preferred_task_type_override
            .clone()
            .unwrap_or_else(|| llm_router.resolve_task_type_for_agent(&assigned_agent))
    };
    let (tok_prov, tok_model) = llm_router
        .primary_route_for_task_type(&router_task_type_for_compact)
        .unwrap_or_else(|| ("default".to_string(), "default".to_string()));
    if let Some(ref st) = short_term {
        if memory_profile.compact_before_prompt {
            let new_msg_tokens = ShortTermStore::estimate_tokens_calibrated(
                tok_prov.as_str(),
                tok_model.as_str(),
                &message,
            );
            compact_short_term_if_needed(
                st,
                &session_id,
                &llm_router,
                new_msg_tokens,
                long_term_client.as_ref(),
                tok_prov.as_str(),
                tok_model.as_str(),
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
    let intent_flags = compute_message_intent_flags(clean_message);
    let plugin_catalog_reminder = if code_studio_disk_task
        || is_small_talk_fast_lane
        || plugin_registry.is_none()
    {
        String::new()
    } else {
        match plugin_registry.as_ref() {
            None => String::new(),
            Some(reg) => {
                let all_tool: Vec<PluginManifest> = reg
                    .manifests()
                    .into_iter()
                    .filter(|m| m.kind == PluginKind::Tool)
                    .collect();
                if all_tool.is_empty() {
                    String::new()
                } else {
                    let selected_ids = crate::plugins::selection::select_relevant_plugins_via_llm(
                        llm_router.as_ref(),
                        clean_message,
                        &all_tool,
                    )
                    .await;
                    let id_lower: std::collections::HashSet<String> = selected_ids
                        .iter()
                        .map(|s| s.to_lowercase())
                        .collect();
                    let mut picked: Vec<PluginManifest> = all_tool
                        .iter()
                        .filter(|m| id_lower.contains(&m.id.to_lowercase()))
                        .cloned()
                        .collect();
                    if picked.is_empty() {
                        picked.clone_from(&all_tool);
                    }
                    crate::plugins::selection::build_plugin_catalog_block(&picked)
                }
            }
        }
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
    let geolocation_distance_reminder: &str = if intent_flags.geolocation_distance {
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
            "{0}{1}{2}{3}{4}{5}{6}{7}{8}{9}{10}{11}User:\n{12}",
            guardrail_reminder_block,
            write_reminder,
            web_search_reminder,
            web_search_followup_reminder,
            transport_reminder,
            geolocation_distance_reminder,
            plugin_catalog_reminder,
            social_feed_reminder,
            device_camera_reminder,
            image_generation_reminder,
            github_vault_reminder,
            code_dev_sandbox_reminder,
            user_message
        )
    } else {
        format!(
            "{0}{1}{2}{3}{4}{5}{6}{7}{8}{9}{10}{11}{12}User:\n{13}",
            user_prefix.trim_end(),
            guardrail_reminder_block,
            write_reminder,
            web_search_reminder,
            web_search_followup_reminder,
            transport_reminder,
            geolocation_distance_reminder,
            plugin_catalog_reminder,
            social_feed_reminder,
            device_camera_reminder,
            image_generation_reminder,
            github_vault_reminder,
            code_dev_sandbox_reminder,
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
        // Tours d'affilée avec uniquement des outils d'exploration (Code Studio).
        let mut studio_read_only_streak_rounds: u32 = 0;
        let mut social_snapshot_seen = false;
        // Loop detection history is tracked per "agent key".
        // For now, agent key = current task_id (sub-agent tasks each have their own task_id).
        let mut tool_loop_history_by_agent: std::collections::HashMap<
            String,
            Vec<(String, String)>,
        > = std::collections::HashMap::new();
        let loop_agent_key = task_id.to_string();
        let mut last_tool_results_blob: Option<String> = None;
        let mut force_synthesis_attempted = false;
        let mut meta_response_retry_count = 0u32;
        let mut small_talk_off_topic_retries = 0u32;
        // Per-agent (agent key = task_id) set of paths where a full read_file was truncated.
        // Used to hard-block repeated full-file reads and force chunked/windowed reads.
        let mut truncated_read_file_paths_by_agent: std::collections::HashMap<
            String,
            std::collections::HashSet<String>,
        > = std::collections::HashMap::new();
        // Orchestrated deliverables: re-prompts when the model returns no parseable TOOL lines.
        let mut orch_disk_write_nags = 0u32;
        // Code Studio implementation agents: prevent "copy/paste this file" fallback
        // when write tools are available but unused.
        let mut studio_manual_patch_nags = 0u32;
        let mut studio_prose_only_write_nags = 0u32;
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
                prompt: format!("{}{}", current_prompt, tool_instruction),
                max_tokens: Some(completion_max_tokens),
                temperature: Some(0.7),
                preferred_task_type,
                system_prompt: system_prompt.clone(),
                image_data_urls: if tool_loop_history_by_agent
                    .get(&loop_agent_key)
                    .map(|v| v.is_empty())
                    .unwrap_or(true)
                {
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
                        let chunk_ref: &str = if chunk.len() > MAX_ACCUMULATED {
                            tracing::warn!(
                                chunk_len = chunk.len(),
                                max = MAX_ACCUMULATED,
                                "Stream chunk larger than progress cap; truncating for accumulated progress buffer"
                            );
                            let mut end = MAX_ACCUMULATED;
                            while end > 0 && !chunk.is_char_boundary(end) {
                                end -= 1;
                            }
                            &chunk[..end]
                        } else {
                            chunk.as_str()
                        };
                        if accumulated.len() + chunk_ref.len() > MAX_ACCUMULATED {
                            let mut keep_len = MAX_ACCUMULATED.saturating_sub(chunk_ref.len());
                            while keep_len > 0 && !accumulated.is_char_boundary(keep_len) {
                                keep_len -= 1;
                            }
                            accumulated.truncate(keep_len);
                        }
                        accumulated.push_str(chunk_ref);
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
                            let prompt_tokens =
                                resp.usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0);
                            let completion_tokens = resp
                                .usage
                                .as_ref()
                                .map(|u| u.completion_tokens)
                                .unwrap_or(0);
                            let cost = resp.cost_usd.unwrap_or(0.0);
                            store
                                .add(
                                    task_id,
                                    &session_id,
                                    prompt_tokens,
                                    completion_tokens,
                                    cost,
                                )
                                .await;
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
            let response =
                crate::api_llm_stream::choose_merged_response_for_tools(&response, &accumulated);

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

            const MAX_STUDIO_PROSE_ONLY_WRITE_NAGS: u32 = 4;
            if code_studio_disk_task
                && no_parseable_tools_this_round
                && studio_prose_only_write_nags < MAX_STUDIO_PROSE_ONLY_WRITE_NAGS
                && looks_like_code_studio_prose_only_implementation_reply(&response_plain)
            {
                let policy_allows_write = tools_executor_snapshot
                    .as_ref()
                    .map(|e| {
                        e.policy.can_use_tool("write_file")
                            || e.policy.can_use_tool("edit_file")
                            || e.policy.can_use_tool("search_replace")
                            || e.policy.can_use_tool("apply_patch")
                    })
                    .unwrap_or(false);
                if policy_allows_write {
                    studio_prose_only_write_nags += 1;
                    current_prompt = format!(
                        "User request: {}\n\nYour previous reply:\n{}\n\n[Code Studio — mandatory execution guard]\nThe user asked you to implement changes in the repository. Your previous reply was an audit/plan/refusal instead of executing available write tools. You DO have write tools in this task. Emit only executable TOOL lines now.\n\nRequired format examples:\nTOOL: write_file workspace:/path/to/file.ts\n<complete file content on following lines>\n\nTOOL: search_replace workspace:/path/to/file.ts old snippet | new snippet\n\nDo not say \"I cannot modify files\", \"No TOOL lines\", or ask where to start. Apply the requested changes on disk with `write_file`, `edit_file`, `search_replace`, or `apply_patch`.",
                        user_message,
                        response_plain
                    );
                    continue;
                }
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
                        let tool_args: &[String] = args.as_slice();
                        // Hard guardrail: if read_file on this path was already truncated for this
                        // agent key, block repeated full-file reads and require chunked/windowed read.
                        if actual_tool.eq_ignore_ascii_case("read_file") {
                            let (path_tokens, explicit_window, want_full) =
                                parse_read_file_args(&tool_args);
                            let allows_escape_truncation_guard =
                                want_full || explicit_window.is_some();
                            if !allows_escape_truncation_guard {
                                let path_input = path_tokens.join(" ");
                                let path_str = normalize_tool_path_hint(&path_input);
                                let previously_truncated = truncated_read_file_paths_by_agent
                                    .get(&loop_agent_key)
                                    .map(|s| s.contains(&path_str))
                                    .unwrap_or(false);
                                if previously_truncated {
                                    let blocked = format!(
                                        "[read_file] blocked repeated default read after partial output for path={}. Use TOOL: read_file {} --full, or a line window (e.g. TOOL: read_file {} 1 200 then 201 200), or narrow with grep_content/search_files.",
                                        path_str,
                                        if path_str.is_empty() { "<path>" } else { &path_str },
                                        if path_str.is_empty() { "<path>" } else { &path_str }
                                    );
                                    let payload = serde_json::json!({
                                        "tool": "read_file",
                                        "args": tool_args,
                                        "result_preview": blocked,
                                        "success": false,
                                        "reason": "read_file_truncated_requires_chunking"
                                    });
                                    let _ = bus.send(
                                        EventEnvelope::new(EventType::ToolInvoked, Some(payload))
                                            .with_correlation(timeline_correlation),
                                    );
                                    tool_results.push(blocked);
                                    continue;
                                }
                            }
                        }
                        let args_str = tool_args.join(" ");
                        let history = tool_loop_history_by_agent
                            .entry(loop_agent_key.clone())
                            .or_default();
                        history.push((actual_tool.clone(), args_str.clone()));
                        // Phase 4: loop detection — same tool+args repeated 3 times
                        // for the same agent key (agent = sub-agent task_id).
                        if history.len() >= 3 {
                            let last = history.last().unwrap();
                            if history
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
                            let permission_mode = load_permission_mode(data_dir);
                            if permission_mode.mode == "allow_all" {
                                // Global session mode bypasses interactive approval prompts.
                            } else {
                            let scope_key = tool_scope_key(&actual_tool, &tool_args);
                            let state = crate::permissions_center::load(data_dir);
                            let mut already_granted = false;
                            if let Some(decision) =
                                crate::permissions_center::lookup(&actual_tool, &scope_key, &state)
                            {
                                match decision.mode {
                                    crate::permissions_center::DecisionMode::AllowPersistent => {
                                        already_granted = true;
                                        let payload = serde_json::json!({
                                            "tool": actual_tool,
                                            "scope": scope_key,
                                            "approved": true,
                                            "mode": "allow_persistent",
                                            "decision_id": decision.id
                                        });
                                        let _ = bus.send(
                                            EventEnvelope::new(EventType::ToolInvoked, Some(payload))
                                                .with_correlation(timeline_correlation),
                                        );
                                    }
                                    crate::permissions_center::DecisionMode::DenyPersistent => {
                                        tool_results.push(
                                            "Action refusée par la politique d'approbation persistante."
                                                .to_string(),
                                        );
                                        let payload = serde_json::json!({
                                            "tool": actual_tool,
                                            "scope": scope_key,
                                            "approved": false,
                                            "mode": "deny_persistent",
                                            "decision_id": decision.id
                                        });
                                        let _ = bus.send(
                                            EventEnvelope::new(EventType::ToolInvoked, Some(payload))
                                                .with_correlation(timeline_correlation),
                                        );
                                        continue;
                                    }
                                }
                            }
                            if already_granted {
                                // Persistent approval matched: skip interactive prompt.
                            } else {
                            match &human_input_store {
                                Some(store) => {
                                    const APPROVAL_TIMEOUT_SECS: u64 = 300;
                                    // Redact write-like tool args entirely; truncate others to avoid leaking secrets/blobs.
                                    const MAX_APPROVAL_ARG_LEN: usize = 80;
                                    let args_preview: String = if matches!(
                                        actual_tool.as_str(),
                                        "apply_patch" | "edit_file" | "write_file" | "delete_file"
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
                                    let choices = vec![
                                        "Approuver".to_string(),
                                        "Toujours autoriser".to_string(),
                                        "Refuser".to_string(),
                                    ];
                                    let approval_request_id = Uuid::new_v4().to_string();
                                    let now = chrono::Utc::now();
                                    let queue_req = crate::permissions_queue::PermissionQueueRequest {
                                        id: approval_request_id.clone(),
                                        task_id: task_id.to_string(),
                                        tool: actual_tool.clone(),
                                        scope: scope_key.clone(),
                                        action: actual_tool.clone(),
                                        description: format!("{} {}", actual_tool, args_preview),
                                        rationale: format!(
                                            "Outil sensible (confirmation requise) pour la tâche {}",
                                            task_id
                                        ),
                                        urgency: "normal".to_string(),
                                        status: crate::permissions_queue::QueueStatus::Pending,
                                        created_at: now.to_rfc3339(),
                                        updated_at: now.to_rfc3339(),
                                        expires_at: Some(
                                            (now + chrono::Duration::seconds(APPROVAL_TIMEOUT_SECS as i64))
                                                .to_rfc3339(),
                                        ),
                                        decision_note: None,
                                    };
                                    if let Err(err) = crate::permissions_queue::upsert_request(data_dir, queue_req) {
                                        eprintln!(
                                            "failed to persist permission queue request {} for task {} (tool {}): {}",
                                            approval_request_id, task_id, actual_tool, err
                                        );
                                    }
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
                                        "request_id": approval_request_id.clone(),
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
                                        "task_id": task_id.to_string(),
                                        "request_id": approval_request_id.clone()
                                    });
                                    let _ = bus.send(
                                        EventEnvelope::new(
                                            EventType::ToolApprovalRequest,
                                            Some(approval_payload),
                                        )
                                        .with_correlation(task_id),
                                    );
                                    let answer = match tokio::time::timeout(
                                        std::time::Duration::from_secs(APPROVAL_TIMEOUT_SECS),
                                        rx,
                                    )
                                    .await
                                    {
                                        Ok(Ok(reply)) => reply.trim().to_string(),
                                        _ => {
                                            // Timeout or channel error: remove stale pending entry to avoid it staying forever.
                                            {
                                                let mut g = store.write().await;
                                                g.remove(&task_id);
                                            }
                                            let expired_payload = serde_json::json!({
                                                "task_id": task_id.to_string(),
                                                "tool": actual_tool,
                                                "request_id": approval_request_id.clone(),
                                            });
                                            let _ = crate::permissions_queue::update_status(
                                                data_dir,
                                                &approval_request_id,
                                                crate::permissions_queue::QueueStatus::Expired,
                                                Some("timeout".to_string()),
                                            );
                                            let _ = bus.send(
                                                EventEnvelope::new(
                                                    EventType::ToolApprovalExpired,
                                                    Some(expired_payload),
                                                )
                                                .with_correlation(task_id),
                                            );
                                            "Refuser".to_string()
                                        }
                                    };
                                    let granted = answer.eq_ignore_ascii_case("Approuver")
                                        || answer.eq_ignore_ascii_case("Toujours autoriser");
                                    let queue_status = if granted {
                                        crate::permissions_queue::QueueStatus::Approved
                                    } else {
                                        crate::permissions_queue::QueueStatus::Denied
                                    };
                                    if let Err(err) = crate::permissions_queue::update_status(
                                        data_dir,
                                        &approval_request_id,
                                        queue_status,
                                        Some(answer.clone()),
                                    ) {
                                        eprintln!(
                                            "failed to update permission queue status for {} (task {}): {}",
                                            approval_request_id, task_id, err
                                        );
                                    }
                                    if answer.eq_ignore_ascii_case("Toujours autoriser") {
                                        let mut state = crate::permissions_center::load(data_dir);
                                        state.decisions.retain(|d| {
                                            !(d.tool == actual_tool && d.scope == scope_key)
                                        });
                                        state.decisions.push(crate::permissions_center::PermissionDecision {
                                            id: Uuid::new_v4().to_string(),
                                            tool: actual_tool.clone(),
                                            scope: scope_key.clone(),
                                            mode: crate::permissions_center::DecisionMode::AllowPersistent,
                                            created_at: chrono::Utc::now().to_rfc3339(),
                                            expires_at: None,
                                        });
                                        let _ = crate::permissions_center::save(data_dir, &state);
                                    }
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
                                if code_studio_disk_task
                                    && !assigned_agent.eq_ignore_ascii_case("studio_project_manager")
                                {
                                    (
                                        false,
                                        "[delegate_to_agent] réservé à l’agent `studio_project_manager` (chef de projet Code Studio). Les sous-agents implémentent directement avec read_file / write_file / …"
                                            .to_string(),
                                        None,
                                    )
                                } else {
                                    match &delegation_tx {
                                        Some(tx) => {
                                            let (reply_tx, reply_rx) = oneshot::channel();
                                            let agent_type = args
                                                .get(0)
                                                .cloned()
                                                .unwrap_or_else(|| "conversation".to_string());
                                            let message = args
                                                .get(1..)
                                                .map(|a| a.join(" "))
                                                .unwrap_or_else(|| args.get(0).cloned().unwrap_or_default());
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
                        // Phase F: emit ToolInvoked for Actions tab (spec 33)
                        // Redact or truncate args in the event to avoid leaking large blobs or secrets.
                        let redacted_args: Vec<String> = if matches!(
                            actual_tool.as_str(),
                            "apply_patch" | "edit_file" | "write_file" | "delete_file"
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
                        // Chat UI loads map / rich views from timeline_milestone + result_full.
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
                            if code_studio_disk_task
                                && matches!(
                                    actual_tool.as_str(),
                                    "write_file"
                                        | "delete_file"
                                        | "rename_path"
                                        | "move_tree"
                                        | "search_replace"
                                        | "edit_file"
                                        | "apply_patch"
                                )
                            {
                                schedule_code_studio_index_for_root(
                                    store_path.as_path(),
                                    tool_disk_workspace_root.as_path(),
                                );
                            }
                            if actual_tool.eq_ignore_ascii_case("read_file") {
                                let (path_tokens, explicit_window, want_full) =
                                    parse_read_file_args(&tool_args);
                                if !want_full && explicit_window.is_none() {
                                    let path_input = path_tokens.join(" ");
                                    let path_str = normalize_tool_path_hint(&path_input);
                                    let was_truncated = res.contains("[read_file")
                                        && !path_str.is_empty()
                                        && (res.contains("(truncated,")
                                            || res.contains(READ_FILE_PARTIAL_DEFAULT_MARKER));
                                    if was_truncated {
                                        truncated_read_file_paths_by_agent
                                            .entry(loop_agent_key.clone())
                                            .or_default()
                                            .insert(path_str);
                                    }
                                }
                            }
                            log_tool_journal_if_write(&actual_tool, tool_args, &res).await;
                        }
                        tool_results.push(res);
                    }
                }
                let results_blob = tool_results.join("\n");
                let mut studio_readonly_nudge: Option<String> = None;
                if code_studio_disk_task && !calls.is_empty() {
                    let only_survey = calls.iter().all(|(name, _)| {
                        let c = canonicalize_tool_name(&name.trim().to_string());
                        crate::api_studio::studio_survey_tool(&c)
                    });
                    if only_survey {
                        studio_read_only_streak_rounds =
                            studio_read_only_streak_rounds.saturating_add(1);
                    } else {
                        studio_read_only_streak_rounds = 0;
                    }
                    let max_streak = std::env::var("AKASHA_STUDIO_READ_ONLY_STREAK_MAX")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(4)
                        .max(2)
                        .min(20);
                    if studio_read_only_streak_rounds >= max_streak {
                        studio_read_only_streak_rounds = 0;
                        let _ = bus.send(
                            EventEnvelope::new(
                                EventType::ProgressUpdate,
                                Some(serde_json::json!({
                                    "task_id": task_id.to_string(),
                                    "progress_pct": 52,
                                    "message": "[Étape: garde-fou lecture] Plusieurs tours d’affilée n’ont utilisé que des outils d’exploration — appliquez une modification concrète (write_file / search_replace / edit_file) ou indiquez le blocage exact."
                                })),
                            )
                            .with_correlation(timeline_correlation),
                        );
                        studio_readonly_nudge = Some(format!(
                            "[STUDIO_READ_ONLY_STREAK]\nThe last {} model rounds only invoked read-only survey tools (read_file, list_dir, grep_content, search_files, file_diff, git status/log/diff). The user request requires repository changes. Emit at least one write-like TOOL line in this turn (write_file, search_replace, edit_file, apply_patch) OR answer in plain text with the precise blocking reason (e.g. tool policy forbids writes). Do not repeat another exploratory-only round.",
                            max_streak
                        ));
                    }
                }
                last_tool_results_blob = Some(results_blob.clone());
                if results_blob.contains("[browser] Snapshot") {
                    social_snapshot_seen = true;
                }
                let round_had_ask_user = calls.iter().any(|(name, _)| name == "ask_user");
                let msg_social = compute_message_intent_flags(&message).social_feed_fetch;
                let tool_loop_history = tool_loop_history_by_agent
                    .get(&loop_agent_key)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
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
                if let Some(nudge) = studio_readonly_nudge {
                    current_prompt = format!("{nudge}\n\n{current_prompt}");
                }
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
                    let tool_loop_history = tool_loop_history_by_agent
                        .get(&loop_agent_key)
                        .map(|v| v.as_slice())
                        .unwrap_or(&[]);
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
            const MAX_STUDIO_MANUAL_PATCH_NAGS: u32 = 3;
            if code_studio_disk_task
                && looks_like_manual_file_patch_reply(&response_for_user)
                && tools_executor_snapshot.is_some()
            {
                let (policy_allows_write, write_tool_seen) = tools_executor_snapshot
                    .as_ref()
                    .map(|e| {
                        let tool_loop_history = tool_loop_history_by_agent
                            .get(&loop_agent_key)
                            .map(|v| v.as_slice())
                            .unwrap_or(&[]);
                        let allows = e.policy.can_use_tool("write_file")
                            || e.policy.can_use_tool("edit_file")
                            || e.policy.can_use_tool("search_replace")
                            || e.policy.can_use_tool("apply_patch");
                        let seen = tool_loop_history.iter().any(|(t, _)| {
                            matches!(
                                t.as_str(),
                                "write_file" | "edit_file" | "search_replace" | "apply_patch"
                            )
                        });
                        (allows, seen)
                    })
                    .unwrap_or((false, false));
                if policy_allows_write
                    && !write_tool_seen
                    && studio_manual_patch_nags < MAX_STUDIO_MANUAL_PATCH_NAGS
                {
                    studio_manual_patch_nags += 1;
                    current_prompt = format!(
                        "{}\n\n[Code Studio — mandatory runtime guard]\nYour previous reply asked the user to manually paste file changes. This is not acceptable here while write tools are available.\nEmit TOOL lines now and apply the fix directly on disk using `search_replace`, `edit_file`, `write_file`, or `apply_patch` (workspace:/ paths). Do NOT output manual replacement blocks.\nIf a write tool fails, include the tool error and explain the blocker briefly.",
                        current_prompt
                    );
                    continue;
                }
            }
            // If we already ran tools but the model returned a placeholder ("Je vais… Une seconde."), force one more round to get the actual answer.
            let tool_loop_history = tool_loop_history_by_agent
                .get(&loop_agent_key)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
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
            // Guardrail: when tools already produced results, reject generic greeting responses
            // and force one synthesis round from tool outputs.
            if !tool_loop_history.is_empty()
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

    let mut reply_text = if reply_text.is_empty() {
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
    let is_paused = matches!(
        store.get(task_id),
        Ok(Some(Task {
            status: TaskStatus::Paused,
            ..
        }))
    );
    let mut studio_verify_error: Option<String> = None;
    let mut studio_autofix_applied = false;
    if !is_paused && code_studio_disk_task {
        let max_passes: u32 = if is_session_recall {
            1
        } else {
            std::env::var("AKASHA_STUDIO_VERIFY_MAX_PASSES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3)
                .max(1)
                .min(8)
        };
        for pass in 0..max_passes {
            match crate::api_studio::studio_verify_after_agent_task(&tool_disk_workspace_root).await {
                Ok(()) => {
                    studio_verify_error = None;
                    break;
                }
                Err(e) => {
                    studio_verify_error = Some(e.clone());
                    if pass + 1 >= max_passes {
                        break;
                    }
                    if let Some(ref exec_arc) = tools_executor {
                        let denom = max_passes.saturating_sub(1).max(1);
                        let _ = bus.send(
                            EventEnvelope::new(
                                EventType::ProgressUpdate,
                                Some(serde_json::json!({
                                    "task_id": task_id.to_string(),
                                    "progress_pct": 55,
                                    "message": format!(
                                        "Compilation du projet en échec — correction automatique (tentative {}/{}).",
                                        pass + 1,
                                        denom
                                    )
                                })),
                            )
                            .with_correlation(timeline_correlation),
                        );
                        let llm_rounds = std::env::var("AKASHA_STUDIO_VERIFY_AUTOFIX_LLM_ROUNDS")
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(6)
                            .max(1)
                            .min(16);
                        let data_dir = store_path
                            .parent()
                            .unwrap_or_else(|| store_path.as_path());
                        let did_write = studio_verify_run_llm_autofix_rounds(
                            &bus,
                            &llm_router,
                            exec_arc,
                            skill_registry.as_ref(),
                            plugin_registry.as_ref(),
                            process_registry.as_ref(),
                            conv_tx.clone(),
                            long_term_client.as_ref(),
                            workspace_store.as_ref(),
                            browser_registry.as_ref(),
                            device_bridge.as_ref(),
                            task_id,
                            store_path.as_path(),
                            data_dir,
                            tool_disk_workspace_root.as_path(),
                            &e,
                            llm_rounds,
                        )
                        .await;
                        if did_write {
                            studio_autofix_applied = true;
                        }
                    } else {
                        break;
                    }
                }
            }
        }
        if studio_verify_error.is_none() && studio_autofix_applied {
            reply_text.push_str("\n\n_(Compilation du projet Code Studio corrigée automatiquement après échec du build.)_");
        }
        if studio_verify_error.is_none() {
            if let Some(ref pay) = embedded_studio_acceptance {
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": task_id.to_string(),
                            "progress_pct": 57,
                            "message": "[Étape: critères d'acceptation] Vérification fichiers / commandes configurées…"
                        })),
                    )
                    .with_correlation(timeline_correlation),
                );
                let tmo = crate::api_studio::studio_project_verify_timeout_sec(
                    tool_disk_workspace_root.as_path(),
                );
                let mech = crate::api_studio::run_mechanical_acceptance_checks(
                    tool_disk_workspace_root.as_path(),
                    pay,
                    tmo,
                )
                .await;
                if !mech.is_empty() {
                    studio_verify_error = Some(format!(
                        "Échec critères d'acceptation (vérification automatique) :\n{}",
                        mech.join("\n")
                    ));
                } else {
                    let manual_lines: Vec<String> = pay
                        .criteria
                        .iter()
                        .filter(|c| matches!(c.kind, crate::api_studio::StudioCriterionKind::Manual))
                        .map(|c| {
                            let id = if c.id.is_empty() { "-" } else { c.id.as_str() };
                            format!("{id}: {}", c.text)
                        })
                        .collect();
                    if !manual_lines.is_empty() {
                        let _ = bus.send(
                            EventEnvelope::new(
                                EventType::ProgressUpdate,
                                Some(serde_json::json!({
                                    "task_id": task_id.to_string(),
                                    "progress_pct": 59,
                                    "message": "[Étape: contrôle critères] Analyse des critères manuels (après build)…"
                                })),
                            )
                            .with_correlation(timeline_correlation),
                        );
                        let data_dir = store_path.parent().unwrap_or_else(|| store_path.as_path());
                        let diff_summary: String = {
                            let snap_path = data_dir
                                .join("studio-task-snapshots")
                                .join(format!("{task_id}.json"));
                            let snap_json = std::fs::read_to_string(&snap_path).unwrap_or_default();
                            if snap_json.is_empty() {
                                String::new()
                            } else if let Ok(snap) = serde_json::from_str::<
                                crate::studio_task_snapshot::StudioTaskSnapshot,
                            >(&snap_json)
                            {
                                match tokio::task::spawn_blocking(move || {
                                    crate::studio_task_snapshot::compute_studio_task_diff_from_snapshot(
                                        snap,
                                    )
                                })
                                .await
                                {
                                    Ok(Ok(entries)) => {
                                        let mut parts = Vec::new();
                                        for e in entries.iter().take(24) {
                                            parts.push(format!(
                                                "{} [{}]: {}",
                                                e.path,
                                                e.status,
                                                e.diff.chars().take(900).collect::<String>()
                                            ));
                                        }
                                        parts.join("\n")
                                    }
                                    _ => String::new(),
                                }
                            } else {
                                String::new()
                            }
                        };
                        if let Some(missing) = studio_semantic_acceptance_review(
                            &llm_router,
                            &manual_lines,
                            &diff_summary,
                            clean_message,
                            &reply_text,
                        )
                        .await
                        {
                            if !missing.is_empty() {
                                reply_text.push_str(
                                    "\n\n---\n**Revue critères (non bloquant)** — à vérifier manuellement :\n- ",
                                );
                                reply_text.push_str(&missing.join("\n- "));
                                if let Ok(store_ev) = TaskStore::open(&store_path) {
                                    let _ = store_ev.insert_event(
                                        task_id,
                                        "studio_acceptance_review",
                                        Some(&serde_json::json!({ "missing": missing })),
                                        &chrono::Utc::now().to_rfc3339(),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
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
    if let Some(reg) = &browser_registry {
        crate::browser::close_task(reg, task_id).await;
    }
    // Release per-task workspace memory immediately — no longer needed once the task finishes.
    if let Some(ws) = &workspace_store {
        ws.write().await.remove(&task_id);
    }

    let verify_user_lang = studio_verify_detect_user_language(clean_message);
    let studio_verify_display_message: Option<String> = if let Some(ref err) = studio_verify_error {
        let explain_enabled = code_studio_disk_task
            && std::env::var("AKASHA_STUDIO_VERIFY_EXPLAIN_FAILURE")
                .ok()
                .map(|v| v != "0")
                .unwrap_or(true);
        let explain = if explain_enabled {
            let _ = bus.send(
                EventEnvelope::new(
                    EventType::ProgressUpdate,
                    Some(serde_json::json!({
                        "task_id": task_id.to_string(),
                        "progress_pct": 99,
                        "message": studio_verify_analyzing_progress_line(verify_user_lang)
                    })),
                )
                .with_correlation(task_id),
            );
            studio_verify_explain_failure_to_user(
                &llm_router,
                clean_message,
                &reply_text,
                err,
                verify_user_lang,
            )
            .await
        } else {
            None
        };
        let summary = explain.filter(|s| !s.trim().is_empty());
        let banner = studio_verify_failure_banner(verify_user_lang);
        let excerpt_lbl = studio_verify_build_excerpt_label(verify_user_lang);
        Some(if let Some(ref s) = summary {
            let head = studio_verify_summary_heading_markdown(verify_user_lang);
            if head.is_empty() {
                format!(
                    "{banner}\n\n{}\n\n{excerpt_lbl}\n{}",
                    s.trim(),
                    err.chars().take(1_400).collect::<String>()
                )
            } else {
                format!(
                    "{banner}\n\n{head}{}\n\n{excerpt_lbl}\n{}",
                    s.trim(),
                    err.chars().take(1_400).collect::<String>()
                )
            }
        } else {
            format!(
                "{banner}\n{}",
                err.chars().take(1_800).collect::<String>()
            )
        })
    } else {
        None
    };

    if let Some(ref pm) = studio_verify_display_message {
        let _ = bus.send(
            EventEnvelope::new(
                EventType::ProgressUpdate,
                Some(serde_json::json!({
                    "task_id": task_id.to_string(),
                    "progress_pct": 100,
                    "message": pm.chars().take(6_000).collect::<String>()
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

    let mut final_payload = serde_json::json!({
        "task_id": task_id.to_string(),
        "status": final_status_str,
        "model_used": last_llm_model_used
    });
    if studio_verify_error.is_some() {
        if let Some(obj) = final_payload.as_object_mut() {
            let reason_text: String = studio_verify_display_message
                .as_ref()
                .map(|m| m.chars().take(2_000).collect::<String>())
                .unwrap_or_else(|| {
                    studio_verify_error
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(2_000)
                        .collect::<String>()
                });
            obj.insert("reason".to_string(), serde_json::Value::String(reason_text));
        }
    }
    let _ = bus.send(
        EventEnvelope::new(final_event_type, Some(final_payload)).with_correlation(task_id),
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
            let summary_preview: String = if let Some(ref m) = studio_verify_display_message {
                m.chars().take(300).collect()
            } else if let Some(ref e) = studio_verify_error {
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
    tools_executor: Option<
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

    if let Some(resp) = crate::api_security::csrf_reject_response(method, path, headers) {
        return resp;
    }

    let (path_only, query_str) = crate::api_security::split_path_query(path);
    let _http_post_lifecycle = crate::lifecycle_hooks::HttpPostLifecycleHooks {
        data_dir: data_dir.to_path_buf(),
        method: method.to_string(),
        path: path_only.to_string(),
    };
    crate::lifecycle_hooks::run_http_request_pre_hooks(data_dir, method, path_only).await;

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

    if let Some(resp) = crate::api_routes_automation::handle_automation_routes(
        method,
        path_only,
        body.as_deref(),
        headers,
        data_dir,
    )
    .await
    {
        return resp;
    }

    if method == "GET" && path_only == "/api/process/watch/recent" {
        let limit = crate::api_security::parse_query_param(query_str, "limit")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(50);
        let ev = crate::process_watch::recent(limit).await;
        return json_response(
            "200 OK",
            &serde_json::to_string(&ev).unwrap_or_else(|_| "[]".to_string()),
        );
    }

    if let Some(resp) = crate::api_routes_terminal::handle_terminal_routes(
        method,
        path_only,
        query_str,
        body.as_deref(),
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_mcp::handle_mcp_lifecycle_routes(
        method,
        path_only,
        body.as_deref(),
        data_dir,
    )
    .await
    {
        return resp;
    }

    if let Some(resp) = crate::api_routes_mission::handle_mission_routes(
        method,
        path_only,
        query_str,
        body.as_deref(),
        data_dir,
        store_path,
        autonomous_mission.clone(),
    )
    .await
    {
        return resp;
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

    // GET /api/session/resume-brief?session_id=… — Hermes-like resume UX: structured state + short-term size.
    if method == "GET" && path.starts_with("/api/session/resume-brief") {
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
                r#"{"error":"session_id query_parameter_required"}"#,
            );
        }
        let st = crate::session_state::load(data_dir, &session_id);
        let (short_term_turns, compaction_count) = match &short_term {
            Some(st_mem) => {
                let n = st_mem.get_turns(&session_id).await.len();
                let c = st_mem.get_compaction_count(&session_id).await;
                (n, c)
            }
            None => (0usize, 0u32),
        };
        let tools_policy_brief = if let Some(exec_lock) = tools_executor {
            let g = exec_lock.read().await;
            let p = &g.policy;
            serde_json::json!({
                "default_profile": p.default_profile,
                "web_search_enabled": p.web_search_enabled,
                "browser_enabled": p.browser_enabled,
                "web_crawl_enabled": p.web_crawl_enabled,
                "cloudflare_account_configured": p.resolved_cloudflare_account_id().is_some(),
                "cloudflare_token_configured": p.resolved_cloudflare_api_token().is_some(),
            })
        } else {
            serde_json::Value::Null
        };
        return json_response(
            "200 OK",
            &serde_json::to_string(&serde_json::json!({
                "session_id": session_id,
                "session_state": st,
                "short_term_turn_count": short_term_turns,
                "compaction_count": compaction_count,
                "memory_recall_metrics": crate::memory_orchestrator::memory_recall_metrics_snapshot(),
                "tools_policy_brief": tools_policy_brief,
                "hint": "Use this payload to restore UI tabs (goals, constraints) and to explain context to the user after reconnect."
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

    if let Some(resp) = crate::api_routes_profiles::handle_profiles_routes(
        method,
        path_only,
        query_str,
        body.as_deref(),
        data_dir,
        agent_profile_cache,
        short_term.clone(),
        long_term_client.clone(),
    )
    .await
    {
        return resp;
    }

    // Second brain controls: settings, overview and clear.
    if method == "GET" && path == "/api/memory/second-brain/settings" {
        let settings = load_second_brain_settings(data_dir);
        let body = serde_json::json!({ "settings": settings });
        return json_response("200 OK", &body.to_string());
    }
    if method == "POST" && path == "/api/memory/second-brain/settings" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let mut settings = load_second_brain_settings(data_dir);
        if let Some(enabled) = body_json
            .as_ref()
            .and_then(|v| v.get("enabled").and_then(|b| b.as_bool()))
        {
            settings.enabled = enabled;
        }
        if let Some(paused) = body_json
            .as_ref()
            .and_then(|v| v.get("paused").and_then(|b| b.as_bool()))
        {
            settings.paused = paused;
        }
        match save_second_brain_settings(data_dir, &settings) {
            Ok(()) => {
                return json_response(
                    "200 OK",
                    &serde_json::json!({ "ok": true, "settings": settings }).to_string(),
                )
            }
            Err(e) => {
                return json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error":"save_failed", "detail": e.to_string() }).to_string(),
                )
            }
        }
    }
    if method == "GET" && path.starts_with("/api/memory/second-brain/overview") {
        let settings = load_second_brain_settings(data_dir);
        let (entries, total) = if let Some(ref client) = long_term_client {
            let client = client.clone();
            tokio::task::spawn_blocking(move || client.list(500, 0))
                .await
                .unwrap_or((vec![], 0))
        } else {
            (vec![], 0)
        };
        let mut by_type: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
        for (_, _, source, _) in entries {
            let t = if source.contains("preference") {
                "preference"
            } else if source.contains("goal") {
                "goal"
            } else if source.contains("project") {
                "project"
            } else if source.contains("decision") {
                "decision"
            } else if source.contains("constraint") {
                "constraint"
            } else if source.contains("identity") || source.contains("user_fact") {
                "identity"
            } else {
                "other"
            };
            *by_type.entry(t.to_string()).or_insert(0) += 1;
        }
        let body = serde_json::json!({
            "settings": settings,
            "total_entries": total,
            "typed_counts": by_type
        });
        return json_response("200 OK", &body.to_string());
    }
    if method == "POST" && path == "/api/memory/second-brain/clear" {
        let mut deleted = 0u64;
        if let Some(ref client) = long_term_client {
            let client = client.clone();
            deleted = tokio::task::spawn_blocking(move || {
                let mut removed = 0u64;
                loop {
                    let (rows, _total) = client.list(200, 0);
                    if rows.is_empty() {
                        break;
                    }
                    for (id, _content, _source, _created) in rows {
                        if client.delete(id).is_ok() {
                            removed += 1;
                        }
                    }
                }
                removed
            })
            .await
            .unwrap_or(0);
        }
        let body = serde_json::json!({ "ok": true, "deleted_entries": deleted });
        return json_response("200 OK", &body.to_string());
    }

    // GET /api/memory/recall-metrics — counters from memory orchestrator (semantic recall hits/empty).
    if method == "GET" && path == "/api/memory/recall-metrics" {
        let body = crate::memory_orchestrator::memory_recall_metrics_snapshot();
        return json_response(
            "200 OK",
            &serde_json::to_string(&body).unwrap_or_else(|_| "{}".into()),
        );
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
            let ts = headers.get("x-slack-request-timestamp").map(String::as_str);
            return crate::channels::slack::handle_slack_command(
                body,
                sig,
                ts,
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
            ).await;
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
        let mut session_id = {
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
        let studio_code_mode = body_json
            .as_ref()
            .and_then(|v| v.get("studio_code_mode").and_then(|x| x.as_str()))
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        let message_delivery_mode = body_json
            .as_ref()
            .and_then(|v| v.get("message_delivery_mode").and_then(|x| x.as_str()))
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        let studio_policy_hint = body_json
            .as_ref()
            .and_then(|v| v.get("studio_policy_hint").and_then(|x| x.as_str()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let studio_design_hint = body_json
            .as_ref()
            .and_then(|v| v.get("studio_design_hint").and_then(|x| x.as_str()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let studio_design_doc = body_json
            .as_ref()
            .and_then(|v| v.get("studio_design_doc").and_then(|x| x.as_str()))
            .map(|s| s.to_string())
            .filter(|s| !s.trim().is_empty());
        let studio_acceptance_parsed: Option<crate::api_studio::StudioAcceptancePayload> =
            match body_json
                .as_ref()
                .and_then(|v| v.get("studio_acceptance_criteria"))
            {
                Some(v) => match crate::api_studio::parse_api_acceptance_field(v) {
                    Ok(p) => p,
                    Err(e) => {
                        let body = serde_json::json!({
                            "error": "invalid_studio_acceptance_criteria",
                            "detail": e
                        });
                        return json_response("400 Bad Request", &body.to_string());
                    }
                },
                None => None,
            };
        let studio_delegate_single_level = body_json
            .as_ref()
            .and_then(|v| v.get("studio_delegate_single_level").and_then(|x| x.as_bool()))
            .unwrap_or(false);
        let studio_project_id = body_json
            .as_ref()
            .and_then(|v| v.get("studio_project_id").and_then(|x| x.as_str()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let fork_from_task_id: Option<Uuid> = {
            let raw = body_json
                .as_ref()
                .and_then(|v| v.get("fork_from_task_id").and_then(|x| x.as_str()))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            match raw {
                None => None,
                Some(s) => match Uuid::parse_str(&s) {
                    Ok(id) => Some(id),
                    Err(_) => {
                        let body = serde_json::json!({ "error": "invalid_fork_from_task_id", "detail": "fork_from_task_id must be a valid UUID" });
                        return json_response("400 Bad Request", &body.to_string());
                    }
                },
            }
        };
        let fork_after_message_index = body_json
            .as_ref()
            .and_then(|v| v.get("fork_after_message_index").and_then(|x| x.as_i64()))
            .filter(|n| *n >= 0)
            .map(|n| n as u64);
        let mut fork_meta_for_task: Option<serde_json::Value> = None;
        if let Some(parent_task_id) = fork_from_task_id.as_ref() {
            let parent_session_id = session_id.clone();
            let fork_session_id = format!("fork-{}", Uuid::new_v4().simple());
            if let Some(st) = short_term.as_ref() {
                let parent_turns = st.get_turns(&parent_session_id).await;
                let keep = fork_after_message_index
                    .map(|n| (n as usize).saturating_add(1))
                    .unwrap_or(parent_turns.len())
                    .min(parent_turns.len());
                for turn in parent_turns.into_iter().take(keep) {
                    st.append(&fork_session_id, &turn.role, turn.content).await;
                }
                fork_meta_for_task = Some(serde_json::json!({
                    "fork_parent_task_id": parent_task_id.to_string(),
                    "fork_parent_session_id": parent_session_id,
                    "fork_session_id": fork_session_id,
                    "fork_cut_index": fork_after_message_index,
                    "fork_cut_turns": keep,
                    "schema_version": 1
                }));
            } else {
                fork_meta_for_task = Some(serde_json::json!({
                    "fork_parent_task_id": parent_task_id.to_string(),
                    "fork_parent_session_id": parent_session_id,
                    "fork_session_id": fork_session_id,
                    "fork_cut_index": fork_after_message_index,
                    "fork_cut_turns": 0usize,
                    "schema_version": 1,
                    "note": "short_term_store_unavailable"
                }));
            }
            session_id = fork_session_id;
        }
        let studio_disk_root = if let Some(pid) = studio_project_id.as_deref()
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
        let studio_ui_agent_preference = body_json
            .as_ref()
            .and_then(|v| v.get("studio_assigned_agent").and_then(|x| x.as_str()))
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        // Code Studio : toujours router vers le chef de projet ; la valeur UI devient une préférence pour les sous-agents.
        let studio_forced_agent = if studio_disk_root.is_some() {
            Some("studio_project_manager".to_string())
        } else {
            studio_ui_agent_preference.clone()
        };
        let mut studio_evolution_branch = body_json
            .as_ref()
            .and_then(|v| v.get("studio_evolution_branch").and_then(|x| x.as_str()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if studio_evolution_branch.is_none() {
            if let (Some(pid), Some(eid)) = (
                studio_project_id.as_deref(),
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
        // Capture the raw user message for code-RAG retrieval before any prefixes are injected.
        let raw_user_message = message.clone();
        let mut message_for_llm = message;
        if let Some(mode) = message_delivery_mode.as_deref() {
            if mode == "steering" || mode == "follow_up" {
                message_for_llm = format!(
                    "[Delivery mode hint: `{mode}`. Prefer coherent continuation with the current session state.]\n\n{}",
                    message_for_llm
                );
            }
        }
        if let Some(parent_task_id) = fork_from_task_id.as_ref() {
            let cut = fork_after_message_index
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            message_for_llm = format!(
                "[Session fork context] Parent task: {parent_task_id}; cut index: {cut}. Continue from this branch only.\n\n{}",
                message_for_llm
            );
        }
        if let Some(ref root) = studio_disk_root {
            if let Some(plan) = crate::api_studio::studio_code_plan_message_prefix(root) {
                message_for_llm = format!("{plan}{message_for_llm}");
            }
            let (evol_prefix, policy_prefix, tech_prefix) =
                crate::api_studio::studio_meta_prefixes(root);
            if let Some(p) = evol_prefix {
                message_for_llm = format!("{p}{message_for_llm}");
            }
            if let Some(p) = policy_prefix {
                message_for_llm = format!("{p}{message_for_llm}");
            }
            if studio_evolution_branch.is_some() {
                message_for_llm = format!(
                    "[Évolution Code Studio — conserver le même périmètre produit et le même type d’application que le dépôt (cf. CODE_STUDIO_PLAN.md ci-dessus et code existant) ; ne pas remplacer par un autre jeu, une autre app ou un autre domaine fonctionnel sauf instruction explicite de l’utilisateur.]\n\n{}",
                    message_for_llm
                );
            }
            if let Some(prefix) = tech_prefix {
                message_for_llm = format!("{prefix}{message_for_llm}");
            }
            if let Some(ref m) = studio_code_mode {
                if let Some(p) = crate::api_studio::studio_code_mode_message_prefix(m) {
                    message_for_llm = format!("{p}{message_for_llm}");
                }
            }
            if let Some(ref h) = studio_policy_hint {
                if let Some(p) = crate::api_studio::studio_one_shot_policy_hint_prefix(h) {
                    message_for_llm = format!("{p}{message_for_llm}");
                }
            }
            if let Some(ref h) = studio_design_hint {
                if let Some(p) = crate::api_studio::studio_design_hint_prefix(h) {
                    message_for_llm = format!("{p}{message_for_llm}");
                }
            }
            if let Some(ref d) = studio_design_doc {
                if let Some(p) = crate::api_studio::studio_design_doc_prefix(d) {
                    message_for_llm = format!("{p}{message_for_llm}");
                }
            }
            if studio_code_rag_enabled() {
                if let Some(pid) = studio_project_id.as_deref() {
                    let query = raw_user_message.clone();
                    let data_dir = data_dir.to_path_buf();
                    let root = root.clone();
                    let pid = pid.to_string();
                    let top_k = std::env::var("AKASHA_STUDIO_CODE_RAG_TOP_K")
                        .ok()
                        .and_then(|s| s.parse::<usize>().ok())
                        .filter(|&n| n > 0 && n <= 30)
                        .unwrap_or(8);
                    let max_chars = std::env::var("AKASHA_STUDIO_CODE_RAG_MAX_CHARS")
                        .ok()
                        .and_then(|s| s.parse::<usize>().ok())
                        .filter(|&n| n >= 800 && n <= 30_000)
                        .unwrap_or(6_000);
                    let code_ctx = tokio::task::spawn_blocking(move || {
                        let store = crate::code_rag::CodeRagStore::new(&data_dir);
                        let chunks = store.retrieve(
                            &pid,
                            &root,
                            &query,
                            crate::code_rag::RetrieveOptions { top_k, max_chars },
                        )?;
                        Ok::<_, anyhow::Error>(crate::code_rag::format_retrieved_chunks(
                            &chunks, max_chars,
                        ))
                    })
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .flatten();
                    if let Some(prefix) = code_ctx {
                        message_for_llm = format!("{prefix}{message_for_llm}");
                    }
                }
            }
            if studio_delegate_single_level {
                message_for_llm = format!(
                    "[Délégation : privilégier une seule passe agent — éviter les sous-agents ou tâches parallèles implicites sans accord utilisateur.]\n\n{}",
                    message_for_llm
                );
            }
            if let Some(pref) = studio_ui_agent_preference.as_ref() {
                if !pref.is_empty() && pref != "studio_project_manager" {
                    message_for_llm = format!(
                        "[Préférence d’implémentation (sélection UI Code Studio) : `{pref}` — en déléguant via `delegate_to_agent`, oriente les sous-tâches vers le spécialiste le plus adapté (ex. studio_frontend, studio_backend, studio_fullstack, studio_scaffold, qa).]\n\n{}",
                        message_for_llm
                    );
                }
            }
            if let Some(ref pay) = studio_acceptance_parsed {
                let prefix = crate::api_studio::format_acceptance_prefix_for_llm(pay);
                if !prefix.is_empty() {
                    message_for_llm = format!("{prefix}{message_for_llm}");
                }
                if let Ok(embed) = serde_json::to_string(pay) {
                    message_for_llm.push_str(crate::api_studio::STUDIO_ACCEPTANCE_JSON_BEGIN);
                    message_for_llm.push_str(&embed);
                    message_for_llm.push_str(crate::api_studio::STUDIO_ACCEPTANCE_JSON_END);
                }
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
                if let Some(meta) = fork_meta_for_task {
                    if let Ok(task_store) = TaskStore::open(store_path) {
                        let _ = task_store.insert_event(
                            task_id,
                            "session_fork_created",
                            Some(&meta),
                            &chrono::Utc::now().to_rfc3339(),
                        );
                    }
                }
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
                if method == "GET" && parts.get(1) == Some(&"report") {
                    return get_task_report(store_path, events, id).await;
                }
                if method == "GET" && parts.get(1) == Some(&"studio-diff") {
                    return get_task_studio_diff(store_path, id).await;
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
    // POST /api/schedules/{id}/pause|resume|run_now — Hermes-like job ops (enabled flag + manual fire).
    if method == "POST" && path.starts_with("/api/schedules/") {
        let rest = path.trim_start_matches("/api/schedules/");
        let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if parts.len() == 2 {
            if let Ok(id) = Uuid::parse_str(parts[0]) {
                let resp = match parts[1] {
                    "pause" => Some(schedule_set_enabled(store_path, id, false).await),
                    "resume" => Some(schedule_set_enabled(store_path, id, true).await),
                    "run_now" | "run-now" => Some(schedule_run_now(store_path, main_agent, id).await),
                    _ => None,
                };
                if let Some(r) = resp {
                    return r;
                }
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
    if method == "GET" && path == "/api/plugins/metrics" {
        let m = crate::plugins::metrics::snapshot();
        return json_response(
            "200 OK",
            &serde_json::to_string(&m).unwrap_or_else(|_| "{}".to_string()),
        );
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
            let tools_executor_snapshot = if let Some(exec_lock) = tools_executor {
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
            "routing_rules_deprecated": true,
            "routing_rules_note": "Manifest routing_rules are no longer enforced at runtime. Plugin hints are chosen via a short system LLM call from plugin descriptions; this endpoint remains for debugging legacy manifests.",
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
        let tools_executor_snapshot = if let Some(exec_lock) = tools_executor {
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
            "routing_rules_deprecated": true,
            "routing_rules_note": "Manifest routing_rules are no longer enforced at runtime. Plugin hints are chosen via a short system LLM call from plugin descriptions; this endpoint remains for debugging legacy manifests.",
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

    // POST /api/plugins/{id}/disable | /enable | /uninstall — user plugin management
    if method == "POST" && path.starts_with("/api/plugins/") {
        if let Some(rest) = path.strip_prefix("/api/plugins/") {
            let segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
            if segs.len() == 2 {
                let plugin_id = segs[0];
                let action = segs[1];
                if !akasha_plugin_api::is_safe_plugin_id(plugin_id) {
                    let body = serde_json::json!({ "error": "invalid_plugin_id" });
                    return json_response("400 Bad Request", &body.to_string());
                }
                let maybe_result = match action {
                    "disable" => Some(plugin_registry.set_enabled(plugin_id, false)),
                    "enable" => Some(plugin_registry.set_enabled(plugin_id, true)),
                    "uninstall" => Some(plugin_registry.uninstall(plugin_id)),
                    _ => None,
                };
                if let Some(result) = maybe_result {
                    match result {
                        Ok(()) => {
                            let body = serde_json::json!({
                                "ok": true,
                                "id": plugin_id,
                                "action": action,
                            });
                            return json_response("200 OK", &body.to_string());
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            let body = serde_json::json!({
                                "error": "not_found",
                                "detail": e.to_string(),
                            });
                            return json_response("404 Not Found", &body.to_string());
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {
                            let body = serde_json::json!({
                                "error": "invalid_plugin_id",
                                "detail": e.to_string(),
                            });
                            return json_response("400 Bad Request", &body.to_string());
                        }
                        Err(e) => {
                            let body = serde_json::json!({
                                "error": "plugin_action_failed",
                                "detail": e.to_string(),
                            });
                            return json_response("500 Internal Server Error", &body.to_string());
                        }
                    }
                }
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
                let tools_reload = tools_executor.map(|r| (r, policy_path.as_path()));
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
                let allowed_hosts = if let Some(exec_lock) = tools_executor {
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
                let tools_reload = tools_executor.map(|r| (r, policy_path.as_path()));
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
    if method == "GET" && (path == "/api/tools" || path_only == "/api/tools") {
        let list: Vec<serde_json::Value> = AVAILABLE_TOOLS
            .iter()
            .map(|(name, desc)| serde_json::json!({ "name": name, "description": desc }))
            .collect();
        let body = serde_json::to_string(&serde_json::json!({ "tools": list }))
            .unwrap_or_else(|_| "{}".to_string());
        return json_response("200 OK", &body);
    }

    // Effective tool policy (Hermes-like toolsets visibility): allowed / approval / rule sources.
    if method == "GET" && path_only == "/api/tools/effective" {
        match tools_executor {
            Some(ex_arc) => {
                let ex_inner = ex_arc.read().await;
                let executor = ex_inner.as_ref();
                let names: Vec<&str> = AVAILABLE_TOOLS.iter().map(|(n, _)| *n).collect();
                let rows = executor.policy.effective_tool_rows(&names);
                let body = serde_json::to_string(&serde_json::json!({
                    "tools": rows,
                    "default_profile": executor.policy.default_profile,
                }))
                .unwrap_or_else(|_| "{}".to_string());
                return json_response("200 OK", &body);
            }
            None => {
                return json_response(
                    "503 Service Unavailable",
                    r#"{"error":"tools_executor_unavailable"}"#,
                );
            }
        }
    }

    // Telegram channel access (pairing + RBAC lifecycle)
    if method == "GET" && path_only == "/api/channel-access/telegram/users" {
        let state = crate::channel_access::load(data_dir);
        let body = serde_json::json!({
            "approved": state.approved,
            "pending": state.pending
        });
        return json_response("200 OK", &body.to_string());
    }
    if method == "POST" && path_only == "/api/channel-access/telegram/request" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let user_id = body_json
            .as_ref()
            .and_then(|v| v.get("user_id").and_then(|n| n.as_i64()))
            .unwrap_or_default();
        if user_id == 0 {
            return json_response("400 Bad Request", r#"{"error":"missing_user_id"}"#);
        }
        let username = body_json
            .as_ref()
            .and_then(|v| v.get("username").and_then(|s| s.as_str()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let mut state = crate::channel_access::load(data_dir);
        if crate::channel_access::is_approved_user(&state, user_id) {
            return json_response("200 OK", r#"{"ok":true,"already_approved":true}"#);
        }
        state.pending.retain(|p| p.user_id != user_id);
        let code = format!("TG-{}", Uuid::new_v4().to_string()[..8].to_uppercase());
        state.pending.push(crate::channel_access::TelegramPending {
            user_id,
            username,
            pairing_code: code.clone(),
            requested_at: chrono::Utc::now().to_rfc3339(),
        });
        if let Err(e) = crate::channel_access::save(data_dir, &state) {
            tracing::error!(error = %e, "channel-access: failed to persist pairing request");
            return json_response("500 Internal Server Error", r#"{"error":"persistence_error"}"#);
        }
        let body = serde_json::json!({ "ok": true, "pairing_code": code });
        return json_response("200 OK", &body.to_string());
    }
    if method == "POST" && path_only == "/api/channel-access/telegram/approve" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let code = body_json
            .as_ref()
            .and_then(|v| v.get("pairing_code").and_then(|s| s.as_str()))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let user_id = body_json
            .as_ref()
            .and_then(|v| v.get("user_id").and_then(|n| n.as_i64()))
            .unwrap_or_default();
        let mut state = crate::channel_access::load(data_dir);
        let pending = if !code.is_empty() {
            let idx = state.pending.iter().position(|p| p.pairing_code == code);
            idx.map(|i| state.pending.remove(i))
        } else if user_id != 0 {
            let idx = state.pending.iter().position(|p| p.user_id == user_id);
            idx.map(|i| state.pending.remove(i))
        } else {
            None
        };
        let Some(pending) = pending else {
            return json_response("404 Not Found", r#"{"error":"pending_not_found"}"#);
        };
        let is_first = state.approved.is_empty();
        state.approved.retain(|u| u.user_id != pending.user_id);
        state.approved.push(crate::channel_access::TelegramUser {
            user_id: pending.user_id,
            username: pending.username,
            role: if is_first {
                crate::channel_access::TelegramRole::Admin
            } else {
                crate::channel_access::TelegramRole::Member
            },
            approved_at: chrono::Utc::now().to_rfc3339(),
        });
        if let Err(e) = crate::channel_access::save(data_dir, &state) {
            tracing::error!(error = %e, "channel-access: failed to persist approve");
            return json_response("500 Internal Server Error", r#"{"error":"persistence_error"}"#);
        }
        return json_response("200 OK", r#"{"ok":true}"#);
    }
    if method == "POST" && path_only == "/api/channel-access/telegram/reject" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let user_id = body_json
            .as_ref()
            .and_then(|v| v.get("user_id").and_then(|n| n.as_i64()))
            .unwrap_or_default();
        if user_id == 0 {
            return json_response("400 Bad Request", r#"{"error":"missing_user_id"}"#);
        }
        let mut state = crate::channel_access::load(data_dir);
        let before = state.pending.len();
        state.pending.retain(|p| p.user_id != user_id);
        if let Err(e) = crate::channel_access::save(data_dir, &state) {
            tracing::error!(error = %e, "channel-access: failed to persist reject");
            return json_response("500 Internal Server Error", r#"{"error":"persistence_error"}"#);
        }
        let removed = before != state.pending.len();
        return json_response(
            "200 OK",
            &serde_json::json!({ "ok": true, "removed": removed }).to_string(),
        );
    }
    if method == "POST" && path_only == "/api/channel-access/telegram/remove" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let user_id = body_json
            .as_ref()
            .and_then(|v| v.get("user_id").and_then(|n| n.as_i64()))
            .unwrap_or_default();
        if user_id == 0 {
            return json_response("400 Bad Request", r#"{"error":"missing_user_id"}"#);
        }
        let mut state = crate::channel_access::load(data_dir);
        let before = state.approved.len();
        state.approved.retain(|u| u.user_id != user_id);
        if let Err(e) = crate::channel_access::save(data_dir, &state) {
            tracing::error!(error = %e, "channel-access: failed to persist remove");
            return json_response("500 Internal Server Error", r#"{"error":"persistence_error"}"#);
        }
        let removed = before != state.approved.len();
        return json_response(
            "200 OK",
            &serde_json::json!({ "ok": true, "removed": removed }).to_string(),
        );
    }
    if method == "POST" && path_only == "/api/channel-access/telegram/promote" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let user_id = body_json
            .as_ref()
            .and_then(|v| v.get("user_id").and_then(|n| n.as_i64()))
            .unwrap_or_default();
        if user_id == 0 {
            return json_response("400 Bad Request", r#"{"error":"missing_user_id"}"#);
        }
        let mut state = crate::channel_access::load(data_dir);
        if let Some(u) = state.approved.iter_mut().find(|u| u.user_id == user_id) {
            u.role = crate::channel_access::TelegramRole::Admin;
            if let Err(e) = crate::channel_access::save(data_dir, &state) {
                tracing::error!(error = %e, "channel-access: failed to persist promote");
                return json_response("500 Internal Server Error", r#"{"error":"persistence_error"}"#);
            }
            return json_response("200 OK", r#"{"ok":true}"#);
        }
        return json_response("404 Not Found", r#"{"error":"user_not_found"}"#);
    }
    if method == "POST" && path_only == "/api/channel-access/telegram/demote" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let user_id = body_json
            .as_ref()
            .and_then(|v| v.get("user_id").and_then(|n| n.as_i64()))
            .unwrap_or_default();
        if user_id == 0 {
            return json_response("400 Bad Request", r#"{"error":"missing_user_id"}"#);
        }
        let mut state = crate::channel_access::load(data_dir);
        if let Some(u) = state.approved.iter_mut().find(|u| u.user_id == user_id) {
            u.role = crate::channel_access::TelegramRole::Member;
            if let Err(e) = crate::channel_access::save(data_dir, &state) {
                tracing::error!(error = %e, "channel-access: failed to persist demote");
                return json_response("500 Internal Server Error", r#"{"error":"persistence_error"}"#);
            }
            return json_response("200 OK", r#"{"ok":true}"#);
        }
        return json_response("404 Not Found", r#"{"error":"user_not_found"}"#);
    }
    if method == "POST" && path_only == "/api/channel-access/telegram/reset" {
        let state = crate::channel_access::TelegramAccessState::default();
        if let Err(e) = crate::channel_access::save(data_dir, &state) {
            tracing::error!(error = %e, "channel-access: failed to persist reset");
            return json_response("500 Internal Server Error", r#"{"error":"persistence_error"}"#);
        }
        return json_response("200 OK", r#"{"ok":true}"#);
    }

    if method == "GET" && path_only == "/api/permissions/mode" {
        let state = load_permission_mode(data_dir);
        return json_response(
            "200 OK",
            &serde_json::json!({ "mode": state.mode, "updated_at": state.updated_at }).to_string(),
        );
    }
    if method == "POST" && path_only == "/api/permissions/mode" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let mode = body_json
            .as_ref()
            .and_then(|v| v.get("mode").and_then(|s| s.as_str()))
            .unwrap_or("")
            .to_lowercase();
        if mode != "ask_me" && mode != "allow_all" {
            return json_response("400 Bad Request", r#"{"error":"invalid_mode"}"#);
        }
        let state = PermissionModeState {
            mode,
            updated_at: chrono::Utc::now().to_rfc3339(),
        };
        match save_permission_mode(data_dir, &state) {
            Ok(()) => {
                return json_response(
                    "200 OK",
                    &serde_json::json!({ "ok": true, "mode": state.mode }).to_string(),
                )
            }
            Err(e) => {
                return json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error":"save_failed", "detail": e.to_string() }).to_string(),
                )
            }
        }
    }

    // Permission center: persisted approval decisions.
    if method == "GET" && path_only == "/api/permissions/decisions" {
        let state = crate::permissions_center::load(data_dir);
        let body = serde_json::json!({ "decisions": state.decisions });
        return json_response("200 OK", &body.to_string());
    }
    if method == "GET" && path_only == "/api/permissions/queue" {
        let status_filter = path
            .split('?')
            .nth(1)
            .and_then(|q| q.split('&').find(|p| p.starts_with("status=")))
            .and_then(|p| p.split_once('=').map(|(_, v)| decode_url_component(v)))
            .and_then(|v| crate::permissions_queue::QueueStatus::parse(&v));
        let limit = path
            .split('?')
            .nth(1)
            .and_then(|q| q.split('&').find(|p| p.starts_with("limit=")))
            .and_then(|p| p.split_once('=').map(|(_, v)| v.to_string()))
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(100)
            .clamp(1, 500);
        let mut items = crate::permissions_queue::load(data_dir).requests;
        if let Some(status) = status_filter {
            items.retain(|i| i.status == status);
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items.truncate(limit);
        let body = serde_json::json!({ "items": items });
        return json_response("200 OK", &body.to_string());
    }
    if method == "GET" && path_only.starts_with("/api/permissions/queue/") {
        let id = path_only.trim_start_matches("/api/permissions/queue/").trim();
        if id.is_empty() {
            return json_response("400 Bad Request", r#"{"error":"missing_id"}"#);
        }
        if let Some(item) = crate::permissions_queue::get_request(data_dir, id) {
            return json_response(
                "200 OK",
                &serde_json::json!({ "item": item }).to_string(),
            );
        }
        return json_response("404 Not Found", r#"{"error":"request_not_found"}"#);
    }
    if method == "POST"
        && (path_only.starts_with("/api/permissions/queue/") && path_only.ends_with("/approve"))
    {
        let id = path_only
            .trim_start_matches("/api/permissions/queue/")
            .trim_end_matches("/approve")
            .trim_matches('/');
        if id.is_empty() {
            return json_response("400 Bad Request", r#"{"error":"missing_id"}"#);
        }
        let note = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
            .and_then(|v| v.get("note").and_then(|s| s.as_str()).map(|s| s.to_string()));
        match crate::permissions_queue::update_status(
            data_dir,
            id,
            crate::permissions_queue::QueueStatus::Approved,
            note,
        ) {
            Ok(Some(item)) => {
                if let (Some(store), Ok(task_id)) =
                    (human_input_store.as_ref(), Uuid::parse_str(&item.task_id))
                {
                    let pending = {
                        let mut g = store.write().await;
                        g.remove(&task_id)
                    };
                    if let Some(pending) = pending {
                        let _ = pending.response_tx.send("Approuver".to_string());
                    }
                }
                return json_response(
                    "200 OK",
                    &serde_json::json!({ "ok": true, "item": item }).to_string(),
                );
            }
            Ok(None) => return json_response("404 Not Found", r#"{"error":"request_not_found"}"#),
            Err(e) => {
                return json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error":"save_failed", "detail": e.to_string() }).to_string(),
                )
            }
        }
    }
    if method == "POST"
        && (path_only.starts_with("/api/permissions/queue/") && path_only.ends_with("/deny"))
    {
        let id = path_only
            .trim_start_matches("/api/permissions/queue/")
            .trim_end_matches("/deny")
            .trim_matches('/');
        if id.is_empty() {
            return json_response("400 Bad Request", r#"{"error":"missing_id"}"#);
        }
        let note = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
            .and_then(|v| v.get("note").and_then(|s| s.as_str()).map(|s| s.to_string()));
        match crate::permissions_queue::update_status(
            data_dir,
            id,
            crate::permissions_queue::QueueStatus::Denied,
            note,
        ) {
            Ok(Some(item)) => {
                if let (Some(store), Ok(task_id)) =
                    (human_input_store.as_ref(), Uuid::parse_str(&item.task_id))
                {
                    let pending = {
                        let mut g = store.write().await;
                        g.remove(&task_id)
                    };
                    if let Some(pending) = pending {
                        let _ = pending.response_tx.send("Refuser".to_string());
                    }
                }
                return json_response(
                    "200 OK",
                    &serde_json::json!({ "ok": true, "item": item }).to_string(),
                );
            }
            Ok(None) => return json_response("404 Not Found", r#"{"error":"request_not_found"}"#),
            Err(e) => {
                return json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error":"save_failed", "detail": e.to_string() }).to_string(),
                )
            }
        }
    }
    if method == "POST" && path_only == "/api/permissions/decisions" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let tool = body_json
            .as_ref()
            .and_then(|v| v.get("tool").and_then(|s| s.as_str()))
            .unwrap_or("")
            .trim()
            .to_string();
        let scope = body_json
            .as_ref()
            .and_then(|v| v.get("scope").and_then(|s| s.as_str()))
            .unwrap_or("global")
            .trim()
            .to_string();
        let mode_str = body_json
            .as_ref()
            .and_then(|v| v.get("mode").and_then(|s| s.as_str()))
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let mode = match mode_str.as_str() {
            "allow_persistent" => crate::permissions_center::DecisionMode::AllowPersistent,
            "deny_persistent" => crate::permissions_center::DecisionMode::DenyPersistent,
            _ => {
                return json_response(
                    "400 Bad Request",
                    r#"{"error":"invalid_mode","expected":"allow_persistent|deny_persistent"}"#,
                )
            }
        };
        if tool.is_empty() {
            return json_response("400 Bad Request", r#"{"error":"missing_tool"}"#);
        }
        let mut state = crate::permissions_center::load(data_dir);
        state
            .decisions
            .retain(|d| !(d.tool == tool && d.scope == scope));
        let decision = crate::permissions_center::PermissionDecision {
            id: Uuid::new_v4().to_string(),
            tool,
            scope,
            mode,
            created_at: chrono::Utc::now().to_rfc3339(),
            expires_at: None,
        };
        state.decisions.push(decision.clone());
        match crate::permissions_center::save(data_dir, &state) {
            Ok(()) => {
                return json_response(
                    "200 OK",
                    &serde_json::json!({ "ok": true, "decision": decision }).to_string(),
                )
            }
            Err(e) => {
                return json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error":"save_failed", "detail": e.to_string() }).to_string(),
                )
            }
        }
    }
    if method == "DELETE" && path_only.starts_with("/api/permissions/decisions/") {
        let id = path_only
            .trim_start_matches("/api/permissions/decisions/")
            .trim();
        if id.is_empty() {
            return json_response("400 Bad Request", r#"{"error":"missing_id"}"#);
        }
        let mut state = crate::permissions_center::load(data_dir);
        let before = state.decisions.len();
        state.decisions.retain(|d| d.id != id);
        let removed = before != state.decisions.len();
        if removed {
            if let Err(e) = crate::permissions_center::save(data_dir, &state) {
                tracing::error!(error = %e, id = %id, "permissions/decisions: failed to persist delete");
                return json_response("500 Internal Server Error", r#"{"error":"persistence_error"}"#);
            }
        }
        return json_response(
            "200 OK",
            &serde_json::json!({ "ok": true, "removed": removed, "id": id }).to_string(),
        );
    }

    // Budget controls (daily limit + warning ratio + auto-concise mode).
    if method == "GET" && path_only == "/api/budget" {
        let settings = load_budget_settings(data_dir);
        let session_id = path
            .split('?')
            .nth(1)
            .and_then(|q| q.split('&').find(|p| p.starts_with("session_id=")))
            .map(|p| p.trim_start_matches("session_id=").to_string())
            .unwrap_or_default();
        let session_usage = if session_id.is_empty() {
            None
        } else {
            task_usage_store.get_session(&session_id).await
        };
        let totals = task_usage_store.totals().await;
        let used_tokens = session_usage.map(|u| u.0).unwrap_or(totals.0);
        let used_cost_usd = session_usage.map(|u| u.1).unwrap_or(totals.1);
        let usage_ratio = if settings.daily_token_limit == 0 {
            0.0
        } else {
            used_tokens as f64 / settings.daily_token_limit as f64
        };
        let body = serde_json::json!({
            "settings": settings,
            "session_id": if session_id.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(session_id) },
            "usage": {
                "tokens": used_tokens,
                "cost_usd": used_cost_usd,
                "ratio": usage_ratio,
                "warn_reached": usage_ratio >= settings.warn_ratio
            }
        });
        return json_response("200 OK", &body.to_string());
    }
    if method == "POST" && path_only == "/api/budget" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let mut settings = load_budget_settings(data_dir);
        if let Some(limit) = body_json
            .as_ref()
            .and_then(|v| v.get("daily_token_limit").and_then(|n| n.as_u64()))
        {
            settings.daily_token_limit = limit;
        }
        if let Some(warn_ratio) = body_json
            .as_ref()
            .and_then(|v| v.get("warn_ratio").and_then(|n| n.as_f64()))
        {
            settings.warn_ratio = warn_ratio.clamp(0.0, 1.0);
        }
        if let Some(auto_concise) = body_json
            .as_ref()
            .and_then(|v| v.get("auto_concise").and_then(|b| b.as_bool()))
        {
            settings.auto_concise = auto_concise;
        }
        match save_budget_settings(data_dir, &settings) {
            Ok(()) => {
                return json_response(
                    "200 OK",
                    &serde_json::json!({ "ok": true, "settings": settings }).to_string(),
                )
            }
            Err(e) => {
                return json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error":"save_failed", "detail": e.to_string() }).to_string(),
                )
            }
        }
    }
    if method == "POST" && path_only == "/api/budget/reset-session" {
        let body_json = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
        let session_id = body_json
            .as_ref()
            .and_then(|v| v.get("session_id").and_then(|s| s.as_str()))
            .unwrap_or("")
            .trim()
            .to_string();
        if session_id.is_empty() {
            return json_response("400 Bad Request", r#"{"error":"missing_session_id"}"#);
        }
        task_usage_store.reset_session(&session_id).await;
        return json_response(
            "200 OK",
            &serde_json::json!({ "ok": true, "session_id": session_id }).to_string(),
        );
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
                crate::http_get_cache::invalidate_router_models();
                crate::http_get_cache::invalidate_router_routes();
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
        if let Some(cached) = crate::http_get_cache::cache_get_router_routes() {
            return json_response("200 OK", &cached);
        }
        let routes = llm_router.routes_by_category();
        let body = serde_json::to_string(&routes).unwrap_or_else(|_| "{}".to_string());
        crate::http_get_cache::cache_put_router_routes(&body);
        return json_response("200 OK", &body);
    }

    // GET /api/router/models — list models from all providers (config + Ollama live when available)
    if method == "GET" && path == "/api/router/models" {
        if let Some(cached) = crate::http_get_cache::cache_get_router_models() {
            return json_response("200 OK", &cached);
        }
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
        let body_str = body.to_string();
        crate::http_get_cache::cache_put_router_models(&body_str);
        return json_response("200 OK", &body_str);
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

/// Short follow-up actions for Code Studio UI (`GET /api/tasks/:id` → `suggested_actions`).
fn code_studio_suggested_actions(
    status: &TaskStatus,
    failure_detail: Option<&str>,
    last_progress: Option<&ProgressEntry>,
    acceptance_review: Option<&serde_json::Value>,
) -> Vec<serde_json::Value> {
    use TaskStatus::*;
    let mut v = Vec::new();
    match status {
        Completed => {
            if let Some(payload) = acceptance_review {
                if let Some(arr) = payload.get("missing").and_then(|m| m.as_array()) {
                    if !arr.is_empty() {
                        let joined = arr
                            .iter()
                            .filter_map(|x| x.as_str())
                            .take(12)
                            .collect::<Vec<_>>()
                            .join("; ");
                        if !joined.is_empty() {
                            v.push(serde_json::json!({
                                "id": "acceptance_review_followup",
                                "label": "Compléter les critères signalés",
                                "kind": "message",
                                "message": format!(
                                    "La tâche est terminée mais la revue des critères a signalé des points à clarifier ou compléter : {joined}\n\nPropose des changements concrets (fichiers + étapes) pour les traiter."
                                )
                            }));
                        }
                    }
                }
            }
            v.push(serde_json::json!({
                "id": "refresh_files",
                "label": "Rafraîchir la liste des fichiers",
                "kind": "ui",
                "ui_action": "refresh_files"
            }));
            v.push(serde_json::json!({
                "id": "open_editor",
                "label": "Onglet Éditeur",
                "kind": "ui",
                "ui_action": "open_editor"
            }));
            v.push(serde_json::json!({
                "id": "open_preview",
                "label": "Ouvrir l’aperçu",
                "kind": "ui",
                "ui_action": "open_preview"
            }));
            v.push(serde_json::json!({
                "id": "open_design",
                "label": "Voir DESIGN.md",
                "kind": "ui",
                "ui_action": "open_design"
            }));
        }
        Failed | Cancelled => {
            let hint = failure_detail
                .map(str::to_string)
                .or_else(|| last_progress.map(|e| e.message.clone()))
                .unwrap_or_default();
            let truncated = if hint.chars().count() > 500 {
                hint.chars().take(500).collect::<String>() + "…"
            } else {
                hint
            };
            v.push(serde_json::json!({
                "id": "analyze_failure",
                "label": "Demander une analyse de l’échec",
                "kind": "message",
                "message": format!(
                    "La dernière tâche a échoué ou été annulée. Contexte:\n{}\n\nPropose un plan de correction ciblé (fichiers et étapes) sans refaire toute l’implémentation.",
                    truncated
                )
            }));
        }
        WaitingUserInput => {
            v.push(serde_json::json!({
                "id": "reply_wait",
                "label": "Rappel : répondre à l’agent",
                "kind": "message",
                "message": "Je complète ma réponse pour l’agent (voir la zone « Réponse requise » dans le suivi de tâche)."
            }));
        }
        Running | Queued | Pending | Paused | Interrupted => {
            v.push(serde_json::json!({
                "id": "wait_continue",
                "label": "Poursuivre après la tâche",
                "kind": "message",
                "message": "Continue sur la base du plan actuel ; je reviens vérifier le résultat une fois la tâche terminée."
            }));
        }
    }
    v
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
                    task_id: Some(id.to_string()),
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
                    task_id: Some(id.to_string()),
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
    let (last_turn_tokens_in, last_turn_tokens_out, last_turn_cost_usd) =
        task_usage_store.get_last_turn(id).await.unwrap_or((0, 0, 0.0));
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
    let failure_detail: Option<String> = if matches!(
        task.status,
        TaskStatus::Failed | TaskStatus::Cancelled
    ) {
        progress_list
            .iter()
            .rev()
            .find(|e| !task_progress_is_chat_stub(&e.message))
            .map(|e| e.message.chars().take(4000).collect::<String>())
    } else {
        None
    };
    let last_for_suggest = progress_list.last().cloned();
    let acceptance_review = store.get_events(id).ok().and_then(|evs| {
        evs.iter()
            .rev()
            .find(|e| e.event_type == "studio_acceptance_review")
            .and_then(|e| e.payload.clone())
    });
    let suggested = code_studio_suggested_actions(
        &task.status,
        failure_detail.as_deref(),
        last_for_suggest.as_ref(),
        acceptance_review.as_ref(),
    );
    // Dernière ligne de progression par sous-tâche pour le détail Studio (dédoublonnage côté client par task_id).
    if let Ok(children) = store.get_children(id) {
        if !children.is_empty() {
            let mem_snapshot: std::collections::HashMap<Uuid, VecDeque<ProgressEntry>> = {
                let g = progress.read().await;
                g.iter().map(|(k, v)| (*k, v.clone())).collect()
            };
            for c in &children {
                let from_mem = mem_snapshot.get(&c.id).and_then(|q| q.back().cloned());
                let from_disk = store.get_progress(c.id).ok().and_then(|v| {
                    v.last().map(|(pct, msg)| ProgressEntry {
                        progress_pct: *pct,
                        message: msg.clone(),
                        task_id: Some(c.id.to_string()),
                    })
                });
                if let Some(mut e) = from_mem.or(from_disk) {
                    if e.task_id.is_none() {
                        e.task_id = Some(c.id.to_string());
                    }
                    progress_list.push(e);
                }
            }
        }
    }
    let mut body = serde_json::json!({
        "task_id": task.id.to_string(),
        "status": task.status.as_str(),
        "assigned_agent": task.assigned_agent,
        "created_at": task.created_at.to_rfc3339(),
        "updated_at": task.updated_at.to_rfc3339(),
        "progress": progress_list,
        "tokens_used": tokens_used,
        "cost_usd": cost_usd,
        "last_turn_tokens_in": last_turn_tokens_in,
        "last_turn_tokens_out": last_turn_tokens_out,
        "last_turn_cost_usd": last_turn_cost_usd,
        "todos": todos_json,
        "suggested_actions": suggested
    });
    if let Some(u) = todos_updated_at {
        body["todos_updated_at"] = serde_json::Value::String(u);
    }
    if let Some(fd) = failure_detail {
        body["failure_detail"] = serde_json::Value::String(fd);
    }
    if let Some(ref ar) = acceptance_review {
        body["acceptance_review"] = ar.clone();
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

async fn schedule_set_enabled(store_path: &Path, id: Uuid, enabled: bool) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let mut s = match store.get_schedule(id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return json_response("404 Not Found", r#"{"error":"schedule_not_found"}"#);
        }
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    s.enabled = enabled;
    s.updated_at = chrono::Utc::now();
    if store.update_schedule(&s).is_err() {
        return json_response("500 Internal Server Error", r#"{"error":"store"}"#);
    }
    json_response(
        "200 OK",
        &serde_json::json!({ "id": id.to_string(), "enabled": enabled }).to_string(),
    )
}

async fn schedule_run_now(
    store_path: &Path,
    main_agent: &crate::agents::MainAgent,
    schedule_id: Uuid,
) -> String {
    let store = match ScheduleStore::open(store_path) {
        Ok(s) => s,
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    let s = match store.get_schedule(schedule_id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return json_response("404 Not Found", r#"{"error":"schedule_not_found"}"#);
        }
        Err(_) => return json_response("500 Internal Server Error", r#"{"error":"store"}"#),
    };
    if !s.enabled {
        return json_response(
            "400 Bad Request",
            &serde_json::json!({
                "error": "schedule_paused",
                "detail": "Resume the schedule before run-now.",
            })
            .to_string(),
        );
    }
    let msg = s
        .channel_context
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(s.name.as_str());
    let session_id = format!("schedule:{}", schedule_id);
    let correlation = Uuid::new_v4();
    match main_agent
        .handle_message(
            store_path,
            msg,
            correlation,
            true,
            &session_id,
            None,
            TaskPriority::Scheduled,
            None,
            None,
            None,
        )
        .await
    {
        Ok(task_id) => json_response(
            "200 OK",
            &serde_json::json!({
                "task_id": task_id.to_string(),
                "schedule_id": schedule_id.to_string(),
            })
            .to_string(),
        ),
        Err(e) => json_response(
            "500 Internal Server Error",
            &serde_json::json!({ "error": "run_now_failed", "detail": e.to_string() }).to_string(),
        ),
    }
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
        resolve_run_command_working_dir, strip_markdown_fences_from_write_content,
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
    fn detect_session_recall_ignores_task_recap_in_design_spec() {
        assert!(
            detect_session_recall_intent(
                "Nothing else (no task recap, no npm commands, no non-English text)."
            )
            .is_none(),
            "DESIGN.md/Code Studio boilerplate must not trigger session-recall fast path"
        );
    }

    #[test]
    fn detect_session_recall_still_detects_recap_verb() {
        let intent = detect_session_recall_intent("Can you recap what we shipped today?")
            .expect("recap request");
        assert_eq!(intent.range, SessionRecallRange::CurrentDay);
        assert_eq!(intent.language, SmallTalkLanguage::English);
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
    fn parse_tool_calls_xml_tool_call_tags() {
        let s = "<tool_call>read_file workspace:/src/App.tsx 1 10</tool_call>\n<tool_call>read_file workspace:/src/b.tsx 1 5</tool_call>";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 2, "{:?}", c);
        assert_eq!(c[0].0, "read_file");
        assert_eq!(c[1].0, "read_file");
    }

    #[test]
    fn parse_tool_calls_xml_tool_call_chained_without_close() {
        let s = "<tool_call>read_file workspace:/a.tsx 1 2<tool_call>read_file workspace:/b.tsx 3 4";
        let c = parse_tool_calls(s);
        assert_eq!(c.len(), 2, "{:?}", c);
        assert_eq!(c[0].0, "read_file");
        assert_eq!(c[1].0, "read_file");
    }

    /// DeepSeek-style `<｜DSML｜…>` (U+FF5C fullwidth vertical line).
    #[test]
    fn parse_tool_calls_dsml_search_files_fullwidth_delimiters() {
        let d = "\u{ff5c}";
        let s = format!(
            "<{d}DSML{d}tool_calls>\n\
             <{d}DSML{d}invoke name=\"search_files\">\n\
             <{d}DSML{d}parameter name=\"dir\" string=\"true\">workspace:/</{d}DSML{d}parameter>\n\
             <{d}DSML{d}parameter name=\"pattern\" string=\"true\">**/*</{d}DSML{d}parameter>\n\
             <{d}DSML{d}parameter name=\"--no-ignore\" string=\"false\">true</{d}DSML{d}parameter>\n\
             <{d}DSML{d}parameter name=\"limit\" string=\"false\">200</{d}DSML{d}parameter>\n\
             </{d}DSML{d}invoke>\n\
             </{d}DSML{d}tool_calls>",
            d = d
        );
        let c = parse_tool_calls(&s);
        assert_eq!(c.len(), 1, "{:?}", c);
        assert_eq!(c[0].0, "search_files");
        assert_eq!(c[0].1[0], "workspace:/");
        assert_eq!(c[0].1[1], "**/*");
        assert!(c[0].1.contains(&"--no-ignore".to_string()));
        assert!(
            !c[0].1.iter().any(|a| a == "200"),
            "limit must be ignored: {:?}",
            c[0].1
        );
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
    fn parse_write_file_request_preserves_same_line_content_spacing() {
        // All tokens come from the TOOL header (no collected body). The last token is joined
        // with a newline to be consistent with the single-line-body case — the resulting JSON
        // (with a newline before the final `}`) is still valid and parseable.
        let args = vec![
            "workspace:/tsconfig.json".to_string(),
            "{".to_string(),
            "\"compilerOptions\":".to_string(),
            "{".to_string(),
            "\"strict\":".to_string(),
            "true".to_string(),
            "}".to_string(),
            "}".to_string(),
        ];
        let (path, content) = parse_write_file_request(&args).expect("write_file should parse");
        assert_eq!(path, "workspace:/tsconfig.json");
        assert_eq!(content, "{ \"compilerOptions\": { \"strict\": true }\n}");
    }

    #[test]
    fn parse_write_file_request_combines_inline_prefix_and_single_line_body() {
        // Inline token on the TOOL header line + exactly one body line collected by the parser.
        let args = vec![
            "workspace:/src/index.js".to_string(),
            "const".to_string(),
            "x = 1;".to_string(),
            "return x;".to_string(),
        ];
        let (_, content) = parse_write_file_request(&args).expect("write_file should parse");
        assert_eq!(content, "const x = 1;\nreturn x;");
    }

    #[test]
    fn parse_write_file_request_combines_inline_prefix_and_multiline_body() {
        let args = vec![
            "workspace:/index.html".to_string(),
            "<!doctype".to_string(),
            "html>".to_string(),
            "<html>\n<body></body>\n</html>".to_string(),
        ];
        let (_, content) = parse_write_file_request(&args).expect("write_file should parse");
        assert_eq!(content, "<!doctype html>\n<html>\n<body></body>\n</html>");
    }

    #[test]
    fn strip_markdown_fences_removes_wrapping_fence() {
        let raw = "```tsx\nconst x = 1;\n```";
        assert_eq!(strip_markdown_fences_from_write_content(raw), "const x = 1;");
        let raw2 = "```\nhello\n```\n";
        assert_eq!(strip_markdown_fences_from_write_content(raw2), "hello");
        assert_eq!(
            strip_markdown_fences_from_write_content("no fence here"),
            "no fence here"
        );
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

    // --- tool_scope_key ---

    #[test]
    fn tool_scope_key_run_command_joins_first_two_args() {
        let args = vec![s("git"), s("status"), s("--short")];
        let key = super::tool_scope_key("run_command", &args);
        assert_eq!(key, "git status");
    }

    #[test]
    fn tool_scope_key_delete_file_joins_all_args_for_spaced_paths() {
        let args = vec![s("Cas"), s("d'usage.pdf")];
        let key = super::tool_scope_key("delete_file", &args);
        assert_eq!(key, "Cas d'usage.pdf");
    }

    #[test]
    fn tool_scope_key_delete_file_single_arg() {
        let args = vec![s("/tmp/test.txt")];
        let key = super::tool_scope_key("delete_file", &args);
        assert_eq!(key, "/tmp/test.txt");
    }

    #[test]
    fn tool_scope_key_write_file_uses_plain_path_from_first_arg() {
        let args = vec![s("/tmp/hello.txt"), s("content here")];
        let key = super::tool_scope_key("write_file", &args);
        assert_eq!(key, "/tmp/hello.txt");
    }

    #[test]
    fn tool_scope_key_write_file_parses_json_path() {
        let args = vec![s(r#"{"path":"/tmp/from_json.txt","content":"hello"}"#)];
        let key = super::tool_scope_key("write_file", &args);
        assert_eq!(key, "/tmp/from_json.txt");
    }

    #[test]
    fn tool_scope_key_edit_file_uses_first_arg() {
        let args = vec![s("/some/file.rs"), s("1"), s("5"), s("new content")];
        let key = super::tool_scope_key("edit_file", &args);
        assert_eq!(key, "/some/file.rs");
    }

    #[test]
    fn tool_scope_key_unknown_tool_returns_global() {
        let args = vec![s("whatever")];
        let key = super::tool_scope_key("unknown_tool", &args);
        assert_eq!(key, "global");
    }

    #[test]
    fn tool_scope_key_empty_args_returns_global_for_write_file() {
        let args: Vec<String> = vec![];
        let key = super::tool_scope_key("write_file", &args);
        assert_eq!(key, "global");
    }

    // --- permissions_center (load/save/lookup) ---

    #[test]
    fn permissions_center_save_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::permissions_center::PermissionCenterState {
            decisions: vec![crate::permissions_center::PermissionDecision {
                id: "abc".to_string(),
                tool: "delete_file".to_string(),
                scope: "/tmp/foo.txt".to_string(),
                mode: crate::permissions_center::DecisionMode::AllowPersistent,
                created_at: "2024-01-01T00:00:00Z".to_string(),
                expires_at: None,
            }],
        };
        crate::permissions_center::save(tmp.path(), &state).unwrap();
        let loaded = crate::permissions_center::load(tmp.path());
        assert_eq!(loaded.decisions.len(), 1);
        assert_eq!(loaded.decisions[0].tool, "delete_file");
        assert_eq!(loaded.decisions[0].scope, "/tmp/foo.txt");
        assert_eq!(loaded.decisions[0].mode, crate::permissions_center::DecisionMode::AllowPersistent);
    }

    #[test]
    fn permissions_center_lookup_returns_matching_decision() {
        let state = crate::permissions_center::PermissionCenterState {
            decisions: vec![
                crate::permissions_center::PermissionDecision {
                    id: "d1".to_string(),
                    tool: "write_file".to_string(),
                    scope: "/tmp/a.txt".to_string(),
                    mode: crate::permissions_center::DecisionMode::DenyPersistent,
                    created_at: "2024-01-01T00:00:00Z".to_string(),
                    expires_at: None,
                },
            ],
        };
        let found = crate::permissions_center::lookup("write_file", "/tmp/a.txt", &state);
        assert!(found.is_some());
        assert_eq!(found.unwrap().mode, crate::permissions_center::DecisionMode::DenyPersistent);
    }

    #[test]
    fn permissions_center_lookup_returns_none_for_different_scope() {
        let state = crate::permissions_center::PermissionCenterState {
            decisions: vec![crate::permissions_center::PermissionDecision {
                id: "d1".to_string(),
                tool: "write_file".to_string(),
                scope: "/tmp/a.txt".to_string(),
                mode: crate::permissions_center::DecisionMode::AllowPersistent,
                created_at: "2024-01-01T00:00:00Z".to_string(),
                expires_at: None,
            }],
        };
        let not_found = crate::permissions_center::lookup("write_file", "/tmp/b.txt", &state);
        assert!(not_found.is_none());
    }

    #[test]
    fn permissions_center_lookup_returns_none_for_different_tool() {
        let state = crate::permissions_center::PermissionCenterState {
            decisions: vec![crate::permissions_center::PermissionDecision {
                id: "d1".to_string(),
                tool: "write_file".to_string(),
                scope: "global".to_string(),
                mode: crate::permissions_center::DecisionMode::AllowPersistent,
                created_at: "2024-01-01T00:00:00Z".to_string(),
                expires_at: None,
            }],
        };
        let not_found = crate::permissions_center::lookup("delete_file", "global", &state);
        assert!(not_found.is_none());
    }

    #[test]
    fn permissions_center_save_is_atomic_via_tmp_rename() {
        // Verify no .tmp file is left behind after a successful save.
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::permissions_center::PermissionCenterState::default();
        crate::permissions_center::save(tmp.path(), &state).unwrap();
        let tmp_file = tmp.path().join("permissions_center.json.tmp");
        assert!(!tmp_file.exists(), ".tmp file should not remain after save");
    }
}
